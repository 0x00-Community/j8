use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::Duration;

use serde_json::Value;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

const CLIENTS: usize = 8;

const ROUND: usize = 5;

const SILENCE: Duration = Duration::from_secs(5);

const SILENT_CPU: Duration = Duration::from_millis(100);

const USER_HZ: u64 = 100;

type Client = WebSocket<MaybeTlsStream<TcpStream>>;

#[derive(Clone, Debug, PartialEq, Eq)]
struct Said {
    id: i64,
    name: String,
    text: String,
}

impl From<&Value> for Said {
    fn from(v: &Value) -> Self {
        Self { id: v["id"].as_i64().unwrap_or(-1), name: v["name"].as_str().unwrap_or_default().to_owned(), text: v["text"].as_str().unwrap_or_default().to_owned() }
    }
}

struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn start(storage: &Path) -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut child = Command::new(env!("CARGO_BIN_EXE_workerd"))
            .arg(root.join("examples/chat.js"))
            .args(["--bind", "ROOM=ChatRoom", "--port", "0", "--storage"])
            .arg(storage)
            .stderr(Stdio::piped())
            .spawn()
            .expect("workerd starts");
        let mut lines = BufReader::new(child.stderr.take().expect("stderr")).lines();
        let port = lines
            .by_ref()
            .map_while(Result::ok)
            .find_map(|l| l.rsplit_once("listening on http://127.0.0.1:").and_then(|(_, p)| p.trim().parse().ok()))
            .expect("workerd prints its port");
        thread::spawn(move || lines.map_while(Result::ok).for_each(|l| eprintln!("[workerd] {l}")));
        Self { child, port }
    }

    fn cpu(&self) -> Duration {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", self.child.id())).expect("/proc stat");
        let fields: Vec<u64> = stat.rsplit_once(')').expect("stat format").1.split_whitespace().filter_map(|f| f.parse().ok()).collect();
        Duration::from_millis((fields[10] + fields[11]) * 1000 / USER_HZ)
    }

    fn join(&self, name: &str) -> Client {
        let (ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{}/room?name={name}", self.port)).expect("WebSocket handshake");
        ws
    }

    fn get(&self, path: &str) -> String {
        let mut s = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        write!(s, "GET {path} HTTP/1.1\r\nhost: localhost\r\n\r\n").expect("request");
        let mut out = String::new();
        s.read_to_string(&mut out).expect("response");
        out.split_once("\r\n\r\n").map(|(_, b)| b.to_owned()).unwrap_or_default()
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn next(ws: &mut Client) -> Value {
    loop {
        match ws.read().expect("message") {
            Message::Text(t) => return serde_json::from_str(&t).expect("JSON"),
            Message::Close(c) => panic!("closed early: {c:?}"),
            _ => {}
        }
    }
}

fn round(clients: Vec<Client>, tag: &str) -> (Vec<Client>, Vec<Vec<Said>>) {
    let workers: Vec<_> = clients
        .into_iter()
        .enumerate()
        .map(|(i, mut ws)| {
            let tag = tag.to_owned();
            thread::spawn(move || {
                for k in 0..ROUND {
                    ws.send(Message::text(format!("{tag}{k} from u{i}"))).expect("send");
                }
                let seen: Vec<Said> = (0..CLIENTS * ROUND).map(|_| Said::from(&next(&mut ws))).collect();
                (ws, seen)
            })
        })
        .collect();
    workers.into_iter().map(|w| w.join().expect("client thread")).unzip()
}

fn check(seen: &[Vec<Said>], tag: &str) -> Vec<Said> {
    let first = &seen[0];
    assert_eq!(first.len(), CLIENTS * ROUND);
    for (i, s) in seen.iter().enumerate() {
        assert_eq!(s, first, "client u{i} saw a different order");
    }
    assert!(first.windows(2).all(|w| w[0].id < w[1].id), "ids rise");
    for i in 0..CLIENTS {
        let mine: Vec<&str> = first.iter().filter(|m| m.name == format!("u{i}")).map(|m| m.text.as_str()).collect();
        let sent: Vec<String> = (0..ROUND).map(|k| format!("{tag}{k} from u{i}")).collect();
        assert_eq!(mine, sent, "u{i}'s messages arrive in the order sent");
    }
    first.clone()
}

#[test]
fn eight_clients_silence_then_restart() {
    let storage = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("chat-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&storage);

    let server = Server::start(&storage);
    let clients: Vec<Client> = (0..CLIENTS).map(|i| server.join(&format!("u{i}"))).collect();
    let (mut clients, seen) = round(clients, "a");
    let mut log = check(&seen, "a");

    let before = server.cpu();
    thread::sleep(SILENCE);
    let spent = server.cpu().saturating_sub(before);
    eprintln!("server CPU during {SILENCE:?} of silence: {spent:?}");
    assert!(spent < SILENT_CPU, "the server used {spent:?} of CPU while the room was silent");
    for ws in &mut clients {
        if let MaybeTlsStream::Plain(s) = ws.get_mut() {
            s.set_nonblocking(true).expect("nonblocking");
        }
        match ws.read() {
            Err(tungstenite::Error::Io(e)) if e.kind() == std::io::ErrorKind::WouldBlock => {}
            other => panic!("something arrived during the silence: {other:?}"),
        }
        if let MaybeTlsStream::Plain(s) = ws.get_mut() {
            s.set_nonblocking(false).expect("blocking");
        }
    }

    let (clients, seen) = round(clients, "b");
    let second = check(&seen, "b");
    assert!(second[0].id > log[log.len() - 1].id, "ids keep rising after the silence");
    log.extend(second);

    for mut ws in clients {
        ws.close(None).expect("close");
        while ws.read().is_ok() {}
    }
    drop(server);

    let server = Server::start(&storage);
    let mut late = server.join("late");
    let history: Vec<Said> = (0..log.len())
        .map(|_| {
            let v = next(&mut late);
            assert_eq!(v["history"], Value::Bool(true));
            Said::from(&v)
        })
        .collect();
    assert_eq!(history, log, "the history replays every message in broadcast order");
    late.send(Message::text("back again")).expect("send");
    let echo = Said::from(&next(&mut late));
    assert_eq!((echo.name.as_str(), echo.text.as_str()), ("late", "back again"));
    assert!(echo.id > log[log.len() - 1].id);

    let status: Value = serde_json::from_str(&server.get("/room")).expect("JSON status");
    assert_eq!(status["messages"].as_u64(), Some((log.len() + 1) as u64));
    assert_eq!(status["online"].as_u64(), Some(1));
    let _ = std::fs::remove_dir_all(&storage);
}
