//! The run path: turn a resolved project into delivered files.
//!
//! Per Binding: parse pass → render → split → (unmanaged check) → execute, each query on its
//! connection's session (one per connection, opened on first use, in strict YAML order) →
//! format into `target/run/` → schema-drift check → deliver → snapshot → `run_results.json`.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use arrow::ipc::reader::FileReader;
use arrow::ipc::writer::FileWriter;
use chrono::NaiveDate;
use dre_protocol::host::{Execution, HostError, LogSink, PluginProcess};
use dre_protocol::msg::{ColumnOptions, DeliveryFile, ResultSetMeta};
use dre_protocol::{CAP_LOAD, CAP_MULTI_FILE, CAP_READ_ONLY, CAP_SESSIONS};
use serde::Serialize;
use serde_json::{Map as JsonMap, Value as Json, json};

use crate::dates::Calendar;
use crate::lookups::Table;
use crate::parse::ParsedBinding;
use crate::profiles::{BUILTIN_LOCAL, LOCAL_TYPE, ProfileTarget, Profiles, Role};
use crate::project::{Binding, PluginKind, Project, QueryEntry, Report};
use crate::render::{
    Column, Connection, Connections, Mode, QueryRows, QueryRunner, RenderError, Renderer, RendererConfig,
};
use crate::selector;
use crate::sqlsplit::{self, StatementKind};

/// What the caller asked for.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub selector: Option<String>,
    /// `--set <name>` or `--set all`.
    pub set: Option<String>,
    /// `--target` as given; the run's target is [`Project::target_name`].
    pub target: Option<String>,
    /// `--profile`: the inherited connection, for every Binding.
    pub profile: Option<String>,
    pub vars: BTreeMap<String, String>,
    pub output_name: Option<String>,
    pub output_path: Option<String>,
    pub dry_run: bool,
    /// `--preview [N]`: row limit per query.
    pub preview: Option<u64>,
    pub accept_schema_change: bool,
    /// Whether a person is at a terminal to answer prompts.
    pub interactive: bool,
    /// `run.date` (`DRE_RUN_DATE`).
    pub date: Option<NaiveDate>,
    /// The instant this run was scheduled for (`DRE_RUN_AT`): `run.now` and `run.scheduled_at`,
    /// and `run.date` in the run's timezone unless `date` is set.
    pub scheduled_at: Option<chrono::DateTime<chrono::Utc>>,
    /// `validate --live`: check statements instead of executing them.
    pub live_check: bool,
    /// `--schedule <name>`: run exactly the Bindings that schedule targets, with its vars.
    pub schedule: Option<String>,
    /// `--timezone` or `DRE_TIMEZONE`: above every configured `timezone:`.
    pub timezone: Option<String>,
    /// SHA-256 of the manifest this command wrote, for `run_results.json`.
    pub manifest_checksum: Option<String>,
}

impl RunOptions {
    /// What the run was asked to do, for `run_results.json` and the log.
    pub fn params(&self, date: NaiveDate) -> Json {
        json!({
            "selector": self.selector,
            "set": self.set,
            "schedule": self.schedule,
            "target": self.target,
            "profile": self.profile,
            "vars": self.vars,
            "run_date": date.to_string(),
            "scheduled_at": self.scheduled_at.map(rfc3339),
            "timezone": self.timezone,
            "output_name": self.output_name,
            "output_path": self.output_path,
            "dry_run": self.dry_run,
            "preview": self.preview,
            "accept_schema_change": self.accept_schema_change,
        })
    }
}

/// How much a message matters: `Info` is shown by default, `Debug` with `-v`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Info,
}

