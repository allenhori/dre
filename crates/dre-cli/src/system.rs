//! `dre system`: commands about DRE itself rather than a project. `dre system update` updates
//! DRE the way this copy was installed: a direct install (a release archive, install.sh) replaces
//! its own binary after checking it against the release's SHA256SUMS; a package manager's
//! install is left alone, with that manager's command printed; anything else is refused.
//!
//! This is the only command that checks for a newer DRE: nothing else calls the network for it.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::{Args, Subcommand};
use dre_core::manager::{self, Release};
use semver::Version;
use serde::Deserialize;

use crate::output;

const REPO: &str = "allenhori/dre";
/// The install receipt shipped next to the binary: how this copy is distributed.
pub const RECEIPT: &str = "dre-receipt.json";

#[derive(Subcommand)]
pub enum SystemCommand {
    /// Update DRE to the newest release (or VERSION), the way it was installed.
    Update(UpdateArgs),
}

#[derive(Args)]
pub struct UpdateArgs {
    /// The release to install, e.g. `0.0.1-alpha-12` or `v0.0.1-alpha-12` (to pin or roll back).
    version: Option<String>,
    /// Only report whether an update exists; change nothing.
    #[arg(long)]
    check: bool,
}

/// How this `dre` was installed.
#[derive(Debug, PartialEq)]
enum Install {
    /// A release archive (install.sh or unpacked by hand): DRE replaces its own binary.
    Direct,
    Pip {
        python: PathBuf,
    },
    UvTool,
    Pipx,
    Homebrew,
    Scoop,
    Unknown,
}

impl Install {
    /// A package manager's name and the command that updates DRE through it: to the newest
    /// version, or to `pinned`.
    fn manager(&self, pinned: Option<&Version>) -> Option<(&'static str, String)> {
        let pin = pinned.map(pep440);
        Some(match (self, pin) {
            (Install::Pip { python }, None) => {
                ("pip", format!("{} -m pip install -U dre-cli", python.display()))
            }
            (Install::Pip { python }, Some(v)) => {
                ("pip", format!("{} -m pip install dre-cli=={v}", python.display()))
            }
            (Install::UvTool, None) => ("uv", "uv tool upgrade dre-cli".into()),
            (Install::UvTool, Some(v)) => ("uv", format!("uv tool install --force dre-cli=={v}")),
            (Install::Pipx, None) => ("pipx", "pipx upgrade dre-cli".into()),
            (Install::Pipx, Some(v)) => ("pipx", format!("pipx install --force dre-cli=={v}")),
            (Install::Homebrew, None) => ("Homebrew", "brew upgrade dre".into()),
            (Install::Homebrew, Some(_)) => (
                "Homebrew",
                "brew upgrade dre (Homebrew installs only the formula's current version)".into(),
            ),
            (Install::Scoop, None) => ("Scoop", "scoop update dre".into()),
            (Install::Scoop, Some(_)) => ("Scoop", format!("scoop install dre@{}", pinned.unwrap())),
            (Install::Direct | Install::Unknown, _) => return None,
        })
    }
}

/// The `dre-cli` (PyPI) version of a DRE version: 0.0.1-alpha-3 is 0.0.1a3.
fn pep440(v: &Version) -> String {
    let base = format!("{}.{}.{}", v.major, v.minor, v.patch);
    let pre = v.pre.as_str();
    for (word, short) in [("alpha", "a"), ("beta", "b"), ("rc", "rc")] {
        if let Some(rest) = pre.strip_prefix(word) {
            let n = rest.trim_start_matches(['-', '.']);
            return format!("{base}{short}{}", if n.is_empty() { "0" } else { n });
        }
    }
    base
}

#[derive(Deserialize)]
struct Receipt {
    #[serde(default)]
    schema: u64,
    #[serde(default)]
    source: String,
}

