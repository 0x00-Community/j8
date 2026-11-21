use std::process::{Command, ExitCode};
use std::time::Instant;

const BENCHES: [(&str, &str); 11] = [
    ("fib", include_str!("../../benchv8/bench/fib.js")),
    ("loop", include_str!("../../benchv8/bench/loop.js")),
    ("objects", include_str!("../../benchv8/bench/objects.js")),
    ("array", include_str!("../../benchv8/bench/array.js")),
    ("sort", include_str!("../../benchv8/bench/sort.js")),
    ("string", include_str!("../../benchv8/bench/string.js")),
    ("closures", include_str!("../../benchv8/bench/closures.js")),
    ("classes", include_str!("../../benchv8/bench/classes.js")),
    ("collections", include_str!("../../benchv8/bench/collections.js")),
    ("json", include_str!("../../benchv8/bench/json.js")),
    ("regexp", include_str!("../../benchv8/bench/regexp.js")),
];

const DEFAULT_RUNS: usize = 5;

const JSC_NO_JIT: (&str, &str) = ("JSC_useJIT", "false");

#[derive(Clone, Copy, PartialEq, Eq)]
enum Engine {
    J8,
    Jsc,
    JscNoJit,
}

impl Engine {
    const ALL: [Engine; 3] = [Engine::J8, Engine::JscNoJit, Engine::Jsc];

    fn name(self) -> &'static str {
        match self {
            Engine::J8 => "j8",
            Engine::Jsc => "jsc",
            Engine::JscNoJit => "jsc-nojit",
        }
    }
}

impl TryFrom<&str> for Engine {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, String> {
        Engine::ALL
            .into_iter()
            .find(|e| e.name() == s)
            .ok_or_else(|| format!("unknown engine {s} (j8 / jsc / jsc-nojit)"))
    }
}

type Outcome = Result<(f64, String), String>;

struct J8Runner;

impl J8Runner {
    fn run(src: &str, runs: usize) -> Outcome {
        use j8::builtins::Builtins;
        use j8::executor::Executor;
        use j8::frame::{CreateParams, Error, Isolate, Source};
        let platform = j8::platform::DefaultPlatform::default();
        let mut isolate = Isolate::new(&platform, CreateParams::default());
        let mut executor = if std::env::var_os("J8_NOJIT").is_some() { Executor::default() } else { Executor::default().with_tier(j8::jit::Jit::default()) };
        let mut best = f64::INFINITY;
        let mut result = String::new();
        for _ in 0..runs {
            let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));
            let start = Instant::now();
            let r = isolate.run(
                &mut executor,
                &cx,
                &Source {
                    text: src,
                    file: "bench.js",
                },
            );
            best = best.min(start.elapsed().as_secs_f64() * 1e3);
            let text = isolate.enter(|scope| {
                let realm = scope.context_of(&cx);
                let mut a = scope.agent(realm, &executor);
                match r {
                    Ok(g) => {
                        let v = a.local(&g);
                        a.to_string(v)
                            .map(|s| String::from(&**s))
                            .map_err(|e| e.to_string())
                    }
                    Err(Error::Exception(g)) => {
                        let v = a.local(&g);
                        Err(a.to_string(v).map_or_else(
                            |_| String::from("exception"),
                            |s| format!("Uncaught {}", &**s),
                        ))
                    }
                    Err(e) => Err(e.to_string()),
                }
            })?;
            result = text;
        }
        Ok((best, result))
    }
}

mod jsc {
    use std::ffi::{c_char, c_void};

    pub type ContextGroup = *const c_void;
    pub type GlobalContext = *mut c_void;
    pub type JsString = *mut c_void;
    pub type Value = *const c_void;

