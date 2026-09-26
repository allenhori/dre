//! Terminal output: right-aligned coloured status verbs, a live progress bar, log levels,
//! JSON lines for machines, and a full debug log in `logs/dre.log`: every event, and the full
//! text of every SQL statement sent to a database. The log rotates every 10,000 lines
//! (`DRE_LOG_MAX_LINES` overrides), keeping `dre.log.1` (newest) to `dre.log.5`.
//!
//! Lines go to stdout. The progress bar goes to stderr and only appears when stderr is a
//! terminal. Colour follows `--color`, `NO_COLOR` and whether stdout is a terminal.

use std::fs::File;
use std::io::{IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anstyle::{AnsiColor, Effects, Style};
use dre_core::run::{BindingOutcome, Level, Status, Ui};
use dre_protocol::host::LogSink;
use indicatif::{ProgressBar, ProgressDrawTarget, ProgressStyle};
use serde_json::json;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Verbosity {
    /// Errors and the final summary only.
    Quiet,
    /// One line per Binding, plus warnings (the default).
    Info,
    /// Every step: rendering, each statement, formatting, delivery, plugin logs.
    Debug,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum LogFormat {
    Text,
    /// One JSON object per line, for CI and tooling.
    Json,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum ColorChoice {
    Auto,
    Always,
    Never,
}

#[derive(Clone, Copy)]
pub enum Tone {
    Good,
    Bad,
    Warn,
    Step,
    Note,
}

fn style(t: Tone) -> Style {
    let c = match t {
        Tone::Good => AnsiColor::Green,
        Tone::Bad => AnsiColor::Red,
        Tone::Warn => AnsiColor::Yellow,
        Tone::Step => AnsiColor::Cyan,
        Tone::Note => AnsiColor::Blue,
    };
    Style::new().fg_color(Some(c.into())).effects(Effects::BOLD)
}

const DIM: Style = Style::new().effects(Effects::DIMMED);

struct Inner {
    verbosity: Verbosity,
    format: LogFormat,
    color: bool,
    bar: Option<ProgressBar>,
    log: Option<LogFile>,
    current: String,
    started: Instant,
    succeeded: usize,
    failed: usize,
}

/// Renders run events. Cheap to clone; clones share state (plugin log lines arrive on threads).
#[derive(Clone)]
pub struct Printer {
    inner: Arc<Mutex<Inner>>,
}

impl Printer {
    pub fn new(verbosity: Verbosity, format: LogFormat, color: ColorChoice) -> Printer {
        let color = match color {
            ColorChoice::Always => true,
            ColorChoice::Never => false,
            ColorChoice::Auto => {
                std::env::var_os("NO_COLOR").is_none_or(|v| v.is_empty()) && std::io::stdout().is_terminal()
            }
        } && format == LogFormat::Text;
        Printer {
            inner: Arc::new(Mutex::new(Inner {
                verbosity,
                format,
                color,
                bar: None,
                log: None,
                current: String::new(),
                started: Instant::now(),
                succeeded: 0,
                failed: 0,
            })),
        }
    }

    /// Also write every event, at debug level, to `<project>/logs/dre.log`.
    pub fn log_to(&self, project: &Path) {
        let max_lines = std::env::var("DRE_LOG_MAX_LINES")
            .ok()
            .and_then(|v| v.parse().ok())
            .filter(|n| *n > 0)
            .unwrap_or(LOG_MAX_LINES);
        if let Some(l) = LogFile::open(&project.join(LOGS_DIR).join("dre.log"), max_lines) {
            self.inner.lock().unwrap().log = Some(l);
        }
    }

    /// Colour a diagnostic's `error[...]`/`warning[...]` prefix.
    pub fn diagnostic(&self, d: &dre_core::Diagnostic) -> String {
        let i = self.inner.lock().unwrap();
        let text = dre_core::secrets::mask(&d.to_string()).into_owned();
        let (tone, prefix) = match d.severity {
            dre_core::Severity::Error => (Tone::Bad, "error"),
            dre_core::Severity::Warning => (Tone::Warn, "warning"),
        };
        match text.find("]: ") {
            Some(end) if i.color => format!("{}{}", i.paint(style(tone), &text[..end + 1]), &text[end + 1..]),
            _ => {
                let _ = prefix;
                text
            }
        }
    }

    /// `     Verb  text`, the verb right-aligned in a 10-column gutter.
    pub fn line(&self, tone: Tone, verb: &str, text: &str) {
        self.inner.lock().unwrap().line(tone, verb, text, Level::Info);
    }

    pub fn error(&self, msg: &str) {
        let mut i = self.inner.lock().unwrap();
        i.file_log("ERROR", msg);
        if i.format == LogFormat::Json {
            i.json(json!({"event": "error", "message": msg}));
        } else {
            i.print(Tone::Bad, "Error", msg);
        }
    }

    /// Record the run's parameters in the log file (and as a JSON event).
    pub fn log_params(&self, params: &serde_json::Value) {
        let mut i = self.inner.lock().unwrap();
        i.file_log("INFO", &format!("Parameters {params}"));
        if i.format == LogFormat::Json {
            i.json(json!({"event": "run_parameters", "parameters": params}));
        }
    }

    /// Where a compiled Binding's output would go: source, output file, each destination and
    /// its target (non-dev targets stand out), and the schedules that run it.
    pub fn plan(&self, p: &dre_core::run::BindingPlan) {
        let i = self.inner.lock().unwrap();
        let non_dev = |t: &str| t != "dev";
        i.print(Tone::Step, "Binding", &label(&p.report, p.set.as_deref()));
        for f in &p.compiled {
            i.print(Tone::Good, "Compiled", &f.display().to_string());
        }
        let source = format!("{} ({}), target {}", p.profile, p.source_type, p.target);
        i.print(
            if non_dev(&p.target) {
                Tone::Warn
            } else {
                Tone::Note
            },
            "Source",
            &source,
        );
        i.print(
            Tone::Note,
            "Output",
            &format!("{} ({})", p.output.display(), p.format),
        );
        for d in &p.destinations {
            let target = d.target.clone().unwrap_or_default();
            let what = match (&d.kind, &d.path) {
                (Some(k), Some(path)) => format!("{} ({k}), target {target} → {path}", d.profile),
                (Some(k), None) => format!("{} ({k}), target {target}", d.profile),
                (None, _) => format!("{}: no `{target}` target, so nothing is delivered", d.profile),
            };
            let tone = if d.delivers && non_dev(&target) {
                Tone::Warn
            } else {
                Tone::Note
            };
            i.print(tone, if d.delivers { "Delivers" } else { "Keeps" }, &what);
        }
        if !p.schedules.is_empty() {
            i.print(Tone::Note, "Schedules", &p.schedules.join(", "));
        }
    }

    /// Final line of a run.
    pub fn finish(&self, what: &str) {
        let mut i = self.inner.lock().unwrap();
        if let Some(b) = i.bar.take() {
            b.finish_and_clear();
        }
        let (ok, failed) = (i.succeeded, i.failed);
        let secs = i.started.elapsed().as_secs_f64();
        let text = format!("'{what}' in {} · {ok} succeeded, {failed} failed", fmt_secs(secs));
        i.file_log("INFO", &format!("Finished {text}"));
        if i.format == LogFormat::Json {
            i.json(json!({"event": "finished", "command": what, "succeeded": ok, "failed": failed, "elapsed_ms": (secs * 1000.0) as u64}));
        } else {
            i.print(if failed > 0 { Tone::Bad } else { Tone::Good }, "Finished", &text);
        }
    }
}

impl Inner {
    fn paint(&self, s: Style, text: &str) -> String {
        if self.color {
            format!("{s}{text}{s:#}")
        } else {
            text.to_string()
        }
    }

    fn print(&self, tone: Tone, verb: &str, text: &str) {
        let verb = self.paint(style(tone), &format!("{verb:>10}"));
        // Continuation lines line up under the text; blank ones stay blank.
        let text = text
            .lines()
            .enumerate()
            .map(|(n, l)| {
                if n == 0 || l.trim().is_empty() {
                    l.trim_end().to_string()
                } else {
                    format!("{:>10}  {l}", "")
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let out = format!("{verb}  {text}");
        match &self.bar {
            Some(b) => b.suspend(|| println_stdout(&out)),
            None => println_stdout(&out),
        }
    }

    fn line(&mut self, tone: Tone, verb: &str, text: &str, level: Level) {
        self.file_log(
            if level == Level::Debug { "DEBUG" } else { "INFO" },
            &format!("{verb} {text}"),
        );
        if !self.shows(level) {
            return;
        }
        if self.format == LogFormat::Json {
            self.json(json!({"event": "line", "verb": verb, "message": text}));
        } else {
            self.print(tone, verb, text);
        }
    }

    fn shows(&self, level: Level) -> bool {
        match self.verbosity {
            Verbosity::Quiet => false,
            Verbosity::Info => level == Level::Info,
            Verbosity::Debug => true,
        }
    }

    fn json(&self, mut v: serde_json::Value) {
        v["ts"] = json!(chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true));
        let line = v.to_string();
        match &self.bar {
            Some(b) => b.suspend(|| println_stdout(&line)),
            None => println_stdout(&line),
        }
    }

    fn file_log(&mut self, level: &str, msg: &str) {
        if let Some(f) = self.log.as_mut() {
            let ts = chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Millis, true);
            let ctx = if self.current.is_empty() {
                String::new()
            } else {
                format!(" [{}]", self.current)
            };
            f.write(&dre_core::secrets::mask(&format!("{ts} {level:<5}{ctx} {msg}\n")));
        }
    }
}

pub use dre_core::project::LOGS_DIR;
const LOG_MAX_LINES: usize = 10_000;
/// Rotated files kept: `dre.log.1` (newest) to `dre.log.5`.
const LOG_KEEP: usize = 5;

/// An append-only log that rotates once it reaches `max_lines`.
struct LogFile {
    path: PathBuf,
    file: File,
    lines: usize,
    max_lines: usize,
}

impl LogFile {
    fn open(path: &Path, max_lines: usize) -> Option<LogFile> {
        std::fs::create_dir_all(path.parent()?).ok()?;
        let lines = std::fs::read(path).map_or(0, |b| b.iter().filter(|c| **c == b'\n').count());
        let file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .ok()?;
        let mut l = LogFile {
            path: path.to_path_buf(),
            file,
            lines,
            max_lines,
        };
        if l.lines >= l.max_lines {
            l.rotate();
        }
        Some(l)
    }

    /// Entries are never split across files; a file can end a few lines over the limit.
    fn write(&mut self, entry: &str) {
        let _ = self.file.write_all(entry.as_bytes());
        self.lines += entry.matches('\n').count();
        if self.lines >= self.max_lines {
            self.rotate();
        }
    }

    fn rotate(&mut self) {
        let _ = self.file.flush();
        let name = |n: usize| PathBuf::from(format!("{}.{n}", self.path.display()));
        let _ = std::fs::remove_file(name(LOG_KEEP));
        for n in (1..LOG_KEEP).rev() {
            let _ = std::fs::rename(name(n), name(n + 1));
        }
        let _ = std::fs::rename(&self.path, name(1));
        if let Ok(f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)
        {
            self.file = f;
            self.lines = 0;
        }
    }
}

/// Everything printed goes through here, so `DRE_SECRET_*` values are masked on the console.
fn println_stdout(s: &str) {
    let mut out = std::io::stdout().lock();
    let _ = writeln!(out, "{}", dre_core::secrets::mask(s));
}

fn fmt_secs(s: f64) -> String {
    if s < 60.0 {
        format!("{s:.2}s")
    } else {
        format!("{}m {:02}s", (s / 60.0) as u64, (s % 60.0) as u64)
    }
}

fn timing(d: Duration) -> String {
    format!("[{:>6}]", fmt_secs(d.as_secs_f64()))
}

fn label(report: &str, set: Option<&str>) -> String {
    match set {
        Some(s) => format!("{report} [{s}]"),
        None => report.to_string(),
    }
}

impl Ui for Printer {
    fn plan(&mut self, bindings: usize) {
        let mut i = self.inner.lock().unwrap();
        i.started = Instant::now();
        let text = format!("{bindings} Binding{}", if bindings == 1 { "" } else { "s" });
        i.file_log("INFO", &format!("Running {text}"));
        if i.format == LogFormat::Json {
            i.json(json!({"event": "plan", "bindings": bindings}));
            return;
        }
        if i.verbosity != Verbosity::Quiet {
            i.print(Tone::Note, "Running", &text);
        }
        if i.verbosity != Verbosity::Quiet && std::io::stderr().is_terminal() && bindings > 0 {
            let bar = ProgressBar::with_draw_target(Some(bindings as u64), ProgressDrawTarget::stderr());
            bar.set_style(
                ProgressStyle::with_template("{spinner:.green} [{bar:28.green/dim}] {pos}/{len} {wide_msg}")
                    .unwrap()
                    .progress_chars("█▉░"),
            );
            bar.enable_steady_tick(Duration::from_millis(100));
            i.bar = Some(bar);
        }
    }

    fn binding_start(&mut self, report: &str, set: Option<&str>) {
        let mut i = self.inner.lock().unwrap();
        i.current = label(report, set);
        i.file_log("INFO", "Started");
        if let Some(b) = &i.bar {
            b.set_message(i.current.clone());
        }
        if i.format == LogFormat::Json {
            i.json(json!({"event": "binding_start", "report": report, "set": set}));
        } else if i.verbosity == Verbosity::Debug {
            let text = i.current.clone();
            i.print(Tone::Note, "Started", &text);
        }
    }

    fn step(&mut self, level: Level, verb: &str, detail: &str, elapsed: Option<Duration>) {
        let mut i = self.inner.lock().unwrap();
        if let Some(b) = &i.bar {
            b.set_message(format!("{} · {verb} {detail}", i.current));
        }
        let text = match elapsed {
            Some(d) => format!("{} {detail}", i.paint(DIM, &timing(d))),
            None => detail.to_string(),
        };
        if i.format == LogFormat::Json {
            i.file_log("DEBUG", &format!("{verb} {detail}"));
            if i.shows(level) {
                let ms = elapsed.map(|d| d.as_millis() as u64);
                i.json(json!({"event": "step", "binding": i.current, "verb": verb, "message": detail, "elapsed_ms": ms}));
            }
            return;
        }
        i.line(Tone::Step, verb, &text, level);
    }

    fn warn(&mut self, msg: &str) {
        let mut i = self.inner.lock().unwrap();
        i.file_log("WARN", msg);
        if i.format == LogFormat::Json {
            i.json(json!({"event": "warning", "binding": i.current, "message": msg}));
        } else if i.verbosity != Verbosity::Quiet {
            i.print(Tone::Warn, "Warning", msg);
        }
    }

    fn binding_vars(
        &mut self,
        schedule: Option<&str>,
        schedule_vars: Option<&serde_json::Map<String, serde_json::Value>>,
        vars: &serde_json::Map<String, serde_json::Value>,
    ) {
        let mut i = self.inner.lock().unwrap();
        if let Some(name) = schedule {
            let sv = serde_json::Value::Object(schedule_vars.cloned().unwrap_or_default());
            i.file_log("INFO", &format!("Schedule {name} vars {sv}"));
        }
        let v = serde_json::Value::Object(vars.clone());
        i.file_log("INFO", &format!("Vars {v}"));
        if i.format == LogFormat::Json {
            i.json(json!({"event": "binding_vars", "schedule": schedule, "schedule_vars": schedule_vars, "vars": vars}));
        } else if i.shows(Level::Debug) {
            let text = i.paint(DIM, &v.to_string());
            i.print(Tone::Note, "Vars", &text);
        }
    }

    fn compiled(&mut self, p: &dre_core::run::BindingPlan) {
        let mut i = self.inner.lock().unwrap();
        for f in &p.compiled {
            i.file_log("INFO", &format!("Compiled {}", f.display()));
        }
        if i.format == LogFormat::Json {
            i.json(json!({"event": "compiled", "plan": p}));
        } else {
            for f in &p.compiled {
                i.print(Tone::Good, "Compiled", &f.display().to_string());
            }
        }
    }

    fn binding_end(&mut self, o: &BindingOutcome) {
        let mut i = self.inner.lock().unwrap();
        let name = label(&o.report, o.set.as_deref());
        let (tone, verb) = match o.status {
            Status::Error => (Tone::Bad, "Failed"),
            Status::Success => (Tone::Good, "Succeeded"),
            Status::DryRun => (Tone::Good, "Compiled"),
            Status::Checked => (Tone::Good, "Checked"),
        };
        if o.status == Status::Error {
            i.failed += 1;
        } else {
            i.succeeded += 1;
        }
        if let Some(b) = &i.bar {
            b.inc(1);
        }
        let detail = match &o.error {
            Some(e) => e.clone(),
            None => o.summary.clone(),
        };
        i.file_log(
            if o.error.is_some() { "ERROR" } else { "INFO" },
            &format!("{verb} in {} {detail}", fmt_secs(o.elapsed.as_secs_f64())),
        );
        i.current.clear();
        if i.format == LogFormat::Json {
            i.json(json!({
                "event": "binding_end", "report": o.report, "set": o.set, "status": o.status,
                "elapsed_ms": o.elapsed.as_millis() as u64, "error": o.error, "summary": o.summary,
                "files": o.files, "schedule": o.schedule, "schedule_vars": o.schedule_vars, "vars": o.vars,
            }));
            return;
        }
        if i.verbosity == Verbosity::Quiet && o.status != Status::Error {
            return;
        }
        let name = i.paint(Style::new().effects(Effects::BOLD), &name);
        let text = format!("{} {name}  {detail}", i.paint(DIM, &timing(o.elapsed)));
        i.print(tone, verb, text.trim_end());
    }

    fn choose_set(&mut self, report: &str, sets: &[String]) -> Result<Option<String>, String> {
        let bar = self.inner.lock().unwrap().bar.clone();
        let ask = || -> Result<Option<String>, String> {
            eprintln!("Report `{report}` has several Sets:");
            for (i, s) in sets.iter().enumerate() {
                eprintln!("  {}) {s}", i + 1);
            }
            eprintln!("  a) all");
            loop {
                eprint!("Which one? ");
                let _ = std::io::stderr().flush();
                let mut line = String::new();
                if std::io::stdin().read_line(&mut line).map_err(|e| e.to_string())? == 0 {
                    return Err("no Set chosen".into());
                }
                let answer = line.trim();
                if answer == "a" || answer == "all" {
                    return Ok(None);
                }
                if let Ok(n) = answer.parse::<usize>()
                    && (1..=sets.len()).contains(&n)
                {
                    return Ok(Some(sets[n - 1].clone()));
                }
                if sets.iter().any(|s| s == answer) {
                    return Ok(Some(answer.to_string()));
                }
            }
        };
        match bar {
            Some(b) => b.suspend(ask),
            None => ask(),
        }
    }

    fn sql_log(&self) -> LogSink {
        let p = self.clone();
        Arc::new(move |label, sql| {
            let body: String = sql.trim_end().lines().map(|l| format!("\n    {l}")).collect();
            p.inner
                .lock()
                .unwrap()
                .file_log("DEBUG", &format!("SQL {label}:{body}"));
        })
    }

    fn plugin_log(&self) -> LogSink {
        let p = self.clone();
        Arc::new(move |plugin, line| {
            let mut i = p.inner.lock().unwrap();
            i.file_log("DEBUG", &format!("[{plugin}] {line}"));
            if !i.shows(Level::Debug) {
                return;
            }
            if i.format == LogFormat::Json {
                i.json(json!({"event": "plugin_log", "plugin": plugin, "message": line}));
            } else {
                let text = i.paint(DIM, &format!("[{plugin}] {line}"));
                i.print(Tone::Note, "Plugin", &text);
            }
        })
    }
}
