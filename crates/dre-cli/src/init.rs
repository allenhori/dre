//! `dre init` (interactive onboarding) and `dre new` (scaffold a project).

use std::io::{BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use dre_core::manager::{self, Index, IndexPlugin};
use dre_core::project::PluginKind;
use dre_protocol::host::PluginProcess;
use dre_protocol::msg::ConnectionField;
use serde_yaml_ng::{Mapping, Value};

use crate::output::{Printer, Tone};

/// What a scaffolded project declares.
pub struct Scaffold {
    pub name: String,
    pub profile: String,
    pub source: String,
    pub destinations: Vec<(String, String)>,
}

/// Write a starter project into `dir`, which must be missing or empty.
pub fn scaffold(dir: &Path, s: &Scaffold) -> Result<Vec<PathBuf>, String> {
    if dir.exists()
        && std::fs::read_dir(dir)
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
    {
        return Err(format!("{} already exists and isn't empty", dir.display()));
    }
    let mut plugins = format!("sources:\n  - {}\nformats:\n  - csv\n", s.source);
    if !s.destinations.is_empty() {
        plugins.push_str("destinations:\n");
        let mut kinds: Vec<&str> = s.destinations.iter().map(|(t, _)| t.as_str()).collect();
        kinds.dedup();
        for k in kinds {
            plugins.push_str(&format!("  - {k}\n"));
        }
    }
    let output = match s.destinations.first() {
        Some((_, profile)) => format!(
            "\n# Delivered in addition to the copy in target/run/. Adjust the path for your destination.\n\
             output:\n  destination:\n    profile: {profile}\n    path: \"reports/{{{{ run.report }}}}-{{{{ run.date.yyyymmdd }}}}.csv\"\n"
        ),
        None => String::new(),
    };
    let files: Vec<(&str, String)> = vec![
        (
            "dre_project.yml",
            format!(
                "name: {}\n# Source profile (in ~/.dre/profiles.yml) for reports that don't name one.\ndefault_profile: {}\n",
                s.name, s.profile
            ),
        ),
        (
            "plugins.yml",
            format!("# Plugins this project needs. `dre deps` installs them.\n{plugins}"),
        ),
        (
            "reports/examples/hello/hello.yml",
            format!(
                "# A managed report: its queries, and (optionally) output, Sets and schedule.\n\
                 queries:\n  - {{query: hello, tab_name: Hello}}\n{output}"
            ),
        ),
        (
            "reports/examples/hello/hello.sql",
            "-- Jinja works everywhere: run.*, var(), env_var() and your macros.\n\
             select '{{ run.report }}' as report, '{{ run.date }}' as run_date, 'Hello from DRE' as message\n"
                .to_string(),
        ),
        ("macros/.gitkeep", String::new()),
        ("templates/.gitkeep", String::new()),
        (".gitignore", "target/\n".to_string()),
    ];
    let mut written = Vec::new();
    for (rel, content) in files {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap())
            .map_err(|e| format!("can't create {}: {e}", p.display()))?;
        std::fs::write(&p, content).map_err(|e| format!("can't write {}: {e}", p.display()))?;
        written.push(p);
    }
    Ok(written)
}

/// `dre new <dir>`.
pub fn new(dir: PathBuf, profile: String, source: String, printer: &Printer) -> ExitCode {
    let name = project_name(&dir);
    match scaffold(
        &dir,
        &Scaffold {
            name,
            profile,
            source,
            destinations: Vec::new(),
        },
    ) {
        Ok(files) => {
            printer.line(
                Tone::Good,
                "Created",
                &format!("{} ({} files)", dir.display(), files.len()),
            );
            printer.line(Tone::Note, "Next", &format!("cd {} && dre run", dir.display()));
            ExitCode::SUCCESS
        }
        Err(e) => {
            printer.error(&e);
            ExitCode::FAILURE
        }
    }
}

fn project_name(dir: &Path) -> String {
    let raw = dir
        .file_name()
        .map(|f| f.to_string_lossy().to_string())
        .unwrap_or_else(|| "my_reports".into());
    let s: String = raw
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() {
                c.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "my_reports".into() } else { s }
}

struct Prompter<R: BufRead> {
    input: R,
}

