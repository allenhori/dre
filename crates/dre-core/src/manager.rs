//! The plugin manager: a static JSON registry index of plugin packages, checksum-verified
//! downloads, side-by-side versioned installs, and `dre.lock`.
//!
//! Index location: `DRE_REGISTRY_URL`, default [`DEFAULT_REGISTRY`]. It may be an `https://`
//! URL, a `file://` URL or a plain path. Format: `docs/registry.md`.

use std::io::Read;
use std::path::Path;

use dre_protocol::{
    Kind, PluginId, package_executable_name, parse_executable_name, parse_package_executable_name,
};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::lock::{Checksums, Lock, Locked};
use crate::plugins::{Manifest, version_dir};
use crate::project::{PluginRequirement, PluginSource, Project};

pub const DEFAULT_REGISTRY: &str =
    "https://github.com/allenhori/dre/releases/download/registry/packages.json";

/// The index's current schema. Schema 1 listed single plugins (`kind` and `name`); each still
/// reads as a package of that one plugin.
pub const INDEX_SCHEMA: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Index {
    pub schema: u32,
    pub plugins: Vec<IndexPackage>,
}

/// A plugin package: one executable per platform, serving every plugin in `provides`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(from = "RawPackage")]
pub struct IndexPackage {
    pub name: String,
    #[serde(default)]
    pub description: String,
    /// Empty when the source doesn't say (a GitHub release): the executable is asked once
    /// installed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provides: Vec<PluginId>,
    pub versions: Vec<IndexVersion>,
}

#[derive(Deserialize)]
struct RawPackage {
    name: String,
    /// Schema 1: the one plugin's kind.
    #[serde(default)]
    kind: Option<Kind>,
    #[serde(default)]
    description: String,
    #[serde(default)]
    provides: Vec<PluginId>,
    versions: Vec<IndexVersion>,
}

