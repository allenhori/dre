//! Finding installed plugin executables.
//!
//! Plugins live in `DRE_PLUGINS_DIR` (default `~/.dre/plugins`), either side by side by version,
//! `<dir>/<kind>/<name>/<version>/dre-<kind>-<name>`, as installed by the plugin manager, or flat,
//! `<dir>/dre-<kind>-<name>`, for plugins placed by hand (development, tests).

use std::path::{Path, PathBuf};

use dre_protocol::{Kind, executable_name, parse_executable_name};
use semver::{Version, VersionReq};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledPlugin {
    pub kind: Kind,
    pub name: String,
    /// Known for versioned installs; flat plugins report theirs in the handshake.
    pub version: Option<Version>,
    pub path: PathBuf,
}

pub fn plugins_dir() -> PathBuf {
    match std::env::var_os("DRE_PLUGINS_DIR").filter(|p| !p.is_empty()) {
        Some(p) => PathBuf::from(p),
        None => crate::dre_home().join("plugins"),
    }
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

/// Every plugin under `dir`, flat ones first, then versioned ones in version order.
pub fn discover(dir: &Path) -> Vec<InstalledPlugin> {
    let mut out = Vec::new();
    let Ok(entries) = std::fs::read_dir(dir) else {
        return out;
    };
    let mut entries: Vec<_> = entries.filter_map(Result::ok).map(|e| e.path()).collect();
    entries.sort();
    for p in &entries {
        let file = p.file_name().unwrap_or_default().to_string_lossy();
        if let Some((kind, name)) = parse_executable_name(&file)
            && is_executable(p)
        {
            out.push(InstalledPlugin {
                kind,
                name,
                version: None,
                path: p.clone(),
            });
        }
    }
    for kind in [Kind::Source, Kind::Format, Kind::Destination] {
        let kdir = dir.join(kind.as_str());
        let Ok(names) = std::fs::read_dir(&kdir) else {
            continue;
        };
        let mut names: Vec<_> = names.filter_map(Result::ok).map(|e| e.path()).collect();
        names.sort();
        for ndir in names {
            let name = ndir.file_name().unwrap_or_default().to_string_lossy().to_string();
            let Ok(versions) = std::fs::read_dir(&ndir) else {
                continue;
            };
            let mut found: Vec<(Version, PathBuf)> = versions
                .filter_map(Result::ok)
                .filter_map(|v| {
                    let ver = Version::parse(&v.file_name().to_string_lossy()).ok()?;
                    let exe = v.path().join(executable_name(kind, &name));
                    is_executable(&exe).then_some((ver, exe))
                })
                .collect();
            found.sort();
            for (ver, exe) in found {
                out.push(InstalledPlugin {
                    kind,
                    name: name.clone(),
                    version: Some(ver),
                    path: exe,
                });
            }
        }
    }
    out
}

/// Pick the plugin to run: an exact `pin` if given, otherwise the highest installed version
/// matching `req`, otherwise a flat (hand-placed) plugin of that kind and name.
pub fn find(
    dir: &Path,
    kind: Kind,
    name: &str,
    req: Option<&VersionReq>,
    pin: Option<&Version>,
) -> Option<InstalledPlugin> {
    let all: Vec<InstalledPlugin> = discover(dir)
        .into_iter()
        .filter(|p| p.kind == kind && p.name == name)
        .collect();
    if let Some(pin) = pin {
        if let Some(p) = all.iter().find(|p| p.version.as_ref() == Some(pin)) {
            return Some(p.clone());
        }
        return all.into_iter().find(|p| p.version.is_none());
    }
    let versioned = all
        .iter()
        .filter(|p| {
            p.version
                .as_ref()
                .is_some_and(|v| req.is_none_or(|r| r.matches(v)))
        })
        .max_by(|a, b| a.version.cmp(&b.version));
    versioned
        .cloned()
        .or_else(|| all.into_iter().find(|p| p.version.is_none()))
}
