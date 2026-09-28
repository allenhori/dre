//! Row formulas and totals rows: `formula` and `total` in a `columns:` map, on plain sheets,
//! split sheets and template blocks.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{ArrayRef, Date32Array, Float64Array, Int64Array, RecordBatch, StringArray};
use calamine::{Data, Reader, Xlsx, open_workbook};
use dre_protocol::host::{LogSink, PluginProcess};
use dre_protocol::msg::{ColumnOptions, ResultSetMeta};
use serde_json::{Value, json};
use umya_spreadsheet::Workbook;

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-xlsx"))
}

/// `(column, key, value)` with key `format`, `formula` or `total`.
fn cols(entries: &[(&str, &str, &str)]) -> BTreeMap<String, ColumnOptions> {
    let mut out: BTreeMap<String, ColumnOptions> = BTreeMap::new();
    for (n, k, v) in entries {
        let c = out.entry(n.to_string()).or_default();
        let v = Some(v.to_string());
        match *k {
            "format" => c.format = v,
            "formula" => c.formula = v,
            "total" => c.total = v,
            _ => panic!("{k}"),
        }
    }
    out
}

fn meta(name: &str, entries: &[(&str, &str, &str)]) -> ResultSetMeta {
    ResultSetMeta {
        name: name.into(),
        query: name.to_lowercase(),
        result_index: 1,
        anchor: None,
        header: None,
        columns: cols(entries),
    }
}

fn try_write(
    options: Value,
    sets: Vec<(ResultSetMeta, RecordBatch)>,
    template: Option<Value>,
) -> (tempfile::TempDir, PathBuf, Result<Vec<String>, String>) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.xlsx");
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let run = || -> Result<Vec<String>, String> {
        p.write_begin(
            path.to_str().unwrap(),
            "xlsx",
            options.as_object().unwrap().clone(),
            sets.iter().map(|s| s.0.clone()).collect(),
            template,
        )
        .map_err(|e| e.to_string())?;
        for (_, b) in &sets {
            p.write_result_set(&b.schema(), vec![b.clone()])
                .map_err(|e| e.to_string())?;
        }
        p.write_finish_with_warnings()
            .map(|(_, w)| w)
            .map_err(|e| e.to_string())
    };
    let r = run();
    (dir, path, r)
}

fn write(options: Value, sets: Vec<(ResultSetMeta, RecordBatch)>) -> (tempfile::TempDir, PathBuf) {
    let (dir, path, r) = try_write(options, sets, None);
    r.unwrap();
    (dir, path)
}

fn book(path: &Path) -> Workbook {
    umya_spreadsheet::reader::xlsx::read(path).unwrap()
}

fn formula(b: &Workbook, sheet: &str, cell: &str) -> String {
    b.sheet_by_name(sheet)
        .unwrap()
        .cell(cell)
        .map(|c| c.formula().to_string())
        .unwrap_or_default()
}

fn values(path: &Path, sheet: &str) -> Vec<Vec<Data>> {
    let mut wb: Xlsx<_> = open_workbook(path).unwrap();
    let r = wb.worksheet_range(sheet).unwrap();
    r.rows().map(|row| row.to_vec()).collect()
}

/// Every formula on a sheet, by cell, as calamine reads them.
fn formulas(path: &Path, sheet: &str) -> Vec<Vec<String>> {
    let mut wb: Xlsx<_> = open_workbook(path).unwrap();
    let r = wb.worksheet_formula(sheet).unwrap();
    // The range starts at the first formula; pad it back to A1.
    let Some((r0, c0)) = r.start() else {
        return Vec::new();
    };
    let (r1, c1) = r.end().unwrap();
    (0..=r1)
        .map(|row| {
            (0..=c1)
                .map(|col| {
                    if row < r0 || col < c0 {
                        String::new()
                    } else {
                        r.get_value((row, col)).cloned().unwrap_or_default()
                    }
                })
                .collect()
        })
        .collect()
}

