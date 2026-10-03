//! The project manifest (`target/manifest.json`): what `compile`, `validate` and `run` write,
//! its contents, change-detection checksums, and what it leaves out.

mod common;

use common::TestProject;
use serde_json::{Value, json};

const PROFILES: &str = "\
sources:
  warehouse:
    target: dev
    targets:
      dev: {type: duckdb, path: dev.duckdb}
destinations:
  inbox:
    target: dev
    targets:
      dev: {type: local}
  archive:
    target: dev
    targets:
      dev: {type: local}
";

const SCHEDULES: &str = "\
- name: daily_all
  report: daily
  cron: \"0 6 * * *\"
  timezone: Australia/Sydney
  vars: {period: day}
- name: close_a
  report: monthly
  set: client_a
  rrule: \"FREQ=MONTHLY;BYMONTHDAY=1\"
- name: regulatory
  select: \"tag:regulatory\"
  cron: \"0 6 2 * *\"
";

fn files() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\ntimezone: UTC\nvars: {level: project, region: emea}\n\
             reports:\n  finance:\n    +vars: {level: folder}\n",
        ),
        ("dependencies.yml", "plugins:\n  - duckdb\n  - csv\n"),
        (
            "sets.yml",
            "client_a: {vars: {client: client_a}}\nclient_b: {vars: {client: client_b, level: set}}\n",
        ),
        ("schedules.yml", SCHEDULES),
        (
            "reports/ops/daily/daily.yml",
            "queries: [summary]\ntags: [regulatory]\n\
             output:\n  destination:\n    - {profile: inbox, path: \"out/daily-{{ run.date.yyyymmdd }}.csv\"}\n    - {profile: archive, path: archive/daily.csv}\n",
        ),
        ("reports/ops/daily/summary.sql", "select 1 as n\n"),
        (
            "reports/finance/monthly/monthly.yml",
            "queries: [m]\nsets: [client_a, client_b]\ndefault_set: client_a\nvars: {level: report}\n",
        ),
        (
            "reports/finance/monthly/m.sql",
            "select '{{ var('client') }}' as c\n",
        ),
        ("reports/adhoc/scratch.sql", "select 42 as answer\n"),
        (
            "macros/money.sql",
            "{% macro money(x) %}round({{ x }}, 2){% endmacro %}\n",
        ),
    ]
}

fn project() -> TestProject {
    TestProject::new(&files(), PROFILES)
}

fn manifest(p: &TestProject) -> Value {
    p.json("target/manifest.json")
}

#[test]
fn compile_writes_the_whole_project() {
    let p = project();
    p.dre("compile", &["-s", "daily"]).ok();
    let m = manifest(&p);
    assert_eq!(m["schema"], 1);
    assert!(m["version"].is_string());
    assert_eq!(m["project"]["name"], "acme_reports");
    assert_eq!(m["project"]["default_profile"], "warehouse");
    assert_eq!(m["project"]["timezone"], "UTC");

    // `-s daily` doesn't narrow it.
    let reports = m["reports"].as_object().unwrap();
    assert_eq!(
        reports.keys().collect::<Vec<_>>(),
        ["daily", "monthly", "scratch"]
    );

    let daily = &m["reports"]["daily"];
    assert_eq!(daily["managed"], true);
    assert_eq!(daily["file"], "reports/ops/daily/daily.yml");
    assert_eq!(daily["folder"], json!(["ops", "daily"]));
    assert_eq!(daily["tags"], json!(["regulatory"]));
    assert_eq!(daily["valid"], true);
    assert_eq!(
        daily["queries"],
        json!([{"query": "summary", "file": "reports/ops/daily/summary.sql", "tab": true}])
    );
    let b = &daily["bindings"][0];
    assert_eq!(b["set"], Value::Null);
    assert_eq!(b["profile"], "warehouse");
    assert_eq!(b["output"]["format"], "csv");
    assert_eq!(
        b["destinations"],
        json!([
            {"profile": "inbox", "path": "out/daily-{{ run.date.yyyymmdd }}.csv"},
            {"profile": "archive", "path": "archive/daily.csv"}
        ])
    );

    // A report with Sets: one Binding per Set, vars fully merged.
    let monthly = &m["reports"]["monthly"];
    assert_eq!(monthly["default_set"], "client_a");
    let bs = monthly["bindings"].as_array().unwrap();
    assert_eq!(bs.len(), 2);
    assert_eq!(bs[0]["set"], "client_a");
    assert_eq!(
        bs[0]["vars"],
        json!({"client": "client_a", "level": "report", "region": "emea"})
    );
    assert_eq!(bs[1]["vars"]["level"], "set");

    // An unmanaged report.
    let scratch = &m["reports"]["scratch"];
    assert_eq!(scratch["managed"], false);
    assert_eq!(scratch["file"], "reports/adhoc/scratch.sql");

    // Plugins as declared.
    assert_eq!(
        m["plugins"],
        json!([
            {"package": "csv", "version": "*", "source": {"type": "registry"}},
            {"package": "duckdb", "version": "*", "source": {"type": "registry"}}
        ])
    );
}

