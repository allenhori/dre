//! Every first-party format through `dre run`.

mod common;

use calamine::{Reader, Xlsx, open_workbook};
use common::TestProject;

const PROFILES: &str =
    "sources:\n  warehouse:\n    target: dev\n    targets:\n      dev: {type: duckdb, path: data.duckdb}\n";

fn project(report: &str) -> TestProject {
    let p = TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: warehouse\n",
            ),
            (
                "dependencies.yml",
                "sources: [duckdb]\nformats: [csv, delimited, parquet, fixed_width, xlsx]\n",
            ),
            ("reports/fin/r/r.yml", report),
            (
                "reports/fin/r/accounts.sql",
                "select id as account_id, 'Client ' || id as account_name, id * 100.0 as balance from range(1, 4) t(id) order by id",
            ),
            ("reports/fin/r/total.sql", "select count(*) as n from range(1, 4)"),
        ],
        PROFILES,
    );
    p.duckdb("data.duckdb", "select 1;");
    p
}

#[test]
fn delimited_with_options() {
    let p = project(
        "queries: [accounts]\noutput: {format: delimited, delimiter: \"|\", line_ending: \"\\n\", header: false}\n",
    );
    p.dre("run", &["r"]).ok();
    assert_eq!(
        p.read("target/run/r/default/r.txt"),
        "1|Client 1|100.0\n2|Client 2|200.0\n3|Client 3|300.0\n"
    );
}

#[test]
fn fixed_width() {
    let p = project(
        "queries: [accounts]\noutput:\n  format: fixed_width\n  columns:\n    - {name: account_id, width: 3, align: right}\n    - {name: account_name, width: 10}\n",
    );
    p.dre("run", &["r"]).ok();
    assert_eq!(
        p.read("target/run/r/default/r.txt"),
        "001Client 1  \r\n002Client 2  \r\n003Client 3  \r\n"
    );
}

#[test]
fn parquet() {
    let p = project("queries: [accounts]\noutput: {format: parquet}\n");
    p.dre("run", &["r"]).ok();
    let f = std::fs::File::open(p.path("target/run/r/default/r.parquet")).unwrap();
    let reader = parquet::arrow::arrow_reader::ParquetRecordBatchReaderBuilder::try_new(f)
        .unwrap()
        .build()
        .unwrap();
    let rows: usize = reader.map(|b| b.unwrap().num_rows()).sum();
    assert_eq!(rows, 3);
}

#[test]
fn xlsx_template() {
    let p = project(
        "queries: [accounts, total]\noutput:\n  format: xlsx\n  template:\n    file: templates/branded.xlsx\n    bindings:\n\
         \x20     - {query: accounts, sheet: Summary, anchor: A5, header: false, columns: [account_id, account_name, balance]}\n\
         \x20     - {sheet: Summary, cell: B2, value: \"{{ run.date.iso }}\"}\n\
         \x20     - {sheet: Detail, cell: B1, query: total, column: n}\n",
    );
    let tmpl = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../plugins/format-xlsx/tests/fixtures/branded.xlsx");
    std::fs::create_dir_all(p.path("templates")).unwrap();
    std::fs::copy(tmpl, p.path("templates/branded.xlsx")).unwrap();
    p.dre("validate", &[]).ok();
    p.dre("run", &["r"]).ok();
    let mut wb: Xlsx<_> = open_workbook(p.path("target/run/r/default/r.xlsx")).unwrap();
    let s = wb.worksheet_range("Summary").unwrap();
    let v = |r: u32, c: u32| s.get_value((r, c)).map(|d| d.to_string()).unwrap_or_default();
    assert_eq!(
        (v(1, 1), v(4, 1), v(6, 1), v(7, 1)),
        (
            "2026-01-25".into(),
            "Client 1".into(),
            "Client 3".into(),
            "Total".into()
        )
    );
    let f = wb.worksheet_formula("Summary").unwrap();
    assert_eq!(f.get_value((7, 2)).map(String::as_str), Some("SUM(C5:C7)"));
    assert_eq!(
        wb.worksheet_range("Detail")
            .unwrap()
            .get_value((0, 1))
            .map(|d| d.to_string()),
        Some("3".into())
    );
    // The template itself is never modified.
    let mut orig: Xlsx<_> = open_workbook(p.path("templates/branded.xlsx")).unwrap();
    assert_eq!(
        orig.worksheet_formula("Summary")
            .unwrap()
            .get_value((5, 2))
            .map(String::as_str),
        Some("SUM(C5:C5)")
    );
}

#[test]
fn an_unquoted_null_option_is_the_null_marker() {
    for format in ["csv", "delimited"] {
        let p = project(&format!(
            "queries: [accounts]\noutput: {{format: {format}, null: \"NULL\"}}\n"
        ));
        p.write("reports/fin/r/accounts.sql", "select 1 as a, null as b");
        p.dre("run", &["r"]).ok();
        let ext = if format == "csv" { "csv" } else { "txt" };
        let out = p.read(&format!("target/run/r/default/r.{ext}"));
        assert!(
            out.contains("1,NULL") || out.contains("1\tNULL") || out.contains("1|NULL"),
            "{format}: {out}"
        );
    }
}

#[test]
fn fixed_width_refuses_line_breaks() {
    let p = project(
        "queries: [accounts]\noutput:\n  format: fixed_width\n  columns:\n    - {name: a, width: 20}\n",
    );
    p.write(
        "reports/fin/r/accounts.sql",
        "select 'line1' || chr(10) || 'line2' as a",
    );
    p.dre("run", &["r"])
        .failed()
        .says("row 1, column `a`")
        .says("line break");
}

#[test]
fn fixed_width_can_replace_line_breaks() {
    let p = project(
        "queries: [accounts]\noutput:\n  format: fixed_width\n  line_breaks: replace\n  line_ending: \"\\n\"\n  columns:\n    - {name: a, width: 12}\n",
    );
    p.write(
        "reports/fin/r/accounts.sql",
        "select 'l1' || chr(13) || chr(10) || 'l2' || chr(10) || 'x' as a",
    );
    p.dre("run", &["r"]).ok();
    assert_eq!(p.read("target/run/r/default/r.txt"), "l1 l2 x     \n");
}

#[test]
fn extension_override_and_no_extension() {
    let p = project(
        "queries: [accounts]\noutput:\n  format: fixed_width\n  extension: aba\n  columns:\n    - {name: account_id, width: 3, align: right}\n",
    );
    p.dre("run", &["r"]).ok();
    assert_eq!(p.read("target/run/r/default/r.aba"), "001\r\n002\r\n003\r\n");

    let p = project("queries: [total]\noutput: {format: csv, extension: \"\"}\n");
    p.dre("run", &["r"]).ok();
    assert_eq!(p.read("target/run/r/default/r"), "n\r\n3\r\n");

    let p = project("queries: [total]\noutput: {format: delimited, extension: .dat}\n");
    p.dre("run", &["r"]).ok();
    assert!(p.path("target/run/r/default/r.dat").is_file());

    let p = project("queries: [total]\noutput: {format: xlsx, extension: xls}\n");
    p.dre("validate", &[])
        .failed()
        .says("`extension` doesn't apply to xlsx");
}