impl From<RawPackage> for IndexPackage {
    fn from(r: RawPackage) -> IndexPackage {
        let mut provides = r.provides;
        if provides.is_empty()
            && let Some(kind) = r.kind
        {
            provides.push(PluginId::new(kind, r.name.clone()));
        }
        IndexPackage {
            name: r.name,
            description: r.description,
            provides,
            versions: r.versions,
        }
    }
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

/// Name the package to declare for each `undeclared-plugin` error, from DRE's registry, or say
/// that the registry has no such plugin. The registry is fetched now, so a newly published
/// package is known without a new `dre`. Unreachable: unchanged.
pub fn explain_undeclared(diags: &mut crate::Diagnostics) {
    if !diags.iter_mut().any(|d| d.plugin.is_some()) {
        return;
    }
    let Ok(index) = Index::load() else { return };
    for d in diags.iter_mut() {
        let Some((kind, name)) = &d.plugin else { continue };
        let id = PluginId::new(*kind, name.clone());
        let what = d.message.split(" — ").next().unwrap_or_default().to_string();
        let providers: Vec<&str> = index.providers(&id).iter().map(|p| p.name.as_str()).collect();
        let others: Vec<String> = index
            .plugins
            .iter()
            .flat_map(|p| &p.provides)
            .filter(|p| &p.name == name && p.kind != *kind)
            .map(|p| p.kind.to_string())
            .collect();
        d.message = match (providers.as_slice(), others.is_empty()) {
            ([p], _) if *p == name => {
                format!("{what} — add `{p}` under `plugins:` in dependencies.yml, then run `dre deps`")
            }
            ([p], _) => format!(
                "{what} — the `{name}` {kind} is in the `{p}` package: add `{p}` under `plugins:` in dependencies.yml, then run `dre deps`"
            ),
            ([], true) => {
                format!(
                    "{what} — DRE's plugin registry has no {kind} plugin called `{name}`; check the spelling"
                )
            }
            ([], false) => format!(
                "{what} — `{name}` in DRE's plugin registry is a {} plugin, not a {kind}",
                others.join(" and ")
            ),
            (many, _) => format!(
                "{what} — add one of the packages that provide it under `plugins:` in dependencies.yml ({}), then run `dre deps`",
                many.iter()
                    .map(|p| format!("`{p}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        };
    }
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
            // A release asset's API URL answers with its metadata unless asked for the file.
            let accept = if url.contains("/releases/assets/") {
                "application/octet-stream"
            } else {
                "application/vnd.github+json"
            };
            req = req.header("Accept", accept);
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

    /// The index a package installs from, given where the project declares it comes from.
    pub fn for_source(source: &PluginSource, name: &str) -> Result<Index, String> {
        match source {
            PluginSource::Default => Index::load(),
            PluginSource::Registry(u) => Index::load_from(u),
            PluginSource::Github(repo) => github_index(repo, name),
            PluginSource::Local(p) => Err(format!(
                "plugin package `{name}` is used from {p}; there's nothing to install"
            )),
        }
    }

    pub fn package(&self, name: &str) -> Option<&IndexPackage> {
        self.plugins.iter().find(|p| p.name == name)
    }

    /// The packages that provide `id`.
    pub fn providers(&self, id: &PluginId) -> Vec<&IndexPackage> {
        self.plugins.iter().filter(|p| p.provides.contains(id)).collect()
    }

    /// Packages providing a plugin called `name`, of any kind.
    pub fn providers_of_name(&self, name: &str) -> Vec<&IndexPackage> {
        self.plugins
            .iter()
            .filter(|p| p.provides.iter().any(|i| i.name == name))
            .collect()
    }
}

impl IndexPackage {
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
    /// The API URL: it takes `GITHUB_TOKEN` (private repos, GitHub Enterprise) and answers the
    /// file itself with `Accept: application/octet-stream`.
    url: String,
}

/// A one-package index built from `owner/repo`'s GitHub Releases. A release tagged `v1.2.0` (or
/// `1.2.0`) offers version 1.2.0 for each platform it has an asset for, named like the
/// registry's: `dre-plugin-<name>-<version>-<platform>.tar.gz`, or the bare executable. A
/// single plugin's release named `dre-<kind>-<name>-<version>-<platform>` works too.
fn github_index(repo: &str, name: &str) -> Result<Index, String> {
    let url = format!("{}/repos/{repo}/releases?per_page=100", github_api());
    let body = fetch(&url)?;
    let releases: Vec<GithubRelease> =
        serde_json::from_slice(&body).map_err(|e| format!("{url}: unexpected reply from GitHub: {e}"))?;
    let package_stem = format!("dre-plugin-{name}");
    let mut provides: Vec<PluginId> = Vec::new();
    let mut versions = Vec::new();
    for r in releases.iter().filter(|r| !r.draft) {
        let Ok(version) = Version::parse(r.tag_name.trim_start_matches('v')) else {
            continue;
        };
        let suffix = format!("-{version}-");
        let mut artifacts = std::collections::BTreeMap::new();
        for a in &r.assets {
            let Some((stem, rest)) = a.name.split_once(&suffix) else {
                continue;
            };
            if stem != package_stem {
                match parse_executable_name(&format!("{stem}{}", std::env::consts::EXE_SUFFIX)) {
                    Some((kind, n)) if n == name => {
                        let id = PluginId::new(kind, n);
                        if !provides.contains(&id) {
                            provides.push(id);
                        }
                    }
                    _ => continue,
                }
            }
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
                .map(|s| s.url.clone());
            artifacts.insert(
                plat.to_string(),
                Artifact {
                    url: a.url.clone(),
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
            "no release of github.com/{repo} has a `{package_stem}-<version>-<platform>` asset for any platform"
        ));
    }
    // A package of several plugins says what it provides once installed.
    if provides.len() > 1 {
        provides.clear();
    }
    Ok(Index {
        schema: INDEX_SCHEMA,
        plugins: vec![IndexPackage {
            name: name.to_string(),
            description: format!("from github.com/{repo}"),
            provides,
            versions,
        }],
    })
}

/// Download, verify and install one version into `<dir>/<package>/<version>/`. Returns its lock
/// entry.
pub fn install(
    dir: &Path,
    package: &IndexPackage,
    v: &IndexVersion,
    pin: Option<&Checksums>,
) -> Result<Locked, String> {
    let plat = platform();
    let what = format!("plugin package `{}` {}", package.name, v.version);
    let art = v
        .artifacts
        .get(&plat)
        .ok_or_else(|| format!("{what} has no build for {plat}"))?;
    // The published checksum: the index's, a `.sha256` file's, or none yet.
    let published = if !art.sha256.is_empty() {
        Some(art.sha256.clone())
    } else if let Some(u) = &art.sha256_url {
        let text = String::from_utf8_lossy(&fetch(u)?).to_string();
        Some(text.split_whitespace().next().unwrap_or_default().to_string())
    } else {
        None
    };
    let refuse = || format!("{what}: the registry's checksum doesn't match dre.lock; refusing to install");
    // What the lock expects on this platform. A lock that doesn't name platforms (from an older
    // DRE) was written on one of them: its checksum must be one the registry publishes for
    // this version, whichever platform that is.
    let mut want = pin.and_then(|c| c.get(&plat));
    if let (Some(want), Some(published)) = (want, &published)
        && !want.eq_ignore_ascii_case(published)
    {
        return Err(refuse());
    }
    if want.is_none()
        && let Some(legacy) = pin.and_then(Checksums::legacy)
    {
        let all = published_checksums(v);
        if all.is_empty() {
            want = Some(legacy);
        } else if !all.contains(legacy) {
            return Err(refuse());
        }
    }
    let bytes = fetch(&art.url)?;
    let got = hex(&Sha256::digest(&bytes));
    // Nothing published: the lock's pin, if any, is what the download must match.
    let expected = published.as_deref().or(want);
    if let Some(want) = expected
        && !got.eq_ignore_ascii_case(want)
    {
        return Err(format!(
            "checksum mismatch for {what} (expected {want}, got {got}); the download was discarded"
        ));
    }
    let (exe_name, exe) = if art.url.ends_with(".tar.gz") || art.url.ends_with(".tgz") {
        extract_tar_gz(&bytes, &package.name)?
    } else {
        let name = crate::plugins::legacy_executable(&package.provides)
            .unwrap_or_else(|| package_executable_name(&package.name));
        (name, bytes)
    };
    let vdir = version_dir(dir, &package.name, &v.version);
    std::fs::create_dir_all(&vdir).map_err(|e| format!("can't create {}: {e}", vdir.display()))?;
    let dst = vdir.join(&exe_name);
    let tmp = vdir.join(format!(".download-{}", std::process::id()));
    std::fs::write(&tmp, &exe).map_err(|e| format!("can't write {}: {e}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    std::fs::rename(&tmp, &dst).map_err(|e| format!("can't install {}: {e}", dst.display()))?;
    // A source that doesn't list what the package provides: ask the executable.
    let provides = if package.provides.is_empty() {
        crate::plugins::probe(&dst)?
    } else {
        package.provides.clone()
    };
    crate::plugins::write_manifest(
        &vdir,
        &Manifest {
            executable: exe_name,
            provides: provides.clone(),
        },
    )?;
    let mut sha256 = published_checksums(v);
    if let Some(p) = pin {
        sha256.merge(p);
    }
    sha256.insert(&plat, &got);
    Ok(Locked {
        version: v.version.clone(),
        sha256,
        from: None,
        provides,
    })
}

/// The checksums the registry publishes for `v`, by platform.
fn published_checksums(v: &IndexVersion) -> Checksums {
    let mut c = Checksums::new();
    for (p, a) in &v.artifacts {
        if !a.sha256.is_empty() {
            c.insert(p, &a.sha256);
        }
    }
    c
}

/// The package's executable in an archive: `dre-plugin-<package>`, or a single plugin's
/// `dre-<kind>-<package>`. Returns its file name and bytes.
fn extract_tar_gz(bytes: &[u8], package: &str) -> Result<(String, Vec<u8>), String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    for entry in archive.entries().map_err(|e| e.to_string())? {
        let mut entry = entry.map_err(|e| e.to_string())?;
        let path = entry.path().map_err(|e| e.to_string())?.to_path_buf();
        let Some(file) = path.file_name().map(|f| f.to_string_lossy().to_string()) else {
            continue;
        };
        let ours = parse_package_executable_name(&file).is_some_and(|n| n == package)
            || parse_executable_name(&file).is_some_and(|(_, n)| n == package);
        if ours {
            let mut out = Vec::new();
            entry.read_to_end(&mut out).map_err(|e| e.to_string())?;
            return Ok((file, out));
        }
    }
    Err(format!(
        "the archive doesn't contain `{}`",
        package_executable_name(package)
    ))
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// What `sync` did for one package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Synced {
    AlreadyInstalled { name: String, version: Option<Version> },
    Installed { name: String, version: Version },
}

/// Make every declared package available, installing what's missing (unless `install` is
/// false, in which case missing packages are errors). Honours `dre.lock` pins, writes new pins.
///
/// With `resolve` (`dre deps`), a package that `dre.lock` doesn't pin is resolved against the
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
        let name = &req.name;
        if let PluginSource::Local(p) = &req.source {
            if !project.root.join(p).is_file() {
                errors.push(format!(
                    "the plugin package `{name}` is declared `local: {p}`, but there's no file there"
                ));
                continue;
            }
            if lock.local.get(name) != Some(p) {
                lock.local.insert(name.clone(), p.clone());
                lock_changed = true;
            }
            lock_changed |= lock.plugins.remove(name).is_some();
            out.push(Synced::AlreadyInstalled {
                name: name.clone(),
                version: None,
            });
            continue;
        }
        lock_changed |= lock.local.remove(name).is_some();
        // A pin from another source doesn't count: the package is resolved again, and an
        // installed copy (from the old source) isn't reused.
        let moved = lock.get(name).is_some_and(|l| l.from != req.source.lock_key());
        let pin = lock
            .get(name)
            .filter(|l| l.from == req.source.lock_key())
            .cloned();
        if !moved
            && let Some(p) =
                crate::plugins::find(&dir, name, Some(&req.req()), pin.as_ref().map(|l| &l.version))
                    .into_iter()
                    .next()
        {
            // A hand-placed (flat) package satisfies any pin; versioned installs must match it,
            // and when resolving, an unpinned versioned install goes back to the registry.
            let ok = match (&pin, &p.version) {
                (Some(l), Some(v)) => &l.version == v,
                (None, Some(_)) => !resolve,
                _ => true,
            };
            if ok {
                // A lock from before `provides` was recorded learns it from the install.
                if let (Some(l), Some(_)) = (lock.plugins.get_mut(name), &p.version)
                    && l.provides.is_empty()
                    && !p.provides.is_empty()
                {
                    l.provides = p.provides.clone();
                    lock_changed = true;
                }
                out.push(Synced::AlreadyInstalled {
                    name: name.clone(),
                    version: p.version,
                });
                continue;
            }
        }
        // Already downloaded for another project: link the pinned version from the cache. The
        // cache only holds the default registry's builds: it's keyed by name and version.
        if let Some(l) = &pin
            && req.source.is_default()
            && dir != crate::plugins::cache_dir()
        {
            let cached = version_dir(&crate::plugins::cache_dir(), name, &l.version);
            if crate::plugins::read_manifest(&cached).is_some()
                && crate::plugins::link_version(&cached, &version_dir(&dir, name, &l.version)).is_ok()
            {
                out.push(Synced::AlreadyInstalled {
                    name: name.clone(),
                    version: Some(l.version.clone()),
                });
                continue;
            }
        }
        if !install_missing {
            errors.push(format!(
                "the plugin package `{name}` ({}) isn't installed and auto-install is off; run `dre deps`",
                pin.as_ref()
                    .map(|l| format!("locked at {}", l.version))
                    .unwrap_or_else(|| req.version.clone())
            ));
            continue;
        }
        // One index per source; a GitHub repo's covers just the package it was built for.
        let ikey = match &req.source {
            PluginSource::Github(_) => format!("{}#{name}", req.source.lock_key().unwrap_or_default()),
            s => s.lock_key().unwrap_or_default(),
        };
        if !indexes.contains_key(&ikey) {
            match Index::for_source(&req.source, name) {
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
                lock.plugins.insert(name.clone(), locked.clone());
                lock_changed = true;
                out.push(Synced::AlreadyInstalled {
                    name: name.clone(),
                    version: Some(locked.version),
                });
            }
            Ok((locked, true)) => {
                log(&format!("installed plugin package `{name}` {}", locked.version));
                if lock.get(name) != Some(&locked) {
                    lock.plugins.insert(name.clone(), locked.clone());
                    lock_changed = true;
                }
                out.push(Synced::Installed {
                    name: name.clone(),
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
    let name = &req.name;
    let package = index.package(name).ok_or_else(|| {
        let inside: Vec<String> = index
            .plugins
            .iter()
            .filter(|p| p.provides.iter().any(|i| &i.name == name))
            .map(|p| format!("`{}`", p.name))
            .collect();
        if inside.is_empty() {
            format!("the registry has no plugin package `{name}`")
        } else {
            format!(
                "the registry has no plugin package `{name}`; `{name}` is a plugin in {}: declare that under `plugins:` instead",
                inside.join(", ")
            )
        }
    })?;
    let v = match pin {
        Some(l) => package.exact(&l.version).ok_or_else(|| {
            format!(
                "dre.lock pins plugin package `{name}` {}, which the registry doesn't list",
                l.version
            )
        })?,
        None => package.best(&req.req()).ok_or_else(|| {
            format!(
                "no version of the plugin package `{name}` matches `{}` for {}",
                req.version,
                platform()
            )
        })?,
    };
    // Reuse the installed file only when it is byte for byte the registry's artifact: a rebuild
    // published under the same version must be installed again, not pinned to a checksum the
    // installed file doesn't have. (An archived artifact's checksum can't be compared with the
    // extracted executable, so those are always reinstalled.)
    let vdir = version_dir(dir, name, &v.version);
    if pin.is_none()
        && let Some(art) = v.artifacts.get(&platform())
        && !(art.url.ends_with(".tar.gz") || art.url.ends_with(".tgz"))
        && let Some(m) = crate::plugins::read_manifest(&vdir)
        && std::fs::read(vdir.join(&m.executable))
            .is_ok_and(|b| hex(&Sha256::digest(&b)).eq_ignore_ascii_case(&art.sha256))
    {
        let locked = Locked {
            version: v.version.clone(),
            sha256: published_checksums(v),
            from: None,
            provides: m.provides,
        };
        return Ok((locked, false));
    }
    let expect = pin.map(|l| &l.sha256);
    if req.source.is_default() {
        install_linked(dir, package, v, expect).map(|l| (l, true))
    } else {
        install(dir, package, v, expect).map(|l| (l, true))
    }
}

/// Install into `dir` through the shared cache: download there once, then link into `dir`.
pub fn install_linked(
    dir: &Path,
    package: &IndexPackage,
    v: &IndexVersion,
    pin: Option<&Checksums>,
) -> Result<Locked, String> {
    let cache = crate::plugins::cache_dir();
    let locked = install(&cache, package, v, pin)?;
    if dir != cache {
        crate::plugins::link_version(
            &version_dir(&cache, &package.name, &v.version),
            &version_dir(dir, &package.name, &v.version),
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
