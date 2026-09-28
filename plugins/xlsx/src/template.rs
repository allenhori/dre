//! Filling a branded template authored in Excel.
//!
//! Single cells are written first, in template coordinates. Table blocks are then filled bottom
//! to top per sheet, so inserting rows for one block never moves an anchor not yet filled.
//!
//! A table block owns one template row (its first data row). For `n` rows DRE inserts `n - 1`
//! rows after it: content below shifts down, the reserved row's formatting is copied to each new
//! row, and formulas whose range ends on the reserved row (a totals `SUM(C5:C5)`) are extended
//! over the new rows. Ranges spanning the insertion point are adjusted by the insert itself.

use std::collections::HashMap;
use std::path::Path;
use std::sync::LazyLock;

use arrow::array::{Array, RecordBatch};
use dre_protocol::plugin::{Result, ResultSets, WriteRequest};
use regex::Regex;
use serde::Deserialize;
use serde_json::Value;
use umya_spreadsheet::{Workbook as Spreadsheet, Worksheet};

use crate::EXCEL_MAX_ROWS;
use crate::cells::{Excel, excel_value, normalize, parse_cell};
use crate::formats::{ColumnFormat, Formats, Kind};

#[derive(Debug, Deserialize)]
struct Binding {
    sheet: String,
    query: Option<String>,
    result_index: Option<usize>,
    anchor: Option<String>,
    header: Option<bool>,
    columns: Option<Vec<String>>,
    cell: Option<String>,
    value: Option<String>,
    column: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Payload {
    file: String,
    #[serde(default)]
    bindings: Vec<Binding>,
    /// `"Sheet!B2"` → rendered value, for single-cell `value` bindings.
    #[serde(default)]
    values: HashMap<String, String>,
}

struct Collected {
    query: String,
    index: usize,
    name: String,
    names: Vec<String>,
    batch: RecordBatch,
    formats: Vec<ColumnFormat>,
}

pub fn fill(req: &WriteRequest, sets: &mut ResultSets<'_>) -> Result<Vec<String>> {
    let payload: Payload = serde_json::from_value(req.template.clone().unwrap_or(Value::Null))
        .map_err(|e| format!("invalid template payload: {e}"))?;
    let mut book = umya_spreadsheet::reader::xlsx::read(Path::new(&payload.file))
        .map_err(|e| format!("can't open template {}: {e:?}", payload.file))?;

    // Template filling needs every row in hand to know how many rows to insert.
    let mut fmts = Formats::new(&req.options)?;
    let mut results = Vec::new();
    while let Some(mut rs) = sets.next_set()? {
        let mut batches = Vec::new();
        while let Some(b) = rs.next_batch()? {
            batches.push(normalize(&b)?);
        }
        let schema = match batches.first() {
            Some(b) => b.schema(),
            None => normalize(&RecordBatch::new_empty(rs.schema.clone()))?.schema(),
        };
        let batch = arrow::compute::concat_batches(&schema, &batches)?;
        let formats = fmts.for_set(&rs.meta, &schema)?;
        results.push(Collected {
            query: rs.meta.query.clone(),
            index: rs.meta.result_index,
            name: rs.meta.name.clone(),
            names: schema.fields().iter().map(|f| f.name().clone()).collect(),
            batch,
            formats,
        });
    }
    fmts.finish()?;
    let find = |b: &Binding| -> Result<usize> {
        let q = b.query.as_deref().unwrap_or_default();
        let idx = b.result_index.unwrap_or(1);
        results
            .iter()
            .position(|r| r.query == q && r.index == idx)
            .ok_or_else(|| format!("template binding on `{}` uses query `{q}` result {idx}, which didn't return a result set", b.sheet).into())
    };
    let mut bound = vec![false; results.len()];

    // 1. Single cells, in template coordinates.
    for b in payload.bindings.iter().filter(|b| b.cell.is_some()) {
        let cell = b.cell.as_deref().unwrap();
        let ws = sheet(&mut book, &b.sheet)?;
        let (r, c) = parse_cell(cell).ok_or_else(|| format!("invalid cell `{cell}`"))?;
        let coord = (u32::from(c) + 1, r + 1);
        if b.value.is_some() {
            let v = payload
                .values
                .get(&format!("{}!{cell}", b.sheet))
                .cloned()
                .unwrap_or_default();
            ws.cell_mut(coord).set_value(v);
        } else {
            let i = find(b)?;
            bound[i] = true;
            let res = &results[i];
            let col = b.column.as_deref().unwrap_or_default();
            let ci = res.names.iter().position(|n| n == col).ok_or_else(|| {
                format!(
                    "`{}` has no column `{col}` (it has: {})",
                    res.query,
                    res.names.join(", ")
                )
            })?;
            match res.batch.num_rows() {
                1 => {}
                n => {
                    return Err(format!(
                        "the single-cell binding {}!{cell} reads one row of `{}`, but it returned {n} rows",
                        b.sheet, res.query
                    )
                    .into());
                }
            }
            write_value(ws, coord, res.batch.column(ci).as_ref(), 0, col, &res.formats[ci]);
        }
    }

    // 2. Table blocks, bottom to top within each sheet.
    let mut blocks: Vec<(&Binding, u32, u16)> = Vec::new();
    for b in payload.bindings.iter().filter(|b| b.cell.is_none()) {
        let a = b.anchor.as_deref().unwrap_or("A1");
        let (r, c) = parse_cell(a).ok_or_else(|| format!("invalid anchor `{a}`"))?;
        blocks.push((b, r, c));
    }
    blocks.sort_by(|x, y| (x.0.sheet.as_str(), y.1).cmp(&(y.0.sheet.as_str(), x.1)));
    for (b, r0, c0) in blocks {
        let i = find(b)?;
        bound[i] = true;
        fill_block(&mut book, b, r0, c0, &results[i])?;
    }

    // 3. Result sets not bound anywhere become plain sheets after the template's own, continued
    //    on `Name (2)`, ... past `max_rows_per_sheet`, like plain xlsx output.
    let header = req.options.get("header").and_then(Value::as_bool).unwrap_or(true);
    let max_rows = req
        .options
        .get("max_rows_per_sheet")
        .and_then(Value::as_u64)
        .unwrap_or(1_000_000) as usize;
    for (res, _) in results.iter().zip(&bound).filter(|(_, b)| !**b) {
        let total = res.batch.num_rows();
        for part in 0..total.div_ceil(max_rows).max(1) {
            let name = if part == 0 {
                res.name.clone()
            } else {
                crate::continuation_name(&res.name, part as u32 + 1)
            };
            let ws = book
                .new_sheet(&name)
                .map_err(|e| format!("can't add sheet `{name}`: {e}"))?;
            if header {
                for (c, n) in res.names.iter().enumerate() {
                    let cell = ws.cell_mut((c as u32 + 1, 1));
                    cell.set_value(n.clone());
                    cell.style_mut().font_mut().set_bold(true);
                }
            }
            let first = u32::from(header) + 1;
            for (k, row) in (part * max_rows..((part + 1) * max_rows).min(total)).enumerate() {
                for c in 0..res.names.len() {
                    write_value(
                        ws,
                        (c as u32 + 1, first + k as u32),
                        res.batch.column(c).as_ref(),
                        row,
                        &res.names[c],
                        &res.formats[c],
                    );
                }
            }
        }
    }

    umya_spreadsheet::writer::xlsx::write(&book, Path::new(&req.path))
        .map_err(|e| format!("can't save {}: {e:?}", req.path))?;
    Ok(vec![req.path.clone()])
}

fn sheet<'a>(book: &'a mut Spreadsheet, name: &str) -> Result<&'a mut Worksheet> {
    let names: Vec<String> = book
        .sheet_collection()
        .iter()
        .map(|s| s.name().to_string())
        .collect();
    book.sheet_by_name_mut(name).map_err(|_| {
        format!(
            "the template has no sheet `{name}` (sheets: {})",
            names.join(", ")
        )
        .into()
    })
}

