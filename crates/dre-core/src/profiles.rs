//! `profiles.yml`: connection definitions, kept outside the project.
//!
//! Two sections keep database connections apart from delivery targets:
//!
//! ```yaml
//! connections:
//!   warehouse:
//!     targets:
//!       dev: {type: duckdb, path: dev.duckdb}
//! destinations:
//!   client_sftp:
//!     targets:
//!       dev: {type: sftp, host: sftp.example.com}
//! ```
//!
//! A connection profile is referenced by `default_profile`/`profile:` (and a source's
//! `profile:`), a destination profile by `output.destination.profile`; each is looked up in its
//! own section only. Each profile lists one entry per target (environment). Each profile a run
//! uses picks its entry by `--target`, else `DRE_TARGET` (either sets every profile), else the
//! profile's own `target:`, else `dev`. A used profile without that entry is an error; a
//! destination entry `{deliver: false}` deliberately delivers nowhere.
//!
//! DRE 0.1 called the connections section `sources:`; it still loads in 0.2.x, with a warning.
//!
//! Location: `--profiles-dir` > `DRE_PROFILES_DIR` > `~/.dre`, one file, never merged. These are
//! read directly at startup, never through the `env_var()` Jinja function. `env_var()` calls
//! *inside* the file are rendered at run time, just before a connection config is handed to a
//! plugin.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_yaml_ng::Value;

use crate::diag::Diagnostics;
use crate::yaml::YamlFile;

pub const PROFILES_FILE: &str = "profiles.yml";

/// The built-in destination type: copying bytes to a local path needs no plugin (ADR 0003).
/// `profile: local` works without a profiles.yml entry; defining a `local` profile overrides it.
pub const LOCAL_TYPE: &str = "local";

/// What `profile: local` resolves to when profiles.yml doesn't define it, for every target.
pub static BUILTIN_LOCAL: std::sync::LazyLock<ProfileTarget> = std::sync::LazyLock::new(|| ProfileTarget {
    kind: LOCAL_TYPE.into(),
    fields: serde_json::Map::new(),
});

/// The section DRE 0.1 kept connections under; read with a warning in 0.2.x.
pub const OLD_CONNECTIONS_SECTION: &str = "sources";

/// The run's target (environment), and a profile's entry, when nothing chooses one.
pub const DEFAULT_TARGET: &str = "dev";
/// The environment variable choosing the run's target, below `--target`.
pub const TARGET_ENV: &str = "DRE_TARGET";

/// Which section of profiles.yml a profile lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Connection,
    Destination,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Connection => "connection",
            Role::Destination => "destination",
        }
    }
    /// The profiles.yml section key.
    pub fn section(self) -> &'static str {
        match self {
            Role::Connection => "connections",
            Role::Destination => "destinations",
        }
    }
}

/// Where the run's target came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum TargetSource {
    Flag,
    Env,
    #[default]
    Default,
}

impl std::fmt::Display for TargetSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            TargetSource::Flag => "--target",
            TargetSource::Env => TARGET_ENV,
            TargetSource::Default => "default",
        })
    }
}

/// The run's target: `--target`, else `DRE_TARGET`, else `dev`. It's `target.name` in templates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunTarget {
    pub name: String,
    pub from: TargetSource,
}

impl Default for RunTarget {
    fn default() -> Self {
        RunTarget {
            name: DEFAULT_TARGET.into(),
            from: TargetSource::Default,
        }
    }
}

impl RunTarget {
    /// `--target` (`flag`), else `DRE_TARGET`, else `dev`.
    pub fn resolve(flag: Option<&str>) -> RunTarget {
        if let Some(t) = flag.filter(|t| !t.is_empty()) {
            return RunTarget {
                name: t.to_string(),
                from: TargetSource::Flag,
            };
        }
        if let Some(t) = std::env::var(TARGET_ENV).ok().filter(|t| !t.is_empty()) {
            return RunTarget {
                name: t,
                from: TargetSource::Env,
            };
        }
        RunTarget::default()
    }

    /// Whether `--target` or `DRE_TARGET` chose it, which sets every profile's entry.
    pub fn chosen(&self) -> bool {
        self.from != TargetSource::Default
    }
}

/// What a profile uses for the run: its entry, or why it has none.
#[derive(Debug, Clone, Copy)]
pub enum Entry<'a> {
    Use(&'a ProfileTarget),
    /// `{deliver: false}`: a destination that deliberately delivers nowhere on this target.
    Nowhere,
    /// The profile has no entry for its target.
    Missing,
    /// No such profile.
    Unknown,
}

