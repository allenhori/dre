use std::path::{Path, PathBuf};
use std::sync::Arc;

use arrow::array::{
    ArrayRef, BooleanArray, Date32Array, Decimal128Array, Int64Array, RecordBatch, StringArray,
    TimestampMicrosecondArray,
};
use calamine::{Data, Reader, Xlsx, open_workbook};
use dre_protocol::conformance;
use dre_protocol::host::{LogSink, PluginProcess};
use dre_protocol::msg::ResultSetMeta;
use serde_json::{Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-format-xlsx"))
}

fn meta(name: &str, anchor: Option<&str>, header: Option<bool>) -> ResultSetMeta {
    ResultSetMeta {
        name: name.into(),
        query: name.into(),
        result_index: 1,
        anchor: anchor.map(Into::into),
        header,
    }
}

fn write(options: Value, sets: Vec<(ResultSetMeta, Vec<RecordBatch>)>) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.xlsx");
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let Value::Object(options) = options else { panic!() };
    p.write_begin(
        path.to_str().unwrap(),
        "xlsx",
        options,
        sets.iter().map(|s| s.0.clone()).collect(),
        None,
    )
    .unwrap();
    for (_, batches) in sets {
        let schema = batches[0].schema();
        p.write_result_set(&schema, batches).unwrap();
    }
    p.write_finish().unwrap();
    (dir, path)
}

fn sheet(path: &Path, name: &str) -> Vec<Vec<Data>> {
    let mut wb: Xlsx<_> = open_workbook(path).unwrap();
    let r = wb.worksheet_range(name).unwrap();
    // Rows from A1, so anchors show up as leading empty rows/cells.
    let (h, w) = r
        .end()
        .map(|(r, c)| (r as usize + 1, c as usize + 1))
        .unwrap_or((0, 0));
    (0..h)
        .map(|i| {
            (0..w)
                .map(|j| r.get_value((i as u32, j as u32)).cloned().unwrap_or(Data::Empty))
                .collect()
        })
        .collect()
}

fn sheet_names(path: &Path) -> Vec<String> {
    let wb: Xlsx<_> = open_workbook(path).unwrap();
    wb.sheet_names().to_vec()
}

fn ints(n: i64) -> RecordBatch {
    RecordBatch::try_from_iter([("n", Arc::new(Int64Array::from_iter_values(0..n)) as ArrayRef)]).unwrap()
}

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn each_result_set_is_a_sheet_with_a_bold_header() {
    let (_d, path) = write(
        json!({}),
        vec![
            (meta("Summary", None, None), vec![ints(2)]),
            (meta("Detail", None, None), vec![ints(1)]),
        ],
    );
    assert_eq!(sheet_names(&path), vec!["Summary", "Detail"]);
    assert_eq!(
        sheet(&path, "Summary"),
        vec![
            vec![Data::String("n".into())],
            vec![Data::Float(0.0)],
            vec![Data::Float(1.0)]
        ]
    );
    assert_eq!(sheet(&path, "Detail").len(), 2);
}

#[test]
fn anchor_and_per_sheet_header_are_honoured() {
    let (_d, path) = write(
        json!({"header": false}),
        vec![
            (meta("A", Some("B3"), None), vec![ints(1)]),
            (meta("B", Some("A2"), Some(true)), vec![ints(1)]),
        ],
    );
    let a = sheet(&path, "A");
    assert_eq!(a.len(), 3);
    assert_eq!(a[2], vec![Data::Empty, Data::Float(0.0)]);
    let b = sheet(&path, "B");
    assert_eq!(b[1..], [vec![Data::String("n".into())], vec![Data::Float(0.0)]]);
}

#[test]
fn a_long_result_set_continues_on_numbered_sheets_with_the_header_repeated() {
    let (_d, path) = write(
        json!({"max_rows_per_sheet": 4}),
        vec![(meta("Summary", None, None), vec![ints(3), ints(7)])],
    );
    assert_eq!(sheet_names(&path), vec!["Summary", "Summary (2)", "Summary (3)"]);
    let rows = |s: &str| sheet(&path, s);
    assert_eq!(rows("Summary").len(), 5);
    assert_eq!(rows("Summary (2)").len(), 5);
    assert_eq!(rows("Summary (3)")[0], vec![Data::String("n".into())]);
    // 10 rows: 0,1,2 then 0..6 from the second batch → last sheet holds the final two.
    assert_eq!(
        rows("Summary (3)")[1..],
        [vec![Data::Float(5.0)], vec![Data::Float(6.0)]]
    );
}

#[test]
fn continuation_names_stay_within_31_characters() {
    let long = "A very long result set name xyz"; // 31 chars
    let (_d, path) = write(
        json!({"max_rows_per_sheet": 1}),
        vec![(meta(long, None, None), vec![ints(2)])],
    );
    let names = sheet_names(&path);
    assert_eq!(names[1], "A very long result set name (2)");
    assert!(names.iter().all(|n| n.chars().count() <= 31));
}

#[test]
fn values_keep_their_types() {
    let b = RecordBatch::try_from_iter([
        (
            "s",
            Arc::new(StringArray::from(vec![Some("x"), None])) as ArrayRef,
        ),
        ("b", Arc::new(BooleanArray::from(vec![true, false])) as ArrayRef),
        ("d", Arc::new(Date32Array::from(vec![20478, 0])) as ArrayRef),
        (
            "t",
            Arc::new(TimestampMicrosecondArray::from(vec![1_769_342_400_000_000, 0])) as ArrayRef,
        ),
        (
            "m",
            Arc::new(
                Decimal128Array::from(vec![10050, -325])
                    .with_precision_and_scale(10, 2)
                    .unwrap(),
            ) as ArrayRef,
        ),
    ])
    .unwrap();
    let (_d, path) = write(json!({"header": false}), vec![(meta("T", None, None), vec![b])]);
    let rows = sheet(&path, "T");
    assert_eq!(rows[0][0], Data::String("x".into()));
    assert_eq!(rows[1][0], Data::Empty);
    assert_eq!(rows[0][1], Data::Bool(true));
    match &rows[0][2] {
        Data::DateTime(d) => assert_eq!(d.as_f64(), 46047.0), // 2026-01-25
        other => panic!("date cell: {other:?}"),
    }
    match &rows[0][3] {
        Data::DateTime(d) => assert_eq!(d.as_f64(), 46047.5), // 2026-01-25 12:00
        other => panic!("datetime cell: {other:?}"),
    }
    assert_eq!(rows[0][4], Data::Float(100.5));
    assert_eq!(rows[1][4], Data::Float(-3.25));
}
