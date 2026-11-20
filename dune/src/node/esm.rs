//! Node's ES module resolution for `import`: bare specifiers through `node_modules` and package.json `exports` /
//! `main`, `#` specifiers through `imports`, `file:` URLs; paths are left to the inner platform.
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::Value;

/// The `exports` / `imports` conditions `import` matches, in the order a target object lists them
#[derive(Clone, Copy)]
enum Condition {
    Import,
    Node,
    Default,
}

impl TryFrom<&str> for Condition {
    type Error = ();

    fn try_from(key: &str) -> Result<Self, ()> {
        const TABLE: [(Condition, &str); 3] = [(Condition::Import, "import"), (Condition::Node, "node"), (Condition::Default, "default")];
        TABLE.into_iter().find(|(_, k)| *k == key).map(|(c, _)| c).ok_or(())
    }
}

/// What an `import` specifier names
pub enum Specifier<'s> {
    /// A path (relative or absolute), for the inner platform
    Path(&'s str),
    /// `#name`: the importing package's `imports`
    Private(&'s str),
    /// `name` or `@scope/name`, then a subpath (`.` for none, else `./x`)
    Bare { name: &'s str, subpath: String },
}

/// Prefix of a `file:` URL: file modules are named by theirs, as `import.meta.url` is the name
pub const FILE_URL: &str = "file://";

impl<'s> From<&'s str> for Specifier<'s> {
    fn from(text: &'s str) -> Self {
        let text = text.strip_prefix(FILE_URL).unwrap_or(text);
        if text.starts_with('.') || text.starts_with('/') {
            return Self::Path(text);
        }
        if text.starts_with('#') {
            return Self::Private(text);
        }
        let parts = if text.starts_with('@') { 2 } else { 1 };
        let end = text.match_indices('/').nth(parts - 1).map_or(text.len(), |(i, _)| i);
        Self::Bare { name: &text[..end], subpath: format!(".{}", &text[end..]) }
    }
}

/// package.json `type`: how its `.js` (and extensionless) files load
#[derive(Deserialize, Default, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Kind {
    #[default]
    Commonjs,
    Module,
}

/// The parts of a package.json resolution reads
#[derive(Deserialize, Default)]
struct Manifest {
    #[serde(default, rename = "type")]
    kind: Kind,
    exports: Option<Value>,
    imports: Option<Value>,
    main: Option<String>,
}

/// A package directory and its package.json
pub struct Package {
    dir: PathBuf,
    manifest: Manifest,
}

impl Package {
    fn at(dir: &Path) -> Option<Self> {
        let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
        Some(Self { dir: dir.to_path_buf(), manifest: serde_json::from_str(&text).unwrap_or_default() })
    }

    /// The package holding `file`: the nearest package.json above it
    pub fn holding(file: &Path) -> Option<Self> {
        file.ancestors().skip(1).find_map(Self::at)
    }

    /// Package `name` as seen from `from`: the first `node_modules/<name>` up from it
    pub fn find(from: &Path, name: &str) -> Option<Self> {
        from.ancestors().skip(1).find_map(|d| Self::at(&d.join("node_modules").join(name)))
    }

    /// The file `subpath` names: through `exports` when there is one, else `main` / the path itself
    pub fn export(&self, subpath: &str) -> Option<PathBuf> {
        if let Some(exports) = &self.manifest.exports {
            let sugar = exports.as_object().is_none_or(|m| m.keys().next().is_some_and(|k| !k.starts_with('.')));
            let target = if sugar { (subpath == ".").then(|| Self::target(exports, None)).flatten() } else { Self::lookup(exports, subpath) };
            return target.map(|t| self.dir.join(t));
        }
        if subpath != "." {
            return Some(self.dir.join(subpath));
        }
        let main = self.manifest.main.as_deref().map(|m| self.dir.join(m));
        let tries = main.iter().flat_map(|m| [m.clone(), m.with_extension("js"), m.join("index.js")]).chain([self.dir.join("index.js")]);
        tries.into_iter().find(|p| p.is_file())
    }

    /// The target `#name` maps to in `imports`: a file of this package, or another specifier
    pub fn import(&self, name: &str) -> Option<String> {
        self.manifest.imports.as_ref().and_then(|imports| Self::lookup(imports, name))
    }

    /// Whether `file` is CommonJS by Node's rules: `.cjs`, or `.js` / no extension where the nearest package.json
    /// doesn't say `"type": "module"`
    pub fn commonjs(file: &Path) -> bool {
        const MODULE: &str = "mjs";
        const COMMONJS: &str = "cjs";
        const SCRIPT: &str = "js";
        match file.extension().and_then(|e| e.to_str()) {
            Some(COMMONJS) => true,
            None | Some(SCRIPT) => Self::holding(file).is_none_or(|p| p.manifest.kind == Kind::Commonjs),
            Some(MODULE | _) => false,
        }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// `key` in a subpath map: exact, else the `*` pattern with the longest prefix
    fn lookup(map: &Value, key: &str) -> Option<String> {
        let map = map.as_object()?;
        if let Some(target) = map.get(key) {
            return Self::target(target, None);
        }
        let matches = map.iter().filter_map(|(k, target)| {
            let (before, after) = k.split_once('*')?;
            let hole = key.strip_prefix(before)?.strip_suffix(after)?;
            Some((before.len(), target, hole))
        });
        let (_, target, hole) = matches.max_by_key(|(len, ..)| *len)?;
        Self::target(target, Some(hole))
    }

    /// A target: a string (with `*` filled by `hole`), the first resolvable of an array, or the first matching
    /// condition of an object
    fn target(target: &Value, hole: Option<&str>) -> Option<String> {
        match target {
            Value::String(s) => Some(hole.map_or_else(|| s.clone(), |h| s.replace('*', h))),
            Value::Array(all) => all.iter().find_map(|t| Self::target(t, hole)),
            Value::Object(map) => map.iter().filter(|(k, _)| Condition::try_from(k.as_str()).is_ok()).find_map(|(_, t)| Self::target(t, hole)),
            _ => None,
        }
    }
}
