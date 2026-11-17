//! A thousand chat rooms in one process: `workerd --instances 1000`, one runtime per Worker on its own port, all on
//! one event loop set. Two clients per room, two rounds of messages in every room at once with five silent seconds
//! between them (the whole process must sleep), then a restart that replays every room's history.
//!
//! Heavy: run it on its own, in release (`cargo test --release --test workerds -- --nocapture`).
use std::io::{BufRead, BufReader};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde_json::Value;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Message, WebSocket};

/// Chat rooms, one Worker each
const WORKERS: usize = 1000;
/// Clients in each room
const CLIENTS: usize = 2;
/// Messages each client sends per round
const ROUND: usize = 5;
/// The quiet stretch between the two rounds
const SILENCE: Duration = Duration::from_secs(5);
/// Server CPU time allowed during the silence: a thousand sleeping event loops use next to none
const SILENT_CPU: Duration = Duration::from_millis(200);
/// Linux clock ticks per second in /proc/<pid>/stat (USER_HZ)
const USER_HZ: u64 = 100;

type Client = WebSocket<MaybeTlsStream<TcpStream>>;

/// One broadcast as a client saw it
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

/// A running `workerd examples/chat.js --instances WORKERS`
struct Server {
    child: Child,
    /// By instance (and so by storage directory)
    ports: Vec<u16>,
}

impl Server {
    fn start(storage: &Path) -> Self {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        let mut child = Command::new(env!("CARGO_BIN_EXE_workerd"))
            .arg(root.join("examples/chat.js"))
            .args(["--bind", "ROOM=ChatRoom", "--port", "0", "--instances", &WORKERS.to_string(), "--storage"])
            .arg(storage)
            .stderr(Stdio::piped())
            .spawn()
            .expect("workerd starts");
        let mut lines = BufReader::new(child.stderr.take().expect("stderr")).lines();
        let mut ports = vec![0; WORKERS];
        for line in lines.by_ref().map_while(Result::ok) {
            if line.contains("workers ready") {
                eprintln!("{line}");
                break;
            }
            let Some((head, port)) = line.rsplit_once(" listening on http://127.0.0.1:") else {
                eprintln!("[workerd] {line}");
                continue;
            };
            let i: usize = head.rsplit(' ').next().and_then(|i| i.parse().ok()).expect("worker index");
            ports[i] = port.trim().parse().expect("port");
        }
        assert!(ports.iter().all(|&p| p != 0), "every worker listens");
        thread::spawn(move || lines.map_while(Result::ok).for_each(|l| eprintln!("[workerd] {l}")));
        Self { child, ports }
    }

    /// User + system CPU time the server has used so far
    fn cpu(&self) -> Duration {
        let stat = std::fs::read_to_string(format!("/proc/{}/stat", self.child.id())).expect("/proc stat");
        let fields: Vec<u64> = stat.rsplit_once(')').expect("stat format").1.split_whitespace().filter_map(|f| f.parse().ok()).collect();
        Duration::from_millis((fields[10] + fields[11]) * 1000 / USER_HZ)
    }

    /// Threads of the server process
    fn threads(&self) -> usize {
        std::fs::read_dir(format!("/proc/{}/task", self.child.id())).map_or(0, Iterator::count)
    }

