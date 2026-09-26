mod output;
mod plugins;

use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use dre_core::project::{self, LoadOptions};

#[derive(Parser)]
#[command(name = "dre", version = dre_core::version(), about = "DRE, the Declarative Reporting Engine: SQL in, formatted files out")]
struct Cli {
    #[command(subcommand)]
    command: Command,
    /// Show every step (same as `--log-level debug`).
    #[arg(short, long, global = true)]
    verbose: bool,
    /// Only show errors and the final summary.
    #[arg(short, long, global = true, conflicts_with = "verbose")]
    quiet: bool,
    /// How much to show.
    #[arg(long, global = true, value_enum)]
    log_level: Option<output::Verbosity>,
    /// `text` for people, `json` (one object per line) for CI and tooling.
    #[arg(long, global = true, value_enum, default_value = "text")]
    log_format: output::LogFormat,
    /// Colour output: auto (default; off when NO_COLOR is set or output isn't a terminal),
    /// always or never.
    #[arg(long, global = true, value_enum, default_value = "auto")]
    color: output::ColorChoice,
}

impl Cli {
    fn printer(&self) -> output::Printer {
        let v = self.log_level.unwrap_or(if self.verbose {
            output::Verbosity::Debug
        } else if self.quiet {
            output::Verbosity::Quiet
        } else {
            output::Verbosity::Info
        });
        output::Printer::new(v, self.log_format, self.color)
    }
}

#[derive(Subcommand)]
enum Command {
    /// Check the whole project offline: config, references, templates, schedules.
    Validate(ValidateArgs),
    /// Run reports: render, execute, format into target/, and deliver.
    Run(RunArgs),
    /// Remove target/ (compiled SQL, run outputs, schema snapshots).
    Clean(CleanArgs),
    /// Install the project's declared plugins (pinned by dre.lock) without running anything.
    Deps(DepsArgs),
    /// Manage plugins (sources, formats, destinations).
    #[command(subcommand)]
    Plugin(PluginCommand),
}

#[derive(Args)]
struct RunArgs {
    /// What to run: a report name, `tag:<tag>`, a folder name or a dotted folder path.
    /// Runs every report when omitted.
    selector: Option<String>,
    #[command(flatten)]
    project: ProjectArgs,
    /// Run one Set (declared or ad hoc), or `all` of a report's Sets.
    #[arg(long)]
    set: Option<String>,
    /// Use this source profile instead of the resolved one (e.g. for an ad hoc Set).
    #[arg(long)]
    profile: Option<String>,
    /// Override the output file name for this run.
    #[arg(long)]
    output_name: Option<String>,
    /// Override the full output (delivery) path for this run.
    #[arg(long)]
    output_path: Option<String>,
    /// Render SQL into target/compiled/ and stop; no report query is executed.
    #[arg(long)]
    dry_run: bool,
    /// Execute with a row limit (default 100); output stays in target/ and is never delivered.
    #[arg(long, value_name = "ROWS", num_args = 0..=1, default_missing_value = "100")]
    preview: Option<u64>,
    /// Deliver even if the output schema changed since the last successful run, and accept
    /// the new schema. Snapshots live in target/, so a fresh CI runner has no history.
    #[arg(long)]
    accept_schema_change: bool,
}

#[derive(Args)]
struct CleanArgs {
    /// Project directory (default: the current directory).
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
}

#[derive(Subcommand)]
enum PluginCommand {
    /// List installed plugins, with each one's version and protocol version.
    List,
    /// Install a plugin from the registry: `name`, `kind/name`, optionally `@<version req>`.
    Install(PluginArgs),
    /// Install the newest version allowed by the constraint (ignoring dre.lock's pin) and re-pin it.
    Update(PluginArgs),
    /// Remove installed versions of a plugin (`name@version` removes just one).
    Remove(PluginArgs),
}

#[derive(Args)]
struct PluginArgs {
    /// `duckdb`, `source/duckdb`, `xlsx@^1`, ...
    plugin: String,
    /// Project whose dre.lock to update (default: the current directory, if it's a project).
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
}

#[derive(Args)]
struct DepsArgs {
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
    #[arg(long)]
    profiles_dir: Option<PathBuf>,
}