#[test]
fn schedules_resolve_to_bindings_and_bindings_list_their_schedules() {
    let p = project();
    p.dre("compile", &[]).ok();
    let m = manifest(&p);
    let s = &m["schedules"];
    assert_eq!(
        s["daily_all"],
        json!({
            "name": "daily_all",
            "report": "daily",
            "schedule": {"cron": "0 6 * * *"},
            "enabled": true,
            "vars": {"period": "day"},
            "timezone": "Australia/Sydney",
            "bindings": [{"report": "daily", "set": null}]
        })
    );
    assert_eq!(s["close_a"]["set"], "client_a");
    assert_eq!(
        s["close_a"]["schedule"],
        json!({"rrule": "FREQ=MONTHLY;BYMONTHDAY=1"})
    );
    assert_eq!(
        s["close_a"]["bindings"],
        json!([{"report": "monthly", "set": "client_a"}])
    );
    assert_eq!(s["regulatory"]["select"], "tag:regulatory");
    assert_eq!(
        s["regulatory"]["bindings"],
        json!([{"report": "daily", "set": null}])
    );

    assert_eq!(
        m["reports"]["daily"]["bindings"][0]["schedules"],
        json!(["daily_all", "regulatory"])
    );
    let monthly = &m["reports"]["monthly"]["bindings"];
    assert_eq!(monthly[0]["schedules"], json!(["close_a"]));
    assert_eq!(monthly[1]["schedules"], json!([]));
}

#[test]
fn the_same_project_gives_the_same_bytes() {
    let p = project();
    p.dre("compile", &[]).ok();
    let first = p.read("target/manifest.json");
    // Rewriting files with the same contents (new mtimes) changes nothing.
    for (rel, content) in files() {
        p.write(rel, content);
    }
    p.dre("compile", &["-s", "monthly", "--set", "client_b"]).ok();
    assert_eq!(p.read("target/manifest.json"), first);
    assert!(
        !first.contains(&p.root().display().to_string()),
        "no absolute paths"
    );
}

#[test]
fn no_profiles_or_database_needed() {
    let p = project();
    std::fs::remove_file(p.dir.path().join("profiles/profiles.yml")).unwrap();
    p.dre("compile", &[]);
    assert!(p.path("target/manifest.json").is_file());
    assert!(!p.path("dev.duckdb").exists());
    assert_eq!(
        manifest(&p)["reports"]["daily"]["bindings"][0]["profile"],
        "warehouse"
    );
}

#[test]
fn secrets_are_masked_unless_masking_is_off() {
    let p = project();
    p.write(
        "dre_project.yml",
        "name: acme_reports\ndefault_profile: warehouse\nvars: {token: hunter2-very-secret}\n",
    );
    p.dre_env("compile", &[], &[("DRE_SECRET_TOKEN", "hunter2-very-secret")])
        .ok();
    let text = p.read("target/manifest.json");
    assert!(!text.contains("hunter2-very-secret"), "{text}");
    assert_eq!(
        manifest(&p)["reports"]["daily"]["bindings"][0]["vars"]["token"],
        "*****"
    );

    p.write(
        "dre_project.yml",
        "name: acme_reports\ndefault_profile: warehouse\nvars: {token: hunter2-very-secret}\nmask_secrets: false\n",
    );
    p.dre_env("compile", &[], &[("DRE_SECRET_TOKEN", "hunter2-very-secret")])
        .ok();
    assert_eq!(
        manifest(&p)["reports"]["daily"]["bindings"][0]["vars"]["token"],
        "hunter2-very-secret"
    );
}

