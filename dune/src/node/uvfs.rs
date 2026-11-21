use std::ffi::CString;
use std::fs::{File, Metadata};
use std::mem::ManuallyDrop;
use std::os::fd::{FromRawFd, RawFd};
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
use std::time::SystemTime;

use j8::frame::{Agent, Error, Value};
use j8::{Bytes, IntoJs, Json};
use serde::Deserialize;

use crate::host::Offload;

pub struct Uvfs;

#[derive(Deserialize)]
#[serde(untagged)]
pub enum Target {
    Fd(RawFd),
    Path(String),
}

#[derive(Deserialize)]
#[serde(tag = "op", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum Call {
    Access { path: String, mode: i32 },
    Close { fd: RawFd },
    Open { path: String, flags: i32, mode: u32 },
    Read { fd: RawFd, length: usize, position: i64 },
    ReadFile { target: Target, flags: i32 },
    Write { fd: RawFd, position: i64 },
    WriteFile { target: Target, flags: i32, mode: u32 },
    Fsync { fd: RawFd },
    Fdatasync { fd: RawFd },
    Ftruncate { fd: RawFd, length: i64 },
    Rename { from: String, to: String },
    Rmdir { path: String },
    Rm { path: String, recursive: bool },
    Mkdir { path: String, mode: u32, recursive: bool },
    Readdir { path: String },
    Stat { path: String },
    Lstat { path: String },
    Fstat { fd: RawFd },
    Statfs { path: String },
    Link { from: String, to: String },
    Symlink { target: String, path: String },
    Readlink { path: String },
    Unlink { path: String },
    Realpath { path: String },
    CopyFile { from: String, to: String, mode: u32 },
    Chmod { path: String, mode: u32 },
    Fchmod { fd: RawFd, mode: u32 },
    Chown { path: String, uid: u32, gid: u32 },
    Lchown { path: String, uid: u32, gid: u32 },
    Fchown { fd: RawFd, uid: u32, gid: u32 },
    Utimes { path: String, atime: f64, mtime: f64 },
    Lutimes { path: String, atime: f64, mtime: f64 },
    Futimes { fd: RawFd, atime: f64, mtime: f64 },
    Mkdtemp { prefix: String },
}

pub enum Reply {
    Unit,
    Number(f64),
    Text(String),
    Entries(Vec<(String, Dirent)>),
    Data(Vec<u8>),
    Fields(Vec<f64>),
}

#[derive(Clone, Copy)]
pub enum Dirent {
    Unknown,
    File,
    Dir,
    Link,
    Fifo,
    Socket,
    Char,
    Block,
}

impl From<std::fs::FileType> for Dirent {
    fn from(t: std::fs::FileType) -> Self {
        if t.is_file() {
            Self::File
        } else if t.is_dir() {
            Self::Dir
        } else if t.is_symlink() {
            Self::Link
        } else if t.is_fifo() {
            Self::Fifo
        } else if t.is_socket() {
            Self::Socket
        } else if t.is_char_device() {
            Self::Char
        } else if t.is_block_device() {
            Self::Block
        } else {
            Self::Unknown
        }
    }
}

impl From<Dirent> for f64 {
    fn from(d: Dirent) -> Self {
        match d {
            Dirent::Unknown => 0.0,
            Dirent::File => 1.0,
            Dirent::Dir => 2.0,
            Dirent::Link => 3.0,
            Dirent::Fifo => 4.0,
            Dirent::Socket => 5.0,
            Dirent::Char => 6.0,
            Dirent::Block => 7.0,
        }
    }
}

pub struct Fail {
    errno: i32,
    syscall: &'static str,
    path: Option<String>,
    dest: Option<String>,
}

pub struct Outcome(Result<Reply, Fail>);

impl<'gc> IntoJs<'gc> for Outcome {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        let parts = match self.0 {
            Ok(reply) => vec![reply.into_js(a)?],
            Err(f) => vec![f64::from(-f.errno).into_js(a)?, f.syscall.into_js(a)?, f.path.into_js(a)?, f.dest.into_js(a)?],
        };
        parts.into_js(a)
    }
}