/// region, qty, price, line_total (the placeholder: qty * price, one null).
fn lines() -> RecordBatch {
    RecordBatch::try_from_iter([
        (
            "region",
            Arc::new(StringArray::from(vec!["North", "South", "=SUM(A1:A2)"])) as ArrayRef,
        ),
        ("qty", Arc::new(Int64Array::from(vec![2, 3, 4])) as ArrayRef),
        (
            "price",
            Arc::new(Float64Array::from(vec![1.5, 2.0, 10.0])) as ArrayRef,
        ),
        (
            "line_total",
            Arc::new(Float64Array::from(vec![Some(3.0), Some(6.0), None])) as ArrayRef,
        ),
    ])
    .unwrap()
}

#[test]
fn a_row_formula_refers_to_cells_on_its_own_row_and_keeps_the_placeholder_as_its_result() {
    let (_d, path) = write(
        json!({}),
        vec![(
            meta(
                "Lines",
                &[
                    ("line_total", "formula", "={qty}*{price}"),
                    ("line_total", "format", "#,##0.00"),
                ],
            ),
            lines(),
        )],
    );
    let b = book(&path);
    assert_eq!(formula(&b, "Lines", "D2"), "B2*C2");
    assert_eq!(formula(&b, "Lines", "D4"), "B4*C4");
    let v = values(&path, "Lines");
    assert_eq!(v[1][3], Data::Float(3.0));
    assert_eq!(v[2][3], Data::Float(6.0));
    // A null placeholder has no cached result: Excel works it out on opening.
    assert!(
        matches!(&v[3][3], Data::Empty) || v[3][3] == Data::String(String::new()),
        "{:?}",
        v[3][3]
    );
    // Data text is never a formula.
    assert_eq!(v[3][0], Data::String("=SUM(A1:A2)".into()));
    assert_eq!(formula(&b, "Lines", "A4"), "");
    // The format applies to the formula's result.
    let nf = b
        .sheet_by_name("Lines")
        .unwrap()
        .style("D2")
        .number_format()
        .map(|n| n.format_code().to_string());
    assert_eq!(nf.as_deref(), Some("#,##0.00"));
}

#[test]
fn references_follow_anchor_and_header_and_output_level_formulas_apply_on_every_sheet() {
    let mut m = meta("Lines", &[]);
    m.anchor = Some("B4".into());
    m.header = Some(false);
    let (_d, path) = write(
        json!({"columns": {"line_total": {"formula": "={qty}*{price}*$H$1"}}}),
        vec![(m, lines()), (meta("Again", &[]), lines())],
    );
    let b = book(&path);
    assert_eq!(formula(&b, "Lines", "E4"), "C4*D4*$H$1");
    assert_eq!(formula(&b, "Lines", "E5"), "C5*D5*$H$1");
    assert_eq!(formula(&b, "Again", "D2"), "B2*C2*$H$1");
}

#[test]
fn a_query_entry_formula_wins_over_the_output_level_one() {
    let (_d, path) = write(
        json!({"columns": {"line_total": {"formula": "={qty}*{price}"}}}),
        vec![(
            meta("Lines", &[("line_total", "formula", "={qty}+{price}")]),
            lines(),
        )],
    );
    assert_eq!(formula(&book(&path), "Lines", "D2"), "B2+C2");
}

#[test]
fn row_formulas_continue_on_split_sheets() {
    let (_d, path) = write(
        json!({"max_rows_per_sheet": 2}),
        vec![(
            meta("Lines", &[("line_total", "formula", "={qty}*{price}")]),
            lines(),
        )],
    );
    let b = book(&path);
    assert_eq!(formula(&b, "Lines", "D3"), "B3*C3");
    assert_eq!(formula(&b, "Lines (2)", "D2"), "B2*C2");
}

