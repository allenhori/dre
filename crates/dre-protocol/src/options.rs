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

/// What an Excel number format code displays, from [`check_num_format`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NumFormatClass {
    /// Digits, percentages, currency, `General`: for number columns.
    Number,
    /// Dates, times, elapsed time: for date, timestamp and time columns.
    DateTime,
    /// Only `@` (and literals): shows any value as text.
    Text,
}

impl NumFormatClass {
    pub fn describe(self) -> &'static str {
        match self {
            NumFormatClass::Number => "a number format",
            NumFormatClass::DateTime => "a date/time format",
            NumFormatClass::Text => "a text format",
        }
    }
}

/// Characters Excel shows as themselves in a format code without quoting.
const LITERALS: &str = "$-+/():!^&'~{}<>= ";

/// A light syntax check of an Excel number format code (the text of Excel's Format Cells →
/// Custom dialog), and what it displays. Not a full grammar: conditions, colours and locale tags
/// inside `[]` are accepted without being interpreted.
pub fn check_num_format(code: &str) -> Result<NumFormatClass, String> {
    if code.trim().is_empty() {
        return Err("is empty".into());
    }
    let mut sections = vec![String::new()];
    let mut chars = code.chars().peekable();
    // Per section: the tokens left once quoted text, escapes, fills and brackets are removed.
    let mut elapsed = vec![false];
    while let Some(c) = chars.next() {
        match c {
            '"' => loop {
                match chars.next() {
                    Some('"') => break,
                    Some(_) => {}
                    None => return Err("has an unclosed `\"` quote".into()),
                }
            },
            '[' => {
                let mut inner = String::new();
                loop {
                    match chars.next() {
                        Some(']') => break,
                        Some('[') => return Err("has a `[` inside `[ ]`".into()),
                        Some(ch) => inner.push(ch),
                        None => return Err("has an unclosed `[` bracket".into()),
                    }
                }
                let lower = inner.to_ascii_lowercase();
                if !lower.is_empty()
                    && ["h", "m", "s"]
                        .iter()
                        .any(|u| lower.chars().all(|ch| ch.to_string() == *u))
                {
                    *elapsed.last_mut().unwrap() = true;
                }
            }
            ']' => return Err("has a `]` without a matching `[`".into()),
            '\\' | '_' | '*' => {
                if chars.next().is_none() {
                    return Err(format!("ends with `{c}`, which needs a character after it"));
                }
            }
            ';' => {
                sections.push(String::new());
                elapsed.push(false);
            }
            _ => sections.last_mut().unwrap().push(c),
        }
    }
    if sections.len() > 4 {
        return Err(format!(
            "has {} `;` sections; Excel allows at most four (positive;negative;zero;text)",
            sections.len()
        ));
    }
    let mut class = None;
    for (s, elapsed) in sections.iter().zip(elapsed) {
        let general = s.to_ascii_lowercase().contains("general");
        let s = remove_general(s);
        let mut date = elapsed;
        let mut number = general;
        let upper = s.to_ascii_uppercase();
        let mut rest = upper.as_str();
        while let Some(c) = rest.chars().next() {
            if rest.starts_with("AM/PM") {
                date = true;
                rest = &rest[5..];
                continue;
            }
            if rest.starts_with("A/P") {
                date = true;
                rest = &rest[3..];
                continue;
            }
            match c {
                '0' | '#' | '?' | '%' | '.' | ',' => number = true,
                '1'..='9' => {}
                'E' if rest[1..].starts_with(['+', '-']) => {
                    number = true;
                    rest = &rest[2..];
                    continue;
                }
                'Y' | 'M' | 'D' | 'H' | 'S' | 'E' | 'B' | 'G' => date = true,
                '@' => {}
                c if LITERALS.contains(c) => {}
                c => {
                    return Err(format!(
                        "has `{c}`, which isn't part of an Excel number format; put literal text in double quotes"
                    ));
                }
            }
            rest = &rest[c.len_utf8()..];
        }
        let this = if date {
            Some(NumFormatClass::DateTime)
        } else if number {
            Some(NumFormatClass::Number)
        } else {
            None
        };
        class = class.or(this);
    }
    Ok(class.unwrap_or(NumFormatClass::Text))
}

