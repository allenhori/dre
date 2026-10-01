//! Lookups: small tables kept as files under `lookups/` (mapping tables, code lists), used in SQL
//! through `ref('name')` without ever being written to the source database by the user.
//!
//! A lookup is `lookups/<name>.<csv|xlsx|xls|json|jsonl|yml>`. Every value is text unless an
//! optional config gives the column a type. For csv, xlsx, xls, json and jsonl the config is a
//! sibling `<name>.yml`; a `.yml` lookup holds its rows and config together:
//!
//! ```yaml
//! columns: {code: string, population: integer}   # optional
//! sheet: Countries                              # xlsx/xls: which sheet (default: the first)
//! load: auto                                    # auto | inline | temp_table
//! rows:                                         # .yml lookups only
//!   - {code: AU, population: 27000000}
//! ```
//!
//! `ref()` inlines a small lookup as `(select * from (values ...) as t(...))`. A lookup with more
//! rows than `lookup_inline_max_rows` is loaded into a temporary table by the source plugin
//! instead, when it can; otherwise it's inlined with a warning.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock};

use arrow::array::{ArrayRef, BooleanArray, Date32Array, Float64Array, Int64Array, StringArray};
use arrow::datatypes::{DataType, Field, Schema};
use arrow::record_batch::RecordBatch;
use chrono::NaiveDate;
use regex::Regex;
use serde_json::Value as Json;
use serde_yaml_ng::Value;

use crate::diag::Diagnostics;
use crate::yaml::YamlFile;

pub const LOOKUPS_DIR: &str = "lookups";
pub const DEFAULT_INLINE_MAX_ROWS: u64 = 200;
const DATA_EXTS: &[&str] = &["csv", "xlsx", "xls", "json", "jsonl"];
/// Keys of a lookup config file.
pub const CONFIG_KEYS: &[&str] = &["columns", "sheet", "load", "rows"];

static IDENT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^[A-Za-z_][A-Za-z0-9_]*$").unwrap());

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColType {
    String,
    Integer,
    Number,
    Boolean,
    Date,
}

impl ColType {
    fn parse(s: &str) -> Option<ColType> {
        Some(match s {
            "string" => ColType::String,
            "integer" => ColType::Integer,
            "number" => ColType::Number,
            "boolean" => ColType::Boolean,
            "date" => ColType::Date,
            _ => return None,
        })
    }
}

/// How `ref()` hands a lookup to SQL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Load {
    /// Inline up to `lookup_inline_max_rows` rows, load into a temp table above that.
    Auto,
    Inline,
    TempTable,
}

