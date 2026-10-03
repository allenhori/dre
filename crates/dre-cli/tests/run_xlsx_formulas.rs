//! xlsx row formulas and totals rows, end to end: `formula` and `total` in `columns:` on query
//! entries and at output level, and the errors `dre validate` and `dre run` give.

mod common;

use common::TestProject;

const PROFILES: &str = "\
connections:
  warehouse:
    targets:
      dev: {type: duckdb, path: data.duckdb}
";
const PLUGINS: &str = "plugins:\n  - duckdb\n  - csv\n  - xlsx\n";
const PLAIN: &str = "name: acme_reports\ndefault_profile: warehouse\n";
const SQL: (&str, &str) = (
    "reports/fin/r/lines.sql",
    "select region, qty, price, qty * price as line_total from lines order by region",
);

fn project(files: &[(&str, &str)]) -> TestProject {
    let mut all = vec![("dre_project.yml", PLAIN), ("dependencies.yml", PLUGINS)];
    all.extend_from_slice(files);
    let p = TestProject::new(&all, PROFILES);
    p.duckdb(
        "data.duckdb",
        "create table lines as select * from (values ('North', 2, 1.5), ('South', 3, 2.0), ('West', 4, 10.0)) t(region, qty, price);",
    );
    p
}

fn cell(p: &TestProject, rel: &str, sheet: &str, at: &str) -> (String, String) {
    let book = umya_spreadsheet::reader::xlsx::read(p.path(rel)).unwrap();
    let c = book.sheet_by_name(sheet).unwrap().cell(at).unwrap();
    (c.formula().to_string(), c.value().to_string())
}

#[test]
fn row_formulas_and_totals_reach_the_workbook() {
    let p = project(&[
        (
            "reports/fin/r/r.yml",
            "queries:\n\
             \x20 - query: lines\n\
             \x20   columns:\n\
             \x20     line_total: {formula: \"={qty}*{price}\", format: \"#,##0.00\", total: sum}\n\
             output:\n\
             \x20 format: xlsx\n\
             \x20 totals_label: Grand total\n\
             \x20 columns:\n\
             \x20   qty: {total: sum}\n",
        ),
        SQL,
    ]);
    p.dre("validate", &[]).ok();
    p.dre("run", &["r"]).ok();
    let f = "target/run/r/default/r.xlsx";
    assert_eq!(cell(&p, f, "lines", "D2"), ("B2*C2".into(), "3".into()));
    assert_eq!(cell(&p, f, "lines", "D4"), ("B4*C4".into(), "40".into()));
    assert_eq!(cell(&p, f, "lines", "A5").1, "Grand total");
    assert_eq!(cell(&p, f, "lines", "B5"), ("SUM(B2:B4)".into(), "9".into()));
    assert_eq!(cell(&p, f, "lines", "D5"), ("SUM(D2:D4)".into(), "49".into()));
}

#[test]
fn every_set_gets_the_formulas() {
    let p = project(&[
        (
            "reports/fin/r/r.yml",
            "queries:\n  - {query: lines, columns: {line_total: {formula: \"={qty}*{price}\"}}}\nsets: [a, b]\noutput: {format: xlsx}\n",
        ),
        SQL,
        ("sets.yml", "a: {}\nb: {}\n"),
    ]);
    p.dre("run", &["r", "--set", "all"]).ok();
    for set in ["a", "b"] {
        assert_eq!(
            cell(&p, &format!("target/run/r/{set}/r.xlsx"), "lines", "D3").0,
            "B3*C3"
        );
    }
}

#[test]
fn validate_rejects_bad_formulas_and_totals() {
    let p = project(&[
        (
            "reports/fin/r/r.yml",
            "queries:\n  - {query: lines, columns: {line_total: {formula: \"{qty}*2\"}, qty: {total: median}, price: {formula: \"={price:*}\"}}}\n\
             output:\n  format: xlsx\n  columns: {region: {total: \"={qty}\"}}\n",
        ),
        SQL,
    ]);
    p.dre("validate", &[])
        .failed()
        .says("report `r`: query `lines`: column `line_total`: formula `{qty}*2` must start with `=`")
        .says("report `r`: query `lines`: column `qty`: total `median` must be one of `sum`, `average`, `count`, `min`, `max`")
        .says("column `price`: formula `={price:*}` uses `{price:*}`, a whole column, which only a `total` can use")
        .says("`columns`: column `region`: total `={qty}` uses `{qty}`, a cell on the same row");
}

#[test]
fn run_errors_name_the_sheet_and_column() {
    let p = project(&[
        (
            "reports/fin/r/r.yml",
            "queries:\n  - {query: lines, columns: {line_total: {formula: \"={qty}*{prce}\"}}}\noutput: {format: xlsx}\n",
        ),
        SQL,
        (
            "reports/fin/t/t.yml",
            "queries:\n  - {query: tlines, columns: {region: {total: sum}}}\noutput: {format: xlsx}\n",
        ),
        ("reports/fin/t/tlines.sql", "select * from lines"),
    ]);
    p.dre("run", &["r"])
        .failed()
        .says("sheet `lines`: column `line_total`: formula `={qty}*{prce}` refers to `prce`, which query `lines` doesn't return (it has: region, qty, price, line_total)");
    p.dre("run", &["t"])
        .failed()
        .says("sheet `tlines`: column `region` is a text or boolean column, which `total: sum` can't total");
}
