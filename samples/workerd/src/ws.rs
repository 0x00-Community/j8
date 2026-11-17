//! The network side: an HTTP/1.1 listener whose connections can become WebSockets, bound to JS as
//! `__j8_native.Server`. Each connection is a [`Native`], laid out after workerd's `api::WebSocket::Native`: a state
//! machine (awaiting a response or the upgrade ⇒ accepted ⇒ released), the close flags, and an outgoing queue that a
//! pump writes to the socket.
//!
//! Nothing here blocks the runtime thread. Threads do the blocking work and hand results over through the event loop:
//!
//! - the listener thread accepts sockets, and one thread per connection reads its request, then (once upgraded) its
//!   frames;
//! - the pump thread of an accepted connection writes its outgoing queue;
//! - JS asks for each request (`accept()`) and each message (`recv(id)`) with a promise. A reader hands an item only to
//!   a waiting promise ([`Mailbox`]), so a connection is not read further than JS has asked.
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};

use j8::frame::{Agent, Error, ErrorKind, JsObject, Property, Value};
use j8::lockfree::{Fifo, Oneshot};
use j8::{Bytes, Glue, IntoJs};

/// Largest message accepted from a client (workerd's `SUGGESTED_MAX_MESSAGE_SIZE`)
const MAX_MESSAGE: usize = 1 << 20;
/// Appended to `Sec-WebSocket-Key` before hashing (RFC 6455 §1.3)
const HANDSHAKE_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
/// Close codes (RFC 6455 §7.4.1)
const CLOSE_NO_STATUS: u16 = 1005;
const CLOSE_TOO_BIG: u16 = 1009;
const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

// ---- Handing items from threads to JS promises ----

/// Items produced on a thread, each handed to the promise that asked first. A promise waits in the queue as a
/// [`Oneshot`] sender; the producing thread blocks until one is there.
struct Mailbox<T> {
    waiters: Sender<j8::lockfree::Sender<Option<T>>>,
}

/// The producing end of a [`Mailbox`], owned by the thread that makes the items
struct Feed<T> {
    waiters: Receiver<j8::lockfree::Sender<Option<T>>>,
}

impl<T: Send + 'static> Mailbox<T>
where
    Option<T>: for<'g> IntoJs<'g>,
{
    fn new() -> (Self, Feed<T>) {
        let (tx, rx) = mpsc::channel();
        (Self { waiters: tx }, Feed { waiters: rx })
    }

    /// A promise for the next item, `undefined` once the producer is gone
    fn recv<'gc>(&self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        let (tx, rx) = Oneshot::new();
        if let Err(mpsc::SendError(tx)) = self.waiters.send(tx) {
            tx.send(None);
        }
        Glue::spawn(a, async move { Glue::settle(rx.await) })
    }
}

impl<T> Feed<T> {
    /// Hands `item` to the next promise, waiting for JS to ask; `false` once nobody can ask any more
    fn give(&self, item: T) -> bool {
        match self.waiters.recv() {
            Ok(w) => {
                w.send(Some(item));
                true
            }
            Err(_) => false,
        }
    }

    /// The producer is done: every later ask gets `undefined` (until the mailbox is dropped)
    fn close(self) {
        for w in self.waiters {
            w.send(None);
        }
    }
}

// ---- HTTP ----

/// A request read off a new connection
pub struct Incoming {
    id: u32,
    method: String,
    path: String,
    headers: Vec<Vec<String>>,
    body: String,
}

impl Incoming {
    /// Reads the request line, the headers and a `Content-Length` body
    fn read(id: u32, reader: &mut BufReader<TcpStream>) -> Option<Self> {
        let mut line = String::new();
        reader.read_line(&mut line).ok()?;
        let mut parts = line.split_whitespace();
        let (method, path) = (parts.next()?.to_owned(), parts.next()?.to_owned());
        let mut headers = Vec::new();
        let mut length = 0;
        loop {
            line.clear();
            if reader.read_line(&mut line).ok()? == 0 {
                return None;
            }
            let l = line.trim_end();
            if l.is_empty() {
                break;
            }
            if let Some((k, v)) = l.split_once(':') {
                let (k, v) = (k.trim().to_owned(), v.trim().to_owned());
                if k.eq_ignore_ascii_case("content-length") {
                    length = v.parse().ok()?;
                }
                headers.push(vec![k, v]);
            }
        }
        let mut body = vec![0; length];
        reader.read_exact(&mut body).ok()?;
        Some(Self { id, method, path, headers, body: String::from_utf8_lossy(&body).into_owned() })
    }

