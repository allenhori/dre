//! Plugin options: the keys a report's config block for a plugin may hold (a format's `output:`
//! keys, a destination entry's keys), declared once by the plugin and checked by the SDK.
//!
//! A plugin lists its options as [`OptionField`]s. `describe` publishes them, and [`check`] tests
//! a config block against them: unknown keys, types, allowed values and bounds. Rules the
//! declaration can't express go in the plugin's own `validate`, which runs as well.

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::Kind;

/// What a value must be.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OptionType {
    String,
    /// A string of exactly one character (a delimiter, a pad).
    Char,
    Boolean,
    /// A whole number.
    Integer,
    Number,
    /// A string, or a list of strings (recipients, say).
    Strings,
    List,
    Map,
    /// Anything; the plugin's `validate` checks it.
    Any,
}

/// One option a plugin takes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct OptionField {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: OptionType,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub required: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default: Option<Value>,
    /// The only values a string option may take.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub choices: Vec<String>,
    /// Bounds for `integer` and `number`, inclusive.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

impl OptionField {
    pub fn new(name: &str, ty: OptionType, description: &str) -> Self {
        OptionField {
            name: name.into(),
            ty,
            description: description.into(),
            required: false,
            default: None,
            choices: Vec::new(),
            min: None,
            max: None,
        }
    }
    pub fn required(mut self) -> Self {
        self.required = true;
        self
    }
    pub fn default(mut self, v: impl Into<Value>) -> Self {
        self.default = Some(v.into());
        self
    }
    pub fn choices(mut self, c: &[&str]) -> Self {
        self.choices = c.iter().map(|s| s.to_string()).collect();
        self
    }
    pub fn range(mut self, min: Option<f64>, max: Option<f64>) -> Self {
        self.min = min;
        self.max = max;
        self
    }
}

/// Whether a string holds Jinja that core renders before the plugin sees it (destination
/// options). Its final value is unknown, so only its presence is checked.
pub fn is_template(s: &str) -> bool {
    s.contains("{{") || s.contains("{%")
}

/// A choice as it reads in a message: `` `all` ``, or `"\n"` for one with control characters.
fn show(s: &str) -> String {
    if s.chars().any(char::is_control) {
        Value::String(s.into()).to_string()
    } else {
        format!("`{s}`")
    }
}

fn one_of(choices: &[String]) -> String {
    let shown: Vec<String> = choices.iter().map(|c| show(c)).collect();
    match shown.as_slice() {
        [a, b] => format!("{a} or {b}"),
        _ => format!("one of {}", shown.join(", ")),
    }
}

fn number(n: f64) -> String {
    if n.fract() == 0.0 {
        format!("{}", n as i64)
    } else {
        n.to_string()
    }
}

/// Check `options` against the declared `fields` of the `kind` plugin `name`.
pub fn check(kind: Kind, name: &str, fields: &[OptionField], options: &Map<String, Value>) -> Vec<String> {
    let mut errs = Vec::new();
    for k in options.keys() {
        if fields.iter().any(|f| &f.name == k) {
            continue;
        }
        errs.push(if fields.is_empty() {
            format!("the `{name}` {kind} takes no options, but got `{k}`; check the key's spelling")
        } else {
            let known: Vec<&str> = fields.iter().map(|f| f.name.as_str()).collect();
            format!(
                "unknown option `{k}` for {kind} `{name}`; expected one of {}",
                known.join(", ")
            )
        });
    }
    for f in fields {
        let v = match options.get(&f.name) {
            None | Some(Value::Null) => {
                if f.required {
                    errs.push(format!("`{}` is required", f.name));
                }
                continue;
            }
            Some(v) => v,
        };
        if v.as_str().is_some_and(is_template) {
            continue;
        }
        if let Some(e) = check_value(f, v) {
            errs.push(format!("`{}` {e}", f.name));
        }
    }
    errs
}

