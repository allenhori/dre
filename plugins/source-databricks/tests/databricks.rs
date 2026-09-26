//! The Databricks plugin against a fake HiveServer2 endpoint (there's no Databricks emulator).
//! The fake speaks the same Thrift-over-HTTP messages with a toy SQL engine, so these tests
//! exercise the client: sessions, polling, paging, types, errors, retries. Real-warehouse tests
//! run when `DRE_TEST_DATABRICKS_HOST`, `_HTTP_PATH` and `_TOKEN` are set.

use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use arrow::array::{Array, AsArray, RecordBatch};
use arrow::datatypes::{DataType, Decimal128Type, Int64Type, TimeUnit, TimestampMicrosecondType};
use dre_protocol::host::{Execution, LogSink, PluginProcess};
use dre_protocol::{CAP_CHECK, CAP_SESSIONS, conformance};
use dre_source_databricks::thrift::{
    REPLY, Reader, Struct, T_BOOL, T_DOUBLE, T_I32, T_I64, T_STRING, T_STRUCT, V, message, s,
};
use serde_json::{Map, Value, json};

fn bin() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_dre-source-databricks"))
}

#[derive(Default)]
struct State {
    sessions: HashMap<Vec<u8>, HashMap<String, i64>>,
    ops: HashMap<Vec<u8>, Op>,
    closed_sessions: usize,
    timezone_utc: bool,
    unavailable_left: usize,
    next_id: u8,
}

struct Op {
    cols: Vec<(String, i32, i32, i32)>,
    rows: Vec<Vec<V>>,
    has_result: bool,
    count: f64,
    cursor: usize,
    polls: usize,
    error: Option<String>,
}

struct Fake {
    port: u16,
    state: Arc<Mutex<State>>,
    requests: Arc<AtomicUsize>,
}

fn handle(sess: i32) -> Struct {
    Struct::new().with(
        1,
        V::Struct(
            Struct::new()
                .with(1, V::Bin(vec![sess as u8; 16]))
                .with(2, V::Bin(vec![9; 16])),
        ),
    )
}

fn ok() -> V {
    V::Struct(Struct::new().with(1, V::I32(0)))
}

fn col_desc(name: &str, ty: i32, p: i32, sc: i32) -> V {
    let mut prim = Struct::new().with(1, V::I32(ty));
    if ty == 15 {
        let q = |v: i32| V::Struct(Struct::new().with(1, V::I32(v)));
        prim = prim.with(
            2,
            V::Struct(Struct::new().with(
                1,
                V::Map(
                    T_STRING,
                    T_STRUCT,
                    vec![(s("precision"), q(p)), (s("scale"), q(sc))],
                ),
            )),
        );
    }
    let entry = Struct::new().with(1, V::Struct(prim));
    V::Struct(
        Struct::new()
            .with(1, s(name))
            .with(
                2,
                V::Struct(Struct::new().with(1, V::List(T_STRUCT, vec![V::Struct(entry)]))),
            )
            .with(3, V::I32(0)),
    )
}

/// A column chunk in Thrift's columnar form.
fn column(ty: i32, vals: &[V]) -> V {
    let (id, et) = match ty {
        0 => (1, T_BOOL),
        4 => (5, T_I64),
        3 => (4, T_I32),
        6 => (6, T_DOUBLE),
        _ => (7, T_STRING),
    };
    let mut nulls = vec![0u8; vals.len().div_ceil(8)];
    let values: Vec<V> = vals
        .iter()
        .enumerate()
        .map(|(i, v)| {
            if matches!(v, V::List(..)) {
                nulls[i / 8] |= 1 << (i % 8);
                match et {
                    T_BOOL => V::Bool(false),
                    T_I64 => V::I64(0),
                    T_I32 => V::I32(0),
                    T_DOUBLE => V::Double(0.0),
                    _ => V::Bin(vec![]),
                }
            } else {
                v.clone()
            }
        })
        .collect();
    V::Struct(Struct::new().with(
        id,
        V::Struct(Struct::new().with(1, V::List(et, values)).with(2, V::Bin(nulls))),
    ))
}