/// How the run reports progress. The engine emits structured events; the CLI decides how
/// they look (colour, progress bar, JSON, log file).
pub trait Ui {
    /// The number of Bindings about to run, once Sets are resolved.
    fn plan(&mut self, _bindings: usize) {}
    fn binding_start(&mut self, _report: &str, _set: Option<&str>) {}
    /// Right after `binding_start`: the schedule it runs under (if any) and every var it uses.
    fn binding_vars(
        &mut self,
        _schedule: Option<&str>,
        _schedule_vars: Option<&JsonMap<String, Json>>,
        _vars: &JsonMap<String, Json>,
    ) {
    }
    /// One step inside the current Binding: a short verb, a detail, and how long it took.
    fn step(&mut self, level: Level, verb: &str, detail: &str, elapsed: Option<Duration>);
    fn warn(&mut self, msg: &str);
    fn binding_end(&mut self, _outcome: &BindingOutcome) {}
    /// A Binding was compiled (`--dry-run`, `dre compile`, `dre validate`): what it would do.
    fn compiled(&mut self, _plan: &BindingPlan) {}
    /// Ask which Set to run; `None` means "all".
    fn choose_set(&mut self, report: &str, sets: &[String]) -> Result<Option<String>, String>;
    /// Where plugin stderr goes.
    fn plugin_log(&self) -> LogSink;
    /// Where the full text of every statement sent to a source goes: `(label, sql)`.
    fn sql_log(&self) -> LogSink {
        Arc::new(|_, _| {})
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Success,
    Error,
    DryRun,
    Checked,
}

#[derive(Debug, Clone, Serialize)]
pub struct BindingOutcome {
    pub report: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set: Option<String>,
    pub binding: String,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub files: Vec<PathBuf>,
    /// One line describing what the Binding produced (result sets, rows, outputs, delivery).
    pub summary: String,
    /// The schedule this ran under (`--schedule`), and the vars it layered in.
    pub schedule: Option<String>,
    pub schedule_vars: Option<JsonMap<String, Json>>,
    /// Every var the Binding rendered with: its own, the schedule's, then `--var`.
    pub vars: JsonMap<String, Json>,
    /// The run's timezone (IANA name); empty when the Binding never started.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub timezone: String,
    #[serde(skip)]
    pub elapsed: Duration,
}

/// What a compiled Binding would use and produce if it ran.
#[derive(Debug, Clone, Serialize)]
pub struct BindingPlan {
    pub report: String,
    pub set: Option<String>,
    /// Rendered SQL files, relative to the project.
    pub compiled: Vec<PathBuf>,
    /// The inherited connection, if any.
    pub profile: Option<String>,
    /// Each query's connection, its type and the sources it reads.
    pub queries: Vec<PlannedQuery>,
    pub target: String,
    pub format: String,
    /// The file written under `target/run/` (a single-table format writes one per result set
    /// when there are several).
    pub output: PathBuf,
    pub destinations: Vec<PlannedDestination>,
    pub schedules: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedQuery {
    pub query: String,
    pub connection: String,
    #[serde(rename = "type")]
    pub kind: String,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct PlannedDestination {
    pub profile: String,
    /// The destination type for the active target, when it has one.
    pub kind: Option<String>,
    pub target: Option<String>,
    pub path: Option<String>,
    /// False when the profile has no target for this run: the output stays in `target/`.
    pub delivers: bool,
}

#[derive(Debug, Default)]
pub struct RunSummary {
    pub outcomes: Vec<BindingOutcome>,
    /// Selection failed before anything ran.
    pub error: Option<String>,
}

impl RunSummary {
    pub fn failed(&self) -> bool {
        self.error.is_some() || self.outcomes.iter().any(|o| o.status == Status::Error)
    }
}

pub fn run(project: &Project, opts: &RunOptions, ui: &mut dyn Ui) -> RunSummary {
    let mut summary = RunSummary::default();
    if let Err(e) = crate::target::ensure(&project.target_dir) {
        summary.error = Some(format!(
            "can't write to the target path {} (from {}): {e}",
            project.target_dir.display(),
            project.target_source
        ));
        return summary;
    }
    if let Some(name) = &opts.schedule {
        if !project.schedules.iter().any(|e| &e.name == name) {
            summary.error = Some(unknown_schedule(project, name));
            return summary;
        }
        let planned = match schedule_bindings(project, name, opts) {
            Ok(p) => p,
            Err(e) => {
                summary.error = Some(e);
                return summary;
            }
        };
        ui.plan(planned.len());
        for (report, b) in &planned {
            ui.binding_start(&report.name, b.set.as_deref());
            let mut r = BindingRun::new(project, report, b, opts, ui);
            let outcome = r.run();
            ui.binding_end(&outcome);
            summary.outcomes.push(outcome);
        }
        return summary;
    }
    let reports: Vec<&Report> = match &opts.selector {
        None => project.reports.iter().collect(),
        Some(s) => match selector::resolve(project, s) {
            Ok(r) if r.is_empty() => {
                summary.error = Some(format!("selector `{s}` matches no report"));
                return summary;
            }
            Ok(r) => r,
            Err(e) => {
                summary.error = Some(e.to_string());
                return summary;
            }
        },
    };
    // Resolve every report's Bindings first (prompts happen here), so progress has a total.
    let mut planned: Vec<(&Report, Binding)> = Vec::new();
    for report in reports {
        match choose_bindings(project, report, opts, ui) {
            Ok(bs) => planned.extend(bs.into_iter().map(|b| (report, b))),
            Err(e) => {
                let outcome = BindingOutcome {
                    report: report.name.clone(),
                    set: None,
                    binding: "-".into(),
                    status: Status::Error,
                    error: Some(e),
                    files: Vec::new(),
                    summary: String::new(),
                    schedule: opts.schedule.clone(),
                    schedule_vars: None,
                    vars: JsonMap::new(),
                    timezone: String::new(),
                    elapsed: Duration::ZERO,
                };
                ui.binding_end(&outcome);
                summary.outcomes.push(outcome);
            }
        }
    }
    ui.plan(planned.len());
    for (report, b) in &planned {
        ui.binding_start(&report.name, b.set.as_deref());
        let mut r = BindingRun::new(project, report, b, opts, ui);
        let outcome = r.run();
        ui.binding_end(&outcome);
        summary.outcomes.push(outcome);
    }
    summary
}

/// The Bindings `--schedule <name>` runs: all of them, or those a selector and/or `--set` pick.
/// Picking something the schedule doesn't run is an error naming what it does run.
fn schedule_bindings<'a>(
    project: &'a Project,
    name: &str,
    opts: &RunOptions,
) -> Result<Vec<(&'a Report, Binding)>, String> {
    let reports: Option<Vec<&str>> = match &opts.selector {
        None => None,
        Some(s) => Some(
            selector::resolve(project, s)
                .map_err(|e| e.to_string())?
                .into_iter()
                .map(|r| r.name.as_str())
                .collect(),
        ),
    };
    let set = opts.set.as_deref().filter(|s| *s != "all");
    let all: Vec<(&Report, &Binding)> = project
        .reports
        .iter()
        .flat_map(|r| {
            r.bindings
                .iter()
                .filter(|b| b.schedules.iter().any(|s| s == name))
                .map(move |b| (r, b))
        })
        .collect();
    let picked: Vec<(&Report, Binding)> = all
        .iter()
        .filter(|(r, _)| reports.as_ref().is_none_or(|rs| rs.contains(&r.name.as_str())))
        .filter(|(_, b)| set.is_none() || b.set.as_deref() == set)
        .map(|(r, b)| {
            let mut b = (*b).clone();
            if let Some(p) = &opts.profile {
                b.profile = Some(p.clone());
                b.profile_at = Some(profile_flag_at());
            }
            (*r, b)
        })
        .collect();
    if picked.is_empty() && (reports.is_some() || set.is_some()) {
        let mut asked = Vec::new();
        if let Some(s) = &opts.selector {
            asked.push(format!("`{s}`"));
        }
        if let Some(s) = set {
            asked.push(format!("Set `{s}`"));
        }
        let runs: Vec<String> = all
            .iter()
            .map(|(r, b)| match &b.set {
                Some(s) => format!("{}/{s}", r.name),
                None => r.name.clone(),
            })
            .collect();
        return Err(format!(
            "schedule `{name}` doesn't run {}; it runs: {}",
            asked.join(" with "),
            if runs.is_empty() {
                "nothing".to_string()
            } else {
                runs.join(", ")
            }
        ));
    }
    Ok(picked)
}

/// The usage error for `--schedule` with a name that isn't declared.
pub fn unknown_schedule(project: &Project, name: &str) -> String {
    let names: Vec<&str> = project.schedules.iter().map(|e| e.name.as_str()).collect();
    if names.is_empty() {
        format!("no schedule `{name}`: this project's schedules.yml declares none")
    } else {
        format!("no schedule `{name}`; valid names: {}", names.join(", "))
    }
}

/// Which Bindings of a report run, following ADR 0009's Set rules.
pub fn choose_bindings(
    project: &Project,
    report: &Report,
    opts: &RunOptions,
    ui: &mut dyn Ui,
) -> Result<Vec<Binding>, String> {
    let mut out: Vec<Binding> = match opts.set.as_deref() {
        Some("all") => report.bindings.clone(),
        Some(name) => match report.binding(name) {
            Some(b) => vec![b.clone()],
            None => vec![ad_hoc(project, report, name)],
        },
        None if !report.has_sets => report.bindings.clone(),
        None => {
            let names: Vec<String> = report.bindings.iter().filter_map(|b| b.set.clone()).collect();
            let pick = report
                .default_set
                .clone()
                .or_else(|| project.default_set.clone().filter(|d| names.contains(d)))
                .or_else(|| (names.len() == 1).then(|| names[0].clone()));
            match pick {
                Some(p) => report.binding(&p).cloned().into_iter().collect(),
                None if opts.interactive => match ui.choose_set(&report.name, &names)? {
                    Some(p) => report.binding(&p).cloned().into_iter().collect(),
                    None => report.bindings.clone(),
                },
                None => {
                    return Err(format!(
                        "report `{}` has several Sets ({}) and no `default_set`; declare `default_set:` or pass `--set <name>` or `--set all`",
                        report.name,
                        names.join(", ")
                    ));
                }
            }
        }
    };
    if let Some(p) = &opts.profile {
        for b in &mut out {
            b.profile = Some(p.clone());
            b.profile_at = Some(profile_flag_at());
        }
    }
    Ok(out)
}

/// A Set that isn't declared on the report: start from the report itself, then apply the
/// `sets.yml` entry of that name if there is one. `--profile`/`--var` complete it.
fn ad_hoc(project: &Project, report: &Report, name: &str) -> Binding {
    let mut b = report.base.clone();
    b.set = Some(name.to_string());
    if let Some(reg) = project.sets.get(name) {
        if let Some(p) = &reg.profile {
            b.profile = Some(p.clone());
            b.profile_at = Some(crate::project::ProfileAt {
                file: reg.file.clone(),
                line: reg.line,
                key: format!("`profile` of Set `{name}`"),
            });
        }
        b.vars.extend(reg.vars.clone());
    }
    b
}

/// `--profile` as the source of a Binding's inherited connection, for messages.
fn profile_flag_at() -> crate::project::ProfileAt {
    crate::project::ProfileAt {
        file: PathBuf::from("--profile"),
        line: None,
        key: "`--profile`".into(),
    }
}

/// The tab one query produced: its last statement's result set, spooled to disk.
struct Produced {
    query: String,
    connection: String,
    schema: SchemaRef,
    rows: u64,
    spool: PathBuf,
    name: String,
    anchor: Option<String>,
    header: Option<bool>,
    columns: BTreeMap<String, ColumnOptions>,
}

struct Statement {
    query: String,
    /// The connection it runs on.
    connection: String,
    file: PathBuf,
    line: usize,
    text: String,
    kind: StatementKind,
    /// The complete rendered query contained a registered secret before it was split. Every
    /// statement inherits this because splitting can itself separate a secret into fragments.
    sensitive: bool,
    /// Whether this statement's result is its query's tab: the file's last statement, when the
    /// query has `tab: true`.
    tab: bool,
}

struct BindingRun<'a> {
    project: &'a Project,
    report: &'a Report,
    b: &'a Binding,
    /// The Binding's vars with the schedule's layered on (`--var` stays separate, on top).
    vars: JsonMap<String, Json>,
    /// Everything `var()` sees, `--var` included: what's recorded.
    rendered_vars: JsonMap<String, Json>,
    schedule_vars: Option<JsonMap<String, Json>>,
    opts: &'a RunOptions,
    /// `run.date`: `DRE_RUN_DATE`, else today in the run's timezone.
    date: NaiveDate,
    calendar: Calendar,
    ui: &'a mut dyn Ui,
    compiled_dir: PathBuf,
    run_dir: PathBuf,
    schema_dir: PathBuf,
    /// The run's target (environment).
    target: String,
    /// The parse pass for this run's inputs.
    parsed: ParsedBinding,
    /// The Binding's connections and their sessions.
    pool: Option<Arc<Pool>>,
    /// One renderer per connection (`None`: the inherited one, for paths and template values).
    renderers: BTreeMap<Option<String>, Arc<Renderer>>,
    started: Instant,
    started_at: chrono::DateTime<chrono::Utc>,
    produced: Vec<Produced>,
    files: Vec<(PathBuf, Option<String>)>,
    /// The Binding's destinations with paths and options rendered.
    dests: Vec<RenderedDest>,
    /// One record per destination: `run_results.json`'s `deliveries`.
    deliveries: Vec<Json>,
    delivery_note: Option<String>,
    drift: Vec<String>,
    /// The current schema with stable identities and any protection carried from the baseline.
    protected_snapshot: Option<Json>,
}

/// A destination entry after rendering.
struct RenderedDest {
    profile: String,
    path: Option<String>,
    options: JsonMap<String, Json>,
}

type Fail = String;

fn masked_source_error(error: &HostError, sensitive: bool) -> String {
    let error = error.to_string();
    crate::secrets::mask_source_error(&error, sensitive).into_owned()
}

fn masked_sql_summary(sql: &str, max: usize, sensitive: bool) -> String {
    if sensitive {
        crate::secrets::MASK.to_string()
    } else {
        dre_protocol::util::summarize(&crate::secrets::mask(sql), max)
    }
}

fn protected_sql(sql: &str, sensitive: bool) -> std::borrow::Cow<'_, str> {
    if sensitive {
        std::borrow::Cow::Borrowed(crate::secrets::MASK)
    } else {
        crate::secrets::mask(sql)
    }
}

impl<'a> BindingRun<'a> {
    fn new(
        project: &'a Project,
        report: &'a Report,
        b: &'a Binding,
        opts: &'a RunOptions,
        ui: &'a mut dyn Ui,
    ) -> Self {
        let t = &project.target_dir;
        let rel = Path::new(&report.name).join(b.dir_name());
        let schedule = opts
            .schedule
            .as_ref()
            .and_then(|n| project.schedules.iter().find(|e| &e.name == n));
        let schedule_vars = schedule.map(|e| e.vars.clone());
        // Names were checked when the project and the command line were read.
        let calendar: Calendar = crate::parse::calendar(
            project,
            report,
            opts.timezone.as_deref(),
            opts.schedule.as_deref(),
        );
        let date = crate::parse::run_date(&calendar, opts.date, opts.scheduled_at);
        // Binding vars, then the schedule's, then `--var` on top.
        let mut vars = b.vars.clone();
        vars.extend(schedule_vars.clone().unwrap_or_default());
        let rendered_vars = {
            let mut v = vars.clone();
            v.extend(
                opts.vars
                    .iter()
                    .map(|(k, x)| (k.clone(), Json::String(x.clone()))),
            );
            v
        };
        ui.binding_vars(opts.schedule.as_deref(), schedule_vars.as_ref(), &rendered_vars);
        BindingRun {
            project,
            report,
            b,
            vars,
            rendered_vars,
            schedule_vars,
            opts,
            date,
            calendar,
            ui,
            compiled_dir: t.join("compiled").join(&rel),
            run_dir: t.join("run").join(&rel),
            schema_dir: t.join("schema").join(&rel),
            target: project.target_name.clone(),
            parsed: ParsedBinding::default(),
            pool: None,
            renderers: BTreeMap::new(),
            started: Instant::now(),
            started_at: chrono::Utc::now(),
            produced: Vec::new(),
            files: Vec::new(),
            dests: Vec::new(),
            deliveries: Vec::new(),
            delivery_note: None,
            drift: Vec::new(),
            protected_snapshot: None,
        }
    }

    /// What `connection`, `destination` and `profile()` read for this Binding.
    fn connections(&self) -> Arc<ProfileConnections> {
        Arc::new(ProfileConnections {
            profiles: self.project.profiles.clone(),
            target: self.target.clone(),
            root: self.project.root.clone(),
            plugins: self.project.plugins.clone(),
            log: self.ui.plugin_log(),
        })
    }

