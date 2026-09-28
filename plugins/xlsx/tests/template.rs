//! Filling a branded template: values, row insertion, formatting, formulas, extra sheets.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{ArrayRef, Float64Array, Int64Array, RecordBatch, StringArray};
use dre_protocol::host::{LogSink, PluginProcess};
use dre_protocol::msg::ResultSetMeta;
use serde_json::{Value, json};
use umya_spreadsheet::Workbook;

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-xlsx"))
}

fn template() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/branded.xlsx")
}

fn meta(query: &str, name: &str) -> ResultSetMeta {
    ResultSetMeta {
        name: name.into(),
        query: query.into(),
        result_index: 1,
        anchor: None,
        header: None,
    }
}

/// Accounts with columns deliberately in a different order from the template's.
fn accounts(n: usize) -> RecordBatch {
    RecordBatch::try_from_iter([
        (
            "balance",
            Arc::new(Float64Array::from_iter_values(
                (0..n).map(|i| 100.0 * (i + 1) as f64),
            )) as ArrayRef,
        ),
        (
            "account_name",
            Arc::new(StringArray::from_iter_values(
                (0..n).map(|i| format!("Client {i}")),
            )) as ArrayRef,
        ),
        (
            "account_id",
            Arc::new(Int64Array::from_iter_values(0..n as i64)) as ArrayRef,
        ),
    ])
    .unwrap()
}

fn count(n: i64) -> RecordBatch {
    RecordBatch::try_from_iter([("n", Arc::new(Int64Array::from(vec![n; 1])) as ArrayRef)]).unwrap()
}

fn fill(
    sets: Vec<(ResultSetMeta, RecordBatch)>,
    bindings: Value,
) -> Result<(tempfile::TempDir, Workbook), String> {
    fill_with(sets, bindings, json!({}))
}

fn fill_with(
    sets: Vec<(ResultSetMeta, RecordBatch)>,
    bindings: Value,
    options: Value,
) -> Result<(tempfile::TempDir, Workbook), String> {
    let dir = tempfile::tempdir().unwrap();
    let out = dir.path().join("out.xlsx");
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let payload = json!({
        "file": template().to_str().unwrap(),
        "bindings": bindings,
        "values": {"Summary!B2": "2026-01-25"},
    });
    p.write_begin(
        out.to_str().unwrap(),
        "xlsx",
        options.as_object().unwrap().clone(),
        sets.iter().map(|s| s.0.clone()).collect(),
        Some(payload),
    )
    .unwrap();
    for (_, b) in sets {
        p.write_result_set(&b.schema(), vec![b]).unwrap();
    }
    p.write_finish().map_err(|e| e.to_string())?;
    let book = umya_spreadsheet::reader::xlsx::read(&out).unwrap();
    Ok((dir, book))
}

fn standard_bindings() -> Value {
    json!([
        {"query": "accounts", "sheet": "Summary", "anchor": "A5", "header": false, "columns": ["account_id", "account_name", "balance"]},
        {"sheet": "Summary", "cell": "B2", "value": "{{ run.date.iso }}"},
        {"sheet": "Detail", "cell": "B1", "query": "count_q", "column": "n"},
    ])
}

fn value(book: &Workbook, sheet: &str, cell: &str) -> String {
    book.sheet_by_name(sheet).unwrap().value(cell)
}

fn formula(book: &Workbook, sheet: &str, cell: &str) -> String {
    book.sheet_by_name(sheet)
        .unwrap()
        .cell(cell)
        .map(|c| c.formula().to_string())
        .unwrap_or_default()
}

#[test]
fn table_blocks_insert_rows_map_columns_by_name_and_extend_totals() {
    let (_d, book) = fill(
        vec![
            (meta("accounts", "Accounts"), accounts(3)),
            (meta("count_q", "Count"), count(3)),
        ],
        standard_bindings(),
    )
    .unwrap();
    // Columns by name, in the order the binding lists them, whatever the SELECT order was.
    assert_eq!(
        (
            value(&book, "Summary", "A5"),
            value(&book, "Summary", "B5"),
            value(&book, "Summary", "C5")
        ),
        ("0".into(), "Client 0".into(), "100".into())
    );
    assert_eq!(value(&book, "Summary", "B7"), "Client 2");
    // Content below shifted down by two rows; the totals formula covers every data row.
    assert_eq!(value(&book, "Summary", "B8"), "Total");
    assert_eq!(formula(&book, "Summary", "C8"), "SUM(C5:C7)");
    assert_eq!(
        value(&book, "Summary", "A10"),
        "Confidential: prepared for client use"
    );
    // A formula on another sheet referring to the block is extended too.
    assert_eq!(formula(&book, "Notes", "B1"), "SUM(Summary!C5:C7)");
    // Single cells: a rendered value and a one-row query.
    assert_eq!(value(&book, "Summary", "B2"), "2026-01-25");
    assert_eq!(value(&book, "Detail", "B1"), "3");
}

