//! `profiles.yml`: connection definitions, kept outside the project.
//!
//! Two sections keep database connections apart from delivery targets:
//!
//! ```yaml
//! sources:
//!   warehouse:
//!     target: dev
//!     targets:
//!       dev: {type: duckdb, path: dev.duckdb}
//! destinations:
//!   client_sftp:
//!     target: dev
//!     targets:
//!       dev: {type: sftp, host: sftp.example.com}
//! ```
//!
//! A source profile is referenced by `default_profile`/`profile:`, a destination profile by
//! `output.destination.profile`; each is looked up in its own section only.
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

/// Which section of profiles.yml a profile lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    Source,
    Destination,
}

impl Role {
    pub fn as_str(self) -> &'static str {
        match self {
            Role::Source => "source",
            Role::Destination => "destination",
        }
    }
    /// The profiles.yml section key.
    pub fn section(self) -> &'static str {
        match self {
            Role::Source => "sources",
            Role::Destination => "destinations",
        }
    }
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
    /// The default target.
    pub target: String,
    pub targets: BTreeMap<String, ProfileTarget>,
}

#[derive(Debug, Clone, Default)]
pub struct Profiles {
    /// Where DRE looked for the file.
    pub path: PathBuf,
    /// `None` when the file doesn't exist; loading problems are reported when it's needed.
    pub file: Option<YamlFile>,
    pub sources: BTreeMap<String, Profile>,
    pub destinations: BTreeMap<String, Profile>,
}

/// Resolve the profiles directory: `--profiles-dir` > `DRE_PROFILES_DIR` > `~/.dre`.
pub fn profiles_dir(cli: Option<&Path>) -> PathBuf {
    if let Some(p) = cli {
        return p.to_path_buf();
    }
    if let Some(p) = std::env::var_os("DRE_PROFILES_DIR").filter(|p| !p.is_empty()) {
        return PathBuf::from(p);
    }
    crate::dre_home()
}

impl Profiles {
    pub fn load(dir: &Path, diags: &mut Diagnostics) -> Profiles {
        let path = crate::slash(&dir.join(PROFILES_FILE));
        let mut out = Profiles {
            path: path.clone(),
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
                for (k, v) in m {
                    let key = k.as_str().unwrap_or_default();
                    let role = match key {
                        "sources" => Role::Source,
                        "destinations" => Role::Destination,
                        _ => {
                            diags.error(
                                "invalid-profiles",
                                file.clone(),
                                yf.line_of(key, None),
                                format!(
                                    "unknown profiles.yml section `{key}`; profiles go under `sources:` or `destinations:`"
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
                        Role::Source => out.sources = parsed,
                        Role::Destination => out.destinations = parsed,
                    }
                }
            }
            Value::Null => {}
            _ => diags.error(
                "invalid-profiles",
                file,
                None,
                "profiles.yml must be a map with `sources:` and/or `destinations:`",
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
            Role::Source => &self.sources,
            Role::Destination => &self.destinations,
        }
    }

    pub fn get(&self, role: Role, name: &str) -> Option<&Profile> {
        self.section(role).get(name)
    }

    /// Whether a (possibly partially broken) profile of this name is declared in its section.
    pub fn declares(&self, role: Role, name: &str) -> bool {
        self.file
            .as_ref()
            .and_then(|f| f.value.get(role.section()))
            .is_some_and(|s| s.get(name).is_some())
    }

    /// Whether `name` means the built-in local destination (no `local` profile is defined).
    pub fn is_builtin_local(&self, name: &str) -> bool {
        name == LOCAL_TYPE && !self.destinations.contains_key(name)
    }

    /// Best-effort line of a profile's name in the file.
    pub fn line_of(&self, role: Role, name: &str) -> Option<usize> {
        let f = self.file.as_ref()?;
        f.line_of(name, f.line_of(role.section(), None))
    }

    /// The target a profile uses for `target` (or its own default target).
    pub fn target(&self, role: Role, profile: &str, target: Option<&str>) -> Option<(&str, &ProfileTarget)> {
        let p = self.get(role, profile)?;
        let t = target.unwrap_or(&p.target);
        p.targets.get_key_value(t).map(|(k, o)| (k.as_str(), o))
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
            format!("{what} must be a map with `target` and `targets`"),
        );
        return None;
    };
    let target = m.get("target").and_then(Value::as_str);
    let targets = m.get("targets").and_then(Value::as_mapping);
    let (Some(target), Some(targets)) = (target, targets) else {
        let hint = if m.contains_key("outputs") {
            " (`outputs:` is now `targets:`)"
        } else {
            ""
        };
        diags.error(
            "invalid-profile",
            file,
            line,
            format!("{what} needs a `target` and a map of named `targets`{hint}"),
        );
        return None;
    };
    let mut parsed = BTreeMap::new();
    let mut ok = true;
    for (k, o) in targets {
        let Some(tname) = k.as_str() else { continue };
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
    if !parsed.contains_key(target) && ok {
        diags.error(
            "invalid-profile",
            file,
            yf.line_of("target", line),
            format!(
                "{what} has target `{target}`, which isn't one of its targets ({})",
                parsed
                    .keys()
                    .map(|k| format!("`{k}`"))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        );
        return None;
    }
    ok.then_some(Profile {
        target: target.to_string(),
        targets: parsed,
    })
}
