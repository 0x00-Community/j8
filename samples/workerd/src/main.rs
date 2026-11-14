//! workerd: a minimal host that loads Cloudflare Workers (ES modules) and Durable Objects with r8.
//!
//! ```text
//! workerd examples/counter.js --bind COUNTER=Counter --request "GET /counter/inc?name=a"
//! workerd examples/counter.js --bind COUNTER=Counter --port 8787
//! ```
//! The Fetch API subset and Durable Objects are written in JS in `prelude.js`; each object's storage is the Rust
//! [`KvStore`], bound as `__native.KvStore` via `#[r8::class]`.
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use r8::Class;
use r8builtins::Builtins;
use r8executor::Executor;
use r8framework::{ContextRoot, CreateParams, Error, Isolate, Property, Source};
use r8platform::DefaultPlatform;

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
    /// Port for the HTTP server
    #[arg(long, default_value_t = 8787)]
    port: u16,
}

/// Storage of one Durable Object (key ⇒ JSON text).
#[derive(Default)]
struct KvStore {
    map: BTreeMap<String, String>,
}

#[r8::class]
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
    fn unique_id(a: &mut r8framework::Agent<'_, '_>) -> String {
        format!("{:032x}", a.platform.wall_clock().as_nanos())
    }
}

/// A received HTTP request.
struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
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

/// A running Worker: one isolate, one execution environment.
struct Runtime<'p> {
    isolate: Isolate<'p>,
    executor: Executor,
    cx: ContextRoot,
}

impl<'p> Runtime<'p> {
    fn new(platform: &'p DefaultPlatform) -> Result<Self, String> {
        let mut isolate = Isolate::new(platform, CreateParams::default());
        let executor = Executor::default();
        let cx = isolate.enter(|scope| scope.persist(scope.context(&Builtins)));
        isolate.enter(|scope| {
            let realm = scope.context_of(&cx);
            let mut a = scope.agent(realm, &executor);
            let native = a.object();
            let global = a.realm.global;
            global.borrow_mut(a.mc).define("__native", Property::hidden(native.into()));
            KvStore::install(&mut a, native);
        });
        let mut rt = Self { isolate, executor, cx };
        rt.eval(PRELUDE, "[prelude]")?;
        Ok(rt)
    }

    /// Run a script and return ToString of the result.
    fn eval(&mut self, text: &str, file: &str) -> Result<String, String> {
        let r = self.isolate.run(&mut self.executor, &self.cx, &Source { text, file });
        let (executor, cx) = (&self.executor, &self.cx);
        self.isolate.enter(|scope| {
            let realm = scope.context_of(cx);
            let mut a = scope.agent(realm, executor);
            let text = |a: &mut r8framework::Agent<'_, '_>, g: &r8framework::Global| {
                let v = a.local(g);
                a.to_string(v).map_or_else(|_| String::from("<ToString threw>"), |s| String::from(&**s))
            };
            match r {
                Ok(g) => Ok(text(&mut a, &g)),
                Err(Error::Exception(g)) => Err(format!("Uncaught {}", text(&mut a, &g))),
                Err(e) => Err(e.to_string()),
            }
        })
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

    /// Dynamically import the Worker module and install the Durable Object bindings.
    fn load(&mut self, script: &str, bindings: &[(String, String)]) -> Result<(), String> {
        let pairs: Vec<String> = bindings.iter().map(|(n, c)| format!("[{}, {}]", Self::quote(n), Self::quote(c))).collect();
        let src = format!(
            "import({}).then(ns => __workerd.setup(ns, [{}])).catch(e => {{ __workerd.error = String(e && e.stack || e); }})",
            Self::quote(script),
            pairs.join(", ")
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

    /// Read an HTTP/1.1 request (request line, headers, body by Content-Length).
    fn read_request(stream: &mut TcpStream) -> Option<HttpRequest> {
        let mut reader = BufReader::new(stream);
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let mut parts = line.split_whitespace();
        let (method, path) = (parts.next()?.to_owned(), parts.next()?.to_owned());
        let mut headers = Vec::new();
        let mut length = 0;
        loop {
            line.clear();
            reader.read_line(&mut line).ok()?;
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':') {
                let (k, v) = (k.trim().to_owned(), v.trim().to_owned());
                if k.eq_ignore_ascii_case("content-length") {
                    length = v.parse().unwrap_or(0);
                }
                headers.push((k, v));
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).ok()?;
        Some(HttpRequest { method, path, headers, body: String::from_utf8_lossy(&body).into_owned() })
    }

    /// HTTP server: one request per connection (`Connection: close`).
    fn serve(&mut self, port: u16) -> Result<(), String> {
        let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| format!("bind 127.0.0.1:{port}: {e}"))?;
        eprintln!("workerd: listening on http://127.0.0.1:{port}");
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let Some(HttpRequest { method, path, headers, body }) = Self::read_request(&mut stream) else { continue };
            let reply = self.dispatch(&method, &path, &headers, &body);
            eprintln!("{method} {path} -> {}", reply.status);
            let mut out = format!("HTTP/1.1 {} {}\r\n", reply.status, if reply.status < 400 { "OK" } else { "Error" });
            for (k, v) in &reply.headers {
                let _ = write!(out, "{k}: {v}\r\n");
            }
            let _ = write!(out, "content-length: {}\r\nconnection: close\r\n\r\n", reply.body.len());
            out.push_str(&reply.body);
            let _ = stream.write_all(out.as_bytes());
        }
        Ok(())
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
    let script = match std::fs::canonicalize(&cli.script) {
        Ok(p) => p.display().to_string(),
        Err(e) => {
            eprintln!("workerd: {}: {e}", cli.script.display());
            return ExitCode::FAILURE;
        }
    };
    let platform = DefaultPlatform::default();
    let run = || -> Result<(), String> {
        let mut rt = Runtime::new(&platform)?;
        rt.load(&script, &bindings)?;
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
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("workerd: {e}");
            ExitCode::FAILURE
        }
    }
}
