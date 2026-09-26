//! Arrow values into Excel cells, shared by plain and template output.

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::compute::cast;
use arrow::datatypes::{
    DataType, Date32Type, Float64Type, Int64Type, TimeUnit, TimestampMicrosecondType, UInt64Type,
};
use arrow::util::display::{ArrayFormatter, FormatOptions};
use dre_protocol::plugin::Result;
use rust_xlsxwriter::{Format, Worksheet};

/// Days from Excel's epoch (1899-12-30) to 1970-01-01.
const EPOCH_OFFSET: f64 = 25_569.0;
const EXCEL_MAX_STRING: usize = 32_767;

pub struct CellWriter {
    date: Format,
    datetime: Format,
    time: Format,
}

impl CellWriter {
    pub fn new() -> CellWriter {
        CellWriter {
            date: Format::new().set_num_format("yyyy-mm-dd"),
            datetime: Format::new().set_num_format("yyyy-mm-dd hh:mm:ss"),
            time: Format::new().set_num_format("hh:mm:ss"),
        }
    }

    /// Write row `i` of `a` at `(row, col)`. Nulls leave the cell empty.
    pub fn write(
        &self,
        ws: &mut Worksheet,
        row: u32,
        col: u16,
        a: &dyn Array,
        i: usize,
        column: &str,
    ) -> Result<()> {
        if a.is_null(i) {
            return Ok(());
        }
        match a.data_type() {
            DataType::Float64 => {
                ws.write_number(row, col, a.as_primitive::<Float64Type>().value(i))?;
            }
            DataType::Int64 => {
                ws.write_number(row, col, a.as_primitive::<Int64Type>().value(i) as f64)?;
            }
            DataType::UInt64 => {
                ws.write_number(row, col, a.as_primitive::<UInt64Type>().value(i) as f64)?;
            }
            DataType::Boolean => {
                ws.write_boolean(row, col, a.as_boolean().value(i))?;
            }
            DataType::Date32 => {
                let days = a.as_primitive::<Date32Type>().value(i) as f64;
                ws.write_number_with_format(row, col, days + EPOCH_OFFSET, &self.date)?;
            }
            DataType::Timestamp(TimeUnit::Microsecond, _) => {
                let us = a.as_primitive::<TimestampMicrosecondType>().value(i) as f64;
                ws.write_number_with_format(row, col, us / 86_400_000_000.0 + EPOCH_OFFSET, &self.datetime)?;
            }
            DataType::Time64(TimeUnit::Microsecond) => {
                let us = a
                    .as_primitive::<arrow::datatypes::Time64MicrosecondType>()
                    .value(i) as f64;
                ws.write_number_with_format(row, col, us / 86_400_000_000.0, &self.time)?;
            }
            DataType::Utf8 => {
                let s = a.as_string::<i32>().value(i);
                if s.chars().count() > EXCEL_MAX_STRING {
                    return Err(format!("column `{column}`: a value is longer than Excel's {EXCEL_MAX_STRING}-character cell limit").into());
                }
                ws.write_string(row, col, s)?;
            }
            _ => {
                let f = ArrayFormatter::try_new(a, &FormatOptions::default())?;
                ws.write_string(row, col, f.value(i).to_string())?;
            }
        }
        Ok(())
    }
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
            DataType::Decimal32(..)
            | DataType::Decimal64(..)
            | DataType::Decimal128(..)
            | DataType::Decimal256(..) => Some(DataType::Float64),
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
