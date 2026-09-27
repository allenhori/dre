//! Multi-statement execution, xlsx output, verification (dry run, preview, drift) and unmanaged
//! reports at run time, end to end.

mod common;

use calamine::{Data, Reader, Xlsx, open_workbook};
use common::TestProject;

const PROFILES: &str = "\
sources:
  warehouse:
    target: dev
    targets:
      dev: {type: duckdb, path: data.duckdb}
  fixture:
    target: dev
    targets:
      dev: {type: fixture}
destinations:
  inbox:
    target: dev
    targets:
      dev: {type: local}
";
const PLUGINS: &str = "sources:\n  - duckdb\n  - fixture\nformats:\n  - csv\n  - xlsx\n";

fn project(files: &[(&str, &str)]) -> TestProject {
    let mut all = vec![
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\n",
        ),
        ("dependencies.yml", PLUGINS),
    ];
    all.extend_from_slice(files);
    let p = TestProject::new(&all, PROFILES);
    p.duckdb(
        "data.duckdb",
        "create table accounts as select range as id, 'acct ' || range as name from range(12);",
    );
    p
}

fn sheets(p: &TestProject, rel: &str) -> Vec<String> {
    let wb: Xlsx<_> = open_workbook(p.path(rel)).unwrap();
    wb.sheet_names().to_vec()
}

fn cells(p: &TestProject, rel: &str, sheet: &str) -> Vec<Vec<String>> {
    let mut wb: Xlsx<_> = open_workbook(p.path(rel)).unwrap();
    let r = wb.worksheet_range(sheet).unwrap();
    r.rows()
        .map(|row| {
            row.iter()
                .map(|c| match c {
                    Data::Float(f) => f.to_string(),
                    Data::Empty => String::new(),
                    other => other.to_string(),
                })
                .collect()
        })
        .collect()
}

#[test]
fn statements_run_in_yaml_order_on_one_session_and_the_yaml_decides_the_tabs() {
    let p = project(&[
        (
            "reports/fin/monthly/monthly.yml",
            // Tab order is YAML order, not alphabetical: zeta before alpha.
            "queries:\n  - {query: setup, tab: false}\n  - {query: zeta, tab_name: Summary}\n  - {query: alpha, tab_name: Detail}\n  - {query: plain, anchor: B2, header: false}\n  - staged\n  - {query: discarded, tab: false}\noutput: {format: xlsx}\n",
        ),
        // Setup: temp table + a SET, no tab.
        (
            "reports/fin/monthly/setup.sql",
            "create temp table small as select * from accounts where id < 3;\nset threads = 1;\n",
        ),
        (
            "reports/fin/monthly/zeta.sql",
            "select count(*) as n from small;\n",
        ),
        (
            "reports/fin/monthly/alpha.sql",
            "select id, name from small order by id;\n",
        ),
        ("reports/fin/monthly/plain.sql", "select 'x' as a;\n"),
        // Earlier statements prepare; the last one is the tab.
        (
            "reports/fin/monthly/staged.sql",
            "create temp table t2 as select id * 10 as id from small;\nselect * from t2 order by id;",
        ),
        // Returns rows, but `tab: false` says no tab.
        ("reports/fin/monthly/discarded.sql", "select 42 as ignored;"),
    ]);
    p.dre("run", &["monthly"]).ok();
    let f = "target/run/monthly/default/monthly.xlsx";
    assert_eq!(sheets(&p, f), ["Summary", "Detail", "plain", "staged"]);
    assert_eq!(cells(&p, f, "Summary"), [["n"], ["3"]]);
    assert_eq!(cells(&p, f, "Detail")[3], ["2", "acct 2"]);
    assert_eq!(cells(&p, f, "staged")[1], ["0"]);
    // Anchored at B2 with no header: the only cell is B2.
    assert_eq!(cells(&p, f, "plain"), [["x"]]);
    let mut wb: Xlsx<_> = open_workbook(p.path(f)).unwrap();
    assert_eq!(wb.worksheet_range("plain").unwrap().start(), Some((1, 1)));
    let r = p.json("target/run/monthly/default/run_results.json");
    assert_eq!(r["result_sets"].as_array().unwrap().len(), 4);
}