    /// The renderer for SQL on `connection` (`None`: the inherited connection, for output paths
    /// and template values), made on first use.
    fn renderer(&mut self, connection: Option<&str>) -> Result<Arc<Renderer>, Fail> {
        let key = connection.map(str::to_string);
        if let Some(r) = self.renderers.get(&key) {
            return Ok(r.clone());
        }
        let pool = self.pool.clone().expect("the pool is made before rendering");
        // Outside query SQL, `connection.*` and `run_query()` mean the inherited connection.
        let name = key.clone().or_else(|| self.parsed.inherited.clone());
        let source_type = match (&key, &name) {
            (Some(n), _) => pool.kind(n)?,
            // The inherited connection's type matters only if something uses it.
            (None, Some(n)) => pool.kind(n).unwrap_or_default(),
            (None, None) => String::new(),
        };
        let context =
            crate::parse::run_context(self.project, self.report, self.b, &self.inputs(), self.started_at);
        let limited = Arc::new(crate::render::Limited::new(
            context.clone(),
            self.vars.clone(),
            self.opts.vars.clone(),
        ));
        let r = Renderer::new(RendererConfig {
            root: &self.project.root,
            macros: &self.project.macros,
            context,
            vars: self.vars.clone(),
            cli_vars: self.opts.vars.clone(),
            runner: Some(Arc::new(PoolRunner {
                pool: pool.clone(),
                default: name.clone(),
                log: self.ui.sql_log(),
            })),
            connections: Some(pool.connections.clone()),
            mode: Mode::Run,
            connection: name,
            source_type,
            sources: crate::parse::resolver(self.project, limited),
            run_query_max_rows: self.project.run_query_max_rows,
            sql: self.project.sql.clone(),
            lookups: self.project.lookups.clone(),
            lookup_inline_max_rows: self.project.lookup_inline_max_rows,
            packages: self.project.packages.clone(),
            project_name: self.project.name.clone(),
            dispatch: self.project.dispatch.clone(),
        })
        .map_err(|e| e.to_string())?;
        let r = Arc::new(r);
        self.renderers.insert(key, r.clone());
        Ok(r)
    }

    /// The parse pass's inputs for this run.
    fn inputs(&self) -> crate::parse::Inputs {
        crate::parse::Inputs {
            target: self.target.clone(),
            cli_vars: self.opts.vars.clone(),
            date: self.opts.date,
            scheduled_at: self.opts.scheduled_at,
            timezone: self.opts.timezone.clone(),
            schedule: self.opts.schedule.clone(),
            started_at: Some(self.started_at),
        }
    }

    /// Each query's connection, from the parse pass.
    fn connection_of(&self, query: &str) -> Result<String, Fail> {
        self.parsed
            .query(query)
            .and_then(|q| q.connection.clone())
            .ok_or_else(|| format!("query `{query}` has no connection"))
    }

    fn outcome(&self, status: Status, error: Option<String>) -> BindingOutcome {
        BindingOutcome {
            report: self.report.name.clone(),
            set: self.b.set.clone(),
            binding: self.b.dir_name().to_string(),
            status,
            error,
            files: self.files.iter().map(|(p, _)| p.clone()).collect(),
            summary: self.summary_line(),
            schedule: self.opts.schedule.clone(),
            schedule_vars: self.schedule_vars.clone(),
            vars: self.rendered_vars.clone(),
            timezone: self.calendar.tz.name().to_string(),
            elapsed: self.started.elapsed(),
        }
    }

    fn run(&mut self) -> BindingOutcome {
        let dry = self.opts.dry_run || self.opts.live_check;
        if !dry {
            let _ = std::fs::remove_dir_all(&self.run_dir);
        }
        let result = self.run_inner();
        let status = match (&result, dry, self.opts.live_check) {
            (Err(_), _, _) => Status::Error,
            (Ok(()), _, true) => Status::Checked,
            (Ok(()), true, _) => Status::DryRun,
            (Ok(()), false, _) => Status::Success,
        };
        let err = result.err();
        if !dry && let Err(e) = self.write_results(&status, err.as_deref()) {
            self.ui.warn(&format!("can't write run_results.json: {e}"));
        }
        // Spools are scratch space.
        let _ = std::fs::remove_dir_all(self.run_dir.join(".spool"));
        self.outcome(status, err)
    }

    fn run_inner(&mut self) -> Result<(), Fail> {
        // The parse pass with this run's inputs: each query's connection and sources.
        self.parsed = crate::parse::binding(self.project, self.report, self.b, &self.vars, &self.inputs());
        if !self.parsed.errors.is_empty() {
            let msgs: Vec<String> = self
                .parsed
                .errors
                .iter()
                .map(|p| match p.line {
                    Some(l) => format!("{}:{l}: {}", p.file.display(), p.message),
                    None => format!("{}: {}", p.file.display(), p.message),
                })
                .collect();
            return Err(msgs.join("\n    "));
        }
        // Sessions are opened only when something needs the database, so compiling a report
        // whose templates don't query it works without the plugin or credentials.
        let pool = Arc::new(Pool::new(
            self.connections(),
            self.project.root.clone(),
            self.ui.plugin_log(),
            !self.report.managed,
        ));
        self.pool = Some(pool.clone());

        if !self.report.managed {
            self.ui.warn(&format!(
                "`{}` is an unmanaged report ({}): for quick tests only — add a YAML to make it a managed report",
                self.report.name,
                self.report.file.display()
            ));
        }

        // 1. Render.
        std::fs::create_dir_all(&self.compiled_dir).map_err(|e| e.to_string())?;
        // How many queries run on each connection: one session holds them all.
        let mut per_connection: BTreeMap<String, usize> = BTreeMap::new();
        for q in &self.b.queries {
            *per_connection.entry(self.connection_of(&q.query)?).or_default() += 1;
        }
        // A real run of a managed report renders each query just before running it, so a
        // template can look at what earlier queries made (`columns()` of a temp table).
        // Compiling, checking and unmanaged reports render everything first.
        let interleave = !self.opts.dry_run && !self.opts.live_check && self.report.managed;
        let mut statements = Vec::new();
        // Reported after the unmanaged-report check, which matters more.
        let mut two_tabs: Option<String> = None;
        if interleave {
            self.dests = self.render_destinations()?;
            let mut checked: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
            for (c, n) in &per_connection {
                if *n > 1 {
                    self.check_sessions(&pool, c, *n)?;
                    checked.insert(c.clone());
                }
            }
            std::fs::create_dir_all(self.run_dir.join(".spool")).map_err(|e| e.to_string())?;
            let mut i = 0;
            let queries = self.b.queries.clone();
            for q in &queries {
                let conn = self.connection_of(&q.query)?;
                let renderer = self.renderer(Some(&conn))?;
                let sts = self.render_query(&renderer, q, &conn, &mut two_tabs)?;
                for w in renderer.take_warnings() {
                    self.ui.warn(&w);
                }
                if let Some(e) = two_tabs.take() {
                    return Err(e);
                }
                if sts.len() > 1 && checked.insert(conn.clone()) {
                    self.check_sessions(&pool, &conn, sts.len())?;
                }
                let session = pool.session(&conn)?;
                for st in &sts {
                    self.execute_statement(&session, i, st)?;
                    i += 1;
                }
            }
        } else {
            let queries = self.b.queries.clone();
            for q in &queries {
                let conn = self.connection_of(&q.query)?;
                let renderer = self.renderer(Some(&conn))?;
                statements.extend(self.render_query(&renderer, q, &conn, &mut two_tabs)?);
            }
            self.dests = self.render_destinations()?;
            for r in self.renderers.values() {
                for w in r.take_warnings() {
                    self.ui.warn(&w);
                }
            }

            // 3. Unmanaged: every rendered statement must only read (or create temp objects).
            if !self.report.managed {
                if let Some(bad) = statements.iter().find(|s| !s.kind.is_read_only_safe()) {
                    return Err(format!(
                        "{}:{}: unmanaged report `{}` may only run SELECT/WITH or CREATE [OR REPLACE] TEMP|TEMPORARY TABLE|VIEW, but found `{}`; nothing was run — rewrite the statement, or give the report a YAML to declare it",
                        bad.file.display(),
                        bad.line,
                        self.report.name,
                        masked_sql_summary(&bad.text, 60, bad.sensitive)
                    ));
                }
                for conn in per_connection.keys() {
                    let session = pool.session(conn)?;
                    let creates_temp = statements
                        .iter()
                        .any(|s| &s.connection == conn && s.kind == StatementKind::TempCreate)
                        || session.lock().unwrap().loaded;
                    session.lock().unwrap().want_read_only(!creates_temp);
                }
            }
            if let Some(e) = two_tabs {
                return Err(e);
            }

            if self.opts.dry_run {
                let plan = self.plan();
                self.ui.compiled(&plan);
                return Ok(());
            }

            // The one-session-per-connection guarantee.
            for conn in per_connection.keys() {
                let n = statements.iter().filter(|s| &s.connection == conn).count();
                if n > 1 {
                    self.check_sessions(&pool, conn, n)?;
                }
            }

            if self.opts.live_check {
                return self.live_check(&statements);
            }

            // 4. Execute in YAML order, each on its connection's session.
            std::fs::create_dir_all(self.run_dir.join(".spool")).map_err(|e| e.to_string())?;
            for (i, st) in statements.iter().enumerate() {
                let session = pool.session(&st.connection)?;
                self.execute_statement(&session, i, st)?;
            }
        }
        pool.close_all();

        self.name_result_sets()?;

        // 5. Format into target/run/.
        let filename = self.file_names();
        self.format(&filename)?;

        // Schema drift, before delivery.
        if self.opts.preview.is_none() {
            self.drift = self.schema_drift();
            if !self.drift.is_empty() {
                if self.opts.accept_schema_change {
                    self.ui.warn(&format!(
                        "  schema changed since the last successful run (accepted): {}",
                        self.drift.join("; ")
                    ));
                } else {
                    return Err(format!(
                        "schema drift since the last successful run: {}; the output is in {} but was not delivered — pass --accept-schema-change to deliver it and accept the new schema",
                        self.drift.join("; "),
                        rel(&self.project.root, &self.run_dir).display()
                    ));
                }
            }
        }

        // 6. Deliver.
        if self.opts.preview.is_some() {
            self.delivery_note = Some("preview: not delivered".into());
            let dir = rel(&self.project.root, &self.run_dir);
            self.ui.step(
                Level::Info,
                "Preview",
                &format!("not delivered; output stays in {}", dir.display()),
                None,
            );
        } else {
            self.deliver()?;
        }

        // 7. Snapshot the schema for the next drift check.
        if self.opts.preview.is_none()
            && let Err(error) = self.write_snapshot()
        {
            self.ui.warn(&format!(
                "delivery completed, but the schema snapshot could not be written: {error}; the next run may compare against an older schema"
            ));
        }
        Ok(())
    }

