//! `dre schedule ls`: when the project's schedules fire, as a table for people or a versioned
//! JSON document (with the command for each firing) for tools. Pure: it reads the project and
//! nothing else, and writes nothing.

use std::path::PathBuf;
use std::process::ExitCode;

use chrono::{DateTime, Duration, NaiveDate, Timelike, Utc};
use clap::{Args, Subcommand, ValueEnum};
use dre_core::occurrences::{self, Request, Window};
use dre_core::project::{self, LoadOptions};
use serde_json::Value as Json;

#[derive(Subcommand)]
pub enum ScheduleCommand {
    /// List when schedules fire (occurrences) within a window, with the command to run each one.
    Ls(LsArgs),
}

#[derive(Args)]
pub struct LsArgs {
    /// Only this schedule (repeatable).
    #[arg(long, value_name = "NAME")]
    schedule: Vec<String>,
    /// Only schedules that run one of these reports: report names, `tag:<tag>`, folder names or
    /// dotted folder paths, as on `dre run`.
    #[arg(short = 's', long = "select", value_name = "SELECTOR", num_args = 1.., action = clap::ArgAction::Append)]
    select: Vec<String>,
    /// Start of the window: a date (00:00 UTC) or an RFC 3339 date-time (default: now, to the
    /// minute). May be in the past.
    #[arg(long, value_name = "WHEN")]
    from: Option<String>,
    /// End of the window, exclusive (default: 35 days after --from; at most 366 days after it).
    #[arg(long, value_name = "WHEN")]
    to: Option<String>,
    /// At most this many firings per schedule (text output defaults to 5).
    #[arg(long, value_name = "N", value_parser = clap::value_parser!(u64).range(1..))]
    limit: Option<u64>,
    /// One occurrence per report and Set instead of one per firing.
    #[arg(long)]
    split: bool,
    /// `text` for people, `json` for tools (the versioned schedule document).
    #[arg(long, value_enum, default_value = "text")]
    output: Output,
    /// Project directory (default: the current directory).
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
    /// Directory holding profiles.yml (not needed; accepted as on the other project commands).
    #[arg(long)]
    profiles_dir: Option<PathBuf>,
}

#[derive(Clone, Copy, PartialEq, ValueEnum)]
enum Output {
    Text,
    Json,
}

/// A date (00:00 UTC) or an RFC 3339 date-time.
fn when(flag: &str, v: &str) -> Result<DateTime<Utc>, String> {
    if let Ok(d) = NaiveDate::parse_from_str(v, "%Y-%m-%d") {
        return Ok(d.and_hms_opt(0, 0, 0).unwrap().and_utc());
    }
    DateTime::parse_from_rfc3339(v)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| format!("{flag}: `{v}` isn't a date (YYYY-MM-DD) or an RFC 3339 date-time"))
}

fn window(a: &LsArgs) -> Result<Window, String> {
    let from = match &a.from {
        Some(v) => when("--from", v)?,
        None => {
            let now = Utc::now();
            now.with_second(0)
                .and_then(|t| t.with_nanosecond(0))
                .unwrap_or(now)
        }
    };
    let to = match &a.to {
        Some(v) => when("--to", v)?,
        None => from + Duration::days(occurrences::DEFAULT_WINDOW_DAYS),
    };
    Window::new(from, to)
}

