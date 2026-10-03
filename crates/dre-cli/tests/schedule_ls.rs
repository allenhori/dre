//! `dre schedule ls` beyond the fixture table: the window defaults, the hashes a deployment
//! diffs on, and running an occurrence's own command.

mod common;

use common::{DUCK_PROFILES, PLUGINS_YML, TestProject};
use serde_json::Value;

const SCHEDULES: &str = "\
- name: close_monthly
  report: sales
  cron: \"0 6 1 * *\"
  timezone: Australia/Sydney
  vars: {period: month}
- name: flash_daily
  report: sales
  set: client_a
  cron: \"0 7 * * *\"
  timezone: Australia/Sydney
";

fn project() -> TestProject {
    let profiles = format!(
        "{DUCK_PROFILES}destinations:\n  local_fs:\n    target: dev\n    targets:\n      dev: {{type: local}}\n"
    );
    let p = TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme\ndefault_profile: warehouse\nvars: {period: none}\n",
            ),
            ("dependencies.yml", PLUGINS_YML),
            (
                "sets.yml",
                "client_a: {vars: {client: a}}\nclient_b: {vars: {client: b}}\n",
            ),
            ("schedules.yml", SCHEDULES),
            (
                "reports/sales/sales.yml",
                "queries: [summary]\nsets: [client_a, client_b]\ntimezone: Australia/Sydney\n\
                 output:\n  destination: {profile: local_fs, path: \"out/{{ var('client') }}-{{ run.date }}-{{ run.scheduled_at.format('%H%M') }}.csv\"}\n",
            ),
            (
                "reports/sales/summary.sql",
                "select '{{ var('client') }}' as client, '{{ var('period') }}' as period, '{{ run.date }}' as d, '{{ run.now.iso }}' as now\n",
            ),
        ],
        &profiles,
    );
    p.duckdb("data.duckdb", "select 1;");
    p
}

fn doc(p: &TestProject, args: &[&str]) -> Value {
    let mut all = vec!["ls", "--output", "json"];
    all.extend_from_slice(args);
    let r = p.dre("schedule", &all);
    r.ok();
    serde_json::from_str(&r.stdout).unwrap_or_else(|e| panic!("{e}: {}", r.stdout))
}

fn hashes(d: &Value) -> (String, String, String) {
    (
        d["project_hash"].as_str().unwrap().to_string(),
        d["schedules"]["close_monthly"]["definition_hash"]
            .as_str()
            .unwrap()
            .to_string(),
        d["schedules"]["flash_daily"]["definition_hash"]
            .as_str()
            .unwrap()
            .to_string(),
    )
}

