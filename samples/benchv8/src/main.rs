use std::process::{Command, ExitCode};
use std::time::Instant;

const BENCHES: [(&str, &str); 11] = [
    ("fib", include_str!("../bench/fib.js")),
    ("loop", include_str!("../bench/loop.js")),
    ("objects", include_str!("../bench/objects.js")),
    ("array", include_str!("../bench/array.js")),
    ("sort", include_str!("../bench/sort.js")),
    ("string", include_str!("../bench/string.js")),
    ("closures", include_str!("../bench/closures.js")),
    ("classes", include_str!("../bench/classes.js")),
    ("collections", include_str!("../bench/collections.js")),
    ("json", include_str!("../bench/json.js")),
    ("regexp", include_str!("../bench/regexp.js")),
];

const DEFAULT_RUNS: usize = 5;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Engine {
    J8,
    V8,
    V8Jitless,
}

impl Engine {
    const ALL: [Engine; 3] = [Engine::J8, Engine::V8Jitless, Engine::V8];

    fn name(self) -> &'static str {
        match self {
            Engine::J8 => "j8",
            Engine::V8 => "v8",
            Engine::V8Jitless => "v8-jitless",
        }
    }
}

impl TryFrom<&str> for Engine {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, String> {
        Engine::ALL
            .into_iter()
            .find(|e| e.name() == s)
            .ok_or_else(|| format!("unknown engine {s} (j8 / v8 / v8-jitless)"))
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
        let executor = if std::env::var_os("J8_NOJIT").is_some() { Executor::default() } else { Executor::default().with_tier(j8::jit::Jit::default()) };
        let mut best = f64::INFINITY;
        let mut result = String::new();
        for _ in 0..runs {
            let cx = isolate.enter(|scope| scope.mc.stash::<j8::frame::ContextBrand>(scope.context(&Builtins)));
            let start = Instant::now();
            let r = executor.eval(
                &mut isolate,
                &cx,
                &Source {
                    text: src,
                    file: "bench.js",
                },
            );
            best = best.min(start.elapsed().as_secs_f64() * 1e3);
            let text = isolate.enter(|scope| {
                let context = scope.mc.fetch(&cx);
                let mut a = scope.agent(context, &executor);
                match r {
                    Ok(g) => {
                        let v = a.local(&g);
                        a.to_string(v)
                            .map(|s| String::from(&**s))
                            .map_err(|e| e.to_string())
                    }
                    Err(Error::Exception) => {
                        let v = a.take_exception();
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

struct V8Runner;

impl V8Runner {
    fn init(jitless: bool) {
        if jitless {
            v8::V8::set_flags_from_string("--jitless");
        }
        let platform = v8::new_default_platform(0, false).make_shared();
        v8::V8::initialize_platform(platform);
        v8::V8::initialize();
    }

    fn run(src: &str, runs: usize) -> Outcome {
        let isolate = &mut v8::Isolate::new(v8::CreateParams::default());
        let mut best = f64::INFINITY;
        let mut result = String::new();
        for _ in 0..runs {
            v8::scope!(let handle_scope, isolate);
            let context = v8::Context::new(handle_scope, Default::default());
            let scope = &mut v8::ContextScope::new(handle_scope, context);
            v8::tc_scope!(let tc, scope);
            let code = v8::String::new(tc, src).ok_or("source too large")?;
            let start = Instant::now();
            let value = v8::Script::compile(tc, code, None).and_then(|s| s.run(tc));
            best = best.min(start.elapsed().as_secs_f64() * 1e3);
            match value {
                Some(v) => result = v.to_rust_string_lossy(tc),
                None => {
                    let e = tc
                        .exception()
                        .map_or_else(|| String::from("exception"), |e| e.to_rust_string_lossy(tc));
                    return Err(format!("Uncaught {e}"));
                }
            }
        }
        Ok((best, result))
    }
}

impl Engine {
    fn run_all(self, runs: usize) {
        let engine = self;
        if engine != Engine::J8 {
            V8Runner::init(engine == Engine::V8Jitless);
        }
        for (name, src) in BENCHES {
            let r = match engine {
                Engine::J8 => J8Runner::run(src, runs),
                Engine::V8 | Engine::V8Jitless => V8Runner::run(src, runs),
            };
            match r {
                Ok((ms, sum)) => println!("{name}\t{ms:.2}\t{sum}"),
                Err(e) => println!("{name}\tERR\t{}", e.replace(['\t', '\n'], " ")),
            }
        }
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
            eprintln!("benchv8: running {} …", engine.name());
            let out = Command::new(&exe)
                .args(["--engine", engine.name(), "--runs", &runs.to_string()])
                .output()
                .map_err(|e| e.to_string())?;
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
        let ms = |r: &Row| {
            r.ms.map_or_else(|| String::from("—"), |m| format!("{m:.1}"))
        };
        let ratio = |a: &Row, b: &Row| match (a.ms, b.ms) {
            (Some(x), Some(y)) if y > 0.0 => format!("{:.1}×", x / y),
            _ => String::from("—"),
        };
        println!(
            "{:<12} {:>10} {:>14} {:>10} {:>14} {:>10}  check",
            "bench", "j8 ms", "v8-jitless ms", "v8 ms", "j8/jitless", "j8/v8"
        );
        for (i, (name, _)) in BENCHES.iter().enumerate() {
            let (j8, jl, v8) = (&table[0][i], &table[1][i], &table[2][i]);
            let check = if j8.sum == v8.sum && jl.sum == v8.sum {
                String::from("ok")
            } else {
                format!("j8={} v8={}", j8.sum, v8.sum)
            };
            println!(
                "{:<12} {:>10} {:>14} {:>10} {:>14} {:>10}  {}",
                name,
                ms(j8),
                ms(jl),
                ms(v8),
                ratio(j8, jl),
                ratio(j8, v8),
                check
            );
        }
        let geo = |a: usize, b: usize| {
            let logs: Vec<f64> = (0..BENCHES.len())
                .filter_map(|i| Some((table[a][i].ms? / table[b][i].ms?).ln()))
                .collect();
            (logs.iter().sum::<f64>() / logs.len().max(1) as f64).exp()
        };
        println!(
            "geomean      j8/jitless {:.1}×   j8/v8 {:.1}×",
            geo(0, 1),
            geo(0, 2)
        );
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
        Some(e) => Engine::try_from(e.as_str()).map(|engine| engine.run_all(runs)),
        None => Comparison::run(runs),
    };
    match r {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("benchv8: {e}");
            ExitCode::FAILURE
        }
    }
}