/// Diagnostics go to stderr, so stdout holds only data.
pub fn ls(a: LsArgs) -> ExitCode {
    let window = match window(&a) {
        Ok(w) => w,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let opts = LoadOptions {
        profiles_dir: a.profiles_dir.clone(),
        ..Default::default()
    };
    let (project, diags) = project::load(&a.project_dir, &opts);
    // A schedule with an error is left out of the project, which a consumer would read as
    // removed; so an error stops the command. Profiles aren't needed, so their errors don't.
    let errors: Vec<&dre_core::Diagnostic> = diags
        .sorted()
        .into_iter()
        .filter(|d| d.severity == dre_core::Severity::Error && !about_profiles(d, project.as_ref()))
        .collect();
    let project = match project {
        Some(p) if errors.is_empty() => p,
        _ => {
            for d in errors {
                eprintln!("{d}");
            }
            eprintln!("error: the project has errors; fix them first (see `dre validate`)");
            return ExitCode::FAILURE;
        }
    };
    dre_core::secrets::set_enabled(project.mask_secrets);
    let text = a.output == Output::Text;
    let req = Request {
        window,
        schedules: a.schedule.clone(),
        select: (!a.select.is_empty()).then(|| a.select.join(" ")),
        limit: a.limit.map(|n| n as usize).or(text.then_some(5)),
        split: a.split,
    };
    let doc = match occurrences::document(&project, &req) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::FAILURE;
        }
    };
    if text {
        print!("{}", dre_core::secrets::mask(&table(&doc)));
        for p in doc["problems"].as_array().into_iter().flatten() {
            eprintln!(
                "warning[{}]: schedule `{}`: {}",
                p["code"].as_str().unwrap_or(""),
                p["schedule"].as_str().unwrap_or(""),
                p["message"].as_str().unwrap_or("")
            );
        }
    } else {
        print!("{}", dre_core::manifest::render(&doc));
    }
    ExitCode::SUCCESS
}

/// An error about profiles.yml or the profiles a project names, which listing schedules doesn't
/// need.
fn about_profiles(d: &dre_core::Diagnostic, project: Option<&project::Project>) -> bool {
    const CODES: &[&str] = &[
        "unknown-profile",
        "unknown-target",
        "missing-target",
        "profiles-missing",
    ];
    CODES.contains(&d.code) || project.is_some_and(|p| d.file.as_deref() == Some(p.profiles.path.as_path()))
}

/// One firing per line in time order: local time, schedule, reports; then paused schedules.
fn table(doc: &Json) -> String {
    let mut rows = vec![["TIME".to_string(), "SCHEDULE".into(), "REPORTS".into()]];
    for o in doc["occurrences"].as_array().into_iter().flatten() {
        let at = o["fires_at"].as_str().unwrap_or("");
        let tz = o["timezone"]
            .as_str()
            .and_then(|t| t.parse::<chrono_tz::Tz>().ok());
        let time = match (DateTime::parse_from_rfc3339(at), tz) {
            (Ok(t), Some(tz)) => t.with_timezone(&tz).format("%Y-%m-%d %H:%M %Z").to_string(),
            _ => at.to_string(),
        };
        let reports: Vec<String> = o["bindings"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|b| match b["set"].as_str() {
                Some(s) => format!("{}/{s}", b["report"].as_str().unwrap_or("")),
                None => b["report"].as_str().unwrap_or("").to_string(),
            })
            .collect();
        rows.push([
            time,
            o["schedule"].as_str().unwrap_or("").to_string(),
            if reports.is_empty() {
                "-".into()
            } else {
                reports.join(", ")
            },
        ]);
    }
    let mut out = String::new();
    if rows.len() == 1 {
        out.push_str(&format!(
            "No firings from {} to {}\n",
            doc["window"]["from"].as_str().unwrap_or(""),
            doc["window"]["to"].as_str().unwrap_or("")
        ));
    } else {
        let widths: Vec<usize> = (0..3)
            .map(|i| rows.iter().map(|r| r[i].chars().count()).max().unwrap_or(0))
            .collect();
        for r in rows {
            let line: Vec<String> = r
                .iter()
                .enumerate()
                .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
                .collect();
            out.push_str(line.join("  ").trim_end());
            out.push('\n');
        }
    }
    for (name, s) in doc["schedules"].as_object().into_iter().flatten() {
        if s["enabled"] == false {
            out.push_str(&format!("Paused: {name} (enabled: false)\n"));
        }
    }
    out
}
