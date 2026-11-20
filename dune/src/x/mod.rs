//! `dunx` / `dune x`: run a package's executable. The package comes from the nearest `node_modules` up from the
//! current directory, else from dune's cache, else it is installed from the npm registry into the cache (with its
//! dependencies, in a hoisted `node_modules`; no lifecycle scripts run). JavaScript executables run in this process on
//! the Node layer; other executables (shell scripts, native programs) run as child processes.
mod manifest;
mod registry;
mod spec;
mod tree;

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use manifest::Manifest;
use registry::Registry;
use spec::{Spec, Want};
use tree::{Store, Tree};

use crate::job::{Job, Layer};

/// Run a package's executable
#[derive(clap::Args)]
pub struct X {
    /// The package to use when its name differs from the command's (`-p typescript tsc`)
    #[arg(short, long)]
    package: Option<String>,
    /// Never install: use only what the project's `node_modules` or the cache has
    #[arg(long)]
    no_install: bool,
    /// `<package>[@<version>]` (or the executable's name with `--package`), then the arguments passed to it; options
    /// after it are the executable's
    #[arg(required = true, trailing_var_arg = true, allow_hyphen_values = true, value_name = "COMMAND [ARGS]")]
    argv: Vec<String>,
}

impl X {
    pub fn run(&self, jit: bool) -> ExitCode {
        let (command, args) = self.argv.split_first().map_or(("", &[][..]), |(c, a)| (c.as_str(), a));
        match self.locate(command) {
            Ok(file) => Launch::from(file.as_path()).run(&file, args, jit),
            Err(e) => {
                eprintln!("dunx: {e}");
                ExitCode::FAILURE
            }
        }
    }

    /// The executable's file
    fn locate(&self, command: &str) -> Result<PathBuf, String> {
        let spec = Spec::try_from(self.package.as_deref().unwrap_or(command))?;
        let command = self.package.as_ref().map(|_| command);
        if let Some(file) = Self::local(&spec, command) {
            return Ok(file);
        }
        let store = Store::default();
        let cached = spec.exact.as_ref().and_then(|v| store.get(&spec.name, v));
        let dir = match cached {
            Some(dir) => dir,
            None if self.no_install => store.best(&spec).ok_or_else(|| format!("{} is not installed (--no-install)", spec.name))?,
            None => Self::install(&store, &spec).or_else(|e| store.best(&spec).ok_or(e))?,
        };
        Self::entry(&dir, &spec.name, command)?.ok_or_else(|| format!("{}: no package.json", dir.display()))
    }

    /// The executable in the project's `node_modules`, searched up from the current directory: `.bin/<command>` when no
    /// version was asked for, then the package itself when its version is one `spec` accepts
    fn local(spec: &Spec, command: Option<&str>) -> Option<PathBuf> {
        let cwd = std::env::current_dir().ok()?;
        let any_version = matches!(spec.want, Want::Latest);
        let bin = command.unwrap_or_else(|| Spec::unscoped(&spec.name));
        cwd.ancestors().map(|dir| dir.join("node_modules")).find_map(|modules| {
            let linked = modules.join(".bin").join(bin);
            if any_version && linked.is_file() {
                return Some(linked);
            }
            let dir = modules.join(&spec.name);
            let manifest = Self::manifest(&dir)?;
            let fits = match (&spec.want, &manifest.version) {
                (Want::Latest, _) => true,
                (Want::Range(range), Some(v)) => range.satisfies(v),
                _ => false,
            };
            fits.then(|| Self::entry(&dir, &spec.name, command).ok().flatten()).flatten()
        })
    }

    /// Resolves `spec` with the registry and installs it into the cache, unless the version picked is there already
    fn install(store: &Store, spec: &Spec) -> Result<PathBuf, String> {
        let registry = Registry::default();
        let mut tree = Tree::new(&registry, spec.clone());
        let version = tree.top()?;
        if let Some(dir) = store.get(&spec.name, &version) {
            return Ok(dir);
        }
        eprintln!("dunx: installing {}@{version}", spec.name);
        tree.resolve()?;
        store.put(&tree, &spec.name, &version)
    }