#[test]
fn a_tab_with_no_rows_still_gets_its_column_names() {
    let p = project(&[
        (
            "reports/fin/r/r.yml",
            "queries:\n  - {query: none, tab_name: Nothing yet}\noutput: {format: xlsx}\n",
        ),
        (
            "reports/fin/r/none.sql",
            "select id, name from accounts where 1 = 0",
        ),
    ]);
    p.dre("run", &["r"]).ok();
    let f = "target/run/r/default/r.xlsx";
    assert_eq!(sheets(&p, f), ["Nothing yet"]);
    assert_eq!(cells(&p, f, "Nothing yet"), [["id", "name"]]);
}

#[test]
fn a_tab_query_whose_last_statement_returns_nothing_is_an_error() {
    let p = project(&[
        (
            "reports/fin/r/r.yml",
            "queries: [prep, rq]\noutput: {format: xlsx}\n",
        ),
        ("reports/fin/r/prep.sql", "create temp table x as select 1 as a"),
        ("reports/fin/r/rq.sql", "select * from x"),
    ]);
    p.dre("run", &["r"])
        .failed()
        .says("`prep` makes a tab, but its last statement returned no result set")
        .says("add `tab: false`");
    assert!(!p.path("target/run/r/default/r.xlsx").exists());
}

#[test]
fn two_selects_in_one_tab_file_are_an_error_before_anything_runs() {
    let p = project(&[
        ("reports/fin/r/r.yml", "queries: [rq]\noutput: {format: xlsx}\n"),
        ("reports/fin/r/rq.sql", "select 1 as a;\nselect 2 as b;"),
    ]);
    p.dre("run", &["r"])
        .failed()
        .says("reports/fin/r/rq.sql:1")
        .says("one .sql file makes one tab");
    p.write(
        "reports/fin/r/r.yml",
        "queries:\n  - {query: rq, tab_name: [A, B]}\noutput: {format: xlsx}\n",
    );
    p.dre("validate", &[])
        .failed()
        .says("`tab_name` of `rq` is a list, but one .sql file makes one tab");
}

#[test]
fn sheet_names_excel_rejects_fail_before_writing() {
    let p = project(&[
        (
            "reports/fin/r/r.yml",
            "queries:\n  - {query: rq, tab_name: \"Q1/Q2\"}\noutput: {format: xlsx}\n",
        ),
        ("reports/fin/r/rq.sql", "select 1 as a"),
    ]);
    p.dre("run", &["r"])
        .failed()
        .says("sheet name `Q1/Q2` contains `/`");
    assert!(!p.path("target/run/r/default/r.xlsx").exists());
    p.write("reports/fin/r/r.yml", "queries:\n  - {query: rq, tab_name: Same}\n  - {query: rq2, tab_name: same}\noutput: {format: xlsx}\n");
    p.write("reports/fin/r/rq2.sql", "select 2 as b");
    p.dre("run", &["r"])
        .failed()
        .says("sheet name `same` is used twice");
}

#[test]
fn a_long_result_set_splits_across_sheets() {
    let p = project(&[
        (
            "reports/fin/big/big.yml",
            "queries:\n  - {query: all_accounts, tab_name: Accounts}\noutput: {format: xlsx, max_rows_per_sheet: 5}\n",
        ),
        (
            "reports/fin/big/all_accounts.sql",
            "select id from accounts order by id",
        ),
    ]);
    p.dre("run", &["big"]).ok();
    assert_eq!(
        sheets(&p, "target/run/big/default/big.xlsx"),
        ["Accounts", "Accounts (2)", "Accounts (3)"]
    );
    assert_eq!(
        cells(&p, "target/run/big/default/big.xlsx", "Accounts (3)"),
        [["id"], ["10"], ["11"]]
    );
}

