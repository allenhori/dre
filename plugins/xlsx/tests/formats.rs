//! Number formats per column: `columns:` on a query entry and at output level, the date, timestamp
//! and time defaults, and how they meet a template's own formats.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanArray, Date32Array, Float64Array, Int64Array, RecordBatch, StringArray,
    Time64MicrosecondArray, TimestampMicrosecondArray,
};
use calamine::{Data, Reader, Xlsx, open_workbook};
use dre_protocol::host::{LogSink, PluginProcess};
use dre_protocol::msg::{ColumnOptions, ResultSetMeta};
use serde_json::{Value, json};
use umya_spreadsheet::Workbook;

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-xlsx"))
}

fn cols(formats: &[(&str, &str)]) -> BTreeMap<String, ColumnOptions> {
    formats
        .iter()
        .map(|(n, f)| {
            (
                n.to_string(),
                ColumnOptions {
                    format: Some(f.to_string()),
                    ..Default::default()
                },
            )
        })
        .collect()
}

fn meta(name: &str, formats: &[(&str, &str)]) -> ResultSetMeta {
    ResultSetMeta {
        name: name.into(),
        query: name.to_lowercase(),
        result_index: 1,
        anchor: None,
        header: None,
        columns: cols(formats),
    }
}

/// Write the sets; the file and warnings, or the plugin's error.
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

fn write(
    options: Value,
    sets: Vec<(ResultSetMeta, RecordBatch)>,
) -> (tempfile::TempDir, Workbook, Vec<String>) {
    let (dir, path, r) = try_write(options, sets, None);
    let warnings = r.unwrap();
    (
        dir,
        umya_spreadsheet::reader::xlsx::read(&path).unwrap(),
        warnings,
    )
}

fn numfmt(book: &Workbook, sheet: &str, cell: &str) -> String {
    book.sheet_by_name(sheet)
        .unwrap()
        .style(cell)
        .number_format()
        .map(|n| n.format_code().to_string())
        .unwrap_or_else(|| "General".into())
}

fn bold(book: &Workbook, sheet: &str, cell: &str) -> bool {
    book.sheet_by_name(sheet)
        .unwrap()
        .style(cell)
        .font()
        .is_some_and(|f| f.bold())
}

fn values(path: &Path, sheet: &str) -> Vec<Vec<Data>> {
    let mut wb: Xlsx<_> = open_workbook(path).unwrap();
    let r = wb.worksheet_range(sheet).unwrap();
    r.rows().map(|row| row.to_vec()).collect()
}

/// id, amount, share, day, at, clock, label, flag.
fn sales() -> RecordBatch {
    RecordBatch::try_from_iter([
        ("id", Arc::new(Int64Array::from(vec![1, 2])) as ArrayRef),
        (
            "amount",
            Arc::new(Float64Array::from(vec![Some(1234.5), None])) as ArrayRef,
        ),
        (
            "share",
            Arc::new(Float64Array::from(vec![0.125, 0.5])) as ArrayRef,
        ),
        ("day", Arc::new(Date32Array::from(vec![20478, 20479])) as ArrayRef),
        (
            "at",
            Arc::new(TimestampMicrosecondArray::from(vec![1_769_342_400_000_000, 0])) as ArrayRef,
        ),
        (
            "clock",
            Arc::new(Time64MicrosecondArray::from(vec![3_600_000_000, 0])) as ArrayRef,
        ),
        ("label", Arc::new(StringArray::from(vec!["a", "b"])) as ArrayRef),
        (
            "flag",
            Arc::new(BooleanArray::from(vec![true, false])) as ArrayRef,
        ),
    ])
    .unwrap()
}

#[test]
fn a_query_entry_format_is_applied_and_values_keep_their_types() {
    let (_d, book, _) = write(
        json!({}),
        vec![(
            meta(
                "Sales",
                &[
                    ("amount", "#,##0.00"),
                    ("share", "0.0%"),
                    ("day", "mmm yyyy"),
                    ("at", "dd/mm/yyyy hh:mm"),
                    ("label", "@"),
                ],
            ),
            sales(),
        )],
    );
    assert_eq!(numfmt(&book, "Sales", "B2"), "#,##0.00");
    assert_eq!(numfmt(&book, "Sales", "C2"), "0.0%");
    assert_eq!(numfmt(&book, "Sales", "D2"), "mmm yyyy");
    assert_eq!(numfmt(&book, "Sales", "E2"), "dd/mm/yyyy hh:mm");
    assert_eq!(numfmt(&book, "Sales", "G2"), "@");
    // Unformatted numbers stay General; dates and times keep the built-in defaults.
    assert_eq!(numfmt(&book, "Sales", "A2"), "General");
    assert_eq!(numfmt(&book, "Sales", "F2"), "hh:mm:ss");
    // The header stays bold text.
    assert!(bold(&book, "Sales", "B1"));
    assert_eq!(book.sheet_by_name("Sales").unwrap().value("B1"), "amount");
    // Still a number: 1234.5, not text.
    let cell = book.sheet_by_name("Sales").unwrap().cell("B2").unwrap();
    assert_eq!(cell.value_number(), Some(1234.5));
    // A null stays empty.
    assert_eq!(book.sheet_by_name("Sales").unwrap().value("B3"), "");
}

