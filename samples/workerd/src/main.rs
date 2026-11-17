//! workerd: a minimal host that loads Cloudflare Workers (ES modules) and Durable Objects with j8.
//!
//! ```text
//! workerd examples/counter.js --bind COUNTER=Counter --request "GET /counter/inc?name=a"
//! workerd examples/counter.js --bind COUNTER=Counter --port 8787
//! workerd examples/chat.js --bind ROOM=ChatRoom --port 0 --instances 1000
//! ```
use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use j8::executor::{CodeCache, Executor};
use j8::platform::DefaultPlatform;
use workerd::{Worker, Workerd, ws};

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
    /// Directory for Durable Objects' SQL storage (one SQLite file per object); in memory when not given. With several
    /// instances, instance `i` keeps its storage in `<storage>/<i>`
    #[arg(long)]
    storage: Option<PathBuf>,
    /// How many copies of the Worker to serve, each its own runtime on its own port (`--port` + its index, or a free
    /// one each with `--port 0`), all in one event loop set
    #[arg(long, default_value_t = NonZeroUsize::MIN)]
    instances: NonZeroUsize,
    /// Loop threads the instances run on (default: one per instance, at most one per CPU)
    #[arg(long)]
    threads: Option<NonZeroUsize>,
}

/// Copies of one Worker to start
struct Fleet<'a> {
    script: &'a str,
    bindings: &'a [(String, String)],
    storage: &'a str,
    port: u16,
    instances: usize,
    reactor: &'a Arc<ws::Reactor>,
    code: &'a Arc<CodeCache>,
}

impl Fleet<'_> {
    /// Where instance `i` keeps its storage
    fn storage(&self, i: usize) -> Result<String, String> {
        if self.storage.is_empty() || self.instances == 1 {
            return Ok(self.storage.to_owned());
        }
        let dir = format!("{}/{i}", self.storage);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{dir}: {e}"))?;
        Ok(dir)
    }

    /// The port instance `i` asks for
    fn port(&self, i: usize) -> Result<u16, String> {
        if self.port == 0 {
            return Ok(0);
        }
        u16::try_from(i).ok().and_then(|i| self.port.checked_add(i)).ok_or_else(|| format!("no port for instance {i}"))
    }

    fn one<'p>(&self, set: &j8::EventLoopSet<'_, 'p, Executor>, i: usize) -> Result<(Worker<'p>, u16), String> {
        let mut w = Worker::new::<Workerd>(set, self.reactor, self.code)?;
        w.load(self.script, self.bindings, &self.storage(i)?)?;
        let port = w.listen(self.port(i)?)?;
        Ok((w, port))
    }

    /// Starts every instance, `hosts` at a time; gives them with their ports, by index
    fn start<'p>(&self, set: &j8::EventLoopSet<'_, 'p, Executor>, hosts: usize) -> Result<Vec<(Worker<'p>, u16)>, String> {
        let mut started: Vec<(usize, (Worker<'p>, u16))> = std::thread::scope(|s| {
            let handles: Vec<_> = (0..hosts).map(|h| s.spawn(move || (h..self.instances).step_by(hosts).map(|i| self.one(set, i).map(|w| (i, w))).collect::<Result<Vec<_>, String>>())).collect();
            handles.into_iter().map(|h| h.join().unwrap_or_else(|_| Err("a starting thread panicked".to_owned()))).collect::<Result<Vec<_>, String>>()
        })?
        .into_iter()
        .flatten()
        .collect();
        started.sort_by_key(|(i, _)| *i);
        Ok(started.into_iter().map(|(_, w)| w).collect())
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
    // One epoll thread for every socket of every instance
    let reactor_thread = match ws::ReactorThread::start() {
        Ok(r) => r,
        Err(e) => {
            eprintln!("workerd: reactor: {e}");
            return ExitCode::FAILURE;
        }
    };
    let reactor = reactor_thread.reactor();
    // The prelude and the Worker's modules compile once for every instance
    let code = CodeCache::shared();
    let instances = cli.instances.get();
    let threads = cli.threads.unwrap_or_else(|| std::thread::available_parallelism().unwrap_or(NonZeroUsize::MIN).min(cli.instances));
    let run = |set: &j8::EventLoopSet<'_, '_, Executor>| -> Result<(), String> {
        if !cli.requests.is_empty() {
            let mut rt = Worker::new::<Workerd>(set, &reactor, &code)?;
            rt.load(&script, &bindings, &storage)?;
            for r in &cli.requests {
                let (method, path) = r.split_once(' ').unwrap_or(("GET", r.as_str()));
                let reply = rt.dispatch(method, path, &[], "");
                println!("{} {method} {path}", reply.status);
                for (k, v) in &reply.headers {
                    println!("{k}: {v}");
                }
                println!("\n{}", reply.body.trim_end());
            }
            return Ok(());
        }
        let workers = Fleet { script: &script, bindings: &bindings, storage: &storage, port: cli.port, instances, reactor: &reactor, code: &code }.start(set, threads.get())?;
        if let [(_, port)] = workers.as_slice() {
            eprintln!("workerd: listening on http://127.0.0.1:{port}");
        } else {
            for (i, (_, port)) in workers.iter().enumerate() {
                eprintln!("workerd: worker {i} listening on http://127.0.0.1:{port}");
            }
            eprintln!("workerd: {instances} workers ready on {threads} loop threads");
        }
        workers[0].0.wait()
    };
    match j8::EventLoopSet::scope(&platform, std::num::NonZeroUsize::MIN, run) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("workerd: {e}");
            ExitCode::FAILURE
        }
    }
}