#[test]
fn single_table_formats_write_one_file_per_result_set_and_deliver_each() {
    let p = project(&[
        (
            "reports/fin/split/split.yml",
            "queries:\n  - {query: sq1, tab_name: Summary}\n  - {query: sq2, tab_name: Detail}\noutput:\n  destination: {profile: inbox, path: out/split.csv}\n",
        ),
        ("reports/fin/split/sq1.sql", "select 1 as a;"),
        ("reports/fin/split/sq2.sql", "select 2 as b;"),
    ]);
    p.dre("run", &["split"]).ok();
    assert_eq!(p.read("out/split_Summary.csv"), "a\r\n1\r\n");
    assert_eq!(p.read("out/split_Detail.csv"), "b\r\n2\r\n");
    let r = p.json("target/run/split/default/run_results.json");
    let outs: Vec<&str> = r["outputs"]
        .as_array()
        .unwrap()
        .iter()
        .map(|o| o["path"].as_str().unwrap())
        .collect();
    assert_eq!(
        outs,
        [
            "target/run/split/default/split_Summary.csv",
            "target/run/split/default/split_Detail.csv"
        ]
    );
}

#[test]
fn a_source_without_sessions_refuses_multi_statement_bindings_before_running_anything() {
    let p = project(&[
        ("reports/fin/f/f.yml", "queries: [fq]\nprofile: fixture\n"),
        ("reports/fin/f/fq.sql", "log first statement ran; rows 1;"),
    ]);
    p.dre_env("run", &["f"], &[("DRE_FIXTURE_MODE", "no_sessions")])
        .failed()
        .says("can't hold one session across them; nothing was run");
    let r = p.dre_env("run", &["f"], &[("DRE_FIXTURE_MODE", "no_sessions")]);
    assert!(!r.stderr.contains("first statement ran"), "{}", r.stderr);
    // With sessions it runs.
    p.dre("run", &["f"]).ok();
}

#[test]
fn dry_run_compiles_and_executes_nothing() {
    let p = project(&[
        ("reports/fin/d/d.yml", "queries: [dq]\n"),
        (
            "reports/fin/d/dq.sql",
            "delete from accounts where id = {{ 1 + 1 }};\nselect count(*) as n from accounts",
        ),
    ]);
    p.dre("run", &["d", "--dry-run"])
        .ok()
        .says("Compiled  target/compiled/d/default/dq.sql");
    assert_eq!(
        p.read("target/compiled/d/default/dq.sql"),
        "delete from accounts where id = 2;\nselect count(*) as n from accounts"
    );
    assert!(!p.path("target/run/d").exists());
    p.dre("run", &["d"]).ok();
    assert_eq!(
        p.read("target/run/d/default/d.csv"),
        "n\r\n11\r\n",
        "the delete ran exactly once, in the real run"
    );
}

#[test]
fn preview_limits_rows_keeps_output_local_and_is_flagged() {
    let p = project(&[
        (
            "reports/fin/pv/pv.yml",
            "queries: [pq]\noutput:\n  destination: {profile: inbox, path: out/pv.csv}\n",
        ),
        ("reports/fin/pv/pq.sql", "select id from accounts order by id"),
    ]);
    p.dre("run", &["pv", "--preview", "2"])
        .ok()
        .says("Preview")
        .says("not delivered");
    assert_eq!(p.read("target/run/pv/default/pv.csv"), "id\r\n0\r\n1\r\n");
    assert!(!p.path("out/pv.csv").exists());
    let r = p.json("target/run/pv/default/run_results.json");
    assert_eq!(
        (r["preview"].as_bool(), r["row_limit"].as_u64()),
        (Some(true), Some(2))
    );
    // A preview never becomes the drift baseline.
    assert!(!p.path("target/schema/pv/default/last_success.json").exists());
}