#[test]
fn the_window_defaults_to_now_floored_to_the_minute_plus_35_days() {
    let p = project();
    let before = chrono::Utc::now();
    let d = doc(&p, &[]);
    let from = chrono::DateTime::parse_from_rfc3339(d["window"]["from"].as_str().unwrap()).unwrap();
    let to = chrono::DateTime::parse_from_rfc3339(d["window"]["to"].as_str().unwrap()).unwrap();
    assert_eq!(to - from, chrono::Duration::days(35));
    assert!(from.timestamp() % 60 == 0, "{from}");
    assert!(
        (before - from.with_timezone(&chrono::Utc)).num_seconds() < 120,
        "{from}"
    );
    // JSON has no default limit: every firing in the window.
    let flash = d["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["schedule"] == "flash_daily")
        .count();
    assert!((34..=36).contains(&flash), "{flash}");
    // A --to alone counts from the default --from.
    let r = p.dre("schedule", &["ls", "--to", "2000-01-01"]);
    assert_eq!(r.code, 2);
    r.says("must be after --from");
}

#[test]
fn hashes_are_stable_and_change_with_what_they_cover() {
    let p = project();
    let window = ["--from", "2026-09-28", "--to", "2026-10-28"];
    let first = doc(&p, &window);
    let (project_hash, close, _) = hashes(&first);
    // Same project, another window and filter: same hashes.
    let again = doc(&p, &["--from", "2026-01-01", "--schedule", "close_monthly"]);
    assert_eq!(again["project_hash"], project_hash.as_str());
    assert_eq!(
        again["schedules"]["close_monthly"]["definition_hash"],
        close.as_str()
    );
    assert!(again["schedules"].get("flash_daily").is_none());

    let edit = |from: &str, to: &str| {
        let text = p.read("schedules.yml").replacen(from, to, 1);
        p.write("schedules.yml", &text);
        hashes(&doc(&p, &window))
    };
    // Timing, timezone, vars and bindings each change the definition hash of that schedule only.
    for (from, to) in [
        ("cron: \"0 6 1 * *\"", "cron: \"0 5 1 * *\""),
        (
            "timezone: Australia/Sydney\n  vars",
            "timezone: Europe/London\n  vars",
        ),
        ("{period: month}", "{period: quarter}"),
        ("report: sales\n  cron", "report: sales\n  set: client_b\n  cron"),
        (
            "{period: quarter}",
            "{period: quarter}\n  except: [\"2026-10-01\"]",
        ),
        ("{period: quarter}", "{period: quarter}\n  enabled: false"),
    ] {
        let before = hashes(&doc(&p, &window));
        let after = edit(from, to);
        assert_ne!(after.0, before.0, "{to}: project hash");
        assert_ne!(after.1, before.1, "{to}: close_monthly");
        assert_eq!(after.2, before.2, "{to}: flash_daily");
    }
    // Something schedules don't cover (a report's SQL) changes nothing.
    let before = hashes(&doc(&p, &window));
    p.write("reports/sales/summary.sql", "select 2 as n\n");
    assert_eq!(hashes(&doc(&p, &window)), before);
    // A removed schedule changes the project hash and drops out of the document.
    p.write(
        "schedules.yml",
        &SCHEDULES[SCHEDULES.find("- name: flash_daily").unwrap()..],
    );
    let d = doc(&p, &window);
    assert_ne!(d["project_hash"], before.0.as_str());
    assert!(d["schedules"].get("close_monthly").is_none());
    assert_eq!(
        d["schedules"]["flash_daily"]["definition_hash"],
        before.2.as_str()
    );
}

/// Run an occurrence's command the way an orchestrator would: its argv from the project
/// directory, with its env.
fn execute(p: &TestProject, o: &Value) {
    let argv: Vec<&str> = o["invocation"]["argv"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a.as_str().unwrap())
        .collect();
    assert_eq!(argv[0], "dre");
    let env: Vec<(String, String)> = o["invocation"]["env"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, v)| (k.clone(), v.as_str().unwrap().to_string()))
        .collect();
    let env: Vec<(&str, &str)> = env.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    p.dre_env(argv[1], &argv[2..], &env).ok();
}

