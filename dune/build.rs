use std::fmt::Write;
use std::path::{Path, PathBuf};

const ROOTS: [&str; 2] = ["src/node/shim", "src/node/lib"];
const EXTENSION: &str = "js";

struct Tree {
    files: Vec<(String, PathBuf)>,
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
                if !self.files.iter().any(|(known, _)| *known == id) {
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
    let mut tree = Tree { files: Vec::new() };
    for root in ROOTS {
        println!("cargo::rerun-if-changed={root}");
        tree.walk(Path::new(root), Path::new(root));
    }
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR")).join("lib.rs");
    std::fs::write(out, tree.source()).expect("writing lib.rs");
}
