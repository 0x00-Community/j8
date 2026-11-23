mod esm;
mod bytes;
mod v8;
mod vendor;
mod uvfs;

use std::collections::BTreeMap;
use std::fmt::Write;
use std::process::Command;
use std::time::Duration;

use j8::frame::{Agent, Error, Platform, Pool, Task, Value};
use j8::{Bytes, IntoJs};

pub use bytes::Raw;
pub use v8::V8;
pub use vendor::Lib;
pub use uvfs::Uvfs;

use crate::host::Io;
use crate::web::Web;

const PRELUDE: &str = concat!(
    include_str!("loader.js"),
    include_str!("internal.js"),
    include_str!("misc.js"),
    include_str!("process.js"),
);

pub struct Process;

#[j8::object("process")]
impl Process {
    fn cwd() -> String {
        std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()
    }

    fn chdir(dir: &str) -> Io<()> {
        Io::new(std::env::set_current_dir(dir), "chdir", dir)
    }

    fn uptime(a: &mut Agent<'_, '_>) -> f64 {
        a.platform.now().as_secs_f64()
    }

    #[expect(clippy::cast_precision_loss, reason = "a resident set fits a double")]
    fn rss() -> f64 {
        let (Ok(pid), Ok(mut system)) = (sysinfo::get_current_pid(), sysinfo::System::new()) else { return 0.0 };
        system.refresh_processes(sysinfo::ProcessesToUpdate::Some(&[pid]), true);
        system.process(pid).map_or(0.0, |p| p.memory() as f64)
    }
}

pub struct Os;

#[j8::object("os")]
impl Os {
    fn hostname() -> String {
        sysinfo::System::host_name().unwrap_or_else(|_| String::from("localhost"))
    }

    fn release() -> String {
        sysinfo::System::kernel_version().unwrap_or_default()
    }

    fn homedir() -> String {
        std::env::home_dir().map_or_else(|| String::from("/"), |p| p.display().to_string())
    }

    fn tmpdir() -> String {
        std::env::temp_dir().display().to_string()
    }

    fn cores() -> usize {
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    }

    #[expect(clippy::cast_precision_loss, reason = "memory sizes fit a double")]
    fn memory() -> Vec<f64> {
        let Ok(mut system) = sysinfo::System::new() else { return vec![0.0, 0.0] };
        system.refresh_memory();
        vec![system.total_memory() as f64, system.available_memory() as f64]
    }

    fn loadavg() -> Vec<f64> {
        sysinfo::System::load_average().map_or_else(|_| vec![0.0; 3], |l| vec![l.one, l.five, l.fifteen])
    }

    #[expect(clippy::cast_precision_loss, reason = "seconds since boot fit a double")]
    fn uptime() -> f64 {
        sysinfo::System::uptime().map_or(0.0, |s| s as f64)
    }

    fn username() -> String {
        whoami::username().unwrap_or_default()
    }

    fn version() -> &'static str {
        env!("CARGO_PKG_VERSION")
    }

    fn platform() -> &'static str {
        crate::cli::Os::HOST.into()
    }

    fn arch() -> &'static str {
        Dune::arch()
    }

    fn env() -> BTreeMap<String, String> {
        std::env::vars().collect()
    }

    fn handle_type(fd: u32) -> u32 {
        Handle::guess(fd).into()
    }
}

#[derive(Clone, Copy)]
enum Handle {
    Tty,
    File,
    Pipe,
    Unknown,
}

impl Handle {
    fn guess(fd: u32) -> Self {
        use std::io::IsTerminal;
        use uvfs::Dirent;
        let Ok(fd) = i32::try_from(fd) else { return Self::Unknown };
        let file = uvfs::Fd(fd).file();
        if file.is_terminal() {
            return Self::Tty;
        }
        match file.metadata().map(|m| Dirent::from(m.file_type())) {
            Ok(Dirent::File | Dirent::Dir | Dirent::Char | Dirent::Block) => Self::File,
            Ok(Dirent::Fifo | Dirent::Socket) => Self::Pipe,
            Err(_) if cfg!(windows) => Self::Pipe,
            _ => Self::Unknown,
        }
    }
}

