//! DRE source plugin for Databricks SQL warehouses.
//!
//! Databricks' REST Statement Execution API runs every statement on its own, so temp views
//! don't survive between statements. This plugin instead holds a real session using the
//! HiveServer2 protocol over HTTPS (the protocol Databricks' ODBC/JDBC drivers speak): one
//! session per Binding, so temp views and `SET`s persist and the plugin advertises `sessions`.
//!
//! Profile output fields: `host`, `http_path`, `token` (personal access token or OAuth token),
//! optional `catalog`, `schema`, and `retry_timeout` (seconds to keep retrying while a stopped
//! warehouse starts; default 900).
//!
//! The session runs in UTC, so timestamps arrive as UTC instants. Complex types (arrays, maps,
//! structs) and intervals arrive as text. Warehouses have no read-only session mode, so the
//! plugin doesn't advertise `read_only`. `check` uses `EXPLAIN`.

pub mod thrift;

use std::sync::Arc;
use std::time::{Duration, Instant};

use arrow::array::{
    ArrayRef, BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder, Float32Builder,
    Float64Builder, Int8Builder, Int16Builder, Int32Builder, Int64Builder, RecordBatch, StringBuilder,
    TimestampMicrosecondBuilder,
};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
use dre_protocol::msg::ConnectionField;
use dre_protocol::plugin::{About, Result, ResultSink, Source, conn_required, conn_str, serve_source};
use dre_protocol::{CAP_CHECK, CAP_SESSIONS};
use serde_json::{Map, Value};
use thrift::{CALL, EXCEPTION, Reader, Struct, T_STRING, V, message, s};

/// HIVE_CLI_SERVICE_PROTOCOL_V8: columnar result sets.
const CLIENT_PROTOCOL: i32 = 7;
const FETCH_ROWS: i64 = 50_000;

// HiveServer2 operation states.
const FINISHED: i32 = 2;
const CANCELED: i32 = 3;
const CLOSED: i32 = 4;
const ERROR: i32 = 5;
const TIMEDOUT: i32 = 8;

pub struct Hs2 {
    url: String,
    token: String,
    agent: ureq::Agent,
    seq: i32,
    session: Option<Struct>,
    retry_timeout: Duration,
}

/// A column of the result schema: name, HiveServer2 type id, decimal precision and scale.
#[derive(Debug, Clone)]
pub struct Col {
    pub name: String,
    pub type_id: i32,
    pub precision: u8,
    pub scale: i8,
}

