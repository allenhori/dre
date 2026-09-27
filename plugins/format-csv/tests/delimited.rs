use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, BooleanArray, Date32Array, Float64Array, Int64Array, RecordBatch, StringArray};
use dre_protocol::conformance;
use dre_protocol::host::{LogSink, PluginProcess};
use dre_protocol::msg::ResultSetMeta;
use serde_json::{Value, json};

fn csv() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-format-csv"))
}

fn delimited() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-format-delimited"))
}

fn batch() -> RecordBatch {
    RecordBatch::try_from_iter([
        (
            "id",
            Arc::new(Int64Array::from(vec![Some(1), Some(2), None])) as ArrayRef,
        ),
        (
            "name",
            Arc::new(StringArray::from(vec![
                Some("plain"),
                Some("has, comma"),
                Some("say \"hi\""),
            ])) as ArrayRef,
        ),
        (
            "amount",
            Arc::new(Float64Array::from(vec![Some(1.5), None, Some(-2.0)])) as ArrayRef,
        ),
        (
            "active",
            Arc::new(BooleanArray::from(vec![Some(true), Some(false), None])) as ArrayRef,
        ),
        (
            "opened",
            Arc::new(Date32Array::from(vec![Some(20478), Some(0), None])) as ArrayRef,
        ),
    ])
    .unwrap()
}

fn write(bin: &Path, options: Value, batches: Vec<RecordBatch>) -> Result<Vec<u8>, String> {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("out.csv");
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin, log).unwrap();
    let Value::Object(options) = options else { panic!() };
    let schema = batches[0].schema();
    let meta = ResultSetMeta {
        name: "out".into(),
        query: "q".into(),
        result_index: 1,
        anchor: None,
        header: None,
    };
    p.write_begin(path.to_str().unwrap(), "csv", options, vec![meta], None)
        .unwrap();
    p.write_result_set(&schema, batches).map_err(|e| e.to_string())?;
    let files = p.write_finish().map_err(|e| e.to_string())?;
    assert_eq!(files, vec![path.to_str().unwrap().to_string()]);
    Ok(std::fs::read(&path).unwrap())
}

fn text(bin: &Path, options: Value) -> String {
    String::from_utf8(write(bin, options, vec![batch()]).unwrap()).unwrap()
}

#[test]
fn both_binaries_conform_to_the_protocol() {
    conformance::assert_conforms(csv());
    conformance::assert_conforms(delimited());
}

#[test]
fn defaults_are_header_minimal_quoting_crlf_utf8_no_bom() {
    assert_eq!(
        text(csv(), json!({})),
        "id,name,amount,active,opened\r\n\
         1,plain,1.5,true,2026-01-25\r\n\
         2,\"has, comma\",,false,1970-01-01\r\n\
         ,\"say \"\"hi\"\"\",-2.0,,\r\n"
    );
}

#[test]
fn every_documented_option_is_honoured() {
    let out = text(
        delimited(),
        json!({"delimiter": "|", "quote": "'", "quoting": "all", "header": false, "line_ending": "\n", "null": "NULL"}),
    );
    assert_eq!(
        out,
        "'1'|'plain'|'1.5'|'true'|'2026-01-25'\n\
         '2'|'has, comma'|NULL|'false'|'1970-01-01'\n\
         NULL|'say \"hi\"'|'-2.0'|NULL|NULL\n"
    );
}

#[test]
fn byte_order_mark_is_opt_in() {
    let bytes = write(
        csv(),
        json!({"byte_order_mark": true, "header": false}),
        vec![batch()],
    )
    .unwrap();
    assert!(bytes.starts_with(b"\xEF\xBB\xBF1,plain"));
}

#[test]
fn quoting_strings_quotes_text_columns_and_leaves_numbers_booleans_and_nulls_bare() {
    assert_eq!(
        text(csv(), json!({"quoting": "strings", "line_ending": "\n"})),
        "\"id\",\"name\",\"amount\",\"active\",\"opened\"\n\
         1,\"plain\",1.5,true,\"2026-01-25\"\n\
         2,\"has, comma\",,false,\"1970-01-01\"\n\
         ,\"say \"\"hi\"\"\",-2.0,,\n"
    );
}

#[test]
fn quoting_strings_still_quotes_a_number_that_contains_the_delimiter() {
    let out = text(
        csv(),
        json!({"quoting": "strings", "delimiter": ".", "header": false, "line_ending": "\n"}),
    );
    assert!(out.starts_with("1.\"plain\".\"1.5\".true."), "{out}");
}

#[test]
fn quoting_none_refuses_values_it_cant_represent() {
    let err = write(csv(), json!({"quoting": "none"}), vec![batch()]).unwrap_err();
    assert!(err.contains("row 2, column `name`"), "{err}");
}

#[test]
fn other_encodings_are_written() {
    let b = RecordBatch::try_from_iter([("city", Arc::new(StringArray::from(vec!["Zürich"])) as ArrayRef)])
        .unwrap();
    let bytes = write(csv(), json!({"encoding": "latin1", "header": false}), vec![b]).unwrap();
    assert_eq!(bytes, b"Z\xFCrich\r\n");
}

#[test]
fn an_empty_result_set_still_gets_its_header() {
    let empty = RecordBatch::new_empty(batch().schema());
    assert_eq!(
        String::from_utf8(write(csv(), json!({}), vec![empty]).unwrap()).unwrap(),
        "id,name,amount,active,opened\r\n"
    );
}

fn timestamps(tz: &str) -> RecordBatch {
    use arrow::array::TimestampMicrosecondArray;
    // 2026-01-01 00:00:00 UTC, in two batches' worth of rows.
    let a = TimestampMicrosecondArray::from(vec![Some(1_767_225_600_000_000), None]).with_timezone(tz);
    RecordBatch::try_from_iter([("t", Arc::new(a) as ArrayRef)]).unwrap()
}

#[test]
fn timezone_aware_timestamps_are_written_in_their_zone() {
    for bin in [csv(), delimited()] {
        let out = write(
            bin,
            json!({"header": false}),
            vec![timestamps("UTC"), timestamps("UTC")],
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "2026-01-01 00:00:00+00:00\r\n\r\n".repeat(2)
        );
        let out = write(
            bin,
            json!({"header": false}),
            vec![timestamps("Australia/Sydney")],
        )
        .unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "2026-01-01 11:00:00+11:00\r\n\r\n"
        );
        let out = write(bin, json!({"header": false}), vec![timestamps("+10:00")]).unwrap();
        assert_eq!(
            String::from_utf8(out).unwrap(),
            "2026-01-01 10:00:00+10:00\r\n\r\n"
        );
    }
}

#[test]
fn an_error_on_the_first_of_several_batches_is_the_plugins_own() {
    // The first batch fails (`quoting: none` can't write a comma); many more follow.
    let mut batches = vec![batch()];
    batches.extend(std::iter::repeat_n(batch(), 2000));
    let err = write(csv(), json!({"quoting": "none"}), batches).unwrap_err();
    assert!(err.contains("row 2, column `name`"), "{err}");
    assert!(
        !err.contains("expected `finish`") && !err.contains("255, 255"),
        "{err}"
    );
}
