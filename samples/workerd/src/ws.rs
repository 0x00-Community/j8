use std::cell::UnsafeCell;
use std::collections::HashMap;
use std::fmt::Write as _;
use std::future::Future;
use std::io::{self, Cursor, Read, Write};
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Weak};
use std::task::{Context, Poll, Waker};
use std::thread::JoinHandle;

use j8::frame::{Agent, Error, ErrorKind, JsObject, Property, Value};
use j8::lockfree::{Fifo, SegVec};
use j8::{Bytes, Class, Glue, IntoJs};
use mio::net::{TcpListener, TcpStream};
use mio::{Events, Interest, Token};

const MAX_MESSAGE: usize = 1 << 20;

const HANDSHAKE_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";

const CLOSE_NO_STATUS: u16 = 1005;
const CLOSE_TOO_BIG: u16 = 1009;
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

const WAKE: Token = Token(usize::MAX);

const READ_CHUNK: usize = 64 * 1024;

const EVENTS: usize = 1024;

const IDLE: u8 = 0;
const REGISTERING: u8 = 1;
const WAKING: u8 = 2;

struct WakeSlot {
    state: AtomicU8,
    waker: UnsafeCell<Option<Waker>>,
}

unsafe impl Sync for WakeSlot {}

impl Default for WakeSlot {
    fn default() -> Self {
        Self { state: AtomicU8::new(IDLE), waker: UnsafeCell::new(None) }
    }
}

impl WakeSlot {
    fn register(&self, w: &Waker) {
        match self.state.compare_exchange(IDLE, REGISTERING, Ordering::Acquire, Ordering::Acquire) {
            Ok(_) => {
                unsafe { *self.waker.get() = Some(w.clone()) };
                if self.state.compare_exchange(REGISTERING, IDLE, Ordering::AcqRel, Ordering::Acquire).is_err() {
                    let w = unsafe { (*self.waker.get()).take() };
                    self.state.store(IDLE, Ordering::Release);
                    if let Some(w) = w {
                        w.wake();
                    }
                }
            }
            Err(WAKING) => w.wake_by_ref(),
            Err(_) => {}
        }
    }

    fn wake(&self) {
        if self.state.fetch_or(WAKING, Ordering::AcqRel) == IDLE {
            let w = unsafe { (*self.waker.get()).take() };
            self.state.fetch_and(!WAKING, Ordering::Release);
            if let Some(w) = w {
                w.wake();
            }
        }
    }
}

struct Stream<T> {
    items: Fifo<T>,
    slot: WakeSlot,
    closed: AtomicBool,
}

impl<T> Default for Stream<T> {
    fn default() -> Self {
        Self { items: Fifo::default(), slot: WakeSlot::default(), closed: AtomicBool::new(false) }
    }
}

impl<T> Stream<T> {
    fn push(&self, t: T) {
        self.items.push(t);
        self.slot.wake();
    }

    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.slot.wake();
    }
}

struct Next<T>(Arc<Stream<T>>);

impl<T> Future for Next<T> {
    type Output = Option<T>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<T>> {
        let s = &self.0;
        if let Some(t) = s.items.pop() {
            return Poll::Ready(Some(t));
        }
        s.slot.register(cx.waker());
        if let Some(t) = s.items.pop() {
            return Poll::Ready(Some(t));
        }
        if s.closed.load(Ordering::Acquire) {
            return Poll::Ready(s.items.pop());
        }
        Poll::Pending
    }
}

pub struct Incoming {
    id: u32,
    method: String,
    path: String,
    headers: Vec<Vec<String>>,
    body: String,
}

enum Parsed {
    More,
    Bad,
    Request(Incoming, usize),
}