impl Hs2 {
    pub fn new(host: &str, http_path: &str, token: &str, retry_timeout: Duration) -> Hs2 {
        let host = host.trim_end_matches('/');
        let base = if host.starts_with("http://") || host.starts_with("https://") {
            host.to_string()
        } else {
            format!("https://{host}")
        };
        let path = if http_path.starts_with('/') {
            http_path.to_string()
        } else {
            format!("/{http_path}")
        };
        let agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(900)))
            .build()
            .new_agent();
        Hs2 {
            url: format!("{base}{path}"),
            token: token.to_string(),
            agent,
            seq: 0,
            session: None,
            retry_timeout,
        }
    }

    /// One RPC: POST a Thrift call, retrying while the warehouse is starting (429/503).
    fn call(&mut self, method: &str, req: Struct) -> Result<Struct> {
        self.seq += 1;
        let body = message(method, CALL, self.seq, &Struct::new().with(1, V::Struct(req)));
        let started = Instant::now();
        let mut wait = Duration::from_secs(2);
        let bytes = loop {
            let resp = self
                .agent
                .post(&self.url)
                .header("Authorization", &format!("Bearer {}", self.token))
                .header("Content-Type", "application/x-thrift")
                .header("Accept", "application/x-thrift")
                .header("User-Agent", "dre")
                .send(&body[..]);
            let mut resp = match resp {
                Ok(r) => r,
                Err(e) => return Err(format!("can't reach Databricks at {}: {e}", self.url).into()),
            };
            let status = resp.status().as_u16();
            if status == 200 {
                break resp.body_mut().with_config().limit(1 << 30).read_to_vec()?;
            }
            if matches!(status, 429 | 503) && started.elapsed() < self.retry_timeout {
                let retry_after = resp
                    .headers()
                    .get("Retry-After")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .map(Duration::from_secs);
                if method == "OpenSession" && started.elapsed() < Duration::from_secs(1) {
                    eprintln!("the warehouse isn't ready (HTTP {status}); waiting for it to start");
                }
                std::thread::sleep(retry_after.unwrap_or(wait).min(Duration::from_secs(60)));
                wait = (wait * 2).min(Duration::from_secs(30));
                continue;
            }
            let text = resp.body_mut().read_to_string().unwrap_or_default();
            let hint = match status {
                401 | 403 => " (check the token and that it can use this warehouse)",
                404 => " (check `host` and `http_path`)",
                _ => "",
            };
            return Err(format!(
                "Databricks answered HTTP {status}{hint}: {}",
                text.chars().take(300).collect::<String>()
            )
            .into());
        };
        let (_, kind, _, result) = Reader::new(&bytes).read_message()?;
        if kind == EXCEPTION {
            return Err(format!(
                "Databricks rejected `{method}`: {}",
                result.str(1).unwrap_or_default()
            )
            .into());
        }
        let resp = result
            .st(0)
            .cloned()
            .ok_or_else(|| format!("`{method}` returned no result"))?;
        check_status(&resp)?;
        Ok(resp)
    }

    pub fn open(&mut self) -> Result<()> {
        let req = Struct::new().with(1, V::I32(CLIENT_PROTOCOL)).with(
            4,
            V::Map(
                T_STRING,
                T_STRING,
                vec![(s("spark.sql.session.timeZone"), s("UTC"))],
            ),
        );
        let resp = self.call("OpenSession", req)?;
        self.session = Some(
            resp.st(3)
                .cloned()
                .ok_or("OpenSession returned no session handle")?,
        );
        Ok(())
    }

    fn session(&self) -> Result<Struct> {
        self.session.clone().ok_or_else(|| "no open session".into())
    }

    /// Run a statement to completion; returns its operation handle.
    pub fn execute(&mut self, sql: &str) -> Result<Struct> {
        let req = Struct::new()
            .with(1, V::Struct(self.session()?))
            .with(2, s(sql))
            .with(4, V::Bool(true));
        let resp = self.call("ExecuteStatement", req)?;
        let op = resp
            .st(2)
            .cloned()
            .ok_or("ExecuteStatement returned no operation handle")?;
        let mut wait = Duration::from_millis(50);
        loop {
            let st = self.call("GetOperationStatus", Struct::new().with(1, V::Struct(op.clone())))?;
            match st.i32(2) {
                Some(FINISHED) => return Ok(op),
                Some(state @ (CANCELED | CLOSED | ERROR | TIMEDOUT)) => {
                    let msg = st
                        .str(5)
                        .or_else(|| st.st(1).and_then(|s| s.str(5)))
                        .unwrap_or_else(|| format!("state {state}"));
                    let _ = self.close_op(&op);
                    return Err(clean_error(&msg).into());
                }
                _ => {
                    std::thread::sleep(wait);
                    wait = (wait * 2).min(Duration::from_secs(1));
                }
            }
        }
    }

    pub fn columns(&mut self, op: &Struct) -> Result<Vec<Col>> {
        let resp = self.call(
            "GetResultSetMetadata",
            Struct::new().with(1, V::Struct(op.clone())),
        )?;
        let cols = resp.st(2).and_then(|t| t.list(1)).unwrap_or_default();
        cols.iter()
            .map(|c| {
                let V::Struct(c) = c else {
                    return Err("bad column descriptor".into());
                };
                let name = c.str(1).unwrap_or_default();
                let entry = c.st(2).and_then(|t| t.list(1)).and_then(|l| l.first()).cloned();
                let (type_id, precision, scale) = match entry {
                    Some(V::Struct(e)) => match e.st(1) {
                        Some(prim) => {
                            let q = |k: &str| -> Option<i32> {
                                let V::Map(_, _, m) = prim.st(2)?.get(1)? else {
                                    return None;
                                };
                                m.iter().find_map(|(key, v)| match (key, v) {
                                    (V::Bin(b), V::Struct(val)) if b == k.as_bytes() => val.i32(1),
                                    _ => None,
                                })
                            };
                            (
                                prim.i32(1).unwrap_or(7),
                                q("precision").unwrap_or(38) as u8,
                                q("scale").unwrap_or(0) as i8,
                            )
                        }
                        // Arrays, maps, structs: delivered as text.
                        None => (7, 0, 0),
                    },
                    _ => (7, 0, 0),
                };
                Ok(Col {
                    name,
                    type_id,
                    precision,
                    scale,
                })
            })
            .collect()
    }

    /// Fetch the next chunk: its row set and whether more rows follow.
    pub fn fetch(&mut self, op: &Struct, max_rows: i64) -> Result<(Vec<V>, bool)> {
        let req = Struct::new()
            .with(1, V::Struct(op.clone()))
            .with(2, V::I32(0))
            .with(3, V::I64(max_rows));
        let resp = self.call("FetchResults", req)?;
        let cols = resp
            .st(3)
            .and_then(|r| r.list(3))
            .map(|l| l.to_vec())
            .unwrap_or_default();
        Ok((cols, resp.bool(2).unwrap_or(false)))
    }

    pub fn close_op(&mut self, op: &Struct) -> Result<()> {
        self.call("CloseOperation", Struct::new().with(1, V::Struct(op.clone())))
            .map(|_| ())
    }

    pub fn close(&mut self) {
        if let Some(sess) = self.session.take() {
            let _ = self.call("CloseSession", Struct::new().with(1, V::Struct(sess)));
        }
    }
}

