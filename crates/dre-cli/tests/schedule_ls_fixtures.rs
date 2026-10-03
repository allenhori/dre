//! Fixture harness for `dre schedule ls`: the conformance table for schedule expansion.
//!
//! Each directory under `tests/fixtures/schedule_ls/` is one case:
//!
//! - `project/`      the DRE project (required; no profiles are needed)
//! - `args`          the command's arguments, one per line (required; must pin `--from`)
//! - `expected.txt`  golden output: exit code, stdout, then stderr
//!
//! Every JSON output is also checked against `docs/schedule-ls.schema.json`.
//! Run with `UPDATE_GOLDEN=1` to rewrite goldens, then review the diff by eye.

use std::path::{Path, PathBuf};

use assert_cmd::Command;

fn fixtures_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/schedule_ls")
}

fn schema() -> jsonschema::Validator {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../docs/schedule-ls.schema.json");
    let schema: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
    jsonschema::validator_for(&schema).unwrap()
}

fn run_case(case: &Path, schema: &jsonschema::Validator, failures: &mut Vec<String>) -> String {
    let name = case.file_name().unwrap().to_string_lossy().to_string();
    let args = std::fs::read_to_string(case.join("args")).unwrap_or_default();
    let args: Vec<&str> = args.lines().map(str::trim).filter(|l| !l.is_empty()).collect();
    assert!(
        args.contains(&"--from"),
        "{name}: every case pins --from, so it doesn't depend on today"
    );
    let home = tempfile::tempdir().unwrap();
    let out = Command::cargo_bin("dre")
        .unwrap()
        .args(["schedule", "ls", "--project-dir"])
        .arg(case.join("project"))
        .args(&args)
        .env("HOME", home.path())
        .env("USERPROFILE", home.path())
        .env_remove("DRE_PROFILES_DIR")
        .env_remove("DRE_TIMEZONE")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if let Ok(doc) = serde_json::from_str::<serde_json::Value>(&stdout) {
        let errors: Vec<String> = schema
            .iter_errors(&doc)
            .map(|e| format!("{e} at {}", e.instance_path))
            .collect();
        if !errors.is_empty() {
            failures.push(format!("{name}: output doesn't match the schema: {errors:#?}"));
        }
    }
    // Keep the goldens release-independent.
    let stdout = stdout.replace(
        &format!("\"dre_version\": \"{}\"", env!("CARGO_PKG_VERSION")),
        "\"dre_version\": \"$VERSION\"",
    );
    let stderr = String::from_utf8_lossy(&out.stderr).to_string();
    let stderr = stderr.replace(&case.to_string_lossy().replace('\\', "/"), "$CASE");
    format!(
        "exit: {}\n--- stdout\n{stdout}--- stderr\n{stderr}",
        out.status.code().unwrap_or(-1)
    )
}

#[test]
fn schedule_ls_fixtures_match_goldens() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(fixtures_dir())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("project").is_dir())
        .collect();
    cases.sort();
    assert!(!cases.is_empty(), "no fixtures found");
    let schema = schema();
    let mut failures = Vec::new();
    for case in &cases {
        let actual = run_case(case, &schema, &mut failures);
        let golden = case.join("expected.txt");
        if std::env::var_os("UPDATE_GOLDEN").is_some() {
            std::fs::write(&golden, &actual).unwrap();
            continue;
        }
        match std::fs::read_to_string(&golden) {
            Ok(expected) if expected == actual => {}
            Ok(expected) => failures.push(format!(
                "{}\n--- expected\n{expected}\n--- actual\n{actual}",
                case.file_name().unwrap().to_string_lossy()
            )),
            Err(_) => failures.push(format!(
                "{}: missing expected.txt; actual:\n{actual}",
                case.display()
            )),
        }
    }
    assert!(
        failures.is_empty(),
        "{} fixture(s) failed:\n\n{}",
        failures.len(),
        failures.join("\n\n")
    );
}