impl<R: BufRead> Prompter<R> {
    fn ask(&mut self, q: &str, default: Option<&str>) -> Result<String, String> {
        match default {
            Some(d) if !d.is_empty() => eprint!("{q} [{d}]: "),
            _ => eprint!("{q}: "),
        }
        let _ = std::io::stderr().flush();
        let mut line = String::new();
        if self.input.read_line(&mut line).map_err(|e| e.to_string())? == 0 {
            return match default {
                Some(d) => Ok(d.to_string()),
                None => Err("input ended before setup finished".into()),
            };
        }
        let a = line.trim().to_string();
        Ok(if a.is_empty() {
            default.unwrap_or_default().to_string()
        } else {
            a
        })
    }

    fn pick<'a>(
        &mut self,
        q: &str,
        options: &[&'a IndexPlugin],
        allow_none: bool,
    ) -> Result<Vec<&'a IndexPlugin>, String> {
        for (i, p) in options.iter().enumerate() {
            eprintln!("  {}) {:<18} {}", i + 1, p.name, p.description);
        }
        loop {
            let a = self.ask(q, if allow_none { Some("") } else { None })?;
            if a.is_empty() && allow_none {
                return Ok(Vec::new());
            }
            let mut picked = Vec::new();
            let mut ok = true;
            for part in a.split(',').map(str::trim).filter(|p| !p.is_empty()) {
                let found = part
                    .parse::<usize>()
                    .ok()
                    .and_then(|n| options.get(n.wrapping_sub(1)).copied())
                    .or_else(|| options.iter().find(|p| p.name == part).copied());
                match found {
                    Some(p) => picked.push(p),
                    None => ok = false,
                }
            }
            if ok && !picked.is_empty() && (allow_none || picked.len() == 1) {
                return Ok(picked);
            }
            eprintln!("Please choose from the list.");
        }
    }
}

/// Prompt for a plugin's connection fields; secrets default to an `env_var()` reference.
fn connection<R: BufRead>(
    p: &mut Prompter<R>,
    profile: &str,
    fields: &[ConnectionField],
) -> Result<Mapping, String> {
    let mut m = Mapping::new();
    for f in fields {
        let env_default = format!(
            "{{{{ env_var('{}_{}') }}}}",
            profile.to_uppercase(),
            f.name.to_uppercase()
        );
        let default = if f.secret {
            Some(env_default)
        } else {
            f.default.as_ref().map(|d| match d {
                serde_json::Value::String(s) => s.clone(),
                other => other.to_string(),
            })
        };
        let label = match (f.description.is_empty(), f.required) {
            (true, true) => format!("{} (required)", f.name),
            (true, false) => f.name.clone(),
            (false, true) => format!("{} — {} (required)", f.name, f.description),
            (false, false) => format!("{} — {}", f.name, f.description),
        };
        loop {
            let v = p.ask(&format!("  {label}"), default.as_deref())?;
            if v.is_empty() && f.required {
                eprintln!("  `{}` is required.", f.name);
                continue;
            }
            if !v.is_empty() {
                m.insert(Value::String(f.name.clone()), Value::String(v));
            }
            break;
        }
    }
    Ok(m)
}

fn install_and_describe(
    index_plugin: &IndexPlugin,
    printer: &Printer,
) -> Result<Vec<ConnectionField>, String> {
    let v = index_plugin.best(&semver::VersionReq::STAR).ok_or_else(|| {
        format!(
            "no release of `{}` for {}",
            index_plugin.name,
            manager::platform()
        )
    })?;
    let dir = dre_core::plugins::plugins_dir();
    let locked = manager::install(&dir, index_plugin, v, None)?;
    printer.line(
        Tone::Good,
        "Installed",
        &format!(
            "{} plugin `{}` {}",
            index_plugin.kind.as_str(),
            index_plugin.name,
            locked.version
        ),
    );
    let path = manager::install_path(&dir, index_plugin.kind, &index_plugin.name, &locked.version);
    let mut proc_ = PluginProcess::start(&path, std::sync::Arc::new(|_, _| {})).map_err(|e| e.to_string())?;
    let fields = proc_.describe().map_err(|e| e.to_string())?;
    let _ = proc_.close();
    Ok(fields)
}

