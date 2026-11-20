use std::collections::BTreeMap;

use base64::Engine;
use nodejs_semver::Version;
use serde::Deserialize;
use sha2::Digest;

use super::spec::{Dep, Spec};

/// One version of a package: from the registry, or a package.json on disk
#[derive(Deserialize)]
pub struct Manifest {
    pub version: Option<Version>,
    #[serde(default)]
    dependencies: BTreeMap<String, String>,
    #[serde(default, rename = "optionalDependencies")]
    optional: BTreeMap<String, String>,
    #[serde(default, rename = "peerDependencies")]
    peers: BTreeMap<String, String>,
    #[serde(default, rename = "peerDependenciesMeta")]
    peer_meta: BTreeMap<String, PeerMeta>,
    #[serde(default)]
    pub bin: Bin,
    #[serde(default)]
    os: Allows<Os>,
    #[serde(default)]
    cpu: Allows<Cpu>,
    pub dist: Option<Dist>,
}

#[derive(Deserialize, Default)]
struct PeerMeta {
    #[serde(default)]
    optional: bool,
}

/// Whether a dependency must be there
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Need {
    Required,
    /// `optionalDependencies` (and optional peers): left out when they can't be installed
    Optional,
}

impl Manifest {
    /// Whether this package runs here, by its `os` and `cpu` fields
    pub fn fits(&self) -> bool {
        self.os.admit(Os::HOST) && self.cpu.admit(Cpu::HOST)
    }

    /// What to install with it: dependencies, optional dependencies, and peers (as bun and npm 7+ do); an optional
    /// dependency listed in `dependencies` too is optional
    pub fn deps(&self) -> Vec<(String, Dep, Need)> {
        let peer_need = |name: &str| if self.peer_meta.get(name).is_some_and(|m| m.optional) { Need::Optional } else { Need::Required };
        let mut all: BTreeMap<&str, (&str, Need)> = BTreeMap::new();
        all.extend(self.peers.iter().map(|(n, v)| (n.as_str(), (v.as_str(), peer_need(n)))));
        all.extend(self.dependencies.iter().map(|(n, v)| (n.as_str(), (v.as_str(), Need::Required))));
        all.extend(self.optional.iter().map(|(n, v)| (n.as_str(), (v.as_str(), Need::Optional))));
        all.into_iter().map(|(n, (v, need))| (n.to_owned(), Dep::new(n, v), need)).collect()
    }
}

/// package.json `bin`
#[derive(Deserialize, Default)]
#[serde(untagged)]
pub enum Bin {
    #[default]
    None,
    /// One executable, named after the package
    One(String),
    Many(BTreeMap<String, String>),
}

impl Bin {
    /// The file (relative to the package) of executable `command`; with no command, the package's only executable or
    /// the one named after it
    pub fn entry(&self, package: &str, command: Option<&str>) -> Result<&str, String> {
        let own = Spec::unscoped(package);
        let found = match (self, command) {
            (Self::One(file), None) => Some(file),
            (Self::One(file), Some(c)) => (c == own).then_some(file),
            (Self::Many(map), None) if map.len() == 1 => map.values().next(),
            (Self::Many(map), c) => map.get(c.unwrap_or(own)),
            (Self::None, _) => None,
        };
        found.map(String::as_str).ok_or_else(|| match self {
            Self::Many(map) => format!("{package} has no executable {:?}; it has: {}", command.unwrap_or(own), map.keys().cloned().collect::<Vec<_>>().join(", ")),
            _ => format!("{package} has no executable {:?}", command.unwrap_or(own)),
        })
    }
}

/// `dist` of a registry version
#[derive(Deserialize)]
pub struct Dist {
    pub tarball: String,
    pub integrity: Option<Integrity>,
}

/// Subresource integrity: the strongest digest of the ones listed
#[derive(Deserialize)]
#[serde(try_from = "String")]
pub struct Integrity {
    algo: Algo,
    digest: Vec<u8>,
}

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Algo {
    Sha256,
    Sha384,
    Sha512,
}

impl Algo {
    const TABLE: [(Self, &str); 3] = [(Self::Sha256, "sha256-"), (Self::Sha384, "sha384-"), (Self::Sha512, "sha512-")];

    fn digest(self, bytes: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha256 => sha2::Sha256::digest(bytes).to_vec(),
            Self::Sha384 => sha2::Sha384::digest(bytes).to_vec(),
            Self::Sha512 => sha2::Sha512::digest(bytes).to_vec(),
        }
    }
}

impl TryFrom<String> for Integrity {
    type Error = String;

