//! `dre ls`: the reports and Bindings a selection or schedule covers, read from the loaded
//! project (the manifest's per-selection view), with each Binding's connections from the parse
//! pass; or, with `--resource-type source`, the declared sources and who reads them. Offline and
//! read-only: it writes nothing, not even the manifest.

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, ValueEnum};
use dre_core::project::{self, Binding, LoadOptions, Project, Report};

#[derive(Args)]
pub struct LsArgs {
    /// What to list: report names, `tag:<tag>`, folder names or dotted folder paths. Lists every
    /// report when omitted.
    selector: Vec<String>,
    /// What to select (dbt's `--select`), as on `dre run`.
    #[arg(short = 's', long = "select", value_name = "SELECTOR", num_args = 1.., action = clap::ArgAction::Append, conflicts_with = "selector")]
    select: Vec<String>,
    /// Only this Set's Bindings (`all`: every Set). Without it, every declared Binding of each
    /// selected report is listed, not only the default Set a plain `dre run` would pick.
    #[arg(long)]
    set: Option<String>,
    /// The Bindings a schedules.yml entry runs.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["selector", "select", "set"])]
    schedule: Option<String>,
    /// `text` for people, `json` for tools (the manifest's shape, holding only what matched).
    #[arg(long, value_enum, default_value = "text")]
    output: LsOutput,
    /// What to list: reports (default), or the declared sources (each table, its connection,
    /// and the reports that read it; unused ones are flagged).
    #[arg(long, value_enum, default_value = "report")]
    resource_type: ResourceType,
    /// The run's target (environment), which sets every profile's entry (default: $DRE_TARGET;
    /// without either, each profile uses its own `target:`, else `dev`).
    #[arg(long)]
    target: Option<String>,
    /// Set a variable for `var()`, as on `dre run`.
    #[arg(long = "var", value_name = "NAME=VALUE", value_parser = crate::parse_var)]
    vars: Vec<(String, String)>,
    /// Project directory (default: the current directory).
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
    /// Directory holding profiles.yml (not needed; accepted as on the other project commands).
    #[arg(long)]
    profiles_dir: Option<PathBuf>,
    /// The target path, accepted as on the other project commands; `ls` writes nothing to it.
    #[arg(long, value_name = "PATH")]
    target_path: Option<String>,
}

#[derive(Clone, Copy, ValueEnum)]
enum LsOutput {
    Text,
    Json,
}

#[derive(Clone, Copy, PartialEq, Eq, ValueEnum)]
enum ResourceType {
    Report,
    Source,
}

/// Diagnostics go to stderr, so stdout holds only data.
pub fn ls(a: LsArgs) -> ExitCode {
    let opts = LoadOptions {
        profiles_dir: a.profiles_dir.clone(),
        target_path: a.target_path.clone(),
        target: a.target.clone(),
        vars: a.vars.iter().cloned().collect(),
        date: crate::run_date(),
        scheduled_at: crate::run_at().ok().flatten(),
        timezone: std::env::var("DRE_TIMEZONE").ok().filter(|t| !t.is_empty()),
    };
    let (project, diags) = project::load(&a.project_dir, &opts);
    let Some(project) = project else {
        for d in diags.sorted() {
            eprintln!("{d}");
        }
        return ExitCode::FAILURE;
    };
    dre_core::secrets::set_enabled(project.mask_secrets);
    if a.resource_type == ResourceType::Source {
        return sources(&project, &a);
    }
    let (reports, schedules) = match select(&project, &a) {
        Ok(found) => found,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    match a.output {
        LsOutput::Json => {
            let errors = dre_core::manifest::report_errors(&project, &diags);
            let doc = dre_core::manifest::subset(&project, reports, &schedules, &errors);
            print!("{}", dre_core::manifest::render(&doc));
        }
        LsOutput::Text => print!("{}", dre_core::secrets::mask(&table(&reports))),
    }
    ExitCode::SUCCESS
}

type Selected<'a> = (Vec<(&'a Report, Vec<&'a Binding>)>, Vec<String>);

fn select<'a>(project: &'a Project, a: &LsArgs) -> Result<Selected<'a>, String> {
    if let Some(name) = &a.schedule {
        if !project.schedules.iter().any(|e| &e.name == name) {
            return Err(dre_core::run::unknown_schedule(project, name));
        }
        let reports = project
            .reports
            .iter()
            .map(|r| {
                (
                    r,
                    r.bindings
                        .iter()
                        .filter(|b| b.schedules.contains(name))
                        .collect::<Vec<_>>(),
                )
            })
            .filter(|(_, bs)| !bs.is_empty())
            .collect();
        return Ok((reports, vec![name.clone()]));
    }
    let all = if a.select.is_empty() {
        &a.selector
    } else {
        &a.select
    };
    let chosen: Vec<&Report> = if all.is_empty() {
        project.reports.iter().collect()
    } else {
        let s = all.join(" ");
        match dre_core::selector::resolve(project, &s) {
            Ok(r) if r.is_empty() => return Err(format!("selector `{s}` matches no report")),
            Ok(r) => r,
            Err(e) => return Err(e.to_string()),
        }
    };
    let reports: Vec<(&Report, Vec<&Binding>)> = chosen
        .into_iter()
        .map(|r| {
            let bs = r
                .bindings
                .iter()
                .filter(|b| match a.set.as_deref() {
                    None | Some("all") => true,
                    Some(set) => b.set.as_deref() == Some(set),
                })
                .collect::<Vec<_>>();
            (r, bs)
        })
        .filter(|(_, bs)| a.set.is_none() || !bs.is_empty())
        .collect();
    if let Some(set) = &a.set
        && reports.is_empty()
    {
        return Err(format!("no selected report declares Set `{set}`"));
    }
    Ok((reports, Vec::new()))
}