#[derive(Args)]
struct ProjectArgs {
    /// Project directory (default: the current directory).
    #[arg(long, default_value = ".")]
    project_dir: PathBuf,
    /// Directory holding profiles.yml (default: $DRE_PROFILES_DIR, then ~/.dre).
    #[arg(long)]
    profiles_dir: Option<PathBuf>,
    /// Fail instead of installing declared plugins that are missing.
    #[arg(long)]
    no_auto_install: bool,
    /// Use this output of every profile instead of each profile's default `target`.
    #[arg(long)]
    target: Option<String>,
    /// Set a variable for `var()`, overriding every other level: `--var name=value`.
    #[arg(long = "var", value_name = "NAME=VALUE", value_parser = parse_var)]
    vars: Vec<(String, String)>,
}

impl ProjectArgs {
    fn load_options(&self) -> LoadOptions {
        LoadOptions {
            profiles_dir: self.profiles_dir.clone(),
            target: self.target.clone(),
            vars: self.vars.iter().cloned().collect(),
        }
    }
}

fn parse_var(s: &str) -> Result<(String, String), String> {
    match s.split_once('=') {
        Some((k, v)) if !k.trim().is_empty() => Ok((k.trim().to_string(), v.to_string())),
        _ => Err(format!("expected NAME=VALUE, got `{s}`")),
    }
}

#[derive(Args)]
struct ValidateArgs {
    /// With --live: which reports to check (same selectors as `dre run`; default: all).
    selector: Option<String>,
    #[command(flatten)]
    project: ProjectArgs,
    /// Emit machine-readable JSON instead of text.
    #[arg(long)]
    json: bool,
    /// After the offline checks, connect to each Binding's source and check every rendered
    /// statement without executing it (EXPLAIN or the dialect's equivalent).
    #[arg(long)]
    live: bool,
    /// With --live: check one Set, or `all` of them.
    #[arg(long)]
    set: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let printer = cli.printer();
    match cli.command {
        Command::Validate(a) => validate(a, &printer),
        Command::Run(a) => run(a, printer),
        Command::Clean(a) => clean(a),
        Command::Deps(a) => deps(a, &printer),
        Command::Plugin(PluginCommand::List) => plugin_list(),
        Command::Plugin(PluginCommand::Install(a)) => {
            plugins::install(a.plugin, a.project_dir, false, &printer)
        }
        Command::Plugin(PluginCommand::Update(a)) => {
            plugins::install(a.plugin, a.project_dir, true, &printer)
        }
        Command::Plugin(PluginCommand::Remove(a)) => plugins::remove(a.plugin, a.project_dir, &printer),
    }
}

fn validate(a: ValidateArgs, printer: &output::Printer) -> ExitCode {
    let (project, mut diags) = project::load(&a.project.project_dir, &a.project.load_options());
    if let Some(p) = &project {
        plugins::check_for_validate(p, !a.project.no_auto_install, &mut diags, printer);
    }
    let ok = !diags.has_errors();
    if a.json {
        let out = serde_json::json!({
            "ok": ok,
            "errors": diags.error_count(),
            "warnings": diags.warning_count(),
            "diagnostics": diags.sorted(),
            "project": project,
        });
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
    } else {
        for d in diags.sorted() {
            println!("{}", printer.diagnostic(d));
        }
        let (e, w) = (diags.error_count(), diags.warning_count());
        let verdict = if ok { "passed" } else { "failed" };
        println!(
            "Validation {verdict}: {e} error{}, {w} warning{}",
            plural(e),
            plural(w)
        );
    }
    if !ok {
        return ExitCode::FAILURE;
    }
    if a.live
        && let Some(project) = project
    {
        return validate_live(&project, a.selector, a.set, &a.project, printer.clone());
    }
    ExitCode::SUCCESS
}

