//! Finding installed plugin packages.
//!
//! A project's plugins live in `<project>/dre_deps/plugins`, hard-linked (or copied) from a shared
//! download cache in `~/.dre/plugins`. `DRE_PLUGINS_DIR` replaces both. Inside a plugins
//! directory, packages sit side by side by version, `<dir>/<package>/<version>/`, holding the
//! package's executable and a [`MANIFEST`] naming it and the plugins it provides, as installed by
//! the plugin manager. Executables placed by hand (development, tests) sit flat in the directory:
//! `dre-plugin-<package>`, or `dre-<kind>-<name>` for a package of that one plugin, called `<name>`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use dre_protocol::{PluginId, executable_name, parse_executable_name, parse_package_executable_name};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};

/// What an installed version's directory holds besides the executable.
pub const MANIFEST: &str = "plugin.json";

/// An installed version's [`MANIFEST`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    /// The executable's file name, in the same directory.
    pub executable: String,
    pub provides: Vec<PluginId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPackage {
    pub name: String,
    /// Known for versioned installs; flat ones report theirs in the handshake.
    pub version: Option<Version>,
    pub path: PathBuf,
    /// Empty when a flat package executable can't be asked (it doesn't start).
    pub provides: Vec<PluginId>,
}

impl InstalledPackage {
    pub fn provides(&self, id: &PluginId) -> bool {
        self.provides.contains(id)
    }
}

/// Everything a project installs: plugins and macro packages.
pub const DEPS_DIR: &str = "dre_deps";

/// `DRE_PLUGINS_DIR`, when set: plugins live and install there, for every project.
pub fn override_dir() -> Option<PathBuf> {
    std::env::var_os("DRE_PLUGINS_DIR")
        .filter(|p| !p.is_empty())
        .map(PathBuf::from)
}

/// The shared download cache, which project installs link from; also where `dre init` installs
/// before a project exists.
pub fn cache_dir() -> PathBuf {
    override_dir().unwrap_or_else(|| crate::dre_home().join("plugins"))
}

/// Where a project's plugins live (`None`: outside any project, the cache).
pub fn plugins_dir(project: Option<&Path>) -> PathBuf {
    match (override_dir(), project) {
        (Some(d), _) => d,
        (None, Some(root)) => root.join(DEPS_DIR).join("plugins"),
        (None, None) => cache_dir(),
    }
}

/// Put `src` at `dst` without a second copy on disk where possible.
pub fn link_or_copy(src: &Path, dst: &Path) -> Result<(), String> {
    let parent = dst.parent().unwrap();
    std::fs::create_dir_all(parent).map_err(|e| format!("can't create {}: {e}", parent.display()))?;
    let _ = std::fs::remove_file(dst);
    if std::fs::hard_link(src, dst).is_ok() {
        return Ok(());
    }
    std::fs::copy(src, dst).map_err(|e| format!("can't copy {} to {}: {e}", src.display(), dst.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dst, std::fs::Permissions::from_mode(0o755)).map_err(|e| e.to_string())?;
    }
    Ok(())
}

/// Link (or copy) an installed version's directory, executable and manifest, from one plugins
/// directory to another.
pub fn link_version(from: &Path, to: &Path) -> Result<(), String> {
    let m = read_manifest(from).ok_or_else(|| format!("{} has no {MANIFEST}", from.display()))?;
    link_or_copy(&from.join(&m.executable), &to.join(&m.executable))?;
    link_or_copy(&from.join(MANIFEST), &to.join(MANIFEST))
}

pub fn read_manifest(version_dir: &Path) -> Option<Manifest> {
    let text = std::fs::read_to_string(version_dir.join(MANIFEST)).ok()?;
    serde_json::from_str(&text).ok()
}

pub fn write_manifest(version_dir: &Path, m: &Manifest) -> Result<(), String> {
    let p = version_dir.join(MANIFEST);
    std::fs::write(&p, serde_json::to_string_pretty(m).unwrap())
        .map_err(|e| format!("can't write {}: {e}", p.display()))
}

fn is_executable(p: &Path) -> bool {
    let Ok(meta) = std::fs::metadata(p) else {
        return false;
    };
    if !meta.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        meta.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    true
}

/// What an executable said it provides, and the file's modification time when asked.
type Asked = (Option<std::time::SystemTime>, Vec<PluginId>);

