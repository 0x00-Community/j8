//! test262 一致性测试（`harness = false`，挂在 dune 包下：`cargo test -p dune --test test262`）。
//!
//! - 不带参数：只跑基线 `tests/test262.passing` 里的用例，有掉下去的就失败（回归检查）
//! - 带参数：跑 `tests/test262/test/` 下这些目录 / 文件，如 `-- language/expressions/addition built-ins/Math`
//! - `R8_TEST262_UPDATE=1`：把这次跑到的用例的结果写回基线（没跑到的条目保留）
//! - `R8_TEST262_VERBOSE=1`：逐条打印没过的原因
//!
//! 每个用例在子进程里跑（本测试二进制带 `R8_TEST262_CASE` 再起一次），死循环靠超时杀、崩溃不连累别的用例。
//! 模式：只跑非严格模式，`onlyStrict` 的跑严格模式；`module` 跳过。
use std::collections::BTreeSet;
use std::io::Read;
use std::num::NonZeroUsize;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use r8builtins::Builtins;
use r8executor::Executor;
use r8frame::{CreateParams, Error, Isolate, Platform, Source, Task};
use serde::Deserialize;

/// test262 检出（子模块）
const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/test262");
/// 基线：过了的用例，一行一个（相对 `test/`）
const BASELINE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/test262.passing");
/// 子进程要跑的用例
const CASE_ENV: &str = "R8_TEST262_CASE";
const UPDATE_ENV: &str = "R8_TEST262_UPDATE";
const VERBOSE_ENV: &str = "R8_TEST262_VERBOSE";
/// 开 JIT（阈值 0：每个函数第一次调用就编），拿全部用例对拍机器码
const JIT_ENV: &str = "R8_TEST262_JIT";
/// 单个用例的时限
const TIMEOUT: Duration = Duration::from_secs(10);
/// 等子进程时的轮询间隔
const POLL: Duration = Duration::from_millis(2);
/// 异步用例跑完时 `$DONE` 打出的行
const ASYNC_DONE: &str = "Test262:AsyncTestComplete";
/// 汇总表按路径前几段分组
const GROUP_DEPTH: usize = 2;

/// 用例头上 `/*--- … ---*/` 里的元数据（只取用得到的键）。
#[derive(Deserialize, Default)]
struct Meta {
    #[serde(default)]
    includes: Vec<String>,
    #[serde(default)]
    flags: Vec<Flag>,
    negative: Option<Negative>,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
enum Flag {
    #[serde(rename = "onlyStrict")]
    OnlyStrict,
    #[serde(rename = "noStrict")]
    NoStrict,
    #[serde(rename = "module")]
    Module,
    #[serde(rename = "raw")]
    Raw,
    #[serde(rename = "async")]
    Async,
    #[serde(other)]
    Other,
}

#[derive(Deserialize)]
struct Negative {
    phase: Phase,
    #[serde(rename = "type")]
    kind: ErrorType,
}

#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Phase {
    Parse,
    Resolution,
    Runtime,
}

impl Phase {
    const fn name(self) -> &'static str {
        match self {
            Self::Parse => "parse",
            Self::Resolution => "resolution",
            Self::Runtime => "runtime",
        }
    }
}

/// 期望抛出的错误构造器。
#[derive(Deserialize, Clone, Copy, PartialEq, Eq)]
enum ErrorType {
    SyntaxError,
    TypeError,
    ReferenceError,
    RangeError,
    EvalError,
    URIError,
    Test262Error,
    #[serde(other)]
    Other,
}

impl ErrorType {
    const fn name(self) -> &'static str {
        match self {
            Self::SyntaxError => "SyntaxError",
            Self::TypeError => "TypeError",
            Self::ReferenceError => "ReferenceError",
            Self::RangeError => "RangeError",
            Self::EvalError => "EvalError",
            Self::URIError => "URIError",
            Self::Test262Error => "Test262Error",
            Self::Other => "?",
        }
    }
}