impl From<Handle> for u32 {
    fn from(h: Handle) -> Self {
        match h {
            Handle::Tty => 1,
            Handle::File => 3,
            Handle::Pipe => 4,
            Handle::Unknown => 5,
        }
    }
}

pub struct Finished(std::process::Output);

impl<'scope> IntoJs<'scope> for Finished {
    fn into_js(self, a: &mut Agent<'scope, '_>) -> Result<Value<'scope>, Error> {
        let status = self.0.status.code().into_js(a)?;
        let out = vec![status, Bytes(self.0.stdout).into_js(a)?, Bytes(self.0.stderr).into_js(a)?];
        out.into_js(a)
    }
}

#[derive(serde::Deserialize, Clone, Copy, Default)]
#[serde(rename_all = "lowercase")]
enum Stream {
    #[default]
    Pipe,
    Inherit,
    Ignore,
}

impl Stream {
    fn stdio(self, pipe: bool) -> std::process::Stdio {
        match (self, pipe) {
            (Self::Pipe, true) => std::process::Stdio::piped(),
            (Self::Pipe | Self::Inherit, _) => std::process::Stdio::inherit(),
            (Self::Ignore, _) => std::process::Stdio::null(),
        }
    }
}

#[derive(serde::Deserialize, Default)]
#[serde(default)]
struct Spawn {
    shell: bool,
    cwd: Option<String>,
    env: Option<BTreeMap<String, String>>,
    stdio: [Stream; 3],
}

pub struct Child;

#[j8::object("child")]
impl Child {
    fn spawn_sync(file: String, args: Vec<String>, options: j8::Json<Spawn>, input: Option<Bytes>) -> Io<Finished> {
        use std::io::Write;
        let o = options.0;
        let mut cmd = if o.shell {
            let mut c = Command::new("/bin/sh");
            c.arg("-c").arg(std::iter::once(file.clone()).chain(args).collect::<Vec<_>>().join(" "));
            c
        } else {
            let mut c = Command::new(&file);
            c.args(args);
            c
        };
        if let Some(dir) = o.cwd {
            cmd.current_dir(dir);
        }
        if let Some(env) = o.env {
            cmd.env_clear().envs(env);
        }
        let [stdin, stdout, stderr] = o.stdio;
        cmd.stdin(stdin.stdio(input.is_some())).stdout(stdout.stdio(true)).stderr(stderr.stdio(true));
        let run = || -> std::io::Result<std::process::Output> {
            let mut child = cmd.spawn()?;
            if let (Some(mut pipe), Some(input)) = (child.stdin.take(), input) {
                pipe.write_all(&input.0)?;
            }
            child.wait_with_output()
        };
        Io::new(run().map(Finished), "spawnSync", &file)
    }
}

#[derive(Clone, Copy)]
enum Builtin {
    Fs,
    FsPromises,
    Path,
    Os,
    Events,
    Util,
    Buffer,
    Assert,
    Url,
    Timers,
    TimersPromises,
    ChildProcess,
    Crypto,
    Module,
    Process,
    StringDecoder,
    PerfHooks,
    Stream,
    StreamPromises,
    StreamConsumers,
    AsyncHooks,
    Querystring,
}

impl Builtin {
    const ALL: [Self; 22] = [Self::Fs, Self::FsPromises, Self::Path, Self::Os, Self::Events, Self::Util, Self::Buffer, Self::Assert, Self::Url, Self::Timers, Self::TimersPromises, Self::ChildProcess, Self::Crypto, Self::Module, Self::Process, Self::StringDecoder, Self::PerfHooks, Self::Stream, Self::StreamPromises, Self::StreamConsumers, Self::AsyncHooks, Self::Querystring];