impl Incoming {
    fn parse(id: u32, buf: &[u8]) -> Parsed {
        let Some(end) = buf.windows(4).position(|w| w == b"\r\n\r\n") else { return Parsed::More };
        let head = String::from_utf8_lossy(&buf[..end]);
        let mut lines = head.split("\r\n");
        let mut first = lines.next().unwrap_or_default().split_whitespace();
        let (Some(method), Some(path)) = (first.next(), first.next()) else { return Parsed::Bad };
        let mut headers = Vec::new();
        let mut length = 0;
        for l in lines {
            if let Some((k, v)) = l.split_once(':') {
                let (k, v) = (k.trim().to_owned(), v.trim().to_owned());
                if k.eq_ignore_ascii_case("content-length") {
                    let Ok(n) = v.parse() else { return Parsed::Bad };
                    length = n;
                }
                headers.push(vec![k, v]);
            }
        }
        let total = end + 4 + length;
        if buf.len() < total {
            return Parsed::More;
        }
        let body = String::from_utf8_lossy(&buf[end + 4..total]).into_owned();
        Parsed::Request(Self { id, method: method.to_owned(), path: path.to_owned(), headers, body }, total)
    }

    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|h| h[0].eq_ignore_ascii_case(name)).map(|h| h[1].as_str())
    }

    fn websocket_key(&self) -> Option<String> {
        let upgrade = self.header("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
        upgrade.then(|| self.header("sec-websocket-key")).flatten().map(str::to_owned)
    }

    fn accept_key(key: &str) -> String {
        let digest = sha1_smol::Sha1::from(format!("{key}{HANDSHAKE_GUID}")).digest().bytes();
        let mut out = String::with_capacity(28);
        for chunk in digest.chunks(3) {
            let n = chunk.iter().enumerate().fold(0u32, |n, (i, &b)| n | u32::from(b) << (16 - 8 * i));
            for i in 0..4 {
                out.push(if i <= chunk.len() { char::from(BASE64[(n >> (18 - 6 * i) & 63) as usize]) } else { '=' });
            }
        }
        out
    }
}

fn record<'gc>(a: &mut Agent<'gc, '_>, fields: Vec<(&str, Value<'gc>)>) -> JsObject<'gc> {
    let o = a.object();
    for (k, v) in fields {
        o.borrow_mut(a.mc).define(k, Property::data(v));
    }
    o
}

impl<'gc> IntoJs<'gc> for Incoming {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        let fields = vec![
            ("id", self.id.into_js(a)?),
            ("method", self.method.into_js(a)?),
            ("path", self.path.into_js(a)?),
            ("headers", self.headers.into_js(a)?),
            ("body", self.body.into_js(a)?),
        ];
        Ok(record(a, fields).into())
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Opcode {
    Continuation,
    Text,
    Binary,
    Close,
    Ping,
    Pong,
}

impl TryFrom<u8> for Opcode {
    type Error = io::Error;

    fn try_from(b: u8) -> io::Result<Self> {
        Ok(match b {
            0x0 => Self::Continuation,
            0x1 => Self::Text,
            0x2 => Self::Binary,
            0x8 => Self::Close,
            0x9 => Self::Ping,
            0xa => Self::Pong,
            _ => return Err(io::Error::new(io::ErrorKind::InvalidData, "unknown opcode")),
        })
    }
}

impl From<Opcode> for u8 {
    fn from(op: Opcode) -> u8 {
        match op {
            Opcode::Continuation => 0x0,
            Opcode::Text => 0x1,
            Opcode::Binary => 0x2,
            Opcode::Close => 0x8,
            Opcode::Ping => 0x9,
            Opcode::Pong => 0xa,
        }
    }
}

struct Frame {
    fin: bool,
    op: Opcode,
    payload: Vec<u8>,
}

impl Frame {
    fn read(r: &mut impl Read) -> io::Result<Self> {
        let mut head = [0u8; 2];
        r.read_exact(&mut head)?;
        let (fin, op, masked) = (head[0] & 0x80 != 0, Opcode::try_from(head[0] & 0x0f)?, head[1] & 0x80 != 0);
        let len = match head[1] & 0x7f {
            126 => {
                let mut b = [0u8; 2];
                r.read_exact(&mut b)?;
                usize::from(u16::from_be_bytes(b))
            }
            127 => {
                let mut b = [0u8; 8];
                r.read_exact(&mut b)?;
                usize::try_from(u64::from_be_bytes(b)).unwrap_or(usize::MAX)
            }
            n => usize::from(n),
        };
        if len > MAX_MESSAGE {
            return Err(io::Error::new(io::ErrorKind::InvalidData, "frame too large"));
        }
        let mut mask = [0u8; 4];
        if masked {
            r.read_exact(&mut mask)?;
        }
        let mut payload = vec![0; len];
        r.read_exact(&mut payload)?;
        if masked {
            payload.iter_mut().zip(mask.iter().cycle()).for_each(|(b, m)| *b ^= m);
        }
        Ok(Self { fin, op, payload })
    }