#[test]
fn a_totals_row_sums_under_the_data_with_cached_results_and_a_label() {
    let (_d, path) = write(
        json!({}),
        vec![(
            meta(
                "Lines",
                &[
                    ("qty", "total", "sum"),
                    ("price", "total", "average"),
                    ("price", "format", "0.00"),
                    ("line_total", "total", "=SUM({line_total:*})/SUM({qty:*})"),
                ],
            ),
            lines(),
        )],
    );
    let f = formulas(&path, "Lines");
    assert_eq!(f[4][1], "SUM(B2:B4)");
    assert_eq!(f[4][2], "AVERAGE(C2:C4)");
    assert_eq!(f[4][3], "SUM(D2:D4)/SUM(B2:B4)");
    let v = values(&path, "Lines");
    assert_eq!(v[4][0], Data::String("Total".into()));
    assert_eq!(v[4][1], Data::Float(9.0));
    assert!(matches!(v[4][2], Data::Float(x) if (x - 13.5 / 3.0).abs() < 1e-9));
    let b = book(&path);
    let ws = b.sheet_by_name("Lines").unwrap();
    assert!(ws.style("B5").font().is_some_and(|f| f.bold()));
    assert!(ws.style("A5").font().is_some_and(|f| f.bold()));
    assert_eq!(
        ws.style("C5")
            .number_format()
            .map(|n| n.format_code().to_string())
            .as_deref(),
        Some("0.00")
    );
}

#[test]
fn count_min_max_and_a_custom_label() {
    let b = RecordBatch::try_from_iter([
        (
            "region",
            Arc::new(StringArray::from(vec![Some("a"), None, Some("c")])) as ArrayRef,
        ),
        (
            "day",
            Arc::new(Date32Array::from(vec![20478, 20400, 20500])) as ArrayRef,
        ),
        ("n", Arc::new(Int64Array::from(vec![5, -2, 7])) as ArrayRef),
    ])
    .unwrap();
    let (_d, path) = write(
        json!({"totals_label": "All"}),
        vec![(
            meta(
                "T",
                &[
                    ("region", "total", "count"),
                    ("day", "total", "max"),
                    ("n", "total", "min"),
                ],
            ),
            b,
        )],
    );
    let f = formulas(&path, "T");
    assert_eq!(f[4], vec!["COUNTA(A2:A4)", "MAX(B2:B4)", "MIN(C2:C4)"]);
    let v = values(&path, "T");
    // The first column has a total, so there's no label.
    assert_eq!(v[4][0], Data::Float(2.0));
    assert_eq!(v[4][2], Data::Float(-2.0));
    let b = book(&path);
    assert_eq!(
        b.sheet_by_name("T")
            .unwrap()
            .style("B5")
            .number_format()
            .map(|n| n.format_code().to_string())
            .as_deref(),
        Some("yyyy-mm-dd")
    );
}

#[test]
fn each_split_sheet_gets_a_totals_row_over_its_own_rows() {
    let (_d, path) = write(
        json!({"max_rows_per_sheet": 2}),
        vec![(meta("Lines", &[("qty", "total", "sum")]), lines())],
    );
    let v1 = values(&path, "Lines");
    assert_eq!(v1.len(), 4);
    assert_eq!(v1[3][1], Data::Float(5.0));
    assert_eq!(formulas(&path, "Lines")[3][1], "SUM(B2:B3)");
    let v2 = values(&path, "Lines (2)");
    assert_eq!(v2[2][0], Data::String("Total".into()));
    assert_eq!(v2[2][1], Data::Float(4.0));
}

#[test]
fn totals_follow_the_anchor_and_an_empty_set_gets_no_totals_row() {
    let mut m = meta("Lines", &[("qty", "total", "sum")]);
    m.anchor = Some("C3".into());
    let empty = lines().slice(0, 0);
    let (_d, path) = write(
        json!({}),
        vec![(m, lines()), (meta("Empty", &[("qty", "total", "sum")]), empty)],
    );
    assert_eq!(formulas(&path, "Lines")[6][3], "SUM(D4:D6)");
    assert_eq!(values(&path, "Empty").len(), 1);
}

