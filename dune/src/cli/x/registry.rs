use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::time::Duration;

use nodejs_semver::Version;
use serde::Deserialize;

use super::manifest::{Dist, Manifest};
use super::spec::{LATEST, Want};

const REGISTRY_VARS: [&str; 2] = ["DUNE_REGISTRY", "npm_config_registry"];
const DEFAULT_REGISTRY: &str = "https://registry.npmjs.org";

const ABBREVIATED: &str = "application/vnd.npm.install-v1+json; q=1.0, application/json; q=0.8";

const METADATA_LIMIT: u64 = 256 << 20;

const TARBALL_LIMIT: u64 = 1 << 30;
const TIMEOUT: Duration = Duration::from_secs(120);

pub struct Registry {
    base: String,
    agent: ureq::Agent,
}

impl Default for Registry {
    fn default() -> Self {
        let base = REGISTRY_VARS.into_iter().find_map(|v| std::env::var(v).ok()).unwrap_or_else(|| DEFAULT_REGISTRY.to_owned());
        let agent = ureq::Agent::config_builder().timeout_global(Some(TIMEOUT)).build().into();
        Self { base: base.trim_end_matches('/').to_owned(), agent }
    }
}

impl Registry {
    pub fn package(&self, name: &str) -> Result<Package, String> {
        let url = format!("{}/{}", self.base, name.replace('/', "%2f"));
        let fail = |e: ureq::Error| format!("{name}: {e}");
        let mut response = self.agent.get(&url).header("Accept", ABBREVIATED).call().map_err(fail)?;
        let doc: Document = response.body_mut().with_config().limit(METADATA_LIMIT).read_json().map_err(fail)?;
        Ok(Package::from(doc))
    }

    pub fn tarball(&self, name: &str, dist: &Dist) -> Result<Tarball, String> {
        let fail = |e: ureq::Error| format!("{name}: {e}");
        let mut response = self.agent.get(&dist.tarball).call().map_err(fail)?;
        let bytes = response.body_mut().with_config().limit(TARBALL_LIMIT).read_to_vec().map_err(fail)?;
        match &dist.integrity {
            Some(i) if !i.check(&bytes) => Err(format!("{name}: integrity check failed for {}", dist.tarball)),
            _ => Ok(Tarball(bytes)),
        }
    }
}

#[derive(Deserialize)]
struct Document {
    #[serde(rename = "dist-tags", default)]
    tags: BTreeMap<String, String>,
    #[serde(default)]
    versions: BTreeMap<String, serde_json::Value>,
}

pub struct Package {
    tags: BTreeMap<String, Version>,
    versions: BTreeMap<Version, serde_json::Value>,
}

impl From<Document> for Package {
    fn from(doc: Document) -> Self {
        Self {
            tags: doc.tags.into_iter().filter_map(|(t, v)| Some((t, v.parse().ok()?))).collect(),
            versions: doc.versions.into_iter().filter_map(|(v, m)| Some((v.parse().ok()?, m))).collect(),
        }
    }
}

impl Package {
    pub fn pick(&self, want: &Want) -> Option<&Version> {
        match want {
            Want::Latest => self.tags.get(LATEST).filter(|v| self.versions.contains_key(v)).or_else(|| self.versions.keys().rev().find(|v| !v.is_prerelease())),
            Want::Tag(tag) => self.tags.get(tag).filter(|v| self.versions.contains_key(v)),
            Want::Range(range) => {
                let latest = self.tags.get(LATEST).filter(|v| range.satisfies(v) && self.versions.contains_key(v));
                latest.or_else(|| self.versions.keys().rev().find(|v| range.satisfies(v)))
            }
        }
    }

    pub fn admits(&self, want: &Want, version: &Version) -> bool {
        match want {
            Want::Latest | Want::Tag(_) => self.pick(want) == Some(version),
            Want::Range(range) => range.satisfies(version),
        }
    }

    pub fn manifest(&self, version: &Version) -> Result<Manifest, String> {
        let value = self.versions.get(version).ok_or_else(|| format!("no version {version}"))?;
        Manifest::deserialize(value).map_err(|e| format!("version {version}: {e}"))
    }
}

pub struct Tarball(Vec<u8>);

impl Tarball {
    pub fn unpack(&self, into: &Path) -> std::io::Result<()> {
        let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(self.0.as_slice()));
        for entry in archive.entries()? {
            let mut entry = entry?;
            let path = entry.path()?;
            let inside = path.components().skip(1).all(|c| matches!(c, Component::Normal(_)));
            let rel: PathBuf = path.components().skip(1).collect();
            if !inside || rel.as_os_str().is_empty() {
                continue;
            }
            let dest = into.join(rel);
            match entry.header().entry_type() {
                tar::EntryType::Directory => std::fs::create_dir_all(&dest)?,
                tar::EntryType::Regular | tar::EntryType::Continuous => {
                    if let Some(parent) = dest.parent() {
                        std::fs::create_dir_all(parent)?;
                    }
                    entry.unpack(&dest)?;
                }
                _ => {}
            }
        }
        Ok(())
    }
}