#[test]
fn running_an_occurrences_invocation_renders_that_firing() {
    let p = project();
    let d = doc(
        &p,
        &["--from", "2026-09-30T00:00:00Z", "--to", "2026-10-01T00:00:00Z"],
    );
    let o = d["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["schedule"] == "close_monthly")
        .unwrap()
        .clone();
    // 06:00 on the 1st in Sydney is 20:00 UTC the day before; the run is for the 1st.
    assert_eq!(o["fires_at"], "2026-09-30T20:00:00Z");
    assert_eq!(o["run_date"], "2026-10-01");
    assert_eq!(
        o["invocation"]["argv"],
        serde_json::json!(["dre", "run", "--schedule", "close_monthly"])
    );
    execute(&p, &o);
    let expected = "client,period,d,now\r\na,month,2026-10-01,2026-10-01T06:00:00+10:00\r\n";
    assert_eq!(p.read("out/a-2026-10-01-0600.csv"), expected);
    assert_eq!(
        p.read("out/b-2026-10-01-0600.csv"),
        expected.replace("\na,", "\nb,")
    );
    let r = p.json("target/run/sales/client_a/run_results.json");
    assert_eq!(r["schedule"], "close_monthly");
    assert_eq!(r["scheduled_at"], "2026-09-30T20:00:00Z");
    // Rerunning it later renders the same SQL.
    let sql = p.read("target/compiled/sales/client_a/summary.sql");
    execute(&p, &o);
    assert_eq!(p.read("target/compiled/sales/client_a/summary.sql"), sql);
}

#[test]
fn schedule_ls_needs_no_profiles_and_writes_nothing() {
    let p = project();
    std::fs::remove_dir_all(p.path("target")).ok();
    std::fs::remove_file(p.path("../profiles/profiles.yml")).unwrap();
    let r = p.dre("schedule", &["ls", "--from", "2026-09-28"]);
    r.ok();
    assert!(r.stdout.contains("close_monthly"), "{}", r.stdout);
    assert!(!p.path("target").exists());
    assert!(!p.path("logs").exists());
}

#[test]
fn occurrences_are_the_same_whatever_window_is_asked_for() {
    let p = project();
    p.write(
        "schedules.yml",
        "- {name: fortnightly, report: sales, rrule: \"FREQ=WEEKLY;INTERVAL=2;BYDAY=MO\", starting: \"2026-01-05\", at: \"09:00\"}\n\
         - {name: second_tuesday, report: sales, rrule: \"FREQ=MONTHLY;BYDAY=2TU\", at: \"07:00\"}\n\
         - {name: every_5_days, report: sales, every: {days: 5}, starting: \"2026-01-03\", at: \"06:00\"}\n\
         - {name: three_times, report: sales, rrule: \"FREQ=DAILY;COUNT=3\", starting: \"2026-03-30\", at: \"10:00\"}\n",
    );
    let keys = |from: &str, to: &str| -> Vec<String> {
        doc(&p, &["--from", from, "--to", to])["occurrences"]
            .as_array()
            .unwrap()
            .iter()
            .map(|o| o["key"].as_str().unwrap().to_string())
            .collect()
    };
    let whole = keys("2026-03-01", "2026-06-01");
    let mut parts = keys("2026-03-01", "2026-03-31T09:00:00Z");
    parts.extend(keys("2026-03-31T09:00:00Z", "2026-04-17"));
    parts.extend(keys("2026-04-17", "2026-06-01"));
    assert_eq!(parts, whole);
    assert!(whole.iter().any(|k| k.starts_with("three_times/")), "{whole:?}");
    assert_eq!(whole.iter().filter(|k| k.starts_with("three_times/")).count(), 3);
}

#[test]
fn editing_a_shared_timing_changes_every_schedule_that_uses_it() {
    let p = project();
    p.write(
        "timings.yml",
        "month_start: {cron: \"0 6 1 * *\", timezone: Australia/Sydney}\n",
    );
    p.write(
        "schedules.yml",
        "- {name: close_monthly, report: sales, set: client_a, timing: month_start}\n\
         - {name: flash_daily, report: sales, set: client_b, cron: \"0 7 * * *\"}\n",
    );
    let window = ["--from", "2026-09-28", "--to", "2026-10-28"];
    let before = doc(&p, &window);
    assert_eq!(before["schedules"]["close_monthly"]["timing"], "month_start");
    let close = before["occurrences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["schedule"] == "close_monthly")
        .unwrap();
    assert_eq!(close["timing"], "month_start");
    assert_eq!(close["timezone"], "Australia/Sydney");
    let before = hashes(&before);
    p.write(
        "timings.yml",
        "month_start: {cron: \"0 6 1 * *\", timezone: Europe/London}\n",
    );
    let after = hashes(&doc(&p, &window));
    assert_ne!(after.0, before.0);
    assert_ne!(after.1, before.1);
    assert_eq!(after.2, before.2);
}

#[test]
fn a_split_occurrence_renders_what_its_binding_renders_in_the_full_run() {
    let p = project();
    let window = [
        "--from",
        "2026-09-30T00:00:00Z",
        "--to",
        "2026-10-01T00:00:00Z",
        "--schedule",
        "close_monthly",
    ];
    let full = doc(&p, &window)["occurrences"][0].clone();
    execute(&p, &full);
    let sql = |set: &str| p.read(&format!("target/compiled/sales/{set}/summary.sql"));
    let expected = (sql("client_a"), sql("client_b"));
    std::fs::remove_dir_all(p.path("target/compiled")).unwrap();

    let mut split_args = window.to_vec();
    split_args.push("--split");
    let split = doc(&p, &split_args);
    let occurrences = split["occurrences"].as_array().unwrap();
    assert_eq!(split["split"], true);
    assert_eq!(occurrences.len(), 2);
    assert_eq!(
        occurrences[0]["key"],
        "close_monthly/2026-09-30T20:00:00Z/sales/client_a"
    );
    assert_eq!(
        occurrences[1]["invocation"]["argv"],
        serde_json::json!([
            "dre",
            "run",
            "--schedule",
            "close_monthly",
            "-s",
            "sales",
            "--set",
            "client_b"
        ])
    );
    execute(&p, &occurrences[0]);
    assert_eq!(sql("client_a"), expected.0);
    assert!(!p.path("target/compiled/sales/client_b").exists());
    execute(&p, &occurrences[1]);
    assert_eq!(sql("client_b"), expected.1);
}