    /// The value of header `name`
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|h| h[0].eq_ignore_ascii_case(name)).map(|h| h[1].as_str())
    }

    /// `Sec-WebSocket-Key` when this is a WebSocket upgrade request
    fn websocket_key(&self) -> Option<String> {
        let upgrade = self.header("upgrade").is_some_and(|u| u.eq_ignore_ascii_case("websocket"));
        upgrade.then(|| self.header("sec-websocket-key")).flatten().map(str::to_owned)
    }

    /// `Sec-WebSocket-Accept` for `key`: base64(SHA-1(key + GUID))
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

/// Defines `fields` as data properties on a fresh object
fn record<'gc>(a: &mut Agent<'gc, '_>, fields: Vec<(&str, Value<'gc>)>) -> JsObject<'gc> {
    let o = a.object();
    for (k, v) in fields {
        o.borrow_mut(a.mc).define(k, Property::data(v));
    }
    o
}

impl<'gc> IntoJs<'gc> for Incoming {
    /// `{ id, method, path, headers: [[name, value]…], body }`
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

// ---- WebSocket frames (RFC 6455 §5) ----

/// A frame's opcode
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

/// One frame on the wire
struct Frame {
    fin: bool,
    op: Opcode,
    payload: Vec<u8>,
}

impl Frame {
    /// Reads a frame (client frames are masked; the mask is taken off)
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

    /// Writes the frame unmasked (server to client)
    fn write(&self, w: &mut impl Write) -> io::Result<()> {
        let mut head = vec![u8::from(self.fin) << 7 | u8::from(self.op)];
        match self.payload.len() {
            n if n < 126 => head.push(u8::try_from(n).unwrap_or(0)),
            n if u16::try_from(n).is_ok() => {
                head.push(126);
                head.extend_from_slice(&u16::try_from(n).unwrap_or(0).to_be_bytes());
            }
            n => {
                head.push(127);
                head.extend_from_slice(&(n as u64).to_be_bytes());
            }
        }
        w.write_all(&head)?;
        w.write_all(&self.payload)?;
        w.flush()
    }
}

/// A complete message received from the client
pub enum Message {
    Text(String),
    Binary(Vec<u8>),
    Close { code: u16, reason: String },
}

impl<'gc> IntoJs<'gc> for Message {
    /// A string, a `Uint8Array`, or `{ close: code, reason }`
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

/// A message queued for the client (workerd's `GatedMessage`, without output gates)
enum Outgoing {
    Text(String),
    Binary(Vec<u8>),
    Pong(Vec<u8>),
    Close { code: u16, reason: String },
}

impl Outgoing {
    /// Payload bytes, counted in `bufferedAmount`
    fn len(&self) -> usize {
        match self {
            Self::Text(s) => s.len(),
            Self::Binary(b) | Self::Pong(b) => b.len(),
            Self::Close { reason, .. } => 2 + reason.len(),
        }
    }
}

impl From<Outgoing> for Frame {
    fn from(m: Outgoing) -> Self {
        let (op, payload) = match m {
            Outgoing::Text(s) => (Opcode::Text, s.into_bytes()),
            Outgoing::Binary(b) => (Opcode::Binary, b),
            Outgoing::Pong(b) => (Opcode::Pong, b),
            Outgoing::Close { code, reason } => (Opcode::Close, code.to_be_bytes().into_iter().chain(reason.into_bytes()).collect()),
        };
        Self { fin: true, op, payload }
    }
}

// ---- One connection ----

/// The flags workerd's `Native` keeps, shared with this connection's threads
#[derive(Default)]
struct Flags {
    /// A Close has been queued for the client
    closed_outgoing: AtomicBool,
    /// A Close has been received, or the connection dropped
    closed_incoming: AtomicBool,
    /// Writing to the client failed: further messages are dropped
    outgoing_aborted: AtomicBool,
    /// Payload bytes queued and not written yet
    buffered_amount: AtomicU64,
}

/// What the runtime decided for a request, sent to its connection thread
enum Verdict {
    /// Answered with a plain response: the thread is done
    Answered,
    /// Upgraded: read frames into `feed`, answer pings and closes through `outgoing`
    Upgraded { feed: Feed<Message>, outgoing: Sender<Outgoing> },
}

/// Where a connection is (workerd's `NativeState`)
enum State {
    /// A request waiting for its response or its upgrade
    AwaitingAcceptanceOrCoupling { stream: TcpStream, key: Option<String>, verdict: Sender<Verdict> },
    /// A WebSocket: messages come in through `incoming`, go out through the pump's queue
    Accepted { incoming: Mailbox<Message>, outgoing: Sender<Outgoing> },
}

/// One connection's network side
struct Native {
    state: State,
    flags: Arc<Flags>,
}

impl Native {
    /// Queues `m` for the pump (dropped once the connection failed or a Close went out)
    fn send(&self, m: Outgoing) {
        let State::Accepted { outgoing, .. } = &self.state else { return };
        let closing = matches!(m, Outgoing::Close { .. });
        if self.flags.outgoing_aborted.load(Ordering::Acquire) || (!closing && self.flags.closed_outgoing.load(Ordering::Acquire)) {
            return;
        }
        if closing && self.flags.closed_outgoing.swap(true, Ordering::AcqRel) {
            return;
        }
        self.flags.buffered_amount.fetch_add(m.len() as u64, Ordering::AcqRel);
        let _ = outgoing.send(m);
    }