fn fill_block(book: &mut Spreadsheet, b: &Binding, r0: u32, c0: u16, res: &Collected) -> Result<()> {
    let header = b.header.unwrap_or(true);
    let cols: Vec<usize> = match &b.columns {
        Some(list) => list
            .iter()
            .map(|c| {
                res.names.iter().position(|n| n == c).ok_or_else(|| {
                    format!(
                        "template block on `{}`: `{}` has no column `{c}` (it has: {})",
                        b.sheet,
                        res.query,
                        res.names.join(", ")
                    )
                    .into()
                })
            })
            .collect::<Result<_>>()?,
        None => (0..res.names.len()).collect(),
    };
    let n = res.batch.num_rows() as u32;
    // 1-based rows from here on.
    let first = r0 + 1 + u32::from(header);
    if n > 0 && first + n - 1 > EXCEL_MAX_ROWS {
        return Err(format!(
            "template block on `{}` at {}: {n} rows don't fit on the sheet (template blocks never split)",
            b.sheet,
            b.anchor.as_deref().unwrap_or("A1")
        )
        .into());
    }
    let sheet_name = b.sheet.clone();
    {
        let ws = sheet(book, &b.sheet)?;
        if n > 1 {
            ws.insert_new_row(first + 1, n - 1);
            // Copy the reserved row's formatting (every styled cell in it) onto the new rows.
            let styled: Vec<(u32, umya_spreadsheet::Style)> = ws
                .collection_by_row(first)
                .into_iter()
                .map(|c| (c.coordinate().col_num(), c.style().clone()))
                .collect();
            for row in first + 1..first + n {
                for (col, style) in &styled {
                    ws.cell_mut((*col, row)).set_style(style.clone());
                }
            }
        }
        if header {
            for (k, &ci) in cols.iter().enumerate() {
                ws.cell_mut((u32::from(c0) + 1 + k as u32, r0 + 1))
                    .set_value(res.names[ci].clone());
            }
        }
        if n == 0 {
            for k in 0..cols.len() {
                ws.cell_mut((u32::from(c0) + 1 + k as u32, first))
                    .set_value(String::new());
            }
        }
        for row in 0..n as usize {
            for (k, &ci) in cols.iter().enumerate() {
                write_value(
                    ws,
                    (u32::from(c0) + 1 + k as u32, first + row as u32),
                    res.batch.column(ci).as_ref(),
                    row,
                    &res.names[ci],
                    &res.formats[ci],
                );
            }
        }
    }
    if n > 1 {
        extend_formulas(book, &sheet_name, first, first + n - 1);
    }
    Ok(())
}