    fn encode(&self, out: &mut Vec<u8>) {
        out.push(u8::from(self.fin) << 7 | u8::from(self.op));
        match self.payload.len() {
            n if n < 126 => out.push(u8::try_from(n).unwrap_or(0)),
            n if u16::try_from(n).is_ok() => {
                out.push(126);
                out.extend_from_slice(&u16::try_from(n).unwrap_or(0).to_be_bytes());
            }
            n => {
                out.push(127);
                out.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        out.extend_from_slice(&self.payload);
    }

    fn close(code: u16, reason: &str) -> Self {
        Self { fin: true, op: Opcode::Close, payload: code.to_be_bytes().into_iter().chain(reason.bytes()).collect() }
    }
}

pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Close { code: u16, reason: String },
}

impl<'gc> IntoJs<'gc> for Message {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        match self {
            Self::Text(s) => s.into_js(a),
            Self::Binary(b) => Bytes(b).into_js(a),
            Self::Close { code, reason } => {
                let fields = vec![("close", u32::from(code).into_js(a)?), ("reason", reason.into_js(a)?)];
                Ok(record(a, fields).into())
            }
        }
    }
}

#[derive(Default)]
struct ServerShared {
    requests: Arc<Stream<Incoming>>,
    port: u16,
}

#[derive(Default)]
struct ConnShared {
    incoming: Arc<Stream<Message>>,
    upgradable: AtomicBool,
    closed_outgoing: AtomicBool,
    buffered: AtomicU64,
}

enum Cmd {
    Listen { id: usize, listener: TcpListener },
    Respond { conn: usize, bytes: Vec<u8> },
    Upgrade { conn: usize },
    Write { conn: usize, bytes: Vec<u8> },
    Release { conn: usize },
}

pub struct Reactor {
    inbox: Fifo<Cmd>,
    waker: mio::Waker,
    stop: AtomicBool,
    servers: SegVec<Arc<ServerShared>>,
    conns: SegVec<Weak<ConnShared>>,
}

impl Reactor {
    fn send(&self, cmd: Cmd) {
        self.inbox.push(cmd);
        let _ = self.waker.wake();
    }

    fn conn(&self, id: u32) -> Option<Arc<ConnShared>> {
        self.conns.get(id as usize)?.upgrade()
    }

    fn server(&self, id: u32) -> Option<&Arc<ServerShared>> {
        self.servers.get(id as usize)
    }
}

pub struct ReactorThread {
    reactor: Arc<Reactor>,
    thread: Option<JoinHandle<()>>,
}

impl ReactorThread {
    pub fn start() -> io::Result<Self> {
        let poll = mio::Poll::new()?;
        let waker = mio::Waker::new(poll.registry(), WAKE)?;
        let reactor = Arc::new(Reactor { inbox: Fifo::default(), waker, stop: AtomicBool::new(false), servers: SegVec::default(), conns: SegVec::default() });
        let shared = Arc::clone(&reactor);
        let thread = std::thread::Builder::new().name("workerd-reactor".into()).spawn(move || Io { poll, listeners: HashMap::new(), conns: HashMap::new() }.run(&shared))?;
        Ok(Self { reactor, thread: Some(thread) })
    }

