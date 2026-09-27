//! YAML loading with file/line locations for diagnostics.

use std::path::{Path, PathBuf};

use serde_yaml_ng::Value;

use crate::diag::Diagnostics;

/// A parsed YAML file, kept with its source text so keys can be located for diagnostics.
#[derive(Debug, Clone)]
pub struct YamlFile {
    /// Path as shown in diagnostics (relative to the project root when inside it).
    pub display: PathBuf,
    pub text: String,
    pub value: Value,
}

impl YamlFile {
    /// Parse `path`, recording a diagnostic (with line) and returning `None` on failure.
    pub fn load(path: &Path, display: PathBuf, diags: &mut Diagnostics) -> Option<YamlFile> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) => {
                diags.error("io-error", Some(display), None, format!("cannot read file: {e}"));
                return None;
            }
        };
        Self::parse(text, display, diags)
    }

    pub fn parse(text: String, display: PathBuf, diags: &mut Diagnostics) -> Option<YamlFile> {
        match serde_yaml_ng::from_str::<Value>(&text) {
            Ok(mut value) => {
                if let Err(e) = value.apply_merge() {
                    diags.error(
                        "yaml-syntax",
                        Some(display),
                        None,
                        format!("invalid YAML merge key (`<<`): {e}"),
                    );
                    return None;
                }
                if !string_keys(&mut value) {
                    diags.error(
                        "yaml-syntax",
                        Some(display),
                        None,
                        "a map key is a list or a map; keys must be names",
                    );
                    return None;
                }
                Some(YamlFile { display, text, value })
            }
            Err(e) => {
                let line = e.location().map(|l| l.line());
                let msg = strip_location(&e.to_string());
                diags.error("yaml-syntax", Some(display), line, format!("invalid YAML: {msg}"));
                None
            }
        }
    }

    /// Best-effort line of `key:` in this file. With `after`, searches from that line on,
    /// which is enough to find nested keys under a located parent.
    pub fn line_of(&self, key: &str, after: Option<usize>) -> Option<usize> {
        let start = after.unwrap_or(1);
        self.text.lines().enumerate().skip(start - 1).find_map(|(i, l)| {
            let t = l.trim_start().trim_start_matches("- ").trim_start();
            let t = t
                .strip_prefix('"')
                .and_then(|t| t.strip_prefix(key).and_then(|r| r.strip_prefix('"')))
                .or_else(|| {
                    t.strip_prefix('\'')
                        .and_then(|t| t.strip_prefix(key).and_then(|r| r.strip_prefix('\'')))
                })
                .or_else(|| t.strip_prefix(key));
            match t {
                Some(rest) if rest.trim_start().starts_with(':') => Some(i + 1),
                _ => None,
            }
        })
    }

    /// Best-effort line of the first occurrence of `needle` anywhere in the file.
    pub fn line_containing(&self, needle: &str) -> Option<usize> {
        self.text.lines().position(|l| l.contains(needle)).map(|i| i + 1)
    }
}

/// Turn scalar mapping keys into strings. Unquoted `null:`, `true:` or `2024:` are YAML
/// null/bool/number keys, but every key DRE reads is a name: `null: "NULL"` means the
/// option `null`, not a missing key. False when a key is a list or a map.
fn string_keys(v: &mut Value) -> bool {
    match v {
        Value::Mapping(m) => {
            if m.keys().any(|k| !k.is_string()) {
                let old = std::mem::take(m);
                for (k, v) in old {
                    let k = match k {
                        Value::Null => Value::String("null".into()),
                        Value::Bool(b) => Value::String(b.to_string()),
                        Value::Number(n) => Value::String(n.to_string()),
                        Value::String(s) => Value::String(s),
                        _ => return false,
                    };
                    m.insert(k, v);
                }
            }
            m.values_mut().all(string_keys)
        }
        Value::Sequence(s) => s.iter_mut().all(string_keys),
        Value::Tagged(t) => string_keys(&mut t.value),
        _ => true,
    }
}

fn strip_location(msg: &str) -> String {
    // serde_yaml_ng appends " at line X column Y"; the line is reported separately.
    match msg.find(" at line ") {
        Some(i) => msg[..i].to_string(),
        None => msg.to_string(),
    }
}

/// Render a YAML scalar as a short string for messages.
pub fn scalar_str(v: &Value) -> Option<String> {
    match v {
        Value::String(s) => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        Value::Bool(b) => Some(b.to_string()),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> (Option<YamlFile>, Diagnostics) {
        let mut d = Diagnostics::default();
        (
            YamlFile::parse(text.to_string(), PathBuf::from("t.yml"), &mut d),
            d,
        )
    }

    #[test]
    fn merge_keys_are_applied_and_explicit_keys_win() {
        let (yf, _) = parse(
            "base: &b {type: postgres, host: h, database: shop}\n\
             other: &o {port: 5433, host: other}\n\
             prod:\n  <<: *b\n  database: shop_prod\n\
             both:\n  <<: [*b, *o]\n",
        );
        let v = yf.unwrap().value;
        assert_eq!(v["prod"]["type"], "postgres");
        assert_eq!(v["prod"]["database"], "shop_prod");
        assert!(v["prod"].get("<<").is_none());
        // With a list, earlier maps win over later ones.
        assert_eq!(v["both"]["host"], "h");
        assert_eq!(v["both"]["port"], 5433);
    }

    #[test]
    fn scalar_keys_become_names() {
        let (yf, _) = parse("output: {format: csv, null: NULL, true: 1, 2024: x}\n");
        let v = yf.unwrap().value;
        assert!(v["output"]["null"].is_null());
        assert_eq!(v["output"]["true"], 1);
        assert_eq!(v["output"]["2024"], "x");
        let (yf, _) = parse("output: {null: \"NULL\"}\n");
        assert_eq!(yf.unwrap().value["output"]["null"], "NULL");
    }

    #[test]
    fn complex_keys_are_an_error() {
        let (yf, d) = parse("? [a, b]\n: 1\n");
        assert!(yf.is_none());
        assert!(format!("{d:?}").contains("keys must be names"));
    }
}
