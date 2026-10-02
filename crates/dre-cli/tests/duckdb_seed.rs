mod common;

use common::{TestProject, workspace_bin};
use std::path::PathBuf;

fn project() -> TestProject {
    TestProject {
        dir: tempfile::tempdir().unwrap(),
        plugins: PathBuf::new(),
    }
}

#[test]
fn test_project_seeds_its_database_and_panics_with_helper_diagnostics_on_failure() {
    let p = project();
    std::fs::create_dir_all(p.root()).unwrap();
    p.duckdb("seed données.duckdb", "create table seeds as select 42 as value;");
    p.duckdb(
        "seed données.duckdb",
        "select case when value = 42 then true else error('wrong seed') end from seeds;",
    );
    let before = std::fs::read(p.path("seed données.duckdb")).unwrap();
    let failure = std::panic::catch_unwind(|| {
        p.duckdb(
            "seed données.duckdb",
            "select error('deliberate helper failure');",
        );
    })
    .unwrap_err();
    let message = failure.downcast_ref::<String>().unwrap();
    for context in [
        "dre-test-duckdb-seed",
        "seed données.duckdb",
        "SQL via stdin",
        "helper exited with",
        "stderr:",
        "deliberate helper failure",
    ] {
        assert!(message.contains(context), "missing {context:?}: {message}");
    }
    assert_eq!(std::fs::read(p.path("seed données.duckdb")).unwrap(), before);
}

#[test]
fn helper_spawn_failure_identifies_executable_database_and_sql() {
    let p = project();
    let helper = p.dir.path().join("missing helper");
    let database = p.path("seed.duckdb");
    let error = common::duckdb::seed(&helper, &database, "select 1;").unwrap_err();
    let message = format!("{error:#}");
    for context in [
        "missing helper",
        "seed.duckdb",
        "select 1;",
        "invoking DuckDB seed helper",
    ] {
        assert!(message.contains(context), "missing {context:?}: {message}");
    }
    assert!(!database.exists());
}

#[test]
fn helper_open_failure_includes_status_stderr_and_invocation_context() {
    let p = project();
    let helper = workspace_bin("dre-test-duckdb-seed");
    let database = p.path("missing parent/seed.duckdb");
    let message = format!(
        "{:#}",
        common::duckdb::seed(&helper, &database, "select 1;").unwrap_err()
    );
    for context in [
        "helper exited with",
        "stderr:",
        "creating staging directory",
        "seed.duckdb",
        "select 1;",
    ] {
        assert!(message.contains(context), "missing {context:?}: {message}");
    }
    assert!(!database.exists());
}
