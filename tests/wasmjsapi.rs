use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::io::Read;
use std::process::{Command, ExitCode, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use j8builtins::Builtins;
use j8executor::{Executor, WebAssembly};
use j8framework::{CreateParams, Error, Isolate, Platform, Source, Task};

const ROOT: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/wasm-jsapi");

const HARNESS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/wasmjsapi.harness.js");

const BASELINE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/wasmjsapi.passing");

const UPDATE_ENV: &str = "J8_WASMJSAPI_UPDATE";
const VERBOSE_ENV: &str = "J8_WASMJSAPI_VERBOSE";
const JIT_ENV: &str = "J8_WASMJSAPI_JIT";
const CASE_ENV: &str = "J8_WASMJSAPI_CASE";

const TIMEOUT: Duration = Duration::from_secs(60);
const POLL: Duration = Duration::from_millis(5);

const META: &str = "// META: script=";
const SITE: &str = "/wasm/jsapi/";
const PASS: &str = "PASS\t";
const FAIL: &str = "FAIL\t";
const DONE: &str = "DONE";

struct Capture {
    rel: String,
    start: Instant,
    lines: Mutex<Vec<String>>,
}

impl j8framework::Pool for Capture {
    fn threads(&self) -> usize {
        1
    }

    fn run(&self, tasks: Vec<Task<'_>>) {
        for t in tasks {
            t();
        }
    }
}

impl Platform for Capture {
    fn wall_clock(&self) -> Duration {
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap_or_default()
    }

    fn now(&self) -> Duration {
        self.start.elapsed()
    }

    fn print(&self, line: &str) {
        if let Some(c) = Case::console(&self.rel, line) {
            println!("{}", c.line());
        }
        self.lines.lock().expect("an unpoisoned log").push(line.into());
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Outcome {
    Pass,
    Fail,
}

struct Case {
    id: String,
    outcome: Outcome,
    why: String,
}

impl Case {
    fn line(&self) -> String {
        format!("{}{}\t{}", if self.outcome == Outcome::Pass { PASS } else { FAIL }, self.id, self.why)
    }

    fn console(rel: &str, l: &str) -> Option<Self> {
        let (outcome, rest) = if let Some(r) = l.strip_prefix(PASS) { (Outcome::Pass, r) } else { (Outcome::Fail, l.strip_prefix(FAIL)?) };
        let (name, why) = rest.split_once('\t').unwrap_or((rest, ""));
        Some(Self { id: format!("{rel}\t{}", name.replace('\t', " ")), outcome, why: why.into() })
    }

    fn parse(l: &str) -> Option<Self> {
        let (outcome, rest) = if let Some(r) = l.strip_prefix(PASS) { (Outcome::Pass, r) } else { (Outcome::Fail, l.strip_prefix(FAIL)?) };
        let mut parts = rest.splitn(3, '\t');
        let (file, name, why) = (parts.next()?, parts.next()?, parts.next().unwrap_or(""));
        Some(Self { id: format!("{file}\t{name}"), outcome, why: why.into() })
    }
}

struct File {
    path: PathBuf,
    rel: String,
}

impl File {
    fn all(filters: &[String]) -> Vec<Self> {
        let mut out = Vec::new();
        Self::walk(Path::new(ROOT), &mut out);
        out.retain(|f| filters.is_empty() || filters.iter().any(|x| f.rel.starts_with(x.as_str())));
        out.sort_by(|a, b| a.rel.cmp(&b.rel));
        out
    }

    fn walk(dir: &Path, out: &mut Vec<Self>) {
        let Ok(rd) = std::fs::read_dir(dir) else { return };
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                Self::walk(&p, out);
            } else if p.to_string_lossy().ends_with(".any.js") {
                let rel = p.strip_prefix(ROOT).unwrap_or(&p).to_string_lossy().into_owned();
                out.push(Self { path: p, rel });
            }
        }
    }

    fn source(&self) -> Result<String, String> {
        let text = std::fs::read_to_string(&self.path).map_err(|e| e.to_string())?;
        let mut src = std::fs::read_to_string(HARNESS).map_err(|e| e.to_string())?;
        let dir = self.path.parent().unwrap_or(Path::new(ROOT));
        for line in text.lines().filter_map(|l| l.strip_prefix(META)) {
            let p = line.strip_prefix(SITE).map_or_else(|| dir.join(line.trim()), |rest| Path::new(ROOT).join(rest.trim()));
            src.push_str(&std::fs::read_to_string(&p).map_err(|e| format!("{}: {e}", p.display()))?);
            src.push('\n');
        }
        src.push_str(&text);
        src.push_str("\n__run();\n");
        Ok(src)
    }

    fn spawn(&self, exe: &Path) -> Vec<Case> {
        let fail = |why: &str| vec![Case { id: format!("{}\t[load]", self.rel), outcome: Outcome::Fail, why: why.into() }];
        let Ok(mut child) = Command::new(exe).env(CASE_ENV, &self.rel).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::null()).spawn() else { return fail("could not start") };
        let mut out = child.stdout.take();
        let reader = std::thread::spawn(move || {
            let mut text = String::new();
            if let Some(o) = out.as_mut() {
                let _ = o.read_to_string(&mut text);
            }
            text
        });
        let start = Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if start.elapsed() > TIMEOUT => {
                    let _ = child.kill();
                    let _ = child.wait();
                    let mut cases: Vec<Case> = reader.join().unwrap_or_default().lines().filter_map(Case::parse).collect();
                    cases.extend(fail("timed out"));
                    return cases;
                }
                Ok(None) => std::thread::sleep(POLL),
                Err(_) => return fail("lost the child"),
            }
        }
        let text = reader.join().unwrap_or_default();
        let cases: Vec<Case> = text.lines().filter_map(Case::parse).collect();
        if cases.is_empty() { fail("crashed") } else { cases }
    }

    fn run(&self) -> Vec<Case> {
        let load = |why: String| vec![Case { id: format!("{}\t[load]", self.rel), outcome: Outcome::Fail, why }];
        let src = match self.source() {
            Ok(s) => s,
            Err(e) => return load(e),
        };
        let platform = Capture { rel: self.rel.clone(), start: Instant::now(), lines: Mutex::default() };
        let mut isolate = Isolate::new(&platform, CreateParams::default());
        let executor = Executor::default();
        let mut wasm = WebAssembly::from(Builtins);
        if std::env::var_os(JIT_ENV).is_some() {
            let jit = j8jit::WasmJit::from(0);
            assert!(jit.ready(), "the wasm JIT's stencils did not compile");
            wasm = wasm.with_tier(std::sync::Arc::new(jit));
        }
        let cx = isolate.create_context(&wasm);
        let r = executor.eval(&mut isolate, &cx, &Source { text: &src, file: &self.rel });
        let error = isolate.enter(&cx, &executor, |agent| match r {
            Ok(_) => None,
            Err(Error::Exception) => {
                let v = agent.take_exception();
                Some(agent.to_string(v).map_or_else(|_| String::from("?"), |s| String::from(&**s)))
            }
            Err(e) => Some(format!("{e:?}")),
        });
        let lines = std::mem::take(&mut *platform.lines.lock().expect("an unpoisoned log"));
        let mut out = Vec::new();
        if let Some(e) = error {
            out.extend(load(e));
        } else if !lines.iter().any(|l| l == DONE) {
            out.extend(load(String::from("promise tests did not finish")));
        }
        out
    }
}