/// 一个用例的结果；子进程用退出码回报。
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
enum Outcome {
    Pass = 0,
    Fail = 1,
    /// r8 报 `Error::Unsupported`
    Unsupported = 2,
    /// 没跑（`module`）
    Skip = 3,
    Timeout = 4,
    /// 子进程崩了（panic / 栈溢出 / 被信号杀）
    Crash = 5,
}

impl Outcome {
    const ALL: [Self; 6] = [Self::Pass, Self::Fail, Self::Unsupported, Self::Skip, Self::Timeout, Self::Crash];

    const fn name(self) -> &'static str {
        match self {
            Self::Pass => "pass",
            Self::Fail => "fail",
            Self::Unsupported => "unsupported",
            Self::Skip => "skip",
            Self::Timeout => "timeout",
            Self::Crash => "crash",
        }
    }
}

impl From<Outcome> for ExitCode {
    fn from(o: Outcome) -> Self {
        ExitCode::from(o as u8)
    }
}

impl TryFrom<i32> for Outcome {
    type Error = ();

    fn try_from(code: i32) -> Result<Self, ()> {
        Self::ALL.into_iter().find(|o| i32::from(*o as u8) == code).ok_or(())
    }
}

/// 截下 `print` / `console.log` 的单线程平台。
struct Capture {
    start: Instant,
    lines: Mutex<Vec<String>>,
}

impl Platform for Capture {
    fn wall_clock(&self) -> Duration {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default()
    }

    /// 模块：相对路径按引用者（相对 `test/` 或绝对路径）所在目录解析。
    fn load_module(&self, referrer: &str, specifier: &str) -> Option<(String, String)> {
        let referrer = Suite::test_dir().join(referrer);
        let base = referrer.parent()?;
        let path = std::fs::canonicalize(base.join(specifier)).ok()?;
        let text = std::fs::read_to_string(&path).ok()?;
        Some((path.display().to_string(), text))
    }

    fn workers(&self) -> NonZeroUsize {
        NonZeroUsize::MIN
    }

    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    fn print(&self, line: &str) {
        self.lines.lock().expect("an unpoisoned log").push(line.into());
    }

    fn run(&self, tasks: Vec<Task<'_>>) {
        for t in tasks {
            t();
        }
    }
}

/// 一个用例文件。
struct Case {
    path: PathBuf,
    text: String,
    meta: Meta,
}

impl Case {
    fn load(path: PathBuf) -> Result<Self, String> {
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let meta = match (text.find("/*---"), text.find("---*/")) {
            (Some(a), Some(b)) if a < b => serde_yaml::from_str(&text[a + 5..b]).map_err(|e| format!("metadata: {e}"))?,
            _ => Meta::default(),
        };
        Ok(Self { path, text, meta })
    }

    fn has(&self, flag: Flag) -> bool {
        self.meta.flags.contains(&flag)
    }

    /// 拼上 harness 的完整源码。
    fn source(&self) -> Result<String, String> {
        if self.has(Flag::Raw) {
            return Ok(self.text.clone());
        }
        let mut src = String::new();
        if self.has(Flag::OnlyStrict) {
            src.push_str("\"use strict\";\n");
        }
        src.push_str(HOST_262);
        let mut harness = vec!["assert.js", "sta.js"];
        if self.has(Flag::Async) {
            src.push_str("function print(m) { console.log(m); }\n");
            harness.push("doneprintHandle.js");
        }
        for h in harness.into_iter().chain(self.meta.includes.iter().map(String::as_str)) {
            let p = Path::new(ROOT).join("harness").join(h);
            src.push_str(&std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?);
            src.push('\n');
        }
        // 模块用例：harness 先当脚本跑（全局），模块只是用例本身
        if !self.has(Flag::Module) {
            src.push_str(&self.text);
        }
        Ok(src)
    }