#[derive(Debug, Clone)]
pub struct Lookup {
    pub name: String,
    /// The data file, relative to the project root.
    pub file: PathBuf,
    pub types: BTreeMap<String, ColType>,
    pub sheet: Option<String>,
    pub load: Load,
    /// A `.yml` lookup's rows, parsed with its config.
    inline_rows: Option<Json>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Cell {
    Null,
    Text(String),
    Int(i64),
    Num(f64),
    Bool(bool),
    Date(NaiveDate),
}

/// A lookup's rows, typed.
#[derive(Debug, Clone)]
pub struct Table {
    pub columns: Vec<String>,
    pub types: Vec<ColType>,
    pub rows: Vec<Vec<Cell>>,
}

/// Find every lookup among the files under `lookups/` (relative paths) and parse their config.
pub fn discover(root: &Path, files: &[PathBuf], diags: &mut Diagnostics) -> BTreeMap<String, Lookup> {
    let mut by_name: BTreeMap<String, Vec<&PathBuf>> = BTreeMap::new();
    for f in files {
        let stem = f.file_stem().unwrap_or_default().to_string_lossy().to_string();
        by_name.entry(stem).or_default().push(f);
    }
    let mut out = BTreeMap::new();
    for (name, paths) in by_name {
        let ext = |p: &Path| {
            p.extension()
                .and_then(|e| e.to_str())
                .unwrap_or("")
                .to_ascii_lowercase()
        };
        let data: Vec<&&PathBuf> = paths
            .iter()
            .filter(|p| DATA_EXTS.contains(&ext(p).as_str()))
            .collect();
        let config: Vec<&&PathBuf> = paths
            .iter()
            .filter(|p| matches!(ext(p).as_str(), "yml" | "yaml"))
            .collect();
        if data.is_empty() && config.is_empty() {
            continue;
        }
        if data.len() > 1 || config.len() > 1 {
            diags.error(
                "duplicate-lookup",
                Some((*paths[1]).clone()),
                None,
                format!(
                    "lookup `{name}` is defined more than once: {}",
                    paths
                        .iter()
                        .map(|p| p.display().to_string())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
            );
            continue;
        }
        if !IDENT.is_match(&name) {
            diags.error(
                "invalid-lookup",
                Some((*paths[0]).clone()),
                None,
                format!("lookup name `{name}` must be letters, digits and `_`, not starting with a digit"),
            );
            continue;
        }
        let mut l = Lookup {
            name: name.clone(),
            file: data
                .first()
                .map_or_else(|| (**config[0]).clone(), |d| (***d).clone()),
            types: BTreeMap::new(),
            sheet: None,
            load: Load::Auto,
            inline_rows: None,
        };
        if let Some(cfg) = config.first() {
            let Some(yf) = YamlFile::load(&root.join(cfg), (**cfg).clone(), diags) else {
                continue;
            };
            if !parse_config(&yf, data.is_empty(), &mut l, diags) {
                continue;
            }
        }
        if data.is_empty() && l.inline_rows.is_none() {
            // A bare list of rows.
            let yf_rows = std::fs::read_to_string(root.join(&l.file))
                .ok()
                .and_then(|t| serde_yaml_ng::from_str::<Value>(&t).ok())
                .and_then(|v| serde_json::to_value(v).ok());
            l.inline_rows = yf_rows;
        }
        out.insert(name, l);
    }
    out
}

fn parse_config(yf: &YamlFile, is_data: bool, l: &mut Lookup, diags: &mut Diagnostics) -> bool {
    let file = Some(yf.display.clone());
    let m = match &yf.value {
        Value::Mapping(m) => m,
        // A `.yml` lookup may be just a list of rows.
        Value::Sequence(_) if is_data => return true,
        _ => {
            diags.error(
                "invalid-lookup",
                file,
                None,
                if is_data {
                    "a .yml lookup is a list of rows, or a map with `rows:` (and optional `columns`, `load`)"
                } else {
                    "a lookup config must be a map (`columns`, `sheet`, `load`)"
                },
            );
            return false;
        }
    };
    let mut ok = true;
    for k in m.keys() {
        let k = k.as_str().unwrap_or_default();
        if !CONFIG_KEYS.contains(&k) || (k == "rows" && !is_data) || (k == "sheet" && is_data) {
            diags.error(
                "invalid-lookup",
                file.clone(),
                yf.line_of(k, None),
                format!(
                    "unknown key `{k}` in lookup config (expected: columns, sheet, load{})",
                    if is_data { ", rows" } else { "" }
                ),
            );
            ok = false;
        }
    }
    if let Some(cols) = m.get("columns") {
        match cols.as_mapping() {
            Some(cols) => {
                for (k, v) in cols {
                    let (Some(c), Some(t)) = (k.as_str(), v.as_str()) else {
                        continue;
                    };
                    match ColType::parse(t) {
                        Some(t) => {
                            l.types.insert(c.to_string(), t);
                        }
                        None => {
                            diags.error(
                                "invalid-lookup",
                                file.clone(),
                                yf.line_of(c, None),
                                format!("column `{c}` has type `{t}`; use string, integer, number, boolean or date"),
                            );
                            ok = false;
                        }
                    }
                }
            }
            None => {
                diags.error(
                    "invalid-lookup",
                    file.clone(),
                    yf.line_of("columns", None),
                    "`columns` must map column names to types",
                );
                ok = false;
            }
        }
    }
    l.sheet = m.get("sheet").and_then(Value::as_str).map(str::to_string);
    if let Some(v) = m.get("load") {
        l.load = match v.as_str() {
            Some("auto") => Load::Auto,
            Some("inline") => Load::Inline,
            Some("temp_table") => Load::TempTable,
            _ => {
                diags.error(
                    "invalid-lookup",
                    file.clone(),
                    yf.line_of("load", None),
                    "`load` must be auto, inline or temp_table",
                );
                ok = false;
                Load::Auto
            }
        };
    }
    if is_data {
        match m.get("rows") {
            Some(rows) => l.inline_rows = serde_json::to_value(rows).ok(),
            None => {
                diags.error(
                    "invalid-lookup",
                    file,
                    None,
                    "a .yml lookup written as a map needs `rows:`",
                );
                ok = false;
            }
        }
    }
    ok
}

/// Raw rows: column names, then text cells (`None` for empty), with each row's label for errors.
type Raw = (Vec<String>, Vec<(String, Vec<Option<String>>)>);

/// Read and type a lookup's rows.
pub fn read(root: &Path, l: &Lookup) -> Result<Table, String> {
    let path = root.join(&l.file);
    let ext = l
        .file
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let (columns, rows) = match ext.as_str() {
        "csv" => read_csv(&path)?,
        "xlsx" | "xls" => read_sheet(&path, l.sheet.as_deref())?,
        "json" => {
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let v: Json = serde_json::from_str(&text).map_err(|e| format!("invalid JSON: {e}"))?;
            read_objects(v.as_array().ok_or("a .json lookup must be an array of objects")?)?
        }
        "jsonl" => {
            let text = std::fs::read_to_string(&path).map_err(|e| e.to_string())?;
            let mut items = Vec::new();
            for (i, line) in text.lines().enumerate().filter(|(_, l)| !l.trim().is_empty()) {
                items.push(
                    serde_json::from_str(line).map_err(|e| format!("line {}: invalid JSON: {e}", i + 1))?,
                );
            }
            read_objects(&items)?
        }
        _ => match &l.inline_rows {
            Some(Json::Array(items)) => read_objects(items)?,
            _ => return Err("a .yml lookup's rows must be a list of maps".into()),
        },
    };
    for c in &columns {
        if !IDENT.is_match(c) {
            return Err(format!(
                "column `{c}`: names must be letters, digits and `_`, not starting with a digit, so SQL can use them unquoted"
            ));
        }
    }
    for c in l.types.keys() {
        if !columns.contains(c) {
            return Err(format!(
                "the config types column `{c}`, which isn't in the data ({})",
                columns.join(", ")
            ));
        }
    }
    let types: Vec<ColType> = columns
        .iter()
        .map(|c| l.types.get(c).copied().unwrap_or(ColType::String))
        .collect();
    let mut typed = Vec::with_capacity(rows.len());
    for (label, row) in rows {
        let mut cells = Vec::with_capacity(row.len());
        for ((v, t), c) in row.into_iter().zip(&types).zip(&columns) {
            cells.push(to_cell(v, *t).map_err(|e| format!("{label}, column `{c}`: {e}"))?);
        }
        typed.push(cells);
    }
    Ok(Table {
        columns,
        types,
        rows: typed,
    })
}

fn read_csv(path: &Path) -> Result<Raw, String> {
    let text = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
    let mut r = csv::ReaderBuilder::new().from_reader(text.as_bytes());
    let columns: Vec<String> = r
        .headers()
        .map_err(|e| e.to_string())?
        .iter()
        .map(|h| h.trim().to_string())
        .collect();
    let mut rows = Vec::new();
    for rec in r.records() {
        let rec = rec.map_err(|e| e.to_string())?;
        let line = rec.position().map_or(0, |p| p.line());
        rows.push((
            format!("line {line}"),
            rec.iter()
                .map(|v| (!v.is_empty()).then(|| v.to_string()))
                .collect(),
        ));
    }
    Ok((columns, rows))
}

fn read_sheet(path: &Path, sheet: Option<&str>) -> Result<Raw, String> {
    use calamine::{Data, Reader};
    let mut wb = calamine::open_workbook_auto(path).map_err(|e| e.to_string())?;
    let names = wb.sheet_names();
    let name = match sheet {
        Some(s) if names.iter().any(|n| n == s) => s.to_string(),
        Some(s) => return Err(format!("no sheet `{s}` (sheets: {})", names.join(", "))),
        None => names.first().cloned().ok_or("the workbook has no sheets")?,
    };
    let range = wb.worksheet_range(&name).map_err(|e| e.to_string())?;
    let first_row = range.start().map_or(0, |(r, _)| r as usize);
    let text = |d: &Data| -> Result<Option<String>, String> {
        Ok(match d {
            Data::Empty => None,
            Data::String(s) if s.is_empty() => None,
            Data::String(s) | Data::DateTimeIso(s) | Data::DurationIso(s) => Some(s.clone()),
            Data::Int(i) => Some(i.to_string()),
            Data::Float(f) if f.fract() == 0.0 && f.abs() < 1e15 => Some((*f as i64).to_string()),
            Data::Float(f) => Some(f.to_string()),
            Data::Bool(b) => Some(b.to_string()),
            Data::DateTime(dt) => dt.as_datetime().map(|t| {
                if t.time() == chrono::NaiveTime::MIN {
                    t.date().to_string()
                } else {
                    t.format("%Y-%m-%d %H:%M:%S").to_string()
                }
            }),
            Data::Error(e) => return Err(format!("cell error {e:?}")),
        })
    };
    let mut it = range.rows().enumerate();
    let Some((_, header)) = it.next() else {
        return Ok((Vec::new(), Vec::new()));
    };
    let mut columns = Vec::new();
    for d in header {
        columns.push(text(d)?.unwrap_or_default().trim().to_string());
    }
    while columns.last().is_some_and(String::is_empty) {
        columns.pop();
    }
    let mut rows = Vec::new();
    for (i, r) in it {
        let label = format!("row {}", first_row + i + 1);
        let mut cells = Vec::new();
        for d in r.iter().take(columns.len()) {
            cells.push(text(d).map_err(|e| format!("{label}: {e}"))?);
        }
        cells.resize(columns.len(), None);
        if cells.iter().all(Option::is_none) {
            continue;
        }
        rows.push((label, cells));
    }
    Ok((columns, rows))
}

/// Rows given as objects (json, jsonl, yml): columns are every key, in first-seen order.
fn read_objects(items: &[Json]) -> Result<Raw, String> {
    let mut columns: Vec<String> = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let obj = item
            .as_object()
            .ok_or_else(|| format!("entry {}: each row must be an object", i + 1))?;
        for k in obj.keys() {
            if !columns.contains(k) {
                columns.push(k.clone());
            }
        }
    }
    let mut rows = Vec::new();
    for (i, item) in items.iter().enumerate() {
        let obj = item.as_object().unwrap();
        let label = format!("entry {}", i + 1);
        let mut cells = Vec::new();
        for c in &columns {
            cells.push(match obj.get(c) {
                None | Some(Json::Null) => None,
                Some(Json::String(s)) if s.is_empty() => None,
                Some(Json::String(s)) => Some(s.clone()),
                Some(Json::Number(n)) => Some(n.to_string()),
                Some(Json::Bool(b)) => Some(b.to_string()),
                Some(_) => {
                    return Err(format!(
                        "{label}, column `{c}`: values must be plain, not lists or maps"
                    ));
                }
            });
        }
        rows.push((label, cells));
    }
    Ok((columns, rows))
}

fn to_cell(v: Option<String>, t: ColType) -> Result<Cell, String> {
    let Some(v) = v else { return Ok(Cell::Null) };
    let s = v.trim();
    Ok(match t {
        ColType::String => Cell::Text(v),
        ColType::Integer => Cell::Int(s.parse().map_err(|_| format!("`{s}` isn't an integer"))?),
        ColType::Number => {
            let n: f64 = s.parse().map_err(|_| format!("`{s}` isn't a number"))?;
            if !n.is_finite() {
                return Err(format!("`{s}` isn't a finite number"));
            }
            Cell::Num(n)
        }
        ColType::Boolean => Cell::Bool(match s.to_ascii_lowercase().as_str() {
            "true" | "yes" | "y" | "1" => true,
            "false" | "no" | "n" | "0" => false,
            _ => return Err(format!("`{s}` isn't a boolean (true/false)")),
        }),
        ColType::Date => Cell::Date(
            NaiveDate::parse_from_str(s, "%Y-%m-%d")
                .map_err(|_| format!("`{s}` isn't a date (YYYY-MM-DD)"))?,
        ),
    })
}

impl Table {
    /// `(select * from (values ...) as t(cols))`: portable across DuckDB, Postgres and Databricks.
    pub fn inline_sql(&self) -> String {
        let cols = self.columns.join(", ");
        if self.rows.is_empty() {
            let nulls = vec!["NULL"; self.columns.len()].join(", ");
            return format!("(select * from (values ({nulls})) as t({cols}) where 1 = 0)");
        }
        let rows: Vec<String> = self
            .rows
            .iter()
            .map(|r| format!("({})", r.iter().map(literal).collect::<Vec<_>>().join(", ")))
            .collect();
        format!(
            "(select * from (values\n  {}\n) as t({cols}))",
            rows.join(",\n  ")
        )
    }