fn main() -> ExitCode {
    std::panic::set_hook(Box::new(|_| {}));
    if let Some(rel) = std::env::var_os(CASE_ENV) {
        let rel = rel.to_string_lossy().into_owned();
        let f = File { path: Path::new(ROOT).join(&rel), rel };
        let cases = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f.run())).unwrap_or_else(|p| {
            let why = p.downcast_ref::<&str>().map(|s| (*s).to_owned()).or_else(|| p.downcast_ref::<String>().cloned()).unwrap_or_default();
            vec![Case { id: format!("{}\t[load]", f.rel), outcome: Outcome::Fail, why: format!("panicked: {why}") }]
        });
        for c in cases {
            println!("{}", c.line());
        }
        return ExitCode::SUCCESS;
    }
    let Ok(exe) = std::env::current_exe() else { return ExitCode::FAILURE };
    let filters: Vec<String> = std::env::args().skip(1).filter(|a| !a.starts_with('-')).collect();
    let files = File::all(&filters);
    if files.is_empty() {
        eprintln!("wasmjsapi: no .any.js files under {ROOT}");
        return ExitCode::FAILURE;
    }
    let verbose = std::env::var_os(VERBOSE_ENV).is_some();
    let workers = std::thread::available_parallelism().map_or(1, usize::from);
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results: Mutex<Vec<Case>> = Mutex::default();
    std::thread::scope(|s| {
        for _ in 0..workers {
            s.spawn(|| {
                loop {
                    let i = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    let Some(f) = files.get(i) else { break };
                    let c = f.spawn(&exe);
                    results.lock().expect("an unpoisoned result list").extend(c);
                }
            });
        }
    });
    let cases = results.into_inner().unwrap_or_default();
    let mut per: BTreeMap<String, [usize; 2]> = BTreeMap::new();
    for c in &cases {
        let file = c.id.split('\t').next().unwrap_or_default().to_owned();
        let row = per.entry(file).or_default();
        row[usize::from(c.outcome == Outcome::Fail)] += 1;
        if verbose && c.outcome == Outcome::Fail {
            println!("FAIL {}\t{}", c.id, c.why);
        }
    }
    println!("{:<60}{:>8}{:>8}", "wasmjsapi", "pass", "fail");
    for (f, [p, x]) in &per {
        println!("{f:<60}{p:>8}{x:>8}");
    }
    let passing: BTreeSet<&str> = cases.iter().filter(|c| c.outcome == Outcome::Pass).map(|c| c.id.as_str()).collect();
    println!("{} files, {} subtests, {} passing", files.len(), cases.len(), passing.len());
    let base_text = std::fs::read_to_string(BASELINE).unwrap_or_default();
    let base: BTreeSet<&str> = base_text.lines().filter(|l| !l.is_empty()).collect();
    let ran: BTreeSet<&str> = files.iter().map(|f| f.rel.as_str()).collect();
    let in_ran = |id: &str| ran.contains(id.split('\t').next().unwrap_or_default());
    let lost: Vec<&&str> = base.iter().filter(|b| in_ran(b) && !passing.contains(**b)).collect();
    if std::env::var_os(UPDATE_ENV).is_some() {
        let merged: BTreeSet<&str> = base.iter().copied().filter(|b| !in_ran(b)).chain(passing.iter().copied()).collect();
        let body: String = merged.iter().flat_map(|l| [*l, "\n"]).collect();
        if let Err(e) = std::fs::write(BASELINE, body) {
            eprintln!("wasmjsapi: {BASELINE}: {e}");
            return ExitCode::FAILURE;
        }
        println!("baseline: {} passing", merged.len());
    }
    for l in &lost {
        println!("REGRESSED {l}");
    }
    if lost.is_empty() { ExitCode::SUCCESS } else { ExitCode::FAILURE }
}
