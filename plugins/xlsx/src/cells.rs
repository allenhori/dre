//! Arrow values into Excel cells, shared by plain and template output.

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::compute::cast;
use arrow::datatypes::{
    DataType, Date32Type, Float64Type, Int64Type, TimeUnit, TimestampMicrosecondType, UInt64Type,
};
use arrow::util::display::{ArrayFormatter, FormatOptions};
use dre_protocol::plugin::Result;
use std::collections::HashMap;

use rust_xlsxwriter::{Format, Worksheet};

use crate::formats::{ColumnFormat, Kind};

/// Days from Excel's epoch (1899-12-30) to 1970-01-01.
pub const EPOCH_OFFSET: f64 = 25_569.0;
const EXCEL_MAX_STRING: usize = 32_767;

/// A value as Excel stores it.
pub enum Excel {
    Number(f64),
    Date(f64),
    DateTime(f64),
    Time(f64),
    Bool(bool),
    Text(String),
}

/// Excel keeps 15 significant digits.
const EXCEL_DIGITS: usize = 15;
/// Excel's largest number.
const EXCEL_MAX_NUMBER: f64 = 9.99999999999999e307;
/// Serials Excel shows as dates exactly: 1900-03-01 (before it, Excel's phantom 1900-02-29
/// shifts every date by a day) to 9999-12-31.
const FIRST_SERIAL: f64 = 61.0;
const END_SERIAL: f64 = 2_958_466.0;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum Lossy {
    Digits,
    Range,
    Date,
}

/// Values written as text instead of numbers or dates, per column: reported once each.
static LOSSY: std::sync::Mutex<std::collections::BTreeMap<(String, Lossy), u64>> =
    std::sync::Mutex::new(std::collections::BTreeMap::new());

/// Columns with a format some of whose values were written as text, so the format wasn't applied.
static UNFORMATTED: std::sync::Mutex<std::collections::BTreeSet<String>> =
    std::sync::Mutex::new(std::collections::BTreeSet::new());

/// Note that a formatted column had a value written as text, without its format.
pub fn unformatted(column: &str) {
    UNFORMATTED.lock().unwrap().insert(column.to_string());
}

fn lossy(column: &str, why: Lossy) {
    *LOSSY
        .lock()
        .unwrap()
        .entry((column.to_string(), why))
        .or_default() += 1;
}

/// One warning per column and reason for values written as text, since the last call.
pub fn take_warnings() -> Vec<String> {
    let unformatted = std::mem::take(&mut *UNFORMATTED.lock().unwrap());
    std::mem::take(&mut *LOSSY.lock().unwrap())
        .into_iter()
        .map(|((column, why), n)| {
            let what = match why {
                Lossy::Digits => format!(
                    "{n} value(s) have more than Excel's {EXCEL_DIGITS} significant digits and were written as text, keeping every digit"
                ),
                Lossy::Range => format!("{n} value(s) are beyond what Excel can store as a number and were written as text"),
                Lossy::Date => format!(
                    "{n} date(s) before 1900-03-01 or after 9999-12-31, which Excel can't show as dates, were written as ISO text"
                ),
            };
            let note = if unformatted.contains(&column) {
                "; the column's format wasn't applied to them"
            } else {
                ""
            };
            format!("column `{column}`: {what}{note}")
        })
        .collect()
}

fn significant_digits(text: &str) -> usize {
    let digits: String = text
        .chars()
        .take_while(|c| *c != 'e' && *c != 'E')
        .filter(char::is_ascii_digit)
        .collect();
    let digits = digits.trim_start_matches('0');
    // Trailing zeros of an integer part or a fraction carry no precision.
    digits.trim_end_matches('0').len()
}

fn text(a: &dyn Array, i: usize) -> String {
    ArrayFormatter::try_new(a, &FormatOptions::default())
        .map(|f| f.value(i).to_string())
        .unwrap_or_default()
}

/// A number, or its exact text when Excel would change it.
fn number(a: &dyn Array, i: usize, column: &str, v: f64) -> Excel {
    let t = text(a, i);
    if significant_digits(&t) > EXCEL_DIGITS {
        lossy(column, Lossy::Digits);
        Excel::Text(t)
    } else {
        Excel::Number(v)
    }
}

fn serial(a: &dyn Array, i: usize, column: &str, serial: f64, as_date: fn(f64) -> Excel) -> Excel {
    if (FIRST_SERIAL..END_SERIAL).contains(&serial) {
        as_date(serial)
    } else {
        lossy(column, Lossy::Date);
        Excel::Text(text(a, i))
    }
}

