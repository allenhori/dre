//! The plugin manager: a static JSON registry index, checksum-verified downloads, side-by-side
//! versioned installs, and `dre.lock`.
//!
//! Index location: `DRE_REGISTRY_URL`, default [`DEFAULT_REGISTRY`]. It may be an `https://`
//! URL, a `file://` URL or a plain path. Format: `docs/registry.md`.

use std::io::Read;
use std::path::{Path, PathBuf};

use dre_protocol::executable_name;
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::lock::{Lock, Locked};
use crate::project::{PluginKind, PluginRequirement, Project};

pub const DEFAULT_REGISTRY: &str = "https://github.com/allenhori/dre/releases/download/registry/index.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub schema: u32,
    pub plugins: Vec<IndexPlugin>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexPlugin {
    pub kind: PluginKind,
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub versions: Vec<IndexVersion>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexVersion {
    pub version: Version,
    /// Plugin protocol version this release speaks.
    #[serde(default)]
    pub protocol: u32,
    /// Platform (see [`platform`]) → artifact.
    pub artifacts: std::collections::BTreeMap<String, Artifact>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Artifact {
    pub url: String,
    pub sha256: String,
}

/// This machine's platform key in the index: `<os>-<arch>`, e.g. `macos-aarch64`.
pub fn platform() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

pub fn registry_url() -> String {
    std::env::var("DRE_REGISTRY_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_REGISTRY.to_string())
}

fn fetch(url: &str) -> Result<Vec<u8>, String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        let mut resp = ureq::get(url)
            .call()
            .map_err(|e| format!("can't download {url}: {e}"))?;
        let mut body = Vec::new();
        resp.body_mut()
            .as_reader()
            .read_to_end(&mut body)
            .map_err(|e| format!("can't download {url}: {e}"))?;
        return Ok(body);
    }
    let path = url.strip_prefix("file://").unwrap_or(url);
    std::fs::read(path).map_err(|e| format!("can't read {path}: {e}"))
}

impl Index {
    pub fn load() -> Result<Index, String> {
        let url = registry_url();
        let body = fetch(&url)?;
        serde_json::from_slice(&body)
            .map_err(|e| format!("the plugin registry at {url} isn't a valid index: {e}"))
    }

    pub fn plugin(&self, kind: PluginKind, name: &str) -> Option<&IndexPlugin> {
        self.plugins.iter().find(|p| p.kind == kind && p.name == name)
    }

    /// Plugins called `name`, of any kind.
    pub fn by_name(&self, name: &str) -> Vec<&IndexPlugin> {
        self.plugins.iter().filter(|p| p.name == name).collect()
    }
}

impl IndexPlugin {
    /// The highest version matching `req` that has an artifact for this platform and speaks a
    /// protocol this core supports.
    pub fn best(&self, req: &VersionReq) -> Option<&IndexVersion> {
        let plat = platform();
        self.versions
            .iter()
            .filter(|v| req.matches(&v.version))
            .filter(|v| (dre_protocol::MIN_VERSION..=dre_protocol::MAX_VERSION).contains(&v.protocol))
            .filter(|v| v.artifacts.contains_key(&plat))
            .max_by(|a, b| a.version.cmp(&b.version))
    }

    pub fn exact(&self, v: &Version) -> Option<&IndexVersion> {
        self.versions.iter().find(|x| &x.version == v)
    }
}

/// Where a version is installed.
pub fn install_path(dir: &Path, kind: PluginKind, name: &str, version: &Version) -> PathBuf {
    dir.join(kind.as_str())
        .join(name)
        .join(version.to_string())
        .join(executable_name(kind, name))
}

/// Download, verify and install one version. Returns its lock entry.
pub fn install(
    dir: &Path,
    plugin: &IndexPlugin,
    v: &IndexVersion,
    expect_sha: Option<&str>,
) -> Result<Locked, String> {
    let plat = platform();
    let art = v.artifacts.get(&plat).ok_or_else(|| {
        format!(
            "{} `{}` {} has no build for {plat}",
            plugin.kind.as_str(),
            plugin.name,
            v.version
        )
    })?;
    if let Some(want) = expect_sha
        && !want.eq_ignore_ascii_case(&art.sha256)
    {
        return Err(format!(
            "{} `{}` {}: the registry's checksum doesn't match dre.lock; refusing to install",
            plugin.kind.as_str(),
            plugin.name,
            v.version
        ));
    }
    let bytes = fetch(&art.url)?;
    let got = hex(&Sha256::digest(&bytes));
    if !got.eq_ignore_ascii_case(&art.sha256) {
        return Err(format!(
            "checksum mismatch for {} `{}` {} (expected {}, got {got}); the download was discarded",
            plugin.kind.as_str(),
            plugin.name,
            v.version,
            art.sha256
        ));
    }
    let exe = if art.url.ends_with(".tar.gz") || art.url.ends_with(".tgz") {
        extract_tar_gz(&bytes, &executable_name(plugin.kind, &plugin.name))?
    } else {
        bytes
    };
    let dst = install_path(dir, plugin.kind, &plugin.name, &v.version);
    let parent = dst.parent().unwrap();
    std::fs::create_dir_all(parent).map_err(|e| format!("can't create {}: {e}", parent.display()))?;
    let tmp = parent.join(format!(".download-{}", std::process::id()));
    std::fs::write(&tmp, &exe).map_err(|e| format!("can't write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &dst).map_err(|e| format!("can't install {}: {e}", dst.display()))?;
    Ok(Locked {
        version: v.version.clone(),
        sha256: art.sha256.to_lowercase(),
    })
}

fn extract_tar_gz(bytes: &[u8], exe: &str) -> Result<Vec<u8>, String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.to_path_buf();
        if path.file_name().is_some_and(|f| f == exe) {
            let mut out = Vec::new();
            entry.read_to_end(&mut out).map_err(|e| e.to_string())?;
            return Ok(out);
        }
    }
    Err(format!("the archive doesn't contain `{exe}`"))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What `sync` did for one plugin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Synced {
    AlreadyInstalled {
        kind: PluginKind,
        name: String,
        version: Option<Version>,
    },
    Installed {
        kind: PluginKind,
        name: String,
        version: Version,
    },
}

/// Make every declared plugin available, installing what's missing (unless `install` is false,
/// in which case missing plugins are errors). Honours `dre.lock` pins, writes new pins.
pub fn sync(
    project: &Project,
    install_missing: bool,
    mut log: impl FnMut(&str),
) -> Result<Vec<Synced>, Vec<String>> {
    let dir = crate::plugins::plugins_dir();
    let mut lock = Lock::load(&project.root).map_err(|e| vec![e])?;
    let mut index: Option<Index> = None;
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let mut lock_changed = false;
    for req in &project.plugins {
        let pin = lock.get(req.kind, &req.name).cloned();
        if let Some(p) = crate::plugins::find(
            &dir,
            req.kind,
            &req.name,
            Some(&req.req()),
            pin.as_ref().map(|l| &l.version),
        ) {
            // A hand-placed (flat) plugin satisfies any pin; versioned installs must match it.
            let ok = match (&pin, &p.version) {
                (Some(l), Some(v)) => &l.version == v,
                _ => true,
            };
            if ok {
                out.push(Synced::AlreadyInstalled {
                    kind: req.kind,
                    name: req.name.clone(),
                    version: p.version,
                });
                continue;
            }
        }
        if !install_missing {
            errors.push(format!(
                "the {} plugin `{}` ({}) isn't installed and auto-install is off; run `dre deps`",
                req.kind.as_str(),
                req.name,
                pin.as_ref()
                    .map(|l| format!("locked at {}", l.version))
                    .unwrap_or_else(|| req.version.clone())
            ));
            continue;
        }
        if index.is_none() {
            match Index::load() {
                Ok(i) => index = Some(i),
                Err(e) => return Err(vec![e]),
            }
        }
        match install_one(&dir, index.as_ref().unwrap(), req, pin.as_ref()) {
            Ok(locked) => {
                log(&format!(
                    "installed {} plugin `{}` {}",
                    req.kind.as_str(),
                    req.name,
                    locked.version
                ));
                if lock.get(req.kind, &req.name) != Some(&locked) {
                    lock.map_mut(req.kind).insert(req.name.clone(), locked.clone());
                    lock_changed = true;
                }
                out.push(Synced::Installed {
                    kind: req.kind,
                    name: req.name.clone(),
                    version: locked.version,
                });
            }
            Err(e) => errors.push(e),
        }
    }
    if lock_changed && let Err(e) = lock.save(&project.root) {
        errors.push(e);
    }
    if errors.is_empty() { Ok(out) } else { Err(errors) }
}

fn install_one(
    dir: &Path,
    index: &Index,
    req: &PluginRequirement,
    pin: Option<&Locked>,
) -> Result<Locked, String> {
    let plugin = index
        .plugin(req.kind, &req.name)
        .ok_or_else(|| format!("the registry has no {} plugin `{}`", req.kind.as_str(), req.name))?;
    let v = match pin {
        Some(l) => plugin.exact(&l.version).ok_or_else(|| {
            format!(
                "dre.lock pins {} `{}` {}, which the registry doesn't list",
                req.kind.as_str(),
                req.name,
                l.version
            )
        })?,
        None => plugin.best(&req.req()).ok_or_else(|| {
            format!(
                "no version of the {} plugin `{}` matches `{}` for {}",
                req.kind.as_str(),
                req.name,
                req.version,
                platform()
            )
        })?,
    };
    install(dir, plugin, v, pin.map(|l| l.sha256.as_str()))
}
