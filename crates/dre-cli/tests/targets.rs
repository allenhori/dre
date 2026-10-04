//! Which entry each profile uses: the run's target (`--target`, `DRE_TARGET`), else the
//! profile's own `target:`, else `dev`; missing entries; `deliver: false`; `target.name`.

mod common;

use common::{PLUGINS_YML, TestProject};

/// `warehouse` defaults to prod, as a developer reading prod data locally would set it up;
/// `inbox` delivers nowhere on dev.
const PROFILES: &str = "\
connections:
  warehouse:
    target: prod
    targets:
      dev: {type: duckdb, path: dev.duckdb}
      prod: {type: duckdb, path: prod.duckdb}
  plain:
    targets:
      dev: {type: duckdb, path: dev.duckdb}
      prod: {type: duckdb, path: prod.duckdb}
destinations:
  inbox:
    targets:
      dev: {deliver: false}
      prod: {type: local}
  prod_only:
    targets:
      prod: {type: local}
";

fn project(files: &[(&str, &str)]) -> TestProject {
    project_with(files, PROFILES)
}

fn project_with(files: &[(&str, &str)], profiles: &str) -> TestProject {
    let mut all = vec![
        (
            "dre_project.yml",
            "name: acme_reports\ndefault_profile: warehouse\n",
        ),
        ("dependencies.yml", PLUGINS_YML),
    ];
    all.extend_from_slice(files);
    let p = TestProject::new(&all, profiles);
    p.duckdb("dev.duckdb", "create table env as select 'dev' as name;");
    p.duckdb("prod.duckdb", "create table env as select 'prod' as name;");
    p
}

const T: (&str, &str) = (
    "reports/ops/t/t.yml",
    "queries: [tq]\noutput:\n  destination: {profile: inbox, path: \"out/{{ destination.target }}/t.csv\"}\n",
);
const TQ: (&str, &str) = ("reports/ops/t/tq.sql", "select name from env\n");

#[test]
fn each_profile_uses_its_own_target_without_flags() {
    // The common case: prod data, delivered nowhere, no flags.
    let p = project(&[T, TQ]);
    p.dre("run", &["t"])
        .ok()
        .says("destination `inbox`: `dev` delivers nowhere");
    assert_eq!(p.read("target/run/t/default/t.csv"), "name\r\nprod\r\n");
    assert!(!p.path("out").exists());
    let r = p.json("target/run/t/default/run_results.json");
    assert_eq!(r["target"], "dev");
    assert_eq!(r["deliveries"][0]["profile"], "inbox");
    assert_eq!(r["deliveries"][0]["status"], "not_delivered");
    assert_eq!(r["deliveries"][0]["target"], "dev");
    assert_eq!(r["status"], "success");
}

#[test]
fn the_run_target_sets_every_profile_flag_over_env() {
    let p = project(&[T, TQ]);
    // --target sets every profile, overriding a profile's own `target:`.
    p.dre("run", &["t", "--target", "prod"]).ok();
    assert_eq!(p.read("out/prod/t.csv"), "name\r\nprod\r\n");
    std::fs::remove_dir_all(p.path("out")).unwrap();
    // DRE_TARGET too.
    p.dre_env("run", &["t"], &[("DRE_TARGET", "prod")]).ok();
    assert_eq!(p.read("out/prod/t.csv"), "name\r\nprod\r\n");
    std::fs::remove_dir_all(p.path("out")).unwrap();
    // DRE_TARGET=dev beats the profile's `target: prod`.
    p.dre_env("run", &["t"], &[("DRE_TARGET", "dev")]).ok();
    assert_eq!(p.read("target/run/t/default/t.csv"), "name\r\ndev\r\n");
    // --target beats DRE_TARGET.
    p.dre_env("run", &["t", "--target", "prod"], &[("DRE_TARGET", "dev")])
        .ok();
    assert_eq!(p.read("out/prod/t.csv"), "name\r\nprod\r\n");
}

#[test]
fn a_profile_without_its_own_target_uses_dev() {
    let p = project(&[T, TQ, ("reports/ops/t/t.yml", "queries: [tq]\nprofile: plain\n")]);
    p.dre("run", &["t"]).ok();
    assert_eq!(p.read("target/run/t/default/t.csv"), "name\r\ndev\r\n");
}

#[test]
fn a_missing_entry_fails_before_anything_runs() {
    let p = project(&[
        TQ,
        (
            "reports/ops/t/t.yml",
            "queries: [tq]\noutput:\n  destination: {profile: prod_only, path: out/t.csv}\n",
        ),
        ("reports/ops/a/a.yml", "queries: [aq]\n"),
        ("reports/ops/a/aq.sql", "select 1 as n\n"),
    ]);
    for cmd in ["run", "compile", "validate"] {
        p.dre(cmd, &[])
            .failed()
            .says("destination `prod_only` has no `dev` entry (it has: prod)")
            .says("`dev: {deliver: false}`");
        // Nothing ran, not even the report that's fine.
        assert!(!p.path("target/run").exists(), "{cmd}");
        assert!(!p.path("target/compiled").exists(), "{cmd}");
    }
    // With the entry's target, it delivers.
    p.dre("run", &["--target", "prod"]).ok();
    assert_eq!(p.read("out/t.csv"), "name\r\nprod\r\n");
}

#[test]
fn a_target_typo_fails_loudly_naming_every_profile() {
    let p = project(&[T, TQ]);
    p.dre("run", &["t", "--target", "prd"])
        .failed()
        .says("connection `warehouse` has no `prd` entry (it has: dev, prod)")
        .says("destination `inbox` has no `prd` entry (it has: dev, prod)")
        .says("--target");
    assert!(!p.path("target/run").exists());
    p.dre_env("validate", &[], &[("DRE_TARGET", "prd")])
        .failed()
        .says("connection `warehouse` has no `prd` entry")
        .says("DRE_TARGET");
}