/// `--resource-type source`: one line per source table, or the manifest's `sources` (only those
/// a `source:` selector names, when there is one).
fn sources(project: &Project, a: &LsArgs) -> ExitCode {
    let terms: Vec<String> = a
        .select
        .iter()
        .chain(&a.selector)
        .flat_map(|s| s.split([' ', ',', ';']).map(str::to_string).collect::<Vec<_>>())
        .filter(|t| !t.is_empty())
        .collect();
    let mut wanted: Vec<(String, Option<String>)> = Vec::new();
    for t in &terms {
        let Some(sel) = t.strip_prefix("source:") else {
            eprintln!(
                "error: with --resource-type source, select with `source:<source>` or `source:<source>.<table>`, not `{t}`"
            );
            return ExitCode::FAILURE;
        };
        let (s, tb) = match sel.split_once('.') {
            Some((s, tb)) => (s.to_string(), Some(tb.to_string())),
            None => (sel.to_string(), None),
        };
        match project.sources.get(&s) {
            None => {
                eprintln!("error: no source `{s}`");
                return ExitCode::FAILURE;
            }
            Some(def) if tb.as_ref().is_some_and(|t| def.table(t).is_none()) => {
                eprintln!("error: source `{s}` has no table `{}`", tb.unwrap());
                return ExitCode::FAILURE;
            }
            _ => wanted.push((s, tb)),
        }
    }
    let picked = |s: &str, t: &str| {
        wanted.is_empty()
            || wanted
                .iter()
                .any(|(ws, wt)| ws == s && wt.as_deref().is_none_or(|wt| wt == t))
    };
    let doc = dre_core::manifest::build(project, &Default::default());
    let mut all = doc["sources"].as_object().cloned().unwrap_or_default();
    for (name, src) in all.iter_mut() {
        if let Some(tables) = src["tables"].as_object_mut() {
            tables.retain(|t, _| picked(name, t));
        }
    }
    all.retain(|_, src| src["tables"].as_object().is_some_and(|t| !t.is_empty()));
    match a.output {
        LsOutput::Json => {
            let doc = serde_json::json!({"sources": all});
            print!("{}", dre_core::manifest::render(&doc));
        }
        LsOutput::Text => {
            let mut rows = vec![[
                "SOURCE".to_string(),
                "CONNECTION".into(),
                "RELATION".into(),
                "USED BY".into(),
            ]];
            for (name, src) in &all {
                let conn = src["profile"].as_str().unwrap_or("(the query's)").to_string();
                let prefix: Vec<&str> = [src["database"].as_str(), src["schema"].as_str()]
                    .into_iter()
                    .flatten()
                    .collect();
                for (t, table) in src["tables"].as_object().into_iter().flatten() {
                    let used: Vec<&str> = table["used_by"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(|u| u.as_str())
                        .collect();
                    let mut rel = prefix.clone();
                    rel.push(table["identifier"].as_str().unwrap_or(t));
                    rows.push([
                        format!("{name}.{t}"),
                        conn.clone(),
                        rel.join("."),
                        if used.is_empty() {
                            "(unused)".into()
                        } else {
                            used.join(", ")
                        },
                    ]);
                }
            }
            print!("{}", dre_core::secrets::mask(&columns(rows)));
        }
    }
    ExitCode::SUCCESS
}

/// Rows of cells, left-aligned in columns.
fn columns<const N: usize>(rows: Vec<[String; N]>) -> String {
    let widths: Vec<usize> = (0..N)
        .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
        .collect();
    let mut out = String::new();
    for r in rows {
        let line: Vec<String> = r
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
            .collect();
        out.push_str(line.join("  ").trim_end());
        out.push('\n');
    }
    out
}

/// One Binding per line: report, Set, the connections its queries run on, format and
/// destinations.
fn table(reports: &[(&Report, Vec<&Binding>)]) -> String {
    let mut rows = vec![[
        "REPORT".to_string(),
        "SET".into(),
        "CONNECTIONS".into(),
        "FORMAT".into(),
        "DESTINATIONS".into(),
    ]];
    for (r, bs) in reports {
        for b in bs {
            let conns = b
                .parsed
                .as_ref()
                .map(|p| p.connections().join(", "))
                .filter(|c| !c.is_empty())
                .unwrap_or_else(|| "-".into());
            let dests: Vec<String> = b
                .output
                .destinations
                .iter()
                .map(|d| match &d.path {
                    Some(p) => format!("{}:{p}", d.profile),
                    None => d.profile.clone(),
                })
                .collect();
            rows.push([
                r.name.clone(),
                b.set.clone().unwrap_or_else(|| "-".into()),
                conns,
                b.output.format.clone(),
                if dests.is_empty() {
                    "-".into()
                } else {
                    dests.join(", ")
                },
            ]);
        }
    }
    columns(rows)
}