const NULL: V = V::List(0, vec![]);

fn engine(st: &mut State, sess: &[u8], sql: &str) -> Op {
    let views = st.sessions.entry(sess.to_vec()).or_default();
    let lower = sql.trim().to_lowercase();
    let none = |count: f64| Op {
        cols: vec![],
        rows: vec![],
        has_result: false,
        count,
        cursor: 0,
        polls: 0,
        error: None,
    };
    let result = |cols: Vec<(&str, i32, i32, i32)>, rows: Vec<Vec<V>>| Op {
        cols: cols
            .into_iter()
            .map(|(n, t, p, s)| (n.to_string(), t, p, s))
            .collect(),
        rows,
        has_result: true,
        count: -1.0,
        cursor: 0,
        polls: 0,
        error: None,
    };
    let failed = |msg: &str| Op {
        error: Some(msg.to_string()),
        ..none(0.0)
    };
    if lower.starts_with("use ") || lower.starts_with("set ") {
        none(-1.0)
    } else if let Some(rest) = lower.strip_prefix("create temporary view ") {
        let (name, val) = rest.split_once(" as select ").unwrap();
        views.insert(name.to_string(), val.trim().parse().unwrap());
        none(-1.0)
    } else if let Some(name) = lower.strip_prefix("select * from ") {
        match views.get(name.trim()) {
            Some(v) => result(vec![("n", 4, 0, 0)], vec![vec![V::I64(*v)]]),
            None => failed(&format!(
                "[TABLE_OR_VIEW_NOT_FOUND] The table or view `{}` cannot be found.\n\tat org.apache.spark.Foo(Foo.scala:1)",
                name.trim()
            )),
        }
    } else if lower == "select range" {
        result(
            vec![("id", 4, 0, 0)],
            (0..120_000).map(|i| vec![V::I64(i)]).collect(),
        )
    } else if lower == "select types" {
        result(
            vec![
                ("b", 0, 0, 0),
                ("i", 3, 0, 0),
                ("d", 6, 0, 0),
                ("s", 7, 0, 0),
                ("m", 15, 10, 2),
                ("day", 17, 0, 0),
                ("ts", 8, 0, 0),
                ("gone", 7, 0, 0),
            ],
            vec![vec![
                V::Bool(true),
                V::I32(7),
                V::Double(2.5),
                s("Acme Corp"),
                s("123.45"),
                s("2026-01-25"),
                s("2026-01-25 12:00:00.5"),
                NULL,
            ]],
        )
    } else if lower.starts_with("insert") {
        none(3.0)
    } else if let Some(q) = lower.strip_prefix("explain ") {
        let plan = if q.contains("nope") {
            "== Physical Plan ==\nError occurred during query planning: \n[UNRESOLVED_COLUMN.WITHOUT_SUGGESTION] A column `nope` cannot be resolved."
        } else {
            "== Physical Plan ==\n*(1) Project [1 AS 1#0]"
        };
        result(vec![("plan", 7, 0, 0)], vec![vec![s(plan)]])
    } else {
        failed(&format!("[PARSE_SYNTAX_ERROR] Syntax error at or near '{sql}'"))
    }
}

