//! Tracer bullet: a one-query report, DuckDB → csv → target/ (and a local destination).

mod common;

use common::{DUCK_PROFILES, PLUGINS_YML, TestProject};

fn project(report_yml: &str, extra_profiles: &str) -> TestProject {
    let p = TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: warehouse\n",
            ),
            ("dependencies.yml", PLUGINS_YML),
            ("reports/ops/daily/daily.yml", report_yml),
            (
                "reports/ops/daily/summary.sql",
                "select id, name, balance\nfrom accounts\norder by id\n",
            ),
        ],
        &format!("{DUCK_PROFILES}{extra_profiles}"),
    );
    p.duckdb(
        "data.duckdb",
        "create table accounts (id int, name varchar, balance decimal(10,2));
         insert into accounts values (1, 'Acme Corp', 100.50), (2, 'Client, A', -3.25), (3, 'Beta \"B\"', 0);",
    );
    p
}

#[test]
fn a_one_query_report_writes_csv_into_target_and_records_the_run() {
    let p = project("queries: [summary]\n", "");
    p.dre("run", &["daily", "-v"])
        .ok()
        .says("no destination declared: output stays in target/run/daily/default");

    assert_eq!(
        p.read("target/run/daily/default/daily.csv"),
        "id,name,balance\r\n1,Acme Corp,100.50\r\n2,\"Client, A\",-3.25\r\n3,\"Beta \"\"B\"\"\",0.00\r\n"
    );
    assert_eq!(
        p.read("target/compiled/daily/default/summary.sql"),
        "select id, name, balance\nfrom accounts\norder by id\n"
    );

    let r = p.json("target/run/daily/default/run_results.json");
    assert_eq!(r["status"], "success");
    assert_eq!(r["report"], "daily");
    assert_eq!(r["target"], "dev");
    assert_eq!(r["preview"], false);
    assert_eq!(r["result_sets"][0]["rows"], 3);
    assert_eq!(r["outputs"][0]["path"], "target/run/daily/default/daily.csv");
    assert_eq!(
        r["outputs"][0]["size"],
        p.read("target/run/daily/default/daily.csv").len()
    );
    assert!(r["outputs"][0]["delivered_to"].is_null());
    assert!(r["duration_ms"].is_u64());
}

#[test]
fn profile_local_needs_no_profiles_entry() {
    let p = project(
        "queries: [summary]\noutput:\n  destination: {profile: local, path: out/daily.csv}\n",
        "",
    );
    p.dre("validate", &[]).ok();
    p.dre("run", &["daily"]).ok();
    assert_eq!(
        p.read("out/daily.csv"),
        p.read("target/run/daily/default/daily.csv")
    );
}

#[test]
fn a_local_destination_copies_the_file_creating_directories() {
    let p = project(
        "queries: [summary]\noutput:\n  destination: {profile: local_fs, path: out/nested/daily-report.csv}\n",
        "destinations:\n  local_fs:\n    targets:\n      dev: {type: local}\n",
    );
    p.dre("run", &["daily"]).ok();
    let delivered = p.read("out/nested/daily-report.csv");
    assert_eq!(delivered, p.read("target/run/daily/default/daily-report.csv"));
    assert!(delivered.starts_with("id,name,balance\r\n"));
    let r = p.json("target/run/daily/default/run_results.json");
    assert!(
        r["outputs"][0]["delivered_to"]
            .as_str()
            .unwrap()
            .ends_with("out/nested/daily-report.csv")
    );
}

#[test]
fn bad_sql_fails_the_run_and_is_recorded() {
    let p = project("queries: [summary]\n", "");
    p.write("reports/ops/daily/summary.sql", "select nope from accounts\n");
    p.dre("run", &["daily"])
        .failed()
        .says("nope")
        .says("0 succeeded, 1 failed");
    let r = p.json("target/run/daily/default/run_results.json");
    assert_eq!(r["status"], "error");
    assert!(
        r["error"]
            .as_str()
            .unwrap()
            .contains("reports/ops/daily/summary.sql:1"),
        "{}",
        r["error"]
    );
}

#[test]
fn clean_removes_target() {
    let p = project("queries: [summary]\n", "");
    p.dre("run", &["daily"]).ok();
    assert!(p.path("target").exists());
    p.dre("clean", &[]).ok().says("Removed target/");
    assert!(!p.path("target").exists());
    p.dre("clean", &[]).ok().says("Nothing to clean");
}

#[test]
fn an_invalid_project_does_not_run() {
    let p = project("queries: [missing_query]\n", "");
    p.dre("run", &["daily"])
        .failed()
        .says("unknown-query")
        .says("fix them before running");
    assert!(!p.path("target/run").exists());
}
