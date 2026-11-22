use std::fs::{File, Metadata, OpenOptions};
use std::io::{Read, Write};
use std::mem::ManuallyDrop;
use std::path::Path;
use std::time::SystemTime;

use faccess::{AccessMode, PathExt};
use filetime::FileTime;
use j8::frame::{Agent, Error, Value};
use j8::{Bytes, IntoJs, Json};
use positioned_io::{ReadAt, WriteAt};
use serde::Deserialize;

use crate::host::Offload;

pub type RawFd = i32;

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
    Access {
        path: String,
        #[serde(default)]
        mode: i32,
    },
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
        } else {
            Self::special(t)
        }
    }
}

impl Dirent {
    #[cfg(unix)]
    fn special(t: std::fs::FileType) -> Self {
        use std::os::unix::fs::FileTypeExt;
        if t.is_fifo() {
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

    #[cfg(not(unix))]
    fn special(_: std::fs::FileType) -> Self {
        Self::Unknown
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

#[derive(Clone, Copy)]
#[repr(i32)]
pub enum Errno {
    Eperm = 1,
    Enoent = 2,
    Eintr = 4,
    Eio = 5,
    Ebadf = 9,
    Eagain = 11,
    Enomem = 12,
    Eacces = 13,
    Ebusy = 16,
    Eexist = 17,
    Exdev = 18,
    Enotdir = 20,
    Eisdir = 21,
    Einval = 22,
    Etxtbsy = 26,
    Efbig = 27,
    Enospc = 28,
    Espipe = 29,
    Erofs = 30,
    Emlink = 31,
    Epipe = 32,
    Enametoolong = 36,
    Enosys = 38,
    Enotempty = 39,
}

impl From<&std::io::Error> for Errno {
    fn from(e: &std::io::Error) -> Self {
        use std::io::ErrorKind as K;
        match e.kind() {
            K::NotFound => Self::Enoent,
            K::PermissionDenied => Self::Eacces,
            K::AlreadyExists => Self::Eexist,
            K::DirectoryNotEmpty => Self::Enotempty,
            K::NotADirectory => Self::Enotdir,
            K::IsADirectory => Self::Eisdir,
            K::ReadOnlyFilesystem => Self::Erofs,
            K::StorageFull => Self::Enospc,
            K::InvalidInput => Self::Einval,
            K::InvalidFilename => Self::Enametoolong,
            K::CrossesDevices => Self::Exdev,
            K::TooManyLinks => Self::Emlink,
            K::ResourceBusy => Self::Ebusy,
            K::ExecutableFileBusy => Self::Etxtbsy,
            K::FileTooLarge => Self::Efbig,
            K::NotSeekable => Self::Espipe,
            K::Unsupported => Self::Enosys,
            K::Interrupted => Self::Eintr,
            K::WouldBlock => Self::Eagain,
            K::BrokenPipe => Self::Epipe,
            K::OutOfMemory => Self::Enomem,
            _ => Self::Eio,
        }
    }
}

impl Errno {
    fn code(e: &std::io::Error) -> i32 {
        match (Self::from(e), e.raw_os_error()) {
            (Self::Eio, Some(raw)) if cfg!(target_os = "linux") => raw,
            (known, _) => known as i32,
        }
    }
}

impl From<Errno> for std::io::Error {
    fn from(e: Errno) -> Self {
        use std::io::ErrorKind as K;
        Self::from(match e {
            Errno::Enoent => K::NotFound,
            Errno::Eexist => K::AlreadyExists,
            Errno::Enotdir => K::NotADirectory,
            Errno::Einval => K::InvalidInput,
            _ => K::Other,
        })
    }
}

struct Stat(Metadata);

impl Stat {
    #[cfg(unix)]
    #[expect(clippy::cast_precision_loss, reason = "sizes, inodes and times fit a double in practice")]
    fn fields(&self) -> Vec<f64> {
        use std::os::unix::fs::MetadataExt;
        let m = &self.0;
        let birth = Self::time(m.created()).unwrap_or((m.ctime() as f64, m.ctime_nsec() as f64));
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
            birth.0,
            birth.1,
        ]
    }

    #[cfg(not(unix))]
    #[expect(clippy::cast_precision_loss, reason = "sizes and times fit a double in practice")]
    fn fields(&self) -> Vec<f64> {
        const S_IFREG: u32 = 0o100_000;
        const S_IFDIR: u32 = 0o040_000;
        const S_IFLNK: u32 = 0o120_000;
        const READ_WRITE: u32 = 0o666;
        const READ_ONLY: u32 = 0o444;
        const SEARCH: u32 = 0o111;
        const BLOCK_SIZE: u64 = 4096;
        const SECTOR: u64 = 512;
        let m = &self.0;
        let t = m.file_type();
        let kind = if t.is_symlink() { S_IFLNK } else if t.is_dir() { S_IFDIR | SEARCH } else { S_IFREG };
        let mode = kind | if m.permissions().readonly() { READ_ONLY } else { READ_WRITE };
        let zero = (0.0, 0.0);
        let mtime = Self::time(m.modified()).unwrap_or(zero);
        let atime = Self::time(m.accessed()).unwrap_or(mtime);
        let birth = Self::time(m.created()).unwrap_or(mtime);
        vec![0.0, f64::from(mode), 1.0, 0.0, 0.0, 0.0, BLOCK_SIZE as f64, 0.0, m.len() as f64, m.len().div_ceil(SECTOR) as f64, atime.0, atime.1, mtime.0, mtime.1, mtime.0, mtime.1, birth.0, birth.1]
    }

    #[expect(clippy::cast_precision_loss, reason = "seconds since the epoch fit a double")]
    fn time(t: std::io::Result<SystemTime>) -> Option<(f64, f64)> {
        let d = t.ok()?.duration_since(SystemTime::UNIX_EPOCH).ok()?;
        Some((d.as_secs() as f64, f64::from(d.subsec_nanos())))
    }
}

struct OpenFlags(i32);

impl OpenFlags {
    const ACCESS: i32 = 0b11;
    const WRITE_ONLY: i32 = 1;
    const READ_WRITE: i32 = 2;
    const CREATE: i32 = 0o100;
    const EXCLUSIVE: i32 = 0o200;
    const TRUNCATE: i32 = 0o1000;
    const APPEND: i32 = 0o2000;

    fn has(&self, flag: i32) -> bool {
        self.0 & flag != 0
    }

    fn options(&self, mode: u32) -> OpenOptions {
        let access = self.0 & Self::ACCESS;
        let mut o = OpenOptions::new();
        o.read(access != Self::WRITE_ONLY).write(access == Self::WRITE_ONLY || access == Self::READ_WRITE || self.has(Self::APPEND));
        o.append(self.has(Self::APPEND)).truncate(self.has(Self::TRUNCATE));
        match (self.has(Self::CREATE), self.has(Self::EXCLUSIVE)) {
            (true, true) => o.create_new(true),
            (true, false) => o.create(true),
            _ => &mut o,
        };
        Platform::mode(&mut o, mode);
        o
    }
}

pub struct Platform;

#[cfg(unix)]
impl Platform {
    fn mode(o: &mut OpenOptions, mode: u32) {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(mode);
    }

    fn dir_mode(b: &mut std::fs::DirBuilder, mode: u32) {
        use std::os::unix::fs::DirBuilderExt;
        b.mode(mode);
    }

    pub fn permissions(_: std::fs::Permissions, mode: u32) -> std::fs::Permissions {
        use std::os::unix::fs::PermissionsExt;
        std::fs::Permissions::from_mode(mode)
    }

    fn chown(path: &Path, uid: u32, gid: u32, follow: bool) -> std::io::Result<()> {
        if follow { std::os::unix::fs::chown(path, Some(uid), Some(gid)) } else { std::os::unix::fs::lchown(path, Some(uid), Some(gid)) }
    }

    fn fchown(file: &File, uid: u32, gid: u32) -> std::io::Result<()> {
        std::os::unix::fs::fchown(file, Some(uid), Some(gid))
    }

    fn borrow(fd: RawFd) -> ManuallyDrop<File> {
        use std::os::fd::FromRawFd;
        ManuallyDrop::new(unsafe { File::from_raw_fd(fd) })
    }

    #[expect(clippy::unnecessary_wraps, reason = "the same signature as the Windows implementation")]
    fn adopt(file: File) -> std::io::Result<RawFd> {
        use std::os::fd::IntoRawFd;
        Ok(file.into_raw_fd())
    }

    #[expect(clippy::unnecessary_wraps, reason = "the same signature as the Windows implementation")]
    fn close(fd: RawFd) -> std::io::Result<()> {
        drop(ManuallyDrop::into_inner(Self::borrow(fd)));
        Ok(())
    }
}

#[cfg(windows)]
impl Platform {
    const WRITABLE: u32 = 0o222;

    fn mode(_: &mut OpenOptions, _: u32) {}

    fn dir_mode(_: &mut std::fs::DirBuilder, _: u32) {}

    pub fn permissions(mut p: std::fs::Permissions, mode: u32) -> std::fs::Permissions {
        p.set_readonly(mode & Self::WRITABLE == 0);
        p
    }

    fn chown(_: &Path, _: u32, _: u32, _: bool) -> std::io::Result<()> {
        Ok(())
    }

    fn fchown(_: &File, _: u32, _: u32) -> std::io::Result<()> {
        Ok(())
    }

    fn borrow(fd: RawFd) -> ManuallyDrop<File> {
        use std::os::windows::io::{FromRawHandle, RawHandle};
        ManuallyDrop::new(unsafe { File::from_raw_handle(libc::get_osfhandle(fd) as RawHandle) })
    }

    fn adopt(file: File) -> std::io::Result<RawFd> {
        use std::os::windows::io::IntoRawHandle;
        let fd = unsafe { libc::open_osfhandle(file.into_raw_handle() as libc::intptr_t, 0) };
        if fd < 0 { Err(std::io::Error::last_os_error()) } else { Ok(fd) }
    }

    fn close(fd: RawFd) -> std::io::Result<()> {
        if unsafe { libc::close(fd) } < 0 { Err(std::io::Error::last_os_error()) } else { Ok(()) }
    }
}

pub struct Fd(pub RawFd);

impl Fd {
    pub fn file(&self) -> ManuallyDrop<File> {
        Platform::borrow(self.0)
    }

    fn read(&self, length: usize, position: i64) -> std::io::Result<Vec<u8>> {
        let mut buf = vec![0u8; length];
        let file = self.file();
        let n = match u64::try_from(position) {
            Ok(at) => file.read_at(at, &mut buf)?,
            Err(_) => (&*file).read(&mut buf)?,
        };
        buf.truncate(n);
        Ok(buf)
    }

    fn write(&self, data: &[u8], position: i64) -> std::io::Result<usize> {
        let mut file = self.file();
        match u64::try_from(position) {
            Ok(at) => file.write_at(at, data),
            Err(_) => (&*file).write(data),
        }
    }

    fn read_all(&self) -> std::io::Result<Vec<u8>> {
        let mut out = Vec::new();
        (&*self.file()).read_to_end(&mut out)?;
        Ok(out)
    }

    fn write_all(&self, data: &[u8]) -> std::io::Result<()> {
        (&*self.file()).write_all(data)
    }

    fn open(path: &str, flags: i32, mode: u32) -> std::io::Result<Self> {
        let file = OpenFlags(flags).options(mode).open(path)?;
        Platform::adopt(file).map(Self)
    }

    fn close(&self) -> std::io::Result<()> {
        Platform::close(self.0)
    }

    fn times(atime: f64, mtime: f64) -> (FileTime, FileTime) {
        let at = |t: f64| {
            let secs = t.floor();
            #[expect(clippy::cast_possible_truncation, clippy::cast_sign_loss, reason = "the timestamp is a whole number of seconds / nanoseconds")]
            FileTime::from_unix_time(secs as i64, ((t - secs) * 1e9) as u32)
        };
        (at(atime), at(mtime))
    }

    fn access(path: &str, mode: i32) -> std::io::Result<()> {
        const R_OK: i32 = 4;
        const W_OK: i32 = 2;
        const X_OK: i32 = 1;
        let mut m = AccessMode::EXISTS;
        for (bit, flag) in [(R_OK, AccessMode::READ), (W_OK, AccessMode::WRITE), (X_OK, AccessMode::EXECUTE)] {
            if mode & bit != 0 {
                m |= flag;
            }
        }
        Path::new(path).access(m)
    }

    fn futimes(&self, atime: f64, mtime: f64) -> std::io::Result<()> {
        let (a, m) = Self::times(atime, mtime);
        filetime::set_file_handle_times(&self.file(), Some(a), Some(m))
    }

    fn mkdtemp(prefix: &str) -> std::io::Result<String> {
        const RANDOM: usize = 6;
        let p = Path::new(prefix);
        let (dir, name) = if prefix.ends_with(std::path::is_separator) { (p, "") } else { (p.parent().unwrap_or(Path::new("")), p.file_name().and_then(|n| n.to_str()).unwrap_or_default()) };
        let here = dir.as_os_str().is_empty();
        let made = tempfile::Builder::new().prefix(name).rand_bytes(RANDOM).tempdir_in(if here { Path::new(".") } else { dir })?.keep();
        let suffix = made.file_name().and_then(|n| n.to_str()).and_then(|n| n.strip_prefix(name)).unwrap_or_default();
        Ok(format!("{prefix}{suffix}"))
    }

    #[expect(clippy::cast_precision_loss, reason = "statfs fields fit a double in practice")]
    fn statfs(path: &str) -> std::io::Result<Vec<f64>> {
        let s = fs4::statvfs(path)?;
        let block = s.allocation_granularity().max(1);
        Ok(vec![0.0, block as f64, (s.total_space() / block) as f64, (s.free_space() / block) as f64, (s.available_space() / block) as f64, 0.0, 0.0])
    }

    fn utimes(path: &str, atime: f64, mtime: f64, follow: bool) -> std::io::Result<()> {
        let (a, m) = Self::times(atime, mtime);
        if follow { filetime::set_file_times(path, a, m) } else { filetime::set_symlink_file_times(path, a, m) }
    }

    pub fn symlink(target: &str, path: &str) -> std::io::Result<()> {
        let link = Path::new(path);
        let resolved = link.parent().unwrap_or(Path::new(".")).join(target);
        if resolved.is_dir() { symlink::symlink_dir(target, link) } else { symlink::symlink_file(target, link) }
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
            Fail { errno: Errno::code(&e), syscall, path, dest }
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
            Self::Symlink { target, path } => unit(Fd::symlink(target, path)),
            Self::Readlink { path } => std::fs::read_link(path).map(|p| Reply::Text(p.to_string_lossy().into_owned())),
            Self::Unlink { path } => unit(std::fs::remove_file(path)),
            Self::Realpath { path } => std::fs::canonicalize(path).map(|p| Reply::Text(p.to_string_lossy().into_owned())),
            Self::CopyFile { from, to, mode } => {
                const EXCL: u32 = 1;
                if mode & EXCL != 0 && std::fs::symlink_metadata(to).is_ok() {
                    return Err(Errno::Eexist.into());
                }
                unit(std::fs::copy(from, to).map(drop))
            }
            Self::Chmod { path, mode } => unit(std::fs::metadata(path).and_then(|m| std::fs::set_permissions(path, Platform::permissions(m.permissions(), *mode)))),
            Self::Fchmod { fd, mode } => {
                let file = Fd(*fd).file();
                unit(file.metadata().and_then(|m| file.set_permissions(Platform::permissions(m.permissions(), *mode))))
            }
            Self::Chown { path, uid, gid } => unit(Platform::chown(Path::new(path), *uid, *gid, true)),
            Self::Lchown { path, uid, gid } => unit(Platform::chown(Path::new(path), *uid, *gid, false)),
            Self::Fchown { fd, uid, gid } => unit(Platform::fchown(&Fd(*fd).file(), *uid, *gid)),
            Self::Utimes { path, atime, mtime } => unit(Fd::utimes(path, *atime, *mtime, true)),
            Self::Lutimes { path, atime, mtime } => unit(Fd::utimes(path, *atime, *mtime, false)),
            Self::Futimes { fd, atime, mtime } => unit(Fd(*fd).futimes(*atime, *mtime)),
            Self::Mkdtemp { prefix } => Fd::mkdtemp(prefix).map(Reply::Text),
        }
    }

    fn mkdir(path: &str, mode: u32, recursive: bool) -> std::io::Result<Reply> {
        if !recursive {
            let mut b = std::fs::DirBuilder::new();
            Platform::dir_mode(&mut b, mode);
            return b.create(path).map(|()| Reply::Unit);
        }
        let target = std::path::Path::new(path);
        if let Ok(m) = std::fs::metadata(target) {
            return if m.is_dir() { Ok(Reply::Unit) } else { Err(Errno::Eexist.into()) };
        }
        let first = target.ancestors().take_while(|p| !p.as_os_str().is_empty() && std::fs::symlink_metadata(p).is_err()).last().map(|p| p.to_string_lossy().into_owned());
        let mut b = std::fs::DirBuilder::new();
        Platform::dir_mode(&mut b, mode);
        b.recursive(true).create(target).map_err(|e| if e.kind() == std::io::ErrorKind::AlreadyExists { Errno::Enotdir.into() } else { e })?;
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
            Err(e) => -Errno::code(&e),
        }
    }

    fn exists(path: &str) -> bool {
        std::fs::metadata(path).is_ok()
    }
}
