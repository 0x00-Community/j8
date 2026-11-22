use std::fmt::Write;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use nodejs_semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const REPOSITORY: &str = "0x00-Community/j8";
const USER_AGENT: &str = concat!("dune/", env!("CARGO_PKG_VERSION"));
const TIMEOUT: Duration = Duration::from_secs(300);
const ARCHIVE_LIMIT: u64 = 1 << 30;
const SUMS: &str = "SHA256SUMS";
const EXE: &str = if cfg!(windows) { ".exe" } else { "" };
const OS: &str = if cfg!(target_os = "macos") { "darwin" } else if cfg!(windows) { "win32" } else { "linux" };
const CPU: &str = if cfg!(target_arch = "aarch64") { "arm64" } else { "x64" };

#[derive(clap::Args)]
#[command(about = "Replace this dune with the latest release (or the one given) from GitHub")]
pub struct Upgrade {
    #[arg(help = "Release tag to install, such as `v0.5.0` (default: the latest release)")]
    #[arg(long, value_name = "TAG")]
    tag: Option<String>,
    #[arg(help = "Install even when this dune is already that version")]
    #[arg(long)]
    force: bool,
}

#[derive(Deserialize)]
struct Release {
    tag_name: String,
}

struct Asset {
    name: String,
}

impl Default for Asset {
    fn default() -> Self {
        Self { name: format!("dune-{OS}-{CPU}") }
    }
}

impl Asset {
    fn archive(&self) -> String {
        format!("{}.tar.gz", self.name)
    }

    fn binary(&self) -> String {
        format!("{}/dune{EXE}", self.name)
    }
}

struct GitHub {
    agent: ureq::Agent,
}

impl Default for GitHub {
    fn default() -> Self {
        Self { agent: ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).build().into() }
    }
}

impl GitHub {
    fn fetch(&self, url: &str) -> Result<Vec<u8>, String> {
        let fail = |e: ureq::Error| format!("{url}: {e}");
        let mut response = self.agent.get(url).header("User-Agent", USER_AGENT).call().map_err(fail)?;
        response.body_mut().with_config().limit(ARCHIVE_LIMIT).read_to_vec().map_err(fail)
    }

    fn latest(&self) -> Result<String, String> {
        let url = format!("https://api.github.com/repos/{REPOSITORY}/releases/latest");
        let bytes = self.fetch(&url)?;
        serde_json::from_slice::<Release>(&bytes).map(|r| r.tag_name).map_err(|e| format!("{url}: {e}"))
    }

    fn download(&self, tag: &str, file: &str) -> Result<Vec<u8>, String> {
        self.fetch(&format!("https://github.com/{REPOSITORY}/releases/download/{tag}/{file}"))
    }
}

impl Upgrade {
    pub fn run(&self) -> ExitCode {
        match self.upgrade() {
            Ok(message) => {
                println!("{message}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("dune upgrade: {e}");
                ExitCode::FAILURE
            }
        }
    }

    fn version(tag: &str) -> Option<Version> {
        let bare = tag.trim_start_matches('v');
        let full = if bare.matches('.').count() == 1 { format!("{bare}.0") } else { bare.to_owned() };
        full.parse().ok()
    }

    fn upgrade(&self) -> Result<String, String> {
        let github = GitHub::default();
        let tag = match &self.tag {
            Some(tag) => tag.clone(),
            None => github.latest()?,
        };
        let current: Option<Version> = env!("CARGO_PKG_VERSION").parse().ok();
        if !self.force && current.is_some() && Self::version(&tag) == current {
            return Ok(format!("dune {} is already the release {tag}", env!("CARGO_PKG_VERSION")));
        }
        let asset = Asset::default();
        let archive = github.download(&tag, &asset.archive())?;
        let sums = github.download(&tag, SUMS)?;
        Self::verify(&archive, &sums, &asset.archive())?;
        let binary = Self::extract(&archive, &asset.binary())?;
        let exe = std::env::current_exe().and_then(|p| p.canonicalize()).map_err(|e| format!("this executable: {e}"))?;
        Self::replace(&exe, &binary).map_err(|e| format!("{}: {e}", exe.display()))?;
        Ok(format!("dune {} ⇒ {tag} ({})", env!("CARGO_PKG_VERSION"), exe.display()))
    }

    fn verify(archive: &[u8], sums: &[u8], name: &str) -> Result<(), String> {
        let digest = Sha256::digest(archive).iter().fold(String::new(), |mut hex, b| {
            let _ = write!(hex, "{b:02x}");
            hex
        });
        let listed = String::from_utf8_lossy(sums).lines().find_map(|l| {
            let (hash, file) = l.split_once(char::is_whitespace)?;
            (file.trim_start().trim_start_matches('*') == name).then(|| hash.to_owned())
        });
        match listed {
            Some(hash) if hash == digest => Ok(()),
            Some(_) => Err(format!("{name}: checksum mismatch")),
            None => Err(format!("{name}: not listed in {SUMS}")),
        }
    }

    fn extract(archive: &[u8], path: &str) -> Result<Vec<u8>, String> {
        let mut tar = tar::Archive::new(flate2::read::GzDecoder::new(archive));
        let fail = |e: std::io::Error| format!("archive: {e}");
        for entry in tar.entries().map_err(fail)? {
            let mut entry = entry.map_err(fail)?;
            if entry.path().map_err(fail)?.to_str() == Some(path) {
                let mut bytes = Vec::new();
                entry.read_to_end(&mut bytes).map_err(fail)?;
                return Ok(bytes);
            }
        }
        Err(format!("archive: no {path}"))
    }

    fn sibling(exe: &Path, suffix: &str) -> PathBuf {
        let mut name = exe.file_name().unwrap_or_default().to_os_string();
        name.push(suffix);
        exe.with_file_name(name)
    }

    fn replace(exe: &Path, binary: &[u8]) -> std::io::Result<()> {
        let fresh = Self::sibling(exe, ".new");
        let old = Self::sibling(exe, ".old");
        std::fs::write(&fresh, binary)?;
        Self::executable(&fresh)?;
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old)?;
        if let Err(e) = std::fs::rename(&fresh, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(e);
        }
        let _ = std::fs::remove_file(&old);
        Self::relink(exe)
    }

    #[cfg(unix)]
    fn executable(file: &Path) -> std::io::Result<()> {
        use std::os::unix::fs::PermissionsExt;
        const EXECUTABLE: u32 = 0o755;
        std::fs::set_permissions(file, std::fs::Permissions::from_mode(EXECUTABLE))
    }

    #[cfg(not(unix))]
    fn executable(_: &Path) -> std::io::Result<()> {
        Ok(())
    }

    fn relink(exe: &Path) -> std::io::Result<()> {
        let dunx = exe.with_file_name(format!("dunx{EXE}"));
        match std::fs::symlink_metadata(&dunx) {
            Ok(meta) if meta.is_file() => {
                let old = Self::sibling(&dunx, ".old");
                let _ = std::fs::remove_file(&old);
                std::fs::rename(&dunx, &old)?;
                std::fs::hard_link(exe, &dunx).or_else(|_| std::fs::copy(exe, &dunx).map(drop))?;
                let _ = std::fs::remove_file(&old);
                Ok(())
            }
            _ => Ok(()),
        }
    }
}