fn check_status(resp: &Struct) -> Result<()> {
    let Some(st) = resp.st(1) else { return Ok(()) };
    match st.i32(1) {
        Some(3 | 4) => {
            let msg = st.str(5).unwrap_or_else(|| "unknown error".into());
            Err(clean_error(&msg).into())
        }
        _ => Ok(()),
    }
}

/// Server errors often carry a JVM stack trace; keep the useful first part.
fn clean_error(msg: &str) -> String {
    let first: Vec<&str> = msg
        .lines()
        .take_while(|l| !l.trim_start().starts_with("at "))
        .collect();
    first.join("\n").trim().to_string()
}

fn arrow_type(c: &Col) -> DataType {
    match c.type_id {
        0 => DataType::Boolean,
        1 => DataType::Int8,
        2 => DataType::Int16,
        3 => DataType::Int32,
        4 => DataType::Int64,
        5 => DataType::Float32,
        6 => DataType::Float64,
        8 | 22 => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
        9 => DataType::Binary,
        15 if c.precision <= 38 && c.scale >= 0 && c.scale as u8 <= c.precision => {
            DataType::Decimal128(c.precision.max(1), c.scale)
        }
        17 => DataType::Date32,
        _ => DataType::Utf8,
    }
}

fn is_null(nulls: Option<&[u8]>, i: usize) -> bool {
    nulls.is_some_and(|n| n.get(i / 8).is_some_and(|b| b & (1 << (i % 8)) != 0))
}

