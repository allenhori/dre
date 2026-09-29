//! The target path: the folder DRE writes its generated files to (compiled SQL, run outputs,
//! schema snapshots, `run_results.json`, the manifest). `target/` in the project root unless
//! set, highest first, by `--target-path`, `DRE_TARGET_PATH` or `target_path:` in
//! `dre_project.yml`. It's always a local (or mounted) path, never inside the project's sources.
//!
//! Unrelated to a profile's `target` (the environment a connection uses).

use std::fmt;
use std::path::{Component, Path, PathBuf};

use crate::lookups::LOOKUPS_DIR;
use crate::project::{MACROS_DIR, PROJECT_FILE, REPORTS_DIR, TARGET_DIR};

pub const ENV: &str = "DRE_TARGET_PATH";
/// The project-file key.
pub const KEY: &str = "target_path";
/// Dropped in every target folder DRE creates, so `dre clean` only deletes DRE's own folders.
pub const MARKER: &str = ".dre_target";

/// Where the target path was set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Flag,
    Env,
    Project,
    Default,
}

impl fmt::Display for Source {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Source::Flag => "--target-path",
            Source::Env => ENV,
            Source::Project => "`target_path` in dre_project.yml",
            Source::Default => "the default target path",
        })
    }
}

/// A resolved target path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPath {
    /// Absolute, or the root joined with the relative value.
    pub dir: PathBuf,
    pub source: Source,
}

impl TargetPath {
    pub fn default_for(root: &Path) -> TargetPath {
        TargetPath {
            dir: root.join(TARGET_DIR),
            source: Source::Default,
        }
    }
}

/// Resolve the target path for the project at `root`: `flag`, else `DRE_TARGET_PATH`, else the
/// project file's `target_path` (`project_value`), else `target/`. Checks that it's local and
/// clear of the project's sources.
pub fn resolve(root: &Path, flag: Option<&str>, project_value: Option<&str>) -> Result<TargetPath, String> {
    let env = std::env::var(ENV).ok();
    let (value, source) = match (flag, env.as_deref(), project_value) {
        (Some(v), _, _) => (v, Source::Flag),
        (None, Some(v), _) if !v.is_empty() => (v, Source::Env),
        (None, _, Some(v)) => (v, Source::Project),
        _ => return Ok(TargetPath::default_for(root)),
    };
    let fail = |why: String| format!("{source}: target path `{value}` {why}");
    if value.trim().is_empty() {
        return Err(fail("is empty".into()));
    }
    if looks_like_url(value) {
        return Err(fail(
            "is a URL; the target path must be a local or mounted path (a Databricks Volume under /Volumes/..., an NFS/EFS share, a gcsfuse mount). To copy outputs to object storage, deliver them with a destination".into(),
        ));
    }
    let expanded = expand_home(value);
    let dir = clean(&if expanded.is_absolute() {
        expanded
    } else {
        root.join(expanded)
    });
    check(root, &dir).map_err(fail)?;
    Ok(TargetPath { dir, source })
}

/// `scheme://…`, but not a Windows drive (`C:\`, `C:/`).
fn looks_like_url(v: &str) -> bool {
    let Some((scheme, _)) = v.split_once("://") else {
        return false;
    };
    scheme.len() > 1
        && scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        && scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "+-.".contains(c))
}

fn expand_home(v: &str) -> PathBuf {
    let home = || std::env::home_dir().unwrap_or_default();
    if v == "~" {
        return home();
    }
    match v.strip_prefix("~/").or_else(|| v.strip_prefix("~\\")) {
        Some(rest) => home().join(rest),
        None => PathBuf::from(v),
    }
}

/// `p` with `.` and `..` removed lexically.
fn clean(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push(c);
                }
            }
            c => out.push(c),
        }
    }
    out
}