    pub fn reactor(&self) -> Arc<Reactor> {
        Arc::clone(&self.reactor)
    }
}

impl Drop for ReactorThread {
    fn drop(&mut self) {
        self.reactor.stop.store(true, Ordering::Release);
        let _ = self.reactor.waker.wake();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

enum Phase {
    Request,
    Answering { key: Option<String> },
    Socket { partial: Option<(Opcode, Vec<u8>)> },
}

struct Conn {
    id: usize,
    server: usize,
    stream: TcpStream,
    shared: Arc<ConnShared>,
    phase: Phase,
    read: Vec<u8>,
    write: Vec<u8>,
    close_when_flushed: bool,
}

struct Io {
    poll: mio::Poll,
    listeners: HashMap<Token, (usize, TcpListener)>,
    conns: HashMap<Token, Conn>,
}

impl Io {
    fn conn_token(id: usize) -> Token {
        Token(id * 2)
    }

    fn listener_token(id: usize) -> Token {
        Token(id * 2 + 1)
    }

    fn run(mut self, reactor: &Reactor) {
        let mut events = Events::with_capacity(EVENTS);
        while !reactor.stop.load(Ordering::Acquire) {
            if let Err(e) = self.poll.poll(&mut events, None)
                && e.kind() != io::ErrorKind::Interrupted
            {
                eprintln!("workerd: epoll: {e}");
                return;
            }
            while let Some(cmd) = reactor.inbox.pop() {
                self.command(reactor, cmd);
            }
            for ev in &events {
                match ev.token() {
                    WAKE => {}
                    t if t.0 % 2 == 1 => self.accept(reactor, t),
                    t => {
                        if ev.is_writable() {
                            self.flush(t);
                        }
                        if ev.is_readable() || ev.is_read_closed() {
                            self.read(reactor, t);
                        }
                    }
                }
            }
        }
    }

    fn command(&mut self, reactor: &Reactor, cmd: Cmd) {
        match cmd {
            Cmd::Listen { id, mut listener } => {
                let t = Self::listener_token(id);
                if self.poll.registry().register(&mut listener, t, Interest::READABLE).is_ok() {
                    self.listeners.insert(t, (id, listener));
                    self.accept(reactor, t);
                }
            }
            Cmd::Respond { conn, bytes } => {
                let t = Self::conn_token(conn);
                if let Some(c) = self.conns.get_mut(&t) {
                    c.write.extend_from_slice(&bytes);
                    c.close_when_flushed = true;
                    self.flush(t);
                }
            }
            Cmd::Upgrade { conn } => {
                let t = Self::conn_token(conn);
                if let Some(c) = self.conns.get_mut(&t)
                    && let Phase::Answering { key: Some(key) } = &c.phase
                {
                    let head = format!("HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {}\r\n\r\n", Incoming::accept_key(key));
                    c.write.extend_from_slice(head.as_bytes());
                    c.phase = Phase::Socket { partial: None };
                    self.flush(t);
                    self.process(reactor, t);
                }
            }
            Cmd::Write { conn, bytes } => {
                let t = Self::conn_token(conn);
                if let Some(c) = self.conns.get_mut(&t) {
                    c.write.extend_from_slice(&bytes);
                    self.flush(t);
                }
            }
            Cmd::Release { conn } => self.drop_conn(Self::conn_token(conn)),
        }
    }

    fn accept(&mut self, reactor: &Reactor, t: Token) {
        loop {
            let Some((server, listener)) = self.listeners.get(&t) else { return };
            let server = *server;
            let (mut stream, _) = match listener.accept() {
                Ok(s) => s,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return,
                Err(_) => continue,
            };
            let _ = stream.set_nodelay(true);
            let shared = Arc::new(ConnShared::default());
            let id = reactor.conns.push(Arc::downgrade(&shared));
            let ct = Self::conn_token(id);
            if self.poll.registry().register(&mut stream, ct, Interest::READABLE | Interest::WRITABLE).is_err() {
                continue;
            }
            self.conns.insert(ct, Conn { id, server, stream, shared, phase: Phase::Request, read: Vec::new(), write: Vec::new(), close_when_flushed: false });
            self.read(reactor, ct);
        }
    }

    fn read(&mut self, reactor: &Reactor, t: Token) {
        let Some(c) = self.conns.get_mut(&t) else { return };
        let mut chunk = vec![0; READ_CHUNK];
        let mut ended = false;
        loop {
            match c.stream.read(&mut chunk) {
                Ok(0) => {
                    ended = true;
                    break;
                }
                Ok(n) => c.read.extend_from_slice(&chunk[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    ended = true;
                    break;
                }
            }
        }
        self.process(reactor, t);
        if ended {
            self.drop_conn(t);
        }
    }

    fn process(&mut self, reactor: &Reactor, t: Token) {
        let Some(c) = self.conns.get_mut(&t) else { return };
        match c.phase {
            Phase::Socket { .. } => self.frames(t),
            Phase::Answering { .. } => {}
            Phase::Request => match Incoming::parse(u32::try_from(c.id).unwrap_or(u32::MAX), &c.read) {
                Parsed::More => {}
                Parsed::Bad => self.drop_conn(t),
                Parsed::Request(request, used) => {
                    c.read.drain(..used);
                    let key = request.websocket_key();
                    c.shared.upgradable.store(key.is_some(), Ordering::Release);
                    c.phase = Phase::Answering { key };
                    if let Some(s) = reactor.servers.get(c.server) {
                        s.requests.push(request);
                    }
                }
            },
        }
    }

    fn frames(&mut self, t: Token) {
        let Some(c) = self.conns.get_mut(&t) else { return };
        let Phase::Socket { partial } = &mut c.phase else { return };
        let mut used = 0;
        loop {
            let mut cursor = Cursor::new(&c.read[used..]);
            let frame = match Frame::read(&mut cursor) {
                Ok(f) => f,
                Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => break,
                Err(_) => {
                    if !c.shared.closed_outgoing.swap(true, Ordering::AcqRel) {
                        Frame::close(CLOSE_TOO_BIG, "").encode(&mut c.write);
                    }
                    c.shared.incoming.close();
                    c.close_when_flushed = true;
                    break;
                }
            };
            used += usize::try_from(cursor.position()).unwrap_or(usize::MAX);
            let message = match frame.op {
                Opcode::Ping => {
                    Frame { fin: true, op: Opcode::Pong, payload: frame.payload }.encode(&mut c.write);
                    continue;
                }
                Opcode::Pong => continue,
                Opcode::Close => {
                    let code = frame.payload.get(..2).map_or(CLOSE_NO_STATUS, |b| u16::from_be_bytes([b[0], b[1]]));
                    let reason = String::from_utf8_lossy(frame.payload.get(2..).unwrap_or_default()).into_owned();
                    if !c.shared.closed_outgoing.swap(true, Ordering::AcqRel) {
                        Frame::close(code, "").encode(&mut c.write);
                    }
                    c.shared.incoming.push(Message::Close { code, reason });
                    c.shared.incoming.close();
                    c.close_when_flushed = true;
                    break;
                }
                Opcode::Text | Opcode::Binary if frame.fin => (frame.op, frame.payload),
                Opcode::Text | Opcode::Binary => {
                    *partial = Some((frame.op, frame.payload));
                    continue;
                }
                Opcode::Continuation => {
                    let Some((op, mut data)) = partial.take() else { continue };
                    data.extend_from_slice(&frame.payload);
                    if !frame.fin {
                        *partial = Some((op, data));
                        continue;
                    }
                    (op, data)
                }
            };
            c.shared.incoming.push(match message {
                (Opcode::Text, data) => Message::Text(String::from_utf8_lossy(&data).into_owned()),
                (_, data) => Message::Binary(data),
            });
        }
        c.read.drain(..used.min(c.read.len()));
        self.flush(t);
    }

    fn flush(&mut self, t: Token) {
        let Some(c) = self.conns.get_mut(&t) else { return };
        let mut written = 0;
        while written < c.write.len() {
            match c.stream.write(&c.write[written..]) {
                Ok(0) => break,
                Ok(n) => written += n,
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(_) => {
                    c.write.clear();
                    written = 0;
                    c.close_when_flushed = true;
                    break;
                }
            }
        }
        c.write.drain(..written);
        c.shared.buffered.store(c.write.len() as u64, Ordering::Release);
        if c.write.is_empty() && c.close_when_flushed {
            self.drop_conn(t);
        }
    }

    fn drop_conn(&mut self, t: Token) {
        if let Some(mut c) = self.conns.remove(&t) {
            let _ = self.poll.registry().deregister(&mut c.stream);
            let _ = c.stream.shutdown(std::net::Shutdown::Both);
            c.shared.incoming.close();
        }
    }
}

pub struct Net {
    reactor: Arc<Reactor>,
}

#[j8::class]
impl Net {
    fn listen(&self, a: &mut Agent<'_, '_>, port: u32) -> Result<u32, Error> {
        let port = u16::try_from(port).map_err(|_| a.throw(ErrorKind::Range, "port out of range"))?;
        let std = std::net::TcpListener::bind(("127.0.0.1", port)).map_err(|e| a.throw(ErrorKind::Error, &format!("bind 127.0.0.1:{port}: {e}")))?;
        std.set_nonblocking(true).map_err(|e| a.throw(ErrorKind::Error, &format!("listen: {e}")))?;
        let port = std.local_addr().map_or(port, |s| s.port());
        let id = self.reactor.servers.push(Arc::new(ServerShared { requests: Arc::default(), port }));
        self.reactor.send(Cmd::Listen { id, listener: TcpListener::from_std(std) });
        u32::try_from(id).map_err(|_| a.throw(ErrorKind::Range, "too many servers"))
    }

    fn port(&self, id: u32) -> u32 {
        self.reactor.server(id).map_or(0, |s| u32::from(s.port))
    }

    fn accept<'gc>(&self, a: &mut Agent<'gc, '_>, id: u32) -> Result<Value<'gc>, Error> {
        let Some(server) = self.reactor.server(id) else { return Err(a.throw(ErrorKind::Type, &format!("no server {id}"))) };
        let next = Next(Arc::clone(&server.requests));
        Glue::spawn(a, async move { Glue::settle(next.await) })
    }

    fn respond(&self, id: u32, status: u32, headers: Vec<Vec<String>>, body: &str) {
        let mut out = format!("HTTP/1.1 {status} {}\r\n", if status < 400 { "OK" } else { "Error" });
        for h in headers.iter().filter(|h| h.len() == 2 && !h[0].eq_ignore_ascii_case("content-length")) {
            let _ = write!(out, "{}: {}\r\n", h[0], h[1]);
        }
        let _ = write!(out, "content-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
        self.reactor.send(Cmd::Respond { conn: id as usize, bytes: out.into_bytes() });
    }

    fn upgrade(&self, a: &mut Agent<'_, '_>, id: u32) -> Result<(), Error> {
        if !self.reactor.conn(id).is_some_and(|c| c.upgradable.load(Ordering::Acquire)) {
            return Err(a.throw(ErrorKind::Type, "a 101 response needs a pending WebSocket upgrade request"));
        }
        self.reactor.send(Cmd::Upgrade { conn: id as usize });
        Ok(())
    }

    fn recv<'gc>(&self, a: &mut Agent<'gc, '_>, id: u32) -> Result<Value<'gc>, Error> {
        let Some(conn) = self.reactor.conn(id) else { return Err(a.throw(ErrorKind::Type, &format!("no WebSocket {id}"))) };
        let next = Next(Arc::clone(&conn.incoming));
        Glue::spawn(a, async move { Glue::settle(next.await) })
    }

    fn send_text(&self, id: u32, text: String) {
        self.frame(id, &Frame { fin: true, op: Opcode::Text, payload: text.into_bytes() });
    }

    fn send_binary(&self, id: u32, data: Bytes) {
        self.frame(id, &Frame { fin: true, op: Opcode::Binary, payload: data.0 });
    }

    fn close(&self, id: u32, code: u32, reason: String) {
        if let Some(c) = self.reactor.conn(id)
            && !c.closed_outgoing.swap(true, Ordering::AcqRel)
        {
            let mut bytes = Vec::new();
            Frame::close(u16::try_from(code).unwrap_or(CLOSE_NO_STATUS), &reason).encode(&mut bytes);
            self.reactor.send(Cmd::Write { conn: id as usize, bytes });
        }
    }

    #[expect(clippy::cast_precision_loss, reason = "a JS number; queues stay far below 2^53 bytes")]
    fn buffered_amount(&self, id: u32) -> f64 {
        self.reactor.conn(id).map_or(0.0, |c| c.buffered.load(Ordering::Acquire) as f64)
    }

    fn release(&self, id: u32) {
        self.reactor.send(Cmd::Release { conn: id as usize });
    }
}

impl Net {
    pub fn attach(a: &mut Agent<'_, '_>, reactor: Arc<Reactor>) {
        let natives = Glue::natives(a);
        let net = <Self as Class>::install(a, natives).instance(a, Self { reactor });
        natives.borrow_mut(a.mc).define("net", Property::hidden(net.into()));
    }

    fn frame(&self, id: u32, frame: &Frame) {
        if self.reactor.conn(id).is_some_and(|c| !c.closed_outgoing.load(Ordering::Acquire)) {
            let mut bytes = Vec::new();
            frame.encode(&mut bytes);
            self.reactor.send(Cmd::Write { conn: id as usize, bytes });
        }
    }
}