/// One Thrift `TColumn` into an Arrow array of `ty`.
fn convert(col: &V, ty: &DataType, name: &str) -> Result<ArrayRef> {
    let V::Struct(u) = col else {
        return Err("bad column".into());
    };
    let (_, inner) = u.0.iter().next().ok_or("empty column")?;
    let V::Struct(inner) = inner else {
        return Err("bad column".into());
    };
    let values = inner.list(1).unwrap_or_default();
    let nulls = inner.bin(2);
    let n = values.len();
    macro_rules! prim {
        ($b:ty, $pat:path, $conv:expr) => {{
            let mut b = <$b>::with_capacity(n);
            for (i, v) in values.iter().enumerate() {
                match v {
                    $pat(x) if !is_null(nulls, i) => b.append_value($conv(*x)),
                    _ => b.append_null(),
                }
            }
            Arc::new(b.finish()) as ArrayRef
        }};
    }
    let text = |i: usize| -> Option<String> {
        if is_null(nulls, i) {
            return None;
        }
        match &values[i] {
            V::Bin(b) => Some(String::from_utf8_lossy(b).to_string()),
            V::I64(x) => Some(x.to_string()),
            V::I32(x) => Some(x.to_string()),
            V::Double(x) => Some(x.to_string()),
            V::Bool(x) => Some(x.to_string()),
            _ => None,
        }
    };
    let bad = |i: usize, what: &str| {
        format!(
            "column `{name}`: can't read `{}` as {what}",
            text(i).unwrap_or_default()
        )
    };
    Ok(match ty {
        DataType::Boolean => prim!(BooleanBuilder, V::Bool, |x: bool| x),
        DataType::Int8 => prim!(Int8Builder, V::Byte, |x: i8| x),
        DataType::Int16 => prim!(Int16Builder, V::I16, |x: i16| x),
        DataType::Int32 => prim!(Int32Builder, V::I32, |x: i32| x),
        DataType::Int64 => prim!(Int64Builder, V::I64, |x: i64| x),
        DataType::Float32 => prim!(Float32Builder, V::Double, |x: f64| x as f32),
        DataType::Float64 => prim!(Float64Builder, V::Double, |x: f64| x),
        DataType::Binary => {
            let mut b = BinaryBuilder::new();
            for (i, v) in values.iter().enumerate() {
                match v {
                    V::Bin(x) if !is_null(nulls, i) => b.append_value(x),
                    _ => b.append_null(),
                }
            }
            Arc::new(b.finish())
        }
        DataType::Date32 => {
            let epoch = chrono::NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
            let mut b = Date32Builder::with_capacity(n);
            for i in 0..n {
                match text(i) {
                    Some(t) => {
                        let d = chrono::NaiveDate::parse_from_str(&t, "%Y-%m-%d")
                            .map_err(|_| bad(i, "a date"))?;
                        b.append_value((d - epoch).num_days() as i32);
                    }
                    None => b.append_null(),
                }
            }
            Arc::new(b.finish())
        }
        DataType::Timestamp(..) => {
            let mut b = TimestampMicrosecondBuilder::with_capacity(n).with_timezone("UTC");
            for i in 0..n {
                match text(i) {
                    Some(t) => {
                        let t = t.trim_end_matches('Z').replace('T', " ");
                        let dt = chrono::NaiveDateTime::parse_from_str(&t, "%Y-%m-%d %H:%M:%S%.f")
                            .map_err(|_| bad(i, "a timestamp"))?;
                        b.append_value(dt.and_utc().timestamp_micros());
                    }
                    None => b.append_null(),
                }
            }
            Arc::new(b.finish())
        }
        DataType::Decimal128(p, sc) => {
            let mut b = Decimal128Builder::with_capacity(n).with_precision_and_scale(*p, *sc)?;
            for i in 0..n {
                match text(i) {
                    Some(t) => b.append_value(
                        dre_protocol::util::scaled_decimal(&t, *sc)
                            .map_err(|e| format!("column `{name}`: {e}"))?,
                    ),
                    None => b.append_null(),
                }
            }
            Arc::new(b.finish())
        }
        _ => {
            let mut b = StringBuilder::with_capacity(n, n * 16);
            for i in 0..n {
                b.append_option(text(i));
            }
            Arc::new(b.finish())
        }
    })
}

#[derive(Default)]
pub struct Databricks {
    conn: Option<Hs2>,
}

fn quote(ident: &str) -> String {
    format!("`{}`", ident.replace('`', "``"))
}

impl Databricks {
    fn conn(&mut self) -> Result<&mut Hs2> {
        self.conn.as_mut().ok_or_else(|| "no open session".into())
    }
}