/// An absolute, symlink-resolved form of `p` for comparisons, whether or not it exists yet: the
/// deepest existing ancestor is canonicalised and the rest appended.
fn canonical(p: &Path) -> PathBuf {
    let abs = clean(&std::path::absolute(p).unwrap_or_else(|_| p.to_path_buf()));
    let mut existing = abs.as_path();
    let mut rest = Vec::new();
    loop {
        if let Ok(c) = existing.canonicalize() {
            let mut out = strip_verbatim(c);
            for r in rest.iter().rev() {
                out.push(r);
            }
            return out;
        }
        match (existing.parent(), existing.file_name()) {
            (Some(parent), Some(name)) => {
                rest.push(name.to_os_string());
                existing = parent;
            }
            _ => return abs,
        }
    }
}

/// Windows' `canonicalize` returns `\\?\C:\…`; compare without the prefix.
fn strip_verbatim(p: PathBuf) -> PathBuf {
    let s = p.to_string_lossy();
    match s.strip_prefix(r"\\?\UNC\") {
        Some(rest) => PathBuf::from(format!(r"\\{rest}")),
        None => match s.strip_prefix(r"\\?\") {
            Some(rest) => PathBuf::from(rest),
            None => p,
        },
    }
}

/// Refuse the project root, a folder containing it, and anything inside the project's sources
/// or its plugins.
fn check(root: &Path, dir: &Path) -> Result<(), String> {
    let (root, dir) = (canonical(root), canonical(dir));
    if dir == root {
        return Err("is the project root; generated files can't mix with the project's sources".into());
    }
    if root.starts_with(&dir) {
        return Err(
            "contains the project; pick a folder outside it, or one inside it such as `target`".into(),
        );
    }
    for inner in [REPORTS_DIR, MACROS_DIR, LOOKUPS_DIR, crate::plugins::DEPS_DIR] {
        if dir.starts_with(root.join(inner)) {
            return Err(format!(
                "is inside `{inner}/`; generated files there would be read as project files"
            ));
        }
    }
    Ok(())
}

/// The target folder relative to the root when it's inside the project (so project scanning
/// can skip it), comparing resolved paths.
pub fn inside(root: &Path, dir: &Path) -> Option<PathBuf> {
    canonical(dir)
        .strip_prefix(canonical(root))
        .ok()
        .map(Path::to_path_buf)
}

/// `target_path:` from `dre_project.yml`, read on its own (for `dre clean`, which doesn't load
/// the project). A missing or unreadable file, or no key, is `None`.
pub fn project_value(root: &Path) -> Option<String> {
    let text = std::fs::read_to_string(root.join(PROJECT_FILE)).ok()?;
    let v: serde_yaml_ng::Value = serde_yaml_ng::from_str(&text).ok()?;
    v.get(KEY)?.as_str().map(str::to_string)
}

/// Create the target folder (with its parents) if needed. Only a folder DRE creates gets the
/// ownership marker that lets `dre clean` delete it.
pub fn ensure(dir: &Path) -> std::io::Result<()> {
    let fresh = !dir.exists();
    std::fs::create_dir_all(dir)?;
    if fresh {
        std::fs::write(
            dir.join(MARKER),
            "DRE writes its generated files here; `dre clean` may delete this folder.\n",
        )?;
    }
    Ok(())
}

/// What `dre clean` did.
#[derive(Debug, PartialEq, Eq)]
pub enum Cleaned {
    Removed,
    Missing,
}

/// `dre clean`: delete the target folder, but only one DRE made (it has the marker) or the
/// default `target/` of the project (which may predate the marker).
pub fn clean_dir(root: &Path, t: &TargetPath) -> Result<Cleaned, String> {
    if !t.dir.exists() {
        return Ok(Cleaned::Missing);
    }
    let default = canonical(&t.dir) == canonical(&root.join(TARGET_DIR));
    if !default && !t.dir.join(MARKER).is_file() {
        return Err(format!(
            "refusing to delete {} (from {}): DRE didn't create it (it has no {MARKER} file). Check the path, or delete the folder yourself",
            t.dir.display(),
            t.source
        ));
    }
    std::fs::remove_dir_all(&t.dir).map_err(|e| format!("can't remove {}: {e}", t.dir.display()))?;
    Ok(Cleaned::Removed)
}