    /// The pump: writes queued messages in order; stops after a Close or when the client is gone
    fn pump(mut stream: TcpStream, queue: Receiver<Outgoing>, flags: &Flags) {
        for m in queue {
            let (len, closing) = (m.len() as u64, matches!(m, Outgoing::Close { .. }));
            let written = Frame::from(m).write(&mut stream);
            flags.buffered_amount.fetch_sub(len, Ordering::AcqRel);
            if written.is_err() {
                flags.outgoing_aborted.store(true, Ordering::Release);
                break;
            }
            if closing {
                let _ = stream.shutdown(Shutdown::Write);
                break;
            }
        }
    }

    /// The connection thread after the upgrade: reads frames, joins fragments, answers pings, hands each message to
    /// JS. A Close from the client is answered (unless one was sent already) and handed on last.
    fn read_frames(mut reader: BufReader<TcpStream>, feed: Feed<Message>, outgoing: &Sender<Outgoing>, flags: &Flags) {
        let mut partial: Option<(Opcode, Vec<u8>)> = None;
        loop {
            let frame = match Frame::read(&mut reader) {
                Ok(f) => f,
                Err(e) => {
                    if e.kind() == io::ErrorKind::InvalidData && !flags.closed_outgoing.swap(true, Ordering::AcqRel) {
                        let _ = outgoing.send(Outgoing::Close { code: CLOSE_TOO_BIG, reason: String::new() });
                    }
                    break;
                }
            };
            let message = match frame.op {
                Opcode::Ping => {
                    let _ = outgoing.send(Outgoing::Pong(frame.payload));
                    continue;
                }
                Opcode::Pong => continue,
                Opcode::Close => {
                    let code = frame.payload.get(..2).map_or(CLOSE_NO_STATUS, |b| u16::from_be_bytes([b[0], b[1]]));
                    let reason = String::from_utf8_lossy(frame.payload.get(2..).unwrap_or_default()).into_owned();
                    if !flags.closed_outgoing.swap(true, Ordering::AcqRel) {
                        let _ = outgoing.send(Outgoing::Close { code, reason: String::new() });
                    }
                    flags.closed_incoming.store(true, Ordering::Release);
                    feed.give(Message::Close { code, reason });
                    break;
                }
                Opcode::Text | Opcode::Binary if frame.fin => (frame.op, frame.payload),
                Opcode::Text | Opcode::Binary => {
                    partial = Some((frame.op, frame.payload));
                    continue;
                }
                Opcode::Continuation => {
                    let Some((op, mut data)) = partial.take() else { break };
                    data.extend_from_slice(&frame.payload);
                    if data.len() > MAX_MESSAGE {
                        break;
                    }
                    if !frame.fin {
                        partial = Some((op, data));
                        continue;
                    }
                    (op, data)
                }
            };
            let message = match message {
                (Opcode::Text, data) => Message::Text(String::from_utf8_lossy(&data).into_owned()),
                (_, data) => Message::Binary(data),
            };
            if !feed.give(message) {
                break;
            }
        }
        flags.closed_incoming.store(true, Ordering::Release);
        feed.close();
    }