    /// Render one query file into its statements, writing it to `target/compiled/`.
    fn render_query(
        &mut self,
        renderer: &Renderer,
        q: &QueryEntry,
        connection: &str,
        two_tabs: &mut Option<String>,
    ) -> Result<Vec<Statement>, Fail> {
        let mut out = Vec::new();
        let src = std::fs::read_to_string(self.project.root.join(&q.path))
            .map_err(|e| format!("{}: {e}", q.path.display()))?;
        let _ = renderer.take_sources();
        let sql = renderer
            .render(&q.path, &src)
            .map_err(|e: RenderError| e.to_string())?;
        // The manifest's `depends_on` and the query's connection come from the parse pass, so
        // a `source()` it couldn't see would make both wrong.
        let known = self
            .parsed
            .query(&q.query)
            .map(|p| p.sources.clone())
            .unwrap_or_default();
        if let Some((s, t)) = renderer
            .take_sources()
            .into_iter()
            .find(|(s, t)| !known.contains(&format!("{s}.{t}")))
        {
            return Err(format!(
                "{}: `source('{s}', '{t}')` was reached while rendering `{}`, but the parse pass didn't find it. The parse pass renders without data (`run_query()` returns no rows, `connection.*` nothing), so it can't see a `source()` that depends on them; call it where it's reached either way",
                q.path.display(),
                q.query
            ));
        }
        let sensitive = crate::secrets::contains_secret(&sql);
        std::fs::write(
            self.compiled_dir.join(format!("{}.sql", q.query)),
            crate::secrets::mask(&sql).as_bytes(),
        )
        .map_err(|e| e.to_string())?;
        self.ui
            .step(Level::Debug, "Rendered", &q.path.display().to_string(), None);
        // 2. Split. One file makes at most one tab, from its last statement, so any earlier
        // SELECT would be lost: that's an error, not a guess.
        let parts = sqlsplit::split(&sql);
        if q.tab && parts.is_empty() {
            return Err(format!(
                "{}: `{}` makes a tab but has no statements; add a query, or `tab: false` if it's meant to be empty",
                q.path.display(),
                q.query
            ));
        }
        let n = parts.len();
        for (i, st) in parts.into_iter().enumerate() {
            let body = sqlsplit::strip_leading_comments(&st.text);
            let line = st.line + st.text[..st.text.len() - body.len()].matches('\n').count();
            let kind = sqlsplit::classify(&st.text);
            let last = i + 1 == n;
            if q.tab && !last && kind == StatementKind::Read && two_tabs.is_none() {
                *two_tabs = Some(format!(
                    "{}:{line}: `{}` has more than one SELECT, but one .sql file makes one tab (from its last statement); put each tab's query in its own .sql file and list each in `queries:`",
                    q.path.display(),
                    q.query
                ));
            }
            out.push(Statement {
                query: q.query.clone(),
                connection: connection.to_string(),
                file: q.path.clone(),
                line,
                kind,
                text: st.text,
                sensitive,
                tab: q.tab && last,
            });
        }
        Ok(out)
    }

    /// Fail unless `connection`'s plugin can hold one session across `n` statements.
    fn check_sessions(&self, pool: &Pool, connection: &str, n: usize) -> Result<(), Fail> {
        let session = pool.session(connection)?;
        let mut s = session.lock().unwrap();
        if !s.get()?.has(CAP_SESSIONS) {
            let kind = pool.kind(connection).unwrap_or_default();
            return Err(format!(
                "this Binding runs {n} statements on connection `{connection}`, but the `{kind}` source plugin can't hold one session across them; nothing was run"
            ));
        }
        Ok(())
    }

    /// Run statement `i`, spooling its result if it makes a tab.
    fn execute_statement(
        &mut self,
        session: &Arc<Mutex<Session>>,
        i: usize,
        st: &Statement,
    ) -> Result<(), Fail> {
        let sql_log = self.ui.sql_log();
        sql_log(
            &format!("{}:{}", st.file.display(), st.line),
            &protected_sql(&st.text, st.sensitive),
        );
        let spool_path = self.run_dir.join(".spool").join(format!("{i}.arrow"));
        let mut writer: Option<FileWriter<File>> = None;
        let t = Instant::now();
        let exec = {
            let mut s = session.lock().unwrap();
            let (p, _log_scope) = s.statement(st.sensitive)?;
            p.execute(&st.text, self.opts.preview, |schema, batch| {
                if writer.is_none() {
                    let f = File::create(&spool_path).map_err(|e| e.to_string())?;
                    writer = Some(FileWriter::try_new(f, schema).map_err(|e| e.to_string())?);
                }
                writer.as_mut().unwrap().write(&batch).map_err(|e| e.to_string())
            })
            .map_err(|e| {
                format!(
                    "{}:{}: {}",
                    st.file.display(),
                    st.line,
                    masked_source_error(&e, st.sensitive)
                )
            })?
        };
        let what = match &exec {
            Execution::Result { rows, .. } => {
                format!("{} row{}", thousands(*rows), if *rows == 1 { "" } else { "s" })
            }
            Execution::NoResult {
                rows_affected: Some(n),
            } => format!("no result set ({n} affected)"),
            Execution::NoResult { .. } => "no result set".to_string(),
        };
        let at = format!("{}:{}", st.file.display(), st.line);
        self.ui.step(
            Level::Debug,
            "Executed",
            &format!("{at}  {what}"),
            Some(t.elapsed()),
        );
        // The YAML decides the tabs; the result only has to match it. A tab with no rows
        // still gets its column names.
        let (schema, rows) = match (st.tab, exec) {
            (true, Execution::Result { schema, rows }) => (schema, rows),
            (true, Execution::NoResult { .. }) => {
                return Err(format!(
                    "{at}: `{}` makes a tab, but its last statement returned no result set; if the query only prepares data (a temp view, a SET), add `tab: false` to it in the YAML",
                    st.query
                ));
            }
            (false, _) => {
                drop(writer);
                let _ = std::fs::remove_file(&spool_path);
                return Ok(());
            }
        };
        {
            let mut w = match writer {
                Some(w) => w,
                None => FileWriter::try_new(File::create(&spool_path).map_err(|e| e.to_string())?, &schema)
                    .map_err(|e| e.to_string())?,
            };
            w.finish().map_err(|e| e.to_string())?;
            self.produced.push(Produced {
                query: st.query.clone(),
                connection: st.connection.clone(),
                schema,
                rows,
                spool: spool_path,
                name: String::new(),
                anchor: None,
                header: None,
                columns: Default::default(),
            });
        }
        Ok(())
    }

    /// Each destination's rendered `profile`, `path` and options; `destination.*` is that
    /// destination while its values render.
    fn render_destinations(&mut self) -> Result<Vec<RenderedDest>, Fail> {
        if self.b.output.destinations.is_empty() {
            return Ok(Vec::new());
        }
        let renderer = self.renderer(None)?;
        let connections = self.pool.as_ref().expect("pool").connections.clone();
        let mut out = Vec::new();
        for (i, d) in self.b.output.destinations.iter().enumerate() {
            let profile = self
                .parsed
                .destinations
                .get(i)
                .cloned()
                .flatten()
                .ok_or_else(|| format!("destination `{}` has no profile", d.profile))?;
            renderer.set_destination(connections.profile(&profile, Some("destination")).ok());
            let rendered = (|| {
                let path = match &d.path {
                    Some(p) => Some(
                        renderer
                            .render(&self.report.file, p)
                            .map_err(|e| format!("output path: {e}"))?,
                    ),
                    None => None,
                };
                let mut options = JsonMap::new();
                for (k, v) in &d.options {
                    let v = render_json(&renderer, &self.report.file, v)
                        .map_err(|e| format!("destination `{profile}` option `{k}`: {e}"))?;
                    options.insert(k.clone(), v);
                }
                Ok::<_, Fail>(RenderedDest {
                    profile: profile.clone(),
                    path,
                    options,
                })
            })();
            renderer.set_destination(None);
            out.push(rendered?);
        }
        Ok(out)
    }

    /// Tab names: `tab_name`, else the query's basename.
    fn name_result_sets(&mut self) -> Result<(), Fail> {
        let entries: BTreeMap<&str, &QueryEntry> =
            self.b.queries.iter().map(|q| (q.query.as_str(), q)).collect();
        for p in &mut self.produced {
            let q = entries[p.query.as_str()];
            p.name = q.tab_name.clone().unwrap_or_else(|| q.query.clone());
            p.anchor = q.anchor.clone();
            p.header = q.header;
            p.columns = q.columns.clone();
        }
        if self.b.output.format == "xlsx" {
            let mut seen: BTreeMap<String, String> = BTreeMap::new();
            for p in &self.produced {
                check_sheet_name(&p.name)?;
                if let Some(prev) = seen.insert(p.name.to_lowercase(), p.query.clone()) {
                    return Err(format!(
                        "sheet name `{}` is used twice (by `{prev}` and `{}`); Excel sheet names must be unique",
                        p.name, p.query
                    ));
                }
            }
        }
        Ok(())
    }

    /// What a real run of this Binding would use and produce, for `dre compile` and `validate`.
    fn plan(&mut self) -> BindingPlan {
        let file = self.file_names();
        let compiled = self
            .b
            .queries
            .iter()
            .map(|q| {
                rel(
                    &self.project.root,
                    &self.compiled_dir.join(format!("{}.sql", q.query)),
                )
            })
            .collect();
        let destinations = self
            .dests
            .iter()
            .map(|d| {
                let resolved = self.dest_output(&d.profile);
                PlannedDestination {
                    profile: d.profile.clone(),
                    kind: resolved.as_ref().map(|(_, o)| o.kind.clone()),
                    target: resolved.map(|(t, _)| t).or_else(|| self.dest_target(&d.profile)),
                    path: d.path.clone(),
                    delivers: self.dest_output(&d.profile).is_some(),
                }
            })
            .collect();
        let pool = self.pool.clone().expect("pool");
        let queries = self
            .parsed
            .queries
            .iter()
            .map(|q| {
                let connection = q.connection.clone().unwrap_or_default();
                PlannedQuery {
                    query: q.query.clone(),
                    kind: pool.kind(&connection).unwrap_or_default(),
                    connection,
                    sources: q.sources.clone(),
                }
            })
            .collect();
        BindingPlan {
            report: self.report.name.clone(),
            set: self.b.set.clone(),
            compiled,
            profile: self.parsed.inherited.clone(),
            queries,
            target: self.target.clone(),
            format: self.b.output.format.clone(),
            output: rel(&self.project.root, &self.run_dir.join(file)),
            destinations,
            schedules: self.b.schedules.clone(),
        }
    }

