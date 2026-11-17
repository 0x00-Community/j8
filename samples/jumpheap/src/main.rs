//! jumpheap: a j8 worker whose isolate holds references into other processes' heaps.
//!
//! ```text
//! jumpheap examples/matrix.js --name wa --dir /dev/shm/jumpheap --port 0
//! ```
//! Each process owns a paragc public heap, `<dir>/<name>.heap`: a memory file other processes map. Its worker (a
//! workerd-style Worker, `export default { fetch }`, resident for the life of the process) makes JS matrices whose
//! rows live there, sends them to other workers over HTTP (`heaps.send`), and listens for theirs (`object` events):
//! what arrives is a JS `Matrix` in its own isolate heap whose rows are another process's heap, not a copy.
mod http;
mod matrix;

use std::num::NonZeroUsize;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

use clap::Parser;
use j8::executor::CodeCache;
use j8::platform::DefaultPlatform;
use workerd::{Worker, ws};

/// fetch and heaps, on top of workerd's prelude
const PRELUDE: &str = include_str!("prelude.js");

j8::runtime! {
    /// A Worker with outbound HTTP and matrices in public heaps
    pub JumpHeap: workerd::Workerd {
        natives: [http::Http],
        prelude: PRELUDE,
    }
}

/// Serve a worker whose matrices live in this process's public heap
#[derive(Parser)]
#[command(name = "jumpheap", version)]
struct Cli {
    /// Worker script (ES module, `export default { fetch }`)
    script: PathBuf,
    /// This worker's name: its heap is `<dir>/<name>.heap`
    #[arg(long)]
    name: String,
    /// Directory of the public heaps, on a shared-memory filesystem (`/dev/shm/…`)
    #[arg(long)]
    dir: PathBuf,
    /// HTTP port (`0` picks a free one, printed on stderr)
    #[arg(long, default_value_t = 0)]
    port: u16,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let run = || -> Result<(), String> {
        std::fs::create_dir_all(&cli.dir).map_err(|e| format!("{}: {e}", cli.dir.display()))?;
        let space = matrix::Space::create(cli.dir.join(format!("{}.heap", cli.name))).map_err(|e| format!("public heap: {e}"))?;
        let script = std::fs::canonicalize(&cli.script).map_err(|e| format!("{}: {e}", cli.script.display()))?.display().to_string();
        let platform = JumpHeap::platform(DefaultPlatform::default());
        let reactor_thread = ws::ReactorThread::start().map_err(|e| format!("reactor: {e}"))?;
        let reactor = reactor_thread.reactor();
        let code = CodeCache::shared();
        j8::EventLoopSet::scope(&platform, NonZeroUsize::MIN, |set| {
            let mut worker = Worker::new::<JumpHeap>(set, &reactor, &code)?;
            let heaps = Arc::clone(&space);
            worker.rt.enter(move |a| matrix::Heaps::attach(a, heaps));
            worker.load(&script, &[], "")?;
            let port = worker.listen(cli.port)?;
            eprintln!("jumpheap: {} listening on http://127.0.0.1:{port}", cli.name);
            worker.wait()
        })
    };
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("jumpheap: {e}");
            ExitCode::FAILURE
        }
    }
}