    /// Resident memory of the server process
    fn rss(&self) -> u64 {
        let statm = std::fs::read_to_string(format!("/proc/{}/statm", self.child.id())).unwrap_or_default();
        statm.split_whitespace().nth(1).and_then(|p| p.parse::<u64>().ok()).unwrap_or(0) * 4096
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn join(port: u16, name: &str) -> Client {
    let (ws, _) = tungstenite::connect(format!("ws://127.0.0.1:{port}/room?name={name}")).expect("WebSocket handshake");
    ws
}

/// Reads the next broadcast
fn next(ws: &mut Client) -> Value {
    loop {
        match ws.read().expect("message") {
            Message::Text(t) => return serde_json::from_str(&t).expect("JSON"),
            Message::Close(c) => panic!("closed early: {c:?}"),
            _ => {}
        }
    }
}

/// Runs `f` for every room at once, one thread each; gives the results by room
fn every_room<T: Send>(rooms: Vec<T>, f: impl Fn(usize, T) -> T + Sync) -> Vec<T> {
    thread::scope(|s| {
        let f = &f;
        let handles: Vec<_> = rooms.into_iter().enumerate().map(|(r, x)| s.spawn(move || f(r, x))).collect();
        handles.into_iter().map(|h| h.join().expect("room thread")).collect()
    })
}

/// One room's clients and everything each of them saw, in order
struct Room {
    clients: Vec<Client>,
    seen: Vec<Vec<Said>>,
}

/// Every client of the room sends `ROUND` messages, then reads all `CLIENTS * ROUND` of the round; checks the order
fn round(r: usize, mut room: Room, tag: &str) -> Room {
    for (c, ws) in room.clients.iter_mut().enumerate() {
        for k in 0..ROUND {
            ws.send(Message::text(format!("{tag}{k} from r{r}c{c}"))).expect("send");
        }
    }
    let seen: Vec<Vec<Said>> = room.clients.iter_mut().map(|ws| (0..CLIENTS * ROUND).map(|_| Said::from(&next(ws))).collect()).collect();
    let first = &seen[0];
    assert!(seen.iter().all(|s| s == first), "room {r}: every client sees the same order");
    assert!(first.windows(2).all(|w| w[0].id < w[1].id), "room {r}: ids rise");
    for c in 0..CLIENTS {
        let mine: Vec<&str> = first.iter().filter(|m| m.name == format!("c{c}")).map(|m| m.text.as_str()).collect();
        let sent: Vec<String> = (0..ROUND).map(|k| format!("{tag}{k} from r{r}c{c}")).collect();
        assert_eq!(mine, sent, "room {r}: c{c}'s messages arrive in the order sent");
    }
    for (all, new) in room.seen.iter_mut().zip(seen) {
        all.extend(new);
    }
    room
}

/// Messages per second and broadcasts delivered per second, for one round over every room
fn rate(took: Duration) -> String {
    let count = |n: usize| f64::from(u32::try_from(n).unwrap_or(u32::MAX));
    let (sent, delivered) = (count(WORKERS * CLIENTS * ROUND), count(WORKERS * CLIENTS * ROUND * CLIENTS));
    format!("{took:.1?}: {:.0} messages/s, {:.0} deliveries/s", sent / took.as_secs_f64(), delivered / took.as_secs_f64())
}

#[test]
fn a_thousand_chat_rooms_in_one_process() {
    let storage = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("workerds-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&storage);

    let t = Instant::now();
    let server = Server::start(&storage);
    eprintln!("start {WORKERS} workers: {:.1?}", t.elapsed());

    let t = Instant::now();
    let rooms = every_room((0..WORKERS).map(|_| Room { clients: Vec::new(), seen: vec![Vec::new(); CLIENTS] }).collect(), |r, mut room| {
        room.clients = (0..CLIENTS).map(|c| join(server.ports[r], &format!("c{c}"))).collect();
        room
    });
    eprintln!("join {} clients: {:.1?} (server: {} threads, {} MiB)", WORKERS * CLIENTS, t.elapsed(), server.threads(), server.rss() >> 20);

    let t = Instant::now();
    let mut rooms = every_room(rooms, |r, room| round(r, room, "a"));
    eprintln!("round a, {}", rate(t.elapsed()));

    // Silence everywhere: nothing arrives, and the whole server sleeps
    let before = server.cpu();
    thread::sleep(SILENCE);
    let spent = server.cpu().saturating_sub(before);
    eprintln!("server CPU during {SILENCE:?} of silence in {WORKERS} rooms: {spent:?}");
    assert!(spent < SILENT_CPU, "the server used {spent:?} of CPU while every room was silent");
    for room in &mut rooms {
        for ws in &mut room.clients {
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
    }

    let t = Instant::now();
    let rooms = every_room(rooms, |r, room| round(r, room, "b"));
    eprintln!("round b, {}", rate(t.elapsed()));

    let logs: Vec<Vec<Said>> = rooms
        .into_iter()
        .map(|mut room| {
            for ws in &mut room.clients {
                ws.close(None).expect("close");
                while ws.read().is_ok() {}
            }
            room.seen.swap_remove(0)
        })
        .collect();
    drop(server);

    // A new process on the same storage: every room replays its history to a newcomer
    let t = Instant::now();
    let server = Server::start(&storage);
    eprintln!("restart {WORKERS} workers: {:.1?}", t.elapsed());
    let t = Instant::now();
    let replayed = every_room(logs, |r, log| {
        let mut late = join(server.ports[r], "late");
        let history: Vec<Said> = (0..log.len())
            .map(|_| {
                let v = next(&mut late);
                assert_eq!(v["history"], Value::Bool(true), "room {r}: replayed");
                Said::from(&v)
            })
            .collect();
        assert_eq!(history, log, "room {r}: the history replays every message in broadcast order");
        history
    });
    eprintln!("replay {} messages to {WORKERS} newcomers: {:.1?}", replayed.iter().map(Vec::len).sum::<usize>(), t.elapsed());
    drop(server);
    let _ = std::fs::remove_dir_all(&storage);
}