    #[link(name = "libjavascriptcoregtk-6.0.so.1", kind = "dylib", modifiers = "+verbatim")]
    unsafe extern "C" {
        pub fn JSContextGroupCreate() -> ContextGroup;
        pub fn JSContextGroupRelease(group: ContextGroup);
        pub fn JSGlobalContextCreateInGroup(group: ContextGroup, class: *const c_void) -> GlobalContext;
        pub fn JSGlobalContextRelease(ctx: GlobalContext);
        pub fn JSEvaluateScript(
            ctx: GlobalContext,
            script: JsString,
            this: *const c_void,
            url: JsString,
            line: i32,
            exception: *mut Value,
        ) -> Value;
        pub fn JSValueToStringCopy(ctx: GlobalContext, value: Value, exception: *mut Value) -> JsString;
        pub fn JSStringCreateWithUTF8CString(s: *const c_char) -> JsString;
        pub fn JSStringGetMaximumUTF8CStringSize(s: JsString) -> usize;
        pub fn JSStringGetUTF8CString(s: JsString, buf: *mut c_char, size: usize) -> usize;
        pub fn JSStringRelease(s: JsString);
    }
}

struct JscString(jsc::JsString);

impl From<&str> for JscString {
    fn from(s: &str) -> Self {
        let c = std::ffi::CString::new(s).unwrap_or_default();
        Self(unsafe { jsc::JSStringCreateWithUTF8CString(c.as_ptr()) })
    }
}

impl From<&JscString> for String {
    fn from(s: &JscString) -> Self {
        unsafe {
            let mut buf = vec![0u8; jsc::JSStringGetMaximumUTF8CStringSize(s.0)];
            let n = jsc::JSStringGetUTF8CString(s.0, buf.as_mut_ptr().cast(), buf.len());
            buf.truncate(n.saturating_sub(1));
            String::from_utf8_lossy(&buf).into_owned()
        }
    }
}

impl Drop for JscString {
    fn drop(&mut self) {
        unsafe { jsc::JSStringRelease(self.0) }
    }
}

struct JscContext(jsc::GlobalContext);

impl JscContext {
    fn new(group: jsc::ContextGroup) -> Self {
        Self(unsafe { jsc::JSGlobalContextCreateInGroup(group, std::ptr::null()) })
    }

    fn string(&self, value: jsc::Value) -> Option<String> {
        let mut exception = std::ptr::null();
        let s = unsafe { jsc::JSValueToStringCopy(self.0, value, &raw mut exception) };
        (!s.is_null()).then(|| String::from(&JscString(s)))
    }

    fn eval(&self, script: &JscString) -> Result<jsc::Value, jsc::Value> {
        let mut exception = std::ptr::null();
        let v = unsafe {
            jsc::JSEvaluateScript(self.0, script.0, std::ptr::null(), std::ptr::null_mut(), 1, &raw mut exception)
        };
        if v.is_null() { Err(exception) } else { Ok(v) }
    }
}

impl Drop for JscContext {
    fn drop(&mut self) {
        unsafe { jsc::JSGlobalContextRelease(self.0) }
    }
}

struct JscRunner;

impl JscRunner {
    fn run(src: &str, runs: usize) -> Outcome {
        let group = unsafe { jsc::JSContextGroupCreate() };
        let script = JscString::from(src);
        let mut best = f64::INFINITY;
        let mut result = Ok(String::new());
        for _ in 0..runs {
            let cx = JscContext::new(group);
            let start = Instant::now();
            let value = cx.eval(&script);
            best = best.min(start.elapsed().as_secs_f64() * 1e3);
            result = match value {
                Ok(v) => cx.string(v).ok_or_else(|| String::from("exception")),
                Err(e) => Err(format!("Uncaught {}", cx.string(e).unwrap_or_else(|| String::from("exception")))),
            };
            if result.is_err() {
                break;
            }
        }
        drop(script);
        unsafe { jsc::JSContextGroupRelease(group) };
        result.map(|r| (best, r))
    }
}

impl Engine {
    fn run_all(self, runs: usize) {
        for (name, src) in BENCHES {
            let r = match self {
                Engine::J8 => J8Runner::run(src, runs),
                Engine::Jsc | Engine::JscNoJit => JscRunner::run(src, runs),
            };
            match r {
                Ok((ms, sum)) => println!("{name}\t{ms:.2}\t{sum}"),
                Err(e) => println!("{name}\tERR\t{}", e.replace(['\t', '\n'], " ")),
            }
        }
    }

