//! Named schedules: `dre run --schedule`, schedule vars, `run.schedule`, the run record, and
//! secret masking in everything DRE writes.

mod common;

use common::{DUCK_PROFILES, PLUGINS_YML, TestProject};

const SCHEDULES: &str = "\
- name: flash_daily
  report: sales_summary
  set: client_a
  cron: \"0 7 * * *\"
  vars: {period: day}
- name: close_monthly
  report: sales_summary
  set: client_a
  cron: \"0 6 1 * *\"
  vars: {period: month}
- name: regulatory_monthly
  select: \"tag:regulatory\"
  cron: \"0 6 2 * *\"
";

fn project(extra: &[(&str, &str)]) -> TestProject {
    let mut files = vec![
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\nvars: {period: project, level: project}\n",
        ),
        ("dependencies.yml", PLUGINS_YML),
        (
            "sets.yml",
            "client_a: {vars: {client: client_a}}\nclient_b: {vars: {client: client_b}}\n",
        ),
        ("schedules.yml", SCHEDULES),
        (
            "reports/finance/sales_summary/sales_summary.yml",
            "queries: [summary]\nsets: [client_a, client_b]\ndefault_set: client_a\n\
             output:\n  destination: {profile: local_fs, path: \"out/{{ var('period') }}-{{ run.date.yyyymmdd }}.csv\"}\n",
        ),
        (
            "reports/finance/sales_summary/summary.sql",
            "select '{{ var('client') }}' as client, '{{ var('period') }}' as period, \
             '{{ run.schedule if run.schedule else 'none' }}' as schedule\n",
        ),
        (
            "reports/ops/filing/filing.yml",
            "queries: [f]\ntags: [regulatory]\n",
        ),
        (
            "reports/ops/filing/f.sql",
            "select '{{ var('period') }}' as period\n",
        ),
        ("reports/ops/other/other.yml", "queries: [o]\n"),
        ("reports/ops/other/o.sql", "select 1 as n\n"),
    ];
    files.extend_from_slice(extra);
    let profiles = format!(
        "{DUCK_PROFILES}destinations:\n  local_fs:\n    target: dev\n    targets:\n      dev: {{type: local}}\n"
    );
    let p = TestProject::new(&files, &profiles);
    p.duckdb("data.duckdb", "select 1;");
    p
}

fn ran(p: &TestProject) -> Vec<String> {
    let mut v: Vec<String> = walk(&p.path("target/run"));
    v.sort();
    v
}

fn walk(dir: &std::path::Path) -> Vec<String> {
    let mut out = Vec::new();
    for r in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        for b in std::fs::read_dir(r.path()).into_iter().flatten().flatten() {
            out.push(format!(
                "{}/{}",
                r.file_name().to_string_lossy(),
                b.file_name().to_string_lossy()
            ));
        }
    }
    out
}

#[test]
fn several_schedules_on_one_binding_validate_and_are_listed() {
    let p = project(&[]);
    let v = p.dre("validate", &["--json"]);
    v.ok();
    let j: serde_json::Value = serde_json::from_str(&v.stdout).unwrap();
    let report = &j["project"]["reports"]["sales_summary"];
    let a = report["bindings"]
        .as_array()
        .unwrap()
        .iter()
        .find(|b| b["set"] == "client_a")
        .unwrap();
    assert_eq!(
        a["schedules"],
        serde_json::json!(["flash_daily", "close_monthly"])
    );
}

#[test]
fn run_schedule_runs_exactly_its_bindings_with_its_vars() {
    let p = project(&[]);
    p.dre("run", &["--schedule", "close_monthly"]).ok();
    assert_eq!(ran(&p), ["sales_summary/client_a"]);
    // The schedule's vars reach SQL and output paths.
    assert_eq!(
        p.read("out/month-20260125.csv"),
        "client,period,schedule\r\nclient_a,month,close_monthly\r\n"
    );

    // A select: target runs every Binding of every matching report.
    p.dre("clean", &[]).ok();
    p.dre("run", &["--schedule", "regulatory_monthly"]).ok();
    assert_eq!(ran(&p), ["filing/default"]);
    assert_eq!(
        p.read("target/run/filing/default/filing.csv"),
        "period\r\nproject\r\n"
    );
}

#[test]
fn var_beats_schedule_vars_which_beat_everything_else() {
    let p = project(&[]);
    p.dre("run", &["--schedule", "flash_daily", "--var", "period=override"])
        .ok();
    assert_eq!(
        p.read("out/override-20260125.csv"),
        "client,period,schedule\r\nclient_a,override,flash_daily\r\n"
    );
    // Without --schedule, run.schedule is falsy and the schedule's vars don't apply.
    p.dre("run", &["sales_summary"]).ok();
    assert_eq!(
        p.read("out/project-20260125.csv"),
        "client,period,schedule\r\nclient_a,project,none\r\n"
    );
}

