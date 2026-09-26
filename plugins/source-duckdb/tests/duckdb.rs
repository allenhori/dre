use std::path::Path;
use std::sync::Arc;

use dre_protocol::host::{Execution, LogSink, PluginProcess};
use dre_protocol::{CAP_CHECK, CAP_READ_ONLY, CAP_SESSIONS, conformance};
use serde_json::{Map, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-source-duckdb"))
}

fn open(path: &str, read_only: bool) -> PluginProcess {
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let mut c = Map::new();
    c.insert("path".into(), json!(path));
    p.open(c, read_only).unwrap();
    p
}

fn rows(p: &mut PluginProcess, sql: &str) -> Option<u64> {
    match p.execute(sql, None, |_, _| Ok(())).unwrap() {
        Execution::Result { rows, .. } => Some(rows),
        Execution::NoResult { .. } => None,
    }
}

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn advertises_sessions_read_only_and_check() {
    let p = open(":memory:", false);
    assert!(p.has(CAP_SESSIONS) && p.has(CAP_READ_ONLY) && p.has(CAP_CHECK));
}

#[test]
fn temp_tables_persist_across_statements_in_one_session() {
    let mut p = open(":memory:", false);
    assert_eq!(
        rows(&mut p, "create temp table t as select range as n from range(5)"),
        None
    );
    assert_eq!(rows(&mut p, "select * from t where n > 1"), Some(3));
}

#[test]
fn only_statements_returning_rows_are_result_sets() {
    let mut p = open(":memory:", false);
    assert_eq!(rows(&mut p, "create table t (n int)"), None);
    match p
        .execute("insert into t values (1), (2)", None, |_, _| Ok(()))
        .unwrap()
    {
        Execution::NoResult { rows_affected } => assert_eq!(rows_affected, Some(2)),
        other => panic!("insert should have no result set: {other:?}"),
    }
    assert_eq!(rows(&mut p, "set threads = 2"), None);
    // A query that happens to return one BIGINT column called Count is still a result set.
    assert_eq!(
        rows(&mut p, "select count(*)::bigint as \"Count\" from t"),
        Some(1)
    );
    assert_eq!(rows(&mut p, "select * from t where false"), Some(0));
}

#[test]
fn row_limit_caps_the_result() {
    let mut p = open(":memory:", false);
    let e = p
        .execute("select * from range(10000)", Some(25), |_, _| Ok(()))
        .unwrap();
    assert!(matches!(e, Execution::Result { rows: 25, .. }));
}

#[test]
fn read_only_sessions_refuse_writes_to_the_database() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("x.duckdb");
    let db = db.to_str().unwrap();
    let mut w = open(db, false);
    rows(&mut w, "create table t as select 1 as n");
    w.close().unwrap();
    let mut r = open(db, true);
    assert_eq!(rows(&mut r, "select * from t"), Some(1));
    let err = r
        .execute("insert into t values (2)", None, |_, _| Ok(()))
        .unwrap_err();
    assert!(err.to_string().to_lowercase().contains("read-only"), "{err}");
}

#[test]
fn check_verifies_without_executing() {
    let mut p = open(":memory:", false);
    rows(&mut p, "create table t (n int)");
    p.check("insert into t values (1)").unwrap();
    assert_eq!(rows(&mut p, "select * from t"), Some(0));
    let err = p.check("select missing_column from t").unwrap_err();
    assert!(err.to_string().contains("missing_column"), "{err}");
}

#[test]
fn sql_errors_are_reported_and_the_session_survives() {
    let mut p = open(":memory:", false);
    let err = p
        .execute("select * from no_such_table", None, |_, _| Ok(()))
        .unwrap_err();
    assert!(err.to_string().contains("no_such_table"), "{err}");
    assert_eq!(rows(&mut p, "select 1"), Some(1));
}
