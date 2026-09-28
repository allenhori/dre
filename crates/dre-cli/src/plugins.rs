//! `dre deps`, `dre plugin install/update/remove`, and auto-install before run/validate.

use std::path::PathBuf;
use std::process::ExitCode;

use dre_core::lock::Lock;
use dre_core::manager::{self, Index, IndexPackage};
use dre_core::project::{self, LoadOptions, PluginSource, Project};
use semver::VersionReq;

use crate::output::{Printer, Tone};

/// Install whatever the project declares but lacks. Returns false (after printing why) if any
/// plugin is missing and couldn't be installed. With `resolve` (`dre deps`), plugins not pinned
/// in `dre.lock` are resolved against the registry and pinned.
pub fn ensure(project: &Project, install: bool, resolve: bool, printer: &Printer) -> bool {
    match manager::sync(project, install, resolve, |m| {
        printer.line(Tone::Note, "Installed", m.trim_start_matches("installed "))
    }) {
        Ok(_) => true,
        Err(errors) => {
            for e in errors {
                printer.error(&e);
            }
            false
        }
    }
}

/// Install missing or outdated macro packages. Runs before the project loads, which needs them.
pub fn sync_packages(root: &std::path::Path, printer: &Printer) -> bool {
    match dre_core::packages::sync(root, true, |m| printer.line(Tone::Note, "Installed", m)) {
        Ok(()) => true,
        Err(errors) => {
            for e in errors {
                printer.error(&e);
            }
            false
        }
    }
}

/// `dre validate`: install missing plugins, or (with `--no-auto-install`) warn about them.
pub fn check_for_validate(
    project: &Project,
    install: bool,
    diags: &mut dre_core::Diagnostics,
    printer: &Printer,
) {
    if diags.has_errors() {
        return;
    }
    let r = manager::sync(project, install, false, |m| {
        printer.line(Tone::Note, "Installed", m.trim_start_matches("installed "))
    });
    if let Err(errors) = r {
        for e in errors {
            if install {
                diags.error("plugin-install-failed", None, None, e);
            } else {
                diags.warning("plugin-not-installed", None, None, e);
            }
        }
    }
}

/// Parse `package[@req]`.
fn parse(spec: &str) -> Result<(String, Option<VersionReq>), String> {
    match spec.split_once('@') {
        Some((i, r)) => Ok((
            i.to_string(),
            Some(VersionReq::parse(r).map_err(|e| format!("invalid version `{r}`: {e}"))?),
        )),
        None => Ok((spec.to_string(), None)),
    }
}

fn project_at(dir: &std::path::Path) -> Option<Project> {
    if !dir.join(project::PROJECT_FILE).is_file() {
        return None;
    }
    project::load(dir, &LoadOptions::default()).0
}

/// The package called `name` in `index`, or why there isn't one.
fn package<'a>(index: &'a Index, name: &str) -> Result<&'a IndexPackage, String> {
    if let Some(p) = index.package(name) {
        return Ok(p);
    }
    let inside: Vec<String> = index
        .providers_of_name(name)
        .iter()
        .map(|p| format!("`{}`", p.name))
        .collect();
    Err(if inside.is_empty() {
        format!("the registry has no plugin package `{name}`")
    } else {
        format!(
            "the registry has no plugin package `{name}`; the `{name}` plugin comes in {}",
            inside.join(", ")
        )
    })
}

