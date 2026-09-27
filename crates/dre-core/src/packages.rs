//! Macro packages: reusable macros from another project (e.g. `dre_utils`), declared under
//! `packages:` in the project's root `dependencies.yml` or `packages.yml`:
//!
//! ```yaml
//! packages:
//!   - git: https://github.com/acme/dre-finance-macros.git
//!     revision: v2.3.0          # tag, branch or commit
//!   - local: ../shared/macros   # a folder on disk, read in place
//! ```
//!
//! A package is a folder with a `dre_package.yml` (`name:`) and a `macros/` folder. `dre deps`
//! clones git packages into `dre_deps/packages/<name>` and pins the commit in `dre.lock`. Their
//! macros are called through the package's name: `{{ dre_utils.star_except(...) }}`.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};
use serde_yaml_ng::Value;

use crate::diag::Diagnostics;
use crate::lock::{Lock, LockedPackage};
use crate::plugins::DEPS_DIR;
use crate::yaml::YamlFile;

/// Root files that may declare `packages:` (and plugins, like any YAML file).
pub const DEPENDENCY_FILES: &[&str] = &["dependencies.yml", "packages.yml"];
pub const PACKAGE_FILE: &str = "dre_package.yml";
const INSTALLED_COMMIT: &str = ".dre_commit";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Git { url: String, revision: String },
    Local { path: String },
}

#[derive(Debug, Clone)]
pub struct Declared {
    pub source: Source,
    /// The file declaring it, relative to the project root.
    pub file: PathBuf,
}

/// An installed package, ready to use.
#[derive(Debug, Clone, Serialize)]
pub struct Package {
    pub name: String,
    #[serde(skip)]
    pub dir: PathBuf,
    /// Macro files, absolute.
    #[serde(skip)]
    pub macros: Vec<PathBuf>,
}

#[derive(Deserialize)]
struct Manifest {
    name: String,
}

/// Every `packages:` entry in the root dependency files. Problems go to `diags`.
pub fn declared(root: &Path, diags: &mut Diagnostics) -> Vec<Declared> {
    let mut out: Vec<Declared> = Vec::new();
    for f in DEPENDENCY_FILES {
        let path = root.join(f);
        if !path.is_file() {
            continue;
        }
        let mut quiet = Diagnostics::default();
        let Some(yf) = YamlFile::load(&path, PathBuf::from(f), &mut quiet) else {
            continue; // the project load reports the parse error
        };
        let Some(list) = yf.value.get("packages") else {
            continue;
        };
        let Some(list) = list.as_sequence() else {
            diags.error(
                "invalid-packages",
                Some(yf.display.clone()),
                yf.line_of("packages", None),
                "`packages` must be a list",
            );
            continue;
        };
        for entry in list {
            let line = yf.line_of("packages", None);
            let get = |k: &str| entry.get(k).and_then(Value::as_str).map(str::to_string);
            let source = match (get("git"), get("local"), get("package")) {
                (Some(url), None, None) => match get("revision") {
                    Some(revision) => Source::Git { url, revision },
                    None => {
                        diags.error(
                            "invalid-packages",
                            Some(yf.display.clone()),
                            line,
                            format!("git package `{url}` needs a `revision` (a tag, branch or commit)"),
                        );
                        continue;
                    }
                },
                (None, Some(path), None) => Source::Local { path },
                (None, None, Some(name)) => {
                    diags.error(
                        "invalid-packages",
                        Some(yf.display.clone()),
                        line,
                        format!("`package: {name}`: registry packages aren't available yet; use `git:` or `local:`"),
                    );
                    continue;
                }
                _ => {
                    diags.error(
                        "invalid-packages",
                        Some(yf.display.clone()),
                        line,
                        "each package needs exactly one of `git:` (with `revision:`) or `local:`",
                    );
                    continue;
                }
            };
            let same_place = |a: &Source, b: &Source| match (a, b) {
                (Source::Git { url: x, .. }, Source::Git { url: y, .. }) => x == y,
                (Source::Local { path: x }, Source::Local { path: y }) => x == y,
                _ => false,
            };
            match out.iter().find(|d| same_place(&d.source, &source)) {
                Some(d) if d.source == source => {}
                Some(d) => diags.error(
                    "conflicting-packages",
                    Some(yf.display.clone()),
                    line,
                    format!(
                        "this package is also declared in {} with a different revision",
                        d.file.display()
                    ),
                ),
                None => out.push(Declared {
                    source,
                    file: PathBuf::from(f),
                }),
            }
        }
    }
    out
}

