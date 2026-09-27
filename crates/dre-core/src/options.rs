//! Plugin options: a format's `output:` keys and a destination entry's keys. Core owns only the
//! keys every output shares (`format`, `destination`, `template`, `extension`, `profile`,
//! `path`); everything else belongs to the plugin, which checks it (the protocol's `validate`).
//! A new format or destination therefore needs no change to core.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use dre_protocol::host::{LogSink, PluginProcess};
use serde_json::{Map, Value};

use crate::Diagnostics;
use crate::profiles::{LOCAL_TYPE, Role};
use crate::project::{PluginKind, Project};
use crate::run::find_plugin;

/// Parse `B12` into zero-based `(row, col)`.
pub use dre_protocol::util::parse_cell;

pub fn is_cell(s: &str) -> bool {
    parse_cell(s).is_some()
}

/// Where a config block is used: the report (and Set), its file, and for a destination, the
/// profile it names.
struct Use {
    ctx: String,
    file: PathBuf,
    profile: Option<String>,
}

/// One plugin's distinct option blocks (keyed by their JSON), each with where it's used.
type Blocks = BTreeMap<String, (Map<String, Value>, Vec<Use>)>;

/// Ask each format and destination plugin to check every config block the project gives it,
/// found by kind and name alone: core knows nothing of any plugin's options. `target` picks each
/// destination profile's output, as `--target` does for a run. A declared plugin that can't be
/// found is an error, or a warning with `offline` (`dre validate --no-auto-install`, which
/// doesn't install plugins).
pub fn check(project: &Project, target: Option<&str>, offline: bool, diags: &mut Diagnostics) {
    // (kind, plugin) -> distinct option blocks -> where each is used.
    let mut blocks: BTreeMap<(PluginKind, String), Blocks> = BTreeMap::new();
    let mut add = |kind, name: &str, options: &Map<String, Value>, u: Use| {
        let key = serde_json::to_string(options).unwrap_or_default();
        blocks
            .entry((kind, name.to_string()))
            .or_default()
            .entry(key)
            .or_insert_with(|| (options.clone(), Vec::new()))
            .1
            .push(u);
    };
    for (format, options) in &project.format_options {
        let u = Use {
            ctx: format!("`format_options.{format}`"),
            file: PathBuf::from(crate::project::PROJECT_FILE),
            profile: None,
        };
        add(PluginKind::Format, format, options, u);
    }
    for r in &project.reports {
        for b in &r.bindings {
            let ctx = match &b.set {
                Some(s) => format!("report `{}`, Set `{s}`", r.name),
                None => format!("report `{}`", r.name),
            };
            let at = |profile: Option<&str>| Use {
                ctx: ctx.clone(),
                file: r.file.clone(),
                profile: profile.map(str::to_string),
            };
            add(PluginKind::Format, &b.output.format, &b.output.options, at(None));
            for d in &b.output.destinations {
                let kind = if project.profiles.is_builtin_local(&d.profile) {
                    LOCAL_TYPE
                } else {
                    match project.profiles.target(Role::Destination, &d.profile, target) {
                        Some((_, t)) => t.kind.as_str(),
                        // A missing profile or target is reported elsewhere.
                        None => continue,
                    }
                };
                add(PluginKind::Destination, kind, &d.options, at(Some(&d.profile)));
            }
        }
    }

    let log: LogSink = Arc::new(|_, _| {});
    for ((kind, name), blocks) in blocks {
        let report = |diags: &mut Diagnostics, u: &Use, e: &str| {
            let (code, msg) = match &u.profile {
                Some(p) => (
                    "invalid-destination-option",
                    format!("{}: destination `{p}`: {e}", u.ctx),
                ),
                None => ("invalid-output-option", format!("{}: {e}", u.ctx)),
            };
            diags.error(code, Some(u.file.clone()), None, msg);
        };
        // An undeclared plugin is already an error (`undeclared-plugin`).
        let local = kind == PluginKind::Destination && name == LOCAL_TYPE;
        if !local && !project.plugins.iter().any(|r| r.kind == kind && r.name == name) {
            continue;
        }
        // Core's own destination.
        if kind == PluginKind::Destination && name == LOCAL_TYPE {
            for (options, uses) in blocks.values() {
                if let Some(k) = options.keys().next() {
                    for u in uses {
                        report(
                            diags,
                            u,
                            &format!(
                                "the local destination takes no options, but got `{k}`; check the key's spelling"
                            ),
                        );
                    }
                }
            }
            continue;
        }
        let path = match find_plugin(project, kind, &name) {
            Ok(p) => p,
            Err(e) => {
                let first = blocks.values().flat_map(|(_, u)| u).next();
                let file = first.map(|u| u.file.clone());
                let at = first.map(|u| format!("{}: ", u.ctx)).unwrap_or_default();
                if offline {
                    diags.warning(
                        "options-unchecked",
                        file,
                        None,
                        format!("{at}{kind} `{name}`'s options weren't checked: {e}"),
                    );
                } else {
                    diags.error(
                        "plugin-not-found",
                        file,
                        None,
                        format!("{at}{kind} `{name}` has no plugin to check it against: {e}"),
                    );
                }
                continue;
            }
        };
        let mut p = match PluginProcess::start_in(&path, log.clone(), Some(&project.root)) {
            Ok(p) => p,
            Err(e) => {
                diags.warning(
                    "options-unchecked",
                    None,
                    None,
                    format!("can't check the options of {kind} `{name}`: {e}"),
                );
                continue;
            }
        };
        if !p.has(dre_protocol::CAP_VALIDATE) {
            if blocks.values().any(|(o, _)| !o.is_empty()) {
                diags.warning(
                    "options-unchecked",
                    None,
                    None,
                    format!(
                        "{kind} `{name}` {} predates option checks, so its options weren't checked; run `dre plugin update {kind}/{name}`",
                        p.info().version
                    ),
                );
            }
            let _ = p.close();
            continue;
        }
        for (options, uses) in blocks.into_values() {
            match p.validate(options) {
                Ok(errors) => {
                    for u in &uses {
                        for e in &errors {
                            report(diags, u, e);
                        }
                    }
                }
                Err(e) => {
                    diags.warning(
                        "options-unchecked",
                        None,
                        None,
                        format!("{kind} `{name}` failed to check its options: {e}"),
                    );
                    break;
                }
            }
        }
        let _ = p.close();
    }
}