#[test]
fn profiles_no_selected_report_uses_are_not_checked() {
    let p = project(&[
        T,
        TQ,
        (
            "reports/ops/o/o.yml",
            "queries: [oq]\noutput:\n  destination: {profile: prod_only, path: out/o.csv}\n",
        ),
        ("reports/ops/o/oq.sql", "select 1 as n\n"),
    ]);
    p.dre("run", &["t"]).ok();
    p.dre("compile", &["t"]).ok();
    p.dre("validate", &["t"]).ok();
}

#[test]
fn deliver_false_is_only_for_destinations() {
    let p = project_with(
        &[T, TQ],
        "connections:\n  warehouse:\n    targets:\n      dev: {deliver: false}\n",
    );
    p.dre("validate", &[])
        .failed()
        .says("`deliver: false` is only for destinations");
    let p = project_with(
        &[T, TQ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: dev.duckdb}\ndestinations:\n  inbox:\n    targets:\n      dev: {deliver: false, type: local}\n",
    );
    p.dre("validate", &[])
        .failed()
        .says("`deliver: false` takes no other settings");
    let p = project_with(
        &[T, TQ],
        "connections:\n  warehouse:\n    targets:\n      dev: {type: duckdb, path: dev.duckdb}\ndestinations:\n  inbox:\n    targets:\n      dev: {deliver: true}\n",
    );
    p.dre("validate", &[])
        .failed()
        .says("`deliver` can only be `false`");
}

#[test]
fn the_project_file_target_is_removed() {
    let p = project(&[T, TQ]);
    p.write(
        "dre_project.yml",
        "name: acme_reports\ndefault_profile: warehouse\ntarget: prod\n",
    );
    p.dre("validate", &[])
        .failed()
        .says("error[removed-key]")
        .says("`target:` in profiles.yml")
        .says("DRE_TARGET");
}

#[test]
fn target_name_is_the_run_target_and_each_profile_has_its_own() {
    let p = project(&[
        (
            "reports/ops/t/t.yml",
            "queries: [tq]\noutput:\n  destination: {profile: inbox, path: \"out/{{ target.name }}-{{ destination.target }}.csv\"}\n",
        ),
        (
            "reports/ops/t/tq.sql",
            "select '{{ target.name }}' as run, '{{ connection.target }}' as conn, name from env\n",
        ),
    ]);
    p.dre("compile", &["t"]).ok();
    assert_eq!(
        p.read("target/compiled/t/default/tq.sql"),
        "select 'dev' as run, 'prod' as conn, name from env\n"
    );
    p.dre("run", &["t", "--target", "prod"]).ok();
    assert_eq!(p.read("out/prod-prod.csv"), "run,conn,name\r\nprod,prod,prod\r\n");
    p.dre("validate", &[]).ok();
    assert_eq!(p.json("target/manifest.json")["project"]["target"], "dev");
}

#[test]
fn the_target_line_shows_entries_that_differ() {
    let p = project(&[T, TQ]);
    p.dre("run", &["t"])
        .ok()
        .says("Target")
        .says("dev (default); connection `warehouse`: prod");
    // With a selector, validate shows each query's and destination's entry.
    p.dre("validate", &["t"])
        .ok()
        .says("dev (default); connection `warehouse`: prod")
        .says("tq on warehouse (duckdb), target prod")
        .says("inbox: `dev` delivers nowhere (`deliver: false`)");
    p.dre("run", &["t", "--target", "prod"])
        .ok()
        .says("prod (--target)");
}

#[test]
fn a_warning_when_every_profile_is_on_another_target() {
    // `inbox` is on dev (deliver: false), so not every profile is elsewhere: no warning.
    let p = project(&[T, TQ]);
    let r = p.dre("run", &["t"]);
    r.ok();
    assert!(!r.stdout.contains("every profile is on"), "{}", r.stdout);
    assert!(!r.stderr.contains("every profile is on"), "{}", r.stderr);
    // Only the prod connection: warned on run and validate.
    let p = project(&[("reports/ops/t/t.yml", "queries: [tq]\n"), TQ]);
    p.dre("run", &["t"]).ok().says(
        "every profile is on `prod` but the run's target is `dev`: pass `--target prod` or set `DRE_TARGET`",
    );
    p.dre("validate", &[])
        .ok()
        .says("warning[target-mismatch]")
        .says("every profile is on `prod`");
    // Not when the run's target matches.
    let r = p.dre("run", &["t", "--target", "prod"]);
    r.ok();
    assert!(!r.stdout.contains("every profile is on"), "{}", r.stdout);
}

#[test]
fn validate_json_lists_each_profile_entry() {
    let p = project(&[T, TQ]);
    let r = p.dre("validate", &["--json"]);
    r.ok();
    let v: serde_json::Value = serde_json::from_str(&r.stdout).unwrap();
    assert_eq!(v["target"]["name"], "dev");
    assert_eq!(v["target"]["from"], "default");
    let profiles = v["target"]["profiles"].as_array().unwrap();
    assert!(
        profiles.contains(&serde_json::json!({"role": "connection", "profile": "warehouse", "target": "prod", "deliver": true})),
        "{profiles:?}"
    );
    assert!(
        profiles.contains(
            &serde_json::json!({"role": "destination", "profile": "inbox", "target": "dev", "deliver": false})
        ),
        "{profiles:?}"
    );
}
