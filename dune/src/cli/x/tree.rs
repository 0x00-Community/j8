use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

use nodejs_semver::Version;

use super::manifest::{Dist, Manifest, Need};
use super::registry::{Package, Registry};
use super::spec::{Dep, Spec, Want};

/// A `node_modules` tree being resolved; node 0 is the directory holding the top `node_modules`
pub struct Tree<'r> {
    registry: &'r Registry,
    packages: HashMap<String, Package>,
    nodes: Vec<Node>,
}

/// One installed package, at `parent`'s `node_modules/<name>`
struct Node {
    /// The directory name: the package's name, or its alias
    name: String,
    /// The package installed (differs from `name` for an alias)
    package: String,
    version: Option<Version>,
    parent: Option<usize>,
    children: BTreeMap<String, usize>,
    dist: Option<Dist>,
    deps: Vec<(String, Dep, Need)>,
    /// Which node each dependency resolved to
    uses: Vec<(String, usize)>,
}

impl Node {
    fn new(name: &str, package: &str, parent: usize, manifest: Manifest) -> Self {
        Self {
            name: name.to_owned(),
            package: package.to_owned(),
            deps: manifest.deps(),
            version: manifest.version,
            parent: Some(parent),
            children: BTreeMap::new(),
            dist: manifest.dist,
            uses: Vec::new(),
        }
    }
}

/// Where a dependency goes
enum Place {
    /// An installed node already satisfies it
    Reuse(usize),
    /// Install it into this node's `node_modules`
    Into(usize),
}

impl<'r> Tree<'r> {
    /// A tree holding `spec` alone, before its dependencies are resolved
    pub fn new(registry: &'r Registry, spec: Spec) -> Self {
        let root = Node {
            name: String::new(),
            package: String::new(),
            version: None,
            parent: None,
            children: BTreeMap::new(),
            dist: None,
            deps: vec![(spec.name.clone(), Dep::Npm(spec), Need::Required)],
            uses: Vec::new(),
        };
        Self { registry, packages: HashMap::new(), nodes: vec![root] }
    }

    /// The registry's metadata for `name`, fetched once
    pub fn package(&mut self, name: &str) -> Result<&Package, String> {
        if !self.packages.contains_key(name) {
            let package = self.registry.package(name)?;
            self.packages.insert(name.to_owned(), package);
        }
        Ok(&self.packages[name])
    }

    /// The version the requested package resolves to
    pub fn top(&mut self) -> Result<Version, String> {
        let name = self.nodes[0].deps.first().map(|(name, ..)| name.clone()).unwrap_or_default();
        self.package(&name)?;
        let Some((_, Dep::Npm(spec), _)) = self.nodes[0].deps.first() else { return Err(format!("{name}: not a registry package")) };
        self.packages[&name].pick(&spec.want).cloned().ok_or_else(|| format!("{name}: no version matches"))
    }

    /// Resolves every dependency, breadth first, fetching each wave's metadata in parallel
    pub fn resolve(&mut self) -> Result<(), String> {
        let mut wave = vec![0];
        while !wave.is_empty() {
            self.prefetch(&wave);
            let mut next = Vec::new();
            for &node in &wave {
                let deps = std::mem::take(&mut self.nodes[node].deps);
                for (name, dep, need) in &deps {
                    match (self.add(node, name, dep), need) {
                        (Ok(Some(added)), _) => next.push(added),
                        (Ok(None), _) | (Err(_), Need::Optional) => {}
                        (Err(e), Need::Required) => return Err(e),
                    }
                }
            }
            wave = next;
        }
        Ok(())
    }