    /// 在本进程里跑，给出结果与原因。
    fn run(&self) -> (Outcome, String) {
        let src = match self.source() {
            Ok(s) => s,
            Err(e) => return (Outcome::Fail, e),
        };
        let platform = Capture { start: Instant::now(), lines: Mutex::default() };
        let mut isolate = Isolate::new(&platform, CreateParams::default());
        let mut executor = if std::env::var_os(JIT_ENV).is_some() { Executor::default().with_tier(std::rc::Rc::new(r8jit::Jit::from(0))) } else { Executor::default() };
        let file = self.path.strip_prefix(Suite::test_dir()).unwrap_or(&self.path).display().to_string();
        let expected = self.meta.negative.as_ref();
        let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));
        let r = if self.has(Flag::Module) {
            match isolate.run(&mut executor, &cx, &Source { text: &src, file: "[harness]" }) {
                Ok(_) => isolate.run_module(&mut executor, &cx, &Source { text: &self.text, file: &file }),
                e => e,
            }
        } else {
            isolate.run(&mut executor, &cx, &Source { text: &src, file: &file })
        };
        let (outcome, why) = isolate.enter(|scope| {
            let realm = scope.context_of(&cx);
            let mut agent = scope.agent(realm, &executor);
            match (r, expected) {
                (Err(Error::Unsupported(m)), _) => (Outcome::Unsupported, m),
                (Err(Error::Syntax(_)), Some(n)) if n.phase != Phase::Runtime && n.kind == ErrorType::SyntaxError => (Outcome::Pass, String::new()),
                (Err(Error::Syntax(m)), _) => (Outcome::Fail, format!("syntax error: {m}")),
                (Err(Error::Exception(g)), n) => {
                    let v = agent.local(&g);
                    let name = agent.get_named(v, "constructor").and_then(|c| agent.get_named(c, "name")).and_then(|n| agent.to_string(n));
                    let name = name.map_or_else(|_| String::new(), |s| String::from(&**s));
                    match n {
                        Some(n) if n.phase == Phase::Runtime && name == n.kind.name() => (Outcome::Pass, String::new()),
                        _ => {
                            let text = agent.to_string(v).map_or_else(|_| String::from("?"), |s| String::from(&**s));
                            (Outcome::Fail, format!("threw {text}"))
                        }
                    }
                }
                (Ok(_), Some(n)) => (Outcome::Fail, format!("expected {} ({} phase)", n.kind.name(), n.phase.name())),
                (Ok(_), None) => (Outcome::Pass, String::new()),
            }
        });
        if outcome == Outcome::Pass && self.has(Flag::Async) {
            let lines = platform.lines.into_inner().expect("an unpoisoned log");
            if !lines.iter().any(|l| l == ASYNC_DONE) {
                return (Outcome::Fail, format!("async: {}", lines.join(" | ")));
            }
        }
        (outcome, why)
    }
}

/// 宿主对象 `$262`：evalScript 走间接 eval，detachArrayBuffer 借 `transfer`（先存好，用例改了原型也不受影响），
/// createRealm 还没有。
const HOST_262: &str = "var $262 = (function (transfer) { return { global: globalThis, gc: function () {}, evalScript: function (s) { return (0, eval)(s); }, \
createRealm: function () { throw new TypeError('createRealm is not supported'); }, \
detachArrayBuffer: function (b) { transfer.call(b); return null; }, agent: {} }; })(ArrayBuffer.prototype.transfer);\n";

/// 父进程：一个用例的子进程结果。
struct Ran {
    rel: String,
    outcome: Outcome,
    why: String,
}

/// 父进程的一次运行。
struct Suite {
    /// 相对 `test/` 的路径
    cases: Vec<String>,
    exe: PathBuf,
}

impl Suite {
    fn test_dir() -> PathBuf {
        Path::new(ROOT).join("test")
    }