impl<'gc> IntoJs<'gc> for Reply {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        match self {
            Self::Unit => Ok(Value::Undefined),
            Self::Number(n) => n.into_js(a),
            Self::Text(s) => s.into_js(a),
            Self::Entries(entries) => {
                let (names, kinds): (Vec<String>, Vec<f64>) = entries.into_iter().map(|(n, k)| (n, f64::from(k))).unzip();
                vec![names.into_js(a)?, kinds.into_js(a)?].into_js(a)
            }
            Self::Data(bytes) => Bytes(bytes).into_js(a),
            Self::Fields(f) => f.into_js(a),
        }
    }
}

struct Stat(Metadata);

impl Stat {
    #[expect(clippy::cast_precision_loss, reason = "sizes, inodes and times fit a double in practice")]
    fn fields(&self) -> Vec<f64> {
        let m = &self.0;
        let birth = m.created().ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or((m.ctime(), m.ctime_nsec()), |d| (d.as_secs().cast_signed(), i64::from(d.subsec_nanos())));
        vec![
            m.dev() as f64,
            f64::from(m.mode()),
            m.nlink() as f64,
            f64::from(m.uid()),
            f64::from(m.gid()),
            m.rdev() as f64,
            m.blksize() as f64,
            m.ino() as f64,
            m.size() as f64,
            m.blocks() as f64,
            m.atime() as f64,
            m.atime_nsec() as f64,
            m.mtime() as f64,
            m.mtime_nsec() as f64,
            m.ctime() as f64,
            m.ctime_nsec() as f64,
            birth.0 as f64,
            birth.1 as f64,
        ]
    }
}

struct Fd(RawFd);

impl Fd {
    fn file(&self) -> ManuallyDrop<File> {
        ManuallyDrop::new(unsafe { File::from_raw_fd(self.0) })
    }

    fn check(r: libc::c_int) -> std::io::Result<libc::c_int> {
        if r < 0 { Err(std::io::Error::last_os_error()) } else { Ok(r) }
    }

    fn checked(r: isize) -> std::io::Result<usize> {
        usize::try_from(r).map_err(|_| std::io::Error::last_os_error())
    }

    fn read(&self, length: usize, position: i64) -> std::io::Result<Vec<u8>> {
        let mut buf = vec![0u8; length];
        let n = if position >= 0 {
            Self::checked(unsafe { libc::pread(self.0, buf.as_mut_ptr().cast(), length, position) })?
        } else {
            Self::checked(unsafe { libc::read(self.0, buf.as_mut_ptr().cast(), length) })?
        };
        buf.truncate(n);
        Ok(buf)
    }

    fn write(&self, data: &[u8], position: i64) -> std::io::Result<usize> {
        if position >= 0 {
            Self::checked(unsafe { libc::pwrite(self.0, data.as_ptr().cast(), data.len(), position) })
        } else {
            Self::checked(unsafe { libc::write(self.0, data.as_ptr().cast(), data.len()) })
        }
    }

    fn read_all(&self) -> std::io::Result<Vec<u8>> {
        const CHUNK: usize = 64 * 1024;
        let mut out = Vec::new();
        loop {
            let chunk = self.read(CHUNK, -1)?;
            if chunk.is_empty() {
                return Ok(out);
            }
            out.extend_from_slice(&chunk);
        }
    }

    fn write_all(&self, mut data: &[u8]) -> std::io::Result<()> {
        while !data.is_empty() {
            let n = self.write(data, -1)?;
            data = &data[n..];
        }
        Ok(())
    }

    fn open(path: &str, flags: i32, mode: u32) -> std::io::Result<Self> {
        let c = Self::c_path(path)?;
        Self::check(unsafe { libc::open(c.as_ptr(), flags | libc::O_CLOEXEC, libc::c_uint::from(mode)) }).map(Self)
    }

    fn close(&self) -> std::io::Result<()> {
        Self::check(unsafe { libc::close(self.0) }).map(drop)
    }

    fn c_path(path: &str) -> std::io::Result<CString> {
        CString::new(path).map_err(|_| std::io::Error::from_raw_os_error(libc::EINVAL))
    }