/// A section with every `General` (any case) taken out.
fn remove_general(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    if !lower.contains("general") {
        return s.to_string();
    }
    let mut out = String::new();
    let mut i = 0;
    while i < s.len() {
        if lower[i..].starts_with("general") {
            i += "general".len();
        } else {
            let c = s[i..].chars().next().unwrap();
            out.push(c);
            i += c.len_utf8();
        }
    }
    out
}

/// The keys a column may set in a `columns:` map.
pub const COLUMN_OPTION_KEYS: &[&str] = &["format", "formula", "total"];

/// The functions a `total:` can name, and the Excel function each writes.
pub const TOTAL_FUNCTIONS: &[(&str, &str)] = &[
    ("sum", "SUM"),
    ("average", "AVERAGE"),
    ("count", "COUNTA"),
    ("min", "MIN"),
    ("max", "MAX"),
];

/// A piece of a formula from YAML.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormulaPart {
    /// Text written as it is.
    Text(String),
    /// `{name}`: that column's cell on the same row.
    Cell(String),
    /// `{name:*}`: that column's data range on the sheet.
    Column(String),
}

/// Split a formula into text and `{name}` / `{name:*}` references. `{{` and `}}` are literal
/// braces. The formula must start with `=`.
pub fn parse_formula(f: &str) -> Result<Vec<FormulaPart>, String> {
    if !f.starts_with('=') {
        return Err("must start with `=`".into());
    }
    let mut parts = Vec::new();
    let mut text = String::new();
    let mut chars = f.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '{' if chars.peek() == Some(&'{') => {
                chars.next();
                text.push('{');
            }
            '}' if chars.peek() == Some(&'}') => {
                chars.next();
                text.push('}');
            }
            '}' => return Err("has a `}` without a matching `{` (write `}}` for a literal brace)".into()),
            '{' => {
                let mut name = String::new();
                loop {
                    match chars.next() {
                        Some('}') => break,
                        Some('{') => return Err("has a `{` inside `{ }`".into()),
                        Some(ch) => name.push(ch),
                        None => return Err("has an unclosed `{`".into()),
                    }
                }
                if !text.is_empty() {
                    parts.push(FormulaPart::Text(std::mem::take(&mut text)));
                }
                let part = match name.strip_suffix(":*") {
                    Some(col) => FormulaPart::Column(col.trim().to_string()),
                    None => FormulaPart::Cell(name.trim().to_string()),
                };
                if matches!(&part, FormulaPart::Cell(n) | FormulaPart::Column(n) if n.is_empty()) {
                    return Err("has an empty `{}` reference".into());
                }
                parts.push(part);
            }
            _ => text.push(c),
        }
    }
    if !text.is_empty() {
        parts.push(FormulaPart::Text(text));
    }
    Ok(parts)
}

/// A row formula: `{name}` references only, since a row can't see the whole column's extent.
pub fn check_row_formula(f: &str) -> Result<Vec<FormulaPart>, String> {
    let parts = parse_formula(f)?;
    if let Some(FormulaPart::Column(n)) = parts.iter().find(|p| matches!(p, FormulaPart::Column(_))) {
        return Err(format!(
            "uses `{{{n}:*}}`, a whole column, which only a `total` can use; a row formula refers to cells on its own row, like `{{{n}}}`"
        ));
    }
    Ok(parts)
}

