use std::path::Path;
use std::sync::Arc;

use arrow::array::{ArrayRef, Int64Array, RecordBatch, StringArray};
use dre_protocol::conformance;
use dre_protocol::host::{LogSink, PluginProcess};
use dre_protocol::msg::ResultSetMeta;
use serde_json::{Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-plugin-fixed_width"))
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
        columns: Default::default(),
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
            {"name": "cents", "width": 6, "align": "right", "pad": "0"},
        ]}),
        batch(),
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "ACME        1050ACME001050\r\nCLIENT A      -7CLIE-00007\r\n               0****000000\r\n"
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
    for (tz, want) in [
        ("UTC", "2026-01-01 00:00:00+00:00"),
        ("Australia/Sydney", "2026-01-01 11:00:00+11:00"),
    ] {
        let a = TimestampMicrosecondArray::from(vec![1_767_225_600_000_000]).with_timezone(tz);
        let b = RecordBatch::try_from_iter([("t", Arc::new(a) as ArrayRef)]).unwrap();
        let out = write(
            json!({"columns": [{"name": "t", "width": 25}], "line_ending": "\n"}),
            b,
        )
        .unwrap();
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
    assert!(
        err.contains("row 2, column `a`") && err.contains("line break"),
        "{err}"
    );
    let out = write(
        json!({"columns": cols, "line_breaks": "replace", "line_ending": "\n"}),
        b,
    )
    .unwrap();
    assert_eq!(
        String::from_utf8(out).unwrap(),
        "ok                  \nline1 line2\tcol     \n"
    );
}

fn lines(options: Value, batch: RecordBatch) -> Vec<String> {
    let mut o = options;
    o["line_ending"] = json!("\n");
    let out = String::from_utf8(write(o, batch).unwrap()).unwrap();
    out.lines().map(str::to_string).collect()
}

fn amounts() -> RecordBatch {
    use arrow::array::{Decimal128Array, Float64Array};
    let dec = Decimal128Array::from(vec![Some(123_456), Some(-1_005), Some(-4), None])
        .with_precision_and_scale(10, 3)
        .unwrap();
    RecordBatch::try_from_iter([
        ("amount", Arc::new(dec) as ArrayRef),
        (
            "rate",
            Arc::new(Float64Array::from(vec![
                Some(0.125),
                Some(2.0),
                Some(-1.5),
                Some(0.0),
            ])) as ArrayRef,
        ),
        (
            "text_amount",
            Arc::new(StringArray::from(vec![
                Some("12.5"),
                Some("-3"),
                Some(".25"),
                None,
            ])) as ArrayRef,
        ),
    ])
    .unwrap()
}

#[test]
fn every_column_is_left_aligned_and_space_filled_unless_told_otherwise() {
    let got = lines(
        json!({"columns": [{"name": "account", "width": 9}, {"name": "cents", "width": 6}]}),
        batch(),
    );
    assert_eq!(got, ["ACME     1050  ", "CLIENT A -7    ", "         0     "]);
}

#[test]
fn every_default_can_be_overridden() {
    let got = lines(
        json!({"columns": [
            {"name": "cents", "width": 6, "type": "number", "sign": "trailing", "align": "right", "pad": "*"},
            {"name": "account", "width": 4, "align": "right", "truncate": true, "null_fill": "?"},
            {"name": "amount", "picture": "9(4)V9", "align": "left", "pad": " "},
        ]}),
        RecordBatch::try_from_iter([
            ("cents", batch().column(1).clone()),
            ("account", batch().column(0).clone()),
            (
                "amount",
                Arc::new(arrow::array::Float64Array::from(vec![1.5, 22.0, 0.0])) as ArrayRef,
            ),
        ])
        .unwrap(),
    );
    assert_eq!(got, ["*1050+ACME15   ", "****7-CLIE220  ", "****0+????0    "]);
}

#[test]
fn decimals_round_half_away_from_zero_and_can_be_implied() {
    let got = lines(
        json!({"columns": [
            {"name": "amount", "width": 8, "decimals": 2, "decimal_point": "implied", "align": "right", "pad": "0"},
            {"name": "amount", "width": 8, "decimals": 2, "align": "right", "pad": "0"},
            {"name": "rate", "width": 6, "decimals": 2, "decimal_point": ",", "align": "right", "pad": "0"},
            {"name": "text_amount", "width": 6, "type": "number", "decimals": 2, "decimal_point": "implied", "align": "right", "pad": "0", "null_fill": " "},
            {"name": "amount", "width": 8, "decimals": 2, "decimal_point": "implied"},
        ]}),
        amounts(),
    );
    assert_eq!(
        got,
        [
            "0001234600123.46000,1300125012346   ",
            "-0000101-0001.01002,00-00300-101    ",
            "0000000000000.00-01,5000002500      ",
            "0000000000000000000,00              ",
        ]
    );
}