fn manifest_name(dir: &Path) -> Result<String, String> {
    let p = dir.join(PACKAGE_FILE);
    let text = std::fs::read_to_string(&p).map_err(|_| format!("{} has no {PACKAGE_FILE}", dir.display()))?;
    let m: Manifest = serde_yaml_ng::from_str(&text).map_err(|e| format!("{}: {e}", p.display()))?;
    if m.name.is_empty()
        || !m.name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        || m.name.starts_with(|c: char| c.is_ascii_digit())
    {
        return Err(format!(
            "{}: package name `{}` must be letters, digits and `_`",
            p.display(),
            m.name
        ));
    }
    Ok(m.name)
}

fn installed_dir(root: &Path, name: &str) -> PathBuf {
    root.join(DEPS_DIR).join("packages").join(name)
}

/// Install missing or outdated git packages into `dre_deps/packages/` and pin them in
/// `dre.lock`. With `install` false, report what's missing instead. Local packages need nothing.
pub fn sync(root: &Path, install: bool, mut log: impl FnMut(&str)) -> Result<(), Vec<String>> {
    let mut diags = Diagnostics::default();
    let decls = declared(root, &mut diags);
    if diags.has_errors() {
        return Ok(()); // reported with the project load
    }
    let mut lock = Lock::load(root).map_err(|e| vec![e])?;
    let mut errors = Vec::new();
    let mut changed = false;
    for d in &decls {
        let Source::Git { url, revision } = &d.source else {
            continue;
        };
        let pinned = lock
            .packages
            .iter()
            .find(|(_, l)| &l.git == url)
            .map(|(n, l)| (n.clone(), l.clone()));
        // Up to date: the lock matches the declaration and that commit is what's installed.
        if let Some((name, l)) = &pinned
            && &l.revision == revision
            && std::fs::read_to_string(installed_dir(root, name).join(INSTALLED_COMMIT))
                .is_ok_and(|c| c.trim() == l.commit)
        {
            continue;
        }
        if !install {
            errors.push(format!(
                "the package from {url} ({revision}) isn't installed; run `dre deps`"
            ));
            continue;
        }
        // Reinstall the locked commit unless the declared revision changed.
        let checkout = match &pinned {
            Some((_, l)) if &l.revision == revision => l.commit.clone(),
            _ => revision.clone(),
        };
        match install_git(root, url, &checkout) {
            Ok((name, commit)) => {
                log(&format!(
                    "package `{name}` from {url} at {revision} ({})",
                    &commit[..commit.len().min(12)]
                ));
                if let Some((old, _)) = &pinned
                    && old != &name
                {
                    lock.packages.remove(old);
                }
                let entry = LockedPackage {
                    git: url.clone(),
                    revision: revision.clone(),
                    commit,
                };
                if lock.packages.get(&name) != Some(&entry) {
                    lock.packages.insert(name, entry);
                    changed = true;
                }
            }
            Err(e) => errors.push(format!("package from {url}: {e}")),
        }
    }
    if changed && let Err(e) = lock.save(root) {
        errors.push(e);
    }
    if errors.is_empty() { Ok(()) } else { Err(errors) }
}

const GIT_MISSING: &str = "git isn't installed (needed for git packages); install git, or use a `local:` package";

fn git_works() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .is_ok_and(|o| o.status.success() && !o.stdout.is_empty())
}

fn git(args: &[&str], cwd: Option<&Path>) -> Result<String, String> {
    let mut c = Command::new("git");
    c.args(args);
    if let Some(d) = cwd {
        c.current_dir(d);
    }
    let out = c.output().map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => GIT_MISSING.to_string(),
        _ => format!("can't run git: {e}"),
    })?;
    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
        // A stub git (macOS without the command line tools) fails without saying why.
        if stderr.is_empty() && !git_works() {
            return Err(GIT_MISSING.to_string());
        }
        let reason = if stderr.is_empty() {
            format!("exited with {}", out.status)
        } else {
            stderr
        };
        return Err(format!("git {}: {reason}", args.join(" ")));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// Clone `url` at `checkout` into a temporary folder, then copy it (without `.git`) into