    const fn spec(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Fs => ("fs", &["readFileSync", "writeFileSync", "appendFileSync", "existsSync", "statSync", "lstatSync", "readdirSync", "mkdirSync", "rmSync", "rmdirSync", "unlinkSync", "renameSync", "copyFileSync", "realpathSync", "readlinkSync", "symlinkSync", "chmodSync", "mkdtempSync", "accessSync", "readFile", "writeFile", "appendFile", "stat", "lstat", "readdir", "mkdir", "rm", "rmdir", "unlink", "rename", "copyFile", "realpath", "access", "exists", "promises", "constants", "Stats", "Dirent"]),
            Self::FsPromises => ("fs/promises", &["readFile", "writeFile", "appendFile", "stat", "lstat", "readdir", "mkdir", "rm", "rmdir", "unlink", "rename", "copyFile", "realpath", "readlink", "symlink", "chmod", "mkdtemp", "access", "constants"]),
            Self::Path => ("path", &["join", "resolve", "normalize", "dirname", "basename", "extname", "isAbsolute", "relative", "parse", "format", "sep", "delimiter", "posix", "win32", "toNamespacedPath", "matchesGlob"]),
            Self::Os => ("os", &["EOL", "platform", "type", "arch", "release", "hostname", "homedir", "tmpdir", "cpus", "availableParallelism", "totalmem", "freemem", "loadavg", "uptime", "endianness", "userInfo", "networkInterfaces", "machine", "version", "devNull"]),
            Self::Events => ("events", &["EventEmitter", "once", "on", "getEventListeners", "defaultMaxListeners", "errorMonitor", "captureRejectionSymbol"]),
            Self::Util => ("util", &["format", "inspect", "promisify", "callbackify", "inherits", "deprecate", "isDeepStrictEqual", "types", "TextEncoder", "TextDecoder", "debuglog", "stripVTControlCharacters", "styleText", "parseArgs", "toUSVString"]),
            Self::Buffer => ("buffer", &["Buffer", "Blob", "File", "atob", "btoa", "constants", "kMaxLength", "isUtf8", "isAscii"]),
            Self::Assert => ("assert", &["ok", "equal", "notEqual", "strictEqual", "notStrictEqual", "deepEqual", "notDeepEqual", "deepStrictEqual", "notDeepStrictEqual", "throws", "doesNotThrow", "rejects", "doesNotReject", "match", "doesNotMatch", "fail", "ifError", "AssertionError", "strict"]),
            Self::Url => ("url", &["URL", "URLSearchParams", "fileURLToPath", "pathToFileURL", "format", "parse"]),
            Self::Timers => ("timers", &["setTimeout", "setInterval", "setImmediate", "clearTimeout", "clearInterval", "clearImmediate"]),
            Self::TimersPromises => ("timers/promises", &["setTimeout", "setImmediate", "setInterval", "scheduler"]),
            Self::ChildProcess => ("child_process", &["spawnSync", "execSync", "execFileSync", "exec", "execFile"]),
            Self::Crypto => ("crypto", &["randomBytes", "randomUUID", "randomInt", "getRandomValues", "webcrypto", "timingSafeEqual"]),
            Self::Module => ("module", &["createRequire", "builtinModules", "isBuiltin"]),
            Self::Process => ("process", &["argv", "env", "exit", "cwd", "platform", "nextTick", "stdout", "stderr", "version", "versions", "pid", "hrtime"]),
            Self::StringDecoder => ("string_decoder", &["StringDecoder"]),
            Self::PerfHooks => ("perf_hooks", &["performance", "PerformanceEntry"]),
            Self::Stream => ("stream", &["Stream", "Readable", "Writable", "Duplex", "Transform", "PassThrough", "pipeline", "finished", "compose", "addAbortSignal", "destroy", "isDestroyed", "isDisturbed", "isErrored", "isReadable", "isWritable", "duplexPair", "promises", "getDefaultHighWaterMark", "setDefaultHighWaterMark"]),
            Self::StreamPromises => ("stream/promises", &["pipeline", "finished"]),
            Self::Querystring => ("querystring", &["decode", "encode", "escape", "parse", "stringify", "unescape", "unescapeBuffer"]),
            Self::StreamConsumers => ("stream/consumers", &["arrayBuffer", "blob", "buffer", "json", "text", "bytes"]),
            Self::AsyncHooks => ("async_hooks", &["AsyncLocalStorage", "AsyncResource", "createHook", "executionAsyncId", "triggerAsyncId", "executionAsyncResource", "asyncWrapProviders"]),
        }
    }

    fn source(self) -> String {
        let (name, exports) = self.spec();
        let mut out = format!("const m = globalThis.{}.builtin({name:?});\nexport default m;\n", j8::NATIVE);
        for e in exports {
            let _ = writeln!(out, "export const {e} = m.{e};");
        }
        out
    }
}