/// `dre plugin install` / `dre plugin update`.
pub fn install(spec: String, project_dir: PathBuf, update: bool, printer: &Printer) -> ExitCode {
    let (name, cli_req) = match parse(&spec) {
        Ok(x) => x,
        Err(e) => {
            printer.error(&e);
            return ExitCode::FAILURE;
        }
    };
    let project = project_at(&project_dir);
    // A package the project declares installs from wherever it's declared to come from.
    let declared = project
        .iter()
        .flat_map(|p| &p.plugins)
        .find(|r| r.name == name)
        .cloned();
    let source = declared.as_ref().map(|r| r.source.clone()).unwrap_or_default();
    if let PluginSource::Local(path) = &source {
        printer.line(
            Tone::Note,
            "Local",
            &format!("plugin package `{name}` is used from {path}; there's nothing to install"),
        );
        return ExitCode::SUCCESS;
    }
    let index = match Index::for_source(&source, &name) {
        Ok(i) => i,
        Err(e) => {
            printer.error(&e);
            return ExitCode::FAILURE;
        }
    };
    let package = match package(&index, &name) {
        Ok(p) => p,
        Err(e) => {
            printer.error(&e);
            return ExitCode::FAILURE;
        }
    };
    let source_key = source.lock_key();
    let declared_req = declared.as_ref().map(|r| r.req());
    let mut lock = project.as_ref().map(|p| Lock::load(&p.root).unwrap_or_default());
    // The declared constraint always applies; a CLI constraint narrows it further.
    let req = match (&declared_req, &cli_req) {
        (Some(d), Some(c)) => {
            if !dre_core::constraints::compatible(&[d, c]) {
                printer.error(&format!(
                    "`{c}` contradicts the project's declared constraint `{d}` for `{name}`"
                ));
                return ExitCode::FAILURE;
            }
            dre_core::constraints::combine(&[d, c])
        }
        (Some(d), None) => d.clone(),
        (None, Some(c)) => c.clone(),
        (None, None) => VersionReq::STAR,
    };
    let pinned = lock
        .as_ref()
        .and_then(|l| l.get(&name).cloned())
        .filter(|l| l.from == source_key);
    let version = match (&pinned, update, &cli_req) {
        (Some(l), false, None) => package.exact(&l.version),
        _ => package.best(&req),
    };
    let Some(version) = version else {
        printer.error(&format!(
            "no version of the plugin package `{name}` matches `{req}` for {}",
            manager::platform()
        ));
        return ExitCode::FAILURE;
    };
    let dir = dre_core::plugins::plugins_dir(project.as_ref().map(|p| p.root.as_path()));
    let installed = if source.is_default() {
        manager::install_linked(&dir, package, version, None)
    } else {
        manager::install(&dir, package, version, None)
    };
    match installed {
        Ok(mut locked) => {
            locked.from = source_key.clone();
            let provides: Vec<String> = locked.provides.iter().map(|p| p.to_string()).collect();
            printer.line(
                Tone::Good,
                "Installed",
                &format!(
                    "plugin package `{name}` {} ({})",
                    locked.version,
                    provides.join(", ")
                ),
            );
            if let (Some(p), Some(l)) = (&project, lock.as_mut())
                && declared.is_some()
            {
                l.plugins.insert(name.clone(), locked);
                if let Err(e) = l.save(&p.root) {
                    printer.error(&e);
                    return ExitCode::FAILURE;
                }
                printer.line(
                    Tone::Note,
                    "Locked",
                    &format!("dre.lock pins `{name}` to {}", version.version),
                );
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            printer.error(&e);
            ExitCode::FAILURE
        }
    }
}

/// `dre plugin remove`.
pub fn remove(spec: String, project_dir: PathBuf, printer: &Printer) -> ExitCode {
    let (name, version) = match spec.split_once('@') {
        Some((i, v)) => match semver::Version::parse(v) {
            Ok(v) => (i.to_string(), Some(v)),
            Err(e) => {
                printer.error(&format!("`{v}` isn't an exact version: {e}"));
                return ExitCode::FAILURE;
            }
        },
        None => (spec.clone(), None),
    };
    let in_project = project_dir.join(project::PROJECT_FILE).is_file();
    let dir = dre_core::plugins::plugins_dir(in_project.then_some(project_dir.as_path()));
    let targets: Vec<_> = dre_core::plugins::discover(&dir)
        .into_iter()
        .filter(|p| p.name == name)
        .filter(|p| version.is_none() || p.version == version)
        .collect();
    if targets.is_empty() {
        printer.error(&format!("no installed plugin package matches `{spec}`"));
        return ExitCode::FAILURE;
    }
    for t in &targets {
        let r = match &t.version {
            Some(_) => t.path.parent().map_or(Ok(()), std::fs::remove_dir_all),
            None => std::fs::remove_file(&t.path),
        };
        if let Err(e) = r {
            printer.error(&format!("can't remove {}: {e}", t.path.display()));
            return ExitCode::FAILURE;
        }
        let v = t
            .version
            .as_ref()
            .map(|v| v.to_string())
            .unwrap_or_else(|| "(unversioned)".into());
        printer.line(Tone::Good, "Removed", &format!("plugin package `{}` {v}", t.name));
    }
    // Drop the lock pin if the pinned version is gone.
    if let Some(p) = project_at(&project_dir)
        && let Ok(mut lock) = Lock::load(&p.root)
    {
        let gone = lock.get(&name).is_some_and(|l| {
            targets
                .iter()
                .any(|t| t.version.is_none() || t.version.as_ref() == Some(&l.version))
        });
        if gone {
            lock.plugins.remove(&name);
            if let Err(e) = lock.save(&p.root) {
                printer.error(&e);
                return ExitCode::FAILURE;
            }
            printer.line(Tone::Note, "Unlocked", "removed the pin from dre.lock");
        }
    }
    ExitCode::SUCCESS
}