    fn manifest(dir: &Path) -> Option<Manifest> {
        let text = std::fs::read_to_string(dir.join("package.json")).ok()?;
        serde_json::from_str(&text).ok()
    }

    /// The file of executable `command` (or the package's default one) in the package at `dir`; `None` without a
    /// package.json
    fn entry(dir: &Path, package: &str, command: Option<&str>) -> Result<Option<PathBuf>, String> {
        let Some(manifest) = Self::manifest(dir) else { return Ok(None) };
        let file = manifest.bin.entry(package, command)?;
        Ok(Some(dir.join(file)))
    }
}

/// How an executable runs
#[derive(Clone, Copy)]
enum Launch {
    /// JavaScript: in this process, on the Node layer
    Js,
    /// Anything else: as a child process
    Exec,
}

impl Launch {
    /// Interpreters whose scripts are JavaScript
    const JS_INTERPRETERS: [&str; 4] = ["node", "nodejs", "bun", "dune"];
    const JS_EXTENSIONS: [&str; 3] = ["js", "cjs", "mjs"];
    /// Enough of a file to hold its `#!` line
    const HEAD: u64 = 512;

    fn run(self, file: &Path, args: &[String], jit: bool) -> ExitCode {
        match self {
            Self::Js => match Job::main(file, args.to_vec()) {
                Ok(job) => job.run(Layer::Node, jit),
                Err(e) => {
                    eprintln!("dunx: {e}");
                    ExitCode::FAILURE
                }
            },
            Self::Exec => {
                Self::executable(file);
                match std::process::Command::new(file).args(args).status() {
                    Ok(status) => status.code().and_then(|c| u8::try_from(c).ok()).map_or(ExitCode::FAILURE, ExitCode::from),
                    Err(e) => {
                        eprintln!("dunx: {}: {e}", file.display());
                        ExitCode::FAILURE
                    }
                }
            }
        }
    }

    /// Package files may come without the execute bit, as npm sets it on install
    #[cfg(unix)]
    fn executable(file: &Path) {
        use std::os::unix::fs::PermissionsExt;
        const EXECUTE: u32 = 0o111;
        if let Ok(meta) = std::fs::metadata(file)
            && meta.permissions().mode() & EXECUTE == 0
        {
            let _ = std::fs::set_permissions(file, std::fs::Permissions::from_mode(meta.permissions().mode() | EXECUTE));
        }
    }

    #[cfg(not(unix))]
    fn executable(_: &Path) {}
}

impl From<&Path> for Launch {
    /// By the `#!` line (`#!/usr/bin/env node`, `#!/usr/bin/env -S node --flag`, `#!/usr/bin/node`); without one, by
    /// the extension (none counts as JavaScript, as npm packages' bins often have none)
    fn from(file: &Path) -> Self {
        let mut head = String::new();
        let _ = std::fs::File::open(file).map(|f| f.take(Self::HEAD).read_to_string(&mut head));
        let Some(line) = head.strip_prefix("#!").and_then(|h| h.lines().next()) else {
            let ext = file.extension().and_then(|e| e.to_str());
            return match ext {
                None => Self::Js,
                Some(ext) if Self::JS_EXTENSIONS.contains(&ext) => Self::Js,
                Some(_) => Self::Exec,
            };
        };
        let mut words = line.split_whitespace().map(|w| w.rsplit('/').next().unwrap_or(w));
        let mut program = words.next();
        if program == Some("env") {
            program = words.find(|w| !w.starts_with('-'));
        }
        match program {
            Some(p) if Self::JS_INTERPRETERS.contains(&p) => Self::Js,
            _ => Self::Exec,
        }
    }
}
