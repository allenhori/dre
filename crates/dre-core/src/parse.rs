//! The parse pass: each Binding rendered without a database, as dbt's parse finds `ref()` and
//! `source()`, to learn every query's sources and the connection it runs on. Its results feed
//! selection (`-s source:`), the manifest, `dre ls` and `dre validate`, none of which connect;
//! a run repeats it with its own inputs and then holds rendering to what it found.
//!
//! A query's connection, by SPEC-011's rule:
//! - explicit: the query's own `profile:` and the `profile:` of every source it uses. These
//!   must agree (compared as rendered names), else it's an error naming both sides;
//! - otherwise the inherited one: the Set's, the report's, the folder's `+profile`, then
//!   `default_profile` (already merged into the Binding's `profile`).
//!
//! A source without a `profile:` never conflicts: it runs wherever its query runs.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use chrono::{DateTime, NaiveDate, Utc};
use serde_json::{Map as JsonMap, Value as Json};

use crate::dates::Calendar;
use crate::project::{Binding, Project, Report};
use crate::render::{Limited, Mode, Renderer, RendererConfig, ResolvedSource, RunContext, SourceResolver};

/// What the parse pass renders `run.*`, `var()` and `target.name` with: the run's inputs.
#[derive(Debug, Clone, Default)]
pub struct Inputs {
    pub target: String,
    /// `--var`.
    pub cli_vars: BTreeMap<String, String>,
    /// `DRE_RUN_DATE`.
    pub date: Option<NaiveDate>,
    /// `DRE_RUN_AT`.
    pub scheduled_at: Option<DateTime<Utc>>,
    /// `--timezone`/`DRE_TIMEZONE`.
    pub timezone: Option<String>,
    /// `--schedule`.
    pub schedule: Option<String>,
}

/// A problem found by the parse pass, at a file and line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub code: &'static str,
    pub file: PathBuf,
    pub line: Option<usize>,
    pub message: String,
}

/// One query of a Binding after the parse pass.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ParsedQuery {
    pub query: String,
    /// Its own `profile:`, rendered.
    pub profile: Option<String>,
    /// The connection it runs on; `None` when none resolves (an error says why).
    pub connection: Option<String>,
    /// `source.table` of every `source()` it calls (directly, through `ref()` or a macro), in
    /// order of first use.
    pub sources: Vec<String>,
}

/// A Binding after the parse pass.
#[derive(Debug, Clone, Default)]
pub struct ParsedBinding {
    /// The inherited connection, rendered.
    pub inherited: Option<String>,
    pub queries: Vec<ParsedQuery>,
    /// Each `output.destination` entry's `profile`, rendered (`None` where that failed).
    pub destinations: Vec<Option<String>>,
    /// Every source the queries use, resolved, by `source.table`.
    pub sources: BTreeMap<String, ResolvedSource>,
    pub errors: Vec<Problem>,
    pub warnings: Vec<Problem>,
}

impl ParsedBinding {
    pub fn query(&self, name: &str) -> Option<&ParsedQuery> {
        self.queries.iter().find(|q| q.query == name)
    }

    /// Every connection the queries use, in order of first use.
    pub fn connections(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for c in self.queries.iter().filter_map(|q| q.connection.as_deref()) {
            if !out.contains(&c) {
                out.push(c);
            }
        }
        out
    }

    /// `source.table` of every source used, in order of first use.
    pub fn source_keys(&self) -> Vec<&str> {
        let mut out: Vec<&str> = Vec::new();
        for s in self.queries.iter().flat_map(|q| q.sources.iter()) {
            if !out.contains(&s.as_str()) {
                out.push(s);
            }
        }
        out
    }
}

/// The run's calendar for a report: `--timezone`, else the schedule's, else the report's, else
/// UTC.
pub fn calendar(
    project: &Project,
    report: &Report,
    timezone: Option<&str>,
    schedule: Option<&str>,
) -> Calendar {
    let schedule_tz = schedule
        .and_then(|n| project.schedules.iter().find(|e| e.name == n))
        .and_then(|e| e.timezone.as_deref());
    let tz = timezone
        .or(schedule_tz)
        .or(report.timezone.as_deref())
        .and_then(|t| crate::dates::parse_tz(t).ok())
        .unwrap_or(chrono_tz::Tz::UTC);
    Calendar {
        tz,
        week_start: project.week_start,
        numbering: project.week_numbering,
    }
}

/// `run.date`: `DRE_RUN_DATE`, else `DRE_RUN_AT`'s date in the run's timezone, else today there.
pub fn run_date(cal: &Calendar, date: Option<NaiveDate>, scheduled_at: Option<DateTime<Utc>>) -> NaiveDate {
    date.or(scheduled_at.map(|t| t.with_timezone(&cal.tz).date_naive()))
        .unwrap_or_else(|| cal.today())
}

