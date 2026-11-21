use std::fs::{self, File};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;
use std::time::{Duration, Instant};

const SUITE: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/node");
const DIRS: [&str; 2] = ["parallel", "sequential"];
const PREFIX: &str = "test-";
const EXTENSIONS: [&str; 3] = ["js", "mjs", "cjs"];
const DUNE: &str = env!("CARGO_BIN_EXE_dune");
const SCRATCH: &str = env!("CARGO_TARGET_TMPDIR");
const LIMIT: Duration = Duration::from_secs(10);
const POLL: Duration = Duration::from_millis(5);
const FILTER: &str = "DUNE_NODE_TEST";
const TAIL_LINES: usize = 20;

struct Case {
    path: PathBuf,
}

struct Suite {
    cases: Vec<Case>,
    next: AtomicUsize,
    failures: Mutex<Vec<String>>,
}

impl Case {
    fn name(&self) -> String {
        self.path.strip_prefix(SUITE).unwrap_or(&self.path).display().to_string()
    }

    fn run(&self, scratch: &Path) -> Result<(), String> {
        let tmp = scratch.join("tmp");
        let _ = fs::remove_dir_all(scratch);
        fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
        let log = scratch.join("log");
        let out = File::create(&log).map_err(|e| e.to_string())?;
        let err = out.try_clone().map_err(|e| e.to_string())?;
        let dir = self.path.parent().unwrap_or(Path::new(SUITE));
        let mut child = Command::new(DUNE).arg(&self.path).current_dir(dir).env("NODE_TEST_DIR", &tmp).stdin(File::open("/dev/null").map_err(|e| e.to_string())?).stdout(out).stderr(err).spawn().map_err(|e| e.to_string())?;
        let start = Instant::now();
        let status = loop {
            if let Some(status) = child.try_wait().map_err(|e| e.to_string())? {
                break Some(status);
            }
            if start.elapsed() > LIMIT {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            thread::sleep(POLL);
        };
        let text = fs::read_to_string(&log).unwrap_or_default();
        let _ = fs::remove_dir_all(scratch);
        let tail = text.lines().rev().take(TAIL_LINES).collect::<Vec<_>>().into_iter().rev().collect::<Vec<_>>().join("\n");
        match status {
            Some(s) if s.success() => Ok(()),
            Some(s) => Err(format!("{}: {s}\n{tail}", self.name())),
            None => Err(format!("{}: timed out after {LIMIT:?}\n{tail}", self.name())),
        }
    }
}

impl Suite {
    fn collect(filter: Option<&str>) -> Self {
        let mut cases = DIRS
            .iter()
            .filter_map(|d| fs::read_dir(Path::new(SUITE).join(d)).ok())
            .flatten()
            .filter_map(Result::ok)
            .map(|e| e.path())
            .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with(PREFIX)) && p.extension().and_then(|e| e.to_str()).is_some_and(|e| EXTENSIONS.contains(&e)))
            .filter(|p| filter.is_none_or(|f| p.to_string_lossy().contains(f)))
            .map(|path| Case { path })
            .collect::<Vec<_>>();
        cases.sort_by(|a, b| a.path.cmp(&b.path));
        Self { cases, next: AtomicUsize::new(0), failures: Mutex::new(Vec::new()) }
    }

    fn work(&self, worker: usize) {
        let scratch = Path::new(SCRATCH).join(format!("node-{}-{worker}", std::process::id()));
        while let Some(case) = self.cases.get(self.next.fetch_add(1, Ordering::Relaxed)) {
            if let Err(e) = case.run(&scratch) {
                self.failures.lock().unwrap_or_else(std::sync::PoisonError::into_inner).push(e);
            }
        }
    }

    fn run(self) -> Vec<String> {
        let workers = thread::available_parallelism().map_or(1, std::num::NonZeroUsize::get);
        thread::scope(|s| {
            for worker in 0..workers {
                let suite = &self;
                s.spawn(move || suite.work(worker));
            }
        });
        let mut failures = self.failures.into_inner().unwrap_or_else(std::sync::PoisonError::into_inner);
        failures.sort();
        failures
    }
}

#[test]
fn node_suite() {
    let filter = std::env::var(FILTER).ok();
    let suite = Suite::collect(filter.as_deref());
    let total = suite.cases.len();
    assert!(total > 0, "no cases under {SUITE} match {filter:?}");
    let failures = suite.run();
    assert!(failures.is_empty(), "{} of {total} Node tests failed:\n\n{}", failures.len(), failures.join("\n\n"));
}
