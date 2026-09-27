mod init;
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
    /// Check the project (config, references, templates, schedules) and compile its SQL.
    Validate(ValidateArgs),
    /// Run reports: render, execute, format into target/, and deliver.
    Run(RunArgs),
    /// Render reports' SQL into target/compiled/ without running it, and list the files.
    Compile(CompileArgs),
    /// Remove target/ (compiled SQL, run outputs, schema snapshots).
    Clean(CleanArgs),
    /// Set up a connection (installing its plugin) and optionally a starter project, interactively.
    Init(InitArgs),
    /// Create a starter project in a new directory.
    New(NewArgs),
    /// Install the project's declared plugins (pinned by dre.lock) without running anything.
    Deps(DepsArgs),
    /// Manage plugins (sources, formats, destinations).
    #[command(subcommand)]
    Plugin(PluginCommand),
}

#[derive(Args)]
struct RunArgs {
    /// What to run: report names, `tag:<tag>`, folder names or dotted folder paths
    /// (`dre run a b` runs both). Runs every report when omitted.
    selector: Vec<String>,
    /// What to select (dbt's `--select`): report names, `tag:<tag>`, folder names or dotted
    /// folder paths. Several match any of them: `-s a b`, `-s a,b`, `-s a -s b` (or `"a;b"`).
    #[arg(short = 's', long = "select", value_name = "SELECTOR", num_args = 1.., action = clap::ArgAction::Append, conflicts_with = "selector")]
    select: Vec<String>,
    #[command(flatten)]
    project: ProjectArgs,
    /// Run one Set (declared or ad hoc), or `all` of a report's Sets.
    #[arg(long)]
    set: Option<String>,
    /// Run the Bindings a schedules.yml entry targets, with its vars. Pass the scheduled
    /// (logical) date through DRE_RUN_DATE so reruns render the same.
    #[arg(long, value_name = "NAME", conflicts_with_all = ["selector", "select", "set"])]
    schedule: Option<String>,
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
struct InitArgs {
    /// Directory holding profiles.yml (default: $DRE_PROFILES_DIR, then ~/.dre).
    #[arg(long)]
    profiles_dir: Option<PathBuf>,
}

#[derive(Args)]
struct NewArgs {
    /// Directory to create (must be missing or empty).
    dir: PathBuf,
    /// The source profile the project uses by default.
    #[arg(long, default_value = "warehouse")]
    profile: String,
    /// The source plugin the project declares.
    #[arg(long, default_value = "duckdb")]
    source: String,
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
    /// Directory holding profiles.yml (default: $DRE_PROFILES_DIR, then the project directory
    /// if it has a profiles.yml, then ~/.dre).
    #[arg(long)]
    profiles_dir: Option<PathBuf>,
    /// Fail instead of installing declared plugins that are missing.
    #[arg(long)]
    no_auto_install: bool,
    /// Use this target (environment) of every profile instead of each profile's default `target`.
    #[arg(long)]
    target: Option<String>,
    /// Set a variable for `var()`, overriding every other level: `--var name=value`.
    #[arg(long = "var", value_name = "NAME=VALUE", value_parser = parse_var)]
    vars: Vec<(String, String)>,
    /// The run's timezone (IANA name, e.g. Australia/Sydney), above every `timezone:` setting
    /// (default: $DRE_TIMEZONE).
    #[arg(long)]
    timezone: Option<String>,
}

impl ProjectArgs {
    /// `--timezone`, else `DRE_TIMEZONE`.
    fn timezone(&self) -> Option<String> {
        self.timezone
            .clone()
            .or_else(|| std::env::var("DRE_TIMEZONE").ok().filter(|t| !t.is_empty()))
    }

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
    /// Which reports to compile and check (same selectors as `dre run`; default: all). With a
    /// selector, validate also shows where each selected Binding's output would go.
    /// Reports whose templates query the database (`run_query()`, `columns()`) connect, and
    /// may sign in, to compile.
    selector: Vec<String>,
    /// What to select (dbt's `--select`): report names, `tag:<tag>`, folder names or dotted
    /// folder paths. Several match any of them: `-s a b`, `-s a,b`, `-s a -s b` (or `"a;b"`).
    #[arg(short = 's', long = "select", value_name = "SELECTOR", num_args = 1.., action = clap::ArgAction::Append, conflicts_with = "selector")]
    select: Vec<String>,
    #[command(flatten)]
    project: ProjectArgs,
    /// Emit machine-readable JSON instead of text.
    #[arg(long)]
    json: bool,
    /// After the offline checks, connect to each Binding's source and check every rendered
    /// statement without executing it (EXPLAIN or the dialect's equivalent).
    #[arg(long)]
    live: bool,
    /// Compile (and with --live, check) one Set instead of every Set.
    #[arg(long)]
    set: Option<String>,
}

#[derive(Args)]
struct CompileArgs {
    /// What to compile: report names, `tag:<tag>`, folder names or dotted folder paths.
    /// Compiles every report when omitted.
    selector: Vec<String>,
    /// What to select (dbt's `--select`): report names, `tag:<tag>`, folder names or dotted
    /// folder paths. Several match any of them: `-s a b`, `-s a,b`, `-s a -s b` (or `"a;b"`).
    #[arg(short = 's', long = "select", value_name = "SELECTOR", num_args = 1.., action = clap::ArgAction::Append, conflicts_with = "selector")]
    select: Vec<String>,
    #[command(flatten)]
    project: ProjectArgs,
    /// Compile one Set (declared or ad hoc), or `all` of a report's Sets.
    #[arg(long)]
    set: Option<String>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let printer = cli.printer();
    let project_args = match &cli.command {
        Command::Validate(a) => Some(&a.project),
        Command::Run(a) => Some(&a.project),
        Command::Compile(a) => Some(&a.project),
        _ => None,
    };
    if let Some(t) = project_args.and_then(ProjectArgs::timezone)
        && let Err(e) = dre_core::dates::parse_tz(&t)
    {
        let from = if project_args.is_some_and(|p| p.timezone.is_some()) {
            "--timezone"
        } else {
            "DRE_TIMEZONE"
        };
        printer.error(&format!("{from}: {e}"));
        return ExitCode::from(2);
    }
    match cli.command {
        Command::Validate(a) => validate(a, &printer),
        Command::Run(a) => run(a, printer),
        Command::Compile(a) => compile(a, printer),
        Command::Clean(a) => clean(a),
        Command::Deps(a) => deps(a, &printer),
        Command::Init(a) => init::init(a.profiles_dir, &printer),
        Command::New(a) => init::new(a.dir, a.profile, a.source, &printer),
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
    // With auto-install off, a missing package is reported by the load.
    if !a.project.no_auto_install {
        plugins::sync_packages(&a.project.project_dir, printer);
    }
    let selector = selection(&a.select, &a.selector);
    let (project, mut diags) = project::load(&a.project.project_dir, &a.project.load_options());
    if let Some(p) = &project {
        dre_core::secrets::set_enabled(p.mask_secrets);
        plugins::check_for_validate(p, !a.project.no_auto_install, &mut diags, printer);
    }
    // Only a project that checks out gets compiled.
    let plans = match &project {
        Some(p) if !diags.has_errors() => compile_for_validate(p, &selector, &a.set, &a.project, &mut diags),
        _ => Vec::new(),
    };
    let ok = !diags.has_errors();
    if a.json {
        let out = serde_json::json!({
            "ok": ok,
            "profiles": project.as_ref().map(|p| serde_json::json!({
                "path": p.profiles.path,
                "exists": p.profiles.exists(),
                "found_by": p.profiles.found_by,
            })),
            "errors": diags.error_count(),
            "warnings": diags.warning_count(),
            "diagnostics": diags.sorted(),
            "compiled": plans,
            "project": project,
        });
        println!(
            "{}",
            dre_core::secrets::mask(&serde_json::to_string_pretty(&out).unwrap())
        );
    } else {
        for d in diags.sorted() {
            println!("{}", printer.diagnostic(d));
        }
        if selector.is_some() {
            for p in &plans {
                printer.plan(p);
            }
        } else if !plans.is_empty() {
            println!(
                "Compiled {} Binding{} into target/compiled/",
                plans.len(),
                plural(plans.len())
            );
        }
        if let Some(p) = &project {
            printer.line(output::Tone::Note, "Profiles", &profiles_line(&p.profiles));
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
        return validate_live(&project, selector, a.set, &a.project, printer.clone());
    }
    ExitCode::SUCCESS
}

/// Compiles quietly, collecting what each Binding would do.
#[derive(Default)]
struct Collect {
    plans: Vec<dre_core::run::BindingPlan>,
}

impl dre_core::run::Ui for Collect {
    fn step(&mut self, _: dre_core::run::Level, _: &str, _: &str, _: Option<std::time::Duration>) {}
    fn warn(&mut self, _: &str) {}
    fn compiled(&mut self, plan: &dre_core::run::BindingPlan) {
        self.plans.push(plan.clone());
    }
    fn choose_set(&mut self, _: &str, _: &[String]) -> Result<Option<String>, String> {
        Ok(None)
    }
    fn plugin_log(&self) -> dre_protocol::host::LogSink {
        std::sync::Arc::new(|_, _| {})
    }
}

/// `validate` compiles the selection (every Set of every report by default); a Binding that
/// doesn't render is an error.
fn compile_for_validate(
    project: &dre_core::project::Project,
    selector: &Option<String>,
    set: &Option<String>,
    p: &ProjectArgs,
    diags: &mut dre_core::Diagnostics,
) -> Vec<dre_core::run::BindingPlan> {
    let opts = dre_core::run::RunOptions {
        selector: selector.clone(),
        set: Some(set.clone().unwrap_or_else(|| "all".into())),
        target: p.target.clone(),
        vars: p.vars.iter().cloned().collect(),
        date: run_date(),
        timezone: p.timezone(),
        dry_run: true,
        ..Default::default()
    };
    let mut ui = Collect::default();
    let summary = dre_core::run::run(project, &opts, &mut ui);
    if let Some(e) = summary.error {
        diags.error("invalid-selector", None, None, e);
    }
    for o in summary
        .outcomes
        .iter()
        .filter(|o| o.status == dre_core::run::Status::Error)
    {
        let set = o.set.as_ref().map(|s| format!(", Set `{s}`")).unwrap_or_default();
        let err = o.error.clone().unwrap_or_default();
        // A template that queries what an earlier query makes (a temp table) can't render
        // without running that query; validate runs nothing, so that's for `dre run` to check.
        if err.contains("run_query() failed:") || (err.contains("`columns('") && err.contains("')` failed:")) {
            diags.warning(
                "compile-needs-run",
                None,
                None,
                format!(
                    "report `{}`{set} queries the database while rendering and can only be checked by `dre run`: {err}",
                    o.report
                ),
            );
            continue;
        }
        diags.error(
            "compile-failed",
            None,
            None,
            format!(
                "report `{}`{set} doesn't compile: {}",
                o.report,
                o.error.clone().unwrap_or_default()
            ),
        );
    }
    ui.plans
}

/// `dre compile`: render the selection into target/compiled/ and list the files.
fn compile(a: CompileArgs, mut printer: output::Printer) -> ExitCode {
    if !a.project.no_auto_install && !plugins::sync_packages(&a.project.project_dir, &printer) {
        return ExitCode::FAILURE;
    }
    let Some(project) = load_for_run(&a.project, &printer) else {
        return ExitCode::FAILURE;
    };
    printer.log_to(&project.root);
    let opts = dre_core::run::RunOptions {
        selector: selection(&a.select, &a.selector),
        set: a.set,
        target: a.project.target.clone(),
        vars: a.project.vars.iter().cloned().collect(),
        date: run_date(),
        timezone: a.project.timezone(),
        dry_run: true,
        interactive: {
            use std::io::IsTerminal;
            std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
        },
        ..Default::default()
    };
    let summary = dre_core::run::run(&project, &opts, &mut printer);
    if let Some(e) = &summary.error {
        printer.error(e);
        return ExitCode::FAILURE;
    }
    printer.finish("compile");
    if summary.failed() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
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
        timezone: p.timezone(),
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

/// `-s` values (joined: space means union) or the positional selector.
fn selection(select: &[String], positional: &[String]) -> Option<String> {
    let all = if select.is_empty() { positional } else { select };
    (!all.is_empty()).then(|| all.join(" "))
}

fn plural(n: usize) -> &'static str {
    if n == 1 { "" } else { "s" }
}

/// The project's plugins when run inside a project, otherwise the shared cache.
fn plugin_list() -> ExitCode {
    let here = std::path::Path::new(".");
    let in_project = here.join(dre_core::project::PROJECT_FILE).is_file();
    let dir = dre_core::plugins::plugins_dir(in_project.then_some(here));
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
/// Which profiles.yml a project uses, and why.
fn profiles_line(p: &dre_core::profiles::Profiles) -> String {
    let missing = if p.exists() { "" } else { ", not found" };
    format!("{} (from {}{missing})", p.path.display(), p.found_by)
}

fn load_for_run(p: &ProjectArgs, printer: &output::Printer) -> Option<dre_core::project::Project> {
    let (project, diags) = project::load(&p.project_dir, &p.load_options());
    if let Some(p) = &project {
        dre_core::secrets::set_enabled(p.mask_secrets);
        printer.detail(output::Tone::Note, "Profiles", &profiles_line(&p.profiles));
    }
    for d in diags.sorted() {
        // Unmanaged reports warn again when they run, and a selected Binding whose profile
        // lacks the `--target` fails with its own error; the rest aren't this run's concern.
        if d.severity == dre_core::Severity::Warning && matches!(d.code, "unmanaged-report" | "missing-target") {
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
    if !a.project.no_auto_install && !plugins::sync_packages(&a.project.project_dir, &printer) {
        return ExitCode::FAILURE;
    }
    let Some(project) = load_for_run(&a.project, &printer) else {
        return ExitCode::FAILURE;
    };
    printer.log_to(&project.root);
    if !plugins::ensure(&project, !a.project.no_auto_install, false, &printer) {
        return ExitCode::FAILURE;
    }
    let opts = dre_core::run::RunOptions {
        selector: selection(&a.select, &a.selector),
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
        timezone: a.project.timezone(),
        live_check: false,
        schedule: a.schedule,
    };
    if let Some(name) = &opts.schedule
        && !project.schedules.iter().any(|e| &e.name == name)
    {
        printer.error(&dre_core::run::unknown_schedule(&project, name));
        return ExitCode::from(2);
    }
    // Each Binding records its own date, in its own timezone; this line only logs the request.
    let date = opts.date.unwrap_or_else(|| chrono::Utc::now().date_naive());
    printer.log_params(&opts.params(date));
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
    if !plugins::sync_packages(&a.project_dir, printer) {
        return ExitCode::FAILURE;
    }
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
    if plugins::ensure(&project, true, true, printer) {
        printer.line(
            output::Tone::Good,
            "Synced",
            &format!(
                "{} plugin(s) and {} package(s); dre.lock is up to date",
                project.plugins.len(),
                project.packages.len()
            ),
        );
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
