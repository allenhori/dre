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
use crate::project::{PluginKind, PluginRequirement, PluginSource, Project};

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
    /// Hex SHA-256 of the download. Empty when the source doesn't publish one: then it comes
    /// from `sha256_url`, or failing that, from the first download (and `dre.lock` pins it).
    #[serde(default)]
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sha256_url: Option<String>,
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

/// The GitHub API DRE talks to: `DRE_GITHUB_API_URL` (GitHub Enterprise, tests), else GitHub's.
fn github_api() -> String {
    std::env::var("DRE_GITHUB_API_URL")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "https://api.github.com".into())
        .trim_end_matches('/')
        .to_string()
}

/// Whether requests to `url` should carry `GITHUB_TOKEN`: GitHub itself or the configured API.
fn is_github(url: &str) -> bool {
    url.starts_with("https://github.com/") || url.starts_with(&format!("{}/", github_api()))
}

fn fetch(url: &str) -> Result<Vec<u8>, String> {
    if url.starts_with("http://") || url.starts_with("https://") {
        let mut req = ureq::get(url).header("User-Agent", "dre");
        if is_github(url) {
            req = req.header(
                "Accept",
                "application/vnd.github+json, application/octet-stream;q=0.9",
            );
            if let Ok(t) = std::env::var("GITHUB_TOKEN")
                && !t.is_empty()
            {
                req = req.header("Authorization", &format!("Bearer {t}"));
            }
        }
        let mut resp = req.call().map_err(|e| format!("can't download {url}: {e}"))?;
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
    /// The default registry.
    pub fn load() -> Result<Index, String> {
        Index::load_from(&registry_url())
    }

    pub fn load_from(url: &str) -> Result<Index, String> {
        let body = fetch(url)?;
        serde_json::from_slice(&body)
            .map_err(|e| format!("the plugin registry at {url} isn't a valid index: {e}"))
    }

    /// The index a plugin installs from, given where the project declares it comes from.
    pub fn for_source(source: &PluginSource, kind: PluginKind, name: &str) -> Result<Index, String> {
        match source {
            PluginSource::Default => Index::load(),
            PluginSource::Registry(u) => Index::load_from(u),
            PluginSource::Github(repo) => github_index(repo, kind, name),
            PluginSource::Local(p) => Err(format!(
                "{} plugin `{name}` is used from {p}; there's nothing to install",
                kind.as_str()
            )),
        }
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
    /// protocol this core supports. Pre-releases only when no stable version matches (see
    /// [`prefer_stable`]).
    pub fn best(&self, req: &VersionReq) -> Option<&IndexVersion> {
        let plat = platform();
        let usable: Vec<&IndexVersion> = self
            .versions
            .iter()
            .filter(|v| (dre_protocol::MIN_VERSION..=dre_protocol::MAX_VERSION).contains(&v.protocol))
            .filter(|v| v.artifacts.contains_key(&plat))
            .collect();
        prefer_stable(usable, req, |v| &v.version)
    }

    pub fn exact(&self, v: &Version) -> Option<&IndexVersion> {
        self.versions.iter().find(|x| &x.version == v)
    }
}

/// The highest of `items` whose version matches `req`. When none does, a pre-release counts if
/// its release would match, so `*` finds `0.0.1-alpha` when a plugin has no stable release yet
/// (semver on its own only matches a pre-release that `req` names explicitly).
pub fn prefer_stable<T>(items: Vec<T>, req: &VersionReq, version: impl Fn(&T) -> &Version) -> Option<T> {
    let highest = |ok: &dyn Fn(&Version) -> bool, items: Vec<T>| {
        items
            .into_iter()
            .filter(|i| ok(version(i)))
            .max_by(|a, b| version(a).cmp(version(b)))
    };
    let exact = |v: &Version| req.matches(v);
    if items.iter().any(|i| exact(version(i))) {
        return highest(&exact, items);
    }
    highest(
        &|v: &Version| !v.pre.is_empty() && req.matches(&Version::new(v.major, v.minor, v.patch)),
        items,
    )
}

#[derive(Deserialize)]
struct GithubRelease {
    tag_name: String,
    #[serde(default)]
    draft: bool,
    #[serde(default)]
    assets: Vec<GithubAsset>,
}

#[derive(Deserialize)]
struct GithubAsset {
    name: String,
    browser_download_url: String,
}

/// A one-plugin index built from `owner/repo`'s GitHub Releases. A release tagged `v1.2.0` (or
/// `1.2.0`) offers version 1.2.0 for each platform it has an asset for, named like the
/// registry's: `dre-<kind>-<name>-<version>-<platform>.tar.gz`, or the bare executable.
fn github_index(repo: &str, kind: PluginKind, name: &str) -> Result<Index, String> {
    let url = format!("{}/repos/{repo}/releases?per_page=100", github_api());
    let body = fetch(&url)?;
    let releases: Vec<GithubRelease> =
        serde_json::from_slice(&body).map_err(|e| format!("{url}: unexpected reply from GitHub: {e}"))?;
    let exe = executable_name(kind, name);
    let stem = exe.trim_end_matches(".exe").to_string();
    let mut versions = Vec::new();
    for r in releases.iter().filter(|r| !r.draft) {
        let Ok(version) = Version::parse(r.tag_name.trim_start_matches('v')) else {
            continue;
        };
        let prefix = format!("{stem}-{version}-");
        let mut artifacts = std::collections::BTreeMap::new();
        for a in &r.assets {
            let Some(rest) = a.name.strip_prefix(&prefix) else {
                continue;
            };
            let plat = rest
                .trim_end_matches(".tar.gz")
                .trim_end_matches(".tgz")
                .trim_end_matches(".exe");
            if plat.contains('.') || plat.is_empty() {
                continue; // `.sha256` and other side files
            }
            let sha256_url = r
                .assets
                .iter()
                .find(|s| s.name == format!("{}.sha256", a.name))
                .map(|s| s.browser_download_url.clone());
            artifacts.insert(
                plat.to_string(),
                Artifact {
                    url: a.browser_download_url.clone(),
                    sha256: String::new(),
                    sha256_url,
                },
            );
        }
        if !artifacts.is_empty() {
            versions.push(IndexVersion {
                version,
                protocol: dre_protocol::MAX_VERSION,
                artifacts,
            });
        }
    }
    if versions.is_empty() {
        return Err(format!(
            "no release of github.com/{repo} has a `{stem}-<version>-<platform>` asset for any platform"
        ));
    }
    Ok(Index {
        schema: 1,
        plugins: vec![IndexPlugin {
            kind,
            name: name.to_string(),
            description: format!("from github.com/{repo}"),
            versions,
        }],
    })
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
    // The published checksum: the index's, a `.sha256` file's, or none yet.
    let published = if !art.sha256.is_empty() {
        Some(art.sha256.clone())
    } else if let Some(u) = &art.sha256_url {
        let text = String::from_utf8_lossy(&fetch(u)?).to_string();
        Some(text.split_whitespace().next().unwrap_or_default().to_string())
    } else {
        None
    };
    if let (Some(want), Some(published)) = (expect_sha, &published)
        && !want.eq_ignore_ascii_case(published)
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
    // Nothing published: the lock's pin, if any, is what the download must match.
    let expected = published.as_deref().or(expect_sha);
    if let Some(want) = expected
        && !got.eq_ignore_ascii_case(want)
    {
        return Err(format!(
            "checksum mismatch for {} `{}` {} (expected {want}, got {got}); the download was discarded",
            plugin.kind.as_str(),
            plugin.name,
            v.version,
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
        sha256: got,
        from: None,
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
///
/// With `resolve` (`dre deps`), a plugin that `dre.lock` doesn't pin is resolved against the
/// registry even when some matching version is already installed, so deleting `dre.lock` picks
/// up the newest allowed release. Without it (auto-install before run/validate), any installed
/// match will do and the registry is only consulted for what's missing.
pub fn sync(
    project: &Project,
    install_missing: bool,
    resolve: bool,
    mut log: impl FnMut(&str),
) -> Result<Vec<Synced>, Vec<String>> {
    let dir = crate::plugins::plugins_dir(Some(&project.root));
    let mut lock = Lock::load(&project.root).map_err(|e| vec![e])?;
    let mut indexes: std::collections::BTreeMap<String, Index> = std::collections::BTreeMap::new();
    let mut out = Vec::new();
    let mut errors = Vec::new();
    let mut lock_changed = false;
    for req in &project.plugins {
        let key = format!("{}/{}", req.kind.as_str(), req.name);
        if let PluginSource::Local(p) = &req.source {
            if !project.root.join(p).is_file() {
                errors.push(format!(
                    "the {} plugin `{}` is declared `local: {p}`, but there's no file there",
                    req.kind.as_str(),
                    req.name
                ));
                continue;
            }
            if lock.local.get(&key) != Some(p) {
                lock.local.insert(key, p.clone());
                lock_changed = true;
            }
            lock_changed |= lock.map_mut(req.kind).remove(&req.name).is_some();
            out.push(Synced::AlreadyInstalled {
                kind: req.kind,
                name: req.name.clone(),
                version: None,
            });
            continue;
        }
        lock_changed |= lock.local.remove(&key).is_some();
        // A pin from another source doesn't count: the plugin is resolved again.
        let pin = lock
            .get(req.kind, &req.name)
            .filter(|l| l.from == req.source.lock_key())
            .cloned();
        if let Some(p) = crate::plugins::find(
            &dir,
            req.kind,
            &req.name,
            Some(&req.req()),
            pin.as_ref().map(|l| &l.version),
        ) {
            // A hand-placed (flat) plugin satisfies any pin; versioned installs must match it,
            // and when resolving, an unpinned versioned install goes back to the registry.
            let ok = match (&pin, &p.version) {
                (Some(l), Some(v)) => &l.version == v,
                (None, Some(_)) => !resolve,
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
        // Already downloaded for another project: link the pinned version from the cache.
        if let Some(l) = &pin
            && dir != crate::plugins::cache_dir()
        {
            let cached = install_path(&crate::plugins::cache_dir(), req.kind, &req.name, &l.version);
            if cached.is_file()
                && crate::plugins::link_or_copy(&cached, &install_path(&dir, req.kind, &req.name, &l.version))
                    .is_ok()
            {
                out.push(Synced::AlreadyInstalled {
                    kind: req.kind,
                    name: req.name.clone(),
                    version: Some(l.version.clone()),
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
        // One index per source; a GitHub repo's covers just the plugin it was built for.
        let ikey = match &req.source {
            PluginSource::Github(_) => format!("{}#{key}", req.source.lock_key().unwrap_or_default()),
            s => s.lock_key().unwrap_or_default(),
        };
        if !indexes.contains_key(&ikey) {
            match Index::for_source(&req.source, req.kind, &req.name) {
                Ok(i) => {
                    indexes.insert(ikey.clone(), i);
                }
                Err(e) if req.source.is_default() => return Err(vec![e]),
                Err(e) => {
                    errors.push(e);
                    continue;
                }
            }
        }
        match install_one(&dir, &indexes[&ikey], req, pin.as_ref()).map(|(mut l, fresh)| {
            l.from = req.source.lock_key();
            (l, fresh)
        }) {
            Ok((locked, false)) => {
                lock.map_mut(req.kind).insert(req.name.clone(), locked.clone());
                lock_changed = true;
                out.push(Synced::AlreadyInstalled {
                    kind: req.kind,
                    name: req.name.clone(),
                    version: Some(locked.version),
                });
            }
            Ok((locked, true)) => {
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

/// Install the pinned version, or the best match when unpinned. The flag is false when that
/// exact version was already in `dir`, identical to the registry's, and only needed its lock
/// entry.
fn install_one(
    dir: &Path,
    index: &Index,
    req: &PluginRequirement,
    pin: Option<&Locked>,
) -> Result<(Locked, bool), String> {
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
    // Reuse the installed file only when it is byte for byte the registry's artifact: a rebuild
    // published under the same version must be installed again, not pinned to a checksum the
    // installed file doesn't have. (An archived artifact's checksum can't be compared with the
    // extracted executable, so those are always reinstalled.)
    let installed = install_path(dir, req.kind, &req.name, &v.version);
    if pin.is_none()
        && let Some(art) = v.artifacts.get(&platform())
        && !(art.url.ends_with(".tar.gz") || art.url.ends_with(".tgz"))
        && std::fs::read(&installed).is_ok_and(|b| hex(&Sha256::digest(&b)).eq_ignore_ascii_case(&art.sha256))
    {
        let locked = Locked {
            version: v.version.clone(),
            sha256: art.sha256.to_lowercase(),
            from: None,
        };
        return Ok((locked, false));
    }
    install_linked(dir, plugin, v, pin.map(|l| l.sha256.as_str())).map(|l| (l, true))
}

/// Install into `dir` through the shared cache: download there once, then link into `dir`.
pub fn install_linked(
    dir: &Path,
    plugin: &IndexPlugin,
    v: &IndexVersion,
    expect_sha: Option<&str>,
) -> Result<Locked, String> {
    let cache = crate::plugins::cache_dir();
    let locked = install(&cache, plugin, v, expect_sha)?;
    if dir != cache {
        crate::plugins::link_or_copy(
            &install_path(&cache, plugin.kind, &plugin.name, &v.version),
            &install_path(dir, plugin.kind, &plugin.name, &v.version),
        )?;
    }
    Ok(locked)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pick(versions: &[&str], req: &str) -> Option<String> {
        let vs: Vec<Version> = versions.iter().map(|v| Version::parse(v).unwrap()).collect();
        prefer_stable(vs.iter().collect(), &VersionReq::parse(req).unwrap(), |v| v).map(|v| v.to_string())
    }

    #[test]
    fn pre_releases_only_when_nothing_stable_matches() {
        assert_eq!(pick(&["0.0.1-alpha"], "*").as_deref(), Some("0.0.1-alpha"));
        assert_eq!(
            pick(&["0.0.1-alpha", "0.0.2-beta"], "*").as_deref(),
            Some("0.0.2-beta")
        );
        assert_eq!(pick(&["0.1.0", "0.2.0-rc.1"], "*").as_deref(), Some("0.1.0"));
        assert_eq!(
            pick(&["0.0.1-alpha"], "^0.0.1-alpha").as_deref(),
            Some("0.0.1-alpha")
        );
        assert_eq!(pick(&["0.0.1-alpha"], "^1").as_deref(), None);
        assert_eq!(pick(&[], "*").as_deref(), None);
    }
}