/// Append one profile to profiles.yml, refusing to overwrite an existing one.
fn add_profile(path: &Path, name: &str, target: &str, kind: &str, fields: Mapping) -> Result<(), String> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    if let Ok(Value::Mapping(m)) = serde_yaml_ng::from_str::<Value>(&existing)
        && m.contains_key(name)
    {
        return Err(format!("profile `{name}` already exists in {}", path.display()));
    }
    let mut output = Mapping::new();
    output.insert("type".into(), Value::String(kind.into()));
    output.extend(fields);
    let mut outputs = Mapping::new();
    outputs.insert(Value::String(target.into()), Value::Mapping(output));
    let mut profile = Mapping::new();
    profile.insert("target".into(), Value::String(target.into()));
    profile.insert("outputs".into(), Value::Mapping(outputs));
    let mut root = Mapping::new();
    root.insert(Value::String(name.into()), Value::Mapping(profile));
    let block = serde_yaml_ng::to_string(&root).map_err(|e| e.to_string())?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut text = existing;
    if !text.is_empty() && !text.ends_with('\n') {
        text.push('\n');
    }
    if !text.is_empty() {
        text.push('\n');
    }
    text.push_str(&block);
    std::fs::write(path, text).map_err(|e| format!("can't write {}: {e}", path.display()))
}

/// `dre init`.
pub fn init(profiles_dir: Option<PathBuf>, printer: &Printer) -> ExitCode {
    match init_inner(profiles_dir, printer, std::io::stdin().lock()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            printer.error(&e);
            ExitCode::FAILURE
        }
    }
}

fn init_inner(profiles_dir: Option<PathBuf>, printer: &Printer, input: impl BufRead) -> Result<(), String> {
    let mut p = Prompter { input };
    let index = Index::load()?;
    let profiles_path =
        dre_core::profiles::profiles_dir(profiles_dir.as_deref()).join(dre_core::profiles::PROFILES_FILE);
    eprintln!("Welcome to DRE. This sets up a connection, installs its plugin and can start a project.\n");

    let sources: Vec<&IndexPlugin> = index
        .plugins
        .iter()
        .filter(|x| x.kind == PluginKind::Source)
        .collect();
    if sources.is_empty() {
        return Err("the plugin registry lists no sources".into());
    }
    eprintln!("Which database do you want to report from?");
    let source = p.pick("Source (number or name)", &sources, false)?[0];
    let fields = install_and_describe(source, printer)?;
    let profile = p.ask("Name for this connection profile", Some("warehouse"))?;
    let target = p.ask("Target (environment) name", Some("dev"))?;
    eprintln!(
        "Connection details for `{}` (secrets default to an env_var() reference; type a value to store it instead):",
        source.name
    );
    let conn = connection(&mut p, &profile, &fields)?;
    add_profile(&profiles_path, &profile, &target, &source.name, conn)?;
    printer.line(
        Tone::Good,
        "Saved",
        &format!("profile `{profile}` to {}", profiles_path.display()),
    );

    let dests: Vec<&IndexPlugin> = index
        .plugins
        .iter()
        .filter(|x| x.kind == PluginKind::Destination)
        .collect();
    let mut destinations = Vec::new();
    if !dests.is_empty() {
        eprintln!(
            "\nWhere should reports be delivered? Output always stays in target/ too. (Enter for none; several: 1,3)"
        );
        for d in p.pick("Destinations", &dests, true)? {
            let fields = install_and_describe(d, printer)?;
            let name = p.ask(
                &format!("Profile name for `{}`", d.name),
                Some(&format!("{}_{}", d.name, "out")),
            )?;
            eprintln!("Connection details for `{}`:", d.name);
            let conn = connection(&mut p, &name, &fields)?;
            add_profile(&profiles_path, &name, &target, &d.name, conn)?;
            printer.line(
                Tone::Good,
                "Saved",
                &format!("profile `{name}` to {}", profiles_path.display()),
            );
            destinations.push((d.name.clone(), name));
        }
    }

    let yes = p.ask("\nCreate a starter project now? (y/n)", Some("y"))?;
    if yes.eq_ignore_ascii_case("y") || yes.eq_ignore_ascii_case("yes") {
        let dir = PathBuf::from(p.ask("Project directory", Some("my_reports"))?);
        let s = Scaffold {
            name: project_name(&dir),
            profile,
            source: source.name.clone(),
            destinations,
        };
        let files = scaffold(&dir, &s)?;
        printer.line(
            Tone::Good,
            "Created",
            &format!("{} ({} files)", dir.display(), files.len()),
        );
        printer.line(Tone::Note, "Next", &format!("cd {} && dre run", dir.display()));
    } else {
        printer.line(Tone::Note, "Next", "run `dre new <dir>` when you want a project");
    }
    Ok(())
}