/// One environment of a profile: a plugin type and its connection fields.
#[derive(Debug, Clone, Serialize)]
pub struct ProfileTarget {
    #[serde(rename = "type")]
    pub kind: String,
    /// Every other field, passed through to the plugin unvalidated by core.
    #[serde(skip)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    /// Its own default entry (`target:`), below `--target` and `DRE_TARGET`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    /// The entries that connect or deliver.
    pub targets: BTreeMap<String, ProfileTarget>,
    /// The destination entries written `{deliver: false}`.
    #[serde(skip_serializing_if = "std::collections::BTreeSet::is_empty")]
    pub nowhere: std::collections::BTreeSet<String>,
}

impl Profile {
    /// Every entry's name, delivering or not.
    pub fn entry_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.targets.keys().chain(&self.nowhere).cloned().collect();
        names.sort();
        names
    }
}

#[derive(Debug, Clone, Default)]
pub struct Profiles {
    /// Where DRE looked for the file.
    pub path: PathBuf,
    /// Why that directory: `--profiles-dir`, `DRE_PROFILES_DIR`, `the project directory` or `~/.dre`.
    pub found_by: &'static str,
    /// `None` when the file doesn't exist; loading problems are reported when it's needed.
    pub file: Option<YamlFile>,
    pub connections: BTreeMap<String, Profile>,
    pub destinations: BTreeMap<String, Profile>,
    /// The section connections were read from: `connections`, or 0.1's `sources`.
    connections_section: &'static str,
    /// The run's target, which picks each profile's entry (see [`Profiles::target_of`]).
    pub run: RunTarget,
}

/// Resolve the profiles directory: `--profiles-dir` > `DRE_PROFILES_DIR` > `~/.dre`.
/// `dre init` writes here; projects look in their own directory first (see [`locate`]).
pub fn profiles_dir(cli: Option<&Path>) -> PathBuf {
    locate(cli, None).0
}

/// Find the profiles directory for a project, with the reason it was chosen:
/// `--profiles-dir` > `DRE_PROFILES_DIR` > the project directory (when it holds a
/// profiles.yml) > `~/.dre`. With the default `--project-dir .` this is dbt's order.
pub fn locate(cli: Option<&Path>, project: Option<&Path>) -> (PathBuf, &'static str) {
    if let Some(p) = cli {
        return (p.to_path_buf(), "--profiles-dir");
    }
    if let Some(p) = std::env::var_os("DRE_PROFILES_DIR").filter(|p| !p.is_empty()) {
        return (PathBuf::from(p), "DRE_PROFILES_DIR");
    }
    if let Some(p) = project.filter(|p| p.join(PROFILES_FILE).is_file()) {
        return (p.to_path_buf(), "the project directory");
    }
    (crate::dre_home(), "~/.dre")
}

