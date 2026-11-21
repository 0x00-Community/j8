use std::path::Path;
use std::time::SystemTime;

use j8::frame::{Agent, Error, Value};
use j8::{Bytes, IntoJs};

use crate::host::{Io, Offload};

pub struct Stat(std::fs::Metadata);

impl Stat {
    fn millis(t: std::io::Result<SystemTime>) -> f64 {
        t.ok().and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok()).map_or(0.0, |d| d.as_secs_f64() * 1e3)
    }
}

impl<'gc> IntoJs<'gc> for Stat {
    #[expect(clippy::cast_precision_loss, reason = "sizes, inodes and ids fit a double in practice")]
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        use std::os::unix::fs::MetadataExt;
        let m = &self.0;
        let t = m.file_type();
        let kind = if t.is_file() { 0.0 } else if t.is_dir() { 1.0 } else if t.is_symlink() { 2.0 } else { 3.0 };
        let fields = [kind, m.len() as f64, f64::from(m.mode()), Self::millis(m.modified()), Self::millis(m.accessed()), m.ctime() as f64 * 1e3, Self::millis(m.created()), m.nlink() as f64, f64::from(m.uid()), f64::from(m.gid()), m.ino() as f64, m.dev() as f64];
        fields.to_vec().into_js(a)
    }
}

pub struct Listing(Vec<(String, f64)>);

impl<'gc> IntoJs<'gc> for Listing {
    fn into_js(self, a: &mut Agent<'gc, '_>) -> Result<Value<'gc>, Error> {
        let (names, kinds): (Vec<String>, Vec<f64>) = self.0.into_iter().unzip();
        let pair = vec![names.into_js(a)?, kinds.into_js(a)?];
        pair.into_js(a)
    }
}

pub struct Fs;

impl Fs {
    fn read(path: &str) -> Io<Bytes> {
        Io::new(std::fs::read(path).map(Bytes), "open", path)
    }

    fn write(path: &str, data: &Bytes, append: bool) -> Io<()> {
        use std::io::Write;
        let r = std::fs::OpenOptions::new().write(true).create(true).append(append).truncate(!append).open(path).and_then(|mut f| f.write_all(&data.0));
        Io::new(r, "open", path)
    }

    fn stat(path: &str, follow: bool) -> Io<Stat> {
        let r = if follow { std::fs::metadata(path) } else { std::fs::symlink_metadata(path) };
        Io::new(r.map(Stat), if follow { "stat" } else { "lstat" }, path)
    }

    fn list(path: &str) -> Io<Listing> {
        let r = std::fs::read_dir(path).and_then(|entries| {
            let mut out = Vec::new();
            for e in entries {
                let e = e?;
                let t = e.file_type()?;
                out.push((e.file_name().to_string_lossy().into_owned(), if t.is_file() { 0.0 } else if t.is_dir() { 1.0 } else if t.is_symlink() { 2.0 } else { 3.0 }));
            }
            out.sort_by(|a, b| a.0.cmp(&b.0));
            Ok(Listing(out))
        });
        Io::new(r, "scandir", path)
    }

    fn make_dir(path: &str, recursive: bool) -> Io<()> {
        Io::new(if recursive { std::fs::create_dir_all(path) } else { std::fs::create_dir(path) }, "mkdir", path)
    }

    fn remove(path: &str, recursive: bool, force: bool) -> Io<()> {
        let p = Path::new(path);
        let r = match std::fs::symlink_metadata(p) {
            Err(e) if force && e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
            Ok(m) if m.is_dir() && recursive => std::fs::remove_dir_all(p),
            Ok(m) if m.is_dir() => Err(std::io::Error::from(std::io::ErrorKind::IsADirectory)),
            Ok(_) => std::fs::remove_file(p),
        };
        Io::new(r, "rm", path)
    }
}

#[j8::object("fs")]
impl Fs {
    fn read_sync(path: &str) -> Io<Bytes> {
        Self::read(path)
    }

    async fn read_async(path: String) -> Io<Bytes> {
        Offload::new(move || Self::read(&path)).await
    }

    fn write_sync(path: &str, data: Bytes, append: bool) -> Io<()> {
        Self::write(path, &data, append)
    }

    async fn write_async(path: String, data: Bytes, append: bool) -> Io<()> {
        Offload::new(move || Self::write(&path, &data, append)).await
    }

    fn stat_sync(path: &str, follow: bool) -> Io<Stat> {
        Self::stat(path, follow)
    }

    async fn stat_async(path: String, follow: bool) -> Io<Stat> {
        Offload::new(move || Self::stat(&path, follow)).await
    }

    fn exists(path: &str) -> bool {
        Path::new(path).exists()
    }

    fn list_sync(path: &str) -> Io<Listing> {
        Self::list(path)
    }

    async fn list_async(path: String) -> Io<Listing> {
        Offload::new(move || Self::list(&path)).await
    }

    fn mkdir_sync(path: &str, recursive: bool) -> Io<()> {
        Self::make_dir(path, recursive)
    }

    async fn mkdir_async(path: String, recursive: bool) -> Io<()> {
        Offload::new(move || Self::make_dir(&path, recursive)).await
    }

    fn rm_sync(path: &str, recursive: bool, force: bool) -> Io<()> {
        Self::remove(path, recursive, force)
    }

    async fn rm_async(path: String, recursive: bool, force: bool) -> Io<()> {
        Offload::new(move || Self::remove(&path, recursive, force)).await
    }

    fn rmdir_sync(path: &str) -> Io<()> {
        Io::new(std::fs::remove_dir(path), "rmdir", path)
    }

    fn unlink_sync(path: &str) -> Io<()> {
        Io::new(std::fs::remove_file(path), "unlink", path)
    }

    fn rename_sync(from: &str, to: &str) -> Io<()> {
        Io::new(std::fs::rename(from, to), "rename", from)
    }

    fn copy_sync(from: &str, to: &str) -> Io<()> {
        Io::new(std::fs::copy(from, to).map(drop), "copyfile", from)
    }

    fn realpath_sync(path: &str) -> Io<String> {
        Io::new(std::fs::canonicalize(path).map(|p| p.display().to_string()), "realpath", path)
    }

    fn readlink_sync(path: &str) -> Io<String> {
        Io::new(std::fs::read_link(path).map(|p| p.display().to_string()), "readlink", path)
    }

    fn symlink_sync(target: &str, path: &str) -> Io<()> {
        Io::new(std::os::unix::fs::symlink(target, path), "symlink", path)
    }

    fn chmod_sync(path: &str, mode: u32) -> Io<()> {
        use std::os::unix::fs::PermissionsExt;
        Io::new(std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode)), "chmod", path)
    }

    fn mkdtemp_sync(prefix: &str) -> Io<String> {
        let nanos = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let path = format!("{prefix}{:06x}", (nanos ^ std::process::id()) & 0xff_ffff);
        Io::new(std::fs::create_dir(&path).map(|()| path.clone()), "mkdtemp", prefix)
    }
}
