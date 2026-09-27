use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use dre_protocol::conformance;
use dre_protocol::host::{LogSink, PluginProcess};
use dre_protocol::msg::ResultSetMeta;
use serde_json::{Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-format-fixed_width"))
}

fn write(options: Value, batch: RecordBatch) -> Result<Vec<u8>, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.txt");
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let Value::Object(options) = options else { panic!() };
    let meta = ResultSetMeta {
        name: "x".into(),
        query: "x".into(),
        result_index: 1,
        anchor: None,
        header: None,
    };
    p.write_begin(path.to_str().unwrap(), "fixed_width", options, vec![meta], None)
        .unwrap();
    p.write_result_set(&batch.schema(), vec![batch]).unwrap();
    p.write_finish().map_err(|e| e.to_string())?;
    Ok(std::fs::read(&path).unwrap())
}

fn batch() -> RecordBatch {
    RecordBatch::try_from_iter([
        (
            "account",
            Arc::new(StringArray::from(vec![Some("ACME"), Some("CLIENT A"), None])) as ArrayRef,
        ),
        (
            "cents",
            Arc::new(Int64Array::from(vec![Some(1050), Some(-7), Some(0)])) as ArrayRef,
        ),
    ])
    .unwrap()
}

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn columns_are_aligned_and_padded_with_sensible_defaults() {
    let out = write(
        json!({"columns": [
            {"name": "account", "width": 10},
            {"name": "cents", "width": 6, "align": "right"},
            {"name": "account", "width": 4, "align": "right", "pad": "*", "truncate": true},
        ]}),
        batch(),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "ACME      001050ACME\r\nCLIENT A  -00007CLIE\r\n          000000****\r\n"
    );
}

#[test]
fn a_value_wider_than_its_column_is_an_error_naming_row_and_column() {
    let err = write(json!({"columns": [{"name": "account", "width": 5}]}), batch()).unwrap_err();
    assert!(err.contains("row 2, column `account`"), "{err}");
}

#[test]
fn line_ending_and_encoding_are_honoured() {
    let b = RecordBatch::try_from_iter([("city", Arc::new(StringArray::from(vec!["Zürich"])) as ArrayRef)])
        .unwrap();
    let out = write(
        json!({"columns": [{"name": "city", "width": 7}], "line_ending": "\n", "encoding": "latin1"}),
        b,
    )
    .unwrap();
    assert_eq!(out, b"Z\xFCrich \n");
}

#[test]
fn an_unknown_column_name_is_reported() {
    let err = write(json!({"columns": [{"name": "nope", "width": 3}]}), batch()).unwrap_err();
    assert!(
        err.contains("column `nope` isn't in the result set (it has: account, cents)"),
        "{err}"
    );
}

#[test]
fn timezone_aware_timestamps_are_written_in_their_zone() {
    use arrow::array::TimestampMicrosecondArray;
    for (tz, want) in [("UTC", "2026-01-01 00:00:00+00:00"), ("Australia/Sydney", "2026-01-01 11:00:00+11:00")] {
        let a = TimestampMicrosecondArray::from(vec![1_767_225_600_000_000]).with_timezone(tz);
        let b = RecordBatch::try_from_iter([("t", Arc::new(a) as ArrayRef)]).unwrap();
        let out = write(json!({"columns": [{"name": "t", "width": 25}], "line_ending": "\n"}), b).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), format!("{want}\n"));
    }
}

#[test]
fn line_breaks_are_refused_or_replaced() {
    let b = RecordBatch::try_from_iter([(
        "a",
        Arc::new(StringArray::from(vec!["ok", "line1\nline2\tcol"])) as ArrayRef,
    )])
    .unwrap();
    let cols = json!([{"name": "a", "width": 20}]);
    let err = write(json!({"columns": cols}), b.clone()).unwrap_err();
    assert!(err.contains("row 2, column `a`") && err.contains("line break"), "{err}");
    let out = write(json!({"columns": cols, "line_breaks": "replace", "line_ending": "\n"}), b).unwrap();
    assert_eq!(String::from_utf8(out).unwrap(), "ok                  \nline1 line2\tcol     \n");
}