    /// A new connection's thread: reads the request, offers it to JS, waits for the verdict, then reads frames if
    /// the connection was upgraded
    fn serve(id: u32, stream: TcpStream, arrivals: &Fifo<(u32, Native)>, requests: &Sender<Incoming>) {
        // Small frames go out at once: no waiting for the peer's delayed ACK (Nagle)
        let _ = stream.set_nodelay(true);
        let Ok(write_half) = stream.try_clone() else { return };
        let mut reader = BufReader::new(stream);
        let Some(request) = Incoming::read(id, &mut reader) else { return };
        let (verdict, decided) = mpsc::channel();
        let state = State::AwaitingAcceptanceOrCoupling { stream: write_half, key: request.websocket_key(), verdict };
        let flags = Arc::new(Flags::default());
        // In the queue before JS can see the id: the runtime takes it out when it answers
        arrivals.push((id, Native { state, flags: Arc::clone(&flags) }));
        if requests.send(request).is_err() {
            return;
        }
        if let Ok(Verdict::Upgraded { feed, outgoing }) = decided.recv() {
            Self::read_frames(reader, feed, &outgoing, &flags);
        }
    }
}

// ---- The server bound to JS ----

/// An HTTP/1.1 listener; requests come to JS through `accept()`, each answered with `respond` or turned into a
/// WebSocket with `upgrade`
pub struct Server {
    port: u16,
    /// Connections read by their threads and not yet taken into `conns`
    arrivals: Arc<Fifo<(u32, Native)>>,
    requests: Mailbox<Incoming>,
    conns: BTreeMap<u32, Native>,
}

#[j8::class]
impl Server {
    /// Listens on `127.0.0.1:port` (`0` picks a free port)
    #[js(constructor)]
    fn listen(a: &mut Agent<'_, '_>, port: u32) -> Result<Self, Error> {
        let port = u16::try_from(port).map_err(|_| a.throw(ErrorKind::Range, "port out of range"))?;
        let listener = TcpListener::bind(("127.0.0.1", port)).map_err(|e| a.throw(ErrorKind::Error, &format!("bind 127.0.0.1:{port}: {e}")))?;
        let port = listener.local_addr().map_or(port, |s| s.port());
        let arrivals = Arc::new(Fifo::default());
        let (mailbox, feed) = Mailbox::new();
        let (requests, incoming) = mpsc::channel::<Incoming>();
        // Requests from every connection go through one thread to whichever `accept()` asked first
        std::thread::spawn(move || {
            for r in incoming {
                if !feed.give(r) {
                    return;
                }
            }
            feed.close();
        });
        let queue = Arc::clone(&arrivals);
        std::thread::spawn(move || {
            for (id, stream) in (1..).zip(listener.incoming()) {
                let Ok(stream) = stream else { continue };
                let (queue, requests) = (Arc::clone(&queue), requests.clone());
                std::thread::spawn(move || Native::serve(id, stream, &queue, &requests));
            }
        });
        Ok(Self { port, arrivals, requests: mailbox, conns: BTreeMap::new() })
    }

    #[js(getter)]
    fn port(&self) -> u32 {
        u32::from(self.port)
    }

    /// A promise for the next request (`{ id, method, path, headers, body }`)
    fn accept<'gc>(&self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        self.requests.recv(a)
    }

