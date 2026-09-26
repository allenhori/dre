//! Sets, targets, delivery rules and selector runs, end to end.

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
    target: prod
    targets:
      prod: {type: local}
  dev_inbox:
    target: dev
    targets:
      dev: {type: local}
      prod: {type: local}
";

fn project(files: &[(&str, &str)]) -> TestProject {
    let mut all = vec![
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\n",
        ),
        ("plugins.yml", PLUGINS_YML),
        (
            "sets.yml",
            "client_a: {profile: warehouse, vars: {client: client_a}}\nclient_b: {profile: warehouse, vars: {client: client_b}}\n",
        ),
    ];
    all.extend_from_slice(files);
    let p = TestProject::new(&all, PROFILES);
    p.duckdb("dev.duckdb", "create table env as select 'dev' as name;");
    p.duckdb("prod.duckdb", "create table env as select 'prod' as name;");
    p
}

const MONTHLY: (&str, &str) = (
    "reports/finance/monthly/monthly.yml",
    "queries: [q1, q2]\n\
     sets:\n\
     \x20 - client_a\n\
     \x20 - name: client_b\n\
     \x20   exclude: [q2]\n\
     \x20   tab_names: {q1: B_Summary}\n\
     \x20   output:\n\
     \x20     destination: {profile: dev_inbox, path: out/b/monthly.csv}\n",
);
const Q1: (&str, &str) = (
    "reports/finance/monthly/q1.sql",
    "select '{{ var('client') }}' as client, name from env\n",
);
const Q2: (&str, &str) = ("reports/finance/monthly/q2.sql", "select 2 as second\n");

#[test]
fn several_sets_without_a_default_fail_outside_a_terminal() {
    let p = project(&[MONTHLY, Q1, Q2]);
    p.dre("run", &["monthly"])
        .failed()
        .says("has several Sets (client_a, client_b)")
        .says("--set");
}

#[test]
fn default_set_wins_and_a_single_set_needs_no_flag() {
    let p = project(&[MONTHLY, Q1, Q2]);
    p.write(
        "reports/finance/monthly/monthly.yml",
        &format!("default_set: client_b\n{}", MONTHLY.1),
    );
    p.dre("run", &["monthly"]).ok();
    assert!(p.path("target/run/monthly/client_b").exists());
    assert!(!p.path("target/run/monthly/client_a").exists());

    let p = project(&[
        ("reports/ops/one/one.yml", "queries: [oq]\nsets: [client_a]\n"),
        ("reports/ops/one/oq.sql", "select 1 as n\n"),
    ]);
    p.dre("run", &["one"]).ok();
    assert!(p.path("target/run/one/client_a/one.csv").exists());
}

#[test]
fn set_all_runs_every_binding_with_its_own_overrides() {
    let p = project(&[MONTHLY, Q1, Q2]);
    p.dre("run", &["monthly", "--set", "all"]).ok();
    // client_a: both queries → two result sets → one csv per result set.
    assert_eq!(
        p.read("target/run/monthly/client_a/monthly_q1.csv"),
        "client,name\r\nclient_a,dev\r\n"
    );
    assert_eq!(
        p.read("target/run/monthly/client_a/monthly_q2.csv"),
        "second\r\n2\r\n"
    );
    // client_b: q2 excluded, its own vars, its own destination.
    assert_eq!(
        p.read("target/run/monthly/client_b/monthly.csv"),
        "client,name\r\nclient_b,dev\r\n"
    );
    assert_eq!(p.read("out/b/monthly.csv"), "client,name\r\nclient_b,dev\r\n");
    let r = p.json("target/run/monthly/client_b/run_results.json");
    assert_eq!(r["result_sets"][0]["name"], "B_Summary");
}

#[test]
fn set_narrows_to_one_binding() {
    let p = project(&[MONTHLY, Q1, Q2]);
    p.dre("run", &["monthly", "--set", "client_a"]).ok();
    assert!(p.path("target/run/monthly/client_a").exists());
    assert!(!p.path("target/run/monthly/client_b").exists());
}