    /// Apply `--output-path`/`--output-name` to every destination that has a path, and return
    /// the local file name: `--output-name`, else the first destination path's file name, else
    /// `<report>.<ext>`.
    fn file_names(&mut self) -> String {
        for d in self.dests.iter_mut().filter(|d| d.path.is_some()) {
            if let Some(p) = &self.opts.output_path {
                d.path = Some(p.clone());
            }
            if let (Some(n), Some(r)) = (&self.opts.output_name, d.path.as_mut()) {
                *r = match r.rfind(['/', '\\']) {
                    Some(i) => format!("{}{n}", &r[..=i]),
                    None => n.clone(),
                };
            }
        }
        let ext = match &self.b.output.extension {
            Some(e) => e.as_str(),
            None => extension(&self.b.output.format),
        };
        let from_remote = self
            .dests
            .iter()
            .find_map(|d| d.path.as_deref())
            .and_then(|r| r.rsplit(['/', '\\']).next())
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        self.opts
            .output_name
            .clone()
            .or(from_remote)
            .unwrap_or_else(|| match ext {
                "" => self.report.name.clone(),
                ext => format!("{}.{ext}", self.report.name),
            })
    }

    fn format(&mut self, filename: &str) -> Result<(), Fail> {
        std::fs::create_dir_all(&self.run_dir).map_err(|e| e.to_string())?;
        if self.produced.is_empty() {
            self.ui
                .warn("  no query makes a tab (every query has `tab: false`); no output file was written");
            return Ok(());
        }
        let out = &self.b.output;
        let plugin = find_plugin(self.project, PluginKind::Format, &out.format)?;
        let mut p = plugin
            .start(self.ui.plugin_log(), Some(&self.project.root))
            .map_err(|e| e.to_string())?;
        let multi = out.format == "xlsx" || out.template.is_some() || !is_single_table(&out.format);
        let groups: Vec<(String, Vec<usize>)> = if multi || self.produced.len() == 1 {
            vec![(filename.to_string(), (0..self.produced.len()).collect())]
        } else {
            let (stem, ext) = split_ext(filename);
            let mut g = Vec::new();
            for (i, r) in self.produced.iter().enumerate() {
                if r.name.contains(['/', '\\']) {
                    return Err(format!("`{}` can't be used in a file name", r.name));
                }
                g.push((format!("{stem}_{}{ext}", r.name), vec![i]));
            }
            g
        };
        let template = match &out.template {
            Some(t) => Some(self.template_payload(t)?),
            None => None,
        };
        for (name, idxs) in groups {
            let t = Instant::now();
            let path = self.run_dir.join(&name);
            let metas: Vec<ResultSetMeta> = idxs
                .iter()
                .map(|&i| {
                    let r = &self.produced[i];
                    ResultSetMeta {
                        name: r.name.clone(),
                        query: r.query.clone(),
                        result_index: 1,
                        anchor: r.anchor.clone(),
                        header: r.header,
                        columns: r.columns.clone(),
                    }
                })
                .collect();
            p.write_begin(
                &path.to_string_lossy(),
                &out.format,
                out.options.clone(),
                metas,
                template.clone(),
            )
            .map_err(|e| e.to_string())?;
            for &i in &idxs {
                let r = &self.produced[i];
                let reader = FileReader::try_new(File::open(&r.spool).map_err(|e| e.to_string())?, None)
                    .map_err(|e| e.to_string())?;
                let schema = r.schema.clone();
                // Stream batch by batch so memory stays flat.
                let mut any = false;
                for b in reader {
                    let b = b.map_err(|e| e.to_string())?;
                    any = true;
                    p.send_batch(&b)
                        .map_err(|e| format!("{} format: {e}", out.format))?;
                }
                if !any {
                    p.send_batch(&RecordBatch::new_empty(schema))
                        .map_err(|e| format!("{} format: {e}", out.format))?;
                }
                p.send(&dre_protocol::msg::Request::ResultSetEnd {})
                    .map_err(|e| format!("{} format: {e}", out.format))?;
            }
            let (files, warnings) = p
                .write_finish_with_warnings()
                .map_err(|e| format!("{} format: {e}", out.format))?;
            for w in warnings {
                self.ui.warn(&format!("  {} format: {w}", out.format));
            }
            for f in files {
                let f = PathBuf::from(f);
                let size = std::fs::metadata(&f).map(|m| m.len()).unwrap_or(0);
                let shown = rel(&self.project.root, &f);
                self.ui.step(
                    Level::Debug,
                    "Wrote",
                    &format!("{} ({})", shown.display(), human_bytes(size)),
                    Some(t.elapsed()),
                );
                self.files.push((f, None));
            }
        }
        let _ = p.close();
        Ok(())
    }

    fn template_payload(&mut self, t: &crate::project::Template) -> Result<Json, Fail> {
        let root = &self.project.root;
        let file = [root.join(&t.file), root.join("templates").join(&t.file)]
            .into_iter()
            .find(|p| p.is_file())
            .ok_or_else(|| format!("template file `{}` doesn't exist", t.file))?;
        let renderer = self.renderer(None)?;
        let mut values = JsonMap::new();
        for b in &t.bindings {
            if let (Some(cell), Some(v)) = (&b.cell, &b.value) {
                let rendered = renderer
                    .render(&self.report.file, v)
                    .map_err(|e| format!("template value: {e}"))?;
                values.insert(format!("{}!{}", b.sheet, cell), Json::String(rendered));
            }
        }
        Ok(json!({"file": file.to_string_lossy(), "bindings": t.bindings, "values": values}))
    }

    /// Deliver to every destination in order. A failure is recorded and the rest are still
    /// attempted; the Binding fails if any did.
    fn deliver(&mut self) -> Result<(), Fail> {
        if self.dests.is_empty() {
            self.delivery_note = Some("no destination declared; output stays in target/".into());
            let dir = rel(&self.project.root, &self.run_dir);
            self.ui.step(
                Level::Debug,
                "Kept",
                &format!("no destination declared: output stays in {}", dir.display()),
                None,
            );
            return Ok(());
        }
        if self.files.is_empty() {
            return Ok(());
        }
        let dests = std::mem::take(&mut self.dests);
        let mut failures = Vec::new();
        let mut skipped = Vec::new();
        for d in &dests {
            // The type of the output used, or for a skipped entry the profile's default one.
            let kind = self
                .dest_output(&d.profile)
                .map(|(_, o)| o.kind.clone())
                .or_else(|| {
                    let p = self.project.profiles.get(Role::Destination, &d.profile)?;
                    p.targets.values().next().map(|o| o.kind.clone())
                });
            let (status, location, error) = match self.deliver_one(d) {
                Ok(Some(loc)) => ("delivered", Some(loc), None),
                Ok(None) => {
                    skipped.push(self.skip_note(&d.profile));
                    ("skipped", None, None)
                }
                Err(e) => {
                    failures.push(e.clone());
                    ("failed", None, Some(e))
                }
            };
            let mut record = json!({"profile": d.profile, "type": kind, "status": status});
            if let Some(l) = location {
                record["location"] = json!(l);
            }
            if let Some(e) = error {
                record["error"] = json!(e);
            }
            self.deliveries.push(record);
        }
        self.dests = dests;
        if self.files.iter().all(|(_, d)| d.is_none()) && !skipped.is_empty() {
            self.delivery_note = Some(skipped.join("; "));
        }
        match failures.len() {
            0 => Ok(()),
            1 if self.dests.len() == 1 => Err(failures.remove(0)),
            n => Err(format!(
                "{n} of {} destinations failed: {}",
                self.dests.len(),
                failures.join("; ")
            )),
        }
    }

    /// The destination profile's settings for the run's target.
    fn dest_output(&self, profile: &str) -> Option<(String, &ProfileTarget)> {
        if self.project.profiles.is_builtin_local(profile) {
            return Some((self.target.clone(), &BUILTIN_LOCAL));
        }
        self.project
            .profiles
            .target(Role::Destination, profile, &self.target)
            .map(|o| (self.target.clone(), o))
    }

    /// The target a destination profile delivers for: the run's, when the profile exists.
    fn dest_target(&self, profile: &str) -> Option<String> {
        self.project
            .profiles
            .get(Role::Destination, profile)
            .map(|_| self.target.clone())
    }

    fn skip_note(&self, profile: &str) -> String {
        let dtarget = self.dest_target(profile).unwrap_or_default();
        format!(
            "destination profile `{profile}` has no `{dtarget}` target: not delivered, output stays in target/"
        )
    }

    /// Deliver every file to one destination. `Ok(None)`: its profile has no output for the
    /// active target, so nothing was sent.
    fn deliver_one(&mut self, d: &RenderedDest) -> Result<Option<String>, Fail> {
        if self.project.profiles.get(Role::Destination, &d.profile).is_none()
            && !self.project.profiles.is_builtin_local(&d.profile)
        {
            return Err(format!(
                "destination profile `{}` isn't in profiles.yml",
                d.profile
            ));
        }
        let Some((_, out)) = self.dest_output(&d.profile) else {
            let note = self.skip_note(&d.profile);
            self.ui.step(Level::Info, "Kept", &note, None);
            return Ok(None);
        };
        let kind = out.kind.clone();
        let connection = render_connection(out)?;
        let targets: Vec<DeliveryFile> = self
            .files
            .iter()
            .map(|(f, _)| {
                let name = f.file_name().unwrap().to_string_lossy().to_string();
                let r = d.path.as_deref().map(|r| {
                    if self.files.len() == 1 {
                        r.to_string()
                    } else {
                        match r.rfind(['/', '\\']) {
                            Some(i) => format!("{}{name}", &r[..=i]),
                            None => name.clone(),
                        }
                    }
                });
                DeliveryFile {
                    local_path: f.to_string_lossy().to_string(),
                    remote_path: r,
                }
            })
            .collect();
        let mut locations = Vec::new();
        if kind == LOCAL_TYPE {
            if let Some(k) = d.options.keys().next() {
                return Err(format!(
                    "the local destination takes no options, but `{}` has `{k}`; check the key's spelling",
                    d.profile
                ));
            }
            for (i, f) in targets.iter().enumerate() {
                let t = Instant::now();
                let r = f
                    .remote_path
                    .as_ref()
                    .ok_or("the local destination needs `output.destination.path`")?;
                let dst = self.project.root.join(r);
                if let Some(parent) = dst.parent() {
                    std::fs::create_dir_all(parent)
                        .map_err(|e| format!("can't create {}: {e}", parent.display()))?;
                }
                std::fs::copy(&f.local_path, &dst).map_err(|e| {
                    format!(
                        "delivery to {} failed: {e}; the output is still in target/",
                        dst.display()
                    )
                })?;
                let loc = dst.to_string_lossy().to_string();
                self.ui.step(Level::Debug, "Delivered", &loc, Some(t.elapsed()));
                self.files[i].1.get_or_insert_with(|| loc.clone());
                locations.push(loc);
            }
            return Ok(Some(locations.join(", ")));
        }
        let failed = |e: dre_protocol::host::HostError| {
            format!("delivery through `{kind}` failed: {e}; the output is still in target/")
        };
        let plugin = find_plugin(self.project, PluginKind::Destination, &kind)?;
        let mut p = plugin
            .start(self.ui.plugin_log(), Some(&self.project.root))
            .map_err(|e| e.to_string())?;
        let batches: Vec<Vec<usize>> = if targets.len() > 1 && p.has(CAP_MULTI_FILE) {
            vec![(0..targets.len()).collect()]
        } else {
            (0..targets.len()).map(|i| vec![i]).collect()
        };
        for batch in batches {
            let t = Instant::now();
            let files: Vec<DeliveryFile> = batch.iter().map(|&i| targets[i].clone()).collect();
            let loc = p
                .deliver_files(&files, connection.clone(), d.options.clone())
                .map_err(failed)?;
            self.ui.step(Level::Debug, "Delivered", &loc, Some(t.elapsed()));
            for i in batch {
                self.files[i].1.get_or_insert_with(|| loc.clone());
            }
            locations.push(loc);
        }
        let _ = p.close();
        Ok(Some(locations.join(", ")))
    }