#[test]
fn reports_without_formats_keep_the_built_in_date_formats() {
    let (_d, book, _) = write(json!({}), vec![(meta("Sales", &[]), sales())]);
    assert_eq!(numfmt(&book, "Sales", "B2"), "General");
    assert_eq!(numfmt(&book, "Sales", "D2"), "yyyy-mm-dd");
    assert_eq!(numfmt(&book, "Sales", "E2"), "yyyy-mm-dd hh:mm:ss");
    assert_eq!(numfmt(&book, "Sales", "F2"), "hh:mm:ss");
}

#[test]
fn a_query_entry_wins_over_output_level_columns() {
    let (_d, book, _) = write(
        json!({"columns": {"amount": {"format": "[$€-x-euro2] #,##0.00"}}}),
        vec![
            (meta("Sales", &[("amount", "#,##0.00")]), sales()),
            (meta("Refunds", &[]), sales()),
        ],
    );
    assert_eq!(numfmt(&book, "Sales", "B2"), "#,##0.00");
    assert_eq!(numfmt(&book, "Refunds", "B2"), "[$€-x-euro2] #,##0.00");
}

#[test]
fn type_defaults_apply_and_a_column_format_beats_them() {
    let (_d, book, _) = write(
        json!({"date_format": "dd/mm/yyyy", "datetime_format": "dd/mm/yyyy hh:mm", "time_format": "h:mm AM/PM"}),
        vec![(meta("Sales", &[("at", "yyyy")]), sales())],
    );
    assert_eq!(numfmt(&book, "Sales", "D2"), "dd/mm/yyyy");
    assert_eq!(numfmt(&book, "Sales", "E2"), "yyyy");
    assert_eq!(numfmt(&book, "Sales", "F2"), "h:mm AM/PM");
}

#[test]
fn formats_follow_anchor_header_and_split_sheets() {
    let n = 5;
    let b = RecordBatch::try_from_iter([(
        "amount",
        Arc::new(Float64Array::from_iter_values((0..n).map(f64::from))) as ArrayRef,
    )])
    .unwrap();
    let mut m = meta("Lines", &[("amount", "0.00")]);
    m.anchor = Some("B4".into());
    m.header = Some(false);
    let (_d, book, _) = write(json!({"max_rows_per_sheet": 2}), vec![(m, b)]);
    assert_eq!(numfmt(&book, "Lines", "B4"), "0.00");
    assert_eq!(numfmt(&book, "Lines", "B5"), "0.00");
    assert_eq!(numfmt(&book, "Lines (3)", "B4"), "0.00");
}

#[test]
fn values_excel_cant_hold_stay_text_and_the_warning_says_the_format_was_skipped() {
    let b = RecordBatch::try_from_iter([
        ("big", Arc::new(Int64Array::from(vec![i64::MAX, 42])) as ArrayRef),
        (
            "d",
            Arc::new(Date32Array::from(vec![-719_162, 20478])) as ArrayRef,
        ),
    ])
    .unwrap();
    let (d, path, r) = try_write(
        json!({}),
        vec![(meta("S", &[("big", "#,##0"), ("d", "dd/mm/yyyy")]), b)],
        None,
    );
    let warnings = r.unwrap();
    let rows = values(&path, "S");
    assert_eq!(rows[1][0], Data::String("9223372036854775807".into()));
    assert_eq!(rows[2][0], Data::Float(42.0));
    assert_eq!(rows[1][1], Data::String("0001-01-01".into()));
    let book = umya_spreadsheet::reader::xlsx::read(&path).unwrap();
    assert_eq!(numfmt(&book, "S", "A2"), "General");
    assert_eq!(numfmt(&book, "S", "A3"), "#,##0");
    drop(d);
    assert_eq!(warnings.len(), 2, "{warnings:?}");
    assert!(
        warnings
            .iter()
            .all(|w| w.ends_with("; the column's format wasn't applied to them")),
        "{warnings:?}"
    );
}

#[test]
fn a_column_the_query_doesnt_return_is_an_error() {
    let (_d, _, r) = try_write(json!({}), vec![(meta("Sales", &[("amt", "0")]), sales())], None);
    let e = r.unwrap_err();
    assert!(
        e.contains(
            "sheet `Sales`: `columns` names `amt`, which query `sales` doesn't return (it has: id, amount, share, day, at, clock, label, flag)"
        ),
        "{e}"
    );
}

