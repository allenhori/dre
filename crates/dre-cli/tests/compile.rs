//! `-s/--select`, `dre compile`, and what `dre validate -s` shows about each selected Binding.

mod common;

use common::{PLUGINS_YML, TestProject};

const PROFILES: &str = "\
sources:
  warehouse:
    target: dev
    targets:
      dev: {type: duckdb, path: dev.duckdb}
      prod: {type: duckdb, path: prod.duckdb}
destinations:
  inbox:
    target: dev
    targets:
      dev: {type: local}
      prod: {type: local}
";

fn project() -> TestProject {
    let p = TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: warehouse\n",
            ),
            ("dependencies.yml", PLUGINS_YML),
            (
                "schedules.yml",
                "- {name: daily_run, report: daily, cron: \"0 6 * * *\"}\n",
            ),
            (
                "reports/ops/daily/daily.yml",
                "queries: [summary, detail]\ntags: [regulatory]\n\
                 output:\n  destination: {profile: inbox, path: \"out/daily-{{ run.date.yyyymmdd }}.csv\"}\n",
            ),
            ("reports/ops/daily/summary.sql", "select {{ 1 + 1 }} as n\n"),
            ("reports/ops/daily/detail.sql", "select 2 as m\n"),
            (
                "reports/finance/monthly/monthly.yml",
                "queries: [m]\ntags: [regulatory]\n",
            ),
            ("reports/finance/monthly/m.sql", "select 3 as x\n"),
            ("reports/finance/other/other.yml", "queries: [o]\n"),
            ("reports/finance/other/o.sql", "select 4 as y\n"),
        ],
        PROFILES,
    );
    // No database files: compiling these reports mustn't need one.
    p
}

#[test]
fn compile_writes_the_sql_and_lists_the_files_without_touching_the_database() {
    let p = project();
    let r = p.dre("compile", &["-s", "daily"]);
    r.ok()
        .says("Compiled  target/compiled/daily/default/summary.sql")
        .says("Compiled  target/compiled/daily/default/detail.sql");
    assert!(
        !r.stdout.contains("select"),
        "the SQL itself isn't printed:\n{}",
        r.stdout
    );
    assert_eq!(
        p.read("target/compiled/daily/default/summary.sql"),
        "select 2 as n\n"
    );
    assert!(!p.path("target/run").exists() && !p.path("dev.duckdb").exists());
}

#[test]
fn select_takes_several_selectors_separated_by_spaces_commas_or_semicolons() {
    let p = project();
    p.dre("compile", &["-s", "daily", "other"]).ok();
    assert!(p.path("target/compiled/daily").is_dir() && p.path("target/compiled/other").is_dir());
    assert!(!p.path("target/compiled/monthly").exists());

    p.dre("clean", &[]).ok();
    p.dre("compile", &["--select", "daily", "--select", "monthly"])
        .ok();
    assert!(p.path("target/compiled/daily").is_dir() && p.path("target/compiled/monthly").is_dir());

    // Commas and semicolons separate selectors too.
    p.dre("clean", &[]).ok();
    p.dre("compile", &["-s", "daily,other"]).ok();
    assert!(p.path("target/compiled/daily").is_dir() && p.path("target/compiled/other").is_dir());
    p.dre("clean", &[]).ok();
    p.dre("compile", &["-s", "monthly;tag:regulatory"]).ok();
    assert!(p.path("target/compiled/monthly").is_dir() && p.path("target/compiled/daily").is_dir());
    assert!(!p.path("target/compiled/other").exists());

    // The positional form still works; both at once is a usage error.
    p.dre("clean", &[]).ok();
    p.dre("compile", &["other"]).ok();
    assert!(p.path("target/compiled/other").is_dir());
    assert_eq!(p.dre("compile", &["other", "-s", "daily"]).code, 2);
}

#[test]
fn validate_compiles_and_with_select_shows_where_output_goes() {
    let p = project();
    let v = p.dre("validate", &[]);
    v.ok().says("Compiled 3 Bindings into target/compiled/");
    assert!(p.path("target/compiled/other/default/o.sql").is_file());

    let v = p.dre("validate", &["-s", "daily"]);
    v.ok()
        .says("Binding  daily")
        .says("Compiled  target/compiled/daily/default/summary.sql")
        .says("Source  warehouse (duckdb), target dev")
        .says("Output  target/run/daily/default/daily-20260125.csv (csv)")
        .says("Delivers  inbox (local), target dev → out/daily-20260125.csv")
        .says("Schedules  daily_run");
    assert!(!v.stdout.contains("monthly"), "{}", v.stdout);

    // Against prod, the same summary says so.
    p.dre("validate", &["-s", "daily", "--target", "prod"])
        .ok()
        .says("Source  warehouse (duckdb), target prod")
        .says("Delivers  inbox (local), target prod → out/daily-20260125.csv");

    // A report that doesn't render fails validation.
    p.write(
        "reports/finance/other/o.sql",
        "select {{ var('missing_at_runtime', none).x }}\n",
    );
    p.dre("validate", &[])
        .failed()
        .says("error[compile-failed]: report `other` doesn't compile");
}

#[test]
fn validate_select_that_matches_nothing_is_an_error() {
    let p = project();
    p.dre("validate", &["-s", "nothing_here"])
        .failed()
        .says("matches no report");
}