#[test]
fn signs_can_lead_trail_always_show_or_overpunch() {
    let got = lines(
        json!({"columns": [
            {"name": "cents", "width": 6, "sign": "always", "align": "right", "pad": "0"},
            {"name": "cents", "width": 6, "sign": "trailing", "align": "right", "pad": "0"},
            {"name": "cents", "width": 6, "sign": "overpunch", "align": "right", "pad": "0"},
            {"name": "cents", "width": 6, "sign": "always", "align": "right"},
        ]}),
        batch(),
    );
    assert_eq!(
        got,
        [
            "+0105001050+00105{ +1050",
            "-0000700007-00000P    -7",
            "+0000000000+00000{    +0",
        ]
    );
}

#[test]
fn an_unsigned_column_refuses_a_negative_number() {
    let err = write(
        json!({"columns": [{"name": "cents", "width": 6, "sign": "none"}]}),
        batch(),
    )
    .unwrap_err();
    assert!(
        err.contains("row 2, column `cents`") && err.contains("negative"),
        "{err}"
    );
}

#[test]
fn numbers_are_never_cut_even_with_truncate() {
    let err = write(
        json!({"columns": [{"name": "cents", "width": 3, "type": "number", "truncate": true}]}),
        batch(),
    )
    .unwrap_err();
    assert!(
        err.contains("row 1, column `cents`") && err.contains("never cut"),
        "{err}"
    );
}

#[test]
fn cobol_pictures_set_width_decimals_and_sign() {
    let got = lines(
        json!({"columns": [
            {"name": "amount", "picture": "S9(5)V99"},
            {"name": "amount", "picture": "s9(3).99", "sign": "leading"},
            {"name": "account", "picture": "X(4)", "truncate": true},
        ]}),
        RecordBatch::try_from_iter([
            ("amount", amounts().column(0).clone()),
            (
                "account",
                Arc::new(StringArray::from(vec!["ACME", "CLIENT A", "", "X"])) as ArrayRef,
            ),
        ])
        .unwrap(),
    );
    assert_eq!(
        got,
        [
            "001234F0123.46ACME",
            "000010J-001.01CLIE",
            "000000{0000.00    ",
            "00000000000000X   "
        ]
    );
}

#[test]
fn a_header_record_labels_each_column_within_its_width() {
    let got = lines(
        json!({"header": true, "columns": [
            {"name": "account", "width": 9},
            {"name": "cents", "width": 4, "header": "AMOUNT_CENTS"},
        ]}),
        batch(),
    );
    assert_eq!(got[0], "account  AMOU");
    assert_eq!(got[1], "ACME     1050");
}

#[test]
fn date_format_lays_out_dates_and_timestamps() {
    use arrow::array::{Date32Array, TimestampMicrosecondArray};
    let b = RecordBatch::try_from_iter([
        ("d", Arc::new(Date32Array::from(vec![20_454])) as ArrayRef),
        (
            "t",
            Arc::new(TimestampMicrosecondArray::from(vec![1_767_225_600_000_000])) as ArrayRef,
        ),
    ])
    .unwrap();
    let got = lines(
        json!({"columns": [
            {"name": "d", "width": 8, "date_format": "%Y%m%d"},
            {"name": "t", "width": 6, "date_format": "%d%m%y"},
        ]}),
        b,
    );
    assert_eq!(got, ["20260101010126"]);
}

#[test]
fn options_that_dont_fit_the_column_type_are_errors() {
    let err = write(
        json!({"columns": [{"name": "account", "width": 8, "date_format": "%Y"}]}),
        batch(),
    )
    .unwrap_err();
    assert!(err.contains("`date_format` doesn't apply"), "{err}");
    let err = write(
        json!({"columns": [{"name": "account", "width": 8, "decimals": 2}]}),
        batch(),
    )
    .unwrap_err();
    assert!(
        err.contains("row 1, column `account`: `ACME` isn't a number"),
        "{err}"
    );
    for (col, want) in [
        (
            json!({"name": "a", "picture": "9(3)", "width": 3}),
            "both `picture` and `width`",
        ),
        (json!({"name": "a", "picture": "Z(3)"}), "isn't a PIC clause"),
        (
            json!({"name": "a", "width": 3, "decimal_point": "implied"}),
            "without `decimals`",
        ),
        (json!({"name": "a", "width": 3, "sign": "left"}), "`sign` must be"),
        (
            json!({"name": "a", "width": 3, "type": "text", "decimals": 1}),
            "`type: text` and a number",
        ),
        (json!({"name": "a", "width": 3, "date_format": "%Q"}), "strftime"),
    ] {
        let err = write(json!({"columns": [col]}), batch()).unwrap_err();
        assert!(err.contains(want), "{want}: {err}");
    }
}
