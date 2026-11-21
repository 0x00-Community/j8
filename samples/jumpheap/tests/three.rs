use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::process::{Child, Command, Stdio};
use std::time::Instant;

const COLS: usize = 100_000;

const ROWS: usize = 40;

const PERIOD: usize = 9;

const SEEDS: [usize; 3] = [1, 2, 3];

struct Worker {
    child: Child,
    port: u16,
}

impl Worker {
    fn start(name: &str, dir: &str) -> Self {
        let mut child = Command::new(env!("CARGO_BIN_EXE_jumpheap"))
            .args([concat!(env!("CARGO_MANIFEST_DIR"), "/examples/matrix.js"), "--name", name, "--dir", dir])
            .stderr(Stdio::piped())
            .spawn()
            .expect("jumpheap starts");
        let mut line = String::new();
        BufReader::new(child.stderr.take().expect("stderr")).read_line(&mut line).expect("a line");
        let port = line.trim().rsplit(':').next().and_then(|p| p.parse().ok()).unwrap_or_else(|| panic!("{name}: {line}"));
        Self { child, port }
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn call(&self, method: &str, path: &str) -> serde_json::Value {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        write!(s, "{method} {path} HTTP/1.1\r\nhost: x\r\ncontent-length: 0\r\nconnection: close\r\n\r\n").expect("send");
        let mut out = String::new();
        s.read_to_string(&mut out).expect("receive");
        let (head, body) = out.split_once("\r\n\r\n").expect("an HTTP response");
        assert!(head.starts_with("HTTP/1.1 200"), "{method} {path}: {out}");
        serde_json::from_str(body).unwrap_or_else(|e| panic!("{method} {path}: {e}: {body}"))
    }

    fn timed(&self, method: &str, path: &str) -> serde_json::Value {
        let t = Instant::now();
        let v = self.call(method, path);
        eprintln!("{method} {path}: {v} ({:.2?})", t.elapsed());
        v
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct Filled {
    dot: [[i64; PERIOD]; PERIOD],
    sum: [i64; PERIOD],
}

impl Filled {
    fn new() -> Self {
        let value = |k: usize, j: usize| i64::try_from((k + j * 13) % PERIOD).expect("small") - 4;
        Self {
            dot: core::array::from_fn(|k| core::array::from_fn(|l| (0..COLS).map(|j| value(k, j) * value(l, j)).sum())),
            sum: core::array::from_fn(|k| (0..COLS).map(|j| value(k, j)).sum()),
        }
    }

    fn rows(seed: usize, rows: usize) -> Vec<usize> {
        (0..rows).map(|i| (i * 7 + seed * 31) % PERIOD).collect()
    }

    fn dot(&self, x: &[usize], y: &[usize]) -> i64 {
        x.iter().zip(y).map(|(&k, &l)| self.dot[k][l]).sum()
    }

    fn sum(&self, x: &[usize]) -> i64 {
        x.iter().map(|&k| self.sum[k]).sum()
    }
}

fn int(v: &serde_json::Value) -> i64 {
    let x = v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"));
    #[expect(clippy::cast_possible_truncation, reason = "checked to be an integer below")]
    let n = x as i64;
    assert!((n as f64 - x).abs() < f64::EPSILON, "{x} is not an integer");
    n
}

#[test]
fn half_of_a_dead_heap() {
    let rows: usize = std::env::var("JUMPHEAP_ROWS").ok().and_then(|r| r.parse().ok()).unwrap_or(ROWS);
    let dir = format!("/dev/shm/jumpheap-test-{}", std::process::id());
    let [wa, mut wb, wc] = ["wa", "wb", "wc"].map(|n| Worker::start(n, &dir));
    let f = Filled::new();
    let [ra, rb, rc] = SEEDS.map(|s| Filled::rows(s, rows));
    let half = rows / 2;
    let a: Vec<usize> = rb[..half].iter().chain(&ra[half..]).copied().collect();

    for (w, seed) in [&wa, &wb, &wc].into_iter().zip(SEEDS) {
        let made = w.timed("POST", &format!("/create?rows={rows}&seed={seed}"));
        assert_eq!(made["rows"], rows);
        assert_eq!(made["cols"], COLS);
    }
    let before = wc.timed("POST", &format!("/send?to={}&name=c&task=dot", wa.url()));
    assert_eq!(int(&before["dot"]), f.dot(&ra, &rc));
    let written = wa.timed("POST", &format!("/send?to={}&name=a&task=overwrite", wb.url()));
    assert_eq!(written["rows"], half);
    wb.child.kill().expect("wb dies");
    wb.child.wait().expect("wb is gone");
    let after = wc.timed("POST", &format!("/send?to={}&name=c2&task=dot", wa.url()));
    assert_eq!(int(&after["dot"]), f.dot(&a, &rc));
    assert_ne!(f.dot(&a, &rc), f.dot(&ra, &rc), "the overwrite changes a·c");
    let sum = wa.timed("GET", "/sum?of=self");
    assert_eq!(int(&sum["sum"]), f.sum(&a));
    drop([wa, wb, wc]);
    let _ = std::fs::remove_dir_all(&dir);
}