/// Row `i` of `a` (after [`normalize`]) as Excel stores it; `None` for a null.
pub fn excel_value(a: &dyn Array, i: usize, column: &str) -> Option<Excel> {
    if a.is_null(i) {
        return None;
    }
    Some(match a.data_type() {
        DataType::Float64 => {
            let v = a.as_primitive::<Float64Type>().value(i);
            if v.is_finite() && v.abs() <= EXCEL_MAX_NUMBER {
                Excel::Number(v)
            } else {
                lossy(column, Lossy::Range);
                Excel::Text(text(a, i))
            }
        }
        DataType::Int64 => number(a, i, column, a.as_primitive::<Int64Type>().value(i) as f64),
        DataType::UInt64 => number(a, i, column, a.as_primitive::<UInt64Type>().value(i) as f64),
        DataType::Decimal32(..)
        | DataType::Decimal64(..)
        | DataType::Decimal128(..)
        | DataType::Decimal256(..) => {
            let t = text(a, i);
            match t.parse::<f64>() {
                Ok(v) if v.abs() <= EXCEL_MAX_NUMBER => number(a, i, column, v),
                _ => {
                    lossy(column, Lossy::Range);
                    Excel::Text(t)
                }
            }
        }
        DataType::Boolean => Excel::Bool(a.as_boolean().value(i)),
        DataType::Date32 => {
            let days = a.as_primitive::<Date32Type>().value(i) as f64;
            serial(a, i, column, days + EPOCH_OFFSET, Excel::Date)
        }
        DataType::Timestamp(TimeUnit::Microsecond, _) => {
            let us = a.as_primitive::<TimestampMicrosecondType>().value(i) as f64;
            serial(
                a,
                i,
                column,
                us / 86_400_000_000.0 + EPOCH_OFFSET,
                Excel::DateTime,
            )
        }
        DataType::Time64(TimeUnit::Microsecond) => {
            let us = a
                .as_primitive::<arrow::datatypes::Time64MicrosecondType>()
                .value(i) as f64;
            Excel::Time(us / 86_400_000_000.0)
        }
        DataType::Utf8 => Excel::Text(a.as_string::<i32>().value(i).to_string()),
        _ => Excel::Text(text(a, i)),
    })
}

/// Writes cells, keeping one Excel format object per distinct code.
pub struct CellWriter {
    cache: HashMap<String, Format>,
}

impl CellWriter {
    pub fn new() -> CellWriter {
        CellWriter {
            cache: HashMap::new(),
        }
    }

    /// The format objects for one result set's columns.
    pub fn formats(&mut self, cols: &[ColumnFormat]) -> Vec<Option<ColumnStyle>> {
        cols.iter()
            .map(|c| {
                c.code().map(|code| ColumnStyle {
                    format: self
                        .cache
                        .entry(code.to_string())
                        .or_insert_with(|| Format::new().set_num_format(code))
                        .clone(),
                    explicit: c.is_explicit(),
                })
            })
            .collect()
    }

    /// Write row `i` of `a` at `(row, col)` with the column's style. Nulls leave the cell empty.
    #[allow(clippy::too_many_arguments)]
    pub fn write(
        &self,
        ws: &mut Worksheet,
        row: u32,
        col: u16,
        a: &dyn Array,
        i: usize,
        column: &str,
        style: Option<&ColumnStyle>,
    ) -> Result<()> {
        let Some(v) = excel_value(a, i, column) else {
            return Ok(());
        };
        let fmt = style.map(|s| &s.format);
        match (v, fmt) {
            (Excel::Number(n) | Excel::Date(n) | Excel::DateTime(n) | Excel::Time(n), Some(f)) => {
                ws.write_number_with_format(row, col, n, f)?
            }
            (Excel::Number(n) | Excel::Date(n) | Excel::DateTime(n) | Excel::Time(n), None) => {
                ws.write_number(row, col, n)?
            }
            (Excel::Bool(b), Some(f)) => ws.write_boolean_with_format(row, col, b, f)?,
            (Excel::Bool(b), None) => ws.write_boolean(row, col, b)?,
            (Excel::Text(s), f) => {
                if s.chars().count() > EXCEL_MAX_STRING {
                    return Err(format!("column `{column}`: a value is longer than Excel's {EXCEL_MAX_STRING}-character cell limit").into());
                }
                match f {
                    // A number or date Excel can't hold stays unformatted text.
                    Some(_) if Kind::of(a.data_type()) != Kind::Other => {
                        if style.is_some_and(|s| s.explicit) {
                            unformatted(column);
                        }
                        ws.write_string(row, col, s)?
                    }
                    Some(f) => ws.write_string_with_format(row, col, s, f)?,
                    None => ws.write_string(row, col, s)?,
                }
            }
        };
        Ok(())
    }
}

/// A column's number format, and whether YAML asked for it.
#[derive(Clone)]
pub struct ColumnStyle {
    pub format: Format,
    pub explicit: bool,
}

/// Cast every column to one of the handful of types `CellWriter` writes natively.
pub fn normalize(batch: &RecordBatch) -> Result<RecordBatch> {
    let mut cols = Vec::with_capacity(batch.num_columns());
    let mut fields = Vec::with_capacity(batch.num_columns());
    for (f, c) in batch.schema().fields().iter().zip(batch.columns()) {
        let to = match c.data_type() {
            DataType::Int8 | DataType::Int16 | DataType::Int32 | DataType::Int64 => Some(DataType::Int64),
            DataType::UInt8 | DataType::UInt16 | DataType::UInt32 | DataType::UInt64 => {
                Some(DataType::UInt64)
            }
            DataType::Float16 | DataType::Float32 | DataType::Float64 => Some(DataType::Float64),
            DataType::Date32 | DataType::Date64 => Some(DataType::Date32),
            DataType::Timestamp(_, _) => Some(DataType::Timestamp(TimeUnit::Microsecond, None)),
            DataType::Time32(_) | DataType::Time64(_) => Some(DataType::Time64(TimeUnit::Microsecond)),
            DataType::LargeUtf8 | DataType::Utf8View => Some(DataType::Utf8),
            _ => None,
        };
        let col = match to {
            Some(t) if &t != c.data_type() => cast(c, &t)?,
            _ => c.clone(),
        };
        fields.push(arrow::datatypes::Field::new(
            f.name(),
            col.data_type().clone(),
            true,
        ));
        cols.push(col);
    }
    Ok(RecordBatch::try_new(
        std::sync::Arc::new(arrow::datatypes::Schema::new(fields)),
        cols,
    )?)
}

pub use dre_protocol::util::parse_cell;
