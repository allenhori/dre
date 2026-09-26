//! Several destinations per output: each is delivered independently and recorded.

mod common;

use common::TestProject;
use serde_json::{Value, json};

/// `rec` records deliveries in `<tmp>/rec`; `broken` always fails; `mail` has only a prod output.
fn profiles(rec: &str) -> String {
    format!(
        "warehouse:\n  target: dev\n  outputs:\n    dev: {{type: duckdb, path: data.duckdb}}\n\
         inbox:\n  target: dev\n  outputs:\n    dev: {{type: local}}\n\
         rec:\n  target: dev\n  outputs:\n    dev: {{type: fixture, dir: \"{rec}\"}}\n\
         broken:\n  target: dev\n  outputs:\n    dev: {{type: fixture, dir: \"{rec}\", fail: true}}\n\
         mail:\n  target: prod\n  outputs:\n    prod: {{type: fixture, dir: \"{rec}\"}}\n"
    )
}

fn project(report_yml: &str, sql: &[(&str, &str)]) -> (TestProject, std::path::PathBuf) {
    let rec_dir = tempfile::tempdir().unwrap().keep();
    let rec = rec_dir.to_string_lossy().replace('\\', "/");
    let mut files = vec![
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\nvars: {team: finance}\n",
        ),
        (
            "plugins.yml",
            "sources: [duckdb]\nformats: [csv]\ndestinations: [fixture]\n",
        ),
        ("reports/ops/daily/daily.yml", report_yml),
    ];
    for (name, body) in sql {
        files.push((name, body));
    }
    let p = TestProject::new(&files, &profiles(&rec));
    p.duckdb("data.duckdb", "select 1;");
    (p, rec_dir)
}

fn deliveries(rec: &std::path::Path) -> Vec<Value> {
    std::fs::read_to_string(rec.join("deliveries.jsonl"))
        .unwrap_or_default()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

fn results(p: &TestProject) -> Value {
    p.json("target/run/daily/default/run_results.json")
}

const Q: (&str, &str) = ("reports/ops/daily/q.sql", "select 1 as n");

#[test]
fn a_list_delivers_to_every_destination_and_records_each() {
    let (p, rec) = project(
        "queries: [q]\noutput:\n  destination:\n\
         \x20   - {profile: inbox, path: out/daily.csv}\n\
         \x20   - {profile: rec, path: \"archive/{{ run.report }}.csv\", to: \"{{ var('team') }}@example.com\", subject: Daily}\n",
        &[Q],
    );
    p.dre("run", &["daily"]).ok();
    assert_eq!(p.read("out/daily.csv"), "n\r\n1\r\n");
    let d = deliveries(&rec);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["files"][0]["remote"], "archive/daily.csv");
    assert_eq!(
        d[0]["options"],
        json!({"to": "finance@example.com", "subject": "Daily"})
    );
    let r = results(&p);
    let ds = r["deliveries"].as_array().unwrap();
    assert_eq!(ds.len(), 2);
    assert_eq!(ds[0]["profile"], "inbox");
    assert_eq!(ds[0]["type"], "local");
    assert_eq!(ds[0]["status"], "delivered");
    assert_eq!(ds[1]["profile"], "rec");
    assert_eq!(ds[1]["type"], "fixture");
    assert_eq!(ds[1]["status"], "delivered");
    assert_eq!(ds[1]["location"], "fixture:archive/daily.csv");
    assert_eq!(r["status"], "success");
}

#[test]
fn a_single_map_still_works_and_is_recorded_as_one_delivery() {
    let (p, _rec) = project(
        "queries: [q]\noutput:\n  destination: {profile: inbox, path: out/daily.csv}\n",
        &[Q],
    );
    p.dre("run", &["daily"]).ok();
    assert_eq!(p.read("out/daily.csv"), "n\r\n1\r\n");
    let r = results(&p);
    assert_eq!(r["deliveries"].as_array().unwrap().len(), 1);
    assert_eq!(
        r["outputs"][0]["delivered_to"],
        p.path("out/daily.csv").to_string_lossy().as_ref()
    );
}

#[test]
fn a_failed_destination_does_not_stop_the_next_one() {
    let (p, rec) = project(
        "queries: [q]\noutput:\n  destination:\n\
         \x20   - {profile: broken, path: a.csv}\n\
         \x20   - {profile: inbox, path: out/daily.csv}\n",
        &[Q],
    );
    p.dre("run", &["daily"])
        .failed()
        .says("1 of 2 destinations failed")
        .says("told to fail")
        .says("the output is still in target/");
    assert_eq!(p.read("out/daily.csv"), "n\r\n1\r\n");
    assert!(deliveries(&rec).is_empty());
    let r = results(&p);
    assert_eq!(r["status"], "error");
    assert_eq!(r["deliveries"][0]["status"], "failed");
    assert!(
        r["deliveries"][0]["error"]
            .as_str()
            .unwrap()
            .contains("told to fail")
    );
    assert_eq!(r["deliveries"][1]["status"], "delivered");
    assert_eq!(p.read("target/run/daily/default/a.csv"), "n\r\n1\r\n");
}

#[test]
fn a_destination_without_an_output_for_the_target_is_skipped_per_entry() {
    let (p, rec) = project(
        "queries: [q]\noutput:\n  destination:\n\
         \x20   - {profile: mail, to: client_a@example.com}\n\
         \x20   - {profile: inbox, path: out/daily.csv}\n",
        &[Q],
    );
    p.dre("run", &["daily", "--target", "dev"])
        .ok()
        .says("destination profile `mail` has no `dev` output");
    assert!(deliveries(&rec).is_empty());
    assert_eq!(p.read("out/daily.csv"), "n\r\n1\r\n");
    let r = results(&p);
    assert_eq!(r["deliveries"][0]["status"], "skipped");
    assert_eq!(r["deliveries"][1]["status"], "delivered");
}

