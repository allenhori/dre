//! xlsx number formats per column, end to end: `columns:` on query entries and at output level,
//! the date defaults from `format_options`, and the errors `dre validate` and `dre run` give.

mod common;

use common::TestProject;

const PROFILES: &str = "\
connections:
  warehouse:
    targets:
      dev: {type: duckdb, path: data.duckdb}
";
const PLUGINS: &str = "plugins:\n  - duckdb\n  - csv\n  - xlsx\n";

fn project(project_file: &str, files: &[(&str, &str)]) -> TestProject {
    let mut all = vec![("dre_project.yml", project_file), ("dependencies.yml", PLUGINS)];
    all.extend_from_slice(files);
    let p = TestProject::new(&all, PROFILES);
    p.duckdb(
        "data.duckdb",
        "create table sales as select range as id, range * 1000.5 as amount, range / 8 as share, \
         date '2026-01-25' + range::int as day from range(3);",
    );
    p
}

const PLAIN: &str = "name: acme_reports\ndefault_profile: warehouse\n";

fn numfmt(p: &TestProject, rel: &str, sheet: &str, cell: &str) -> String {
    let book = umya_spreadsheet::reader::xlsx::read(p.path(rel)).unwrap();
    book.sheet_by_name(sheet)
        .unwrap()
        .style(cell)
        .number_format()
        .map(|n| n.format_code().to_string())
        .unwrap_or_else(|| "General".into())
}

const SQL: (&str, &str) = ("reports/fin/r/sales.sql", "select * from sales order by id");
const SQL2: (&str, &str) = ("reports/fin/r/refunds.sql", "select * from sales order by id");

#[test]
fn query_entry_and_output_level_formats_reach_the_workbook() {
    let p = project(
        PLAIN,
        &[
            (
                "reports/fin/r/r.yml",
                "queries:\n\
                 \x20 - query: sales\n\
                 \x20   columns:\n\
                 \x20     amount: {format: \"#,##0.00\"}\n\
                 \x20     share: {format: \"0.0%\"}\n\
                 \x20 - refunds\n\
                 output:\n\
                 \x20 format: xlsx\n\
                 \x20 date_format: dd/mm/yyyy\n\
                 \x20 columns:\n\
                 \x20   amount: {format: \"[$€-x-euro2] #,##0.00\"}\n",
            ),
            SQL,
            SQL2,
        ],
    );
    p.dre("validate", &[]).ok();
    p.dre("run", &["r"]).ok();
    let f = "target/run/r/default/r.xlsx";
    assert_eq!(numfmt(&p, f, "sales", "B2"), "#,##0.00");
    assert_eq!(numfmt(&p, f, "sales", "C2"), "0.0%");
    assert_eq!(numfmt(&p, f, "refunds", "B2"), "[$€-x-euro2] #,##0.00");
    assert_eq!(numfmt(&p, f, "sales", "D2"), "dd/mm/yyyy");
    assert_eq!(numfmt(&p, f, "sales", "A2"), "General");
}

#[test]
fn format_options_set_the_project_date_format_and_a_report_overrides_it() {
    let p = project(
        "name: acme_reports\ndefault_profile: warehouse\nformat_options:\n  xlsx: {date_format: dd/mm/yyyy}\n",
        &[
            (
                "reports/fin/r/r.yml",
                "queries: [sales]\noutput: {format: xlsx}\n",
            ),
            SQL,
            (
                "reports/fin/o/o.yml",
                "queries: [osales]\noutput: {format: xlsx, date_format: mmm yyyy}\n",
            ),
            ("reports/fin/o/osales.sql", "select * from sales"),
        ],
    );
    p.dre("run", &["r", "o"]).ok();
    assert_eq!(
        numfmt(&p, "target/run/r/default/r.xlsx", "sales", "D2"),
        "dd/mm/yyyy"
    );
    assert_eq!(
        numfmt(&p, "target/run/o/default/o.xlsx", "osales", "D2"),
        "mmm yyyy"
    );
}

#[test]
fn every_set_gets_the_formats() {
    let p = project(
        PLAIN,
        &[
            (
                "reports/fin/r/r.yml",
                "queries:\n  - {query: sales, columns: {amount: {format: \"0.00\"}}}\nsets: [a, b]\noutput: {format: xlsx}\n",
            ),
            SQL,
            ("sets.yml", "a: {}\nb: {}\n"),
        ],
    );
    p.dre("run", &["r", "--set", "all"]).ok();
    for set in ["a", "b"] {
        assert_eq!(
            numfmt(&p, &format!("target/run/r/{set}/r.xlsx"), "sales", "B2"),
            "0.00"
        );
    }
}

#[test]
fn validate_rejects_malformed_codes_and_unknown_keys() {
    let p = project(
        PLAIN,
        &[
            (
                "reports/fin/r/r.yml",
                "queries:\n  - {query: sales, columns: {amount: {format: \"\\\"x\"}, share: {fromat: \"0\"}}}\n\
                 output:\n  format: xlsx\n  date_format: \"#,##0\"\n  columns: {id: {format: \"[Red#\"}}\n",
            ),
            SQL,
        ],
    );
    p.dre("validate", &[])
        .failed()
        .says("report `r`: query `sales`: column `amount`: format `\"x` has an unclosed `\"` quote")
        .says("report `r`: query `sales`: column `share`: unknown key `fromat`; expected `format`")
        .says("`date_format` `#,##0` is a number format, but it must show dates or times")
        .says("`columns`: column `id`: format `[Red#` has an unclosed `[` bracket");
}

#[test]
fn columns_on_a_csv_report_is_an_error() {
    let p = project(
        PLAIN,
        &[
            (
                "reports/fin/r/r.yml",
                "queries:\n  - {query: sales, columns: {amount: {format: \"0.00\"}}}\noutput: {format: csv}\n",
            ),
            SQL,
        ],
    );
    p.dre("validate", &[])
        .failed()
        .says("`columns` on query `sales` only applies to the xlsx format");
}

#[test]
fn run_errors_name_the_sheet_and_column() {
    let p = project(
        PLAIN,
        &[
            (
                "reports/fin/r/r.yml",
                "queries:\n  - {query: sales, columns: {day: {format: \"0.00\"}}}\noutput: {format: xlsx}\n",
            ),
            SQL,
            (
                "reports/fin/u/u.yml",
                "queries:\n  - {query: usales, columns: {amt: {format: \"0.00\"}}}\noutput: {format: xlsx}\n",
            ),
            ("reports/fin/u/usales.sql", "select * from sales"),
        ],
    );
    p.dre("run", &["r"])
        .failed()
        .says("sheet `sales`: column `day` is a date column, but its format `0.00` is a number format");
    p.dre("run", &["u"])
        .failed()
        .says("`columns` names `amt`, which query `usales` doesn't return (it has: id, amount, share, day)");
}