impl TryFrom<&str> for Builtin {
    type Error = ();

    fn try_from(specifier: &str) -> Result<Self, ()> {
        let name = specifier.strip_prefix("node:").unwrap_or(specifier);
        Self::ALL.into_iter().find(|b| b.spec().0 == name).ok_or(())
    }
}

pub struct NodePlatform<P> {
    inner: P,
}

impl<P> From<P> for NodePlatform<P> {
    fn from(inner: P) -> Self {
        Self { inner }
    }
}

impl<P: Pool> Pool for NodePlatform<P> {
    fn threads(&self) -> usize {
        self.inner.threads()
    }

    fn run(&self, jobs: Vec<Task<'_>>) {
        self.inner.run(jobs);
    }
}

impl<P: Platform> NodePlatform<P> {
    fn file(&self, referrer: &str, path: &str) -> Option<(String, String)> {
        let (name, text) = self.inner.load_module(referrer, path)?;
        let text = if esm::Package::commonjs(std::path::Path::new(&name)) {
            let file = crate::session::Session::quote(&name);
            format!("const m = globalThis.{}.builtin(\"module\").createRequire({file})({file});\nexport default m;\n", j8::NATIVE)
        } else {
            text
        };
        Some((esm::FileUrl::of(&name), text))
    }
}

impl<P: Platform> Platform for NodePlatform<P> {
    fn now(&self) -> Duration {
        self.inner.now()
    }

    fn wall_clock(&self) -> Duration {
        self.inner.wall_clock()
    }

    fn load_module(&self, referrer: &str, specifier: &str) -> Option<(String, String)> {
        if let Ok(b) = Builtin::try_from(specifier) {
            return Some((format!("node:{}", b.spec().0), b.source()));
        }
        let referrer = esm::FileUrl::path(referrer);
        let referrer = referrer.as_ref();
        let from = std::path::Path::new(referrer);
        let specifier = esm::FileUrl::path(specifier);
        match esm::Specifier::from(specifier.as_ref()) {
            esm::Specifier::Path(path) => self.file(referrer, path),
            esm::Specifier::Private(name) => {
                let package = esm::Package::holding(from)?;
                let target = package.import(name)?;
                match esm::Specifier::from(target.as_str()) {
                    esm::Specifier::Path(path) => self.file(referrer, &package.dir().join(path).display().to_string()),
                    _ => self.load_module(&package.dir().join("package.json").display().to_string(), &target),
                }
            }
            esm::Specifier::Bare { name, subpath } => {
                let file = esm::Package::find(from, name)?.export(&subpath)?;
                self.file(referrer, &file.display().to_string())
            }
        }
    }

    fn print(&self, line: &str) {
        self.inner.print(line);
    }
}

j8::runtime! {
    pub Dune: Web {
        globals: [Process],
        natives: [Os, Child, V8, Lib, Raw, Uvfs],
        prelude: PRELUDE,
    }
}

impl Dune {
    pub fn argv(rt: &mut j8::Runtime<'_>, script: Option<&str>, args: &[String]) -> Result<(), Error> {
        let exe = std::env::current_exe().map_or_else(|_| String::from("dune"), |p| Self::exec_path(&p).display().to_string());
        let all: Vec<String> = std::iter::once(exe.clone()).chain(script.map(String::from)).chain(args.iter().cloned()).collect();
        rt.set("process.argv", all)?;
        rt.set("process.execPath", exe)?;
        rt.set("process.pid", std::process::id())
    }

    fn exec_path(exe: &std::path::Path) -> std::path::PathBuf {
        const RUNNER: &str = "dunx";
        const RUNTIME: &str = "dune";
        match exe.file_stem().and_then(|s| s.to_str()) {
            Some(RUNNER) => exe.with_file_name(RUNTIME).with_extension(exe.extension().unwrap_or_default()),
            _ => exe.to_path_buf(),
        }
    }

    const fn arch() -> &'static str {
        if cfg!(target_arch = "x86_64") {
            "x64"
        } else if cfg!(target_arch = "aarch64") {
            "arm64"
        } else if cfg!(target_arch = "x86") {
            "ia32"
        } else {
            std::env::consts::ARCH
        }
    }
}