pub fn update(a: UpdateArgs, printer: &output::Printer) -> ExitCode {
    let exe = match std::env::current_exe().and_then(|p| p.canonicalize()) {
        Ok(p) => p,
        Err(e) => {
            printer.error(&format!("can't find the running dre: {e}"));
            return ExitCode::FAILURE;
        }
    };
    let install = detect(&exe);
    if install == Install::Unknown {
        printer.error(&format!(
            "can't update this dre ({}): it wasn't installed from a DRE release (a `cargo build` or `cargo install`, or a copied binary), so it isn't replaced.\n  \
             Install a copy that updates, one of:\n    \
             curl -fsSL https://raw.githubusercontent.com/{REPO}/master/install.sh | sh\n    \
             pip install dre-cli   (or: uv tool install dre-cli, pipx install dre-cli)\n    \
             on Windows, unpack dre-<version>-windows-x86_64.zip from https://github.com/{REPO}/releases",
            exe.display()
        ));
        return ExitCode::FAILURE;
    }
    let current = current_version();
    let releases = match manager::github_releases(REPO) {
        Ok(r) => r,
        Err(e) => {
            printer.error(&format!(
                "can't reach GitHub Releases to check for a newer dre: {e}. Nothing was changed."
            ));
            return ExitCode::FAILURE;
        }
    };
    let wanted = match &a.version {
        Some(v) => {
            let v = v.trim_start_matches('v');
            match releases.iter().find(|r| r.version.to_string() == v) {
                Some(r) => r,
                None => {
                    printer.error(&format!(
                        "there's no dre release {v}; see https://github.com/{REPO}/releases"
                    ));
                    return ExitCode::FAILURE;
                }
            }
        }
        None => match latest(&releases, &current) {
            Some(r) => r,
            None => {
                printer.error(&format!("no release of {REPO} found on GitHub Releases"));
                return ExitCode::FAILURE;
            }
        },
    };
    let newer = manager::version_order(&wanted.version, &current) == std::cmp::Ordering::Greater;
    let same = wanted.version == current;
    if let Some((name, command)) = install.manager(a.version.as_ref().map(|_| &wanted.version)) {
        // Newer than every release (a build of the next one): nothing to point at.
        let same = same || (!newer && a.version.is_none());
        if same {
            println!(
                "{} (installed with {name})",
                up_to_date(&current, a.version.is_some())
            );
        } else {
            println!(
                "dre is installed with {name}. {}: {current} → {}. Update it with:\n  {command}",
                if newer { "Update available" } else { "Available" },
                wanted.version
            );
            if matches!(install, Install::Pip { .. } | Install::UvTool | Install::Pipx) {
                println!(
                    "Upgrading dre-cli also updates the plugins bundled with it. If dre-cli is pinned in a project's or Databricks job's dependencies, bump the pin there too."
                );
            }
        }
        return if a.check || same {
            ExitCode::SUCCESS
        } else {
            ExitCode::FAILURE
        };
    }
    // A direct install.
    if same {
        println!("{}", up_to_date(&current, a.version.is_some()));
        return ExitCode::SUCCESS;
    }
    if a.check {
        if newer || a.version.is_some() {
            println!(
                "{}: {current} → {}, run `dre system update{}`",
                if newer { "Update available" } else { "Available" },
                wanted.version,
                a.version.as_ref().map(|v| format!(" {v}")).unwrap_or_default()
            );
        } else {
            println!("dre {current} is the latest version");
        }
        return ExitCode::SUCCESS;
    }
    if !newer && a.version.is_none() {
        // Running something newer than any release (a pre-release build of the next one).
        println!("dre {current} is the latest version");
        return ExitCode::SUCCESS;
    }
    match replace(&exe, wanted) {
        Ok(()) => {
            let verb = if newer { "Updated" } else { "Downgraded" };
            println!("{verb} dre from {current} to {}", wanted.version);
            println!(
                "Release notes: https://github.com/{REPO}/releases/tag/{}",
                wanted.tag
            );
            println!(
                "Standalone plugins aren't updated; run `dre plugin update <plugin>` in a project to update them."
            );
            ExitCode::SUCCESS
        }
        Err(e) => {
            printer.error(&format!("{e}. Your existing dre ({current}) is untouched."));
            ExitCode::FAILURE
        }
    }
}

fn up_to_date(current: &Version, pinned: bool) -> String {
    if pinned {
        format!("dre {current} is already installed")
    } else {
        format!("dre {current} is the latest version")
    }
}

/// The running version. Debug builds (tests) can pretend to be another one.
fn current_version() -> Version {
    #[cfg(debug_assertions)]
    if let Ok(v) = std::env::var("DRE_TEST_CURRENT_VERSION")
        && let Ok(v) = Version::parse(&v)
    {
        return v;
    }
    Version::parse(dre_core::version()).unwrap_or_else(|_| Version::new(0, 0, 0))
}

