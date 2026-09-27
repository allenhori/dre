//! What people (and CI) see: levels, JSON lines, colour and the log file.

mod common;

/// Status lines: those with a verb in the 10-column gutter (not wrapped error text).
fn status_lines(out: &str) -> Vec<&str> {
    out.lines()
        .filter(|l| l.get(..10).is_some_and(|g| !g.trim().is_empty()))
        .collect()
}

use common::{DUCK_PROFILES, PLUGINS_YML, TestProject};

fn project() -> TestProject {
    let p = TestProject::new(
        &[
            (
                "dre_project.yml",
                "name: acme_reports\ndefault_profile: warehouse\n",
            ),
            ("dependencies.yml", PLUGINS_YML),
            (
                "reports/ops/daily/daily.yml",
                "queries:\n  - {query: setup, tab: false}\n  - summary\n",
            ),
            (
                "reports/ops/daily/setup.sql",
                "create temp table t as select 1 as n union all select 2\n",
            ),
            ("reports/ops/daily/summary.sql", "select * from t\n"),
            ("reports/ops/broken/broken.yml", "queries: [bad]\n"),
            ("reports/ops/broken/bad.sql", "select nope\n"),
        ],
        DUCK_PROFILES,
    );
    p.duckdb("data.duckdb", "select 1;");
    p
}

#[test]
fn default_output_is_one_line_per_binding_and_a_summary() {
    let p = project();
    let r = p.dre("run", &[]);
    r.failed();
    let lines = status_lines(&r.stdout);
    assert_eq!(lines.len(), 4, "{}", r.stdout);
    assert_eq!(lines[0], "   Running  2 Bindings");
    assert!(
        lines[1].starts_with("    Failed  [") && lines[1].contains("broken  reports/ops/broken/bad.sql:1:"),
        "{}",
        lines[1]
    );
    assert!(
        lines[2].starts_with(" Succeeded  [")
            && lines[2].ends_with("daily  1 result set, 2 rows → daily.csv"),
        "{}",
        lines[2]
    );
    assert!(
        lines[3].starts_with("  Finished  'run' in ") && lines[3].ends_with(" · 1 succeeded, 1 failed"),
        "{}",
        lines[3]
    );
}

#[test]
fn verbose_shows_every_step() {
    let p = project();
    let r = p.dre("run", &["daily", "-v"]);
    r.ok();
    for needle in [
        "   Started  daily",
        "  Rendered  reports/ops/daily/setup.sql",
        "  Executed  [",
        "reports/ops/daily/setup.sql:1  no result set",
        "reports/ops/daily/summary.sql:1  2 rows",
        "     Wrote  [",
        "      Kept  no destination declared",
    ] {
        assert!(r.stdout.contains(needle), "missing {needle:?} in:\n{}", r.stdout);
    }
}

#[test]
fn quiet_shows_only_failures_and_the_summary() {
    let p = project();
    let r = p.dre("run", &["-q"]);
    let lines = status_lines(&r.stdout);
    assert_eq!(lines.len(), 2, "{}", r.stdout);
    assert!(lines[0].trim_start().starts_with("Failed"));
    assert!(lines[1].trim_start().starts_with("Finished"));
}

#[test]
fn json_log_format_is_one_object_per_line() {
    let p = project();
    let r = p.dre("run", &["--log-format", "json", "-v"]);
    let events: Vec<serde_json::Value> = r
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect();
    let kinds: Vec<&str> = events.iter().map(|e| e["event"].as_str().unwrap()).collect();
    assert_eq!(kinds[..2], ["run_parameters", "plan"]);
    assert!(kinds.contains(&"binding_vars"));
    assert_eq!(kinds.last(), Some(&"finished"));
    assert!(kinds.contains(&"step") && kinds.contains(&"binding_end"));
    let end = events
        .iter()
        .find(|e| e["event"] == "binding_end" && e["report"] == "daily")
        .unwrap();
    assert_eq!(end["status"], "success");
    assert!(events.iter().all(|e| e["ts"].is_string()));
}

#[test]
fn json_log_format_reports_diagnostics_as_events() {
    let p = project();
    p.write(
        "reports/ops/daily/daily.yml",
        "queries:\n  - {query: setup, tab: false}\n  - summary\noutput: {format: csv, delimeter: \"|\"}\n",
    );
    let r = p.dre("run", &["--log-format", "json"]);
    assert_ne!(r.code, 0);
    let events: Vec<serde_json::Value> = r
        .stdout
        .lines()
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("{l}: {e}")))
        .collect();
    let d = events
        .iter()
        .find(|e| e["event"] == "diagnostic")
        .expect("a diagnostic event");
    assert_eq!(d["severity"], "error");
    assert_eq!(d["code"], "invalid-output-option");
    assert!(d["message"].as_str().unwrap().contains("`delimeter`"), "{d}");
}

#[test]
fn colour_is_off_unless_asked_for_when_not_a_terminal() {
    let p = project();
    assert!(!p.dre("run", &["daily"]).stdout.contains('\u{1b}'));
    let r = p.dre("run", &["daily", "--color", "always"]);
    assert!(
        r.stdout.contains("\u{1b}[1m\u{1b}[32m")
            || r.stdout.contains("\u{1b}[32m")
            || r.stdout.contains("\u{1b}[1;32m"),
        "{:?}",
        r.stdout
    );
}

#[test]
fn every_run_appends_a_debug_log_with_the_sql_it_ran() {
    let p = project();
    p.dre("run", &["daily"]).ok();
    let log = p.read("logs/dre.log");
    assert!(log.contains("DEBUG [daily] Executed"), "{log}");
    assert!(log.contains("INFO  [daily] Succeeded"), "{log}");
    assert!(log.contains("Finished 'run'"), "{log}");
    // The full statement, indented under a label naming its file and line.
    assert!(
        log.contains("DEBUG [daily] SQL reports/ops/daily/setup.sql:1:\n    create temp table t as select 1 as n union all select 2\n"),
        "{log}"
    );
    assert!(
        log.contains("SQL reports/ops/daily/summary.sql:1:\n    select * from t\n"),
        "{log}"
    );
}

#[test]
fn the_log_rotates_keeping_five_old_files() {
    let p = project();
    for _ in 0..8 {
        p.dre_env("run", &["daily"], &[("DRE_LOG_MAX_LINES", "5")]).ok();
    }
    for n in 1..=5 {
        assert!(p.path(&format!("logs/dre.log.{n}")).exists(), "dre.log.{n}");
    }
    assert!(!p.path("logs/dre.log.6").exists());
    // Every run writes more than 5 lines, so each file ends with at most one entry past the limit.
    let current = p.read("logs/dre.log");
    assert!(current.lines().count() < 5 + 3, "{current}");
}

#[test]
fn verbose_says_which_profiles_file_it_used() {
    let p = project();
    p.dre("run", &["daily", "-v"])
        .ok()
        .says("  Profiles  ")
        .says("profiles.yml (from --profiles-dir)");
}
