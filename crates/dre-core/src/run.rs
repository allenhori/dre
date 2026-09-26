//! The run path: turn a resolved project into delivered files.
//!
//! Per Binding: render → split → (unmanaged check) → execute on one session → format into
//! `target/run/` → schema-drift check → deliver → snapshot → `run_results.json`.

use std::collections::BTreeMap;
use std::fs::File;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use arrow::array::RecordBatch;
use arrow::datatypes::SchemaRef;
use arrow::ipc::reader::FileReader;
use arrow::ipc::writer::FileWriter;
use chrono::NaiveDate;
use dre_protocol::host::{Execution, LogSink, PluginProcess};
use dre_protocol::msg::{DeliveryFile, ResultSetMeta};
use dre_protocol::{CAP_MULTI_FILE, CAP_READ_ONLY, CAP_SESSIONS};
use serde::Serialize;
use serde_json::{Map as JsonMap, Value as Json, json};

use crate::profiles::{LOCAL_TYPE, ProfileOutput};
use crate::project::{Binding, PluginKind, Project, QueryEntry, Report, TARGET_DIR, TabName};
use crate::render::{QueryRows, QueryRunner, RenderError, Renderer, RendererConfig, RunContext};
use crate::sqlsplit::{self, StatementKind};
use crate::{lock, selector};

/// What the caller asked for.
#[derive(Debug, Clone, Default)]
pub struct RunOptions {
    pub selector: Option<String>,
    /// `--set <name>` or `--set all`.
    pub set: Option<String>,
    pub target: Option<String>,
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
    /// `run.date`.
    pub date: Option<NaiveDate>,
    /// `validate --live`: check statements instead of executing them.
    pub live_check: bool,
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
    /// One step inside the current Binding: a short verb, a detail, and how long it took.
    fn step(&mut self, level: Level, verb: &str, detail: &str, elapsed: Option<Duration>);
    fn warn(&mut self, msg: &str);
    fn binding_end(&mut self, _outcome: &BindingOutcome) {}
    /// Ask which Set to run; `None` means "all".
    fn choose_set(&mut self, report: &str, sets: &[String]) -> Result<Option<String>, String>;
    /// Where plugin stderr goes.
    fn plugin_log(&self) -> LogSink;
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
    #[serde(skip)]
    pub elapsed: Duration,
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
    let date = opts.date.unwrap_or_else(|| chrono::Local::now().date_naive());
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
        let mut r = BindingRun::new(project, report, b, opts, date, ui);
        let outcome = r.run();
        ui.binding_end(&outcome);
        summary.outcomes.push(outcome);
    }
    summary
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
        }
        b.vars.extend(reg.vars.clone());
    }
    b
}

/// A result set produced by one statement, spooled to disk.
struct Produced {
    query: String,
    /// 1-based among its query's result sets.
    index: usize,
    schema: SchemaRef,
    rows: u64,
    spool: PathBuf,
    name: String,
    anchor: Option<String>,
    header: Option<bool>,
}

struct Statement {
    query: String,
    file: PathBuf,
    line: usize,
    text: String,
    kind: StatementKind,
}

struct BindingRun<'a> {
    project: &'a Project,
    report: &'a Report,
    b: &'a Binding,
    opts: &'a RunOptions,
    date: NaiveDate,
    ui: &'a mut dyn Ui,
    compiled_dir: PathBuf,
    run_dir: PathBuf,
    schema_dir: PathBuf,
    target: String,
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
}

/// A destination entry after rendering.
struct RenderedDest {
    profile: String,
    path: Option<String>,
    options: JsonMap<String, Json>,
}

type Fail = String;