/// The newest stable release, unless there's none or this dre is a pre-release; otherwise the
/// newest release, pre-releases included.
fn latest<'a>(releases: &'a [Release], current: &Version) -> Option<&'a Release> {
    let newest = |stable_only: bool| {
        releases
            .iter()
            .filter(|r| !stable_only || r.version.pre.is_empty())
            .max_by(|a, b| manager::version_order(&a.version, &b.version))
    };
    if current.pre.is_empty() {
        newest(true).or_else(|| newest(false))
    } else {
        newest(false)
    }
}

// -- detection --------------------------------------------------------------------------------

fn detect(exe: &Path) -> Install {
    // Package managers' trees come first: a formula or manifest built from a release archive
    // still carries that archive's receipt.
    let parts: Vec<String> = exe
        .components()
        .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
        .collect();
    let has = |seq: &[&str]| {
        parts
            .windows(seq.len())
            .any(|w| w.iter().zip(seq).all(|(a, b)| a == b))
    };
    let under_env = |var: &str| {
        std::env::var_os(var)
            .filter(|v| !v.is_empty())
            .and_then(|v| PathBuf::from(v).canonicalize().ok())
            .is_some_and(|root| exe.starts_with(root))
    };
    if exe.components().any(|c| c.as_os_str() == "Cellar") {
        return Install::Homebrew;
    }
    if has(&["scoop", "apps"]) || under_env("SCOOP") || under_env("SCOOP_GLOBAL") {
        return Install::Scoop;
    }
    let receipt = std::fs::read(exe.with_file_name(RECEIPT))
        .ok()
        .and_then(|b| serde_json::from_slice::<Receipt>(&b).ok());
    match receipt {
        Some(r) if r.schema >= 1 && r.source == "release_archive" => Install::Direct,
        Some(r) if r.schema >= 1 && r.source == "pypi" => {
            if has(&["uv", "tools"]) || under_env("UV_TOOL_DIR") {
                Install::UvTool
            } else if has(&["pipx", "venvs"]) || under_env("PIPX_HOME") {
                Install::Pipx
            } else {
                Install::Pip {
                    python: python_of(exe),
                }
            }
        }
        _ => Install::Unknown,
    }
}

/// The interpreter of the Python environment holding `exe` (`<env>/lib/pythonX.Y/site-packages/
/// dre_cli/bin/dre`, or `<env>\Lib\site-packages\...` on Windows); plain `python` if unclear.
fn python_of(exe: &Path) -> PathBuf {
    let site = exe.ancestors().find(|a| {
        a.file_name().is_some_and(|n| {
            n.eq_ignore_ascii_case("site-packages") || n.eq_ignore_ascii_case("dist-packages")
        })
    });
    let env = site.and_then(|s| {
        let lib = s.parent()?;
        let lib = if lib.file_name()?.to_string_lossy().starts_with("python") {
            lib.parent()?
        } else {
            lib
        };
        lib.parent()
    });
    let python = match env {
        Some(env) if cfg!(windows) => env.join("Scripts").join("python.exe"),
        Some(env) => env.join("bin").join("python"),
        None => PathBuf::new(),
    };
    // A `pip install --user` or system install has no interpreter of its own there.
    if python.is_file() {
        python
    } else if cfg!(windows) {
        PathBuf::from("python")
    } else {
        PathBuf::from("python3")
    }
}

// -- replacing the binary ---------------------------------------------------------------------

/// Download `release`'s archive for this platform, check it against SHA256SUMS, and swap the
/// binary (and its receipt) in. Nothing on disk changes before the final swap.
fn replace(exe: &Path, release: &Release) -> Result<(), String> {
    let windows = cfg!(windows);
    let archive = format!(
        "dre-{}-{}.{}",
        release.version,
        manager::platform(),
        if windows { "zip" } else { "tar.gz" }
    );
    let asset = |name: &str| {
        release
            .assets
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, u)| u.clone())
            .ok_or_else(|| format!("release {} has no {name}", release.tag))
    };
    let sums = manager::fetch(&asset("SHA256SUMS")?).map_err(|e| format!("the update failed: {e}"))?;
    let sums = String::from_utf8_lossy(&sums);
    let want = sums
        .lines()
        .filter_map(|l| l.split_once(char::is_whitespace))
        .find(|(_, f)| f.trim().trim_start_matches('*') == archive)
        .map(|(h, _)| h.trim().to_lowercase())
        .ok_or_else(|| {
            format!(
                "the update failed: SHA256SUMS of {} has no entry for {archive}",
                release.tag
            )
        })?;
    let bytes = manager::fetch(&asset(&archive)?).map_err(|e| format!("the update failed: {e}"))?;
    let got = {
        use sha2::Digest;
        dre_core::manifest::hex(&sha2::Sha256::digest(&bytes))
    };
    if got != want {
        return Err(format!(
            "the update failed: {archive} doesn't match its SHA256SUMS entry (expected {want}, got {got})"
        ));
    }
    let exe_name = if windows { "dre.exe" } else { "dre" };
    let (binary, receipt) = if windows {
        from_zip(&bytes, exe_name)?
    } else {
        from_tar_gz(&bytes, exe_name)?
    };
    let dir = exe.parent().ok_or("the running dre has no directory")?;
    swap(dir, exe, &binary, receipt.as_deref())
}

