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
    // Narrowing a schedule to something it doesn't run names what it does run.
    p.dre("run", &["other", "--schedule", "flash_daily"])
        .failed()
        .says("schedule `flash_daily` doesn't run `other`; it runs: sales_summary/client_a");
    p.dre("run", &["--set", "client_b", "--schedule", "flash_daily"])
        .failed()
        .says("schedule `flash_daily` doesn't run Set `client_b`; it runs: sales_summary/client_a");
    p.dre("run", &["-s", "sales_summary", "--set", "client_b", "--schedule", "close_monthly"])
        .failed()
        .says("schedule `close_monthly` doesn't run `sales_summary` with Set `client_b`; it runs: sales_summary/client_a");
}

#[test]
fn a_schedule_narrowed_to_one_binding_keeps_its_vars_and_timezone() {
    let p = project(&[]);
    let mut schedules = SCHEDULES.to_string();
    schedules.push_str(
        "- name: both_clients\n  report: sales_summary\n  cron: \"0 8 * * *\"\n  timezone: Pacific/Kiritimati\n  vars: {period: both}\n",
    );
    p.write("schedules.yml", &schedules);
    p.write(
        "reports/finance/sales_summary/summary.sql",
        "select '{{ var('client') }}' as client, '{{ var('period') }}' as period, '{{ run.timezone }}' as tz\n",
    );
    p.dre(
        "run",
        &[
            "--schedule",
            "both_clients",
            "-s",
            "sales_summary",
            "--set",
            "client_b",
        ],
    )
    .ok();
    assert_eq!(ran(&p), ["sales_summary/client_b"]);
    assert_eq!(
        p.read("out/both-20260125.csv"),
        "client,period,tz\r\nclient_b,both,Pacific/Kiritimati\r\n"
    );
    let r = p.json("target/run/sales_summary/client_b/run_results.json");
    assert_eq!(r["schedule"], "both_clients");
    assert_eq!(r["params"]["set"], "client_b");
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

/// What a JSON serializer writes for `s`, without the surrounding quotes.
fn json_escaped(s: &str) -> String {
    let q = serde_json::to_string(s).unwrap();
    q[1..q.len() - 1].to_string()
}

#[test]
fn dre_secrets_needing_json_escapes_never_reach_json_output() {
    let p = project(&[
        ("reports/ops/secret/secret.yml", "queries: [s]\n"),
        (
            "reports/ops/secret/s.sql",
            "select 1 {{ env_var('DRE_SECRET_SHORT') }}\n-- {{ env_var('DRE_SECRET_LONG') }}\n",
        ),
    ]);
    // Quote, backslash, newline, tab, non-ASCII; the first secret is a substring of the second.
    let short = "q\"\\\n\té";
    let long = format!("{short}-and-more");
    let env = [("DRE_SECRET_SHORT", short), ("DRE_SECRET_LONG", long.as_str())];
    let r = p.dre_env(
        "run",
        &["secret", "--log-format", "json", "--color", "never"],
        &env,
    );
    r.failed();
    let surfaces = [
        ("stdout", r.stdout.clone()),
        (
            "run_results.json",
            p.read("target/run/secret/default/run_results.json"),
        ),
        ("dre.log", p.read("logs/dre.log")),
        ("compiled sql", p.read("target/compiled/secret/default/s.sql")),
    ];
    for (name, text) in &surfaces {
        for secret in [short, long.as_str()] {
            for form in [secret.to_string(), json_escaped(secret)] {
                assert!(!text.contains(&form), "{name} leaks {form:?}:\n{text}");
            }
        }
    }
    // The JSON stays valid, and the failing query's error is in it.
    for line in r.stdout.lines() {
        serde_json::from_str::<serde_json::Value>(line).unwrap_or_else(|e| panic!("{e}: {line}"));
    }
    let results = p.json("target/run/secret/default/run_results.json");
    assert_eq!(results["status"], "error", "{results}");
    assert!(surfaces[3].1.contains("*****"), "{}", surfaces[3].1);

    // Configured off: the value is written as is.
    p.write(
        "dre_project.yml",
        "name: acme_reports\ndefault_profile: warehouse\nvars: {period: project, level: project}\nmask_secrets: false\n",
    );
    p.dre_env("run", &["secret", "--log-format", "json"], &env)
        .failed();
    assert!(p.read("target/compiled/secret/default/s.sql").contains(&long));
    p.json("target/run/secret/default/run_results.json");
}

/// A report whose SQL and output path show what a pinned firing renders.
fn pinned_project() -> TestProject {
    project(&[
        (
            "reports/ops/intraday/intraday.yml",
            "queries: [i]\ntimezone: Australia/Sydney\n\
             output:\n  destination: {profile: local_fs, path: \"out/intraday-{{ run.scheduled_at.format('%Y%m%d%H%M') }}.csv\"}\n",
        ),
        (
            "reports/ops/intraday/i.sql",
            "select '{{ run.date }}' as d, '{{ run.now.iso }}' as now, '{{ run.scheduled_at.iso }}' as at\n",
        ),
    ])
}

#[test]
fn dre_run_at_pins_run_now_the_run_date_and_scheduled_at() {
    let p = pinned_project();
    // 07:00 UTC is 18:00 in Sydney (AEDT), so the run date is Sydney's.
    let env = [("DRE_RUN_DATE", ""), ("DRE_RUN_AT", "2026-01-25T07:00:00Z")];
    p.dre_env("run", &["intraday"], &env).ok();
    let out = p.read("out/intraday-202601251800.csv");
    assert_eq!(
        out,
        "d,now,at\r\n2026-01-25,2026-01-25T18:00:00+11:00,2026-01-25T18:00:00+11:00\r\n"
    );
    let r = p.json("target/run/intraday/default/run_results.json");
    assert_eq!(r["scheduled_at"], "2026-01-25T07:00:00Z");
    assert_eq!(r["run_date"], "2026-01-25");
    assert_eq!(r["params"]["scheduled_at"], "2026-01-25T07:00:00Z");
    // started_at is when it really ran, not the pinned instant.
    assert_ne!(r["started_at"], "2026-01-25T07:00:00Z");

    // A rerun with the same DRE_RUN_AT renders identical SQL.
    let first = p.read("target/compiled/intraday/default/i.sql");
    p.dre_env("run", &["intraday"], &env).ok();
    assert_eq!(p.read("target/compiled/intraday/default/i.sql"), first);

    // An offset instant means the same thing.
    p.dre_env(
        "run",
        &["intraday"],
        &[("DRE_RUN_DATE", ""), ("DRE_RUN_AT", "2026-01-25T18:00:00+11:00")],
    )
    .ok();
    assert_eq!(p.read("target/compiled/intraday/default/i.sql"), first);
}

#[test]
fn an_explicit_dre_run_date_beats_dre_run_at() {
    let p = pinned_project();
    let env = [
        ("DRE_RUN_DATE", "2026-01-01"),
        ("DRE_RUN_AT", "2026-01-25T07:00:00Z"),
    ];
    p.dre_env("run", &["intraday"], &env).ok();
    assert_eq!(
        p.read("out/intraday-202601251800.csv"),
        "d,now,at\r\n2026-01-01,2026-01-25T18:00:00+11:00,2026-01-25T18:00:00+11:00\r\n"
    );
}

#[test]
fn run_scheduled_at_is_none_without_dre_run_at_and_the_record_holds_the_schedule() {
    let p = project(&[
        ("reports/ops/plain/plain.yml", "queries: [q]\n"),
        (
            "reports/ops/plain/q.sql",
            "select '{{ run.scheduled_at is none }}' as unset\n",
        ),
    ]);
    p.dre("run", &["plain"]).ok();
    assert_eq!(p.read("target/run/plain/default/plain.csv"), "unset\r\nTrue\r\n");
    let r = p.json("target/run/plain/default/run_results.json");
    assert!(r["scheduled_at"].is_null(), "{r}");

    p.dre_env(
        "run",
        &["--schedule", "close_monthly"],
        &[("DRE_RUN_DATE", ""), ("DRE_RUN_AT", "2026-02-01T06:00:00Z")],
    )
    .ok();
    let r = p.json("target/run/sales_summary/client_a/run_results.json");
    assert_eq!(r["schedule"], "close_monthly");
    assert_eq!(r["scheduled_at"], "2026-02-01T06:00:00Z");
    assert_eq!(r["run_date"], "2026-02-01");
}

#[test]
fn an_invalid_dre_run_at_fails_clearly() {
    let p = project(&[]);
    for cmd in ["run", "compile", "validate"] {
        let r = p.dre_env(cmd, &[], &[("DRE_RUN_AT", "tomorrow 6am")]);
        assert_eq!(r.code, 2, "{cmd}: {}{}", r.stdout, r.stderr);
        r.says("DRE_RUN_AT: `tomorrow 6am` isn't an RFC 3339 date-time (e.g. 2026-09-01T06:00:00Z)");
    }
}

#[test]
fn source_errors_do_not_echo_secret_fragments() {
    let p = project(&[
        ("reports/ops/secret/secret.yml", "queries: [s]\n"),
        (
            "reports/ops/secret/s.sql",
            "select * from \"{{ env_var('DRE_SECRET_API_KEY') }}\"\n",
        ),
    ]);
    let secret = "ISSUE69-UNIQUE-SECRET-0123456789abcdefghijklmnopqrstuvwxyz-ABCDEFGHIJKLMNOPQRSTUVWXYZ-END";
    let env = [("DRE_SECRET_API_KEY", secret)];
    let r = p.dre_env(
        "run",
        &["secret", "--log-format", "json", "--color", "never"],
        &env,
    );
    r.failed();
    let results = p.read("target/run/secret/default/run_results.json");
    let log = p.read("logs/dre.log");
    for (name, text) in [("stdout", &r.stdout), ("run results", &results), ("log", &log)] {
        for fragment in ["ISSUE69-UNIQUE-SECRET", "ABCDEFGHIJKLMNOPQRSTUVWXYZ-END"] {
            assert!(!text.contains(fragment), "{name} leaks {fragment:?}:\n{text}");
        }
        assert!(
            text.contains(dre_core::secrets::MASKED_SOURCE_ERROR),
            "{name}:\n{text}"
        );
    }

    // A failing statement without a secret keeps its useful database detail.
    p.write(
        "reports/ops/secret/s.sql",
        "select * from definitely_missing_table\n",
    );
    let ordinary = p.dre_env("run", &["secret", "--log-format", "json"], &env);
    ordinary.failed();
    assert!(
        ordinary.stdout.contains("definitely_missing_table"),
        "{}",
        ordinary.stdout
    );

    // Configured off: preserve the database diagnostic, including its shortened SQL context.
    p.write(
        "dre_project.yml",
        "name: acme_reports\ndefault_profile: warehouse\nvars: {period: project, level: project}\nmask_secrets: false\n",
    );
    p.write(
        "reports/ops/secret/s.sql",
        "select * from \"{{ env_var('DRE_SECRET_API_KEY') }}\"\n",
    );
    let r = p.dre_env("run", &["secret", "--log-format", "json"], &env);
    r.failed();
    assert!(r.stdout.contains("ISSUE69-UNIQUE-SECRET"), "{}", r.stdout);
    assert!(
        p.read("target/run/secret/default/run_results.json")
            .contains("ISSUE69-UNIQUE-SECRET")
    );
}

#[test]
fn splitting_sql_cannot_split_a_secret_out_of_protection() {
    let p = project(&[(
        "reports/ops/unmanaged_secret.sql",
        "select * from {{ env_var('DRE_SECRET_TABLE') }}\n",
    )]);
    let secret = "ISSUE69-SPLIT-SECRET;TAIL";
    let env = [("DRE_SECRET_TABLE", secret)];
    let r = p.dre_env(
        "run",
        &["unmanaged_secret", "--log-format", "json", "--color", "never"],
        &env,
    );
    r.failed();
    let results = p.read("target/run/unmanaged_secret/default/run_results.json");
    let log = p.read("logs/dre.log");
    for (name, text) in [("stdout", &r.stdout), ("run results", &results), ("log", &log)] {
        for fragment in ["ISSUE69-SPLIT-SECRET", "TAIL"] {
            assert!(!text.contains(fragment), "{name} leaks {fragment:?}:\n{text}");
        }
        assert!(text.contains("*****"), "{name}:\n{text}");
    }
}

#[test]
fn a_schedule_using_a_shared_timing_runs_in_the_timings_timezone() {
    let p = project(&[
        (
            "timings.yml",
            "early: {cron: \"0 6 * * *\", timezone: Pacific/Kiritimati}\n",
        ),
        ("reports/ops/zoned/zoned.yml", "queries: [z]\n"),
        (
            "reports/ops/zoned/z.sql",
            "select '{{ run.timezone }}' as tz, '{{ run.schedule }}' as s\n",
        ),
    ]);
    let mut schedules = SCHEDULES.to_string();
    schedules.push_str("- {name: zoned_early, report: zoned, timing: early}\n");
    p.write("schedules.yml", &schedules);
    p.dre("run", &["--schedule", "zoned_early"]).ok();
    assert_eq!(
        p.read("target/run/zoned/default/zoned.csv"),
        "tz,s\r\nPacific/Kiritimati,zoned_early\r\n"
    );
}

#[test]
fn secret_sql_is_masked_before_internal_summaries_are_truncated() {
    let p = project(&[(
        "reports/ops/unmanaged_secret.sql",
        "{% set table = env_var('DRE_SECRET_TABLE') %}\ninsert into \"{{ table }}\" values (1)\n",
    )]);
    let secret =
        "ISSUE69-INTERNAL-SUMMARY-0123456789abcdefghijklmnopqrstuvwxyz-ABCDEFGHIJKLMNOPQRSTUVWXYZ-END";
    let env = [("DRE_SECRET_TABLE", secret)];
    let r = p.dre_env("run", &["unmanaged_secret"], &env);
    r.failed();
    let output = format!("{}{}", r.stdout, r.stderr);
    let log = p.read("logs/dre.log");
    for (name, text) in [("output", output.as_str()), ("log", log.as_str())] {
        assert!(!text.contains("ISSUE69-INTERNAL-SUMMARY"), "{name}:\n{text}");
        assert!(text.contains("*****"), "{name}:\n{text}");
    }
}

#[test]
fn schema_snapshots_redact_secrets_without_false_drift() {
    let p = project(&[
        ("reports/ops/secret/secret.yml", "queries: [s]\n"),
        (
            "reports/ops/secret/s.sql",
            "select 1 as \"{{ env_var('DRE_SECRET_ALIAS') }}\"\n",
        ),
    ]);
    let secret = "ISSUE69-SCHEMA-SECRET-ALIAS";
    let env = [("DRE_SECRET_ALIAS", secret)];

    // Simulate a pre-fix snapshot, written while masking is disabled.
    p.write(
        "dre_project.yml",
        "name: acme_reports\ndefault_profile: warehouse\nvars: {period: project, level: project}\nmask_secrets: false\n",
    );
    p.dre_env("run", &["secret"], &env).ok();
    let snapshot_path = "target/schema/secret/default/last_success.json";
    assert!(p.read(snapshot_path).contains(secret));

    // Migration happens before drift can block delivery, so even a failed run sanitizes the old
    // persisted file.
    p.write(
        "dre_project.yml",
        "name: acme_reports\ndefault_profile: warehouse\nvars: {period: project, level: project}\n",
    );
    p.write(
        "reports/ops/secret/s.sql",
        "select 1::BIGINT as \"{{ env_var('DRE_SECRET_ALIAS') }}\"\n",
    );
    p.dre_env("run", &["secret"], &env).failed().says("schema drift");
    let migrated = p.read(snapshot_path);
    assert!(!migrated.contains(secret), "{migrated}");
    assert!(migrated.contains("*****"), "{migrated}");

    p.dre_env("run", &["secret", "--accept-schema-change"], &env).ok();
    p.dre_env("run", &["secret"], &env).ok();
    let snapshot = p.read(snapshot_path);
    assert!(!snapshot.contains(secret), "{snapshot}");
    assert!(snapshot.contains("*****"), "{snapshot}");
    let results = p.read("target/run/secret/default/run_results.json");
    assert!(!results.contains(secret), "{results}");
    assert!(results.contains("*****"), "{results}");

    // Conservative baseline matching keeps the same database name protected when the environment
    // no longer registers it as a secret.
    p.write(
        "reports/ops/secret/s.sql",
        &format!("select 1::BIGINT as \"{secret}\"\n"),
    );
    p.dre("run", &["secret"]).ok();
    let snapshot = p.read(snapshot_path);
    assert!(!snapshot.contains(secret), "{snapshot}");
    assert!(snapshot.contains("*****"), "{snapshot}");

    // Result data is operational output, not a diagnostic surface, and remains unchanged.
    assert!(p.read("target/run/secret/default/secret.csv").starts_with(secret));

    // The explicit project opt-out still allows the raw schema name after accepting the expected
    // transition away from a protected baseline.
    p.write(
        "dre_project.yml",
        "name: acme_reports\ndefault_profile: warehouse\nvars: {period: project, level: project}\nmask_secrets: false\n",
    );
    p.dre("run", &["secret", "--accept-schema-change"]).ok();
    assert!(p.read(snapshot_path).contains(secret));
}

#[test]
fn newly_secret_schema_names_are_not_restored_from_the_baseline() {
    let secret = "ISSUE69-LATER-SECRET-ALIAS";
    let p = project(&[
        ("reports/ops/secret/secret.yml", "queries: [s]\n"),
        (
            "reports/ops/secret/s.sql",
            "select 1::INTEGER as \"ISSUE69-LATER-SECRET-ALIAS\"\n",
        ),
    ]);
    let snapshot_path = "target/schema/secret/default/last_success.json";

    p.dre("run", &["secret"]).ok();
    assert!(p.read(snapshot_path).contains(secret));

    p.write(
        "reports/ops/secret/s.sql",
        "select 1::INTEGER as \"{{ env_var('DRE_SECRET_ALIAS') }}\"\n",
    );
    p.dre_env("run", &["secret"], &[("DRE_SECRET_ALIAS", secret)])
        .ok();
    let snapshot = p.read(snapshot_path);
    assert!(!snapshot.contains(secret), "{snapshot}");
    assert!(snapshot.contains("*****"), "{snapshot}");
}

#[test]
fn nested_schema_names_stay_protected_without_hiding_type_drift() {
    let secret = "ISSUE69-NESTED-SECRET-FIELD";
    let env = [("DRE_SECRET_ALIAS", secret)];
    let p = project(&[
        ("reports/ops/secret/secret.yml", "queries: [s]\n"),
        (
            "reports/ops/secret/s.sql",
            "select {'{{ env_var('DRE_SECRET_ALIAS') }}': 1::INTEGER} as payload\n",
        ),
    ]);
    let snapshot_path = "target/schema/secret/default/last_success.json";

    p.dre_env("run", &["secret"], &env).ok();
    assert!(!p.read(snapshot_path).contains(secret));

    p.write(
        "reports/ops/secret/s.sql",
        &format!("select {{'{secret}': 1::INTEGER}} as payload\n"),
    );
    p.dre("run", &["secret"]).ok();
    let snapshot = p.read(snapshot_path);
    assert!(!snapshot.contains(secret), "{snapshot}");

    p.write(
        "reports/ops/secret/s.sql",
        &format!("select {{'{secret}': 1::BIGINT}} as payload\n"),
    );
    let changed = p.dre("run", &["secret"]);
    changed.failed().says("changed type");
    let output = format!("{}{}", changed.stdout, changed.stderr);
    assert!(!output.contains(secret), "{output}");
}

#[test]
fn an_unwritable_schema_snapshot_does_not_turn_delivery_into_a_failed_run() {
    let p = project(&[
        ("reports/ops/example/example.yml", "queries: [q]\n"),
        ("reports/ops/example/q.sql", "select 1 as value\n"),
    ]);
    let snapshot_path = "target/schema/example/default/last_success.json";

    p.dre("run", &["example"]).ok();
    std::fs::remove_file(p.path(snapshot_path)).unwrap();
    std::fs::create_dir(p.path(snapshot_path)).unwrap();

    p.dre("run", &["example"])
        .ok()
        .says("delivery completed, but the schema snapshot could not be written");
}

#[test]
fn masked_schema_name_collisions_remain_distinct_and_detect_drift() {
    let p = project(&[
        ("reports/ops/secret/secret.yml", "queries: [s]\n"),
        (
            "reports/ops/secret/s.sql",
            "select 1::INTEGER as a, 2::BIGINT as a, 3::INTEGER as \"{{ env_var('DRE_SECRET_ALIAS') }}\", 'x'::VARCHAR as \"*****\", true as \"*****#2\"\n",
        ),
    ]);
    // This secret deliberately equals a snapshot structural key. Structural keys must remain
    // intact while the secret-derived column name is protected.
    let secret = "columns";
    let env = [("DRE_SECRET_ALIAS", secret)];
    p.dre_env("run", &["secret"], &env).ok();

    let snapshot_path = "target/schema/secret/default/last_success.json";
    let snapshot = p.read(snapshot_path);
    let parsed: serde_json::Value = serde_json::from_str(&snapshot).unwrap();
    let names: Vec<_> = parsed["result_sets"][0]["columns"]
        .as_array()
        .unwrap()
        .iter()
        .map(|column| column["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a", "a", "*****", "*****", "*****#2"], "{snapshot}");
    let columns = parsed["result_sets"][0]["columns"].as_array().unwrap();
    assert_eq!(columns[2]["name_protected"], true);
    assert!(columns.iter().all(|column| column.get("identity").is_none()));
    assert!(columns.iter().all(|column| column.get("type_shape").is_some()));

    // With the secret absent, conservative matching, a reordered protected column, and duplicate
    // ordinary names produce no false drift.
    p.write(
        "reports/ops/secret/s.sql",
        "select 3::INTEGER as \"columns\", 1::INTEGER as a, 2::BIGINT as a, 'x'::VARCHAR as \"*****\", true as \"*****#2\"\n",
    );
    p.dre("run", &["secret"]).ok();

    p.write(
        "reports/ops/secret/s.sql",
        "select 3::BIGINT as \"columns\", 1::INTEGER as a, 2::BIGINT as a, 'x'::VARCHAR as \"*****\", true as \"*****#2\"\n",
    );
    p.dre("run", &["secret"]).failed().says("changed type");
}
