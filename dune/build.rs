use std::fmt::Write;
use std::path::{Path, PathBuf};

const ROOTS: [&str; 2] = ["src/node/shim", "src/node/lib"];
const EXTENSION: &str = "js";
const UNIX_FAMILY: &str = "unix";

enum Only {
    Exact(&'static str),
    Prefix(&'static str),
}

impl Only {
    const UNIX: [Self; 4] = [Self::Exact("fs"), Self::Exact("fs/promises"), Self::Prefix("internal/fs/"), Self::Prefix("internal/binding/fs")];

    fn matches(&self, id: &str) -> bool {
        match self {
            Self::Exact(e) => id == *e,
            Self::Prefix(p) => id.starts_with(p),
        }
    }
}

struct Tree {
    files: Vec<(String, PathBuf)>,
    unix: bool,
}

impl Tree {
    fn walk(&mut self, root: &Path, dir: &Path) {
        let Ok(entries) = std::fs::read_dir(dir) else { return };
        for path in entries.filter_map(Result::ok).map(|e| e.path()) {
            if path.is_dir() {
                self.walk(root, &path);
            } else if path.extension().is_some_and(|e| e == EXTENSION) {
                let relative = path.strip_prefix(root).unwrap_or(&path).with_extension("");
                let id = relative.components().map(|c| c.as_os_str().to_string_lossy()).collect::<Vec<_>>().join("/");
                let skipped = !self.unix && Only::UNIX.iter().any(|o| o.matches(&id));
                if !skipped && !self.files.iter().any(|(known, _)| *known == id) {
                    self.files.push((id, path.canonicalize().unwrap_or(path)));
                }
            }
        }
    }

    fn source(mut self) -> String {
        self.files.sort();
        let mut out = format!("pub const LIB: [(&str, &str); {}] = [\n", self.files.len());
        for (id, path) in &self.files {
            let _ = writeln!(out, "    ({id:?}, include_str!({:?})),", path.display().to_string());
        }
        out.push_str("];\n");
        out
    }
}

fn main() {
    let unix = std::env::var("CARGO_CFG_TARGET_FAMILY").is_ok_and(|f| f.split(',').any(|f| f == UNIX_FAMILY));
    let mut tree = Tree { files: Vec::new(), unix };
    for root in ROOTS {
        println!("cargo::rerun-if-changed={root}");
        tree.walk(Path::new(root), Path::new(root));
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("lib.rs");
    std::fs::write(out, tree.source()).expect("writing lib.rs");
}
