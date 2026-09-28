//! First-party plugins built outside Cargo (the Go Databricks package, `dre-plugin-databricks`,
//! serving the `databricks` source and destination), checked through the same host code core
//! uses. CI builds them and sets `DRE_TEST_GO_PLUGINS` to their directory; the
//! tests skip themselves when it's unset. The real-warehouse test also needs
//! `DRE_TEST_DATABRICKS_HOST`, `_HTTP_PATH` and `_TOKEN`.

use std::path::PathBuf;
use std::sync::Arc;

use dre_protocol::host::{Execution, LogSink, PluginProcess};
use dre_protocol::{CAP_CHECK, CAP_LOAD, CAP_SESSIONS, Kind, PluginId, conformance};
use serde_json::json;

fn go_plugin(name: &str) -> Option<PathBuf> {
    let dir = std::env::var_os("DRE_TEST_GO_PLUGINS")?;
    let exe = if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_string()
    };
    let p = PathBuf::from(dir).join(exe);
    assert!(
        p.exists(),
        "DRE_TEST_GO_PLUGINS is set but {} doesn't exist",
        p.display()
    );
    Some(p)
}

const PACKAGE: &str = "dre-plugin-databricks";

fn source() -> PluginId {
    PluginId::new(Kind::Source, "databricks")
}

/// Start the package as the `databricks` source.
fn start_source(bin: &std::path::Path, log: LogSink) -> PluginProcess {
    PluginProcess::start_for(bin, Some(&source()), log, None).unwrap()
}

#[test]
fn the_go_databricks_package_conforms_to_the_protocol() {
    let Some(bin) = go_plugin(PACKAGE) else {
        eprintln!("skipped: set DRE_TEST_GO_PLUGINS to the built Go plugins");
        return;
    };
    conformance::assert_conforms(&bin);
    let log: LogSink = Arc::new(|_, _| {});
    let p = start_source(&bin, log.clone());
    assert!(p.has(CAP_SESSIONS) && p.has(CAP_CHECK) && p.has(CAP_LOAD));
    assert_eq!(
        p.info().provides,
        vec![source(), PluginId::new(Kind::Destination, "databricks")]
    );
    let dest = PluginId::new(Kind::Destination, "databricks");
    let p = PluginProcess::start_for(&bin, Some(&dest), log, None).unwrap();
    assert_eq!(p.info().kind, Kind::Destination);
}

#[test]
fn the_go_databricks_source_explains_a_missing_field() {
    let Some(bin) = go_plugin(PACKAGE) else {
        return;
    };
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = start_source(&bin, log);
    let conn = json!({"host": "dbc-1.cloud.databricks.com"});
    let err = p
        .open(conn.as_object().unwrap().clone(), false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("needs a `http_path` field"), "{err}");
    let conn = json!({"host": "h", "http_path": "/p", "auth_type": "saml"});
    let err = p
        .open(conn.as_object().unwrap().clone(), false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("unknown `auth_type` `saml`"), "{err}");
}

/// A real warehouse: one session across statements, typed Arrow results, `check`, `load`.
#[test]
fn the_go_databricks_source_holds_one_session_on_a_real_warehouse() {
    let Some(bin) = go_plugin(PACKAGE) else {
        return;
    };
    let (Ok(host), Ok(path), Ok(token)) = (
        std::env::var("DRE_TEST_DATABRICKS_HOST"),
        std::env::var("DRE_TEST_DATABRICKS_HTTP_PATH"),
        std::env::var("DRE_TEST_DATABRICKS_TOKEN"),
    ) else {
        eprintln!("skipped: set DRE_TEST_DATABRICKS_HOST, _HTTP_PATH and _TOKEN to run");
        return;
    };
    let log: LogSink = Arc::new(|_, l| eprintln!("[plugin] {l}"));
    let mut p = start_source(&bin, log);
    let conn = json!({"host": host, "http_path": path, "token": token});
    p.open(conn.as_object().unwrap().clone(), false).unwrap();
    let mut run = |sql: &str, limit: Option<u64>| {
        let mut batches = Vec::new();
        let e = p
            .execute(sql, limit, |_, b| {
                batches.push(b);
                Ok(())
            })
            .unwrap_or_else(|e| panic!("{sql}: {e}"));
        (e, batches)
    };
    run(
        "create or replace temporary view dre_probe as \
         select id, cast(id * 1.5 as decimal(10, 2)) as amount, date'2026-01-25' as day, \
         timestamp'2026-01-25 12:00:00.5' as ts, id % 2 = 0 as even, null as nothing \
         from range(120000)",
        None,
    );
    let (e, b) = run("select * from dre_probe order by id", None);
    assert!(matches!(e, Execution::Result { rows: 120_000, .. }), "{e:?}");
    let schema = b[0].schema();
    let t = |n: &str| schema.field_with_name(n).unwrap().data_type().to_string();
    assert_eq!(t("id"), "Int64");
    assert_eq!(t("amount"), "Decimal128(10, 2)");
    assert_eq!(t("day"), "Date32");
    assert!(
        t("ts").starts_with("Timestamp(") && t("ts").contains("UTC"),
        "{}",
        t("ts")
    );
    assert_eq!(t("even"), "Boolean");
    assert_eq!(t("nothing"), "Utf8");
    let (e, _) = run("select * from dre_probe", Some(10));
    assert!(matches!(e, Execution::Result { rows: 10, .. }), "{e:?}");
    let (e, b) = run("select * from dre_probe where id < 0", None);
    assert!(matches!(e, Execution::Result { rows: 0, .. }), "{e:?}");
    assert_eq!(b[0].num_columns(), 6, "an empty result still carries its columns");
    assert!(matches!(
        run("set ansi_mode = true", None).0,
        Execution::Result { .. } | Execution::NoResult { .. }
    ));
    p.check("select id from dre_probe").unwrap();
    let err = p.check("select nope from dre_probe").unwrap_err().to_string();
    assert!(err.contains("nope"), "{err}");
    let err = p
        .execute("select * from no_such_table_dre", None, |_, _| Ok(()))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("TABLE_OR_VIEW_NOT_FOUND") && !err.contains("\tat "),
        "{err}"
    );
}