/// The plugins an executable serves, from its name when that's `dre-<kind>-<name>`, else from
/// its handshake. Asked once per file and modification time.
pub fn probe(path: &Path) -> Result<Vec<PluginId>, String> {
    static ASKED: Mutex<BTreeMap<PathBuf, Asked>> = Mutex::new(BTreeMap::new());
    let file = path.file_name().unwrap_or_default().to_string_lossy();
    if let Some((kind, name)) = parse_executable_name(&file) {
        return Ok(vec![PluginId::new(kind, name)]);
    }
    let modified = std::fs::metadata(path).and_then(|m| m.modified()).ok();
    if let Some((m, ids)) = ASKED.lock().unwrap().get(path)
        && *m == modified
    {
        return Ok(ids.clone());
    }
    let quiet: dre_protocol::host::LogSink = std::sync::Arc::new(|_, _| {});
    let p = dre_protocol::host::PluginProcess::start(path, quiet)
        .map_err(|e| format!("can't ask {} what it provides: {e}", path.display()))?;
    let ids = p.info().provides.clone();
    let _ = p.close();
    ASKED
        .lock()
        .unwrap()
        .insert(path.to_path_buf(), (modified, ids.clone()));
    Ok(ids)
}

/// Every package under `dir`, flat ones first, then versioned ones in version order.
pub fn discover(dir: &Path) -> Vec<InstalledPackage> {
    scan(dir, None)
}

/// [`discover`], limited to the package `only` when given (so no other executable is started).
fn scan(dir: &Path, only: Option<&str>) -> Vec<InstalledPackage> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for p in &entries {
        let file = p.file_name().unwrap_or_default().to_string_lossy();
        let name =
            parse_package_executable_name(&file).or_else(|| parse_executable_name(&file).map(|(_, n)| n));
        if let Some(name) = name
            && only.is_none_or(|o| o == name)
            && is_executable(p)
        {
            out.push(InstalledPackage {
                name,
                version: None,
                path: p.clone(),
                provides: probe(p).unwrap_or_default(),
            });
        }
    }
    for pdir in entries.iter().filter(|p| p.is_dir()) {
        let name = pdir.file_name().unwrap_or_default().to_string_lossy().to_string();
        if !dre_protocol::valid_name(&name) || only.is_some_and(|o| o != name) {
            continue;
        }
        let Ok(versions) = std::fs::read_dir(pdir) else {
            continue;
        };
        let mut found: Vec<(Version, InstalledPackage)> = versions
            .filter_map(Result::ok)
            .filter_map(|v| {
                let ver = Version::parse(&v.file_name().to_string_lossy()).ok()?;
                let m = read_manifest(&v.path())?;
                let exe = v.path().join(&m.executable);
                is_executable(&exe).then(|| {
                    (
                        ver.clone(),
                        InstalledPackage {
                            name: name.clone(),
                            version: Some(ver),
                            path: exe,
                            provides: m.provides,
                        },
                    )
                })
            })
            .collect();
        found.sort_by(|a, b| a.0.cmp(&b.0));
        out.extend(found.into_iter().map(|(_, p)| p));
    }
    out
}

/// The installed copies of package `name` to use: an exact `pin` if given, otherwise the highest
/// installed version matching `req`; failing both, the flat (hand-placed) executables of that
/// name. Several only when a package is placed flat as one `dre-<kind>-<name>` per plugin.
pub fn find(
    dir: &Path,
    name: &str,
    req: Option<&VersionReq>,
    pin: Option<&Version>,
) -> Vec<InstalledPackage> {
    let all = scan(dir, Some(name));
    let flat = || {
        all.iter()
            .filter(|p| p.version.is_none())
            .cloned()
            .collect::<Vec<_>>()
    };
    if let Some(pin) = pin {
        if let Some(p) = all.iter().find(|p| p.version.as_ref() == Some(pin)) {
            return vec![p.clone()];
        }
        return flat();
    }
    let versioned: Vec<(&InstalledPackage, &Version)> = all
        .iter()
        .filter_map(|p| p.version.as_ref().map(|v| (p, v)))
        .collect();
    match crate::manager::prefer_stable(versioned, req.unwrap_or(&VersionReq::STAR), |(_, v)| v) {
        Some((p, _)) => vec![p.clone()],
        None => flat(),
    }
}

/// Where a versioned install of a package lives.
pub fn version_dir(dir: &Path, package: &str, version: &Version) -> PathBuf {
    dir.join(package).join(version.to_string())
}

/// The executable file name a package installs under when its archive holds a single plugin's
/// `dre-<kind>-<name>` (a plugin built before packages).
pub fn legacy_executable(provides: &[PluginId]) -> Option<String> {
    match provides {
        [one] => Some(executable_name(one.kind, &one.name)),
        _ => None,
    }
}

/// The executable to start for one plugin, and the plugin to ask it for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Located {
    pub path: PathBuf,
    pub plugin: PluginId,
    /// The package it comes from.
    pub package: String,
}

impl Located {
    /// Start the plugin (in `cwd`, the project directory) and complete the handshake.
    pub fn start(
        &self,
        log: dre_protocol::host::LogSink,
        cwd: Option<&Path>,
    ) -> dre_protocol::host::Result<dre_protocol::host::PluginProcess> {
        dre_protocol::host::PluginProcess::start_for(&self.path, Some(&self.plugin), log, cwd)
    }
}

