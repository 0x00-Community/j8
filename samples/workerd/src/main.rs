//! workerd: a minimal host that loads Cloudflare Workers (ES modules) and Durable Objects with j8.
//!
//! ```text
//! workerd examples/counter.js --bind COUNTER=Counter --request "GET /counter/inc?name=a"
//! workerd examples/counter.js --bind COUNTER=Counter --port 8787
//! ```
//! The Fetch API subset, WebSockets and Durable Objects are written in JS in `prelude.js`. The machine parts are Rust,
//! bound in the `natives` of [`Workerd`]: each object's key-value storage ([`KvStore`]) and SQL storage
//! ([`sql::SqlStorage`]), and the HTTP / WebSocket server ([`ws::Server`]), which runs on the event loop.
mod sql;
mod ws;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use j8::executor::Executor;
use j8::frame::Error;
use j8::platform::DefaultPlatform;


/// The JS part of the Fetch API and Durable Objects.
const PRELUDE: &str = include_str!("prelude.js");
/// Default host name for requests (URLs must be absolute)
const HOST: &str = "http://localhost";

/// Load a Worker and call its `fetch` for each request
#[derive(Parser)]
#[command(name = "workerd", version)]
struct Cli {
    /// Worker script (ES module, `export default { fetch }`)
    script: PathBuf,
    /// Durable Object binding: `name in env=exported class name`
    #[arg(long = "bind", value_name = "NAME=CLASS")]
    bindings: Vec<String>,
    /// One-shot request: `"METHOD path"`, may be repeated; if given, no server is started
    #[arg(long = "request", value_name = "METHOD PATH")]
    requests: Vec<String>,
    /// Port for the HTTP / WebSocket server (`0` picks a free one, printed on stderr)
    #[arg(long, default_value_t = 8787)]
    port: u16,
    /// Directory for Durable Objects' SQL storage (one SQLite file per object); in memory when not given
    #[arg(long)]
    storage: Option<PathBuf>,
}

/// Storage of one Durable Object (key ⇒ JSON text).
#[derive(Default)]
struct KvStore {
    map: BTreeMap<String, String>,
}

#[j8::class]
impl KvStore {
    #[js(constructor)]
    fn new() -> Self {
        Self::default()
    }

    fn get(&self, key: &str) -> Option<String> {
        self.map.get(key).cloned()
    }

    fn put(&mut self, key: &str, value: &str) {
        self.map.insert(key.to_owned(), value.to_owned());
    }

    fn delete(&mut self, key: &str) -> bool {
        self.map.remove(key).is_some()
    }

    fn keys(&self) -> Vec<String> {
        self.map.keys().cloned().collect()
    }

    /// For `newUniqueId()`: timestamp plus counter, in hex.
    fn unique_id(a: &mut j8::frame::Agent<'_, '_>) -> String {
        format!("{:032x}", a.platform.wall_clock().as_nanos())
    }
}

/// A response from the Worker.
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl From<&str> for Reply {
    /// The text from `__workerd.take()`: status line, one header per line, blank line, body.
    fn from(text: &str) -> Self {
        let (head, body) = text.split_once("\n\n").unwrap_or((text, ""));
        let mut lines = head.lines();
        let status = lines.next().and_then(|s| s.trim().parse().ok()).unwrap_or(500);
        let headers = lines.filter_map(|l| l.split_once(": ")).map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
        Self { status, headers, body: body.to_owned() }
    }
}

j8::runtime! {
    /// The Worker runtime: built-ins, the storage binding the prelude uses, and the Fetch API / Durable Object prelude
    Workerd {
        natives: [KvStore, sql::SqlStorage, ws::Server],
        prelude: PRELUDE,
    }
}

/// A running Worker: one isolate, one execution environment.
struct Worker<'p> {
    rt: j8::Runtime<'p>,
}

impl<'p> Worker<'p> {
    fn new(set: &j8::EventLoopSet<'_, 'p, Executor>) -> Result<Self, String> {
        let rt = Workerd::runtime(set, Executor::default()).map_err(|e| format!("prelude failed: {e}"))?;
        Ok(Self { rt })
    }

    /// Run a script and return ToString of the result.
    fn eval(&mut self, text: &str, file: &str) -> Result<String, String> {
        match self.rt.eval(text, file).and_then(|g| self.rt.run_until_idle().map(|()| g)) {
            Ok(g) => Ok(self.rt.text(&g)),
            Err(Error::Exception(g)) => Err(format!("Uncaught {}", self.rt.text(&g))),
            Err(e) => Err(e.to_string()),
        }
    }