    /// Fetches the metadata the dependencies of `wave` need, in parallel
    fn prefetch(&mut self, wave: &[usize]) {
        let mut names: Vec<&str> = wave
            .iter()
            .flat_map(|&n| &self.nodes[n].deps)
            .filter_map(|(_, dep, _)| match dep {
                Dep::Npm(spec) if !self.packages.contains_key(&spec.name) => Some(spec.name.as_str()),
                _ => None,
            })
            .collect();
        names.sort_unstable();
        names.dedup();
        let registry = self.registry;
        let fetched: Vec<(String, Package)> = std::thread::scope(|s| {
            let jobs: Vec<_> = names.iter().map(|&name| s.spawn(move || registry.package(name).ok().map(|p| (name.to_owned(), p)))).collect();
            jobs.into_iter().filter_map(|j| j.join().ok().flatten()).collect()
        });
        self.packages.extend(fetched);
    }

    /// Installs dependency `name` of node `from`; returns the new node, or `None` when an installed one serves
    fn add(&mut self, from: usize, name: &str, dep: &Dep) -> Result<Option<usize>, String> {
        let Dep::Npm(spec) = dep else { return Err(format!("{name}: dependency kind not supported: {}", Self::other(dep))) };
        let place = self.place(from, name, spec)?;
        let at = match place {
            Place::Reuse(node) => {
                self.nodes[from].uses.push((name.to_owned(), node));
                return Ok(None);
            }
            Place::Into(at) => at,
        };
        let package = self.package(&spec.name)?;
        let version = package.pick(&spec.want).ok_or_else(|| format!("{}: no version matches", spec.name))?.clone();
        let manifest = package.manifest(&version).map_err(|e| format!("{}: {e}", spec.name))?;
        if !manifest.fits() {
            return Err(format!("{}@{version}: not for this platform", spec.name));
        }
        let node = self.nodes.len();
        self.nodes.push(Node::new(name, &spec.name, at, manifest));
        self.nodes[at].children.insert(name.to_owned(), node);
        self.nodes[from].uses.push((name.to_owned(), node));
        Ok(Some(node))
    }

    fn other(dep: &Dep) -> &str {
        match dep {
            Dep::Other(v) => v,
            Dep::Npm(spec) => &spec.name,
        }
    }

    /// Where `name` goes for node `from`: walking up as Node's resolution does, reuse the first `name` found when it
    /// satisfies; else install as high as possible below it, where no node already resolved `name` past that level
    fn place(&mut self, from: usize, name: &str, spec: &Spec) -> Result<Place, String> {
        let mut levels = Vec::new();
        let mut at = Some(from);
        while let Some(level) = at {
            if let Some(&found) = self.nodes[level].children.get(name) {
                if self.serves(found, spec)? {
                    return Ok(Place::Reuse(found));
                }
                break;
            }
            levels.push(level);
            at = self.nodes[level].parent;
        }
        let free = levels.iter().rev().find(|&&level| !self.shadows(level, name)).copied();
        free.map(Place::Into).ok_or_else(|| format!("{name}: no place in node_modules for the version {} needs", self.nodes[from].package))
    }

    /// Whether `node` is the package `spec` asks for, in a version it accepts
    fn serves(&mut self, node: usize, spec: &Spec) -> Result<bool, String> {
        let (package, version) = (self.nodes[node].package.clone(), self.nodes[node].version.clone());
        if package != spec.name {
            return Ok(false);
        }
        let Some(version) = version else { return Ok(false) };
        Ok(self.package(&package)?.admits(&spec.want, &version))
    }

    /// Whether putting `name` into `level`'s `node_modules` would change what an installed node below resolves it to
    fn shadows(&self, level: usize, name: &str) -> bool {
        self.nodes.iter().enumerate().any(|(n, node)| {
            node.uses.iter().any(|(used, target)| used == name && self.within(n, level) && !self.within(self.nodes[*target].parent.unwrap_or(0), level))
        })
    }

    /// Whether `node` is `level` or below it
    fn within(&self, node: usize, level: usize) -> bool {
        let mut at = Some(node);
        while let Some(n) = at {
            if n == level {
                return true;
            }
            at = self.nodes[n].parent;
        }
        false
    }