impl<'a> BindingRun<'a> {
    fn new(
        project: &'a Project,
        report: &'a Report,
        b: &'a Binding,
        opts: &'a RunOptions,
        date: NaiveDate,
        ui: &'a mut dyn Ui,
    ) -> Self {
        let t = project.root.join(TARGET_DIR);
        let rel = Path::new(&report.name).join(b.dir_name());
        BindingRun {
            project,
            report,
            b,
            opts,
            date,
            ui,
            compiled_dir: t.join("compiled").join(&rel),
            run_dir: t.join("run").join(&rel),
            schema_dir: t.join("schema").join(&rel),
            target: String::new(),
            started: Instant::now(),
            started_at: chrono::Utc::now(),
            produced: Vec::new(),
            files: Vec::new(),
            dests: Vec::new(),
            deliveries: Vec::new(),
            delivery_note: None,
            drift: Vec::new(),
        }
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
        let profile_name = self
            .b
            .profile
            .clone()
            .ok_or("no source profile resolves for this Binding")?;
        let profiles = &self.project.profiles;
        let profile = profiles
            .get(&profile_name)
            .ok_or_else(|| format!("source profile `{profile_name}` isn't in profiles.yml"))?;
        let target = self.opts.target.clone().unwrap_or_else(|| profile.target.clone());
        let (_, output) = profiles.output(&profile_name, Some(&target)).ok_or_else(|| {
            format!(
                "source profile `{profile_name}` has no `{target}` output (it has: {})",
                profile.outputs.keys().cloned().collect::<Vec<_>>().join(", ")
            )
        })?;
        self.target = target.clone();
        let source_path = find_plugin(self.project, PluginKind::Source, &output.kind)?;
        let connection = render_connection(output)?;

        let session = Arc::new(Mutex::new(Session::new(
            source_path,
            connection,
            self.project.root.clone(),
            self.ui.plugin_log(),
            !self.report.managed,
        )));

        if !self.report.managed {
            self.ui.warn(&format!(
                "`{}` is an unmanaged report ({}): for quick tests only — add a YAML to make it a managed report",
                self.report.name,
                self.report.file.display()
            ));
        }

        // 1. Render.
        let renderer = Renderer::new(RendererConfig {
            root: &self.project.root,
            macros: &self.project.macros,
            context: RunContext {
                report: self.report.name.clone(),
                set: self.b.set.clone(),
                target: target.clone(),
                profile: profile_name.clone(),
                date: self.date,
            },
            vars: self.b.vars.clone(),
            cli_vars: self.opts.vars.clone(),
            runner: Some(Arc::new(SessionRunner(session.clone()))),
            run_query_max_rows: self.project.run_query_max_rows,
        })
        .map_err(|e| e.to_string())?;
        std::fs::create_dir_all(&self.compiled_dir).map_err(|e| e.to_string())?;
        let mut statements = Vec::new();
        for q in &self.b.queries {
            let src = std::fs::read_to_string(self.project.root.join(&q.path))
                .map_err(|e| format!("{}: {e}", q.path.display()))?;
            let sql = renderer
                .render(&q.path, &src)
                .map_err(|e: RenderError| e.to_string())?;
            std::fs::write(self.compiled_dir.join(format!("{}.sql", q.query)), &sql)
                .map_err(|e| e.to_string())?;
            self.ui
                .step(Level::Debug, "Rendered", &q.path.display().to_string(), None);
            // 2. Split.
            for st in sqlsplit::split(&sql) {
                let body = sqlsplit::strip_leading_comments(&st.text);
                let line = st.line + st.text[..st.text.len() - body.len()].matches('\n').count();
                statements.push(Statement {
                    query: q.query.clone(),
                    file: q.path.clone(),
                    line,
                    kind: sqlsplit::classify(&st.text),
                    text: st.text,
                });
            }
        }
        self.dests = self.render_destinations(&renderer)?;

        // 3. Unmanaged: every rendered statement must only read (or create temp objects).
        if !self.report.managed {
            if let Some(bad) = statements.iter().find(|s| !s.kind.is_read_only_safe()) {
                return Err(format!(
                    "{}:{}: unmanaged report `{}` may only run SELECT/WITH or CREATE [OR REPLACE] TEMP|TEMPORARY TABLE|VIEW, but found `{}`; nothing was run — rewrite the statement, or give the report a YAML to declare it",
                    bad.file.display(),
                    bad.line,
                    self.report.name,
                    dre_protocol::util::summarize(&bad.text, 60)
                ));
            }
            let creates_temp = statements.iter().any(|s| s.kind == StatementKind::TempCreate);
            session.lock().unwrap().want_read_only(!creates_temp);
        }

        if self.opts.dry_run {
            let dir = rel(&self.project.root, &self.compiled_dir);
            self.ui.step(
                Level::Info,
                "Compiled",
                &format!("{} (dry run: nothing executed)", dir.display()),
                None,
            );
            return Ok(());
        }

        // The one-session guarantee.
        {
            let mut s = session.lock().unwrap();
            let p = s.get()?;
            if statements.len() > 1 && !p.has(CAP_SESSIONS) {
                return Err(format!(
                    "this Binding runs {} statements, but the `{}` source plugin can't hold one session across them; nothing was run",
                    statements.len(),
                    output.kind
                ));
            }
        }

        if self.opts.live_check {
            return self.live_check(&session, &statements, &output.kind);
        }

        // 4. Execute in order on one session.
        std::fs::create_dir_all(self.run_dir.join(".spool")).map_err(|e| e.to_string())?;
        let mut per_query: BTreeMap<String, usize> = BTreeMap::new();
        for (i, st) in statements.iter().enumerate() {
            let spool_path = self.run_dir.join(".spool").join(format!("{i}.arrow"));
            let mut writer: Option<FileWriter<File>> = None;
            let t = Instant::now();
            let exec = {
                let mut s = session.lock().unwrap();
                let p = s.get()?;
                p.execute(&st.text, self.opts.preview, |schema, batch| {
                    if writer.is_none() {
                        let f = File::create(&spool_path).map_err(|e| e.to_string())?;
                        writer = Some(FileWriter::try_new(f, schema).map_err(|e| e.to_string())?);
                    }
                    writer.as_mut().unwrap().write(&batch).map_err(|e| e.to_string())
                })
                .map_err(|e| format!("{}:{}: {e}", st.file.display(), st.line))?
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
            if let Execution::Result { schema, rows } = exec {
                let mut w = match writer {
                    Some(w) => w,
                    None => {
                        FileWriter::try_new(File::create(&spool_path).map_err(|e| e.to_string())?, &schema)
                            .map_err(|e| e.to_string())?
                    }
                };
                w.finish().map_err(|e| e.to_string())?;
                let n = per_query.entry(st.query.clone()).or_default();
                *n += 1;
                self.produced.push(Produced {
                    query: st.query.clone(),
                    index: *n,
                    schema,
                    rows,
                    spool: spool_path,
                    name: String::new(),
                    anchor: None,
                    header: None,
                });
            }
        }
        if let Ok(mut s) = session.lock() {
            s.close();
        }

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
        if self.opts.preview.is_none() {
            self.write_snapshot()
                .map_err(|e| format!("can't write the schema snapshot: {e}"))?;
        }
        Ok(())
    }

    fn render_destinations(&self, renderer: &Renderer) -> Result<Vec<RenderedDest>, Fail> {
        self.b
            .output
            .destinations
            .iter()
            .map(|d| {
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
                    let v = render_json(renderer, &self.report.file, v)
                        .map_err(|e| format!("destination `{}` option `{k}`: {e}", d.profile))?;
                    options.insert(k.clone(), v);
                }
                Ok(RenderedDest {
                    profile: d.profile.clone(),
                    path,
                    options,
                })
            })
            .collect()
    }

