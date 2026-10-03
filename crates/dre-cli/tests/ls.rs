//! `dre ls`: the reports and Bindings a selection or schedule covers, offline and read-only.

mod common;

use common::TestProject;
use serde_json::{Value, json};

const PROFILES: &str = "\
connections:
  warehouse:
    targets:
      dev: {type: duckdb, path: dev.duckdb}
destinations:
  inbox:
    targets:
      dev: {type: local}
";

fn project() -> TestProject {
    TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: warehouse\n",
            ),
            ("dependencies.yml", "plugins:\n  - duckdb\n  - csv\n"),
            (
                "sets.yml",
                "client_a: {vars: {client: a}}\nclient_b: {vars: {client: b}}\n",
            ),
            (
                "schedules.yml",
                "- {name: close_a, report: monthly, set: client_a, cron: \"0 6 1 * *\"}\n\
                 - {name: regulatory, select: \"tag:regulatory\", cron: \"0 6 2 * *\"}\n",
            ),
            (
                "reports/ops/daily/daily.yml",
                "queries: [s]\ntags: [regulatory]\noutput:\n  destination: {profile: inbox, path: out/daily.csv}\n",
            ),
            ("reports/ops/daily/s.sql", "select 1 as n\n"),
            (
                "reports/finance/monthly/monthly.yml",
                "queries: [m]\nsets: [client_a, client_b]\n",
            ),
            ("reports/finance/monthly/m.sql", "select 2 as n\n"),
        ],
        PROFILES,
    )
}

/// `dre ls` data rows (the header dropped), each as its whitespace-separated columns.
fn rows(stdout: &str) -> Vec<Vec<String>> {
    stdout
        .lines()
        .skip(1)
        .map(|l| l.split_whitespace().map(str::to_string).collect())
        .collect()
}

#[test]
fn no_selector_lists_every_binding() {
    let p = project();
    let r = p.dre("ls", &[]);
    r.ok();
    assert!(r.stdout.starts_with("REPORT"), "{}", r.stdout);
    assert_eq!(
        rows(&r.stdout),
        [
            vec!["daily", "-", "warehouse", "csv", "inbox:out/daily.csv"],
            vec!["monthly", "client_a", "warehouse", "csv", "-"],
            vec!["monthly", "client_b", "warehouse", "csv", "-"],
        ]
    );
    assert!(r.stderr.is_empty(), "diagnostics only: {}", r.stderr);
}

#[test]
fn a_selector_and_a_set_narrow_it() {
    let p = project();
    let r = p.dre("ls", &["-s", "tag:regulatory"]);
    r.ok();
    assert_eq!(
        rows(&r.stdout),
        [vec!["daily", "-", "warehouse", "csv", "inbox:out/daily.csv"]]
    );

    let r = p.dre("ls", &["monthly", "--set", "client_b"]);
    r.ok();
    assert_eq!(
        rows(&r.stdout),
        [vec!["monthly", "client_b", "warehouse", "csv", "-"]]
    );
}

#[test]
fn a_schedule_lists_exactly_what_it_runs() {
    let p = project();
    let r = p.dre("ls", &["--schedule", "close_a"]);
    r.ok();
    assert_eq!(
        rows(&r.stdout),
        [vec!["monthly", "client_a", "warehouse", "csv", "-"]]
    );
}

#[test]
fn json_is_the_manifests_shape_for_what_matched() {
    let p = project();
    p.dre("compile", &["--set", "all"]).ok();
    let full = p.json("target/manifest.json");

    let r = p.dre("ls", &["--schedule", "close_a", "--output", "json"]);
    r.ok();
    let j: Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(j["schema"], full["schema"]);
    assert_eq!(j["version"], full["version"]);
    assert_eq!(j["project"], full["project"]);
    assert_eq!(j["schedules"], json!({"close_a": full["schedules"]["close_a"]}));
    assert_eq!(j["reports"].as_object().unwrap().len(), 1);
    let monthly = &j["reports"]["monthly"];
    assert_eq!(
        monthly["bindings"],
        json!([full["reports"]["monthly"]["bindings"][0]])
    );
    assert_eq!(monthly["checksum"], full["reports"]["monthly"]["checksum"]);

    // With no selector it's the whole manifest.
    let r = p.dre("ls", &["--output", "json"]);
    let j: Value = serde_json::from_str(&r.stdout).unwrap();
    let mut all = full.clone();
    all["schedules"] = json!({});
    assert_eq!(j, all);
}

#[test]
fn nothing_matched_is_an_error() {
    let p = project();
    let r = p.dre("ls", &["nope"]);
    r.failed().says("nope");
    assert!(r.stdout.is_empty());
    p.dre("ls", &["--schedule", "nope"])
        .failed()
        .says("no schedule `nope`");
    p.dre("ls", &["daily", "--set", "client_a"])
        .failed()
        .says("client_a");
}

#[test]
fn it_writes_nothing_and_needs_no_profiles() {
    let p = project();
    std::fs::remove_file(p.dir.path().join("profiles/profiles.yml")).unwrap();
    p.dre("ls", &[]).ok();
    p.dre("ls", &["--output", "json"]).ok();
    assert!(!p.path("target").exists());
    assert!(!p.path("logs").exists());
}