fn check_value(f: &OptionField, v: &Value) -> Option<String> {
    let ok = match f.ty {
        OptionType::String => v.is_string(),
        OptionType::Char => v.as_str().is_some_and(|s| s.chars().count() == 1),
        OptionType::Boolean => v.is_boolean(),
        OptionType::Integer => v.is_i64() || v.is_u64(),
        OptionType::Number => v.is_number(),
        OptionType::Strings => v.is_string() || v.as_array().is_some_and(|a| a.iter().all(Value::is_string)),
        OptionType::List => v.is_array(),
        OptionType::Map => v.is_object(),
        OptionType::Any => true,
    };
    let bounded = matches!(f.ty, OptionType::Integer | OptionType::Number);
    let in_range = v
        .as_f64()
        .is_none_or(|n| f.min.is_none_or(|m| n >= m) && f.max.is_none_or(|m| n <= m));
    if !ok || (bounded && !in_range) {
        let what = match f.ty {
            OptionType::String => "a string",
            OptionType::Char => "a single character",
            OptionType::Boolean => "true or false",
            OptionType::Integer => "a whole number",
            OptionType::Number => "a number",
            OptionType::Strings => "a string or a list of strings",
            OptionType::List => "a list",
            OptionType::Map => "a map",
            OptionType::Any => "",
        };
        let bounds = match (bounded, f.min, f.max) {
            (true, Some(a), Some(b)) => format!(" from {} to {}", number(a), number(b)),
            (true, Some(a), None) => format!(" of at least {}", number(a)),
            (true, None, Some(b)) => format!(" of at most {}", number(b)),
            _ => String::new(),
        };
        return Some(format!("must be {what}{bounds}"));
    }
    if !f.choices.is_empty()
        && let Some(s) = v.as_str()
        && !f.choices.iter().any(|c| c == s)
    {
        return Some(format!("must be {}", one_of(&f.choices)));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fields() -> Vec<OptionField> {
        vec![
            OptionField::new("sep", OptionType::Char, ""),
            OptionField::new("mode", OptionType::String, "").choices(&["a", "b", "c"]),
            OptionField::new("eol", OptionType::String, "").choices(&["\n", "\r\n"]),
            OptionField::new("rows", OptionType::Integer, "").range(Some(1.0), Some(10.0)),
            OptionField::new("to", OptionType::Strings, ""),
            OptionField::new("cols", OptionType::List, "").required(),
        ]
    }

    fn errs(o: Value) -> Vec<String> {
        let Value::Object(o) = o else { panic!() };
        check(Kind::Format, "x", &fields(), &o)
    }

    #[test]
    fn accepts_valid_options() {
        assert!(
            errs(json!({"sep": "|", "mode": "b", "eol": "\n", "rows": 3, "to": ["a"], "cols": []}))
                .is_empty()
        );
    }

    #[test]
    fn reports_every_problem() {
        assert_eq!(
            errs(json!({"sep": "||", "mode": "z", "eol": "\r", "rows": 11, "to": [1], "extra": 1})),
            vec![
                "unknown option `extra` for format `x`; expected one of sep, mode, eol, rows, to, cols",
                "`sep` must be a single character",
                "`mode` must be one of `a`, `b`, `c`",
                r#"`eol` must be "\n" or "\r\n""#,
                "`rows` must be a whole number from 1 to 10",
                "`to` must be a string or a list of strings",
                "`cols` is required",
            ]
        );
    }

    #[test]
    fn templated_values_are_left_for_the_plugin() {
        assert!(errs(json!({"mode": "{{ var('m') }}", "cols": []})).is_empty());
    }

    #[test]
    fn a_plugin_without_options_refuses_any() {
        let Value::Object(o) = json!({"to": "x"}) else {
            panic!()
        };
        assert_eq!(
            check(Kind::Destination, "sftp", &[], &o),
            vec!["the `sftp` destination takes no options, but got `to`; check the key's spelling"]
        );
    }
}