fn respond(st: &mut State, name: &str, req: &Struct) -> Struct {
    match name {
        "OpenSession" => {
            if let Some(V::Map(_, _, m)) = req.get(4) {
                st.timezone_utc = m
                    .iter()
                    .any(|(k, v)| *k == s("spark.sql.session.timeZone") && *v == s("UTC"));
            }
            st.next_id += 1;
            Struct::new()
                .with(1, ok())
                .with(2, V::I32(7))
                .with(3, V::Struct(handle(st.next_id as i32)))
        }
        "ExecuteStatement" => {
            let sess = req
                .st(1)
                .and_then(|h| h.st(1))
                .and_then(|g| g.bin(1))
                .unwrap()
                .to_vec();
            let sql = req.str(2).unwrap();
            let op = engine(st, &sess, &sql);
            st.next_id += 1;
            let id = vec![st.next_id; 16];
            let h = Struct::new()
                .with(
                    1,
                    V::Struct(
                        Struct::new()
                            .with(1, V::Bin(id.clone()))
                            .with(2, V::Bin(vec![1; 16])),
                    ),
                )
                .with(2, V::I32(0))
                .with(3, V::Bool(op.has_result))
                .with(4, V::Double(op.count));
            st.ops.insert(id, op);
            Struct::new().with(1, ok()).with(2, V::Struct(h))
        }
        "GetOperationStatus" | "GetResultSetMetadata" | "FetchResults" | "CloseOperation" => {
            let id = req
                .st(1)
                .and_then(|h| h.st(1))
                .and_then(|g| g.bin(1))
                .unwrap()
                .to_vec();
            let op = st.ops.get_mut(&id).unwrap();
            match name {
                "GetOperationStatus" => {
                    op.polls += 1;
                    // Report RUNNING once, so the client has to poll.
                    let state = match (&op.error, op.polls) {
                        (_, 1) => 1,
                        (Some(_), _) => 5,
                        _ => 2,
                    };
                    let mut r = Struct::new().with(1, ok()).with(2, V::I32(state));
                    if let (Some(e), 5) = (&op.error, state) {
                        r = r.with(5, s(e));
                    }
                    r
                }
                "GetResultSetMetadata" => {
                    let cols = op
                        .cols
                        .iter()
                        .map(|(n, t, p, sc)| col_desc(n, *t, *p, *sc))
                        .collect();
                    Struct::new()
                        .with(1, ok())
                        .with(2, V::Struct(Struct::new().with(1, V::List(T_STRUCT, cols))))
                }
                "FetchResults" => {
                    let max = req.i64(3).unwrap() as usize;
                    let end = (op.cursor + max).min(op.rows.len());
                    let chunk = &op.rows[op.cursor..end];
                    op.cursor = end;
                    let columns: Vec<V> = (0..op.cols.len())
                        .map(|c| {
                            column(
                                op.cols[c].1,
                                &chunk.iter().map(|r| r[c].clone()).collect::<Vec<_>>(),
                            )
                        })
                        .collect();
                    let rowset = Struct::new()
                        .with(1, V::I64(0))
                        .with(2, V::List(T_STRUCT, vec![]))
                        .with(3, V::List(T_STRUCT, columns));
                    Struct::new()
                        .with(1, ok())
                        .with(2, V::Bool(end < op.rows.len()))
                        .with(3, V::Struct(rowset))
                }
                _ => Struct::new().with(1, ok()),
            }
        }
        "CloseSession" => {
            st.closed_sessions += 1;
            Struct::new().with(1, ok())
        }
        _ => Struct::new().with(
            1,
            V::Struct(Struct::new().with(1, V::I32(3)).with(5, s("unsupported"))),
        ),
    }
}

