//! workerd：用 r8 加载 Cloudflare Worker（ES 模块）与 Durable Object 的最小宿主。
//!
//! ```text
//! workerd examples/counter.js --bind COUNTER=Counter --request "GET /counter/inc?name=a"
//! workerd examples/counter.js --bind COUNTER=Counter --port 8787
//! ```
//! Fetch API 的子集与 Durable Object 在 `prelude.js` 里用 JS 写；每个对象的存储是 Rust 的 [`KvStore`]，
//! 经 `#[r8::class]` 绑定成 `__native.KvStore`。
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
use r8frame::{ContextRoot, CreateParams, Error, Isolate, Property, Source};
use r8platform::DefaultPlatform;

/// Fetch API 与 Durable Object 的 JS 部分。
const PRELUDE: &str = include_str!("prelude.js");
/// 请求的缺省主机名（URL 要绝对）
const HOST: &str = "http://localhost";

/// 加载一个 Worker，按请求调它的 `fetch`
#[derive(Parser)]
#[command(name = "workerd", version)]
struct Cli {
    /// Worker 脚本（ES 模块，`export default { fetch }`）
    script: PathBuf,
    /// Durable Object 绑定：`env 里的名字=导出的类名`
    #[arg(long = "bind", value_name = "NAME=CLASS")]
    bindings: Vec<String>,
    /// 一次性请求：`"方法 路径"`，可以给多次；给了就不起服务
    #[arg(long = "request", value_name = "METHOD PATH")]
    requests: Vec<String>,
    /// HTTP 服务的端口
    #[arg(long, default_value_t = 8787)]
    port: u16,
}

/// 一个 Durable Object 的存储（键 ⇒ JSON 文本）。
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

    /// `newUniqueId()` 用：时间戳加计数，十六进制。
    fn unique_id(a: &mut r8frame::Agent<'_, '_>) -> String {
        format!("{:032x}", a.platform.wall_clock().as_nanos())
    }
}

/// 收到的一个 HTTP 请求。
struct HttpRequest {
    method: String,
    path: String,
    headers: Vec<(String, String)>,
    body: String,
}

/// Worker 的一个响应。
struct Reply {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl From<&str> for Reply {
    /// `__workerd.take()` 的文本：状态行、头一行一个、空行、正文。
    fn from(text: &str) -> Self {
        let (head, body) = text.split_once("\n\n").unwrap_or((text, ""));
        let mut lines = head.lines();
        let status = lines.next().and_then(|s| s.trim().parse().ok()).unwrap_or(500);
        let headers = lines.filter_map(|l| l.split_once(": ")).map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
        Self { status, headers, body: body.to_owned() }
    }
}

/// 运行中的 Worker：一个 isolate、一个执行环境。
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

    /// 跑一段脚本，给出结果的 ToString。
    fn eval(&mut self, text: &str, file: &str) -> Result<String, String> {
        let r = self.isolate.run(&mut self.executor, &self.cx, &Source { text, file });
        let (executor, cx) = (&self.executor, &self.cx);
        self.isolate.enter(|scope| {
            let realm = scope.context_of(cx);
            let mut a = scope.agent(realm, executor);
            let text = |a: &mut r8frame::Agent<'_, '_>, g: &r8frame::Global| {
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

    /// 字符串字面量（JSON 写法）。
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

    /// 动态 import 进 Worker 模块，装好 Durable Object 绑定。
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

    /// 把一个请求交给 Worker 的 `fetch`，跑到它的 Promise 落定。
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

    /// 读一个 HTTP/1.1 请求（请求行、头、按 Content-Length 的正文）。
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

    /// HTTP 服务：一个连接一个请求（`Connection: close`）。
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