/// The installed copies of a declared package, honouring its `dre.lock` pin and constraint;
/// `Err` for a `local:` package that can't be used.
fn installed(
    root: &Path,
    lock: &crate::lock::Lock,
    req: &crate::project::PluginRequirement,
) -> Result<Vec<InstalledPackage>, String> {
    use crate::project::PluginSource;
    if let PluginSource::Local(p) = &req.source {
        let path = root.join(p);
        if !path.is_file() {
            return Err(format!(
                "the plugin package `{}` is declared `local: {p}`, but there's no file at {}",
                req.name,
                path.display()
            ));
        }
        return Ok(vec![InstalledPackage {
            name: req.name.clone(),
            version: None,
            provides: probe(&path)?,
            path,
        }]);
    }
    let pin = lock
        .get(&req.name)
        .filter(|l| l.from == req.source.lock_key())
        .map(|l| l.version.clone());
    let dir = plugins_dir(Some(root));
    Ok(find(&dir, &req.name, Some(&req.req()), pin.as_ref()))
}

/// Why [`locate`] found nothing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocateError {
    /// No declared package provides the plugin (an `undeclared-plugin` error elsewhere).
    NotProvided(String),
    /// A declared package might, but isn't installed or can't be used.
    Unavailable(String),
}

impl std::fmt::Display for LocateError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LocateError::NotProvided(m) | LocateError::Unavailable(m) => f.write_str(m),
        }
    }
}

impl From<LocateError> for String {
    fn from(e: LocateError) -> String {
        e.to_string()
    }
}

/// Find the plugin `id` among the project's declared packages.
pub fn locate(project: &crate::project::Project, id: &PluginId) -> Result<Located, LocateError> {
    locate_in(&project.root, &project.plugins, id)
}

/// [`locate`], given the project's root and declared packages.
pub fn locate_in(
    root: &Path,
    plugins: &[crate::project::PluginRequirement],
    id: &PluginId,
) -> Result<Located, LocateError> {
    let lock = crate::lock::Lock::load(root).unwrap_or_default();
    let mut missing = Vec::new();
    for req in plugins {
        let found = installed(root, &lock, req).map_err(LocateError::Unavailable)?;
        if found.is_empty() {
            missing.push(format!("`{}`", req.name));
        }
        if let Some(p) = found.into_iter().find(|p| p.provides(id)) {
            return Ok(Located {
                path: p.path,
                plugin: id.clone(),
                package: req.name.clone(),
            });
        }
    }
    let what = format!("the {} plugin `{}`", id.kind, id.name);
    Err(if missing.is_empty() {
        LocateError::NotProvided(format!("no plugin package the project declares provides {what}"))
    } else {
        LocateError::Unavailable(format!(
            "{what} isn't installed: the plugin package{} {} {} (looked in {}); run `dre deps` to install the project's plugins",
            if missing.len() == 1 { "" } else { "s" },
            missing.join(", "),
            if missing.len() == 1 {
                "isn't installed"
            } else {
                "aren't installed"
            },
            plugins_dir(Some(root)).display()
        ))
    })
}

/// Check that a declared package provides every plugin the project uses: from the installed
/// packages, else from `dre.lock`. A package that's neither installed nor locked is already an
/// install error, so plugins it might provide aren't reported again.
pub fn check_uses(project: &crate::project::Project, diags: &mut crate::Diagnostics) {
    let lock = crate::lock::Lock::load(&project.root).unwrap_or_default();
    let mut known: Vec<PluginId> = Vec::new();
    // A package whose declarations conflict is already an error.
    let mut unknown = project.plugins_incomplete;
    for req in &project.plugins {
        let found = installed(&project.root, &lock, req).unwrap_or_default();
        let from_lock = lock
            .get(&req.name)
            .filter(|l| l.from == req.source.lock_key())
            .map(|l| l.provides.clone())
            .unwrap_or_default();
        if found.is_empty() && from_lock.is_empty() {
            unknown = true;
        }
        known.extend(found.into_iter().flat_map(|p| p.provides));
        known.extend(from_lock);
    }
    for u in &project.plugin_uses {
        if known.contains(&u.plugin) || unknown {
            continue;
        }
        let PluginId { kind, name } = &u.plugin;
        diags.push(crate::Diagnostic {
            severity: crate::Severity::Error,
            code: "undeclared-plugin",
            message: format!(
                "{}, but no plugin package the project declares provides it — add the package with the `{name}` {kind} under `plugins:` in dependencies.yml, then run `dre deps`",
                u.what
            ),
            file: u.file.clone(),
            line: u.line,
            plugin: Some((*kind, name.clone())),
        });
    }
}
