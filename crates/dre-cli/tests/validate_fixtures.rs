//! Fixture harness for `dre validate`.
//!
//! Each directory under `tests/fixtures/validate/` is one case:
//!
//! - `project/`   the DRE project to validate (required)
//! - `profiles/`  passed as `--profiles-dir` when present
//! - `args`       extra CLI arguments, one per line (optional)
//! - `env`        `KEY=VALUE` lines set for the run (optional)
//! - `expected.txt`  golden human-readable output: exit code, then stdout
//! - `expected.json` golden `--json` output (optional)
//!
//! Absolute paths are replaced by `$CASE` so goldens are machine-independent.
//! Run with `UPDATE_GOLDEN=1` to rewrite goldens, then review the diff by eye.

use std::path::{Path, PathBuf};

use assert_cmd::Command;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/validate")
}

fn run_case(case: &Path, json: bool) -> String {
    let mut cmd = Command::cargo_bin("dre").unwrap();
    cmd.arg("validate").arg("--project-dir").arg(case.join("project"));
    let profiles = case.join("profiles");
    if profiles.exists() {
        cmd.arg("--profiles-dir").arg(&profiles);
    } else {
        // Point at a directory that has no profiles.yml, never the developer's real ~/.dre.
        cmd.arg("--profiles-dir").arg(case.join("no-profiles"));
    }
    // Never touch the network or real plugin dirs from the validate harness.
    cmd.arg("--no-auto-install");
    cmd.env("DRE_PLUGINS_DIR", case.join("no-plugins"));
    cmd.env_remove("DRE_PROFILES_DIR");
    if let Ok(args) = std::fs::read_to_string(case.join("args")) {
        for a in args.lines().filter(|l| !l.trim().is_empty()) {
            cmd.arg(a.trim());
        }
    }
    if let Ok(env) = std::fs::read_to_string(case.join("env")) {
        for line in env.lines().filter(|l| !l.trim().is_empty()) {
            let (k, v) = line.split_once('=').expect("env lines are KEY=VALUE");
            cmd.env(k.trim(), v.trim());
        }
    }
    if json {
        cmd.arg("--json");
    }
    let out = cmd.output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).replace('\\', "/");
    let case_str = case.to_string_lossy().replace('\\', "/");
    let stdout = stdout.replace(&case_str, "$CASE");
    format!("exit: {code}\n{stdout}")
}

fn check(case: &Path, file: &str, actual: String, failures: &mut Vec<String>) {
    let golden = case.join(file);
    if std::env::var_os("UPDATE_GOLDEN").is_some() {
        std::fs::write(&golden, &actual).unwrap();
        return;
    }
    match std::fs::read_to_string(&golden) {
        Ok(expected) if expected == actual => {}
        Ok(expected) => failures.push(format!(
            "{}/{file}\n--- expected\n{expected}\n--- actual\n{actual}",
            case.file_name().unwrap().to_string_lossy()
        )),
        Err(_) => failures.push(format!(
            "{}: missing {file}; actual output was:\n{actual}",
            case.display()
        )),
    }
}

#[test]
fn validate_fixtures_match_goldens() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("project").is_dir())
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no fixtures found");

    let mut failures = Vec::new();
    for case in &cases {
        check(case, "expected.txt", run_case(case, false), &mut failures);
        if case.join("expected.json").exists() {
            check(case, "expected.json", run_case(case, true), &mut failures);
        }
    }
    assert!(
        failures.is_empty(),
        "{} fixture(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
