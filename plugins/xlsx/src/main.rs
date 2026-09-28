//! DRE format plugin `xlsx`: one sheet per result set, written in constant memory, or a branded
//! template filled in place (see `template`).
//!
//! Options: `header` (default true), `max_rows_per_sheet` (default 1,000,000), `columns` (per
//! column name: a number `format`, a row `formula`, a `total`), `date_format`, `datetime_format`,
//! `time_format` (see `formats`), `totals_label` (see `formulas`). Per result set: `anchor`
//! (default `A1`), `header` and `columns`. A result set longer than `max_rows_per_sheet`
//! continues on `Name (2)`, `Name (3)`, ... with the header repeated, and a totals row on each.
//!
//! Values Excel can't hold exactly are written as text, with one warning per column: numbers
//! with more than 15 significant digits (int64 beyond that, wide decimals), numbers beyond
//! Excel's range, and dates or timestamps before 1900-03-01 or after 9999-12-31 (as ISO text).

mod cells;
mod formats;
mod formulas;
mod template;

use dre_protocol::options::{OptionField, OptionType};
use dre_protocol::plugin::{About, Format, Result, ResultSets, WriteRequest, serve_format};
use rust_xlsxwriter::{Format as XFormat, Workbook};
use serde_json::Value;

use cells::{CellWriter, parse_cell};
use formats::Formats;
use formulas::{Acc, SetFormulas, Total};

pub const EXCEL_MAX_ROWS: u32 = 1_048_576;
const DEFAULT_MAX_ROWS: u64 = 1_000_000;
const DEFAULT_TOTALS_LABEL: &str = "Total";

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
            OptionField::new(
                "columns",
                OptionType::Map,
                "per column name, on any sheet: `{format: <Excel number format>, formula: \"={a}*{b}\", total: sum}`; a query entry's `columns` wins",
            ),
            OptionField::new(
                "totals_label",
                OptionType::String,
                "text in the first column of a totals row, when that column has no total",
            )
            .default(DEFAULT_TOTALS_LABEL),
            OptionField::new(
                "date_format",
                OptionType::String,
                "Excel number format for date columns",
            )
            .default(formats::DATE_FORMAT),
            OptionField::new(
                "datetime_format",
                OptionType::String,
                "Excel number format for timestamp columns",
            )
            .default(formats::DATETIME_FORMAT),
            OptionField::new(
                "time_format",
                OptionType::String,
                "Excel number format for time columns",
            )
            .default(formats::TIME_FORMAT),
        ]
    }

    fn validate(&self, options: &serde_json::Map<String, Value>) -> Vec<String> {
        formats::validate(options)
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
        let label = req
            .options
            .get("totals_label")
            .and_then(Value::as_str)
            .unwrap_or(DEFAULT_TOTALS_LABEL)
            .to_string();
        let mut fmts = Formats::new(&req.options)?;
        let mut wb = Workbook::new();
        let bold = XFormat::new().set_bold();
        let mut cells = CellWriter::new();
        while let Some(mut rs) = sets.next_set()? {
            let normalized = cells::normalize(&arrow::array::RecordBatch::new_empty(rs.schema.clone()))?;
            let col_formats = fmts.for_set(&rs.meta, &normalized.schema())?;
            let styles = cells.formats(&col_formats);
            let set = SetFormulas::resolve(&rs.meta, fmts.output(), &normalized.schema())?;
            let (anchor_row, anchor_col) = match &rs.meta.anchor {
                Some(a) => parse_cell(a).ok_or_else(|| format!("invalid anchor `{a}`"))?,
                None => (0, 0),
            };
            let header = rs.meta.header.unwrap_or(header_default);
            let names: Vec<String> = rs.schema.fields().iter().map(|f| f.name().clone()).collect();
            let totals = set.has_totals();
            let room = (EXCEL_MAX_ROWS - anchor_row) as u64 - u64::from(header) - u64::from(totals);
            let cap = max_rows.min(room);
            if cap == 0 {
                return Err(format!(
                    "sheet `{}`: anchor {} leaves no room for rows",
                    rs.meta.name,
                    rs.meta.anchor.as_deref().unwrap_or("A1")
                )
                .into());
            }
            let col_of = |n: &str| {
                names
                    .iter()
                    .position(|x| x == n)
                    .map(|i| u32::from(anchor_col) + i as u32)
            };
            let first_row = anchor_row + u32::from(header);
            let totals_row = |cells: &mut CellWriter,
                              ws: &mut rust_xlsxwriter::Worksheet,
                              row: u32,
                              accs: &[Acc]|
             -> Result<()> {
                if !totals || row == first_row {
                    return Ok(());
                }
                for (c, (t, acc)) in set.totals.iter().zip(accs).enumerate() {
                    let col = anchor_col + c as u16;
                    let fmt = cells.totals_format(col_formats[c].code());
                    match t {
                        Some(Total::Function { agg, excel }) => {
                            let f = format!(
                                "={excel}({0}{1}:{0}{2})",
                                formulas::col_letters(u32::from(col)),
                                first_row + 1,
                                row
                            );
                            let f = rust_xlsxwriter::Formula::new(f).set_result(acc.result(*agg));
                            ws.write_formula_with_format(row, col, f, &fmt)?;
                        }
                        Some(Total::Formula(parts)) => {
                            let f = formulas::render(parts, col_of, row, (first_row, row - 1))
                                .map_err(|n| format!("column `{n}` isn't on the sheet"))?;
                            ws.write_formula_with_format(row, col, rust_xlsxwriter::Formula::new(f), &fmt)?;
                        }
                        None if c == 0 && !label.is_empty() => {
                            ws.write_string_with_format(row, col, &label, &cells.totals_format(None))?;
                        }
                        None => {
                            ws.write_blank(row, col, &fmt)?;
                        }
                    }
                }
                Ok(())
            };
            let base = rs.meta.name.clone();
            let mut part = 1u32;
            let mut ws = wb.add_worksheet_with_constant_memory();
            ws.set_name(&base)?;
            ws.set_formula_result_default("");
            let mut written: u64 = 0;
            let mut row = anchor_row;
            let mut accs = vec![Acc::default(); names.len()];
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
                        totals_row(&mut cells, ws, row, &accs)?;
                        accs = vec![Acc::default(); names.len()];
                        part += 1;
                        ws = wb.add_worksheet_with_constant_memory();
                        ws.set_name(continuation_name(&base, part))?;
                        ws.set_formula_result_default("");
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
                        let v = cells::excel_value(col.as_ref(), i, name);
                        let formula = match &set.row[c] {
                            Some(parts) => Some(
                                formulas::render(parts, col_of, row, (0, 0))
                                    .map_err(|n| format!("column `{n}` isn't on the sheet"))?,
                            ),
                            None => None,
                        };
                        // Totals see what the sheet holds: a row formula's cached result.
                        accs[c].add(&v);
                        let style = styles[c].as_ref();
                        cells.write(
                            ws,
                            row,
                            anchor_col + c as u16,
                            v,
                            col.data_type(),
                            name,
                            style,
                            formula.as_deref(),
                        )?;
                    }
                    row += 1;
                    written += 1;
                }
            }
            totals_row(&mut cells, ws, row, &accs)?;
        }
        fmts.finish()?;
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
