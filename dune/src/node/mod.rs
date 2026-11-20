//! The Node-compatible layer, on top of [`Web`](crate::web::Web): `process`, `require` / `module` (CommonJS), and the
//! built-in modules `fs` (+ `fs/promises`), `path`, `os`, `events`, `util`, `buffer` (`Buffer`), `assert`, `url`,
//! `timers` (+ `timers/promises`), `child_process`, `crypto`, `module`, `string_decoder`, `perf_hooks`.
//!
//! The parts that need the machine are bound here in Rust; the modules themselves are JavaScript in this directory
//! (the prelude), registered for `require`, and served to `import` by [`NodePlatform`].
mod fs;

use std::collections::BTreeMap;
use std::fmt::Write;
use std::process::Command;
use std::time::Duration;

use j8::frame::{Agent, Error, Platform, Pool, Task, Value};
use j8::{Bytes, IntoJs};

pub use fs::Fs;

use crate::host::Io;
use crate::web::Web;

/// The prelude: the module system first, then the modules in dependency order, then `process` and the globals
const PRELUDE: &str = concat!(
    include_str!("loader.js"),
    include_str!("events.js"),
    include_str!("buffer.js"),
    include_str!("util.js"),
    include_str!("path.js"),
    include_str!("fs.js"),
    include_str!("misc.js"),
    include_str!("assert.js"),
    include_str!("process.js"),
);

/// `process`: the parts that touch the OS (`argv`, `env` … are set per run by [`Dune::argv`])
pub struct Process;

#[j8::object("process")]
impl Process {
    fn cwd() -> String {
        std::env::current_dir().map(|p| p.display().to_string()).unwrap_or_default()
    }

    fn chdir(dir: &str) -> Io<()> {
        Io::new(std::env::set_current_dir(dir), "chdir", dir)
    }

    /// Seconds since the runtime started
    fn uptime(a: &mut Agent<'_, '_>) -> f64 {
        a.platform.now().as_secs_f64()
    }

    /// `memoryUsage().rss`, from /proc
    fn rss() -> f64 {
        let pages = std::fs::read_to_string("/proc/self/statm").ok().and_then(|s| s.split_whitespace().nth(1).and_then(|n| n.parse::<f64>().ok()));
        pages.unwrap_or(0.0) * 4096.0
    }
}

/// `os`
pub struct Os;

#[j8::object("os")]
impl Os {
    fn hostname() -> String {
        std::fs::read_to_string("/proc/sys/kernel/hostname").map_or_else(|_| String::from("localhost"), |s| s.trim().to_owned())
    }

    fn release() -> String {
        std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|s| s.trim().to_owned()).unwrap_or_default()
    }

    fn homedir() -> String {
        std::env::var("HOME").unwrap_or_else(|_| String::from("/"))
    }

    fn tmpdir() -> String {
        std::env::temp_dir().display().to_string()
    }

    fn cores() -> usize {
        std::thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get)
    }

    /// `[total, free]` bytes, from /proc/meminfo
    fn memory() -> Vec<f64> {
        let info = std::fs::read_to_string("/proc/meminfo").unwrap_or_default();
        let field = |name: &str| info.lines().find(|l| l.starts_with(name)).and_then(|l| l.split_whitespace().nth(1)).and_then(|n| n.parse::<f64>().ok()).unwrap_or(0.0) * 1024.0;
        vec![field("MemTotal:"), field("MemAvailable:")]
    }

    /// `[1, 5, 15]` minute load averages
    fn loadavg() -> Vec<f64> {
        let text = std::fs::read_to_string("/proc/loadavg").unwrap_or_default();
        text.split_whitespace().take(3).map(|n| n.parse().unwrap_or(0.0)).collect()
    }

    fn uptime() -> f64 {
        std::fs::read_to_string("/proc/uptime").ok().and_then(|s| s.split_whitespace().next().and_then(|n| n.parse().ok())).unwrap_or(0.0)
    }

    fn username() -> String {
        std::env::var("USER").unwrap_or_default()
    }
}

/// What a child process left: `[status, stdout, stderr]` (status `null` when a signal ended it)
pub struct Finished(std::process::Output);

impl<'gc> IntoJs<'gc> for Finished {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        let status = self.0.status.code().into_js(a)?;
        let out = vec![status, Bytes(self.0.stdout).into_js(a)?, Bytes(self.0.stderr).into_js(a)?];
        out.into_js(a)
    }
}

/// `child_process`, synchronous forms
pub struct Child;

