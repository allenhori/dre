//! `dre validate --live`: statements checked against the real database without executing them.

mod common;

use common::TestProject;

const PROFILES: &str = "\
sources:
  warehouse:
    target: dev
    targets:
      dev: {type: duckdb, path: data.duckdb}
  fx:
    target: dev
    targets:
      dev: {type: fixture}
";

fn project(files: &[(&str, &str)]) -> TestProject {
    let mut all = vec![
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\n",
        ),
        ("plugins.yml", "sources: [duckdb, fixture]\nformats: [csv]\n"),
    ];
    all.extend_from_slice(files);
    let p = TestProject::new(&all, PROFILES);
    p.duckdb(
        "data.duckdb",
        "create table accounts as select range as id from range(5);",
    );
    p
}

#[test]
fn a_valid_report_passes_and_nothing_is_executed() {
    let p = project(&[
        ("reports/ops/ok/ok.yml", "queries: [setup, q]\n"),
        // The temp table is actually created, so the query after it can be checked.
        (
            "reports/ops/ok/setup.sql",
            "create temp table small as select * from accounts where id < 2",
        ),
        (
            "reports/ops/ok/q.sql",
            "insert into accounts values (99);\nselect id from small",
        ),
    ]);
    p.dre("validate", &["--live"])
        .ok()
        .says("Checked")
        .says("1 succeeded, 0 failed");
    assert!(!p.path("target/run").exists(), "a live check writes no output");
    // The insert was only checked, never executed.
    p.write("reports/ops/ok/q.sql", "select count(*) as n from accounts");
    p.dre("run", &["ok"]).ok();
    assert_eq!(p.read("target/run/ok/default/ok.csv"), "n\r\n5\r\n");
}

#[test]
fn failures_are_reported_per_statement_with_file_and_line() {
    let p = project(&[
        ("reports/ops/bad/bad.yml", "queries: [q]\n"),
        (
            "reports/ops/bad/q.sql",
            "select id from accounts;\n\nselect missing_col from accounts;\nselect * from no_such_table;",
        ),
    ]);
    let r = p.dre("validate", &["--live"]);
    r.failed()
        .says("reports/ops/bad/q.sql:3")
        .says("missing_col")
        .says("reports/ops/bad/q.sql:4")
        .says("no_such_table");
    assert!(!r.stdout.contains("q.sql:1:"), "{}", r.stdout);
}

#[test]
fn a_failure_after_an_unexecuted_setup_statement_is_flagged_as_possibly_false() {
    let p = project(&[
        ("reports/ops/setup/setup.yml", "queries: [q]\n"),
        (
            "reports/ops/setup/q.sql",
            "create table staging as select * from accounts;\nselect * from staging;",
        ),
    ]);
    p.dre("validate", &["--live"])
        .failed()
        .says("q.sql:2")
        .says("may be a false positive: the setup statement at reports/ops/setup/q.sql:1 wasn't executed");
}

#[test]
fn a_source_that_cant_check_is_not_checkable_rather_than_failed() {
    let p = project(&[
        ("reports/ops/fx/fx.yml", "queries: [q]\nprofile: fx\n"),
        ("reports/ops/fx/q.sql", "rows 1"),
    ]);
    p.dre_env("validate", &["--live"], &[("DRE_FIXTURE_MODE", "no_check")])
        .ok()
        .says("not checkable");
}

#[test]
fn live_checks_accept_selectors() {
    let p = project(&[
        ("reports/ops/good/good.yml", "queries: [g]\n"),
        ("reports/ops/good/g.sql", "select 1"),
        ("reports/fin/bad/bad.yml", "queries: [b]\n"),
        ("reports/fin/bad/b.sql", "select nope from accounts"),
    ]);
    p.dre("validate", &["ops", "--live"])
        .ok()
        .says("1 succeeded, 0 failed");
    p.dre("validate", &["--live"]).failed();
}
