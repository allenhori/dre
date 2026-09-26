use std::path::PathBuf;
use std::process::ExitCode;

use clap::{Args, Parser, Subcommand};
use dre_core::project::{self, LoadOptions};

#[derive(Parser)]
#[command(name = "dre", version = dre_core::version(), about = "DRE, the Declarative Reporting Engine: SQL in, formatted files out")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Check the whole project offline: config, references, templates, schedules.
    Validate(ValidateArgs),
    /// Manage plugins (sources, formats, destinations).
    #[command(subcommand)]
    Plugin(PluginCommand),
}

#[derive(Subcommand)]
enum PluginCommand {
    /// List installed plugins, with each one's version and protocol version.
    List,
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
    #[command(flatten)]
    project: ProjectArgs,
    /// Emit machine-readable JSON instead of text.
    #[arg(long)]
    json: bool,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match cli.command {
        Command::Validate(a) => validate(a),
        Command::Plugin(PluginCommand::List) => plugin_list(),
    }
}

fn validate(a: ValidateArgs) -> ExitCode {
    let (project, diags) = project::load(&a.project.project_dir, &a.project.load_options());
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
            println!("{d}");
        }
        let (e, w) = (diags.error_count(), diags.warning_count());
        let verdict = if ok { "passed" } else { "failed" };
        println!(
            "Validation {verdict}: {e} error{}, {w} warning{}",
            plural(e),
            plural(w)
        );
    }
    if ok { ExitCode::SUCCESS } else { ExitCode::FAILURE }
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