    /// Sheet names: `tab_name`, a `tab_name` list, the basename, or `<basename>_N`.
    fn name_result_sets(&mut self) -> Result<(), Fail> {
        let entries: BTreeMap<&str, &QueryEntry> =
            self.b.queries.iter().map(|q| (q.query.as_str(), q)).collect();
        let counts: BTreeMap<String, usize> = self.produced.iter().fold(BTreeMap::new(), |mut m, p| {
            *m.entry(p.query.clone()).or_default() += 1;
            m
        });
        for p in &mut self.produced {
            let q = entries[p.query.as_str()];
            let k = counts[&p.query];
            p.name = match (&q.tab_name, k) {
                (Some(TabName::One(s)), 1) => s.clone(),
                (Some(TabName::Many(l)), n) if l.len() == n => l[p.index - 1].clone(),
                (Some(TabName::Many(l)), n) => {
                    return Err(format!(
                        "`{}` has {} tab names but returned {n} result set{}",
                        p.query,
                        l.len(),
                        if n == 1 { "" } else { "s" }
                    ));
                }
                (Some(TabName::One(s)), n) => {
                    return Err(format!(
                        "`{}` returned {n} result sets but `tab_name` is the single name `{s}`; give a list of {n} names",
                        p.query
                    ));
                }
                (None, 1) => p.query.clone(),
                (None, _) => format!("{}_{}", p.query, p.index),
            };
            p.anchor = q.anchor.clone();
            p.header = q.header;
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
        let ext = extension(&self.b.output.format);
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
            .unwrap_or_else(|| format!("{}.{ext}", self.report.name))
    }

    fn format(&mut self, filename: &str) -> Result<(), Fail> {
        std::fs::create_dir_all(&self.run_dir).map_err(|e| e.to_string())?;
        if self.produced.is_empty() {
            self.ui
                .warn("  no query returned a result set; no output file was written");
            return Ok(());
        }
        let out = &self.b.output;
        let plugin_path = find_plugin(self.project, PluginKind::Format, &out.format)?;
        let mut p = PluginProcess::start_in(&plugin_path, self.ui.plugin_log(), Some(&self.project.root))
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
                        result_index: r.index,
                        anchor: r.anchor.clone(),
                        header: r.header,
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
                    p.send_batch(&b).map_err(|e| e.to_string())?;
                }
                if !any {
                    p.send_batch(&RecordBatch::new_empty(schema))
                        .map_err(|e| e.to_string())?;
                }
                p.send(&dre_protocol::msg::Request::ResultSetEnd {})
                    .map_err(|e| e.to_string())?;
            }
            let files = p
                .write_finish()
                .map_err(|e| format!("{} format: {e}", out.format))?;
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