#[test]
fn schema_drift_blocks_delivery_until_accepted() {
    let p = project(&[
        (
            "reports/fin/s/s.yml",
            "queries: [sq]\noutput:\n  destination: {profile: inbox, path: out/s.csv}\n",
        ),
        (
            "reports/fin/s/sq.sql",
            "select id, name from accounts where id = 1",
        ),
    ]);
    p.dre("run", &["s"]).ok();
    std::fs::remove_file(p.path("out/s.csv")).unwrap();

    p.write(
        "reports/fin/s/sq.sql",
        "select id::varchar as id, 1 as extra from accounts where id = 1",
    );
    p.dre("run", &["s"])
        .failed()
        .says("column `id` changed type from Int64 to Utf8")
        .says("column `name` removed")
        .says("column `extra` added")
        .says("--accept-schema-change");
    assert!(!p.path("out/s.csv").exists());
    assert!(
        p.path("target/run/s/default/s.csv").exists(),
        "the output is still written to target/"
    );

    p.dre("run", &["s", "--accept-schema-change"]).ok();
    assert!(p.path("out/s.csv").exists());
    // The new schema is the baseline now.
    p.dre("run", &["s"]).ok();
}

#[test]
fn unmanaged_reports_run_with_defaults_and_warn() {
    let p = project(&[(
        "reports/scratch/quick_look.sql",
        "create temp table t as select 7 as n;\nselect * from t;\n",
    )]);
    p.dre("run", &["quick_look"])
        .ok()
        .says("`quick_look` is an unmanaged report");
    assert_eq!(
        p.read("target/run/quick_look/default/quick_look.csv"),
        "n\r\n7\r\n"
    );
}

#[test]
fn unmanaged_side_effects_are_refused_before_anything_runs() {
    let p = project(&[(
        "reports/scratch/sneaky.sql",
        "{% set t = 'accounts' %}select count(*) from {{ t }};\ndelete from {{ t }};\n",
    )]);
    p.dre("run", &["sneaky"])
        .failed()
        .says("reports/scratch/sneaky.sql:2")
        .says("found `delete from accounts`")
        .says("nothing was run");
    p.dre("run", &["sneaky", "--dry-run"])
        .failed()
        .says("found `delete from accounts`");
    // Nothing ran: the table still has every row.
    p.write("reports/scratch/sneaky.sql", "select count(*) as n from accounts");
    p.dre("run", &["sneaky"]).ok();
    assert_eq!(p.read("target/run/sneaky/default/sneaky.csv"), "n\r\n12\r\n");
}

#[test]
fn unmanaged_reports_open_read_only_unless_they_create_temp_objects() {
    let p = project(&[
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: fixture\n",
        ),
        ("reports/scratch/reads.sql", "select 1 as n"),
    ]);
    // The fixture source logs how it was opened (it treats any SQL other than its own commands
    // as an error, so this run fails after opening — which is all we need to observe).
    p.dre("run", &["reads", "-v"])
        .says("fixture: opened read_only=true");
    p.write("reports/scratch/reads.sql", "create temp table x as select 1");
    p.dre("run", &["reads", "-v"])
        .says("fixture: opened read_only=false");
}

#[test]
fn run_query_in_an_unmanaged_report_may_only_read() {
    let p = project(&[(
        "reports/scratch/sneaky_macro.sql",
        "select {{ run_query('delete from accounts') | length }} as n",
    )]);
    p.dre("run", &["sneaky_macro"])
        .failed()
        .says("run_query() in an unmanaged report may only read");
    // Nothing was deleted.
    p.write(
        "reports/scratch/sneaky_macro.sql",
        "select count(*) as n from accounts",
    );
    p.dre("run", &["sneaky_macro"]).ok();
    assert_eq!(
        p.read("target/run/sneaky_macro/default/sneaky_macro.csv"),
        "n\r\n12\r\n"
    );
}