type Unpacked = (Vec<u8>, Option<Vec<u8>>);

fn from_tar_gz(bytes: &[u8], exe_name: &str) -> Result<Unpacked, String> {
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(bytes));
    let (mut binary, mut receipt) = (None, None);
    for entry in archive
        .entries()
        .map_err(|e| format!("the update failed: bad archive: {e}"))?
    {
        let mut entry = entry.map_err(|e| format!("the update failed: bad archive: {e}"))?;
        let name = entry
            .path()
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        let mut data = Vec::new();
        if name == exe_name || name == RECEIPT {
            entry
                .read_to_end(&mut data)
                .map_err(|e| format!("the update failed: bad archive: {e}"))?;
        }
        if name == exe_name {
            binary = Some(data);
        } else if name == RECEIPT {
            receipt = Some(data);
        }
    }
    Ok((
        binary.ok_or_else(|| format!("the update failed: the archive has no {exe_name}"))?,
        receipt,
    ))
}

fn from_zip(bytes: &[u8], exe_name: &str) -> Result<Unpacked, String> {
    let mut z = zip::ZipArchive::new(std::io::Cursor::new(bytes))
        .map_err(|e| format!("the update failed: bad archive: {e}"))?;
    let mut read = |name: &str| -> Option<Vec<u8>> {
        let idx = (0..z.len()).find(|&i| {
            z.by_index(i)
                .ok()
                .and_then(|f| f.enclosed_name())
                .and_then(|p| p.file_name().map(|n| n == name))
                .unwrap_or(false)
        })?;
        let mut data = Vec::new();
        z.by_index(idx).ok()?.read_to_end(&mut data).ok()?;
        Some(data)
    };
    let binary = read(exe_name).ok_or_else(|| format!("the update failed: the archive has no {exe_name}"))?;
    Ok((binary, read(RECEIPT)))
}

/// Write the new binary next to `exe`, then rename it into place (on Windows, move the running
/// exe aside first, and put it back if that fails).
fn swap(dir: &Path, exe: &Path, binary: &[u8], receipt: Option<&[u8]>) -> Result<(), String> {
    let tmp = dir.join(format!(".dre-update-{}.tmp", std::process::id()));
    let denied = |e: &std::io::Error| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            format!(
                "the update failed: can't write to {} (permission denied); re-run with the permissions needed to change it (for example with sudo)",
                dir.display()
            )
        } else {
            format!("the update failed: can't write to {}: {e}", dir.display())
        }
    };
    std::fs::write(&tmp, binary).map_err(|e| denied(&e))?;
    let cleanup = |e: String| {
        let _ = std::fs::remove_file(&tmp);
        e
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o755))
            .map_err(|e| cleanup(denied(&e)))?;
    }
    if cfg!(windows) {
        let old = exe.with_extension("old");
        let _ = std::fs::remove_file(&old);
        std::fs::rename(exe, &old).map_err(|e| cleanup(denied(&e)))?;
        if let Err(e) = std::fs::rename(&tmp, exe) {
            let _ = std::fs::rename(&old, exe);
            return Err(cleanup(denied(&e)));
        }
        let _ = std::fs::remove_file(&old);
    } else {
        std::fs::rename(&tmp, exe).map_err(|e| cleanup(denied(&e)))?;
    }
    if let Some(r) = receipt {
        let _ = std::fs::write(exe.with_file_name(RECEIPT), r);
    }
    Ok(())
}
