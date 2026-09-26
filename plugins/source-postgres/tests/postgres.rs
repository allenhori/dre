//! Conformance always; integration tests when `DRE_TEST_POSTGRES=host:port` points at a server
//! with user/password/database `dre` (CI runs one as a service container).

use std::path::Path;
use std::sync::Arc;

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::datatypes::{DataType, Decimal128Type, Int32Type, TimeUnit};
use dre_protocol::host::{Execution, LogSink, PluginProcess};
use dre_protocol::{CAP_CHECK, CAP_READ_ONLY, CAP_SESSIONS, conformance};
use serde_json::{Map, Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-source-postgres"))
}

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

fn server() -> Option<(String, u16)> {
    let v = std::env::var("DRE_TEST_POSTGRES").ok()?;
    let (h, p) = v.split_once(':')?;
    Some((h.to_string(), p.parse().ok()?))
}

fn conn(extra: Value) -> Map<String, Value> {
    let (host, port) = server().unwrap();
    let mut m = json!({"host": host, "port": port, "user": "dre", "password": "dre", "database": "dre", "sslmode": "disable"});
    if let Value::Object(e) = extra {
        m.as_object_mut().unwrap().extend(e);
    }
    m.as_object().unwrap().clone()
}

fn open(read_only: bool, extra: Value) -> PluginProcess {
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    p.open(conn(extra), read_only).unwrap();
    p
}

fn collect(p: &mut PluginProcess, sql: &str, limit: Option<u64>) -> (Execution, Vec<RecordBatch>) {
    let mut out = Vec::new();
    let e = p
        .execute(sql, limit, |_, b| {
            out.push(b);
            Ok(())
        })
        .unwrap_or_else(|e| panic!("{sql}: {e}"));
    (e, out)
}

macro_rules! needs_server {
    () => {
        if server().is_none() {
            eprintln!("skipped: set DRE_TEST_POSTGRES=host:port to run");
            return;
        }
    };
}

#[test]
fn advertises_sessions_read_only_and_check() {
    needs_server!();
    let p = open(false, json!({}));
    assert!(p.has(CAP_SESSIONS) && p.has(CAP_READ_ONLY) && p.has(CAP_CHECK));
}