fn serve_conn(mut stream: TcpStream, state: Arc<Mutex<State>>, requests: Arc<AtomicUsize>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let mut len = 0;
        let mut auth = String::new();
        loop {
            let mut h = String::new();
            reader.read_line(&mut h).unwrap();
            let h = h.trim_end();
            if h.is_empty() {
                break;
            }
            let (k, v) = h.split_once(':').unwrap();
            match k.to_ascii_lowercase().as_str() {
                "content-length" => len = v.trim().parse().unwrap(),
                "authorization" => auth = v.trim().to_string(),
                _ => {}
            }
        }
        let mut body = vec![0; len];
        reader.read_exact(&mut body).unwrap();
        requests.fetch_add(1, Ordering::SeqCst);
        let reply = |stream: &mut TcpStream, status: &str, extra: &str, body: &[u8]| {
            let head = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\n{extra}\r\n",
                body.len()
            );
            stream.write_all(head.as_bytes()).unwrap();
            stream.write_all(body).unwrap();
        };
        if auth != "Bearer good-token" {
            reply(&mut stream, "401 Unauthorized", "", b"invalid token");
            continue;
        }
        {
            let mut st = state.lock().unwrap();
            if st.unavailable_left > 0 {
                st.unavailable_left -= 1;
                drop(st);
                reply(
                    &mut stream,
                    "503 Service Unavailable",
                    "Retry-After: 0\r\n",
                    b"warehouse starting",
                );
                continue;
            }
        }
        let (name, _, seq, args) = Reader::new(&body).read_message().unwrap();
        let req = args.st(1).cloned().unwrap_or_default();
        let resp = respond(&mut state.lock().unwrap(), &name, &req);
        let out = message(&name, REPLY, seq, &Struct::new().with(0, V::Struct(resp)));
        reply(
            &mut stream,
            "200 OK",
            "Content-Type: application/x-thrift\r\n",
            &out,
        );
    }
}

impl Fake {
    fn start(unavailable: usize) -> Fake {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let state = Arc::new(Mutex::new(State {
            unavailable_left: unavailable,
            ..Default::default()
        }));
        let requests = Arc::new(AtomicUsize::new(0));
        let (st, rq) = (state.clone(), requests.clone());
        std::thread::spawn(move || {
            for stream in listener.incoming().flatten() {
                let (st, rq) = (st.clone(), rq.clone());
                std::thread::spawn(move || serve_conn(stream, st, rq));
            }
        });
        Fake {
            port,
            state,
            requests,
        }
    }

    fn conn(&self, token: &str) -> Map<String, Value> {
        json!({"host": format!("http://127.0.0.1:{}", self.port), "http_path": "/sql/1.0/warehouses/abc", "token": token,
               "catalog": "main", "schema": "client_a"})
        .as_object()
        .unwrap()
        .clone()
    }

    fn open(&self) -> PluginProcess {
        let log: LogSink = Arc::new(|_, _| {});
        let mut p = PluginProcess::start(bin(), log).unwrap();
        p.open(self.conn("good-token"), false).unwrap();
        p
    }
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

#[test]
fn conforms_to_the_protocol() {
    conformance::assert_conforms(bin());
}

#[test]
fn a_session_holds_temp_views_across_statements_and_is_closed() {
    let fake = Fake::start(0);
    let mut p = fake.open();
    assert!(p.has(CAP_SESSIONS) && p.has(CAP_CHECK));
    assert!(matches!(
        collect(&mut p, "create temporary view v as select 42", None).0,
        Execution::NoResult { .. }
    ));
    let (e, b) = collect(&mut p, "select * from v", None);
    assert!(matches!(e, Execution::Result { rows: 1, .. }));
    assert_eq!(b[0].column(0).as_primitive::<Int64Type>().value(0), 42);
    p.close().unwrap();
    let st = fake.state.lock().unwrap();
    assert!(st.timezone_utc, "the session runs in UTC");
    assert_eq!(st.closed_sessions, 1);
}

#[test]
fn statements_without_result_sets_report_affected_rows() {
    let fake = Fake::start(0);
    let mut p = fake.open();
    match collect(&mut p, "insert into t values (1), (2), (3)", None).0 {
        Execution::NoResult { rows_affected } => assert_eq!(rows_affected, Some(3)),
        other => panic!("{other:?}"),
    }
}

#[test]
fn large_results_are_paged_and_row_limit_stops_fetching() {
    let fake = Fake::start(0);
    let mut p = fake.open();
    let (e, batches) = collect(&mut p, "select range", None);
    assert!(matches!(e, Execution::Result { rows: 120_000, .. }));
    assert_eq!(batches.len(), 3);
    assert_eq!(
        batches[2].column(0).as_primitive::<Int64Type>().value(19_999),
        119_999
    );
    let before = fake.requests.load(Ordering::SeqCst);
    let (e, _) = collect(&mut p, "select range", Some(10));
    assert!(matches!(e, Execution::Result { rows: 10, .. }));
    // Execute, status polls, metadata, one fetch, close: no paging through the rest.
    assert!(fake.requests.load(Ordering::SeqCst) - before <= 6);
}

#[test]
fn types_map_to_arrow() {
    let fake = Fake::start(0);
    let mut p = fake.open();
    let (_, b) = collect(&mut p, "select types", None);
    let b = &b[0];
    let s = b.schema();
    let t = |n: &str| s.field_with_name(n).unwrap().data_type().clone();
    assert_eq!(t("b"), DataType::Boolean);
    assert_eq!(t("i"), DataType::Int32);
    assert_eq!(t("m"), DataType::Decimal128(10, 2));
    assert_eq!(
        b.column_by_name("m")
            .unwrap()
            .as_primitive::<Decimal128Type>()
            .value(0),
        12_345
    );
    assert_eq!(t("day"), DataType::Date32);
    assert_eq!(
        t("ts"),
        DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into()))
    );
    assert_eq!(
        b.column_by_name("ts")
            .unwrap()
            .as_primitive::<TimestampMicrosecondType>()
            .value(0),
        1_769_342_400_500_000
    );
    assert_eq!(
        b.column_by_name("s").unwrap().as_string::<i32>().value(0),
        "Acme Corp"
    );
    assert!(b.column_by_name("gone").unwrap().is_null(0));
}

