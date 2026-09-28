//! Number formats: which Excel format code each cell gets.
//!
//! Highest first: the query entry's `columns.<name>.format`, the output-level
//! `columns.<name>.format`, (in a template) the template cell's own format if it isn't
//! `General`, then `date_format` / `datetime_format` / `time_format` for date, timestamp and time
//! columns. Numbers, booleans and text have no default.

use std::collections::{BTreeMap, BTreeSet};

use arrow::datatypes::{DataType, Schema};
use dre_protocol::msg::{ColumnOptions, ResultSetMeta};
use dre_protocol::options::{NumFormatClass, check_num_format, parse_columns};
use dre_protocol::plugin::Result;
use serde_json::{Map, Value};

pub const DATE_FORMAT: &str = "yyyy-mm-dd";
pub const DATETIME_FORMAT: &str = "yyyy-mm-dd hh:mm:ss";
pub const TIME_FORMAT: &str = "hh:mm:ss";

/// The type defaults, as options: name and built-in value.
pub const TYPE_DEFAULTS: [(&str, &str); 3] = [
    ("date_format", DATE_FORMAT),
    ("datetime_format", DATETIME_FORMAT),
    ("time_format", TIME_FORMAT),
];

/// Problems with the format options, for the plugin's `validate`.
pub fn validate(options: &Map<String, Value>) -> Vec<String> {
    let mut errs = Vec::new();
    if let Some(v) = options.get("columns").filter(|v| !v.is_null()) {
        errs.extend(parse_columns(v).1.into_iter().map(|e| format!("`columns`: {e}")));
    }
    for (key, _) in TYPE_DEFAULTS {
        if let Some(code) = options.get(key).and_then(Value::as_str) {
            match check_num_format(code) {
                Ok(NumFormatClass::DateTime) => {}
                Ok(class) => errs.push(format!(
                    "`{key}` `{code}` is {}, but it must show dates or times (like `dd/mm/yyyy`)",
                    class.describe()
                )),
                Err(e) => errs.push(format!("`{key}` `{code}` {e}")),
            }
        }
    }
    errs
}

/// What kind of value a column holds, after `cells::normalize`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Number,
    Date,
    DateTime,
    Time,
    Other,
}

impl Kind {
    pub fn of(t: &DataType) -> Kind {
        match t {
            DataType::Int64
            | DataType::UInt64
            | DataType::Float64
            | DataType::Decimal32(..)
            | DataType::Decimal64(..)
            | DataType::Decimal128(..)
            | DataType::Decimal256(..) => Kind::Number,
            DataType::Date32 => Kind::Date,
            DataType::Timestamp(..) => Kind::DateTime,
            DataType::Time64(_) => Kind::Time,
            _ => Kind::Other,
        }
    }

    fn describe(self) -> &'static str {
        match self {
            Kind::Number => "a number column",
            Kind::Date => "a date column",
            Kind::DateTime => "a timestamp column",
            Kind::Time => "a time column",
            Kind::Other => "a text or boolean column",
        }
    }

    fn fits(self, class: NumFormatClass) -> bool {
        match class {
            NumFormatClass::Text => true,
            NumFormatClass::Number => self == Kind::Number,
            NumFormatClass::DateTime => matches!(self, Kind::Date | Kind::DateTime | Kind::Time),
        }
    }
}

/// A column's format: an explicit one from YAML, or its type's default (which a formatted
/// template cell keeps its own format over).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColumnFormat {
    Explicit(String),
    Default(String),
    None,
}

impl ColumnFormat {
    pub fn code(&self) -> Option<&str> {
        match self {
            ColumnFormat::Explicit(c) | ColumnFormat::Default(c) => Some(c),
            ColumnFormat::None => None,
        }
    }
    pub fn is_explicit(&self) -> bool {
        matches!(self, ColumnFormat::Explicit(_))
    }
}

pub struct Formats {
    date: String,
    datetime: String,
    time: String,
    output: BTreeMap<String, ColumnOptions>,
    /// Output-level names some result set has.
    matched: BTreeSet<String>,
    sheets: Vec<String>,
}

impl Formats {
    pub fn new(options: &Map<String, Value>) -> Result<Formats> {
        let errs = validate(options);
        if !errs.is_empty() {
            return Err(errs.join("; ").into());
        }
        let default = |key: &str, builtin: &str| {
            options
                .get(key)
                .and_then(Value::as_str)
                .unwrap_or(builtin)
                .to_string()
        };
        let output = match options.get("columns").filter(|v| !v.is_null()) {
            Some(v) => parse_columns(v).0,
            None => BTreeMap::new(),
        };
        Ok(Formats {
            date: default("date_format", DATE_FORMAT),
            datetime: default("datetime_format", DATETIME_FORMAT),
            time: default("time_format", TIME_FORMAT),
            output,
            matched: BTreeSet::new(),
            sheets: Vec::new(),
        })
    }

    /// Each column's format in one result set (`schema` normalized), checking that every name
    /// the query entry formats exists and that every explicit code fits its column's type.
    pub fn for_set(&mut self, meta: &ResultSetMeta, schema: &Schema) -> Result<Vec<ColumnFormat>> {
        self.sheets.push(meta.name.clone());
        let names: Vec<&str> = schema.fields().iter().map(|f| f.name().as_str()).collect();
        let unknown: Vec<&String> = meta
            .columns
            .keys()
            .filter(|n| !names.contains(&n.as_str()))
            .collect();
        if !unknown.is_empty() {
            let list: Vec<String> = unknown.iter().map(|n| format!("`{n}`")).collect();
            return Err(format!(
                "sheet `{}`: `columns` names {}, which query `{}` doesn't return (it has: {})",
                meta.name,
                list.join(", "),
                meta.query,
                names.join(", ")
            )
            .into());
        }
        let mut out = Vec::with_capacity(names.len());
        for f in schema.fields() {
            let name = f.name();
            if self.output.contains_key(name) {
                self.matched.insert(name.clone());
            }
            let kind = Kind::of(f.data_type());
            let explicit = meta
                .columns
                .get(name)
                .and_then(|c| c.format.clone())
                .or_else(|| self.output.get(name).and_then(|c| c.format.clone()));
            out.push(match explicit {
                Some(code) => {
                    let class = check_num_format(&code)
                        .map_err(|e| format!("column `{name}`: format `{code}` {e}"))?;
                    if !kind.fits(class) {
                        return Err(format!(
                            "sheet `{}`: column `{name}` is {}, but its format `{code}` is {}",
                            meta.name,
                            kind.describe(),
                            class.describe()
                        )
                        .into());
                    }
                    ColumnFormat::Explicit(code)
                }
                None => match kind {
                    Kind::Date => ColumnFormat::Default(self.date.clone()),
                    Kind::DateTime => ColumnFormat::Default(self.datetime.clone()),
                    Kind::Time => ColumnFormat::Default(self.time.clone()),
                    _ => ColumnFormat::None,
                },
            });
        }
        Ok(out)
    }

    /// After the last result set: every output-level name must have matched a column somewhere.
    pub fn finish(&self) -> Result<()> {
        let nowhere: Vec<String> = self
            .output
            .keys()
            .filter(|n| !self.matched.contains(*n))
            .map(|n| format!("`{n}`"))
            .collect();
        if nowhere.is_empty() {
            return Ok(());
        }
        Err(format!(
            "`columns` names {}, which no sheet has (sheets: {}); no workbook was written",
            nowhere.join(", "),
            self.sheets.join(", ")
        )
        .into())
    }
}