    fn baseline() -> BTreeSet<String> {
        std::fs::read_to_string(BASELINE).unwrap_or_default().lines().filter(|l| !l.is_empty()).map(String::from).collect()
    }

    /// `dir` 下全部用例（`_FIXTURE` 是被别的用例引用的，不单跑）
    fn walk(dir: &Path, out: &mut Vec<String>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                Self::walk(&p, out);
            } else if p.extension().is_some_and(|x| x == "js") && !p.to_string_lossy().contains("_FIXTURE") {
                let rel = p.strip_prefix(Self::test_dir()).unwrap_or(&p);
                out.push(rel.to_string_lossy().into_owned());
            }
        }
    }

    fn new(filters: &[String]) -> Result<Self, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut cases = Vec::new();
        if filters.is_empty() {
            cases.extend(Self::baseline());
        }
        for f in filters {
            let p = Self::test_dir().join(f);
            if p.is_file() {
                cases.push(f.clone());
            } else if p.is_dir() {
                Self::walk(&p, &mut cases);
            } else {
                return Err(format!("{}: no such test file or directory", p.display()));
            }
        }
        cases.sort();
        cases.dedup();
        Ok(Self { cases, exe })
    }

    fn run_one(&self, rel: &str) -> Ran {
        let spawned = Command::new(&self.exe)
            .env(CASE_ENV, Self::test_dir().join(rel))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn();
        let mut child = match spawned {
            Ok(c) => c,
            Err(e) => return Ran { rel: rel.into(), outcome: Outcome::Crash, why: e.to_string() },
        };
        let start = Instant::now();
        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break Some(s),
                Ok(None) if start.elapsed() < TIMEOUT => std::thread::sleep(POLL),
                _ => {
                    // 已经超时，杀不掉也只能放着
                    let _ = child.kill();
                    let _ = child.wait();
                    break None;
                }
            }
        };
        let mut why = String::new();
        if let Some(mut e) = child.stderr.take() {
            let _ = e.read_to_string(&mut why);
        }
        let outcome = match status.map(|s| s.code()) {
            None => Outcome::Timeout,
            Some(code) => code.and_then(|c| Outcome::try_from(c).ok()).unwrap_or(Outcome::Crash),
        };
        Ran { rel: rel.into(), outcome, why: why.trim().into() }
    }

    /// 并行跑完全部用例（工作线程数取 CPU 数的一半，给别的活留余量）。
    fn run(&self) -> Vec<Ran> {
        let jobs = std::thread::available_parallelism().map_or(1, |n| (n.get() / 2).max(1));
        let next = AtomicUsize::new(0);
        let done = Mutex::new(Vec::with_capacity(self.cases.len()));
        std::thread::scope(|s| {
            for _ in 0..jobs {
                s.spawn(|| {
                    while let Some(rel) = self.cases.get(next.fetch_add(1, Ordering::Relaxed)) {
                        let r = self.run_one(rel);
                        done.lock().expect("an unpoisoned result list").push(r);
                    }
                });
            }
        });
        let mut done = done.into_inner().expect("an unpoisoned result list");
        done.sort_by(|a, b| a.rel.cmp(&b.rel));
        done
    }
}

/// 汇总与基线对比。
struct Report<'r> {
    ran: &'r [Ran],
}