    fn command(self, exe: &std::path::Path, runs: usize) -> Command {
        let mut cmd = Command::new(exe);
        cmd.args(["--engine", self.name(), "--runs", &runs.to_string()]);
        if self == Engine::JscNoJit {
            cmd.env(JSC_NO_JIT.0, JSC_NO_JIT.1);
        }
        cmd
    }
}

struct Row {
    ms: Option<f64>,
    sum: String,
}

struct Comparison;

impl Comparison {
    fn run(runs: usize) -> Result<(), String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut table: Vec<Vec<Row>> = Vec::new();
        for engine in Engine::ALL {
            eprintln!("benchjsc: running {} …", engine.name());
            let out = engine.command(&exe, runs).output().map_err(|e| e.to_string())?;
            let text = String::from_utf8_lossy(&out.stdout);
            let rows = BENCHES
                .iter()
                .map(|(name, _)| {
                    let line = text.lines().find(|l| l.split('\t').next() == Some(name));
                    let mut parts = line.unwrap_or("").splitn(3, '\t').skip(1);
                    let ms = parts.next().and_then(|m| m.parse().ok());
                    Row {
                        ms,
                        sum: parts.next().unwrap_or("crashed").to_owned(),
                    }
                })
                .collect();
            table.push(rows);
        }
        let ms = |r: &Row| r.ms.map_or_else(|| String::from("—"), |m| format!("{m:.1}"));
        let ratio = |a: &Row, b: &Row| match (a.ms, b.ms) {
            (Some(x), Some(y)) if y > 0.0 => format!("{:.1}×", x / y),
            _ => String::from("—"),
        };
        println!(
            "{:<12} {:>10} {:>14} {:>10} {:>14} {:>10}  check",
            "bench", "j8 ms", "jsc-nojit ms", "jsc ms", "j8/nojit", "j8/jsc"
        );
        for (i, (name, _)) in BENCHES.iter().enumerate() {
            let (j8, nj, jsc) = (&table[0][i], &table[1][i], &table[2][i]);
            let check = if j8.sum == jsc.sum && nj.sum == jsc.sum {
                String::from("ok")
            } else {
                format!("j8={} jsc-nojit={} jsc={}", j8.sum, nj.sum, jsc.sum)
            };
            println!(
                "{:<12} {:>10} {:>14} {:>10} {:>14} {:>10}  {}",
                name,
                ms(j8),
                ms(nj),
                ms(jsc),
                ratio(j8, nj),
                ratio(j8, jsc),
                check
            );
        }
        let geo = |a: usize, b: usize| {
            let logs: Vec<f64> = (0..BENCHES.len())
                .filter_map(|i| Some((table[a][i].ms? / table[b][i].ms?).ln()))
                .collect();
            (logs.iter().sum::<f64>() / logs.len().max(1) as f64).exp()
        };
        println!("geomean      j8/nojit {:.1}×   j8/jsc {:.1}×", geo(0, 1), geo(0, 2));
        Ok(())
    }
}

fn main() -> ExitCode {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let flag = |name: &str| {
        args.iter()
            .position(|a| a == name)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let runs = flag("--runs")
        .and_then(|r| r.parse().ok())
        .unwrap_or(DEFAULT_RUNS);
    let r = match flag("--engine") {
        Some(e) if e == Engine::JscNoJit.name() && std::env::var_os(JSC_NO_JIT.0).is_none() => {
            std::env::current_exe()
                .map_err(|e| e.to_string())
                .and_then(|exe| Engine::JscNoJit.command(&exe, runs).status().map_err(|e| e.to_string()))
                .map(|_| ())
        }
        Some(e) => Engine::try_from(e.as_str()).map(|engine| engine.run_all(runs)),
        None => Comparison::run(runs),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("benchjsc: {e}");
            ExitCode::FAILURE
        }
    }
}
