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

#[derive(Clone, Copy, PartialEq, Eq)]
enum Engine {
    J8,
    QuickJs,
}

impl Engine {
    const ALL: [Engine; 2] = [Engine::J8, Engine::QuickJs];

    fn name(self) -> &'static str {
        match self {
            Engine::J8 => "j8",
            Engine::QuickJs => "quickjs",
        }
    }
}

impl TryFrom<&str> for Engine {
    type Error = String;

    fn try_from(s: &str) -> Result<Self, String> {
        Engine::ALL
            .into_iter()
            .find(|e| e.name() == s)
            .ok_or_else(|| format!("unknown engine {s} (j8 / quickjs)"))
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

struct QuickJsRunner;

impl QuickJsRunner {
    fn run(src: &str, runs: usize) -> Outcome {
        use rquickjs::{Context, Ctx, Error, Runtime, Value, convert::Coerced};
        let text = |ctx: &Ctx<'_>, v: Value<'_>| {
            v.get::<Coerced<String>>()
                .map(|s| s.0)
                .map_err(|_| Self::uncaught(ctx.catch().get::<Coerced<String>>().ok().map(|s| s.0)))
        };
        let rt = Runtime::new().map_err(|e| e.to_string())?;
        let mut best = f64::INFINITY;
        let mut result = String::new();
        for _ in 0..runs {
            let cx = Context::full(&rt).map_err(|e| e.to_string())?;
            result = cx.with(|ctx| {
                let code = src.as_bytes().to_vec();
                let start = Instant::now();
                let value = ctx.eval::<Value, _>(code);
                best = best.min(start.elapsed().as_secs_f64() * 1e3);
                match value {
                    Ok(v) => text(&ctx, v),
                    Err(Error::Exception) => {
                        let e = ctx.catch();
                        Err(Self::uncaught(e.get::<Coerced<String>>().ok().map(|s| s.0)))
                    }
                    Err(e) => Err(e.to_string()),
                }
            })?;
        }
        Ok((best, result))
    }

    fn uncaught(e: Option<String>) -> String {
        format!("Uncaught {}", e.as_deref().unwrap_or("exception"))
    }
}

impl Engine {
    fn run_all(self, runs: usize) {
        for (name, src) in BENCHES {
            let r = match self {
                Engine::J8 => J8Runner::run(src, runs),
                Engine::QuickJs => QuickJsRunner::run(src, runs),
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
            eprintln!("benchquickjs: running {} …", engine.name());
            let out = Command::new(&exe)
                .args(["--engine", engine.name(), "--runs", &runs.to_string()])
                .output().map_err(|e| e.to_string())?;
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
        println!("{:<12} {:>10} {:>12} {:>12}  check", "bench", "j8 ms", "quickjs ms", "j8/quickjs");
        for (i, (name, _)) in BENCHES.iter().enumerate() {
            let (j8, qjs) = (&table[0][i], &table[1][i]);
            let check = if j8.sum == qjs.sum {
                String::from("ok")
            } else {
                format!("j8={} quickjs={}", j8.sum, qjs.sum)
            };
            println!("{:<12} {:>10} {:>12} {:>12}  {}", name, ms(j8), ms(qjs), ratio(j8, qjs), check);
        }
        let logs: Vec<f64> = (0..BENCHES.len())
            .filter_map(|i| Some((table[0][i].ms? / table[1][i].ms?).ln()))
            .collect();
        let geo = (logs.iter().sum::<f64>() / logs.len().max(1) as f64).exp();
        println!("geomean      j8/quickjs {geo:.1}×");
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
            eprintln!("benchquickjs: {e}");
            ExitCode::FAILURE
        }
    }
}
