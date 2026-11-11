//! node 式的宿主接口：`fs` / `path` / `process` 的原生部分用 `#[r8::object]` 绑定，`require` 与 CommonJS 包装在
//! [`PRELUDE`] 里用 JS 写。原生部分挂在不可枚举的全局 `__dune` 上，序言把它们包成模块。
use std::path::{Component, Path, PathBuf};

use r8::{Object, Rest};
use r8frame::{Agent, Error, ErrorKind, JsObject, Property, Value};

/// 序言：`require`（CommonJS，模块缓存、相对路径、`.js` / `.json` / 目录 `index.js`）、`module`、`process`。
pub const PRELUDE: &str = include_str!("prelude.js");

/// `fs` 的同步接口。
pub struct Fs;

#[r8::object("fs")]
impl Fs {
    fn read_file_sync<'gc>(a: &mut Agent<'gc, '_>, path: &str, encoding: Option<Value<'gc>>) -> Result<String, Error> {
        let _ = encoding;
        std::fs::read_to_string(path).map_err(|e| Self::io(a, "open", path, &e))
    }

    fn write_file_sync(a: &mut Agent<'_, '_>, path: &str, data: &str) -> Result<(), Error> {
        std::fs::write(path, data).map_err(|e| Self::io(a, "write", path, &e))
    }

    fn append_file_sync(a: &mut Agent<'_, '_>, path: &str, data: &str) -> Result<(), Error> {
        use std::io::Write;
        let r = std::fs::OpenOptions::new().append(true).create(true).open(path).and_then(|mut f| f.write_all(data.as_bytes()));
        r.map_err(|e| Self::io(a, "append", path, &e))
    }

    fn exists_sync(path: &str) -> bool {
        Path::new(path).exists()
    }

    fn readdir_sync(a: &mut Agent<'_, '_>, path: &str) -> Result<Vec<String>, Error> {
        let entries = std::fs::read_dir(path).map_err(|e| Self::io(a, "scandir", path, &e))?;
        let mut names: Vec<String> = entries.filter_map(Result::ok).map(|e| e.file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        Ok(names)
    }

    fn mkdir_sync(a: &mut Agent<'_, '_>, path: &str) -> Result<(), Error> {
        std::fs::create_dir_all(path).map_err(|e| Self::io(a, "mkdir", path, &e))
    }

    fn unlink_sync(a: &mut Agent<'_, '_>, path: &str) -> Result<(), Error> {
        std::fs::remove_file(path).map_err(|e| Self::io(a, "unlink", path, &e))
    }

    /// 是不是目录（`require` 解析目录模块用）。
    fn is_directory(path: &str) -> bool {
        Path::new(path).is_dir()
    }

    #[js(skip)]
    fn io(a: &Agent<'_, '_>, op: &str, path: &str, e: &std::io::Error) -> Error {
        a.throw(ErrorKind::Error, &format!("{e}, {op} '{path}'"))
    }
}

/// `path`（POSIX）。
pub struct Paths;

#[r8::object("path")]
impl Paths {
    fn join<'gc>(parts: Rest<'gc>, a: &mut Agent<'gc, '_>) -> Result<String, Error> {
        let mut p = PathBuf::new();
        for v in parts.0 {
            p.push(String::from(&**a.to_string(v)?));
        }
        Ok(Self::normalize_path(&p))
    }

    fn resolve<'gc>(parts: Rest<'gc>, a: &mut Agent<'gc, '_>) -> Result<String, Error> {
        let mut p = std::env::current_dir().unwrap_or_default();
        for v in parts.0 {
            p.push(String::from(&**a.to_string(v)?));
        }
        Ok(Self::normalize_path(&p))
    }

    fn normalize(path: &str) -> String {
        Self::normalize_path(Path::new(path))
    }

    fn dirname(path: &str) -> String {
        Path::new(path).parent().map_or_else(|| String::from("."), |p| if p.as_os_str().is_empty() { String::from(".") } else { p.display().to_string() })
    }

    fn basename(path: &str, ext: Option<String>) -> String {
        let base = Path::new(path).file_name().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
        match ext {
            Some(e) if base.ends_with(e.as_str()) && base != e => base[..base.len() - e.len()].to_string(),
            _ => base,
        }
    }

    fn extname(path: &str) -> String {
        Path::new(path).extension().map(|e| format!(".{}", e.to_string_lossy())).unwrap_or_default()
    }

    fn is_absolute(path: &str) -> bool {
        Path::new(path).is_absolute()
    }

    /// 去掉 `.` / `..`（不访问文件系统）。
    #[js(skip)]
    fn normalize_path(p: &Path) -> String {
        let mut out = PathBuf::new();
        for c in p.components() {
            match c {
                Component::CurDir => {}
                Component::ParentDir => {
                    if !out.pop() {
                        out.push("..");
                    }
                }
                other => out.push(other.as_os_str()),
            }
        }
        let s = out.display().to_string();
        if s.is_empty() { String::from(".") } else { s }
    }
}

/// `process` 的方法（属性在 [`Node::install`] 里放）。
pub struct Process;

#[r8::object("process")]
impl Process {
    fn exit(code: Option<i32>) {
        std::process::exit(code.unwrap_or(0));
    }

    fn cwd() -> String {
        std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()
    }

    fn uptime(a: &mut Agent<'_, '_>) -> f64 {
        a.platform.now().as_secs_f64()
    }
}

/// 装 node 式的全局：`__dune`（原生部分）与 `process`。
pub struct Node;

impl Node {
    /// `argv` 是脚本路径之后的参数（前面补上 `dune` 与脚本路径）。
    pub fn install<'gc>(a: &mut Agent<'gc, '_>, script: &str, args: &[String]) {
        let global = a.realm.global;
        let native = a.object();
        global.borrow_mut(a.mc).define("__dune", Property::hidden(native.into()));
        Fs::install(a, native);
        Paths::install(a, native);
        let process = Process::install(a, global);
        let mut argv: Vec<Value<'gc>> = vec![a.string("dune").into(), a.string(script).into()];
        argv.extend(args.iter().map(|s| Value::from(a.string(s))));
        let argv = a.array(argv);
        let env = a.object();
        for (k, v) in std::env::vars() {
            let v = a.string(&v);
            env.borrow_mut(a.mc).set(&k, v.into());
        }
        let props: [(&str, Value<'gc>); 5] = [
            ("argv", argv.into()),
            ("env", env.into()),
            ("platform", a.string(std::env::consts::OS).into()),
            ("version", a.string(concat!("v", env!("CARGO_PKG_VERSION"))).into()),
            ("pid", f64::from(std::process::id()).into()),
        ];
        for (k, v) in props {
            Self::set(a, process, k, v);
        }
    }

    fn set<'gc>(a: &Agent<'gc, '_>, o: JsObject<'gc>, k: &str, v: Value<'gc>) {
        o.borrow_mut(a.mc).define(k, Property::data(v));
    }
}