    fn times(atime: f64, mtime: f64) -> [libc::timespec; 2] {
        let spec = |t: f64| {
            let secs = t.floor();
            #[expect(clippy::cast_possible_truncation, reason = "the timestamp is a whole number of seconds / nanoseconds")]
            libc::timespec { tv_sec: secs as libc::time_t, tv_nsec: ((t - secs) * 1e9) as libc::c_long }
        };
        [spec(atime), spec(mtime)]
    }

    fn access(path: &str, mode: i32) -> std::io::Result<()> {
        let c = Self::c_path(path)?;
        Self::check(unsafe { libc::access(c.as_ptr(), mode) }).map(drop)
    }

    fn futimes(&self, atime: f64, mtime: f64) -> std::io::Result<()> {
        Self::check(unsafe { libc::futimens(self.0, Self::times(atime, mtime).as_ptr()) }).map(drop)
    }

    fn mkdtemp(prefix: &str) -> std::io::Result<String> {
        let mut template = Self::c_path(&format!("{prefix}XXXXXX"))?.into_bytes_with_nul();
        if unsafe { libc::mkdtemp(template.as_mut_ptr().cast()) }.is_null() {
            return Err(std::io::Error::last_os_error());
        }
        template.pop();
        Ok(String::from_utf8_lossy(&template).into_owned())
    }

    #[expect(clippy::cast_precision_loss, reason = "statfs fields fit a double in practice")]
    fn statfs(path: &str) -> std::io::Result<Vec<f64>> {
        let c = Self::c_path(path)?;
        let mut s = unsafe { std::mem::zeroed::<libc::statfs>() };
        Self::check(unsafe { libc::statfs(c.as_ptr(), &raw mut s) })?;
        Ok(vec![s.f_type as f64, s.f_bsize as f64, s.f_blocks as f64, s.f_bfree as f64, s.f_bavail as f64, s.f_files as f64, s.f_ffree as f64])
    }

    fn utimes(path: &str, atime: f64, mtime: f64, follow: bool) -> std::io::Result<()> {
        let c = Self::c_path(path)?;
        let flags = if follow { 0 } else { libc::AT_SYMLINK_NOFOLLOW };
        Self::check(unsafe { libc::utimensat(libc::AT_FDCWD, c.as_ptr(), Self::times(atime, mtime).as_ptr(), flags) }).map(drop)
    }
}

