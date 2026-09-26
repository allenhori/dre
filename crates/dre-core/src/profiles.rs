//! `profiles.yml`: connection definitions, kept outside the project (dbt's shape exactly).
//!
//! Location: `--profiles-dir` > `DRE_PROFILES_DIR` > `~/.dre`. These are read directly at startup,
//! never through the `env_var()` Jinja function. `env_var()` calls *inside* the file are rendered
//! at run time, just before a connection config is handed to a plugin.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_yaml_ng::Value;

use crate::diag::Diagnostics;
use crate::yaml::YamlFile;

pub const PROFILES_FILE: &str = "profiles.yml";

/// The built-in destination type: copying bytes to a local path needs no plugin (ADR 0003).
pub const LOCAL_TYPE: &str = "local";

#[derive(Debug, Clone, Serialize)]
pub struct ProfileOutput {
    #[serde(rename = "type")]
    pub kind: String,
    /// Every other field, passed through to the plugin unvalidated by core.
    #[serde(skip)]
    pub fields: serde_json::Map<String, serde_json::Value>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Profile {
    pub target: String,
    pub outputs: BTreeMap<String, ProfileOutput>,
}

#[derive(Debug, Clone, Default)]
pub struct Profiles {
    /// Where DRE looked for the file.
    pub path: PathBuf,
    /// `None` when the file doesn't exist; loading problems are reported when it's needed.
    pub file: Option<YamlFile>,
    pub profiles: BTreeMap<String, Profile>,
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
                    let Some(name) = k.as_str() else { continue };
                    if let Some(p) = parse_profile(name, v, &yf, diags) {
                        out.profiles.insert(name.to_string(), p);
                    }
                }
            }
            Value::Null => {}
            _ => diags.error(
                "invalid-profiles",
                file,
                None,
                "profiles.yml must be a map of profile names",
            ),
        }
        out.file = Some(yf);
        out
    }

    pub fn exists(&self) -> bool {
        self.file.is_some()
    }

    pub fn get(&self, name: &str) -> Option<&Profile> {
        self.profiles.get(name)
    }

    /// Whether a (possibly partially broken) profile of this name is declared at all.
    pub fn declares(&self, name: &str) -> bool {
        self.file.as_ref().is_some_and(|f| f.value.get(name).is_some())
    }

    /// The output a profile uses for `target` (or its own default target).
    pub fn output(&self, profile: &str, target: Option<&str>) -> Option<(&str, &ProfileOutput)> {
        let p = self.get(profile)?;
        let t = target.unwrap_or(&p.target);
        p.outputs.get_key_value(t).map(|(k, o)| (k.as_str(), o))
    }
}

fn parse_profile(name: &str, v: &Value, yf: &YamlFile, diags: &mut Diagnostics) -> Option<Profile> {
    let file = Some(yf.display.clone());
    let line = yf.line_of(name, None);
    let Some(m) = v.as_mapping() else {
        diags.error(
            "invalid-profile",
            file,
            line,
            format!("profile `{name}` must be a map with `target` and `outputs`"),
        );
        return None;
    };
    let target = m.get("target").and_then(Value::as_str);
    let outputs = m.get("outputs").and_then(Value::as_mapping);
    let (Some(target), Some(outputs)) = (target, outputs) else {
        diags.error(
            "invalid-profile",
            file,
            line,
            format!("profile `{name}` needs a `target` and a map of named `outputs`"),
        );
        return None;
    };
    let mut parsed = BTreeMap::new();
    let mut ok = true;
    for (k, o) in outputs {
        let Some(oname) = k.as_str() else { continue };
        let kind = o.get("type").and_then(Value::as_str);
        let Some(kind) = kind else {
            diags.error(
                "invalid-profile",
                file.clone(),
                yf.line_of(oname, line),
                format!("output `{oname}` of profile `{name}` has no `type`"),
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
            oname.to_string(),
            ProfileOutput {
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
                "profile `{name}` has target `{target}`, which isn't one of its outputs ({})",
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
        outputs: parsed,
    })
}
