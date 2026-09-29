//! `dre ls`: the reports and Bindings a selection or schedule covers, read from the loaded
//! project (the manifest's per-selection view). Offline and read-only: it writes nothing, not
//! even the manifest.

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

/// Diagnostics go to stderr, so stdout holds only data.
pub fn ls(a: LsArgs) -> ExitCode {
    let opts = LoadOptions {
        profiles_dir: a.profiles_dir.clone(),
        target_path: a.target_path.clone(),
        ..Default::default()
    };
    let (project, diags) = project::load(&a.project_dir, &opts);
    let Some(project) = project else {
        for d in diags.sorted() {
            eprintln!("{d}");
        }
        return ExitCode::FAILURE;
    };
    dre_core::secrets::set_enabled(project.mask_secrets);
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

/// One Binding per line: report, Set, format and destinations.
fn table(reports: &[(&Report, Vec<&Binding>)]) -> String {
    let mut rows = vec![[
        "REPORT".to_string(),
        "SET".into(),
        "FORMAT".into(),
        "DESTINATIONS".into(),
    ]];
    for (r, bs) in reports {
        for b in bs {
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
                b.output.format.clone(),
                if dests.is_empty() {
                    "-".into()
                } else {
                    dests.join(", ")
                },
            ]);
        }
    }
    let widths: Vec<usize> = (0..4)
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