#[test]
fn target_switches_source_and_destination_profiles() {
    let p = project(&[
        (
            "reports/ops/t/t.yml",
            "queries: [tq]\noutput:\n  destination: {profile: inbox, path: out/t.csv}\n",
        ),
        ("reports/ops/t/tq.sql", "select name from env\n"),
    ]);
    // Default target: source dev, destination profile's own default (prod) delivers.
    p.dre("run", &["t"]).ok();
    assert_eq!(p.read("out/t.csv"), "name\r\ndev\r\n");
    std::fs::remove_file(p.path("out/t.csv")).unwrap();
    // --target prod reads prod data and delivers.
    p.dre("run", &["t", "--target", "prod"]).ok();
    assert_eq!(p.read("out/t.csv"), "name\r\nprod\r\n");
    std::fs::remove_file(p.path("out/t.csv")).unwrap();
    // --target dev: the destination profile has no dev target → nothing delivered.
    p.dre("run", &["t", "--target", "dev"])
        .ok()
        .says("destination profile `inbox` has no `dev` target: not delivered, output stays in target/");
    assert!(!p.path("out/t.csv").exists());
    assert_eq!(p.read("target/run/t/default/t.csv"), "name\r\ndev\r\n");
}

#[test]
fn an_ad_hoc_set_runs_with_profile_and_vars_from_flags() {
    let p = project(&[MONTHLY, Q1, Q2]);
    p.dre(
        "run",
        &[
            "monthly",
            "--set",
            "client_d",
            "--profile",
            "warehouse",
            "--var",
            "client=client_d",
            "--target",
            "prod",
        ],
    )
    .ok();
    assert_eq!(
        p.read("target/run/monthly/client_d/monthly_q1.csv"),
        "client,name\r\nclient_d,prod\r\n"
    );
}

#[test]
fn a_failed_binding_does_not_stop_the_others() {
    let p = project(&[MONTHLY, Q1, Q2]);
    p.write("reports/finance/monthly/q2.sql", "select * from missing_table\n");
    p.dre("run", &["monthly", "--set", "all"])
        .failed()
        .says("1 succeeded, 1 failed");
    assert_eq!(
        p.json("target/run/monthly/client_a/run_results.json")["status"],
        "error"
    );
    assert_eq!(
        p.json("target/run/monthly/client_b/run_results.json")["status"],
        "success"
    );
}

fn many_reports() -> TestProject {
    project(&[
        (
            "reports/finance/monthly/fin_monthly/fin_monthly.yml",
            "queries: [a]\ntags: [regulatory]\n",
        ),
        ("reports/finance/monthly/fin_monthly/a.sql", "select 1 as n\n"),
        (
            "reports/ops/monthly/ops_monthly/ops_monthly.yml",
            "queries: [b]\n",
        ),
        ("reports/ops/monthly/ops_monthly/b.sql", "select 1 as n\n"),
        (
            "reports/ops/daily/daily.yml",
            "queries: [c]\ntags: [regulatory]\n",
        ),
        ("reports/ops/daily/c.sql", "select 1 as n\n"),
    ])
}

fn ran(p: &TestProject) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(p.path("target/run"))
        .map(|d| {
            d.map(|e| e.unwrap().file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

#[test]
fn selectors_pick_reports_by_name_path_tag_or_everything() {
    let p = many_reports();
    p.dre("run", &["finance.monthly"]).ok();
    assert_eq!(ran(&p), ["fin_monthly"]);
    p.dre("clean", &[]).ok();
    p.dre("run", &["tag:regulatory"]).ok();
    assert_eq!(ran(&p), ["daily", "fin_monthly"]);
    p.dre("clean", &[]).ok();
    p.dre("run", &["ops"]).ok();
    assert_eq!(ran(&p), ["daily", "ops_monthly"]);
    p.dre("clean", &[]).ok();
    p.dre("run", &[]).ok().says("3 succeeded, 0 failed");
    assert_eq!(ran(&p), ["daily", "fin_monthly", "ops_monthly"]);
}

#[test]
fn an_ambiguous_selector_gives_the_same_error_as_validate() {
    let p = many_reports();
    let expected = "\"monthly\" matches more than one location — use the dotted form to disambiguate:\n  \
                    finance.monthly   (reports/finance/monthly/)\n  ops.monthly       (reports/ops/monthly/)";
    // Same message from the same resolver; only the surrounding indentation differs.
    let flat = |s: &str| s.lines().map(str::trim).collect::<Vec<_>>().join("\n");
    let run = p.dre("run", &["monthly"]);
    run.failed();
    assert!(flat(&run.stdout).contains(&flat(expected)), "{}", run.stdout);
    p.write("schedules.yml", "- select: monthly\n  cron: \"0 6 1 * *\"\n");
    let v = p.dre("validate", &[]);
    v.failed();
    assert!(flat(&v.stdout).contains(&flat(expected)), "{}", v.stdout);
}
