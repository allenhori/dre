//! `dre deps`, `dre plugin install/update/remove`, and auto-install before run/validate.

use std::path::PathBuf;
use std::process::ExitCode;

use dre_core::lock::Lock;
use dre_core::manager::{self, Index};
use dre_core::project::{self, LoadOptions, PluginKind, PluginSource, Project};
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

/// Parse `name`, `kind/name`, with an optional `@req`.
fn parse(spec: &str, index: &Index) -> Result<(PluginKind, String, Option<VersionReq>), String> {
    let (id, req) = match spec.split_once('@') {
        Some((i, r)) => (
            i,
            Some(VersionReq::parse(r).map_err(|e| format!("invalid version `{r}`: {e}"))?),
        ),
        None => (spec, None),
    };
    if let Some((k, n)) = id.split_once('/') {
        let kind = PluginKind::parse(k)
            .ok_or_else(|| format!("unknown plugin kind `{k}` (source, format, destination)"))?;
        return Ok((kind, n.to_string(), req));
    }
    match index.by_name(id).as_slice() {
        [] => Err(format!("the registry has no plugin called `{id}`")),
        [p] => Ok((p.kind, id.to_string(), req)),
        many => Err(format!(
            "`{id}` is ambiguous; use one of: {}",
            many.iter()
                .map(|p| format!("{}/{id}", p.kind.as_str()))
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

fn project_at(dir: &std::path::Path) -> Option<Project> {
    if !dir.join(project::PROJECT_FILE).is_file() {
        return None;
    }
    project::load(dir, &LoadOptions::default()).0
}

/// `dre plugin install` / `dre plugin update`.
pub fn install(spec: String, project_dir: PathBuf, update: bool, printer: &Printer) -> ExitCode {
    let project = project_at(&project_dir);
    // A plugin the project declares installs from wherever it's declared to come from.
    let id = spec.split_once('@').map_or(spec.as_str(), |(i, _)| i);
    let from_project: Vec<&dre_core::project::PluginRequirement> = project
        .iter()
        .flat_map(|p| &p.plugins)
        .filter(|r| r.name == id || format!("{}/{}", r.kind.as_str(), r.name) == id)
        .collect();
    let source = match from_project.as_slice() {
        [r] => Some((r.source.clone(), r.kind, r.name.clone())),
        _ => None,
    };
    if let Some((PluginSource::Local(path), kind, name)) = &source {
        printer.line(
            Tone::Note,
            "Local",
            &format!(
                "{} plugin `{name}` is used from {path}; there's nothing to install",
                kind.as_str()
            ),
        );
        return ExitCode::SUCCESS;
    }
    let index = match &source {
        Some((s, kind, name)) => Index::for_source(s, *kind, name),
        None => Index::load(),
    };
    let index = match index {
        Ok(i) => i,
        Err(e) => {
            printer.error(&e);
            return ExitCode::FAILURE;
        }
    };
    let (kind, name, cli_req) = match parse(&spec, &index) {
        Ok(x) => x,
        Err(e) => {
            printer.error(&e);
            return ExitCode::FAILURE;
        }
    };
    let source_key = source.as_ref().and_then(|(s, _, _)| s.lock_key());
    let declared = project
        .as_ref()
        .and_then(|p| p.plugins.iter().find(|r| r.kind == kind && r.name == name))
        .map(|r| r.req());
    let mut lock = project.as_ref().map(|p| Lock::load(&p.root).unwrap_or_default());
    // The declared constraint always applies; a CLI constraint narrows it further.
    let req = match (&declared, &cli_req) {
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
    let Some(plugin) = index.plugin(kind, &name) else {
        printer.error(&format!("the registry has no {} plugin `{name}`", kind.as_str()));
        return ExitCode::FAILURE;
    };
    let pinned = lock
        .as_ref()
        .and_then(|l| l.get(kind, &name).cloned())
        .filter(|l| l.from == source_key);
    let version = match (&pinned, update, &cli_req) {
        (Some(l), false, None) => plugin.exact(&l.version),
        _ => plugin.best(&req),
    };
    let Some(version) = version else {
        printer.error(&format!(
            "no version of {} `{name}` matches `{req}` for {}",
            kind.as_str(),
            manager::platform()
        ));
        return ExitCode::FAILURE;
    };
    let dir = dre_core::plugins::plugins_dir(project.as_ref().map(|p| p.root.as_path()));
    match manager::install_linked(&dir, plugin, version, None) {
        Ok(mut locked) => {
            locked.from = source_key.clone();
            printer.line(
                Tone::Good,
                "Installed",
                &format!("{} plugin `{name}` {}", kind.as_str(), locked.version),
            );
            if let (Some(p), Some(l)) = (&project, lock.as_mut())
                && declared.is_some()
            {
                l.map_mut(kind).insert(name.clone(), locked);
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
    let (id, version) = match spec.split_once('@') {
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
    let (kind_filter, name) = match id.split_once('/') {
        Some((k, n)) => (PluginKind::parse(k), n.to_string()),
        None => (None, id),
    };
    let targets: Vec<_> = dre_core::plugins::discover(&dir)
        .into_iter()
        .filter(|p| p.name == name && kind_filter.is_none_or(|k| k == p.kind))
        .filter(|p| version.is_none() || p.version == version)
        .collect();
    if targets.is_empty() {
        printer.error(&format!("no installed plugin matches `{spec}`"));
        return ExitCode::FAILURE;
    }
    for t in &targets {
        let dir_to_remove = if t.version.is_some() {
            t.path.parent().map(|p| p.to_path_buf())
        } else {
            None
        };
        let r = match dir_to_remove {
            Some(d) => std::fs::remove_dir_all(&d),
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
        printer.line(
            Tone::Good,
            "Removed",
            &format!("{} plugin `{}` {v}", t.kind, t.name),
        );
    }
    // Drop the lock pin if the pinned version is gone.
    if let Some(p) = project_at(&project_dir)
        && let Ok(mut lock) = Lock::load(&p.root)
    {
        let mut changed = false;
        for t in &targets {
            let kind = t.kind;
            if let Some(l) = lock.get(kind, &t.name)
                && (t.version.is_none() || t.version.as_ref() == Some(&l.version))
            {
                lock.map_mut(kind).remove(&t.name);
                changed = true;
            }
        }
        if changed {
            if let Err(e) = lock.save(&p.root) {
                printer.error(&e);
                return ExitCode::FAILURE;
            }
            printer.line(Tone::Note, "Unlocked", "removed the pin from dre.lock");
        }
    }
    ExitCode::SUCCESS
}