static RANGE: LazyLock<Regex> = LazyLock::new(|| {
    Regex::new(r"(?:(?P<sheet>'(?:[^']|'')+'|[A-Za-z_][\w.]*)!)?(?P<c1>\$?[A-Z]{1,3})(?P<a1>\$?)(?P<r1>\d+):(?P<c2>\$?[A-Z]{1,3})(?P<a2>\$?)(?P<r2>\d+)").unwrap()
});

/// Extend every range ending on the block's reserved row `reserved` (and starting at or above
/// it) down to `last`, in formulas on the block's sheet and in other sheets referring to it.
fn extend_formulas(book: &mut Spreadsheet, block_sheet: &str, reserved: u32, last: u32) {
    for ws in book.sheet_collection_mut() {
        let same = ws.name() == block_sheet;
        let cells: Vec<(u32, u32, String)> = ws
            .cells()
            .into_iter()
            .filter(|c| !c.formula().is_empty())
            .map(|c| {
                (
                    c.coordinate().col_num(),
                    c.coordinate().row_num(),
                    c.formula().to_string(),
                )
            })
            .collect();
        for (col, row, f) in cells {
            let new = RANGE.replace_all(&f, |caps: &regex::Captures<'_>| {
                let whole = caps[0].to_string();
                let refers = match caps.name("sheet") {
                    Some(s) => s.as_str().trim_matches('\'').replace("''", "'") == block_sheet,
                    None => same,
                };
                let (r1, r2): (u32, u32) = (caps["r1"].parse().unwrap_or(0), caps["r2"].parse().unwrap_or(0));
                if !refers || r2 != reserved || r1 > reserved {
                    return whole;
                }
                let prefix = caps
                    .name("sheet")
                    .map(|s| format!("{}!", s.as_str()))
                    .unwrap_or_default();
                format!(
                    "{prefix}{}{}{r1}:{}{}{last}",
                    &caps["c1"], &caps["a1"], &caps["c2"], &caps["a2"]
                )
            });
            if new != f {
                ws.cell_mut((col, row)).set_formula(new.to_string());
            }
        }
    }
}

/// Write one value. An explicit YAML format replaces the cell's number format (keeping its font,
/// fill and border); a type default only fills a `General` cell, so a template's own format wins.
fn write_value(
    ws: &mut Worksheet,
    coord: (u32, u32),
    a: &dyn Array,
    i: usize,
    column: &str,
    fmt: &ColumnFormat,
) {
    let cell = ws.cell_mut(coord);
    let apply = |cell: &mut umya_spreadsheet::Cell| {
        let Some(code) = fmt.code() else { return };
        let nf = cell.style_mut().number_format_mut();
        if fmt.is_explicit() || nf.format_code() == "General" {
            nf.set_format_code(code);
        }
    };
    match excel_value(a, i, column) {
        None => {
            cell.set_value(String::new());
        }
        Some(Excel::Number(n) | Excel::Date(n) | Excel::DateTime(n) | Excel::Time(n)) => {
            cell.set_value_number(n);
            apply(cell);
        }
        Some(Excel::Bool(b)) => {
            cell.set_value_bool(b);
            apply(cell);
        }
        Some(Excel::Text(t)) => {
            cell.set_value(t);
            if Kind::of(a.data_type()) == Kind::Other {
                apply(cell);
            } else if fmt.is_explicit() {
                // A number or date Excel can't hold stays unformatted text.
                crate::cells::unformatted(column);
            }
        }
    }
}