    /// Where node `n` goes under `root`
    fn path(&self, root: &Path, n: usize) -> PathBuf {
        let mut names = Vec::new();
        let mut at = n;
        while let Some(parent) = self.nodes[at].parent {
            names.push(self.nodes[at].name.as_str());
            at = parent;
        }
        names.iter().rev().fold(root.to_path_buf(), |p, name| p.join("node_modules").join(name))
    }

    /// Downloads and unpacks every package under `root`, several at a time
    pub fn write(&self, root: &Path) -> Result<(), String> {
        let next = AtomicUsize::new(1);
        let workers = std::thread::available_parallelism().map_or(4, |n| n.get() * 2).min(self.nodes.len());
        std::thread::scope(|s| {
            let jobs: Vec<_> = (0..workers)
                .map(|_| {
                    s.spawn(|| -> Result<(), String> {
                        loop {
                            let n = next.fetch_add(1, Ordering::Relaxed);
                            let Some(node) = self.nodes.get(n) else { return Ok(()) };
                            let Some(dist) = &node.dist else { return Err(format!("{}: no tarball", node.package)) };
                            let dir = self.path(root, n);
                            let tarball = self.registry.tarball(&node.package, dist)?;
                            tarball.unpack(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
                        }
                    })
                })
                .collect();
            jobs.into_iter().try_for_each(|j| j.join().unwrap_or_else(|_| Err("install thread panicked".to_owned())))
        })
    }
}

/// Environment variable naming dunx's cache directory
const CACHE_VAR: &str = "DUNE_CACHE";

/// dunx's cache: one directory per installed `name@version`, holding its `node_modules`
pub struct Store {
    dir: PathBuf,
}

impl Default for Store {
    fn default() -> Self {
        let dir = std::env::var_os(CACHE_VAR).map_or_else(
            || {
                let cache = std::env::var_os("XDG_CACHE_HOME").map(PathBuf::from);
                let cache = cache.or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")));
                cache.unwrap_or_else(std::env::temp_dir).join("dune").join("x")
            },
            PathBuf::from,
        );
        Self { dir }
    }
}

impl Store {
    /// The installed package `name@version`, when the cache has it
    pub fn get(&self, name: &str, version: &Version) -> Option<PathBuf> {
        let dir = self.dir.join(Spec::key(name, version)).join("node_modules").join(name);
        dir.is_dir().then_some(dir)
    }

    /// The highest cached version of `spec.name` that `spec` accepts (without the registry, tags can't be checked)
    pub fn best(&self, spec: &Spec) -> Option<PathBuf> {
        let prefix = format!("{}@", spec.name.replace('/', "+"));
        let versions = std::fs::read_dir(&self.dir).ok()?.filter_map(|e| e.ok()?.file_name().to_str()?.strip_prefix(&prefix)?.parse::<Version>().ok());
        let fits = |v: &Version| match &spec.want {
            Want::Range(r) => r.satisfies(v),
            Want::Latest | Want::Tag(_) => true,
        };
        let best = versions.filter(|v| fits(v)).max()?;
        self.get(&spec.name, &best)
    }

    /// Installs `tree` (whose top package is `name@version`) and returns the package's directory; written beside the
    /// cache first and moved in whole, so a cached directory is always complete
    pub fn put(&self, tree: &Tree<'_>, name: &str, version: &Version) -> Result<PathBuf, String> {
        let key = Spec::key(name, version);
        let staging = self.dir.join(format!(".{key}.{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&staging);
        std::fs::create_dir_all(&staging).map_err(|e| format!("{}: {e}", staging.display()))?;
        if let Err(e) = tree.write(&staging) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(e);
        }
        let dest = self.dir.join(&key);
        if std::fs::rename(&staging, &dest).is_err() {
            // another dunx got there first
            let _ = std::fs::remove_dir_all(&staging);
        }
        self.get(name, version).ok_or_else(|| format!("{}: install failed", dest.display()))
    }
}