/// A `total:`: a function name, or a formula with `{name:*}` references only.
pub fn check_total(t: &str) -> Result<(), String> {
    if t.starts_with('=') {
        let parts = parse_formula(t)?;
        if let Some(FormulaPart::Cell(n)) = parts.iter().find(|p| matches!(p, FormulaPart::Cell(_))) {
            return Err(format!(
                "uses `{{{n}}}`, a cell on the same row, which a totals row doesn't have; use `{{{n}:*}}` for the whole column"
            ));
        }
        return Ok(());
    }
    if TOTAL_FUNCTIONS.iter().any(|(k, _)| *k == t) {
        return Ok(());
    }
    Err(format!(
        "must be one of {} or a formula starting with `=`",
        TOTAL_FUNCTIONS
            .iter()
            .map(|(k, _)| format!("`{k}`"))
            .collect::<Vec<_>>()
            .join(", ")
    ))
}

/// Parse a `columns:` map (column name → column options), collecting every problem as a
/// sentence naming the column. Used by core for query entries and by the xlsx plugin for its
/// output-level `columns` option.
pub fn parse_columns(
    v: &Value,
) -> (
    std::collections::BTreeMap<String, crate::msg::ColumnOptions>,
    Vec<String>,
) {
    let mut out = std::collections::BTreeMap::new();
    let mut errs = Vec::new();
    let Some(m) = v.as_object() else {
        errs.push("`columns` must be a map from column name to options like `{format: \"#,##0.00\"}`".into());
        return (out, errs);
    };
    for (name, opts) in m {
        let Some(o) = opts.as_object() else {
            errs.push(format!(
                "column `{name}` must be a map of options like `{{format: \"#,##0.00\"}}`"
            ));
            continue;
        };
        let mut col = crate::msg::ColumnOptions::default();
        for (k, val) in o {
            match k.as_str() {
                "format" => match val.as_str() {
                    Some(code) => match check_num_format(code) {
                        Ok(_) => col.format = Some(code.to_string()),
                        Err(e) => errs.push(format!("column `{name}`: format `{code}` {e}")),
                    },
                    None => errs.push(format!("column `{name}`: `format` must be a string")),
                },
                "formula" => match val.as_str() {
                    Some(f) => match check_row_formula(f) {
                        Ok(_) => col.formula = Some(f.to_string()),
                        Err(e) => errs.push(format!("column `{name}`: formula `{f}` {e}")),
                    },
                    None => errs.push(format!("column `{name}`: `formula` must be a string")),
                },
                "total" => match val.as_str() {
                    Some(t) => match check_total(t) {
                        Ok(()) => col.total = Some(t.to_string()),
                        Err(e) => errs.push(format!("column `{name}`: total `{t}` {e}")),
                    },
                    None => errs.push(format!("column `{name}`: `total` must be a string")),
                },
                _ => errs.push(format!(
                    "column `{name}`: unknown key `{k}`; expected {}",
                    COLUMN_OPTION_KEYS
                        .iter()
                        .map(|k| format!("`{k}`"))
                        .collect::<Vec<_>>()
                        .join(", ")
                )),
            }
        }
        out.insert(name.clone(), col);
    }
    (out, errs)
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

    #[test]
    fn number_format_codes() {
        use NumFormatClass::*;
        let ok = [
            ("General", Number),
            ("0", Number),
            ("#,##0.00", Number),
            ("0.0%", Number),
            ("0.00E+00", Number),
            ("# ?/?", Number),
            ("[$€-x-euro2] #,##0.00", Number),
            ("[$$-409]#,##0.00", Number),
            ("#,##0.00;[Red](#,##0.00)", Number),
            ("#,##0;(#,##0);\"-\";@", Number),
            ("_(* #,##0.00_);_(* (#,##0.00);_(* \"-\"??_);_(@_)", Number),
            ("[>=1000]#,##0;0", Number),
            ("\\€#,##0", Number),
            ("yyyy-mm-dd", DateTime),
            ("dd/mm/yyyy", DateTime),
            ("mmm yyyy", DateTime),
            ("yyyy-mm-dd hh:mm:ss", DateTime),
            ("h:mm AM/PM", DateTime),
            ("h:mm A/P", DateTime),
            ("[h]:mm:ss", DateTime),
            ("[mm]", DateTime),
            ("hh:mm:ss.000", DateTime),
            ("@", Text),
            ("\"Total: \"@", Text),
        ];
        for (code, class) in ok {
            assert_eq!(check_num_format(code), Ok(class), "{code}");
        }
        let bad = [
            ("", "is empty"),
            (
                "0;0;0;@;0",
                "has 5 `;` sections; Excel allows at most four (positive;negative;zero;text)",
            ),
            ("\"abc", "has an unclosed `\"` quote"),
            ("[Red#,##0", "has an unclosed `[` bracket"),
            ("#,##0]", "has a `]` without a matching `[`"),
            ("0\\", "ends with `\\`, which needs a character after it"),
            (
                "0 units",
                "has `U`, which isn't part of an Excel number format; put literal text in double quotes",
            ),
        ];
        for (code, err) in bad {
            assert_eq!(check_num_format(code), Err(err.to_string()), "{code}");
        }
    }

    #[test]
    fn column_maps() {
        let (cols, errs) = parse_columns(
            &json!({"a": {"format": "0.0%"}, "b": {"fromat": "0"}, "c": "0", "d": {"format": "\"x"}}),
        );
        assert_eq!(cols["a"].format.as_deref(), Some("0.0%"));
        assert_eq!(
            errs,
            vec![
                "column `b`: unknown key `fromat`; expected `format`, `formula`, `total`",
                "column `c` must be a map of options like `{format: \"#,##0.00\"}`",
                "column `d`: format `\"x` has an unclosed `\"` quote",
            ]
        );
    }

    #[test]
    fn formulas() {
        use FormulaPart::*;
        assert_eq!(
            parse_formula("={qty}*{ price }+{{1}}").unwrap(),
            vec![
                Text("=".into()),
                Cell("qty".into()),
                Text("*".into()),
                Cell("price".into()),
                Text("+{1}".into())
            ]
        );
        assert_eq!(
            parse_formula("=SUM({amount:*})").unwrap(),
            vec![Text("=SUM(".into()), Column("amount".into()), Text(")".into())]
        );
        for (f, err) in [
            ("{a}*2", "must start with `=`"),
            ("={a", "has an unclosed `{`"),
            (
                "=a}",
                "has a `}` without a matching `{` (write `}}` for a literal brace)",
            ),
            ("={}", "has an empty `{}` reference"),
            ("={a{b}}", "has a `{` inside `{ }`"),
        ] {
            assert_eq!(parse_formula(f), Err(err.to_string()), "{f}");
        }
        assert!(
            check_row_formula("={a}/SUM({a:*})")
                .unwrap_err()
                .contains("only a `total`")
        );
        assert!(check_total("sum").is_ok());
        assert!(check_total("=SUM({a:*})-MIN({b:*})").is_ok());
        assert!(check_total("={a}").unwrap_err().contains("doesn't have"));
        assert_eq!(
            check_total("total"),
            Err(
                "must be one of `sum`, `average`, `count`, `min`, `max` or a formula starting with `=`"
                    .into()
            )
        );
        let (cols, errs) = parse_columns(&json!({
            "t": {"formula": "={a}*2", "format": "0.00", "total": "sum"},
            "u": {"formula": "a*2"},
            "v": {"total": "median"}
        }));
        assert_eq!(cols["t"].formula.as_deref(), Some("={a}*2"));
        assert_eq!(cols["t"].total.as_deref(), Some("sum"));
        assert_eq!(errs.len(), 2, "{errs:?}");
        assert!(errs[0].starts_with("column `u`: formula `a*2` must start with `=`"));
        assert!(errs[1].starts_with("column `v`: total `median` must be one of"));
    }
}