fn validate_live(
    project: &dre_core::project::Project,
    selector: Option<String>,
    set: Option<String>,
    p: &ProjectArgs,
    mut printer: output::Printer,
) -> ExitCode {
    let opts = dre_core::run::RunOptions {
        selector,
        set,
        target: p.target.clone(),
        vars: p.vars.iter().cloned().collect(),
        date: run_date(),
        live_check: true,
        ..Default::default()
    };
    let summary = dre_core::run::run(project, &opts, &mut printer);
    if let Some(e) = &summary.error {
        printer.error(e);
        return ExitCode::FAILURE;
    }
    printer.finish("validate --live");
    if summary.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

fn plugin_list() -> ExitCode {
    let dir = dre_core::plugins::plugins_dir();
    let found = dre_core::plugins::discover(&dir);
    if found.is_empty() {
        println!("No plugins installed in {}", dir.display());
        return ExitCode::SUCCESS;
    }
    let quiet: dre_protocol::host::LogSink = std::sync::Arc::new(|_, _| {});
    let mut rows = vec![[
        "KIND".to_string(),
        "NAME".into(),
        "VERSION".into(),
        "PROTOCOL".into(),
        "PATH".into(),
    ]];
    for p in found {
        let (version, protocol) = match dre_protocol::host::PluginProcess::start(&p.path, quiet.clone()) {
            Ok(proc_) => {
                let info = proc_.info().clone();
                let _ = proc_.close();
                (info.version, format!("v{}", info.protocol_version))
            }
            Err(e) => (
                p.version.map(|v| v.to_string()).unwrap_or_else(|| "?".into()),
                format!("error: {e}"),
            ),
        };
        rows.push([
            p.kind.to_string(),
            p.name,
            version,
            protocol,
            p.path.display().to_string(),
        ]);
    }
    let widths: Vec<usize> = (0..5)
        .map(|i| rows.iter().map(|r| r[i].len()).max().unwrap_or(0))
        .collect();
    for r in rows {
        let line: Vec<String> = r
            .iter()
            .enumerate()
            .map(|(i, c)| format!("{c:<w$}", w = widths[i]))
            .collect();
        println!("{}", line.join("  ").trim_end());
    }
    ExitCode::SUCCESS
}

/// Load and validate the project; print problems. `None` when it can't run.
fn load_for_run(p: &ProjectArgs, printer: &output::Printer) -> Option<dre_core::project::Project> {
    let (project, diags) = project::load(&p.project_dir, &p.load_options());
    for d in diags.sorted() {
        // Unmanaged reports warn again when they run.
        if d.severity == dre_core::Severity::Warning && d.code == "unmanaged-report" {
            continue;
        }
        println!("{}", printer.diagnostic(d));
    }
    if diags.has_errors() {
        printer.error(&format!(
            "the project has {} error(s); fix them before running (see `dre validate`)",
            diags.error_count()
        ));
        return None;
    }
    project
}

fn run_date() -> Option<chrono::NaiveDate> {
    std::env::var("DRE_RUN_DATE")
        .ok()
        .and_then(|d| chrono::NaiveDate::parse_from_str(&d, "%Y-%m-%d").ok())
}

fn run(a: RunArgs, mut printer: output::Printer) -> ExitCode {
    use std::io::IsTerminal;
    let Some(project) = load_for_run(&a.project, &printer) else {
        return ExitCode::FAILURE;
    };
    printer.log_to(&project.root);
    if !plugins::ensure(&project, !a.project.no_auto_install, &printer) {
        return ExitCode::FAILURE;
    }
    let opts = dre_core::run::RunOptions {
        selector: a.selector,
        set: a.set,
        target: a.project.target.clone(),
        profile: a.profile,
        vars: a.project.vars.iter().cloned().collect(),
        output_name: a.output_name,
        output_path: a.output_path,
        dry_run: a.dry_run,
        preview: a.preview,
        accept_schema_change: a.accept_schema_change,
        interactive: std::io::stdin().is_terminal() && std::io::stderr().is_terminal(),
        date: run_date(),
        live_check: false,
    };
    let summary = dre_core::run::run(&project, &opts, &mut printer);
    if let Some(e) = &summary.error {
        printer.error(e);
        return ExitCode::FAILURE;
    }
    printer.finish("run");
    if summary.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn clean(a: CleanArgs) -> ExitCode {
    match dre_core::run::clean(&a.project_dir) {
        Ok(true) => {
            eprintln!("Removed target/");
            ExitCode::SUCCESS
        }
        Ok(false) => {
            eprintln!("Nothing to clean: no target/ directory");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("error: can't remove target/: {e}");
            ExitCode::FAILURE
        }
    }
}

fn deps(a: DepsArgs, printer: &output::Printer) -> ExitCode {
    let opts = LoadOptions {
        profiles_dir: a.profiles_dir,
        ..Default::default()
    };
    let (project, diags) = project::load(&a.project_dir, &opts);
    let Some(project) = project else {
        for d in diags.sorted() {
            println!("{}", printer.diagnostic(d));
        }
        return ExitCode::FAILURE;
    };
    if plugins::ensure(&project, true, printer) {
        printer.line(
            output::Tone::Good,
            "Synced",
            &format!(
                "{} declared plugin(s); dre.lock is up to date",
                project.plugins.len()
            ),
        );
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