#[test]
fn a_multi_file_destination_gets_every_file_in_one_delivery() {
    let (p, rec) = project(
        "queries:\n  - {query: q, tab_name: [Orders, Refunds]}\noutput:\n  destination:\n\
         \x20   - {profile: rec, path: out/daily.csv}\n",
        &[("reports/ops/daily/q.sql", "select 1 as n; select 2 as m")],
    );
    p.dre("run", &["daily"]).ok();
    let d = deliveries(&rec);
    assert_eq!(d.len(), 1, "{d:?}");
    let remotes: Vec<&str> = d[0]["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["remote"].as_str().unwrap())
        .collect();
    assert_eq!(remotes, ["out/daily_Orders.csv", "out/daily_Refunds.csv"]);
}

#[test]
fn without_multi_file_each_file_is_its_own_delivery() {
    let (p, rec) = project(
        "queries:\n  - {query: q, tab_name: [Orders, Refunds]}\noutput:\n  destination:\n\
         \x20   - {profile: rec, path: out/daily.csv}\n",
        &[("reports/ops/daily/q.sql", "select 1 as n; select 2 as m")],
    );
    p.dre_env("run", &["daily"], &[("DRE_FIXTURE_SINGLE_FILE", "1")])
        .ok();
    assert_eq!(deliveries(&rec).len(), 2);
}

#[test]
fn a_set_can_replace_the_destination_list() {
    let (p, rec) = project(
        "queries: [q]\noutput:\n  destination:\n\
         \x20   - {profile: inbox, path: out/default.csv}\n\
         \x20   - {profile: rec, to: all@example.com}\n\
         sets:\n  - name: client_a\n    profile: warehouse\n    vars: {client: client_a}\n    output:\n      destination:\n\
         \x20       - {profile: rec, to: \"{{ var('client') }}@example.com\"}\n",
        &[Q],
    );
    p.write("sets.yml", "client_a: {profile: warehouse}\n");
    p.dre("run", &["daily", "--set", "client_a"]).ok();
    let d = deliveries(&rec);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["options"]["to"], "client_a@example.com");
    assert!(!p.path("out/default.csv").exists());
}

#[test]
fn validate_rejects_malformed_destination_lists() {
    let (p, _rec) = project("queries: [q]\noutput:\n  destination: []\n", &[Q]);
    p.dre("validate", &[])
        .failed()
        .says("`output.destination` is an empty list");

    p.write(
        "reports/ops/daily/daily.yml",
        "queries: [q]\noutput:\n  destination:\n    - {path: a.csv}\n    - nope\n",
    );
    p.dre("validate", &[])
        .failed()
        .says("needs a `profile:`")
        .says("entry 2 must be a map");

    p.write(
        "reports/ops/daily/daily.yml",
        "queries: [q]\noutput:\n  destination: [{profile: nowhere}]\n",
    );
    p.dre("validate", &[]).failed().says("nowhere");

    p.write(
        "reports/ops/daily/daily.yml",
        "queries: [q]\noutput:\n  destination:\n    - {profile: inbox, path: a.csv}\n    - {profile: rec}\n\
         sets:\n  - name: client_a\n    output:\n      destination: {path: b.csv}\n",
    );
    p.write("sets.yml", "client_a: {profile: warehouse}\n");
    p.dre("validate", &[])
        .failed()
        .says("inherits 2 destinations")
        .says("override the full list");
}

#[test]
fn a_misspelt_key_on_a_destination_without_options_is_an_error_not_ignored() {
    let (p, _rec) = project(
        "queries: [q]\noutput:\n  destination: {profile: inbox, pth: out/daily.csv}\n",
        &[Q],
    );
    p.dre("run", &["daily"])
        .failed()
        .says("the local destination takes no options, but `inbox` has `pth`");
    let r = results(&p);
    assert_eq!(r["deliveries"][0]["status"], "failed");
}

#[test]
fn a_plugin_without_options_refuses_them() {
    // The SFTP plugin uses the SDK's default, which rejects options before connecting.
    let (p, _rec) = project(
        "queries: [q]\noutput:\n  destination: {profile: box, path: /in/daily.csv, pasth: x}\n",
        &[Q],
    );
    let mut profiles = std::fs::read_to_string(p.dir.path().join("profiles/profiles.yml")).unwrap();
    profiles.push_str("box:\n  target: dev\n  outputs:\n    dev: {type: sftp, host: 127.0.0.1, port: 1, username: u, password: x}\n");
    std::fs::write(p.dir.path().join("profiles/profiles.yml"), profiles).unwrap();
    p.write(
        "plugins.yml",
        "sources: [duckdb]\nformats: [csv]\ndestinations: [fixture, sftp]\n",
    );
    p.dre("run", &["daily"])
        .failed()
        .says("this destination takes no options, but the destination entry has `pasth`");
}

#[test]
fn a_skipped_entry_still_records_its_type() {
    let (p, _rec) = project(
        "queries: [q]\noutput:\n  destination: [{profile: mail, to: a@example.com}]\n",
        &[Q],
    );
    p.dre("run", &["daily", "--target", "dev"]).ok();
    let d = &results(&p)["deliveries"][0];
    assert_eq!(d["status"], "skipped");
    assert_eq!(d["type"], "fixture");
    assert!(d.get("location").is_none() && d.get("error").is_none(), "{d}");
}