/// `dre_deps/packages/<name>`.
fn install_git(root: &Path, url: &str, checkout: &str) -> Result<(String, String), String> {
    let base = root.join(DEPS_DIR).join("packages");
    std::fs::create_dir_all(&base).map_err(|e| format!("can't create {}: {e}", base.display()))?;
    let tmp = std::env::temp_dir().join(format!("dre-package-{}-{}", std::process::id(), base_name(url)));
    remove_dir_force(&tmp);
    let result = (|| {
        git(&["clone", "--quiet", url, &tmp.to_string_lossy()], None)?;
        git(&["checkout", "--quiet", checkout], Some(&tmp))?;
        let commit = git(&["rev-parse", "HEAD"], Some(&tmp))?;
        let name = manifest_name(&tmp)?;
        let dst = installed_dir(root, &name);
        remove_dir_force(&dst);
        copy_tree(&tmp, &dst).map_err(|e| format!("can't install into {}: {e}", dst.display()))?;
        std::fs::write(dst.join(INSTALLED_COMMIT), &commit).map_err(|e| e.to_string())?;
        Ok((name, commit))
    })();
    remove_dir_force(&tmp);
    result
}

/// The last path segment of a URL or path, for a readable temp folder name.
fn base_name(url: &str) -> String {
    let url = url.trim_end_matches(['/', '\\']);
    url.strip_suffix(".git")
        .unwrap_or(url)
        .rsplit(['/', '\\', ':'])
        .next()
        .unwrap_or("package")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '_' || *c == '-')
        .collect()
}

/// Copy `from` into `to`, leaving out `.git`.
fn copy_tree(from: &Path, to: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(to)?;
    for e in std::fs::read_dir(from)? {
        let e = e?;
        if e.file_name() == ".git" {
            continue;
        }
        let dst = to.join(e.file_name());
        if e.file_type()?.is_dir() {
            copy_tree(&e.path(), &dst)?;
        } else {
            std::fs::copy(e.path(), dst)?;
        }
    }
    Ok(())
}

/// Remove a folder even when it holds read-only files, as git's object store does on Windows.
fn remove_dir_force(dir: &Path) {
    if !dir.exists() {
        return;
    }
    for e in walkdir::WalkDir::new(dir).into_iter().filter_map(Result::ok) {
        if let Ok(meta) = e.metadata() {
            let mut perm = meta.permissions();
            if perm.readonly() {
                #[allow(clippy::permissions_set_readonly_false)]
                perm.set_readonly(false);
                let _ = std::fs::set_permissions(e.path(), perm);
            }
        }
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// Resolve the declared packages to installed folders and their macro files.
pub fn resolve(root: &Path, decls: &[Declared], diags: &mut Diagnostics) -> Vec<Package> {
    let lock = Lock::load(root).unwrap_or_default();
    let mut out: Vec<Package> = Vec::new();
    for d in decls {
        let dir = match &d.source {
            Source::Local { path } => {
                let p = root.join(path);
                if !p.is_dir() {
                    diags.error(
                        "package-missing",
                        Some(d.file.clone()),
                        None,
                        format!("local package `{path}` doesn't exist"),
                    );
                    continue;
                }
                p
            }
            Source::Git { url, .. } => match lock.packages.iter().find(|(_, l)| &l.git == url) {
                Some((name, _)) if installed_dir(root, name).is_dir() => installed_dir(root, name),
                _ => {
                    diags.error(
                        "package-missing",
                        Some(d.file.clone()),
                        None,
                        format!("the package from {url} isn't installed; run `dre deps`"),
                    );
                    continue;
                }
            },
        };
        let name = match manifest_name(&dir) {
            Ok(n) => n,
            Err(e) => {
                diags.error("invalid-package", Some(d.file.clone()), None, e);
                continue;
            }
        };
        if let Some(other) = out.iter().find(|p| p.name == name) {
            diags.error(
                "duplicate-package",
                Some(d.file.clone()),
                None,
                format!(
                    "two packages are named `{name}` ({} and {})",
                    other.dir.display(),
                    dir.display()
                ),
            );
            continue;
        }
        let mut macros: Vec<PathBuf> = walkdir::WalkDir::new(dir.join("macros"))
            .sort_by_file_name()
            .into_iter()
            .filter_map(Result::ok)
            .map(|e| e.into_path())
            .filter(|p| p.extension().is_some_and(|x| x == "sql"))
            .collect();
        macros.sort();
        out.push(Package { name, dir, macros });
    }
    out
}

/// `dispatch:` in `dre_project.yml`: per macro namespace, where `dispatch()` looks and in what
/// order. Unlisted namespaces search the root project first, then the namespace itself.
pub type DispatchOrder = BTreeMap<String, Vec<String>>;
