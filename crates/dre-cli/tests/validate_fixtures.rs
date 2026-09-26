//! Fixture harness for `dre validate`.
//!
//! Each directory under `tests/fixtures/validate/` is one case:
//!
//! - `project/`   the DRE project to validate (required)
//! - `profiles/`  passed as `--profiles-dir` when present
//! - `args`       extra CLI arguments, one per line (optional)
//! - `env`        `KEY=VALUE` lines set for the run (optional)
//! - `no_profiles_flag`  marker: don't pass `--profiles-dir` (tests the env/home fallbacks)
//!
//! `$CASE` in `args` and `env` expands to the case directory.
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

/// Stand-in executables for the plugins fixtures commonly declare, so `--no-auto-install`
/// doesn't warn in every golden. Validation never starts them.
fn stand_in_plugins() -> PathBuf {
    static DIR: std::sync::OnceLock<tempfile::TempDir> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let d = tempfile::tempdir().unwrap();
        let names = [
            "source-duckdb",
            "source-postgres",
            "source-fixture",
            "format-csv",
            "format-delimited",
            "format-xlsx",
            "format-parquet",
            "format-fixed_width",
            "destination-sftp",
            "destination-s3",
        ];
        for n in names {
            let p = d.path().join(format!("dre-{n}{}", std::env::consts::EXE_SUFFIX));
            std::fs::write(&p, "").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
            }
        }
        d
    })
    .path()
    .to_path_buf()
}

fn run_case(case: &Path, json: bool) -> String {
    let mut cmd = Command::cargo_bin("dre").unwrap();
    cmd.arg("validate").arg("--project-dir").arg(case.join("project"));
    let case_str = case.to_string_lossy().to_string();
    let expand = |s: &str| s.replace("$CASE", &case_str);
    let profiles = case.join("profiles");
    // HOME points inside the case, so the ~/.dre fallback never reads the developer's files.
    cmd.env("HOME", case.join("home"))
        .env("USERPROFILE", case.join("home"));
    if !case.join("no_profiles_flag").exists() {
        // Without a profiles/ dir, point at one that has no profiles.yml.
        let dir = if profiles.exists() {
            profiles
        } else {
            case.join("no-profiles")
        };
        cmd.arg("--profiles-dir").arg(dir);
    }
    // Never touch the network or real plugin dirs from the validate harness.
    cmd.arg("--no-auto-install");
    cmd.env("DRE_PLUGINS_DIR", stand_in_plugins());
    cmd.env_remove("DRE_PROFILES_DIR");
    if let Ok(args) = std::fs::read_to_string(case.join("args")) {
        for a in args.lines().filter(|l| !l.trim().is_empty()) {
            cmd.arg(expand(a.trim()));
        }
    }
    if let Ok(env) = std::fs::read_to_string(case.join("env")) {
        for line in env.lines().filter(|l| !l.trim().is_empty()) {
            let (k, v) = line.split_once('=').expect("env lines are KEY=VALUE");
            cmd.env(k.trim(), expand(v.trim()));
        }
    }
    if json {
        cmd.arg("--json");
    }
    let out = cmd.output().unwrap();
    let code = out.status.code().unwrap_or(-1);
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    let stdout = stdout.replace(&case_str.replace('\\', "/"), "$CASE");
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