    /// The rows as one Arrow batch, for loading into a temp table.
    pub fn to_batch(&self) -> Result<RecordBatch, String> {
        let mut fields = Vec::new();
        let mut arrays: Vec<ArrayRef> = Vec::new();
        for (i, (c, t)) in self.columns.iter().zip(&self.types).enumerate() {
            let col = self.rows.iter().map(|r| &r[i]);
            let (dt, a): (DataType, ArrayRef) = match t {
                ColType::String => (
                    DataType::Utf8,
                    Arc::new(
                        col.map(|v| match v {
                            Cell::Text(s) => Some(s.as_str()),
                            _ => None,
                        })
                        .collect::<StringArray>(),
                    ),
                ),
                ColType::Integer => (
                    DataType::Int64,
                    Arc::new(
                        col.map(|v| match v {
                            Cell::Int(n) => Some(*n),
                            _ => None,
                        })
                        .collect::<Int64Array>(),
                    ),
                ),
                ColType::Number => (
                    DataType::Float64,
                    Arc::new(
                        col.map(|v| match v {
                            Cell::Num(n) => Some(*n),
                            _ => None,
                        })
                        .collect::<Float64Array>(),
                    ),
                ),
                ColType::Boolean => (
                    DataType::Boolean,
                    Arc::new(
                        col.map(|v| match v {
                            Cell::Bool(b) => Some(*b),
                            _ => None,
                        })
                        .collect::<BooleanArray>(),
                    ),
                ),
                ColType::Date => {
                    let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
                    (
                        DataType::Date32,
                        Arc::new(
                            col.map(|v| match v {
                                Cell::Date(d) => Some((*d - epoch).num_days() as i32),
                                _ => None,
                            })
                            .collect::<Date32Array>(),
                        ),
                    )
                }
            };
            fields.push(Field::new(c, dt, true));
            arrays.push(a);
        }
        RecordBatch::try_new(Arc::new(Schema::new(fields)), arrays).map_err(|e| e.to_string())
    }
}

fn literal(c: &Cell) -> String {
    match c {
        Cell::Null => "NULL".into(),
        Cell::Text(s) => text_literal(s),
        Cell::Int(n) => n.to_string(),
        Cell::Num(n) => format!("{n:?}"),
        Cell::Bool(b) => b.to_string(),
        Cell::Date(d) => format!("date '{d}'"),
    }
}

/// A string literal every supported dialect reads the same way. Quotes and backslashes are
/// spelled `chr(39)`/`chr(92)`: doubling a quote works in Postgres and DuckDB but Spark SQL reads
/// `'a''b'` as two adjacent literals, and Spark treats backslash as an escape where Postgres
/// doesn't.
fn text_literal(s: &str) -> String {
    if !s.contains(['\'', '\\']) {
        return format!("'{s}'");
    }
    let mut parts = Vec::new();
    let mut run = String::new();
    for ch in s.chars() {
        let code = match ch {
            '\'' => 39,
            '\\' => 92,
            _ => {
                run.push(ch);
                continue;
            }
        };
        if !run.is_empty() {
            parts.push(format!("'{}'", std::mem::take(&mut run)));
        }
        parts.push(format!("chr({code})"));
    }
    if !run.is_empty() {
        parts.push(format!("'{run}'"));
    }
    format!("({})", parts.join(" || "))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn inlined_text_never_relies_on_a_dialects_escapes() {
        // Portable on DuckDB, Postgres and Databricks: no '' and no \'.
        assert_eq!(text_literal("plain"), "'plain'");
        assert_eq!(
            text_literal("a\\b'c"),
            "('a' || chr(92) || 'b' || chr(39) || 'c')"
        );
        assert_eq!(text_literal("'"), "(chr(39))");
    }

    fn lookup(dir: &Path, file: &str, body: &str, cfg: Option<&str>) -> (Lookup, Diagnostics) {
        std::fs::create_dir_all(dir.join("lookups")).unwrap();
        std::fs::write(dir.join("lookups").join(file), body).unwrap();
        let mut files = vec![PathBuf::from("lookups").join(file)];
        if let Some(c) = cfg {
            let stem = Path::new(file).file_stem().unwrap().to_string_lossy().to_string();
            std::fs::write(dir.join("lookups").join(format!("{stem}.yml")), c).unwrap();
            files.push(PathBuf::from("lookups").join(format!("{stem}.yml")));
        }
        let mut d = Diagnostics::default();
        let mut found = discover(dir, &files, &mut d);
        (found.pop_first().map(|(_, l)| l).expect("a lookup"), d)
    }

    #[test]
    fn csv_is_all_text_unless_typed() {
        let dir = tempfile::tempdir().unwrap();
        let (l, _) = lookup(
            dir.path(),
            "c.csv",
            "\u{feff}code,name,pop\nAU,\"O'Brien, Aus\",27\nNZ,,5\n",
            Some("columns: {pop: integer}\n"),
        );
        let t = read(dir.path(), &l).unwrap();
        assert_eq!(t.columns, ["code", "name", "pop"]);
        assert_eq!(
            t.inline_sql(),
            "(select * from (values\n  ('AU', ('O' || chr(39) || 'Brien, Aus'), 27),\n  ('NZ', NULL, 5)\n) as t(code, name, pop))"
        );
        assert_eq!(t.to_batch().unwrap().num_rows(), 2);
    }

    #[test]
    fn json_jsonl_and_yml_rows_are_objects() {
        let dir = tempfile::tempdir().unwrap();
        let (l, _) = lookup(
            dir.path(),
            "j.json",
            r#"[{"a": 1, "b": true}, {"a": 2, "c": "x"}]"#,
            None,
        );
        let t = read(dir.path(), &l).unwrap();
        assert_eq!(t.columns, ["a", "b", "c"]);
        assert_eq!(
            t.rows[1],
            [Cell::Text("2".into()), Cell::Null, Cell::Text("x".into())]
        );

        let (l, _) = lookup(dir.path(), "k.jsonl", "{\"a\": 1}\n\n{\"a\": 2}\n", None);
        assert_eq!(read(dir.path(), &l).unwrap().rows.len(), 2);

        let (l, d) = lookup(
            dir.path(),
            "y.yml",
            "columns: {d: date, ok: boolean}\nrows:\n  - {d: 2026-01-02, ok: yes}\n",
            None,
        );
        assert!(!d.has_errors());
        let t = read(dir.path(), &l).unwrap();
        assert_eq!(
            t.inline_sql(),
            "(select * from (values\n  (date '2026-01-02', true)\n) as t(d, ok))"
        );
    }

    #[test]
    fn problems_name_the_row_and_column() {
        let dir = tempfile::tempdir().unwrap();
        let (l, _) = lookup(
            dir.path(),
            "c.csv",
            "code,pop\nAU,many\n",
            Some("columns: {pop: integer}\n"),
        );
        assert_eq!(
            read(dir.path(), &l).unwrap_err(),
            "line 2, column `pop`: `many` isn't an integer"
        );
        let (l, _) = lookup(dir.path(), "d.csv", "country code\nAU\n", None);
        assert!(
            read(dir.path(), &l)
                .unwrap_err()
                .starts_with("column `country code`")
        );
        std::fs::write(dir.path().join("lookups/e.csv"), "a\n1\n").unwrap();
        std::fs::write(
            dir.path().join("lookups/e.yml"),
            "columns: {a: int}\nlod: inline\n",
        )
        .unwrap();
        let mut d = Diagnostics::default();
        let files = [PathBuf::from("lookups/e.csv"), PathBuf::from("lookups/e.yml")];
        assert!(discover(dir.path(), &files, &mut d).is_empty());
        assert_eq!(d.error_count(), 2);
    }

    #[test]
    fn an_empty_lookup_still_has_its_columns() {
        let dir = tempfile::tempdir().unwrap();
        let (l, _) = lookup(dir.path(), "c.csv", "a,b\n", None);
        assert_eq!(
            read(dir.path(), &l).unwrap().inline_sql(),
            "(select * from (values (NULL, NULL)) as t(a, b) where 1 = 0)"
        );
    }
}
