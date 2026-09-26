//! Multi-statement execution, xlsx output, verification (dry run, preview, drift) and unmanaged
//! reports at run time, end to end.

mod common;

use calamine::{Data, Reader, Xlsx, open_workbook};
use common::TestProject;

const PROFILES: &str = "\
warehouse:
  target: dev
  outputs:
    dev: {type: duckdb, path: data.duckdb}
fixture:
  target: dev
  outputs:
    dev: {type: fixture}
inbox:
  target: dev
  outputs:
    dev: {type: local}
";
const PLUGINS: &str = "sources:\n  - duckdb\n  - fixture\nformats:\n  - csv\n  - xlsx\n";

fn project(files: &[(&str, &str)]) -> TestProject {
    let mut all = vec![
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\n",
        ),
        ("plugins.yml", PLUGINS),
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
fn statements_run_in_order_on_one_session_and_only_results_become_sheets() {
    let p = project(&[
        (
            "reports/fin/monthly/monthly.yml",
            "queries:\n  - setup\n  - {query: multi, tab_name: [Summary, Detail]}\n  - {query: plain, anchor: B2, header: false}\n  - two\noutput: {format: xlsx}\n",
        ),
        // Setup: temp table + a SET, no result sets.
        (
            "reports/fin/monthly/setup.sql",
            "create temp table small as select * from accounts where id < 3;\nset threads = 1;\n",
        ),
        (
            "reports/fin/monthly/multi.sql",
            "select count(*) as n from small;\nselect id, name from small order by id;\n",
        ),
        ("reports/fin/monthly/plain.sql", "select 'x' as a;\n"),
        ("reports/fin/monthly/two.sql", "select 1 as one; select 2 as two;"),
    ]);
    p.dre("run", &["monthly"]).ok();
    let f = "target/run/monthly/default/monthly.xlsx";
    assert_eq!(sheets(&p, f), ["Summary", "Detail", "plain", "two_1", "two_2"]);
    assert_eq!(cells(&p, f, "Summary"), [["n"], ["3"]]);
    assert_eq!(cells(&p, f, "Detail")[3], ["2", "acct 2"]);
    // Anchored at B2 with no header: the only cell is B2.
    assert_eq!(cells(&p, f, "plain"), [["x"]]);
    let mut wb: Xlsx<_> = open_workbook(p.path(f)).unwrap();
    assert_eq!(wb.worksheet_range("plain").unwrap().start(), Some((1, 1)));
    let r = p.json("target/run/monthly/default/run_results.json");
    assert_eq!(r["result_sets"].as_array().unwrap().len(), 5);
}

#[test]
fn a_tab_name_list_must_match_the_number_of_result_sets() {
    let p = project(&[
        (
            "reports/fin/r/r.yml",
            "queries:\n  - {query: rq, tab_name: [OnlyOne]}\noutput: {format: xlsx}\n",
        ),
        ("reports/fin/r/rq.sql", "select 1 as a; select 2 as b;"),
    ]);
    p.dre("run", &["r"])
        .failed()
        .says("`rq` has 1 tab names but returned 2 result sets");
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
            "queries:\n  - {query: sq, tab_name: [Summary, Detail]}\noutput:\n  destination: {profile: inbox, path: out/split.csv}\n",
        ),
        ("reports/fin/split/sq.sql", "select 1 as a; select 2 as b;"),
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
    p.dre("run", &["d", "--dry-run"]).ok().says("dry run");
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
    p.dre("run", &["pv", "--preview", "2"]).ok().says("preview");
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
    p.dre("run", &["reads"]).says("fixture: opened read_only=true");
    p.write("reports/scratch/reads.sql", "create temp table x as select 1");
    p.dre("run", &["reads"]).says("fixture: opened read_only=false");
}