#[test]
fn the_run_record_and_log_carry_the_schedule_vars_and_parameters() {
    let p = project(&[]);
    p.dre("run", &["--schedule", "close_monthly", "--var", "level=cli"])
        .ok();
    let r = p.json("target/run/sales_summary/client_a/run_results.json");
    assert_eq!(r["schedule"], "close_monthly");
    assert_eq!(r["schedule_vars"], serde_json::json!({"period": "month"}));
    assert_eq!(r["vars"]["period"], "month");
    assert_eq!(r["vars"]["level"], "cli");
    assert_eq!(r["vars"]["client"], "client_a");
    assert_eq!(r["run_date"], "2026-01-25");
    assert_eq!(r["params"]["schedule"], "close_monthly");
    assert_eq!(r["params"]["vars"], serde_json::json!({"level": "cli"}));

    let log = p.read("logs/dre.log");
    assert!(log.contains("INFO  Parameters {"), "{log}");
    assert!(
        log.contains("Schedule close_monthly vars {\"period\":\"month\"}"),
        "{log}"
    );
    assert!(log.contains("Vars {"), "{log}");

    // Without a schedule, the record says so.
    p.dre("run", &["other"]).ok();
    let r = p.json("target/run/other/default/run_results.json");
    assert!(r["schedule"].is_null() && r["schedule_vars"].is_null());

    let j = p.dre("run", &["--schedule", "close_monthly", "--log-format", "json"]);
    j.ok();
    let end: serde_json::Value = j
        .stdout
        .lines()
        .map(|l| serde_json::from_str::<serde_json::Value>(l).unwrap())
        .find(|e| e["event"] == "binding_end")
        .unwrap();
    assert_eq!(end["schedule"], "close_monthly");
    assert_eq!(end["schedule_vars"]["period"], "month");
}

#[test]
fn schedule_usage_errors() {
    let p = project(&[]);
    p.dre("run", &["--schedule", "nope"])
        .failed()
        .says("no schedule `nope`; valid names: flash_daily, close_monthly, regulatory_monthly");
    assert_eq!(p.dre("run", &["--schedule", "nope"]).code, 2);
    let r = p.dre("run", &["sales_summary", "--schedule", "flash_daily"]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    assert!(r.stderr.contains("cannot be used with"), "{}", r.stderr);
    let r = p.dre("run", &["--set", "client_a", "--schedule", "flash_daily"]);
    assert_eq!(r.code, 2, "{}", r.stderr);
}

#[test]
fn validate_warns_when_two_schedules_deliver_to_the_same_path() {
    let p = project(&[]);
    p.dre("validate", &[]).ok().says("0 warnings");
    p.write(
        "reports/finance/sales_summary/sales_summary.yml",
        "queries: [summary]\nsets: [client_a, client_b]\ndefault_set: client_a\n\
         output:\n  destination: {profile: local_fs, path: \"out/monthly-{{ run.date.yyyymmdd }}.csv\"}\n",
    );
    p.dre("validate", &[])
        .ok()
        .says("schedules `flash_daily` and `close_monthly` both run report `sales_summary`, Set `client_a`, and deliver to `out/monthly-20260101.csv`");
    // A path that can't render offline is skipped, not warned about.
    p.write(
        "reports/finance/sales_summary/sales_summary.yml",
        "queries: [summary]\nsets: [client_a, client_b]\ndefault_set: client_a\n\
         output:\n  destination: {profile: local_fs, path: \"out/{{ run_query('select 1').rows[0][0] }}.csv\"}\n",
    );
    p.dre("validate", &[]).ok().says("0 warnings");
}

#[test]
fn dre_secret_env_vars_are_masked_everywhere_people_read() {
    let p = project(&[
        ("reports/ops/secret/secret.yml", "queries: [s]\n"),
        (
            "reports/ops/secret/s.sql",
            "select length('{{ env_var('DRE_SECRET_API_KEY') }}') as n, '{{ env_var('PLAIN_VALUE') }}' as plain\n",
        ),
    ]);
    let env = [
        ("DRE_SECRET_API_KEY", "hunter2-very-secret"),
        ("PLAIN_VALUE", "visible-value"),
    ];
    p.dre_env("run", &["secret", "-v"], &env).ok();
    // The database saw the real value; people see the mask.
    assert_eq!(
        p.read("target/run/secret/default/secret.csv"),
        "n,plain\r\n19,visible-value\r\n"
    );
    let compiled = p.read("target/compiled/secret/default/s.sql");
    assert!(
        compiled.contains("length('*****')") && compiled.contains("visible-value"),
        "{compiled}"
    );
    let log = p.read("logs/dre.log");
    assert!(!log.contains("hunter2") && log.contains("*****"), "{log}");
    // A failing statement's error message is masked too.
    p.write(
        "reports/ops/secret/s.sql",
        "select * from \"{{ env_var('DRE_SECRET_API_KEY') }}\"\n",
    );
    let r = p.dre_env("run", &["secret"], &env);
    r.failed();
    assert!(
        !r.stdout.contains("hunter2") && !p.read("logs/dre.log").contains("hunter2"),
        "{}",
        r.stdout
    );
    assert!(
        !p.read("target/run/secret/default/run_results.json")
            .contains("hunter2")
    );

    // Configured off: shown as is.
    p.write(
        "dre_project.yml",
        "name: acme_reports\ndefault_profile: warehouse\nvars: {period: project, level: project}\nmask_secrets: false\n",
    );
    p.write(
        "reports/ops/secret/s.sql",
        "select '{{ env_var('DRE_SECRET_API_KEY') }}' as k\n",
    );
    p.dre_env("run", &["secret", "--accept-schema-change"], &env).ok();
    assert!(
        p.read("target/compiled/secret/default/s.sql")
            .contains("hunter2-very-secret")
    );
}