impl Profiles {
    pub fn load(dir: &Path, found_by: &'static str, diags: &mut Diagnostics) -> Profiles {
        let path = crate::slash(&dir.join(PROFILES_FILE));
        let mut out = Profiles {
            path: path.clone(),
            found_by,
            ..Default::default()
        };
        if !path.is_file() {
            return out;
        }
        let Some(yf) = YamlFile::load(&path, path.clone(), diags) else {
            // Parsed badly: treat as present-but-empty so references don't pile up extra errors.
            out.file = Some(YamlFile {
                display: path,
                text: String::new(),
                value: Value::Null,
            });
            return out;
        };
        let file = Some(path.clone());
        match &yf.value {
            Value::Mapping(m) => {
                let both = m.contains_key("connections") && m.contains_key(OLD_CONNECTIONS_SECTION);
                for (k, v) in m {
                    let key = k.as_str().unwrap_or_default();
                    let role = match key {
                        "connections" => Role::Connection,
                        OLD_CONNECTIONS_SECTION if both => {
                            diags.error(
                                "invalid-profiles",
                                file.clone(),
                                yf.line_of(key, None),
                                "profiles.yml has both `connections:` and `sources:`; `sources:` is the old name of `connections:`, so move its profiles under `connections:`",
                            );
                            continue;
                        }
                        OLD_CONNECTIONS_SECTION => {
                            diags.warning(
                                "profiles-sources-renamed",
                                file.clone(),
                                yf.line_of(key, None),
                                "`sources:` in profiles.yml is now `connections:` (DRE 0.2); rename it. `sources:` still works in 0.2.x",
                            );
                            out.connections_section = OLD_CONNECTIONS_SECTION;
                            Role::Connection
                        }
                        "destinations" => Role::Destination,
                        _ => {
                            diags.error(
                                "invalid-profiles",
                                file.clone(),
                                yf.line_of(key, None),
                                format!(
                                    "unknown profiles.yml section `{key}`; profiles go under `connections:` or `destinations:`"
                                ),
                            );
                            continue;
                        }
                    };
                    let section_line = yf.line_of(key, None);
                    let parsed = match v {
                        Value::Mapping(ps) => ps
                            .iter()
                            .filter_map(|(k, v)| {
                                let name = k.as_str()?;
                                parse_profile(role, name, v, &yf, section_line, diags)
                                    .map(|p| (name.to_string(), p))
                            })
                            .collect(),
                        Value::Null => BTreeMap::new(),
                        _ => {
                            diags.error(
                                "invalid-profiles",
                                file.clone(),
                                section_line,
                                format!("`{key}` must be a map of profile names"),
                            );
                            BTreeMap::new()
                        }
                    };
                    match role {
                        Role::Connection => out.connections = parsed,
                        Role::Destination => out.destinations = parsed,
                    }
                }
            }
            Value::Null => {}
            _ => diags.error(
                "invalid-profiles",
                file,
                None,
                "profiles.yml must be a map with `connections:` and/or `destinations:`",
            ),
        }
        out.file = Some(yf);
        out
    }

    pub fn exists(&self) -> bool {
        self.file.is_some()
    }

    fn section(&self, role: Role) -> &BTreeMap<String, Profile> {
        match role {
            Role::Connection => &self.connections,
            Role::Destination => &self.destinations,
        }
    }

    /// The section key a role's profiles were read from (`sources` for an 0.1 file).
    pub fn section_key(&self, role: Role) -> &'static str {
        match role {
            Role::Connection if self.connections_section == OLD_CONNECTIONS_SECTION => {
                OLD_CONNECTIONS_SECTION
            }
            r => r.section(),
        }
    }

    pub fn get(&self, role: Role, name: &str) -> Option<&Profile> {
        self.section(role).get(name)
    }

    /// Whether a (possibly partially broken) profile of this name is declared in its section.
    pub fn declares(&self, role: Role, name: &str) -> bool {
        self.file
            .as_ref()
            .and_then(|f| f.value.get(self.section_key(role)))
            .is_some_and(|s| s.get(name).is_some())
    }

    /// Whether `name` means the built-in local destination (no `local` profile is defined).
    pub fn is_builtin_local(&self, name: &str) -> bool {
        name == LOCAL_TYPE && !self.destinations.contains_key(name)
    }

    /// Best-effort line of a profile's name in the file.
    pub fn line_of(&self, role: Role, name: &str) -> Option<usize> {
        let f = self.file.as_ref()?;
        f.line_of(name, f.line_of(self.section_key(role), None))
    }

    /// The entry a profile uses in this run: the run's target when `--target` or `DRE_TARGET`
    /// chose it, else the profile's own `target:`, else `dev`.
    pub fn target_of(&self, role: Role, profile: &str) -> String {
        if self.run.chosen() {
            return self.run.name.clone();
        }
        self.get(role, profile)
            .and_then(|p| p.target.clone())
            .unwrap_or_else(|| self.run.name.clone())
    }

    /// What a profile uses in this run. `profile: local` without a defined `local` profile is
    /// the built-in local destination, for every target.
    pub fn entry(&self, role: Role, profile: &str) -> Entry<'_> {
        if role == Role::Destination && self.is_builtin_local(profile) {
            return Entry::Use(&BUILTIN_LOCAL);
        }
        let Some(p) = self.get(role, profile) else {
            return Entry::Unknown;
        };
        let t = self.target_of(role, profile);
        match p.targets.get(&t) {
            Some(o) => Entry::Use(o),
            None if p.nowhere.contains(&t) => Entry::Nowhere,
            None => Entry::Missing,
        }
    }

    /// A profile's settings for this run, when it has an entry that connects or delivers.
    pub fn target(&self, role: Role, profile: &str) -> Option<&ProfileTarget> {
        match self.entry(role, profile) {
            Entry::Use(o) => Some(o),
            _ => None,
        }
    }

    /// Why a used profile has no entry for this run, with the way to fix it.
    pub fn missing_entry(&self, role: Role, profile: &str) -> String {
        let t = self.target_of(role, profile);
        let has = self
            .get(role, profile)
            .map(|p| p.entry_names().join(", "))
            .unwrap_or_default();
        let why = match self.run.from {
            TargetSource::Default if self.get(role, profile).is_some_and(|p| p.target.is_some()) => {
                "its `target:`".to_string()
            }
            TargetSource::Default => "the default".to_string(),
            from => from.to_string(),
        };
        let mut msg = format!(
            "{} `{profile}` has no `{t}` entry (it has: {has}); `{t}` comes from {why}",
            role.as_str()
        );
        if role == Role::Destination {
            msg.push_str(&format!(
                ". To deliver nowhere on `{t}`, add `{t}: {{deliver: false}}` to its `targets:`"
            ));
        }
        msg
    }
}

