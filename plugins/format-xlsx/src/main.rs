//! DRE format plugin `xlsx`: one sheet per result set, written in constant memory, or a branded
//! template filled in place (see `template`).
//!
//! Options: `header` (default true), `max_rows_per_sheet` (default 1,000,000). Per result set:
//! `anchor` (default `A1`) and `header`. A result set longer than `max_rows_per_sheet` continues
//! on `Name (2)`, `Name (3)`, ... with the header repeated.
//!
//! Values Excel can't hold exactly are written as text, with one warning per column: numbers
//! with more than 15 significant digits (int64 beyond that, wide decimals), numbers beyond
//! Excel's range, and dates or timestamps before 1900-03-01 or after 9999-12-31 (as ISO text).

mod cells;
mod template;

use dre_protocol::options::{OptionField, OptionType};
use dre_protocol::plugin::{About, Format, Result, ResultSets, WriteRequest, serve_format};
use rust_xlsxwriter::{Format as XFormat, Workbook};
use serde_json::Value;

use cells::{CellWriter, parse_cell};

pub const EXCEL_MAX_ROWS: u32 = 1_048_576;
const DEFAULT_MAX_ROWS: u64 = 1_000_000;

struct Xlsx;

impl Format for Xlsx {
    fn options(&self) -> Vec<OptionField> {
        vec![
            OptionField::new(
                "header",
                OptionType::Boolean,
                "write column names above each result set",
            )
            .default(true),
            OptionField::new(
                "max_rows_per_sheet",
                OptionType::Integer,
                "rows per sheet before continuing on `Name (2)`; Excel's limit less a header",
            )
            .range(Some(1.0), Some((EXCEL_MAX_ROWS - 1) as f64))
            .default(DEFAULT_MAX_ROWS),
        ]
    }

    fn write(&mut self, req: &WriteRequest, sets: &mut ResultSets<'_>) -> Result<Vec<String>> {
        if req.template.is_some() {
            let files = template::fill(req, sets)?;
            for w in cells::take_warnings() {
                sets.warn(w);
            }
            return Ok(files);
        }
        let header_default = req.options.get("header").and_then(Value::as_bool).unwrap_or(true);
        let max_rows = req
            .options
            .get("max_rows_per_sheet")
            .and_then(Value::as_u64)
            .unwrap_or(DEFAULT_MAX_ROWS);
        let mut wb = Workbook::new();
        let bold = XFormat::new().set_bold();
        let cells = CellWriter::new();
        while let Some(mut rs) = sets.next_set()? {
            let (anchor_row, anchor_col) = match &rs.meta.anchor {
                Some(a) => parse_cell(a).ok_or_else(|| format!("invalid anchor `{a}`"))?,
                None => (0, 0),
            };
            let header = rs.meta.header.unwrap_or(header_default);
            let names: Vec<String> = rs.schema.fields().iter().map(|f| f.name().clone()).collect();
            let room = (EXCEL_MAX_ROWS - anchor_row) as u64 - u64::from(header);
            let cap = max_rows.min(room);
            if cap == 0 {
                return Err(format!(
                    "sheet `{}`: anchor {} leaves no room for rows",
                    rs.meta.name,
                    rs.meta.anchor.as_deref().unwrap_or("A1")
                )
                .into());
            }
            let base = rs.meta.name.clone();
            let mut part = 1u32;
            let mut ws = wb.add_worksheet_with_constant_memory();
            ws.set_name(&base)?;
            let mut written: u64 = 0;
            let mut row = anchor_row;
            if header {
                for (c, n) in names.iter().enumerate() {
                    ws.write_string_with_format(row, anchor_col + c as u16, n, &bold)?;
                }
                row += 1;
            }
            while let Some(batch) = rs.next_batch()? {
                let batch = cells::normalize(&batch)?;
                for i in 0..batch.num_rows() {
                    if written == cap {
                        part += 1;
                        ws = wb.add_worksheet_with_constant_memory();
                        ws.set_name(continuation_name(&base, part))?;
                        row = anchor_row;
                        written = 0;
                        if header {
                            for (c, n) in names.iter().enumerate() {
                                ws.write_string_with_format(row, anchor_col + c as u16, n, &bold)?;
                            }
                            row += 1;
                        }
                    }
                    for (c, (col, name)) in batch.columns().iter().zip(&names).enumerate() {
                        cells.write(ws, row, anchor_col + c as u16, col.as_ref(), i, name)?;
                    }
                    row += 1;
                    written += 1;
                }
            }
        }
        wb.save(&req.path)
            .map_err(|e| format!("can't save {}: {e}", req.path))?;
        for w in cells::take_warnings() {
            sets.warn(w);
        }
        Ok(vec![req.path.clone()])
    }
}

/// `Base (n)`, shortening the base so the name stays within Excel's 31 characters.
pub fn continuation_name(base: &str, n: u32) -> String {
    let suffix = format!(" ({n})");
    let keep = 31 - suffix.chars().count();
    let short: String = base.chars().take(keep).collect();
    format!("{short}{suffix}")
}

fn main() {
    serve_format(About::new("xlsx", env!("CARGO_PKG_VERSION")), Xlsx)
}