#[test]
fn server_errors_are_reported_without_stack_traces() {
    let fake = Fake::start(0);
    let mut p = fake.open();
    let err = p
        .execute("select * from missing", None, |_, _| Ok(()))
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("[TABLE_OR_VIEW_NOT_FOUND]") && !err.contains("scala"),
        "{err}"
    );
    collect(&mut p, "create temporary view w as select 1", None);
}

#[test]
fn check_uses_explain_and_catches_planning_errors() {
    let fake = Fake::start(0);
    let mut p = fake.open();
    p.check("select 1").unwrap();
    let err = p.check("select nope").unwrap_err().to_string();
    assert!(err.contains("UNRESOLVED_COLUMN") && err.contains("nope"), "{err}");
}

#[test]
fn a_starting_warehouse_is_waited_for() {
    let fake = Fake::start(3);
    let mut p = fake.open();
    assert!(matches!(
        collect(&mut p, "select range", Some(1)).0,
        Execution::Result { rows: 1, .. }
    ));
}

#[test]
fn a_bad_token_is_a_clear_error() {
    let fake = Fake::start(0);
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let err = p.open(fake.conn("wrong"), false).unwrap_err().to_string();
    assert!(
        err.contains("HTTP 401") && err.contains("check the token"),
        "{err}"
    );
}

/// Against a real warehouse, when credentials are provided.
#[test]
fn real_warehouse_session_round_trip() {
    let (Ok(host), Ok(path), Ok(token)) = (
        std::env::var("DRE_TEST_DATABRICKS_HOST"),
        std::env::var("DRE_TEST_DATABRICKS_HTTP_PATH"),
        std::env::var("DRE_TEST_DATABRICKS_TOKEN"),
    ) else {
        eprintln!("skipped: set DRE_TEST_DATABRICKS_HOST, _HTTP_PATH and _TOKEN to run");
        return;
    };
    let log: LogSink = Arc::new(|_, _| {});
    let mut p = PluginProcess::start(bin(), log).unwrap();
    let conn = json!({"host": host, "http_path": path, "token": token});
    p.open(conn.as_object().unwrap().clone(), false).unwrap();
    collect(
        &mut p,
        "create or replace temporary view dre_probe as select 1 as n, current_date() as d",
        None,
    );
    let (e, _) = collect(&mut p, "select * from dre_probe", None);
    assert!(matches!(e, Execution::Result { rows: 1, .. }));
    p.check("select * from dre_probe").unwrap();
    assert!(p.check("select nope from dre_probe").is_err());
}