fn parse_profile(
    role: Role,
    name: &str,
    v: &Value,
    yf: &YamlFile,
    section_line: Option<usize>,
    diags: &mut Diagnostics,
) -> Option<Profile> {
    let file = Some(yf.display.clone());
    let line = yf.line_of(name, section_line);
    let what = format!("{} profile `{name}`", role.as_str());
    let Some(m) = v.as_mapping() else {
        diags.error(
            "invalid-profile",
            file,
            line,
            format!("{what} must be a map with `targets`"),
        );
        return None;
    };
    let own_target = match m.get("target") {
        None => None,
        Some(Value::String(t)) if !t.trim().is_empty() => Some(t.clone()),
        Some(_) => {
            diags.error(
                "invalid-profile",
                file.clone(),
                yf.line_of("target", line),
                format!("{what}: `target` must be the name of one of its `targets`"),
            );
            return None;
        }
    };
    let Some(targets) = m.get("targets").and_then(Value::as_mapping) else {
        let hint = if m.contains_key("outputs") {
            " (`outputs:` is now `targets:`)"
        } else {
            ""
        };
        diags.error(
            "invalid-profile",
            file,
            line,
            format!("{what} needs a map of named `targets`{hint}"),
        );
        return None;
    };
    let mut parsed = BTreeMap::new();
    let mut nowhere = std::collections::BTreeSet::new();
    let mut ok = true;
    for (k, o) in targets {
        let Some(tname) = k.as_str() else { continue };
        if let Some(d) = o.get("deliver") {
            let problem = if role == Role::Connection {
                Some("`deliver: false` is only for destinations; a connection entry needs a `type`")
            } else if d != &Value::Bool(false) {
                Some("`deliver` can only be `false` (an entry that delivers just has a `type`)")
            } else if o.as_mapping().is_some_and(|m| m.len() > 1) {
                Some("`deliver: false` takes no other settings: the entry delivers nowhere")
            } else {
                None
            };
            match problem {
                Some(p) => {
                    diags.error(
                        "invalid-profile",
                        file.clone(),
                        yf.line_of(tname, line),
                        format!("target `{tname}` of {what}: {p}"),
                    );
                    ok = false;
                }
                None => {
                    nowhere.insert(tname.to_string());
                }
            }
            continue;
        }
        let kind = o.get("type").and_then(Value::as_str);
        let Some(kind) = kind else {
            diags.error(
                "invalid-profile",
                file.clone(),
                yf.line_of(tname, line),
                format!("target `{tname}` of {what} has no `type`"),
            );
            ok = false;
            continue;
        };
        let fields = match serde_json::to_value(o) {
            Ok(serde_json::Value::Object(mut f)) => {
                f.remove("type");
                f
            }
            _ => serde_json::Map::new(),
        };
        parsed.insert(
            tname.to_string(),
            ProfileTarget {
                kind: kind.to_string(),
                fields,
            },
        );
    }
    if let Some(t) = &own_target
        && ok
        && !parsed.contains_key(t)
        && !nowhere.contains(t)
    {
        let mut has: Vec<&String> = parsed.keys().chain(&nowhere).collect();
        has.sort();
        diags.error(
            "invalid-profile",
            file.clone(),
            yf.line_of("target", line),
            format!(
                "{what}: `target: {t}` isn't one of its targets ({})",
                has.into_iter().cloned().collect::<Vec<_>>().join(", ")
            ),
        );
        return None;
    }
    ok.then_some(Profile {
        target: own_target,
        targets: parsed,
        nowhere,
    })
}
