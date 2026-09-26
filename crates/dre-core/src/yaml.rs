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
            Ok(value) => Some(YamlFile { display, text, value }),
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
