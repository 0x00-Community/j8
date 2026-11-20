//! Pieces both layers share: [`Offload`] runs blocking work on a thread for an `async fn` binding, [`Io`] is a result
//! that throws a JS error carrying a Node-style error code.
use std::future::Future;
use std::pin::Pin;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::task::{Context, Poll};

use j8::IntoJs;
use j8::frame::{Agent, Error, ErrorKind, Value};

/// Blocking work moved to a thread of its own; ready once it is done (the thread wakes the event loop)
pub struct Offload<T, F> {
    work: Option<F>,
    done: Option<Receiver<T>>,
}

// Nothing in it points into itself
impl<T, F> Unpin for Offload<T, F> {}

impl<T: Send + 'static, F: FnOnce() -> T + Send + 'static> Offload<T, F> {
    pub fn new(work: F) -> Self {
        Self { work: Some(work), done: None }
    }
}

impl<T: Send + 'static, F: FnOnce() -> T + Send + 'static> Future for Offload<T, F> {
    type Output = T;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<T> {
        let this = self.get_mut();
        if let Some(work) = this.work.take() {
            let (tx, rx) = mpsc::channel();
            let waker = cx.waker().clone();
            std::thread::spawn(move || {
                // The receiver is gone only when the runtime dropped the job
                let _ = tx.send(work());
                waker.wake();
            });
            this.done = Some(rx);
        }
        match this.done.as_ref().map(Receiver::try_recv) {
            Some(Ok(v)) => Poll::Ready(v),
            Some(Err(TryRecvError::Empty)) => Poll::Pending,
            Some(Err(TryRecvError::Disconnected)) | None => panic!("offloaded work panicked"),
        }
    }
}

/// Node's system error codes (the subset `std::io::ErrorKind` tells apart)
#[derive(Clone, Copy)]
pub enum Errno {
    Enoent,
    Eacces,
    Eexist,
    Eisdir,
    Enotdir,
    Enotempty,
    Einval,
    Econnrefused,
    Econnreset,
    Etimedout,
    Eaddrinuse,
    Epipe,
    Eio,
}

impl Errno {
    /// (code, description) as libuv words them
    const fn text(self) -> (&'static str, &'static str) {
        match self {
            Self::Enoent => ("ENOENT", "no such file or directory"),
            Self::Eacces => ("EACCES", "permission denied"),
            Self::Eexist => ("EEXIST", "file already exists"),
            Self::Eisdir => ("EISDIR", "illegal operation on a directory"),
            Self::Enotdir => ("ENOTDIR", "not a directory"),
            Self::Enotempty => ("ENOTEMPTY", "directory not empty"),
            Self::Einval => ("EINVAL", "invalid argument"),
            Self::Econnrefused => ("ECONNREFUSED", "connection refused"),
            Self::Econnreset => ("ECONNRESET", "connection reset by peer"),
            Self::Etimedout => ("ETIMEDOUT", "connection timed out"),
            Self::Eaddrinuse => ("EADDRINUSE", "address already in use"),
            Self::Epipe => ("EPIPE", "broken pipe"),
            Self::Eio => ("EIO", "i/o error"),
        }
    }
}

impl From<std::io::ErrorKind> for Errno {
    fn from(kind: std::io::ErrorKind) -> Self {
        use std::io::ErrorKind as K;
        match kind {
            K::NotFound => Self::Enoent,
            K::PermissionDenied => Self::Eacces,
            K::AlreadyExists => Self::Eexist,
            K::IsADirectory => Self::Eisdir,
            K::NotADirectory => Self::Enotdir,
            K::DirectoryNotEmpty => Self::Enotempty,
            K::InvalidInput | K::InvalidData => Self::Einval,
            K::ConnectionRefused => Self::Econnrefused,
            K::ConnectionReset | K::ConnectionAborted => Self::Econnreset,
            K::TimedOut => Self::Etimedout,
            K::AddrInUse => Self::Eaddrinuse,
            K::BrokenPipe => Self::Epipe,
            _ => Self::Eio,
        }
    }
}

/// Why a host call failed: thrown as `kind` with `message` once back on the runtime thread
pub struct Failure {
    kind: ErrorKind,
    message: String,
}

impl Failure {
    /// `ENOENT: no such file or directory, open 'a.txt'` (the JS side reads the code back off the front)
    pub fn io(e: &std::io::Error, syscall: &str, path: &str) -> Self {
        let (code, text) = Errno::from(e.kind()).text();
        Self { kind: ErrorKind::Error, message: format!("{code}: {text}, {syscall} '{path}'") }
    }
}

/// A host call's result: the value, or a [`Failure`] thrown as a JS error
pub struct Io<T>(pub Result<T, Failure>);

impl<T> Io<T> {
    /// `r`, with an I/O error described as `syscall` on `path`
    pub fn new(r: std::io::Result<T>, syscall: &str, path: &str) -> Self {
        Self(r.map_err(|e| Failure::io(&e, syscall, path)))
    }
}

impl<'gc, T: IntoJs<'gc>> IntoJs<'gc> for Io<T> {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        match self.0 {
            Ok(v) => v.into_js(a),
            Err(f) => Err(a.throw(f.kind, &f.message)),
        }
    }
}