#[test]
fn formatting_merges_and_images_survive_and_new_rows_copy_the_reserved_row() {
    let (_d, book) = fill(
        vec![
            (meta("accounts", "Accounts"), accounts(3)),
            (meta("count_q", "Count"), count(3)),
        ],
        standard_bindings(),
    )
    .unwrap();
    let s = book.sheet_by_name("Summary").unwrap();
    let fill_of = |c: &str| {
        s.style(c)
            .fill()
            .and_then(|f| f.pattern_fill())
            .and_then(|p| p.foreground_color())
            .map(|c| {
                let a = c.argb();
                format!("{:02X}{:02X}{:02X}", a.r, a.g, a.b)
            })
    };
    assert_eq!(
        fill_of("A5"),
        fill_of("A7"),
        "new rows copy the reserved row's fill"
    );
    assert!(
        fill_of("A7").is_some_and(|c| c.ends_with("FFF2CC")),
        "{:?}",
        fill_of("A7")
    );
    assert_eq!(
        s.style("C7")
            .number_format()
            .map(|n| n.format_code().to_string())
            .as_deref(),
        Some("#,##0.00")
    );
    assert_eq!(
        s.style("A4").font().map(|f| f.bold()),
        Some(true),
        "the template's header styling is untouched"
    );
    assert_eq!(
        s.merge_cells().iter().map(|r| r.range()).collect::<Vec<_>>(),
        ["A1:C1"]
    );
    assert_eq!(s.image_collection().len(), 1, "the logo is kept");
}

#[test]
fn unbound_result_sets_become_plain_sheets_after_the_templates_own() {
    let extra = RecordBatch::try_from_iter([("x", Arc::new(Int64Array::from(vec![7])) as ArrayRef)]).unwrap();
    let (_d, book) = fill(
        vec![
            (meta("accounts", "Accounts"), accounts(1)),
            (meta("count_q", "Count"), count(1)),
            (meta("extra_q", "Extra"), extra),
        ],
        standard_bindings(),
    )
    .unwrap();
    let names: Vec<&str> = book.sheet_collection().iter().map(|s| s.name()).collect();
    assert_eq!(names, ["Summary", "Detail", "Notes", "Extra"]);
    assert_eq!(
        (value(&book, "Extra", "A1"), value(&book, "Extra", "A2")),
        ("x".into(), "7".into())
    );
}

#[test]
fn an_empty_result_leaves_the_reserved_row_blank_and_totals_alone() {
    let (_d, book) = fill(
        vec![
            (meta("accounts", "Accounts"), accounts(0)),
            (meta("count_q", "Count"), count(0)),
        ],
        standard_bindings(),
    )
    .unwrap();
    assert_eq!(value(&book, "Summary", "A5"), "");
    assert_eq!(formula(&book, "Summary", "C6"), "SUM(C5:C5)");
}

#[test]
fn a_single_cell_query_must_return_exactly_one_row() {
    let two =
        RecordBatch::try_from_iter([("n", Arc::new(Int64Array::from(vec![1, 2])) as ArrayRef)]).unwrap();
    let err = fill(
        vec![
            (meta("accounts", "Accounts"), accounts(1)),
            (meta("count_q", "Count"), two),
        ],
        standard_bindings(),
    )
    .unwrap_err();
    assert!(
        err.contains("Detail!B1") && err.contains("returned 2 rows"),
        "{err}"
    );
}

#[test]
fn a_block_that_would_overflow_the_sheet_is_an_error() {
    let err = fill(
        vec![(meta("accounts", "Accounts"), accounts(3))],
        json!([{"query": "accounts", "sheet": "Detail", "anchor": "A1048575", "header": false}]),
    )
    .unwrap_err();
    assert!(err.contains("don't fit on the sheet"), "{err}");
}

#[test]
fn a_large_unbound_result_set_continues_on_numbered_sheets() {
    let many = RecordBatch::try_from_iter([("x", Arc::new(Int64Array::from_iter_values(0..5)) as ArrayRef)])
        .unwrap();
    let (_d, book) = fill_with(
        vec![
            (meta("accounts", "Accounts"), accounts(1)),
            (meta("count_q", "Count"), count(1)),
            (meta("extra_q", "Extra"), many),
        ],
        standard_bindings(),
        json!({"max_rows_per_sheet": 2}),
    )
    .unwrap();
    let names: Vec<&str> = book.sheet_collection().iter().map(|s| s.name()).collect();
    assert_eq!(
        names,
        ["Summary", "Detail", "Notes", "Extra", "Extra (2)", "Extra (3)"]
    );
    assert_eq!(
        (value(&book, "Extra (3)", "A1"), value(&book, "Extra (3)", "A2")),
        ("x".into(), "4".into())
    );
}