#[j8::object("child")]
impl Child {
    /// Runs `file` with `args` (through `/bin/sh -c` when `shell`), feeding it `input`
    fn spawn_sync(file: String, args: Vec<String>, shell: bool, cwd: Option<String>, input: Option<Bytes>) -> Io<Finished> {
        use std::io::Write;
        use std::process::Stdio;
        let mut cmd = if shell {
            let mut c = Command::new("/bin/sh");
            c.arg("-c").arg(std::iter::once(file.clone()).chain(args).collect::<Vec<_>>().join(" "));
            c
        } else {
            let mut c = Command::new(&file);
            c.args(args);
            c
        };
        if let Some(dir) = cwd {
            cmd.current_dir(dir);
        }
        cmd.stdin(if input.is_some() { Stdio::piped() } else { Stdio::inherit() }).stdout(Stdio::piped()).stderr(Stdio::piped());
        let run = || -> std::io::Result<std::process::Output> {
            let mut child = cmd.spawn()?;
            if let (Some(mut stdin), Some(input)) = (child.stdin.take(), input) {
                stdin.write_all(&input.0)?;
            }
            child.wait_with_output()
        };
        Io::new(run().map(Finished), "spawnSync", &file)
    }
}

/// Built-in modules written in JavaScript, served to `import` as `node:<name>` (or the bare name) with these named
/// exports; the module object is `__j8_native.builtin(name)`
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
}

impl Builtin {
    const ALL: [Self; 17] = [Self::Fs, Self::FsPromises, Self::Path, Self::Os, Self::Events, Self::Util, Self::Buffer, Self::Assert, Self::Url, Self::Timers, Self::TimersPromises, Self::ChildProcess, Self::Crypto, Self::Module, Self::Process, Self::StringDecoder, Self::PerfHooks];

    /// (name, named exports)
    const fn spec(self) -> (&'static str, &'static [&'static str]) {
        match self {
            Self::Fs => ("fs", &["readFileSync", "writeFileSync", "appendFileSync", "existsSync", "statSync", "lstatSync", "readdirSync", "mkdirSync", "rmSync", "rmdirSync", "unlinkSync", "renameSync", "copyFileSync", "realpathSync", "readlinkSync", "symlinkSync", "chmodSync", "mkdtempSync", "accessSync", "readFile", "writeFile", "appendFile", "stat", "lstat", "readdir", "mkdir", "rm", "rmdir", "unlink", "rename", "copyFile", "realpath", "access", "exists", "promises", "constants", "Stats", "Dirent"]),
            Self::FsPromises => ("fs/promises", &["readFile", "writeFile", "appendFile", "stat", "lstat", "readdir", "mkdir", "rm", "rmdir", "unlink", "rename", "copyFile", "realpath", "readlink", "symlink", "chmod", "mkdtemp", "access", "constants"]),
            Self::Path => ("path", &["join", "resolve", "normalize", "dirname", "basename", "extname", "isAbsolute", "relative", "parse", "format", "sep", "delimiter", "posix", "toNamespacedPath"]),
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
        }
    }

    /// The ES module source standing for this built-in
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

/// Serves the built-in modules to `import`, everything else through `inner`
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

impl<P: Platform> Platform for NodePlatform<P> {
    fn now(&self) -> Duration {
        self.inner.now()
    }

    fn wall_clock(&self) -> Duration {
        self.inner.wall_clock()
    }

    fn load_module(&self, referrer: &str, specifier: &str) -> Option<(String, String)> {
        match Builtin::try_from(specifier) {
            Ok(b) => Some((format!("node:{}", b.spec().0), b.source())),
            Err(()) => self.inner.load_module(referrer, specifier),
        }
    }

    fn print(&self, line: &str) {
        self.inner.print(line);
    }
}

j8::runtime! {
    /// The Node-compatible runtime: everything [`Web`] has, plus `process`, `require` and the `node:` modules
    pub Dune: Web {
        globals: [Process],
        natives: [Fs, Os, Child],
        prelude: PRELUDE,
    }
}

impl Dune {
    /// This run's `process.argv` (`dune`, the script, its arguments), `env`, `platform`, `arch`, `pid`, `execPath`
    pub fn argv(rt: &mut j8::Runtime<'_>, script: Option<&str>, args: &[String]) -> Result<(), Error> {
        let exe = std::env::current_exe().map_or_else(|_| String::from("dune"), |p| p.display().to_string());
        let all: Vec<String> = std::iter::once(exe.clone()).chain(script.map(String::from)).chain(args.iter().cloned()).collect();
        rt.set("process.argv", all)?;
        rt.set("process.execPath", exe)?;
        rt.set("process.env", std::env::vars().collect::<BTreeMap<String, String>>())?;
        rt.set("process.platform", std::env::consts::OS)?;
        rt.set("process.arch", Self::arch())?;
        rt.set("process.pid", std::process::id())
    }

    /// Node's name for this CPU architecture
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