impl Report<'_> {
    fn counts<'a>(rs: impl Iterator<Item = &'a Ran>) -> [usize; Outcome::ALL.len()] {
        let mut c = [0; Outcome::ALL.len()];
        rs.for_each(|r| c[r.outcome as usize] += 1);
        c
    }

    fn row(label: &str, c: &[usize; Outcome::ALL.len()]) {
        let total: usize = c.iter().sum();
        let cols: Vec<String> = c.iter().map(|n| format!("{n:>12}")).collect();
        #[expect(clippy::cast_precision_loss, reason = "百分比")]
        let pct = if total == 0 { 0.0 } else { c[Outcome::Pass as usize] as f64 * 100.0 / total as f64 };
        println!("{label:<48}{}{total:>8}{pct:>7.1}%", cols.concat());
    }

    fn print(&self) {
        let heads: Vec<String> = Outcome::ALL.iter().map(|o| format!("{:>12}", o.name())).collect();
        println!("{:<48}{}{:>8}{:>8}", "test262", heads.concat(), "total", "pass%");
        let mut groups: Vec<&str> = self.ran.iter().map(|r| Self::group(&r.rel)).collect();
        groups.dedup();
        for g in groups {
            Self::row(g, &Self::counts(self.ran.iter().filter(|r| Self::group(&r.rel) == g)));
        }
        Self::row("total", &Self::counts(self.ran.iter()));
        if std::env::var_os(VERBOSE_ENV).is_some() {
            for r in self.ran.iter().filter(|r| r.outcome != Outcome::Pass) {
                println!("{:<11} {}  {}", r.outcome.name(), r.rel, r.why.lines().next().unwrap_or(""));
            }
        }
    }

    /// 路径前 `GROUP_DEPTH` 段
    fn group(rel: &str) -> &str {
        rel.match_indices('/').nth(GROUP_DEPTH - 1).map_or(rel, |(i, _)| &rel[..i])
    }

    /// 和基线比：返回掉下去的用例。`update` 时把这次的结果写回基线。
    fn against_baseline(&self, update: bool) -> Result<Vec<&str>, String> {
        let base = Suite::baseline();
        let passing: BTreeSet<&str> = self.ran.iter().filter(|r| r.outcome == Outcome::Pass).map(|r| r.rel.as_str()).collect();
        let lost: Vec<&str> = self.ran.iter().map(|r| r.rel.as_str()).filter(|r| base.contains(*r) && !passing.contains(r)).collect();
        let gained = passing.iter().filter(|r| !base.contains(**r)).count();
        if update {
            let ran: BTreeSet<&str> = self.ran.iter().map(|r| r.rel.as_str()).collect();
            let merged: BTreeSet<&str> = base.iter().map(String::as_str).filter(|r| !ran.contains(r)).chain(passing.iter().copied()).collect();
            let text: String = merged.iter().flat_map(|r| [*r, "\n"]).collect();
            std::fs::write(BASELINE, text).map_err(|e| format!("{BASELINE}: {e}"))?;
            println!("baseline: {} passing (+{gained}, -{})", merged.len(), lost.len());
        } else if gained > 0 {
            println!("{gained} newly passing; rerun with {UPDATE_ENV}=1 to record them");
        }
        Ok(lost)
    }
}

fn main() -> ExitCode {
    if let Some(path) = std::env::var_os(CASE_ENV) {
        let (outcome, why) = match Case::load(PathBuf::from(path)) {
            Ok(case) => case.run(),
            Err(e) => (Outcome::Fail, e),
        };
        if !why.is_empty() {
            eprintln!("{why}");
        }
        return outcome.into();
    }
    // cargo test 会顺带传 `--quiet` 之类的开关，只认路径
    let filters: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let suite = match Suite::new(&filters) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("test262: {e}");
            return ExitCode::FAILURE;
        }
    };
    if suite.cases.is_empty() {
        println!("test262: nothing to run (empty baseline; pass directories, e.g. `-- language/expressions`)");
        return ExitCode::SUCCESS;
    }
    let ran = suite.run();
    let report = Report { ran: &ran };
    report.print();
    match report.against_baseline(std::env::var_os(UPDATE_ENV).is_some()) {
        Ok(lost) if lost.is_empty() => ExitCode::SUCCESS,
        Ok(lost) => {
            println!("{} regressions against {BASELINE}:", lost.len());
            for r in &lost {
                println!("  {r}");
            }
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("test262: {e}");
            ExitCode::FAILURE
        }
    }
}