#[test]
fn a_format_that_doesnt_fit_its_column_is_an_error() {
    for (col, code, want) in [
        (
            "amount",
            "dd/mm/yyyy",
            "column `amount` is a number column, but its format `dd/mm/yyyy` is a date/time format",
        ),
        (
            "day",
            "#,##0",
            "column `day` is a date column, but its format `#,##0` is a number format",
        ),
        (
            "label",
            "0.00",
            "column `label` is a text or boolean column, but its format `0.00` is a number format",
        ),
    ] {
        let (_d, _, r) = try_write(json!({}), vec![(meta("Sales", &[(col, code)]), sales())], None);
        let e = r.unwrap_err();
        assert!(e.contains(&format!("sheet `Sales`: {want}")), "{e}");
    }
}

#[test]
fn an_output_level_name_on_no_sheet_is_an_error_and_writes_nothing() {
    let (_d, path, r) = try_write(
        json!({"columns": {"amout": {"format": "0"}}}),
        vec![(meta("Sales", &[]), sales())],
        None,
    );
    let e = r.unwrap_err();
    assert!(
        e.contains("`columns` names `amout`, which no sheet has (sheets: Sales); no workbook was written"),
        "{e}"
    );
    assert!(!path.exists());
}

// -- templates ------------------------------------------------------------------------------------

fn template() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/branded.xlsx")
}

/// Three columns for the template's table block at `Summary!A5`, whose reserved row has a
/// `General` A5 and a `#,##0.00` C5 on a yellow fill.
fn block(a: ArrayRef, c: ArrayRef) -> RecordBatch {
    let n = a.len();
    RecordBatch::try_from_iter([
        ("a", a),
        (
            "b",
            Arc::new(StringArray::from_iter_values((0..n).map(|i| format!("r{i}")))) as ArrayRef,
        ),
        ("c", c),
    ])
    .unwrap()
}

fn fill(options: Value, m: ResultSetMeta, b: RecordBatch) -> (tempfile::TempDir, Workbook) {
    let payload = json!({
        "file": template().to_str().unwrap(),
        "bindings": [{"query": m.query, "sheet": "Summary", "anchor": "A5", "header": false, "columns": ["a", "b", "c"]}],
    });
    let (dir, path, r) = try_write(options, vec![(m, b)], Some(payload));
    r.unwrap();
    let book = umya_spreadsheet::reader::xlsx::read(&path).unwrap();
    (dir, book)
}

fn fill_colour(book: &Workbook, cell: &str) -> Option<String> {
    book.sheet_by_name("Summary")
        .unwrap()
        .style(cell)
        .fill()
        .and_then(|f| f.pattern_fill())
        .and_then(|p| p.foreground_color())
        .map(|c| {
            let a = c.argb();
            format!("{:02X}{:02X}{:02X}", a.r, a.g, a.b)
        })
}

#[test]
fn an_explicit_format_overrides_a_formatted_template_cell_and_keeps_its_fill() {
    let (_d, book) = fill(
        json!({}),
        meta("Block", &[("c", "0.0%")]),
        block(
            Arc::new(Int64Array::from(vec![1, 2, 3])),
            Arc::new(Float64Array::from(vec![0.1, 0.2, 0.3])),
        ),
    );
    for cell in ["C5", "C6", "C7"] {
        assert_eq!(numfmt(&book, "Summary", cell), "0.0%", "{cell}");
    }
    assert!(fill_colour(&book, "C7").is_some_and(|c| c.ends_with("FFF2CC")));
}

#[test]
fn a_formatted_template_cell_beats_date_format_and_a_general_one_gets_it() {
    let (_d, book) = fill(
        json!({"date_format": "dd/mm/yyyy"}),
        meta("Block", &[]),
        block(
            Arc::new(Date32Array::from(vec![20478, 20479])),
            Arc::new(Date32Array::from(vec![20478, 20479])),
        ),
    );
    // A5 is General in the template, so it takes `date_format`, and so does the inserted A6.
    assert_eq!(numfmt(&book, "Summary", "A5"), "dd/mm/yyyy");
    assert_eq!(numfmt(&book, "Summary", "A6"), "dd/mm/yyyy");
    // C5 has its own format, which wins over `date_format` on every row.
    assert_eq!(numfmt(&book, "Summary", "C5"), "#,##0.00");
    assert_eq!(numfmt(&book, "Summary", "C6"), "#,##0.00");
}

#[test]
fn unbound_sets_in_a_template_get_formats_like_plain_output() {
    let payload = json!({"file": template().to_str().unwrap(), "bindings": []});
    let (_d, path, r) = try_write(
        json!({"columns": {"amount": {"format": "#,##0.00"}}, "date_format": "dd/mm/yyyy"}),
        vec![(meta("Extra", &[]), sales())],
        Some(payload),
    );
    r.unwrap();
    let book = umya_spreadsheet::reader::xlsx::read(&path).unwrap();
    assert_eq!(numfmt(&book, "Extra", "B2"), "#,##0.00");
    assert_eq!(numfmt(&book, "Extra", "D2"), "dd/mm/yyyy");
}