#[test]
fn temp_tables_persist_and_result_sets_are_told_apart_from_side_effects() {
    needs_server!();
    let mut p = open(false, json!({}));
    assert!(matches!(
        collect(&mut p, "create temp table t (id int, name text)", None).0,
        Execution::NoResult { .. }
    ));
    match collect(
        &mut p,
        "insert into t values (1, 'Acme Corp'), (2, 'Client A')",
        None,
    )
    .0
    {
        Execution::NoResult { rows_affected } => assert_eq!(rows_affected, Some(2)),
        other => panic!("{other:?}"),
    }
    assert!(matches!(
        collect(&mut p, "set statement_timeout = 0", None).0,
        Execution::NoResult { .. }
    ));
    let (e, b) = collect(&mut p, "select id, name from t order by id", None);
    assert!(matches!(e, Execution::Result { rows: 2, .. }));
    assert_eq!(b[0].column(0).as_primitive::<Int32Type>().values(), &[1, 2]);
    assert_eq!(b[0].column(1).as_string::<i32>().value(1), "Client A");
    // An empty result still has a schema.
    match collect(&mut p, "select id from t where false", None).0 {
        Execution::Result { rows: 0, schema } => assert_eq!(schema.field(0).name(), "id"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn types_map_to_arrow() {
    needs_server!();
    let mut p = open(false, json!({}));
    let (_, b) = collect(
        &mut p,
        "select true as b, 1::int2 as s, 2::int8 as l, 1.5::float4 as f, 2.25::float8 as d,
                12345.678::numeric(10,3) as money, 1.5::numeric as loose, (-0.0001)::numeric as tiny,
                date '2026-01-25' as day, time '06:30:00.5' as at,
                timestamp '2026-01-25 12:00:00' as ts, timestamptz '2026-01-25 12:00:00+10' as tstz,
                '6f1c9a52-8d1e-4c4b-9a4e-1f2b3c4d5e6f'::uuid as id, '{\"a\": 1}'::jsonb as j, '\\x0102'::bytea as raw,
                null::int4 as missing",
        None,
    );
    let b = &b[0];
    let s = b.schema();
    let t = |n: &str| s.field_with_name(n).unwrap().data_type().clone();
    assert_eq!(t("b"), DataType::Boolean);
    assert_eq!(t("s"), DataType::Int16);
    assert_eq!(t("money"), DataType::Decimal128(10, 3));
    assert_eq!(
        b.column_by_name("money")
            .unwrap()
            .as_primitive::<Decimal128Type>()
            .value(0),
        12_345_678
    );
    assert_eq!(t("loose"), DataType::Utf8);
    assert_eq!(
        b.column_by_name("loose").unwrap().as_string::<i32>().value(0),
        "1.5"
    );
    assert_eq!(
        b.column_by_name("tiny").unwrap().as_string::<i32>().value(0),
        "-0.0001"
    );
    assert_eq!(t("day"), DataType::Date32);
    assert_eq!(t("at"), DataType::Time64(TimeUnit::Microsecond));
    assert_eq!(t("ts"), DataType::Timestamp(TimeUnit::Microsecond, None));
    assert_eq!(
        t("tstz"),
        DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
    );
    let tstz = b
        .column_by_name("tstz")
        .unwrap()
        .as_primitive::<arrow::datatypes::TimestampMicrosecondType>()
        .value(0);
    assert_eq!(tstz, 1_769_306_400_000_000, "12:00+10 is 02:00 UTC");
    assert_eq!(
        b.column_by_name("id").unwrap().as_string::<i32>().value(0),
        "6f1c9a52-8d1e-4c4b-9a4e-1f2b3c4d5e6f"
    );
    assert_eq!(
        b.column_by_name("j").unwrap().as_string::<i32>().value(0),
        "{\"a\":1}"
    );
    assert_eq!(t("raw"), DataType::Binary);
    assert!(b.column_by_name("missing").unwrap().is_null(0));
}

#[test]
fn unsupported_types_ask_for_a_cast() {
    needs_server!();
    let mut p = open(false, json!({}));
    let err = p
        .execute("select interval '1 day' as gap", None, |_, _| Ok(()))
        .unwrap_err();
    assert!(
        err.to_string().contains("cast it in the query, e.g. `gap::text`"),
        "{err}"
    );
    // The session is fine afterwards.
    collect(&mut p, "select 1", None);
}

#[test]
fn row_limit_and_large_results_stream_in_batches() {
    needs_server!();
    let mut p = open(false, json!({}));
    let (e, _) = collect(&mut p, "select g from generate_series(1, 100000) g", Some(10));
    assert!(matches!(e, Execution::Result { rows: 10, .. }));
    let (e, batches) = collect(&mut p, "select g from generate_series(1, 20000) g", None);
    assert!(matches!(e, Execution::Result { rows: 20000, .. }));
    assert!(batches.len() >= 3, "streamed in batches");
}

#[test]
fn read_only_sessions_refuse_writes() {
    needs_server!();
    let mut w = open(false, json!({}));
    collect(&mut w, "create table if not exists ro_probe (n int)", None);
    let mut r = open(true, json!({}));
    let err = r
        .execute("insert into ro_probe values (1)", None, |_, _| Ok(()))
        .unwrap_err();
    assert!(err.to_string().contains("read-only transaction"), "{err}");
}

#[test]
fn check_explains_without_executing_and_reports_errors() {
    needs_server!();
    let mut p = open(false, json!({}));
    collect(&mut p, "create temp table c (n int)", None);
    p.check("insert into c values (1)").unwrap();
    assert!(matches!(
        collect(&mut p, "select * from c", None).0,
        Execution::Result { rows: 0, .. }
    ));
    let err = p.check("select nope from c").unwrap_err();
    assert!(
        err.to_string().contains("column \"nope\" does not exist"),
        "{err}"
    );
}

#[test]
fn schema_sets_the_search_path() {
    needs_server!();
    let mut setup = open(false, json!({}));
    collect(&mut setup, "create schema if not exists client_a", None);
    collect(
        &mut setup,
        "create table if not exists client_a.accounts as select 42 as n",
        None,
    );
    let mut p = open(false, json!({"schema": "client_a"}));
    let (_, b) = collect(&mut p, "select n from accounts", None);
    assert_eq!(b[0].column(0).as_primitive::<Int32Type>().value(0), 42);
}

#[test]
fn bad_credentials_give_a_clear_error() {
    needs_server!();
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let err = p.open(conn(json!({"password": "wrong"})), false).unwrap_err();
    assert!(
        err.to_string().contains("password authentication failed"),
        "{err}"
    );
}