#[test]
fn unknown_references_and_totals_that_dont_fit_are_errors() {
    let (_d, _p, r) = try_write(
        json!({}),
        vec![(
            meta("Lines", &[("line_total", "formula", "={qty}*{prce}")]),
            lines(),
        )],
        None,
    );
    let e = r.unwrap_err();
    assert!(
        e.contains(
            "refers to `prce`, which query `lines` doesn't return (it has: region, qty, price, line_total)"
        ),
        "{e}"
    );
    let (_d, _p, r) = try_write(
        json!({}),
        vec![(meta("Lines", &[("region", "total", "sum")]), lines())],
        None,
    );
    let e = r.unwrap_err();
    assert!(
        e.contains("column `region` is a text or boolean column, which `total: sum` can't total"),
        "{e}"
    );
    let (_d, _p, r) = try_write(
        json!({}),
        vec![(meta("Lines", &[("nope", "formula", "={qty}")]), lines())],
        None,
    );
    assert!(r.unwrap_err().contains("doesn't return"));
}

#[test]
fn bad_options_are_reported_by_validate() {
    let (_d, _p, r) = try_write(
        json!({"columns": {"x": {"formula": "qty*2"}, "y": {"total": "median"}, "z": {"total": "={a}"}}}),
        vec![(meta("Lines", &[]), lines())],
        None,
    );
    let e = r.unwrap_err();
    assert!(
        e.contains("column `x`: formula `qty*2` must start with `=`"),
        "{e}"
    );
    assert!(e.contains("column `y`: total `median` must be one of"), "{e}");
    assert!(e.contains("column `z`: total `={a}` uses `{a}`"), "{e}");
}

// -- templates ------------------------------------------------------------------------------------

fn template() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/branded.xlsx")
}

/// a, b, c for the template's block at `Summary!A5` (three rows, so two are inserted).
fn block() -> RecordBatch {
    RecordBatch::try_from_iter([
        ("a", Arc::new(Int64Array::from(vec![1, 2, 3])) as ArrayRef),
        ("b", Arc::new(StringArray::from(vec!["x", "y", "z"])) as ArrayRef),
        (
            "c",
            Arc::new(Float64Array::from(vec![10.0, 20.0, 30.0])) as ArrayRef,
        ),
    ])
    .unwrap()
}

fn fill(m: ResultSetMeta, columns: &[&str]) -> (tempfile::TempDir, PathBuf, Result<Vec<String>, String>) {
    let payload = json!({
        "file": template().to_str().unwrap(),
        "bindings": [{"query": m.query, "sheet": "Summary", "anchor": "A5", "header": false, "columns": columns}],
    });
    try_write(json!({}), vec![(m, block())], Some(payload))
}

#[test]
fn row_formulas_in_a_template_block_use_the_blocks_columns() {
    let (_d, path, r) = fill(meta("Summary", &[("c", "formula", "={a}*10")]), &["a", "b", "c"]);
    r.unwrap();
    let b = book(&path);
    assert_eq!(formula(&b, "Summary", "C5"), "A5*10");
    assert_eq!(formula(&b, "Summary", "C7"), "A7*10");
    let v = values(&path, "Summary");
    let row7 = v.iter().find(|r| r.contains(&Data::String("z".into()))).unwrap();
    assert!(row7.contains(&Data::Float(30.0)), "{row7:?}");
}

#[test]
fn a_template_block_formula_needs_its_columns_placed() {
    let (_d, _p, r) = fill(meta("Summary", &[("c", "formula", "={b}&{a}")]), &["b", "c"]);
    let e = r.unwrap_err();
    assert!(
        e.contains("column `c`'s formula refers to `a`, which the block doesn't place (it places: b, c)"),
        "{e}"
    );
}

#[test]
fn totals_with_a_template_are_an_error() {
    let (_d, _p, r) = fill(meta("Summary", &[("c", "total", "sum")]), &["a", "b", "c"]);
    let e = r.unwrap_err();
    assert!(
        e.contains("column `c` has a `total`, which templates don't take"),
        "{e}"
    );
}
