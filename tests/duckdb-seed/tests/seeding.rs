//! Subprocess tests keep `DuckDB` linked only into the helper, not the test harness.
use std::io::{Seek, Write};
use std::path::Path;
use std::process::{Command, Output, Stdio};

fn seed(path: &Path, sql: &str) -> Output {
    let mut input = tempfile::tempfile().unwrap();
    input.write_all(sql.as_bytes()).unwrap();
    input.rewind().unwrap();
    Command::new(env!("CARGO_BIN_EXE_dre-test-duckdb-seed"))
        .arg(path)
        .stdin(Stdio::from(input))
        .output()
        .unwrap()
}

fn succeeds(path: &Path, sql: &str) {
    let output = seed(path, sql);
    assert!(
        output.status.success(),
        "{}: {}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn seeds_and_updates_a_database_with_spaces_and_unicode_in_its_path() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("sales données 東京.duckdb");
    succeeds(
        &path,
        "create table seeds(value varchar); insert into seeds values ('quotes '' and \"; $(no shell) 東京');",
    );
    succeeds(&path, "insert into seeds values ('second');");
    succeeds(
        &path,
        "select case when count(*) = 2 and min(length(value)) = 6 then true else error('seed contents differ') end from seeds;",
    );
    assert!(path.is_file());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
}

#[test]
fn invalid_sql_does_not_publish_a_new_database_or_change_an_existing_one() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.duckdb");
    let invalid = "begin; create table partial(value int); insert into partial values (1); commit; select error('deliberate seed failure');";
    let output = seed(&path, invalid);
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("deliberate seed failure"), "{stderr}");
    assert!(stderr.contains("executing SQL from stdin"), "{stderr}");
    assert!(stderr.contains(&path.display().to_string()), "{stderr}");
    assert!(!path.exists());
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

    succeeds(
        &path,
        "create table original(value int); insert into original values (42);",
    );
    let before = std::fs::read(&path).unwrap();
    assert!(!seed(&path, invalid).status.success());
    assert_eq!(std::fs::read(&path).unwrap(), before);
    succeeds(
        &path,
        "select case when value = 42 then true else error('original changed') end from original; select case when count(*) = 0 then true else error('partial seed published') end from information_schema.tables where table_name = 'partial';",
    );
}

#[test]
fn parallel_helpers_keep_databases_isolated() {
    let dirs: Vec<_> = (0..8).map(|_| tempfile::tempdir().unwrap()).collect();
    std::thread::scope(|scope| {
        for (value, dir) in dirs.iter().enumerate() {
            scope.spawn(move || {
                let path = dir.path().join("same name.duckdb");
                succeeds(&path, &format!("create table seeds as select {value} as value;"));
                succeeds(&path, &format!("select case when count(*) = 1 and min(value) = {value} then true else error('cross-database state') end from seeds;"));
            });
        }
    });
}

#[test]
fn accepts_relative_database_paths() {
    let dir = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_dre-test-duckdb-seed"))
        .arg("relative.duckdb")
        .current_dir(dir.path())
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(dir.path().join("relative.duckdb").is_file());
}

#[test]
fn malformed_invocations_fail_with_usage() {
    for args in [vec![], vec!["one.duckdb", "unexpected"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_dre-test-duckdb-seed"))
            .args(args)
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("usage: dre-test-duckdb-seed DATABASE < SQL")
        );
    }
}

#[test]
fn refuses_to_replace_a_database_with_an_existing_wal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("data.duckdb");
    succeeds(&path, "create table original as select 42 as value;");
    let before = std::fs::read(&path).unwrap();
    let wal = dir.path().join("data.duckdb.wal");
    std::fs::write(&wal, "sentinel").unwrap();
    let output = seed(&path, "create table replacement as select 0 as value;");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("close and checkpoint"));
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert_eq!(std::fs::read_to_string(&wal).unwrap(), "sentinel");
}