    /// Answers request `id` with a plain response and closes the connection
    fn respond(&mut self, a: &mut Agent<'_, '_>, id: u32, status: u32, headers: Vec<Vec<String>>, body: &str) -> Result<(), Error> {
        let Some(Native { state: State::AwaitingAcceptanceOrCoupling { mut stream, verdict, .. }, .. }) = self.take(id) else {
            return Err(a.throw(ErrorKind::Type, &format!("request {id} was already answered")));
        };
        let mut out = format!("HTTP/1.1 {status} {}\r\n", if status < 400 { "OK" } else { "Error" });
        for h in headers.iter().filter(|h| h.len() == 2 && !h[0].eq_ignore_ascii_case("content-length")) {
            let _ = write!(out, "{}: {}\r\n", h[0], h[1]);
        }
        let _ = write!(out, "content-length: {}\r\nconnection: close\r\n\r\n{body}", body.len());
        let _ = stream.write_all(out.as_bytes());
        let _ = stream.shutdown(Shutdown::Both);
        let _ = verdict.send(Verdict::Answered);
        Ok(())
    }

    /// Completes the WebSocket handshake for request `id`; its messages then come through `recv(id)`
    fn upgrade(&mut self, a: &mut Agent<'_, '_>, id: u32) -> Result<(), Error> {
        let native = self.take(id);
        let Some(Native { state: State::AwaitingAcceptanceOrCoupling { mut stream, key: Some(key), verdict }, flags }) = native else {
            if let Some(n) = native {
                self.conns.insert(id, n);
            }
            return Err(a.throw(ErrorKind::Type, "a 101 response needs a pending WebSocket upgrade request"));
        };
        let head = format!("HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: Upgrade\r\nsec-websocket-accept: {}\r\n\r\n", Incoming::accept_key(&key));
        if stream.write_all(head.as_bytes()).is_err() {
            flags.outgoing_aborted.store(true, Ordering::Release);
        }
        let (outgoing, queue) = mpsc::channel();
        let pump_flags = Arc::clone(&flags);
        std::thread::spawn(move || Native::pump(stream, queue, &pump_flags));
        let (incoming, feed) = Mailbox::new();
        let _ = verdict.send(Verdict::Upgraded { feed, outgoing: outgoing.clone() });
        self.conns.insert(id, Native { state: State::Accepted { incoming, outgoing }, flags });
        Ok(())
    }

    /// A promise for the next message of WebSocket `id`: a string, a `Uint8Array`, `{ close, reason }` for a Close,
    /// or `undefined` once the connection is gone
    fn recv<'gc>(&self, a: &mut Agent<'gc, '_>, id: u32) -> Result<Value<'gc>, Error> {
        match self.conns.get(&id) {
            Some(Native { state: State::Accepted { incoming, .. }, .. }) => incoming.recv(a),
            _ => Err(a.throw(ErrorKind::Type, &format!("no WebSocket {id}"))),
        }
    }

    fn send_text(&self, id: u32, text: String) {
        self.with(id, |n| n.send(Outgoing::Text(text)));
    }

    fn send_binary(&self, id: u32, data: Bytes) {
        self.with(id, |n| n.send(Outgoing::Binary(data.0)));
    }

    /// Queues a Close (only the first one goes out)
    fn close(&self, id: u32, code: u32, reason: String) {
        let code = u16::try_from(code).unwrap_or(CLOSE_NO_STATUS);
        self.with(id, |n| n.send(Outgoing::Close { code, reason }));
    }

    /// Payload bytes queued for WebSocket `id` and not yet written
    #[expect(clippy::cast_precision_loss, reason = "a JS number; queues stay far below 2^53 bytes")]
    fn buffered_amount(&self, id: u32) -> f64 {
        self.conns.get(&id).map_or(0.0, |n| n.flags.buffered_amount.load(Ordering::Acquire) as f64)
    }

    /// Forgets WebSocket `id` (after its close): its threads finish once their queues drain
    fn release(&mut self, id: u32) {
        self.take(id);
    }
}

impl Server {
    /// Takes connection `id` out of the table (moving new arrivals in first)
    fn take(&mut self, id: u32) -> Option<Native> {
        while let Some((i, n)) = self.arrivals.pop() {
            self.conns.insert(i, n);
        }
        self.conns.remove(&id)
    }

    fn with(&self, id: u32, f: impl FnOnce(&Native)) {
        if let Some(n) = self.conns.get(&id) {
            f(n);
        }
    }
}