impl Call {
    const fn syscall(&self) -> &'static str {
        match self {
            Self::Access { .. } => "access",
            Self::Close { .. } => "close",
            Self::Open { .. } | Self::ReadFile { .. } | Self::WriteFile { .. } => "open",
            Self::Read { .. } => "read",
            Self::Write { .. } => "write",
            Self::Fsync { .. } => "fsync",
            Self::Fdatasync { .. } => "fdatasync",
            Self::Ftruncate { .. } => "ftruncate",
            Self::Rename { .. } => "rename",
            Self::Rmdir { .. } => "rmdir",
            Self::Rm { .. } => "rm",
            Self::Mkdir { .. } => "mkdir",
            Self::Readdir { .. } => "scandir",
            Self::Stat { .. } => "stat",
            Self::Lstat { .. } => "lstat",
            Self::Fstat { .. } => "fstat",
            Self::Statfs { .. } => "statfs",
            Self::Link { .. } => "link",
            Self::Symlink { .. } => "symlink",
            Self::Readlink { .. } => "readlink",
            Self::Unlink { .. } => "unlink",
            Self::Realpath { .. } => "realpath",
            Self::CopyFile { .. } => "copyfile",
            Self::Chmod { .. } => "chmod",
            Self::Fchmod { .. } => "fchmod",
            Self::Chown { .. } => "chown",
            Self::Lchown { .. } => "lchown",
            Self::Fchown { .. } => "fchown",
            Self::Utimes { .. } => "utime",
            Self::Lutimes { .. } => "lutime",
            Self::Futimes { .. } => "futime",
            Self::Mkdtemp { .. } => "mkdtemp",
        }
    }

    fn paths(&self) -> (Option<String>, Option<String>) {
        match self {
            Self::Rename { from, to } | Self::Link { from, to } | Self::CopyFile { from, to, .. } => (Some(from.clone()), Some(to.clone())),
            Self::Symlink { target, path } => (Some(target.clone()), Some(path.clone())),
            Self::Mkdtemp { prefix } => (Some(format!("{prefix}XXXXXX")), None),
            Self::ReadFile { target: Target::Path(path), .. }
            | Self::WriteFile { target: Target::Path(path), .. }
            | Self::Access { path, .. }
            | Self::Open { path, .. }
            | Self::Rmdir { path }
            | Self::Rm { path, .. }
            | Self::Mkdir { path, .. }
            | Self::Readdir { path }
            | Self::Stat { path }
            | Self::Lstat { path }
            | Self::Statfs { path }
            | Self::Readlink { path }
            | Self::Unlink { path }
            | Self::Realpath { path }
            | Self::Chmod { path, .. }
            | Self::Chown { path, .. }
            | Self::Lchown { path, .. }
            | Self::Utimes { path, .. }
            | Self::Lutimes { path, .. } => (Some(path.clone()), None),
            _ => (None, None),
        }
    }

    fn run(self, data: Option<&[u8]>) -> Outcome {
        let syscall = self.syscall();
        let result = self.work(data.unwrap_or_default());
        Outcome(result.map_err(|e| {
            let (path, dest) = self.paths();
            Fail { errno: e.raw_os_error().unwrap_or(libc::EIO), syscall, path, dest }
        }))
    }

    fn work(&self, data: &[u8]) -> std::io::Result<Reply> {
        let unit = |r: std::io::Result<()>| r.map(|()| Reply::Unit);
        match self {
            Self::Access { path, mode } => unit(Fd::access(path, *mode)),
            Self::Close { fd } => unit(Fd(*fd).close()),
            Self::Open { path, flags, mode } => Fd::open(path, *flags, *mode).map(|fd| Reply::Number(f64::from(fd.0))),
            Self::Read { fd, length, position } => Fd(*fd).read(*length, *position).map(Reply::Data),
            Self::ReadFile { target, flags } => {
                let bytes = match target {
                    Target::Fd(fd) => Fd(*fd).read_all()?,
                    Target::Path(path) => {
                        let fd = Fd::open(path, *flags, 0)?;
                        let r = fd.read_all();
                        fd.close()?;
                        r?
                    }
                };
                Ok(Reply::Data(bytes))
            }
            Self::Write { fd, position } => Fd(*fd).write(data, *position).map(|n| Reply::Number(u32::try_from(n).map_or(f64::MAX, f64::from))),
            Self::WriteFile { target, flags, mode } => unit(match target {
                Target::Fd(fd) => Fd(*fd).write_all(data),
                Target::Path(path) => {
                    let fd = Fd::open(path, *flags, *mode)?;
                    let r = fd.write_all(data);
                    fd.close()?;
                    r
                }
            }),
            Self::Fsync { fd } => unit(Fd(*fd).file().sync_all()),
            Self::Fdatasync { fd } => unit(Fd(*fd).file().sync_data()),
            Self::Ftruncate { fd, length } => unit(Fd(*fd).file().set_len(u64::try_from(*length).unwrap_or(0))),
            Self::Rename { from, to } => unit(std::fs::rename(from, to)),
            Self::Rmdir { path } => unit(std::fs::remove_dir(path)),
            Self::Rm { path, recursive } => {
                let meta = match std::fs::symlink_metadata(path) {
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Reply::Unit),
                    m => m?,
                };
                unit(if meta.is_dir() && *recursive { std::fs::remove_dir_all(path) } else if meta.is_dir() { std::fs::remove_dir(path) } else { std::fs::remove_file(path) })
            }
            Self::Mkdir { path, mode, recursive } => Self::mkdir(path, *mode, *recursive),
            Self::Readdir { path } => {
                let mut entries = Vec::new();
                for entry in std::fs::read_dir(path)? {
                    let entry = entry?;
                    let kind = entry.file_type().map_or(Dirent::Unknown, Dirent::from);
                    entries.push((entry.file_name().to_string_lossy().into_owned(), kind));
                }
                Ok(Reply::Entries(entries))
            }
            Self::Stat { path } => std::fs::metadata(path).map(|m| Reply::Fields(Stat(m).fields())),
            Self::Lstat { path } => std::fs::symlink_metadata(path).map(|m| Reply::Fields(Stat(m).fields())),
            Self::Fstat { fd } => Fd(*fd).file().metadata().map(|m| Reply::Fields(Stat(m).fields())),
            Self::Statfs { path } => Fd::statfs(path).map(Reply::Fields),
            Self::Link { from, to } => unit(std::fs::hard_link(from, to)),
            Self::Symlink { target, path } => unit(std::os::unix::fs::symlink(target, path)),
            Self::Readlink { path } => std::fs::read_link(path).map(|p| Reply::Text(p.to_string_lossy().into_owned())),
            Self::Unlink { path } => unit(std::fs::remove_file(path)),
            Self::Realpath { path } => std::fs::canonicalize(path).map(|p| Reply::Text(p.to_string_lossy().into_owned())),
            Self::CopyFile { from, to, mode } => {
                const EXCL: u32 = 1;
                if mode & EXCL != 0 && std::fs::symlink_metadata(to).is_ok() {
                    return Err(std::io::Error::from_raw_os_error(libc::EEXIST));
                }
                unit(std::fs::copy(from, to).map(drop))
            }
            Self::Chmod { path, mode } => unit(std::fs::set_permissions(path, std::fs::Permissions::from_mode(*mode))),
            Self::Fchmod { fd, mode } => unit(Fd(*fd).file().set_permissions(std::fs::Permissions::from_mode(*mode))),
            Self::Chown { path, uid, gid } => unit(std::os::unix::fs::chown(path, Some(*uid), Some(*gid))),
            Self::Lchown { path, uid, gid } => unit(std::os::unix::fs::lchown(path, Some(*uid), Some(*gid))),
            Self::Fchown { fd, uid, gid } => unit(std::os::unix::fs::fchown(&*Fd(*fd).file(), Some(*uid), Some(*gid))),
            Self::Utimes { path, atime, mtime } => unit(Fd::utimes(path, *atime, *mtime, true)),
            Self::Lutimes { path, atime, mtime } => unit(Fd::utimes(path, *atime, *mtime, false)),
            Self::Futimes { fd, atime, mtime } => unit(Fd(*fd).futimes(*atime, *mtime)),
            Self::Mkdtemp { prefix } => Fd::mkdtemp(prefix).map(Reply::Text),
        }
    }

    fn mkdir(path: &str, mode: u32, recursive: bool) -> std::io::Result<Reply> {
        if !recursive {
            return std::fs::DirBuilder::new().mode(mode).create(path).map(|()| Reply::Unit);
        }
        let target = std::path::Path::new(path);
        if let Ok(m) = std::fs::metadata(target) {
            return if m.is_dir() { Ok(Reply::Unit) } else { Err(std::io::Error::from_raw_os_error(libc::EEXIST)) };
        }
        let first = target.ancestors().take_while(|p| !p.as_os_str().is_empty() && std::fs::symlink_metadata(p).is_err()).last().map(|p| p.to_string_lossy().into_owned());
        std::fs::DirBuilder::new().mode(mode).recursive(true).create(target).map_err(|e| if e.raw_os_error() == Some(libc::EEXIST) { std::io::Error::from_raw_os_error(libc::ENOTDIR) } else { e })?;
        Ok(first.map_or(Reply::Unit, Reply::Text))
    }

}

#[j8::object("uvfs")]
impl Uvfs {
    fn call_sync(call: Json<Call>, data: Option<Bytes>) -> Outcome {
        call.0.run(data.as_ref().map(|b| b.0.as_slice()))
    }

    async fn call(call: Json<Call>, data: Option<Bytes>) -> Outcome {
        Offload::new(move || call.0.run(data.as_ref().map(|b| b.0.as_slice()))).await
    }

    fn module_stat(path: &str) -> i32 {
        match std::fs::metadata(path) {
            Ok(m) if m.is_dir() => 1,
            Ok(_) => 0,
            Err(e) => -e.raw_os_error().unwrap_or(libc::ENOENT),
        }
    }

    fn exists(path: &str) -> bool {
        std::fs::metadata(path).is_ok()
    }
}