    fn snapshot(&self) -> Json {
        let sets: Vec<Json> = self
            .produced
            .iter()
            .map(|p| {
                json!({
                    "name": p.name,
                    "columns": p.schema.fields().iter().map(|f| json!({
                        "name": f.name(),
                        "type": f.data_type().to_string(),
                        "type_shape": crate::schema::type_shape(f.data_type()),
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({ "result_sets": sets })
    }

    fn schema_drift(&mut self) -> Vec<String> {
        let path = self.schema_dir.join("last_success.json");
        let mut current = self.snapshot();
        let Ok(prev) = std::fs::read_to_string(&path) else {
            crate::schema::redact(&mut current);
            self.protected_snapshot = Some(current);
            return Vec::new();
        };
        let Ok(mut prev) = serde_json::from_str::<Json>(&prev) else {
            crate::schema::redact(&mut current);
            self.protected_snapshot = Some(current);
            return Vec::new();
        };
        let original = prev.clone();
        crate::schema::redact(&mut prev);
        crate::schema::redact(&mut current);
        if crate::secrets::enabled() {
            crate::schema::align_protection(&mut prev, &mut current);
        }
        if prev != original {
            let protected =
                serde_json::to_string_pretty(&prev).expect("serializing a serde_json::Value cannot fail");
            if let Err(error) = std::fs::write(&path, protected) {
                self.ui.warn(&format!(
                    "can't protect the existing schema snapshot at {}: {error}; continuing without rewriting it",
                    path.display()
                ));
            }
        }
        let drift = crate::schema::drift(&prev, &current);
        self.protected_snapshot = Some(current);
        drift
    }

    fn write_snapshot(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.schema_dir)?;
        let snapshot = self.protected_snapshot.clone().unwrap_or_else(|| {
            let mut snapshot = self.snapshot();
            crate::schema::redact(&mut snapshot);
            snapshot
        });
        std::fs::write(
            self.schema_dir.join("last_success.json"),
            serde_json::to_string_pretty(&snapshot)?,
        )
    }

    fn write_results(&self, status: &Status, error: Option<&str>) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.run_dir)?;
        let outputs: Vec<Json> = self
            .files
            .iter()
            .map(|(f, delivered)| {
                json!({
                    "path": record_path(self.project, f),
                    "size": std::fs::metadata(f).map(|m| m.len()).unwrap_or(0),
                    "delivered_to": delivered,
                })
            })
            .collect();
        let results = json!({
            "report": self.report.name,
            "set": self.b.set,
            "binding": self.b.dir_name(),
            "managed": self.report.managed,
            "profile": self.parsed.inherited,
            "connections": self.parsed.connections(),
            "target": self.target,
            "schedule": self.opts.schedule,
            "schedule_vars": self.schedule_vars,
            "vars": self.rendered_vars,
            "run_date": self.date.to_string(),
            "scheduled_at": self.opts.scheduled_at.map(rfc3339),
            "timezone": self.calendar.tz.name(),
            "params": self.opts.params(self.date),
            "status": status,
            "error": error,
            "preview": self.opts.preview.is_some(),
            "row_limit": self.opts.preview,
            "started_at": self.started_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "duration_ms": self.started.elapsed().as_millis() as u64,
            "result_sets": self.produced.iter().map(|p| json!({
                "name": p.name,
                "query": p.query,
                "connection": p.connection,
                "rows": p.rows,
                "columns": p.schema.fields().iter().map(|f| f.name().clone()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "outputs": outputs,
            "delivery": self.delivery_note,
            "deliveries": self.deliveries,
            "schema_drift": self.drift,
            "target_path": self.project.target_dir,
            "manifest_checksum": self.opts.manifest_checksum,
        });
        std::fs::write(
            self.run_dir.join("run_results.json"),
            (crate::secrets::to_json_pretty(&results)? + "\n").as_bytes(),
        )
    }

    /// `dre validate --live`: execute temp creates, `check` everything else, each on its
    /// connection; then check declared source columns against the database.
    fn live_check(&mut self, statements: &[Statement]) -> Result<(), Fail> {
        let pool = self.pool.clone().expect("pool");
        let mut failures = Vec::new();
        let mut unexecuted_setup: BTreeMap<String, (PathBuf, usize)> = BTreeMap::new();
        let mut uncheckable: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
        let sql_log = self.ui.sql_log();
        for st in statements {
            if uncheckable.contains(&st.connection) {
                continue;
            }
            let session = pool.session(&st.connection)?;
            let mut s = session.lock().unwrap();
            if !s.get()?.has(dre_protocol::CAP_CHECK) {
                let kind = pool.kind(&st.connection).unwrap_or_default();
                self.ui.warn(&format!(
                    "  not checkable: the `{kind}` source plugin (connection `{}`) can't check statements without running them",
                    st.connection
                ));
                uncheckable.insert(st.connection.clone());
                continue;
            }
            let verb = if st.kind == StatementKind::TempCreate {
                ""
            } else {
                " (check)"
            };
            sql_log(
                &format!("{}:{}{verb}", st.file.display(), st.line),
                &protected_sql(&st.text, st.sensitive),
            );
            if st.kind == StatementKind::TempCreate {
                let result = {
                    let (process, _log_scope) = s.statement(st.sensitive)?;
                    process.execute(&st.text, None, |_, _| Ok(()))
                };
                if let Err(e) = result {
                    failures.push(format!(
                        "{}:{}: {}",
                        st.file.display(),
                        st.line,
                        masked_source_error(&e, st.sensitive)
                    ));
                }
                continue;
            }
            let result = {
                let (process, _log_scope) = s.statement(st.sensitive)?;
                process.check(&st.text)
            };
            if let Err(e) = result {
                let mut msg = format!(
                    "{}:{}: {}",
                    st.file.display(),
                    st.line,
                    masked_source_error(&e, st.sensitive)
                );
                if let Some((f, l)) = unexecuted_setup.get(&st.connection) {
                    msg.push_str(&format!(
                        " (may be a false positive: the setup statement at {}:{l} wasn't executed during the check)",
                        f.display()
                    ));
                }
                failures.push(msg);
            }
            if st.kind == StatementKind::Other {
                unexecuted_setup.insert(st.connection.clone(), (st.file.clone(), st.line));
            }
        }
        self.check_source_columns(&pool, &mut failures);
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("\n    "))
        }
    }

    /// Declared source `columns` against the real table, once per source table, connection and
    /// target in this process: each must exist, and match its `data_type` (loosely) where given.
    fn check_source_columns(&mut self, pool: &Arc<Pool>, failures: &mut Vec<String>) {
        /// `(project, source.table, connection, target)`.
        type Checked = std::collections::BTreeSet<(PathBuf, String, String, String)>;
        static CHECKED: std::sync::LazyLock<Mutex<Checked>> = std::sync::LazyLock::new(Mutex::default);
        for q in self.parsed.queries.clone() {
            let Some(conn) = q.connection.clone() else {
                continue;
            };
            for key in &q.sources {
                let Some(r) = self.parsed.sources.get(key).cloned() else {
                    continue;
                };
                let Some(declared) = self
                    .project
                    .sources
                    .get(&r.source)
                    .and_then(|s| s.table(&r.table))
                    .map(|t| t.columns.clone())
                    .filter(|c| !c.is_empty())
                else {
                    continue;
                };
                let id = (
                    self.project.root.clone(),
                    key.clone(),
                    conn.clone(),
                    self.target.clone(),
                );
                if !CHECKED.lock().unwrap().insert(id) {
                    continue;
                }
                let quote = if r.quoting.any() {
                    let kind = pool.kind(&conn).unwrap_or_default();
                    match pool.connections.identifier_quote(&kind) {
                        Ok(q) => q,
                        Err(e) => {
                            failures.push(format!("source `{key}`: can't quote its name: {e}"));
                            continue;
                        }
                    }
                } else {
                    None
                };
                let rel = r.relation(quote.as_deref());
                let runner = PoolRunner {
                    pool: pool.clone(),
                    default: Some(conn.clone()),
                    log: self.ui.sql_log(),
                };
                let actual = match runner.columns(&format!("select * from {rel} where 1=0"), None) {
                    Ok(c) => c,
                    Err(e) => {
                        failures.push(format!("source `{key}` ({rel} on connection `{conn}`): {e}"));
                        continue;
                    }
                };
                for c in &declared {
                    let Some(a) = actual.iter().find(|a| a.name.eq_ignore_ascii_case(&c.name)) else {
                        failures.push(format!(
                            "source `{key}`: declared column `{}` isn't in {rel} on connection `{conn}` (it has: {})",
                            c.name,
                            actual.iter().map(|a| a.name.as_str()).collect::<Vec<_>>().join(", ")
                        ));
                        continue;
                    };
                    let Some(dt) = &c.data_type else { continue };
                    match crate::coltypes::matches(dt, &a.data_type) {
                        Some(true) => {}
                        Some(false) => failures.push(format!(
                            "source `{key}`: column `{}` is declared `{dt}`, but {rel} on connection `{conn}` returns {}",
                            c.name, a.data_type
                        )),
                        None => self.ui.warn(&format!(
                            "  source `{key}`: column `{}`'s `data_type: {dt}` isn't a type DRE can compare, so only its presence was checked",
                            c.name
                        )),
                    }
                }
            }
        }
    }
}

/// The source session, started on first use (rendering may need it for `run_query()`).
struct Session {
    path: Result<crate::plugins::Located, String>,
    connection: Result<JsonMap<String, Json>, String>,
    cwd: PathBuf,
    log: LogSink,
    /// Whether plugin diagnostics for the active statement must be hidden rather than exactly
    /// masked. Connection diagnostics always use exact-value masking.
    opaque_log: Arc<AtomicBool>,
    /// Unmanaged reports run read-only unless they create temp objects.
    unmanaged: bool,
    read_only: bool,
    /// A lookup was loaded into a temp table, so the session must stay open as it is.
    loaded: bool,
    proc_: Option<PluginProcess>,
}

impl Session {
    fn new(
        path: Result<crate::plugins::Located, String>,
        connection: Result<JsonMap<String, Json>, String>,
        cwd: PathBuf,
        log: LogSink,
        unmanaged: bool,
    ) -> Session {
        let opaque_log = Arc::new(AtomicBool::new(false));
        let protect = Arc::clone(&opaque_log);
        let downstream = log;
        let log: LogSink = Arc::new(move |plugin, line| {
            let line = crate::secrets::mask_source_error(line, protect.load(Ordering::SeqCst));
            downstream(plugin, &line);
        });
        Session {
            path,
            connection,
            cwd,
            log,
            opaque_log,
            unmanaged,
            read_only: unmanaged,
            loaded: false,
            proc_: None,
        }
    }

    fn want_read_only(&mut self, ro: bool) {
        if self.read_only != ro {
            // Reopen if a session was already started in the other mode.
            self.close();
            self.read_only = ro;
        }
    }

    fn get(&mut self) -> Result<&mut PluginProcess, String> {
        if self.proc_.is_none() {
            let plugin = self.path.clone()?;
            let connection = self.connection.clone()?;
            let mut p = plugin
                .start(self.log.clone(), Some(&self.cwd))
                .map_err(|e| masked_source_error(&e, false))?;
            let ro = self.unmanaged && self.read_only && p.has(CAP_READ_ONLY);
            p.open(connection, ro).map_err(|e| {
                let protected = masked_source_error(&e, false);
                format!("can't open the source connection: {protected}")
            })?;
            self.proc_ = Some(p);
        }
        Ok(self.proc_.as_mut().unwrap())
    }

    fn statement(&mut self, sensitive: bool) -> Result<(&mut PluginProcess, OpaqueLogScope), String> {
        let opaque_log = Arc::clone(&self.opaque_log);
        let process = self.get()?;
        let scope = OpaqueLogScope::new(opaque_log, sensitive);
        Ok((process, scope))
    }

    fn close(&mut self) {
        if let Some(p) = self.proc_.take() {
            let _ = p.close();
        }
    }
}

struct OpaqueLogScope(Arc<AtomicBool>);

impl OpaqueLogScope {
    fn new(active: Arc<AtomicBool>, sensitive: bool) -> Self {
        active.store(sensitive, Ordering::SeqCst);
        Self(active)
    }
}

impl Drop for OpaqueLogScope {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

/// The Binding's connections: each one's session, opened when a query first needs it and held
/// until the report ends.
struct Pool {
    connections: Arc<ProfileConnections>,
    cwd: PathBuf,
    log: LogSink,
    unmanaged: bool,
    sessions: Mutex<BTreeMap<String, Arc<Mutex<Session>>>>,
}

impl Pool {
    fn new(connections: Arc<ProfileConnections>, cwd: PathBuf, log: LogSink, unmanaged: bool) -> Pool {
        Pool {
            connections,
            cwd,
            log,
            unmanaged,
            sessions: Mutex::default(),
        }
    }

    /// A connection's settings for the run's target.
    fn target(&self, name: &str) -> Result<ProfileTarget, String> {
        let profiles = &self.connections.profiles;
        let target = &self.connections.target;
        let p = profiles.get(Role::Connection, name).ok_or_else(|| {
            format!(
                "connection `{name}` isn't under `{}:` in {}",
                profiles.section_key(Role::Connection),
                profiles.path.display()
            )
        })?;
        p.targets.get(target).cloned().ok_or_else(|| {
            format!(
                "connection `{name}` has no `{target}` target (it has: {})",
                p.targets.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })
    }

    /// A connection's plugin type.
    fn kind(&self, name: &str) -> Result<String, String> {
        Ok(self.target(name)?.kind)
    }

    fn session(&self, name: &str) -> Result<Arc<Mutex<Session>>, String> {
        if let Some(s) = self.sessions.lock().unwrap().get(name) {
            return Ok(s.clone());
        }
        let output = self.target(name)?;
        let id = crate::project::PluginId::new(PluginKind::Source, output.kind.clone());
        let path = crate::plugins::locate_in(&self.connections.root, &self.connections.plugins, &id)
            .map_err(String::from);
        let session = Arc::new(Mutex::new(Session::new(
            path,
            render_connection(&output),
            self.cwd.clone(),
            self.log.clone(),
            self.unmanaged,
        )));
        self.sessions
            .lock()
            .unwrap()
            .insert(name.to_string(), session.clone());
        Ok(session)
    }

    fn close_all(&self) {
        for s in self.sessions.lock().unwrap().values() {
            if let Ok(mut s) = s.lock() {
                s.close();
            }
        }
    }
}

/// `run_query()`, `columns()` and lookups for one renderer: on the named connection's session,
/// else on the renderer's own (`default`).
struct PoolRunner {
    pool: Arc<Pool>,
    default: Option<String>,
    log: LogSink,
}

impl PoolRunner {
    fn session(&self, profile: Option<&str>) -> Result<Arc<Mutex<Session>>, String> {
        let name = profile.or(self.default.as_deref()).ok_or(
            "no connection here: pass `profile=`, or give the report a `profile:` (this isn't a query's SQL)",
        )?;
        self.pool.session(name)
    }
}

impl QueryRunner for PoolRunner {
    fn run_query(&self, sql: &str, max_rows: u64, profile: Option<&str>) -> Result<QueryRows, String> {
        let session = self.session(profile)?;
        let mut s = session.lock().map_err(|_| "session lock poisoned".to_string())?;
        let sensitive = crate::secrets::contains_secret(sql);
        // An unmanaged report may only read, and run_query() runs before the file's own
        // statements are checked, so it gets the same rule up front.
        if s.unmanaged
            && let Some(bad) = sqlsplit::split(sql)
                .into_iter()
                .find(|st| sqlsplit::classify(&st.text) != StatementKind::Read)
        {
            return Err(format!(
                "run_query() in an unmanaged report may only read, but got `{}`; give the report a YAML to declare it",
                masked_sql_summary(&bad.text, 60, sensitive)
            ));
        }
        (self.log)("run_query()", &protected_sql(sql, sensitive));
        let (p, _log_scope) = s.statement(sensitive)?;
        let mut out = QueryRows::default();
        let mut too_many = false;
        let exec = p
            .execute(sql, Some(max_rows + 1), |schema, batch| {
                if out.columns.is_empty() {
                    out.columns = schema.fields().iter().map(|f| f.name().clone()).collect();
                }
                if out.rows.len() as u64 + batch.num_rows() as u64 > max_rows {
                    too_many = true;
                    return Ok(());
                }
                out.rows.extend(crate::values::batch_rows(&batch));
                Ok(())
            })
            .map_err(|e| format!("run_query() failed: {}", masked_source_error(&e, sensitive)))?;
        if too_many {
            return Err(format!(
                "run_query() returned more than {max_rows} rows; it's meant for small lookups — raise the cap with `run_query(sql, max_rows=N)` or `run_query_max_rows` in dre_project.yml"
            ));
        }
        if let Execution::Result { schema, .. } = exec
            && out.columns.is_empty()
        {
            out.columns = schema.fields().iter().map(|f| f.name().clone()).collect();
        }
        Ok(out)
    }

    fn columns(&self, sql: &str, profile: Option<&str>) -> Result<Vec<Column>, String> {
        let session = self.session(profile)?;
        let mut s = session.lock().map_err(|_| "session lock poisoned".to_string())?;
        let sensitive = crate::secrets::contains_secret(sql);
        (self.log)("columns()", &protected_sql(sql, sensitive));
        let (p, _log_scope) = s.statement(sensitive)?;
        let mut schema: Option<SchemaRef> = None;
        let exec = p
            .execute(sql, Some(1), |sch, _| {
                schema.get_or_insert_with(|| sch.clone());
                Ok(())
            })
            .map_err(|e| masked_source_error(&e, sensitive))?;
        if let Execution::Result { schema: sch, .. } = exec {
            schema.get_or_insert(sch);
        }
        let schema = schema.ok_or("the query returned no result set")?;
        Ok(schema
            .fields()
            .iter()
            .map(|f| Column {
                name: f.name().clone(),
                data_type: f.data_type().to_string(),
            })
            .collect())
    }

    fn load(&self, name: &str, table: &Table) -> Result<Option<(String, Option<String>)>, String> {
        let session = self.session(None)?;
        let mut s = session.lock().map_err(|_| "session lock poisoned".to_string())?;
        // A temp table is allowed even in an unmanaged report; it needs a writable session.
        s.want_read_only(false);
        if !s.get()?.has(CAP_LOAD) {
            return Ok(None);
        }
        let batch = table.to_batch()?;
        let schema = batch.schema();
        (self.log)(
            &format!("lookup `{name}`"),
            &format!(
                "-- {} rows loaded through the plugin's `load` request",
                batch.num_rows()
            ),
        );
        let r = s.get()?.load(name, &schema, [batch]).map_err(|e| e.to_string())?;
        s.loaded = true;
        Ok(Some(r))
    }
}

/// Render every string inside a destination option value.
fn render_json(renderer: &Renderer, file: &Path, v: &Json) -> Result<Json, RenderError> {
    Ok(match v {
        Json::String(s) => Json::String(renderer.render(file, s)?),
        Json::Array(a) => Json::Array(
            a.iter()
                .map(|v| render_json(renderer, file, v))
                .collect::<Result<_, _>>()?,
        ),
        Json::Object(o) => Json::Object(
            o.iter()
                .map(|(k, v)| Ok((k.clone(), render_json(renderer, file, v)?)))
                .collect::<Result<_, RenderError>>()?,
        ),
        other => other.clone(),
    })
}

/// Render `env_var()` (and only that) inside a profile output's string fields.
fn render_connection(output: &ProfileTarget) -> Result<JsonMap<String, Json>, String> {
    let mut env = minijinja::Environment::new();
    env.set_undefined_behavior(minijinja::UndefinedBehavior::Strict);
    env.add_function("env_var", |name: String, default: Option<String>| -> Result<String, minijinja::Error> {
        std::env::var(&name).ok().or(default).ok_or_else(|| {
            minijinja::Error::new(
                minijinja::ErrorKind::UndefinedError,
                format!("`env_var('{name}')`: environment variable `{name}` is not set and no default is given"),
            )
        })
    });
    fn walk(env: &minijinja::Environment<'_>, v: &Json) -> Result<Json, String> {
        Ok(match v {
            Json::String(s) if crate::preflight::is_templated(s) => Json::String(
                env.render_str(s, ())
                    .map_err(|e| format!("profiles.yml: {}", e.detail().unwrap_or("render error")))?,
            ),
            Json::Array(a) => Json::Array(a.iter().map(|x| walk(env, x)).collect::<Result<_, _>>()?),
            Json::Object(o) => Json::Object(
                o.iter()
                    .map(|(k, x)| Ok((k.clone(), walk(env, x)?)))
                    .collect::<Result<_, String>>()?,
            ),
            other => other.clone(),
        })
    }
    match walk(&env, &Json::Object(output.fields.clone()))? {
        Json::Object(o) => Ok(o),
        _ => unreachable!(),
    }
}

/// Locate the plugin for a declared type among the project's packages, honouring `dre.lock`
/// pins and declared constraints.
pub fn find_plugin(
    project: &Project,
    kind: PluginKind,
    name: &str,
) -> Result<crate::plugins::Located, crate::plugins::LocateError> {
    crate::plugins::locate(project, &crate::project::PluginId::new(kind, name))
}

/// `connection`, `destination` and `profile()` from `profiles.yml`, for the run's target. A
/// field is secret when the plugin's `describe` says so; when the plugin can't be asked, when its
/// name looks like one; and always when its value comes from a `DRE_SECRET_*` variable.
struct ProfileConnections {
    profiles: Profiles,
    /// The run's target.
    target: String,
    root: PathBuf,
    plugins: Vec<crate::project::PluginRequirement>,
    log: LogSink,
}

/// What `describe` said, by `(project, role, type)`, `None` when it couldn't be asked: each
/// plugin is asked once per process, not once per Binding.
static DESCRIBED: std::sync::LazyLock<Mutex<BTreeMap<DescribeKey, Option<Described>>>> =
    std::sync::LazyLock::new(Mutex::default);

/// `(project root, role, plugin type)`.
type DescribeKey = (PathBuf, Role, String);

#[derive(Clone)]
struct Described {
    secrets: Vec<String>,
    identifier_quote: Option<String>,
}

const SECRET_NAME_WORDS: &[&str] = &["password", "secret", "token", "key", "credential"];

impl ProfileConnections {
    fn view(&self, role: Role, name: &str) -> Result<Connection, String> {
        let (target, out): (String, ProfileTarget) = match self.profiles.get(role, name) {
            Some(p) => {
                let t = &self.target;
                let o = p.targets.get(t).ok_or_else(|| {
                    format!(
                        "{} `{name}` has no `{t}` target (it has: {})",
                        role.as_str(),
                        p.targets.keys().cloned().collect::<Vec<_>>().join(", ")
                    )
                })?;
                (t.clone(), o.clone())
            }
            None if role == Role::Destination && self.profiles.is_builtin_local(name) => {
                (self.target.clone(), BUILTIN_LOCAL.clone())
            }
            None => {
                return Err(format!(
                    "no {} profile `{name}` in {}",
                    role.as_str(),
                    self.profiles.path.display()
                ));
            }
        };
        let mut secrets: Vec<String> = out
            .fields
            .iter()
            .filter(|(_, v)| v.as_str().is_some_and(|s| s.contains(crate::secrets::PREFIX)))
            .map(|(k, _)| k.clone())
            .collect();
        match self.described(role, &out.kind) {
            Some(d) => secrets.extend(d.secrets),
            None => secrets.extend(
                out.fields
                    .keys()
                    .filter(|k| {
                        let k = k.to_lowercase();
                        SECRET_NAME_WORDS.iter().any(|w| k.contains(w))
                    })
                    .cloned(),
            ),
        }
        let fields = render_connection(&out)?;
        Ok(Connection {
            profile: name.to_string(),
            target,
            kind: out.kind.clone(),
            fields,
            secrets,
        })
    }

    /// The plugin's `describe` reply, asked once per plugin type.
    fn described(&self, role: Role, kind: &str) -> Option<Described> {
        let key = (self.root.clone(), role, kind.to_string());
        if let Some(v) = DESCRIBED.lock().unwrap().get(&key) {
            return v.clone();
        }
        let plugin_kind = match role {
            Role::Connection => PluginKind::Source,
            Role::Destination => PluginKind::Destination,
        };
        let asked = (|| {
            if kind == LOCAL_TYPE {
                return Some(Described {
                    secrets: Vec::new(),
                    identifier_quote: None,
                });
            }
            let id = crate::project::PluginId::new(plugin_kind, kind);
            let plugin = crate::plugins::locate_in(&self.root, &self.plugins, &id).ok()?;
            let mut p = plugin.start(self.log.clone(), Some(&self.root)).ok()?;
            let d = p.description().ok();
            let _ = p.close();
            let d = d?;
            Some(Described {
                secrets: d
                    .connection_fields
                    .into_iter()
                    .filter(|f| f.secret)
                    .map(|f| f.name)
                    .collect(),
                identifier_quote: d.identifier_quote,
            })
        })();
        DESCRIBED.lock().unwrap().insert(key, asked.clone());
        asked
    }
}

impl Connections for ProfileConnections {
    fn profile(&self, name: &str, role: Option<&str>) -> Result<Connection, String> {
        let role = match role {
            Some("connection") => Role::Connection,
            Some(_) => Role::Destination,
            None => {
                let c = self.profiles.get(Role::Connection, name).is_some();
                let d = self.profiles.get(Role::Destination, name).is_some();
                match (c, d) {
                    (true, true) => {
                        return Err(format!(
                            "`{name}` is both a connection and a destination profile; say which with `profile('{name}', role='connection')` or `role='destination'`"
                        ));
                    }
                    (true, false) => Role::Connection,
                    _ => Role::Destination,
                }
            }
        };
        self.view(role, name)
    }

    fn identifier_quote(&self, kind: &str) -> Result<Option<String>, String> {
        match self.described(Role::Connection, kind) {
            Some(d) => Ok(d.identifier_quote),
            None => Err(format!(
                "the `{kind}` source plugin couldn't be asked for its identifier quote character"
            )),
        }
    }
}

fn check_sheet_name(n: &str) -> Result<(), String> {
    if n.is_empty() || n.chars().count() > 31 {
        return Err(format!("sheet name `{n}` must be 1 to 31 characters long"));
    }
    if let Some(c) = n.chars().find(|c| "[]:*?/\\".contains(*c)) {
        return Err(format!(
            "sheet name `{n}` contains `{c}`, which Excel doesn't allow in sheet names"
        ));
    }
    if n.starts_with('\'') || n.ends_with('\'') {
        return Err(format!("sheet name `{n}` can't start or end with an apostrophe"));
    }
    if n.eq_ignore_ascii_case("history") {
        return Err("`History` is reserved by Excel and can't be a sheet name".into());
    }
    Ok(())
}

fn is_single_table(format: &str) -> bool {
    matches!(format, "csv" | "delimited" | "fixed_width" | "parquet")
}

fn extension(format: &str) -> &str {
    match format {
        "delimited" | "fixed_width" => "txt",
        f => f,
    }
}

fn split_ext(name: &str) -> (&str, &str) {
    match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    }
}

fn rel(root: &Path, p: &Path) -> PathBuf {
    crate::slash(p.strip_prefix(root).unwrap_or(p))
}

/// An instant as DRE records it: RFC 3339 in UTC, to the second.
pub fn rfc3339(t: chrono::DateTime<chrono::Utc>) -> String {
    t.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// How a generated file is recorded in `run_results.json`: relative to the project root when
/// it's inside it, else relative to the target path, so moving the target folder doesn't
/// change what's recorded.
fn record_path(project: &Project, p: &Path) -> PathBuf {
    match p.strip_prefix(&project.root) {
        Ok(r) => crate::slash(r),
        Err(_) => rel(&project.target_dir, p),
    }
}

impl BindingRun<'_> {
    /// What this Binding produced, for its end-of-run line.
    fn summary_line(&self) -> String {
        let rows: u64 = self.produced.iter().map(|p| p.rows).sum();
        let mut parts = Vec::new();
        if !self.produced.is_empty() {
            let n = self.produced.len();
            parts.push(format!(
                "{n} result set{}, {} row{}",
                if n == 1 { "" } else { "s" },
                thousands(rows),
                if rows == 1 { "" } else { "s" }
            ));
        }
        let names: Vec<String> = self
            .files
            .iter()
            .map(|(f, _)| f.file_name().unwrap_or_default().to_string_lossy().to_string())
            .collect();
        if !names.is_empty() {
            parts.push(format!("→ {}", names.join(", ")));
        }
        // Every destination's location, not just the first one each file reached.
        let delivered: Vec<&str> = self
            .deliveries
            .iter()
            .filter_map(|d| d.get("location").and_then(Json::as_str))
            .collect();
        if !delivered.is_empty() {
            parts.push(format!("→ {}", delivered.join(", ")));
        } else if let Some(n) = &self.delivery_note
            && self.opts.preview.is_none()
            && !self.b.output.destinations.is_empty()
        {
            parts.push(format!("({n})"));
        }
        parts.join(" ")
    }
}

/// `1234567` → `1,234,567`.
pub fn thousands(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, c) in s.chars().enumerate() {
        if i > 0 && (s.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(c);
    }
    out
}

fn human_bytes(n: u64) -> String {
    match n {
        n if n < 1024 => format!("{n} B"),
        n if n < 1024 * 1024 => format!("{:.1} KB", n as f64 / 1024.0),
        n => format!("{:.1} MB", n as f64 / (1024.0 * 1024.0)),
    }
}

/// `dre clean`: remove the target folder `t`.
pub fn clean(t: &Path) -> std::io::Result<bool> {
    if t.exists() {
        std::fs::remove_dir_all(t)?;
        Ok(true)
    } else {
        Ok(false)
    }
}