    /// String literal (JSON syntax).
    fn quote(s: &str) -> String {
        let mut out = String::from("\"");
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                c if (c as u32) < 0x20 => _ = write!(out, "\\u{:04x}", c as u32),
                c => out.push(c),
            }
        }
        out.push('"');
        out
    }

    /// Dynamically import the Worker module and install the Durable Object bindings (SQL files under `storage`).
    fn load(&mut self, script: &str, bindings: &[(String, String)], storage: &str) -> Result<(), String> {
        let pairs: Vec<String> = bindings.iter().map(|(n, c)| format!("[{}, {}]", Self::quote(n), Self::quote(c))).collect();
        let src = format!(
            "import({}).then(ns => __workerd.setup(ns, [{}], {})).catch(e => {{ __workerd.error = String(e && e.stack || e); }})",
            Self::quote(script),
            pairs.join(", "),
            Self::quote(storage)
        );
        self.eval(&src, "[load]")?;
        match self.eval("String(__workerd.error)", "[load]")?.as_str() {
            "null" => Ok(()),
            e => Err(e.to_owned()),
        }
    }

    /// Hand a request to the Worker's `fetch` and run until its Promise settles.
    fn dispatch(&mut self, method: &str, path: &str, headers: &[(String, String)], body: &str) -> Reply {
        let url = if path.starts_with("http://") || path.starts_with("https://") { path.to_owned() } else { format!("{HOST}{path}") };
        let hs: Vec<String> = headers.iter().map(|(k, v)| format!("[{}, {}]", Self::quote(k), Self::quote(v))).collect();
        let src = format!("__workerd.dispatch({}, {}, [{}], {})", Self::quote(method), Self::quote(&url), hs.join(", "), Self::quote(body));
        if let Err(e) = self.eval(&src, "[dispatch]") {
            return Reply { status: 500, headers: Vec::new(), body: e + "\n" };
        }
        match self.eval("__workerd.take()", "[dispatch]") {
            Ok(text) => Reply::from(text.as_str()),
            Err(e) => Reply { status: 500, headers: Vec::new(), body: e + "\n" },
        }
    }

    /// The HTTP / WebSocket server: everything runs on the event loop, which never empties while the server listens
    fn serve(&mut self, port: u16) -> Result<(), String> {
        self.eval(&format!("__workerd.serve({port})"), "[serve]").map(drop)
    }
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let mut bindings = Vec::new();
    for b in &cli.bindings {
        match b.split_once('=') {
            Some((n, c)) => bindings.push((n.to_owned(), c.to_owned())),
            None => {
                eprintln!("workerd: --bind wants NAME=CLASS, got {b}");
                return ExitCode::FAILURE;
            }
        }
    }
    let storage = match &cli.storage {
        Some(dir) => match std::fs::create_dir_all(dir).and_then(|()| std::fs::canonicalize(dir)) {
            Ok(p) => p.display().to_string(),
            Err(e) => {
                eprintln!("workerd: {}: {e}", dir.display());
                return ExitCode::FAILURE;
            }
        },
        None => String::new(),
    };
    let script = match std::fs::canonicalize(&cli.script) {
        Ok(p) => p.display().to_string(),
        Err(e) => {
            eprintln!("workerd: {}: {e}", cli.script.display());
            return ExitCode::FAILURE;
        }
    };
    let platform = Workerd::platform(DefaultPlatform::default());
    let run = |set: &j8::EventLoopSet<'_, '_, Executor>| -> Result<(), String> {
        let mut rt = Worker::new(set)?;
        rt.load(&script, &bindings, &storage)?;
        if cli.requests.is_empty() {
            return rt.serve(cli.port);
        }
        for r in &cli.requests {
            let (method, path) = r.split_once(' ').unwrap_or(("GET", r.as_str()));
            let reply = rt.dispatch(method, path, &[], "");
            println!("{} {method} {path}", reply.status);
            for (k, v) in &reply.headers {
                println!("{k}: {v}");
            }
            println!("\n{}", reply.body.trim_end());
        }
        Ok(())
    };
    // One Worker, on one loop thread
    match j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("workerd: {e}");
            ExitCode::FAILURE
        }
    }
}