impl Source for Databricks {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            ConnectionField::new("host", "workspace host, e.g. adb-123.4.azuredatabricks.net").required(),
            ConnectionField::new(
                "http_path",
                "the SQL warehouse's HTTP path, e.g. /sql/1.0/warehouses/abc",
            )
            .required(),
            ConnectionField::new("token", "personal access token")
                .required()
                .secret(),
            ConnectionField::new("catalog", "default catalog"),
            ConnectionField::new("schema", "default schema"),
        ]
    }

    fn open(&mut self, c: &Map<String, Value>, _read_only: bool) -> Result<()> {
        let retry = c
            .get("retry_timeout")
            .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
            .unwrap_or(900);
        let mut h = Hs2::new(
            conn_required(c, "host")?,
            conn_required(c, "http_path")?,
            conn_required(c, "token")?,
            Duration::from_secs(retry),
        );
        h.open()?;
        for (key, stmt) in [("catalog", "use catalog"), ("schema", "use schema")] {
            if let Some(v) = conn_str(c, key) {
                let op = h.execute(&format!("{stmt} {}", quote(v)))?;
                h.close_op(&op)?;
            }
        }
        self.conn = Some(h);
        Ok(())
    }

    fn execute(&mut self, sql: &str, row_limit: Option<u64>, out: &mut dyn ResultSink) -> Result<()> {
        let h = self.conn()?;
        let op = h.execute(sql)?;
        if op.bool(3) == Some(false) {
            let n = op.double(4).filter(|n| *n >= 0.0).map(|n| n as u64);
            h.close_op(&op)?;
            return out.no_result(n);
        }
        let cols = h.columns(&op)?;
        let types: Vec<DataType> = cols.iter().map(arrow_type).collect();
        let schema: SchemaRef = Arc::new(Schema::new(
            cols.iter()
                .zip(&types)
                .map(|(c, t)| Field::new(&c.name, t.clone(), true))
                .collect::<Vec<_>>(),
        ));
        out.begin(schema.clone())?;
        let mut fetched = 0u64;
        loop {
            let want = match row_limit {
                Some(l) => (l.saturating_sub(fetched) as i64).min(FETCH_ROWS),
                None => FETCH_ROWS,
            };
            if want == 0 {
                break;
            }
            let (columns, more) = h.fetch(&op, want)?;
            if columns.len() == cols.len() && !columns.is_empty() {
                let arrays: Vec<ArrayRef> = columns
                    .iter()
                    .zip(&types)
                    .zip(&cols)
                    .map(|((c, t), m)| convert(c, t, &m.name))
                    .collect::<Result<_>>()?;
                let batch = RecordBatch::try_new(schema.clone(), arrays)?;
                fetched += batch.num_rows() as u64;
                let rows = batch.num_rows();
                if rows > 0 && !out.batch(batch)? {
                    break;
                }
                if rows == 0 && !more {
                    break;
                }
            }
            if !more {
                break;
            }
        }
        h.close_op(&op)?;
        Ok(())
    }

    fn check(&mut self, sql: &str) -> Result<()> {
        let h = self.conn()?;
        let op = h.execute(&format!("EXPLAIN {sql}"))?;
        let (columns, _) = h.fetch(&op, 1)?;
        h.close_op(&op)?;
        // EXPLAIN reports planning errors inside its plan text rather than failing.
        let plan = columns
            .first()
            .and_then(|c| match c {
                V::Struct(u) => u.0.values().next().cloned(),
                _ => None,
            })
            .and_then(|inner| match inner {
                V::Struct(s) => s.list(1).and_then(|l| l.first().cloned()),
                _ => None,
            })
            .and_then(|v| match v {
                V::Bin(b) => Some(String::from_utf8_lossy(&b).to_string()),
                _ => None,
            })
            .unwrap_or_default();
        for marker in [
            "Error occurred during query planning",
            "AnalysisException",
            "ParseException",
            "[UNRESOLVED",
            "[TABLE_OR_VIEW_NOT_FOUND",
        ] {
            if plan.contains(marker) {
                return Err(clean_error(plan.trim_start_matches("== Physical Plan ==").trim()).into());
            }
        }
        Ok(())
    }

    fn close(&mut self) {
        if let Some(h) = self.conn.as_mut() {
            h.close();
        }
    }
}

pub fn serve() -> ! {
    let about = About::new("databricks", env!("CARGO_PKG_VERSION")).capabilities(&[CAP_SESSIONS, CAP_CHECK]);
    serve_source(about, Databricks::default())
}