/// The `run.*` context a Binding renders with.
pub fn run_context(
    project: &Project,
    report: &Report,
    b: &Binding,
    inputs: &Inputs,
    now: DateTime<Utc>,
) -> RunContext {
    let calendar = calendar(
        project,
        report,
        inputs.timezone.as_deref(),
        inputs.schedule.as_deref(),
    );
    RunContext {
        report: report.name.clone(),
        set: b.set.clone(),
        target: inputs.target.clone(),
        schedule: inputs.schedule.clone(),
        date: run_date(&calendar, inputs.date, inputs.scheduled_at),
        now: inputs.scheduled_at.unwrap_or(now),
        scheduled_at: inputs.scheduled_at,
        calendar,
    }
}

/// Resolved sources by `(source, table)`, failures included.
type Resolved = BTreeMap<(String, String), Result<ResolvedSource, String>>;

/// Resolves `source()` for one Binding: fields rendered with `limited`, once each.
pub fn resolver(project: &Project, limited: Arc<Limited>) -> Option<SourceResolver> {
    if project.sources.is_empty() {
        return None;
    }
    let sources = project.sources.clone();
    let cache: Arc<Mutex<Resolved>> = Arc::default();
    Some(Arc::new(move |source: &str, table: &str| {
        let key = (source.to_string(), table.to_string());
        if let Some(r) = cache.lock().unwrap().get(&key) {
            return r.clone();
        }
        let r = (|| {
            let Some(s) = sources.get(source) else {
                return Err(format!(
                    "no source `{source}` is declared (declared: {})",
                    sources.keys().cloned().collect::<Vec<_>>().join(", ")
                ));
            };
            let Some(t) = s.table(table) else {
                return Err(format!(
                    "source `{source}` has no table `{table}` (it has: {})",
                    s.tables
                        .iter()
                        .map(|t| t.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            };
            let at = |key: &str| format!("source `{source}`: `{key}`");
            let render = |key: &str, v: &str| -> Result<String, String> {
                let out = limited.render(&at(key), v)?;
                if out.is_empty() {
                    return Err(format!("{} `{v}` renders empty", at(key)));
                }
                Ok(out)
            };
            Ok(ResolvedSource {
                source: source.to_string(),
                table: table.to_string(),
                profile: s.profile.as_deref().map(|p| render("profile", p)).transpose()?,
                database: s.database.as_deref().map(|d| render("database", d)).transpose()?,
                schema: render("schema", s.schema.as_deref().unwrap_or(&s.name))?,
                identifier: render("identifier", t.identifier.as_deref().unwrap_or(&t.name))?,
                quoting: t.quoting.over(&s.quoting),
            })
        })();
        cache.lock().unwrap().insert(key, r.clone());
        r
    }))
}

/// Parse one Binding: render each query without a database, then resolve its connection.
/// `vars` are the Binding's vars as the run sees them (with a schedule's layered in).
pub fn binding(
    project: &Project,
    report: &Report,
    b: &Binding,
    vars: &JsonMap<String, Json>,
    inputs: &Inputs,
) -> ParsedBinding {
    let ctx_name = match &b.set {
        Some(s) => format!("report `{}`, Set `{s}`", report.name),
        None => format!("report `{}`", report.name),
    };
    let context = run_context(project, report, b, inputs, Utc::now());
    let limited = Arc::new(Limited::new(
        context.clone(),
        vars.clone(),
        inputs.cli_vars.clone(),
    ));
    let mut out = ParsedBinding::default();
    let error =
        |out: &mut ParsedBinding, code: &'static str, file: PathBuf, line: Option<usize>, msg: String| {
            out.errors.push(Problem {
                code,
                file,
                line,
                message: msg,
            })
        };
    let render_profile = |out: &mut ParsedBinding, what: &str, raw: &str, file: &PathBuf| -> Option<String> {
        match limited.render(what, raw) {
            Ok(v) if !v.is_empty() => Some(v),
            Ok(_) => {
                out.errors.push(Problem {
                    code: "invalid-profile-value",
                    file: file.clone(),
                    line: None,
                    message: format!("{ctx_name}: {what} `{raw}` renders empty"),
                });
                None
            }
            Err(e) => {
                out.errors.push(Problem {
                    code: "invalid-profile-value",
                    file: file.clone(),
                    line: None,
                    message: format!("{ctx_name}: {e}"),
                });
                None
            }
        }
    };
    out.inherited = b
        .profile
        .as_deref()
        .and_then(|raw| render_profile(&mut out, "`profile`", raw, &report.file));
    let sources = resolver(project, limited.clone());
    let renderer = Renderer::new(RendererConfig {
        root: &project.root,
        macros: &project.macros,
        context,
        vars: vars.clone(),
        cli_vars: inputs.cli_vars.clone(),
        runner: None,
        connections: None,
        mode: Mode::Parse,
        connection: None,
        source_type: String::new(),
        sources: sources.clone(),
        run_query_max_rows: project.run_query_max_rows,
        sql: project.sql.clone(),
        lookups: project.lookups.clone(),
        lookup_inline_max_rows: project.lookup_inline_max_rows,
        packages: project.packages.clone(),
        project_name: project.name.clone(),
        dispatch: project.dispatch.clone(),
    });
    let renderer = match renderer {
        Ok(r) => Some(r),
        Err(e) => {
            error(
                &mut out,
                "parse-failed",
                e.file.clone(),
                e.line,
                format!("{ctx_name}: {}", e.message),
            );
            None
        }
    };
    for q in &b.queries {
        let mut pq = ParsedQuery {
            query: q.query.clone(),
            ..Default::default()
        };
        if let Some(raw) = &q.profile {
            pq.profile = render_profile(
                &mut out,
                &format!("`profile` of query `{}`", q.query),
                raw,
                &report.file,
            );
        }
        let Some(r) = &renderer else {
            out.queries.push(pq);
            continue;
        };
        let src = match std::fs::read_to_string(project.root.join(&q.path)) {
            Ok(s) => s,
            Err(e) => {
                error(
                    &mut out,
                    "parse-failed",
                    q.path.clone(),
                    None,
                    format!("can't read it: {e}"),
                );
                out.queries.push(pq);
                continue;
            }
        };
        let _ = r.take_sources();
        if let Err(e) = r.render(&q.path, &src) {
            error(
                &mut out,
                "parse-failed",
                e.file.clone(),
                e.line,
                format!("{ctx_name}: query `{}` doesn't parse: {}", q.query, e.message),
            );
            out.queries.push(pq);
            continue;
        }
        let mut explicit: Vec<(String, String)> = Vec::new();
        if let Some(p) = &pq.profile {
            explicit.push((format!("query `{}` sets `profile: {p}`", q.query), p.clone()));
        }
        for (s, t) in r.take_sources() {
            let resolve = sources
                .as_ref()
                .expect("source() succeeded, so sources are declared");
            let rs = resolve(&s, &t).expect("resolved during the render");
            let key = rs.key();
            if let Some(p) = &rs.profile {
                explicit.push((format!("source `{key}` is on connection `{p}`"), p.clone()));
            }
            out.sources.entry(key.clone()).or_insert(rs);
            pq.sources.push(key);
        }
        pq.connection = match explicit.split_first() {
            Some(((first_why, first), rest)) => {
                if let Some((why, _)) = rest.iter().find(|(_, p)| p != first) {
                    error(
                        &mut out,
                        "connection-conflict",
                        q.path.clone(),
                        None,
                        format!(
                            "{ctx_name}: query `{}` can't run on one connection: {first_why}, but {why}; one query runs on one connection",
                            q.query
                        ),
                    );
                    None
                } else {
                    Some(first.clone())
                }
            }
            None => out.inherited.clone(),
        };
        if pq.connection.is_none()
            && !out
                .errors
                .iter()
                .any(|e| e.file == q.path || e.file == report.file)
        {
            error(
                &mut out,
                "no-connection",
                report.file.clone(),
                None,
                format!(
                    "{ctx_name}: query `{}` has no connection: give it `profile:`, use a source with a `profile:`, or give the report one (`profile:` on the report or Set, a folder's `+profile`, or `default_profile` in dre_project.yml)",
                    q.query
                ),
            );
        }
        out.queries.push(pq);
    }
    for d in &b.output.destinations {
        let p = render_profile(&mut out, "destination `profile`", &d.profile, &report.file);
        out.destinations.push(p);
    }
    setup_warnings(report, b, &mut out);
    out
}

/// A `tab: false` query prepares what later queries on its connection read (temp tables,
/// `SET`s). One whose connection no later tab uses, while later tabs run elsewhere, can't reach
/// them.
fn setup_warnings(report: &Report, b: &Binding, out: &mut ParsedBinding) {
    let conn = |name: &str| out.query(name).and_then(|q| q.connection.clone());
    let mut warnings = Vec::new();
    for (i, q) in b.queries.iter().enumerate() {
        if q.tab {
            continue;
        }
        let Some(c) = conn(&q.query) else { continue };
        let later: Vec<(String, String)> = b.queries[i + 1..]
            .iter()
            .filter(|l| l.tab)
            .filter_map(|l| Some((l.query.clone(), conn(&l.query)?)))
            .collect();
        if later.is_empty() || later.iter().any(|(_, lc)| *lc == c) {
            continue;
        }
        let (other, oc) = &later[0];
        warnings.push(Problem {
            code: "setup-on-other-connection",
            file: report.file.clone(),
            line: None,
            message: format!(
                "report `{}`: setup query `{}` (`tab: false`) runs on connection `{c}`, but the tabs after it run elsewhere (`{other}` on `{oc}`); its temp tables and `SET`s are only visible on `{c}`",
                report.name, q.query
            ),
        });
    }
    out.warnings.extend(warnings);
}
