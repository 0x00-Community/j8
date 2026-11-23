use std::future::Future;
use std::pin::Pin;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::task::{Context, Poll};

use j8::IntoJs;
use j8::frame::{Agent, Error, ErrorKind, Value};

pub struct Offload<T, F> {
    work: Option<F>,
    done: Option<Receiver<T>>,
}

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

pub struct Failure {
    kind: ErrorKind,
    message: String,
}

impl Failure {
    pub fn io(e: &std::io::Error, syscall: &str, path: &str) -> Self {
        let (code, text) = Errno::from(e.kind()).text();
        Self { kind: ErrorKind::Error, message: format!("{code}: {text}, {syscall} '{path}'") }
    }
}

pub struct Io<T>(pub Result<T, Failure>);

impl<T> Io<T> {
    pub fn new(r: std::io::Result<T>, syscall: &str, path: &str) -> Self {
        Self(r.map_err(|e| Failure::io(&e, syscall, path)))
    }
}

impl<'scope, T: IntoJs<'scope>> IntoJs<'scope> for Io<T> {
    fn into_js(self, a: &mut Agent<'scope, '_>) -> Result<Value<'scope>, Error> {
        match self.0 {
            Ok(v) => v.into_js(a),
            Err(f) => Err(a.throw(f.kind, &f.message)),
        }
    }
}