    fn try_from(text: String) -> Result<Self, String> {
        let parsed = text.split_whitespace().filter_map(|part| {
            let (algo, prefix) = Algo::TABLE.into_iter().find(|(_, p)| part.starts_with(p))?;
            let digest = base64::engine::general_purpose::STANDARD.decode(part[prefix.len()..].split('?').next()?).ok()?;
            Some(Self { algo, digest })
        });
        parsed.max_by_key(|i| i.algo).ok_or_else(|| format!("unsupported integrity {text}"))
    }
}

impl Integrity {
    pub fn check(&self, bytes: &[u8]) -> bool {
        self.algo.digest(bytes) == self.digest
    }
}

/// `os` / `cpu`: names allowed, and `!name` refused
#[derive(Deserialize)]
#[serde(from = "Names", bound = "T: for<'a> From<&'a str>")]
struct Allows<T>(Vec<(bool, T)>);

impl<T> Default for Allows<T> {
    fn default() -> Self {
        Self(Vec::new())
    }
}

/// A list of names, or one
#[derive(Deserialize)]
#[serde(untagged)]
enum Names {
    One(String),
    Many(Vec<String>),
}

impl<T: for<'a> From<&'a str>> From<Names> for Allows<T> {
    fn from(names: Names) -> Self {
        let names = match names {
            Names::One(n) => vec![n],
            Names::Many(n) => n,
        };
        Self(names.iter().map(|n| n.strip_prefix('!').map_or((true, T::from(n.as_str())), |n| (false, T::from(n)))).collect())
    }
}

impl<T: PartialEq> Allows<T> {
    /// Refused when listed with `!`, or when names are listed without `!` and this one isn't
    fn admit(&self, host: T) -> bool {
        let mut allowed = self.0.iter().filter(|(yes, _)| *yes).peekable();
        let listed = allowed.peek().is_none() || allowed.any(|(_, t)| *t == host);
        listed && !self.0.iter().any(|(yes, t)| !yes && *t == host)
    }
}

/// Node's `process.platform`
#[derive(Clone, Copy, PartialEq, Eq)]
enum Os {
    Aix,
    Android,
    Darwin,
    Freebsd,
    Linux,
    Openbsd,
    Sunos,
    Win32,
    Other,
}

impl Os {
    const TABLE: [(Self, &str); 8] = [(Self::Aix, "aix"), (Self::Android, "android"), (Self::Darwin, "darwin"), (Self::Freebsd, "freebsd"), (Self::Linux, "linux"), (Self::Openbsd, "openbsd"), (Self::Sunos, "sunos"), (Self::Win32, "win32")];

    const HOST: Self = if cfg!(target_os = "android") {
        Self::Android
    } else if cfg!(target_os = "linux") {
        Self::Linux
    } else if cfg!(target_os = "macos") {
        Self::Darwin
    } else if cfg!(target_os = "windows") {
        Self::Win32
    } else if cfg!(target_os = "freebsd") {
        Self::Freebsd
    } else if cfg!(target_os = "openbsd") {
        Self::Openbsd
    } else {
        Self::Other
    };
}

impl From<&str> for Os {
    fn from(name: &str) -> Self {
        Self::TABLE.into_iter().find(|(_, n)| *n == name).map_or(Self::Other, |(os, _)| os)
    }
}

/// Node's `process.arch`
#[derive(Clone, Copy, PartialEq, Eq)]
enum Cpu {
    X64,
    Arm64,
    Ia32,
    Arm,
    Ppc64,
    S390x,
    Riscv64,
    Loong64,
    Other,
}

impl Cpu {
    const TABLE: [(Self, &str); 8] = [(Self::X64, "x64"), (Self::Arm64, "arm64"), (Self::Ia32, "ia32"), (Self::Arm, "arm"), (Self::Ppc64, "ppc64"), (Self::S390x, "s390x"), (Self::Riscv64, "riscv64"), (Self::Loong64, "loong64")];

    const HOST: Self = if cfg!(target_arch = "x86_64") {
        Self::X64
    } else if cfg!(target_arch = "aarch64") {
        Self::Arm64
    } else if cfg!(target_arch = "x86") {
        Self::Ia32
    } else if cfg!(target_arch = "arm") {
        Self::Arm
    } else if cfg!(target_arch = "powerpc64") {
        Self::Ppc64
    } else if cfg!(target_arch = "s390x") {
        Self::S390x
    } else if cfg!(target_arch = "riscv64") {
        Self::Riscv64
    } else if cfg!(target_arch = "loongarch64") {
        Self::Loong64
    } else {
        Self::Other
    };
}

impl From<&str> for Cpu {
    fn from(name: &str) -> Self {
        Self::TABLE.into_iter().find(|(_, n)| *n == name).map_or(Self::Other, |(cpu, _)| cpu)
    }
}
