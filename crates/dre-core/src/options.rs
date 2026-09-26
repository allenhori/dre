//! Output format options, validated offline against the documented set.

use std::sync::LazyLock;

use regex::Regex;
use serde_json::{Map, Value};

/// Excel's row limit minus a header row.
pub const XLSX_MAX_ROWS: u64 = 1_048_575;
pub const XLSX_DEFAULT_ROWS_PER_SHEET: u64 = 1_000_000;

/// Formats whose options core knows. Anything else (a format pack) defines its own options.
pub const KNOWN_FORMATS: &[&str] = &["csv", "delimited", "fixed_width", "parquet", "xlsx"];

static CELL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^\$?([A-Za-z]{1,3})\$?([0-9]+)$").unwrap());

/// Parse `B12` into zero-based `(row, col)`.
pub fn parse_cell(s: &str) -> Option<(u32, u16)> {
    let c = CELL.captures(s.trim())?;
    let col = c[1]
        .to_ascii_uppercase()
        .bytes()
        .fold(0u32, |acc, b| acc * 26 + (b - b'A' + 1) as u32);
    let row: u32 = c[2].parse().ok()?;
    if col == 0 || col > 16_384 || row == 0 || row > 1_048_576 {
        return None;
    }
    Some((row - 1, (col - 1) as u16))
}

pub fn is_cell(s: &str) -> bool {
    parse_cell(s).is_some()
}

fn allowed(format: &str) -> &'static [&'static str] {
    match format {
        "xlsx" => &["header", "max_rows_per_sheet"],
        "csv" | "delimited" => &[
            "delimiter",
            "quote",
            "quoting",
            "header",
            "line_ending",
            "encoding",
            "null",
            "byte_order_mark",
        ],
        "fixed_width" => &["columns", "line_ending", "encoding"],
        _ => &[],
    }
}

fn single_char(v: &Value) -> bool {
    v.as_str().is_some_and(|s| s.chars().count() == 1)
}

/// A text encoding DRE can write.
pub fn encoding_supported(label: &str) -> bool {
    match encoding_rs::Encoding::for_label(label.as_bytes()) {
        // encoding_rs can decode UTF-16 but not encode it.
        Some(e) => e.output_encoding() == e,
        None => false,
    }
}

/// Problems with a format's options (`options` excludes `format`, `destination`, `template`).
pub fn validate(format: &str, options: &Map<String, Value>) -> Vec<String> {
    let mut errs = Vec::new();
    if !KNOWN_FORMATS.contains(&format) {
        return errs;
    }
    for k in options.keys() {
        if !allowed(format).contains(&k.as_str()) {
            errs.push(format!("unknown option `{k}` for format `{format}`"));
        }
    }
    let get = |k: &str| options.get(k);
    let bool_opt = |k: &str, errs: &mut Vec<String>| {
        if let Some(v) = get(k)
            && !v.is_boolean()
        {
            errs.push(format!("`{k}` must be true or false"));
        }
    };
    let line_ending = |errs: &mut Vec<String>| {
        if let Some(v) = get("line_ending")
            && !matches!(v.as_str(), Some("\n") | Some("\r\n"))
        {
            errs.push(r#"`line_ending` must be "\n" or "\r\n""#.to_string());
        }
    };
    let encoding = |errs: &mut Vec<String>| {
        if let Some(v) = get("encoding") {
            match v.as_str() {
                Some(s) if encoding_supported(s) => {}
                _ => errs.push(format!("unknown or unsupported `encoding` {v}")),
            }
        }
    };
    match format {
        "xlsx" => {
            bool_opt("header", &mut errs);
            if let Some(v) = get("max_rows_per_sheet") {
                match v.as_u64() {
                    Some(n) if (1..=XLSX_MAX_ROWS).contains(&n) => {}
                    _ => errs.push(format!(
                        "`max_rows_per_sheet` must be a whole number from 1 to {XLSX_MAX_ROWS} (Excel's limit)"
                    )),
                }
            }
        }
        "csv" | "delimited" => {
            for k in ["delimiter", "quote"] {
                if let Some(v) = get(k)
                    && !single_char(v)
                {
                    errs.push(format!("`{k}` must be a single character"));
                }
            }
            if let Some(v) = get("quoting")
                && !matches!(v.as_str(), Some("minimal" | "all" | "none"))
            {
                errs.push("`quoting` must be one of `minimal`, `all`, `none`".to_string());
            }
            bool_opt("header", &mut errs);
            bool_opt("byte_order_mark", &mut errs);
            if let Some(v) = get("null")
                && !v.is_string()
            {
                errs.push("`null` must be a string".to_string());
            }
            line_ending(&mut errs);
            encoding(&mut errs);
        }
        "fixed_width" => {
            line_ending(&mut errs);
            encoding(&mut errs);
            match get("columns").and_then(Value::as_array) {
                None => errs.push("fixed_width output needs a `columns:` list".to_string()),
                Some(cols) if cols.is_empty() => errs.push("`columns:` must not be empty".to_string()),
                Some(cols) => {
                    for (i, c) in cols.iter().enumerate() {
                        errs.extend(fixed_width_column(i + 1, c));
                    }
                }
            }
        }
        _ => {}
    }
    errs
}

fn fixed_width_column(n: usize, c: &Value) -> Vec<String> {
    let mut errs = Vec::new();
    let Some(m) = c.as_object() else {
        return vec![format!("column {n} must be a map with `name` and `width`")];
    };
    let label = match m.get("name").and_then(Value::as_str) {
        Some(s) => format!("column `{s}`"),
        None => {
            errs.push(format!("column {n} has no `name`"));
            format!("column {n}")
        }
    };
    for k in m.keys() {
        if !["name", "width", "align", "pad", "truncate"].contains(&k.as_str()) {
            errs.push(format!("{label} has unknown key `{k}`"));
        }
    }
    match m.get("width").and_then(Value::as_u64) {
        Some(w) if w > 0 => {}
        _ => errs.push(format!("{label} needs a positive whole-number `width`")),
    }
    if let Some(a) = m.get("align")
        && !matches!(a.as_str(), Some("left" | "right"))
    {
        errs.push(format!("{label} `align` must be `left` or `right`"));
    }
    if let Some(p) = m.get("pad")
        && !single_char(p)
    {
        errs.push(format!("{label} `pad` must be a single character"));
    }
    if let Some(t) = m.get("truncate")
        && !t.is_boolean()
    {
        errs.push(format!("{label} `truncate` must be true or false"));
    }
    errs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cell_references() {
        assert_eq!(parse_cell("A1"), Some((0, 0)));
        assert_eq!(parse_cell("$B$12"), Some((11, 1)));
        assert_eq!(parse_cell("XFD1048576"), Some((1_048_575, 16_383)));
        assert_eq!(parse_cell("XFE1"), None);
        assert_eq!(parse_cell("A0"), None);
        assert_eq!(parse_cell("1A"), None);
    }
}