    fn template_payload(&self, t: &crate::project::Template) -> Result<Json, Fail> {
        let root = &self.project.root;
        let file = [root.join(&t.file), root.join("templates").join(&t.file)]
            .into_iter()
            .find(|p| p.is_file())
            .ok_or_else(|| format!("template file `{}` doesn't exist", t.file))?;
        let renderer = Renderer::new(RendererConfig {
            root,
            macros: &self.project.macros,
            context: RunContext {
                report: self.report.name.clone(),
                set: self.b.set.clone(),
                target: self.target.clone(),
                profile: self.b.profile.clone().unwrap_or_default(),
                date: self.date,
            },
            vars: self.b.vars.clone(),
            cli_vars: self.opts.vars.clone(),
            runner: None,
            run_query_max_rows: self.project.run_query_max_rows,
        })
        .map_err(|e| e.to_string())?;
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
                    let p = self.project.profiles.get(&d.profile)?;
                    p.outputs
                        .get(&p.target)
                        .or(p.outputs.values().next())
                        .map(|o| o.kind.clone())
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

    /// The destination profile's output for the active target.
    fn dest_output(&self, profile: &str) -> Option<(String, &ProfileOutput)> {
        let dtarget = self.dest_target(profile)?;
        self.project
            .profiles
            .output(profile, Some(&dtarget))
            .map(|(_, o)| (dtarget, o))
    }

    /// The target a destination profile delivers for: `--target`, else the profile's own.
    fn dest_target(&self, profile: &str) -> Option<String> {
        let p = self.project.profiles.get(profile)?;
        Some(self.opts.target.clone().unwrap_or_else(|| p.target.clone()))
    }

    fn skip_note(&self, profile: &str) -> String {
        let dtarget = self.dest_target(profile).unwrap_or_default();
        format!(
            "destination profile `{profile}` has no `{dtarget}` output: not delivered, output stays in target/"
        )
    }

    /// Deliver every file to one destination. `Ok(None)`: its profile has no output for the
    /// active target, so nothing was sent.
    fn deliver_one(&mut self, d: &RenderedDest) -> Result<Option<String>, Fail> {
        if self.project.profiles.get(&d.profile).is_none() {
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
        let mut p = PluginProcess::start_in(&plugin, self.ui.plugin_log(), Some(&self.project.root))
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
                    "columns": p.schema.fields().iter().map(|f| json!({"name": f.name(), "type": f.data_type().to_string()})).collect::<Vec<_>>(),
                })
            })
            .collect();
        json!({ "result_sets": sets })
    }

    fn schema_drift(&self) -> Vec<String> {
        let path = self.schema_dir.join("last_success.json");
        let Ok(prev) = std::fs::read_to_string(&path) else {
            return Vec::new();
        };
        let Ok(prev) = serde_json::from_str::<Json>(&prev) else {
            return Vec::new();
        };
        let cols = |v: &Json| -> BTreeMap<String, Vec<(String, String)>> {
            v["result_sets"]
                .as_array()
                .into_iter()
                .flatten()
                .map(|s| {
                    let c = s["columns"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .map(|c| {
                            (
                                c["name"].as_str().unwrap_or("").to_string(),
                                c["type"].as_str().unwrap_or("").to_string(),
                            )
                        })
                        .collect();
                    (s["name"].as_str().unwrap_or("").to_string(), c)
                })
                .collect()
        };
        let (old, new) = (cols(&prev), cols(&self.snapshot()));
        let mut drift = Vec::new();
        for (name, oc) in &old {
            let Some(nc) = new.get(name) else {
                drift.push(format!("result set `{name}` is gone"));
                continue;
            };
            let om: BTreeMap<_, _> = oc.iter().cloned().collect();
            let nm: BTreeMap<_, _> = nc.iter().cloned().collect();
            for (c, t) in &om {
                match nm.get(c) {
                    None => drift.push(format!("`{name}`: column `{c}` removed")),
                    Some(t2) if t2 != t => {
                        drift.push(format!("`{name}`: column `{c}` changed type from {t} to {t2}"))
                    }
                    _ => {}
                }
            }
            for c in nm.keys().filter(|c| !om.contains_key(*c)) {
                drift.push(format!("`{name}`: column `{c}` added"));
            }
        }
        for name in new.keys().filter(|n| !old.contains_key(*n)) {
            drift.push(format!("result set `{name}` is new"));
        }
        drift
    }

    fn write_snapshot(&self) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.schema_dir)?;
        std::fs::write(
            self.schema_dir.join("last_success.json"),
            serde_json::to_string_pretty(&self.snapshot())?,
        )
    }

    fn write_results(&self, status: &Status, error: Option<&str>) -> std::io::Result<()> {
        std::fs::create_dir_all(&self.run_dir)?;
        let outputs: Vec<Json> = self
            .files
            .iter()
            .map(|(f, delivered)| {
                json!({
                    "path": rel(&self.project.root, f),
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
            "profile": self.b.profile,
            "target": if self.target.is_empty() { Json::Null } else { json!(self.target) },
            "status": status,
            "error": error,
            "preview": self.opts.preview.is_some(),
            "row_limit": self.opts.preview,
            "started_at": self.started_at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            "duration_ms": self.started.elapsed().as_millis() as u64,
            "result_sets": self.produced.iter().map(|p| json!({
                "name": p.name,
                "query": p.query,
                "rows": p.rows,
                "columns": p.schema.fields().iter().map(|f| f.name().clone()).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
            "outputs": outputs,
            "delivery": self.delivery_note,
            "deliveries": self.deliveries,
            "schema_drift": self.drift,
        });
        std::fs::write(
            self.run_dir.join("run_results.json"),
            serde_json::to_string_pretty(&results)? + "\n",
        )
    }

    /// `dre validate --live`: execute temp creates, `check` everything else.
    fn live_check(
        &mut self,
        session: &Arc<Mutex<Session>>,
        statements: &[Statement],
        kind: &str,
    ) -> Result<(), Fail> {
        let mut s = session.lock().unwrap();
        let p = s.get()?;
        if !p.has(dre_protocol::CAP_CHECK) {
            self.ui.warn(&format!(
                "  not checkable: the `{kind}` source plugin can't check statements without running them"
            ));
            return Ok(());
        }
        let mut failures = Vec::new();
        let mut unexecuted_setup: Option<(PathBuf, usize)> = None;
        for st in statements {
            if st.kind == StatementKind::TempCreate {
                if let Err(e) = p.execute(&st.text, None, |_, _| Ok(())) {
                    failures.push(format!("{}:{}: {e}", st.file.display(), st.line));
                }
                continue;
            }
            match p.check(&st.text) {
                Ok(()) => {}
                Err(e) => {
                    let mut msg = format!("{}:{}: {e}", st.file.display(), st.line);
                    if let Some((f, l)) = &unexecuted_setup {
                        msg.push_str(&format!(
                            " (may be a false positive: the setup statement at {}:{l} wasn't executed during the check)",
                            f.display()
                        ));
                    }
                    failures.push(msg);
                }
            }
            if st.kind == StatementKind::Other {
                unexecuted_setup = Some((st.file.clone(), st.line));
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("\n    "))
        }
    }
}

/// The source session, started on first use (rendering may need it for `run_query()`).
struct Session {
    path: PathBuf,
    connection: JsonMap<String, Json>,
    cwd: PathBuf,
    log: LogSink,
    /// Unmanaged reports run read-only unless they create temp objects.
    unmanaged: bool,
    read_only: bool,
    proc_: Option<PluginProcess>,
}

impl Session {
    fn new(
        path: PathBuf,
        connection: JsonMap<String, Json>,
        cwd: PathBuf,
        log: LogSink,
        unmanaged: bool,
    ) -> Session {
        Session {
            path,
            connection,
            cwd,
            log,
            unmanaged,
            read_only: unmanaged,
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
            let mut p = PluginProcess::start_in(&self.path, self.log.clone(), Some(&self.cwd))
                .map_err(|e| e.to_string())?;
            let ro = self.unmanaged && self.read_only && p.has(CAP_READ_ONLY);
            p.open(self.connection.clone(), ro)
                .map_err(|e| format!("can't open the source connection: {e}"))?;
            self.proc_ = Some(p);
        }
        Ok(self.proc_.as_mut().unwrap())
    }

    fn close(&mut self) {
        if let Some(p) = self.proc_.take() {
            let _ = p.close();
        }
    }
}

struct SessionRunner(Arc<Mutex<Session>>);

impl QueryRunner for SessionRunner {
    fn run_query(&self, sql: &str, max_rows: u64) -> Result<QueryRows, String> {
        let mut s = self.0.lock().map_err(|_| "session lock poisoned".to_string())?;
        // An unmanaged report may only read, and run_query() runs before the file's own
        // statements are checked, so it gets the same rule up front.
        if s.unmanaged
            && let Some(bad) = sqlsplit::split(sql)
                .into_iter()
                .find(|st| sqlsplit::classify(&st.text) != StatementKind::Read)
        {
            return Err(format!(
                "run_query() in an unmanaged report may only read, but got `{}`; give the report a YAML to declare it",
                dre_protocol::util::summarize(&bad.text, 60)
            ));
        }
        let p = s.get()?;
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
            .map_err(|e| format!("run_query() failed: {e}"))?;
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
fn render_connection(output: &ProfileOutput) -> Result<JsonMap<String, Json>, String> {
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

/// Locate the plugin for a declared type, honouring `dre.lock` pins and declared constraints.
pub fn find_plugin(project: &Project, kind: PluginKind, name: &str) -> Result<PathBuf, String> {
    let req = project
        .plugins
        .iter()
        .find(|p| p.kind == kind && p.name == name)
        .map(|p| p.req());
    let pin = lock::Lock::load(&project.root)
        .ok()
        .and_then(|l| l.version(kind, name));
    let dir = crate::plugins::plugins_dir();
    crate::plugins::find(&dir, kind, name, req.as_ref(), pin.as_ref()).map(|p| p.path).ok_or_else(|| {
        format!(
            "the {} plugin `{name}` isn't installed (looked in {}); run `dre deps` to install the project's plugins",
            kind.as_str(),
            dir.display()
        )
    })
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
        let delivered: Vec<&str> = self.files.iter().filter_map(|(_, d)| d.as_deref()).collect();
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

/// `dre clean`: remove `target/`.
pub fn clean(root: &Path) -> std::io::Result<bool> {
    let t = root.join(TARGET_DIR);
    if t.exists() {
        std::fs::remove_dir_all(&t)?;
        Ok(true)
    } else {
        Ok(false)
    }
}