#[test]
fn clean_removes_it() {
    let p = project();
    p.dre("compile", &[]).ok();
    p.dre("clean", &[]).ok();
    assert!(!p.path("target/manifest.json").exists());
}

#[test]
fn validate_and_run_write_it_too_whatever_is_selected() {
    let p = project();
    p.duckdb("dev.duckdb", "select 1;");
    p.dre("validate", &["-s", "daily"]).ok();
    let m = manifest(&p);
    assert_eq!(m["reports"].as_object().unwrap().len(), 3);
    let bytes = p.read("target/manifest.json");

    std::fs::remove_file(p.path("target/manifest.json")).unwrap();
    p.dre("run", &["-s", "daily", "--dry-run"]).ok();
    assert_eq!(p.read("target/manifest.json"), bytes);

    std::fs::remove_file(p.path("target/manifest.json")).unwrap();
    p.dre("run", &["--schedule", "daily_all"]).ok();
    assert_eq!(p.read("target/manifest.json"), bytes);

    std::fs::remove_file(p.path("target/manifest.json")).unwrap();
    p.dre("run", &["-s", "daily"]).ok();
    assert_eq!(p.read("target/manifest.json"), bytes);
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    sha2::Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[test]
fn run_results_record_the_manifest_checksum_even_when_a_query_fails() {
    let p = project();
    p.duckdb("dev.duckdb", "select 1;");
    p.dre("run", &["-s", "daily"]).ok();
    let want = sha256(&std::fs::read(p.path("target/manifest.json")).unwrap());
    let results = p.json("target/run/daily/default/run_results.json");
    assert_eq!(results["manifest_checksum"], want);

    p.write("reports/ops/daily/summary.sql", "select * from no_such_table\n");
    p.dre("run", &["-s", "daily"]).failed();
    let want = sha256(&std::fs::read(p.path("target/manifest.json")).unwrap());
    let results = p.json("target/run/daily/default/run_results.json");
    assert_eq!(results["status"], "error");
    assert_eq!(results["manifest_checksum"], want);
}

#[test]
fn a_broken_report_is_marked_invalid_and_the_rest_still_listed() {
    let p = project();
    p.write(
        "reports/finance/monthly/monthly.yml",
        "queries: [m, missing_query]\nsets: [client_a, client_b]\n",
    );
    p.dre("validate", &[]).failed();
    let m = manifest(&p);
    assert_eq!(m["reports"]["monthly"]["valid"], false);
    let errors = m["reports"]["monthly"]["errors"].as_array().unwrap();
    assert!(
        errors
            .iter()
            .any(|e| e.as_str().unwrap().contains("missing_query")),
        "{errors:?}"
    );
    assert_eq!(m["reports"]["daily"]["valid"], true);

    // compile too.
    std::fs::remove_file(p.path("target/manifest.json")).unwrap();
    p.dre("compile", &["daily"]).failed();
    assert_eq!(manifest(&p)["reports"]["monthly"]["valid"], false);
}

#[test]
fn a_project_that_cant_load_leaves_no_manifest() {
    let p = project();
    p.dre("compile", &[]).ok();
    assert!(p.path("target/manifest.json").is_file());
    p.write("dre_project.yml", "name: [unclosed\n");
    p.dre("validate", &[]).failed();
    assert!(!p.path("target/manifest.json").exists());
    p.dre("compile", &[]).failed();
    assert!(!p.path("target/manifest.json").exists());
}

#[test]
fn validate_json_project_is_the_manifest() {
    let p = project();
    let v = p.dre("validate", &["--json"]);
    v.ok();
    let j: Value = serde_json::from_str(&v.stdout).unwrap();
    assert_eq!(j["project"], manifest(&p));
}

// -- checksums --------------------------------------------------------------------------------

fn checksums(p: &TestProject) -> (String, Value) {
    p.dre("compile", &[]);
    let m = manifest(p);
    let reports = m["reports"]
        .as_object()
        .unwrap()
        .iter()
        .map(|(k, r)| (k.clone(), r["checksum"].clone()))
        .collect();
    (
        m["project"]["checksum"].as_str().unwrap().to_string(),
        Value::Object(reports),
    )
}

/// Which report checksums differ between two snapshots.
fn changed(a: &Value, b: &Value) -> Vec<String> {
    a.as_object()
        .unwrap()
        .iter()
        .filter(|(k, v)| b[k.as_str()] != **v)
        .map(|(k, _)| k.clone())
        .collect()
}

#[test]
fn a_reports_checksum_follows_its_own_files_only() {
    let p = project();
    let (proj0, r0) = checksums(&p);
    for r in r0.as_object().unwrap().values() {
        assert_eq!(r.as_str().unwrap().len(), 64);
    }

    p.write("reports/finance/monthly/m.sql", "select 'changed' as c\n");
    let (proj1, r1) = checksums(&p);
    assert_eq!(changed(&r0, &r1), ["monthly"]);
    assert_eq!(proj1, proj0);

    p.write(
        "reports/ops/daily/daily.yml",
        "queries: [summary]\ntags: [regulatory, ops]\n",
    );
    let (proj2, r2) = checksums(&p);
    assert_eq!(changed(&r1, &r2), ["daily"]);
    assert_eq!(proj2, proj1);

    p.write("reports/adhoc/scratch.sql", "select 43 as answer\n");
    let (_, r3) = checksums(&p);
    assert_eq!(changed(&r2, &r3), ["scratch"]);
}

#[test]
fn a_template_is_part_of_its_reports_checksum() {
    let p = project();
    p.write(
        "reports/ops/daily/daily.yml",
        "queries: [summary]\noutput:\n  format: xlsx\n  template:\n    file: templates/t.xlsx\n    bindings:\n      - {query: summary, sheet: S}\n",
    );
    p.write("templates/t.xlsx", "one");
    let (proj0, r0) = checksums(&p);
    p.write("templates/t.xlsx", "two");
    let (proj1, r1) = checksums(&p);
    assert_eq!(changed(&r0, &r1), ["daily"]);
    assert_eq!(proj0, proj1);
}

#[test]
fn shared_inputs_change_the_project_checksum() {
    let p = project();
    let (mut before, _) = checksums(&p);
    for (rel, content) in [
        (
            "macros/money.sql",
            "{% macro money(x) %}round({{ x }}, 4){% endmacro %}\n",
        ),
        ("lookups/regions.csv", "code,name\nemea,Europe\n"),
        (
            "schedules.yml",
            "- name: daily_all\n  report: daily\n  cron: \"0 7 * * *\"\n",
        ),
        ("reports/shared/base.sql", "select 1 as base\n"),
    ] {
        p.write(rel, content);
        let (after, _) = checksums(&p);
        assert_ne!(after, before, "{rel} should change the project checksum");
        before = after;
    }
}

// -- the published contract -------------------------------------------------------------------

fn schema() -> jsonschema::Validator {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/manifest.schema.json");
    let schema: Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn assert_valid(v: &jsonschema::Validator, doc: &Value) {
    let errors: Vec<String> = v
        .iter_errors(doc)
        .map(|e| format!("{e} at {}", e.instance_path))
        .collect();
    assert!(errors.is_empty(), "{errors:#?}\n{doc:#}");
}

#[test]
fn the_manifest_and_ls_json_match_the_published_schema() {
    let p = project();
    p.write(
        "reports/ops/branded/branded.yml",
        "queries: [{query: b, tab_name: Main, anchor: B2, header: false}]\noutput:\n  format: xlsx\n  extension: xlsm\n  template:\n    file: templates/t.xlsx\n    bindings:\n      - {query: b, sheet: S}\n",
    );
    p.write("reports/ops/branded/b.sql", "select 1 as n\n");
    p.write("templates/t.xlsx", "x");
    p.write("reports/ops/broken/broken.yml", "queries: [nope]\n");
    p.dre("validate", &[]).failed();
    let m = manifest(&p);
    assert_eq!(m["reports"]["broken"]["valid"], false);
    let v = schema();
    assert_valid(&v, &m);

    let r = p.dre("ls", &["--schedule", "close_a", "--output", "json"]);
    r.ok();
    assert_valid(&v, &serde_json::from_str(&r.stdout).unwrap());
}
