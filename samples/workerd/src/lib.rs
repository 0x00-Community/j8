pub mod sql;
pub mod ws;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;

use j8::Host;
use j8::executor::{CodeCache, Executor};
use j8::frame::Error;

pub const PRELUDE: &str = include_str!("prelude.js");

const HOST: &str = "http://localhost";

#[derive(Default, j8::Trace)]
pub struct KvStore {
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

    fn unique_id(a: &mut j8::frame::Scope<'_, '_>) -> String {
        format!("{:032x}", a.platform.wall_clock().as_nanos())
    }
}

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

impl From<&str> for Reply {
    fn from(text: &str) -> Self {
        let (head, body) = text.split_once("\n\n").unwrap_or((text, ""));
        let mut lines = head.lines();
        let status = lines.next().and_then(|s| s.trim().parse().ok()).unwrap_or(500);
        let headers = lines.filter_map(|l| l.split_once(": ")).map(|(k, v)| (k.to_owned(), v.to_owned())).collect();
        Self { status, headers, body: body.to_owned() }
    }
}

j8::runtime! {
    pub Workerd {
        natives: [KvStore, sql::SqlStorage],
        prelude: PRELUDE,
    }
}

pub struct Worker<'p> {
    pub rt: j8::Runtime<'p>,
}

impl<'p> Worker<'p> {
    pub fn new<H: Host>(set: &j8::EventLoopSet<'_, 'p>, reactor: &Arc<ws::Reactor>, code: &Arc<CodeCache>) -> Result<Self, String> {
        let rt = j8::Runtime::on::<H>(set, Executor::default().with_cache(Arc::clone(code))).map_err(|e| format!("prelude failed: {e}"))?;
        let reactor = Arc::clone(reactor);
        rt.enter(move |a| ws::Net::attach(a, reactor));
        Ok(Self { rt })
    }

    pub fn eval(&mut self, text: &str, file: &str) -> Result<String, String> {
        match self.rt.eval(text, file).and_then(|g| self.rt.run_until_idle().map(|()| g)) {
            Ok(g) => Ok(self.rt.text(&g)),
            Err(Error::Exception) => Err(format!("Uncaught {}", self.rt.exception_text())),
            Err(e) => Err(e.to_string()),
        }
    }

    pub fn quote(s: &str) -> String {
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

    pub fn load(&mut self, script: &str, bindings: &[(String, String)], storage: &str) -> Result<(), String> {
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

    pub fn dispatch(&mut self, method: &str, path: &str, headers: &[(String, String)], body: &str) -> Reply {
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

    pub fn listen(&mut self, port: u16) -> Result<u16, String> {
        let g = match self.rt.eval(&format!("__workerd.serve({port})"), "[serve]") {
            Ok(g) => g,
            Err(Error::Exception) => return Err(format!("Uncaught {}", self.rt.exception_text())),
            Err(e) => return Err(e.to_string()),
        };
        let port = self.rt.text(&g);
        port.parse().map_err(|_| format!("the server gave port {port}"))
    }

    pub fn wait(&self) -> Result<(), String> {
        self.rt.run_until_idle().map_err(|e| e.to_string())
    }
}
