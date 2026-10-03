//! DRE source plugin for PostgreSQL.
//!
//! Profile target fields: `host`, `port` (5432), `user`, `password`, `database` (or `dbname`),
//! `sslmode` (`disable`, `prefer` (default), `require`, `verify-ca`, `verify-full`),
//! `sslrootcert`, `connect_timeout` (seconds), `schema` (sets the search path) and `role`.
//!
//! One connection is held for the whole Binding. Read-only sessions set
//! `default_transaction_read_only`. `check` uses `EXPLAIN`, which plans without executing.
//!
//! Types: booleans, integers, floats, text, dates, times, timestamps (with and without time
//! zone), uuid, json/jsonb (as text), bytea and enums map to Arrow directly. `numeric(p,s)`
//! becomes a decimal; unconstrained `numeric` is returned as exact text, so cast it
//! (`amount::numeric(18,2)`) for a typed column. Other types need a cast, e.g. `::text`.

use std::error::Error as StdError;
use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use arrow::array::Array;
use arrow::array::{
    ArrayBuilder, ArrayRef, BinaryBuilder, BooleanBuilder, Date32Builder, Decimal128Builder, Float32Builder,
    Float64Builder, Int16Builder, Int32Builder, Int64Builder, RecordBatch, StringBuilder,
    Time64MicrosecondBuilder, TimestampMicrosecondBuilder,
};
use arrow::datatypes::{DataType, Field, Schema, SchemaRef, TimeUnit};
use arrow::util::display::{ArrayFormatter, FormatOptions};
use chrono::{DateTime, NaiveDate, NaiveDateTime, NaiveTime, Timelike, Utc};
use dre_protocol::msg::ConnectionField;
use dre_protocol::plugin::{About, Loaded, Result, ResultSet, ResultSink, Source, conn_str, serve_source};
use dre_protocol::{CAP_CHECK, CAP_LOAD, CAP_READ_ONLY, CAP_SESSIONS};
use postgres::fallible_iterator::FallibleIterator;
use postgres::types::{FromSql, Kind, Type};
use postgres::{Client, Column, Config, NoTls};
use serde_json::{Map, Value};

const BATCH_ROWS: usize = 8192;

#[derive(Default)]
struct Postgres {
    client: Option<Client>,
}

/// How a Postgres column becomes an Arrow column.
#[derive(Clone, Copy, PartialEq)]
enum Col {
    Bool,
    I16,
    I32,
    I64,
    F32,
    F64,
    Text,
    Date,
    Time,
    Timestamp,
    TimestampTz,
    Uuid,
    Json,
    Bytes,
    /// `numeric(p, s)` with p ≤ 38.
    Decimal(u8, i8),
    /// Unconstrained numeric: exact decimal text.
    NumericText,
    /// Enum labels arrive as text.
    Enum,
}

fn classify(c: &Column) -> Result<Col> {
    let t = c.type_();
    Ok(match *t {
        Type::BOOL => Col::Bool,
        Type::INT2 => Col::I16,
        Type::INT4 => Col::I32,
        Type::INT8 => Col::I64,
        Type::FLOAT4 => Col::F32,
        Type::FLOAT8 => Col::F64,
        Type::TEXT | Type::VARCHAR | Type::BPCHAR | Type::NAME | Type::UNKNOWN => Col::Text,
        Type::DATE => Col::Date,
        Type::TIME => Col::Time,
        Type::TIMESTAMP => Col::Timestamp,
        Type::TIMESTAMPTZ => Col::TimestampTz,
        Type::UUID => Col::Uuid,
        Type::JSON | Type::JSONB => Col::Json,
        Type::BYTEA => Col::Bytes,
        Type::NUMERIC => {
            let m = c.type_modifier();
            if m < 4 {
                Col::NumericText
            } else {
                let m = m - 4;
                // The scale is a signed 16-bit field: Postgres 15+ allows negative scales and
                // scales above the precision, which Arrow decimals can't hold.
                let (p, s) = ((m >> 16) & 0xffff, i32::from((m & 0xffff) as u16 as i16));
                if p <= 38 && (0..=p).contains(&s) {
                    Col::Decimal(p as u8, s as i8)
                } else {
                    Col::NumericText
                }
            }
        }
        _ if matches!(t.kind(), Kind::Enum(_)) => Col::Enum,
        _ => {
            return Err(format!(
                "column `{}` has type `{}`, which DRE can't read yet; cast it in the query, e.g. `{}::text`",
                c.name(),
                t.name(),
                c.name()
            )
            .into());
        }
    })
}

fn arrow_type(c: Col) -> DataType {
    match c {
        Col::Bool => DataType::Boolean,
        Col::I16 => DataType::Int16,
        Col::I32 => DataType::Int32,
        Col::I64 => DataType::Int64,
        Col::F32 => DataType::Float32,
        Col::F64 => DataType::Float64,
        Col::Text | Col::Uuid | Col::Json | Col::NumericText | Col::Enum => DataType::Utf8,
        Col::Date => DataType::Date32,
        Col::Time => DataType::Time64(TimeUnit::Microsecond),
        Col::Timestamp => DataType::Timestamp(TimeUnit::Microsecond, None),
        Col::TimestampTz => DataType::Timestamp(TimeUnit::Microsecond, Some("UTC".into())),
        Col::Bytes => DataType::Binary,
        Col::Decimal(p, s) => DataType::Decimal128(p, s),
    }
}

/// Any value in Postgres' binary format, raw. Used for enums (whose binary form is the label).
struct Raw(Vec<u8>);

impl<'a> FromSql<'a> for Raw {
    fn from_sql(_: &Type, raw: &'a [u8]) -> std::result::Result<Self, Box<dyn StdError + Sync + Send>> {
        Ok(Raw(raw.to_vec()))
    }
    fn accepts(_: &Type) -> bool {
        true
    }
}

/// A `numeric` decoded from Postgres' binary format into exact decimal text.
struct Numeric(String);

impl<'a> FromSql<'a> for Numeric {
    fn from_sql(_: &Type, raw: &'a [u8]) -> std::result::Result<Self, Box<dyn StdError + Sync + Send>> {
        let rd = |i: usize| -> std::result::Result<u16, Box<dyn StdError + Sync + Send>> {
            raw.get(i..i + 2)
                .map(|b| u16::from_be_bytes([b[0], b[1]]))
                .ok_or_else(|| "short numeric".into())
        };
        let ndigits = rd(0)? as usize;
        let weight = rd(2)? as i16 as i32;
        let sign = rd(4)?;
        let dscale = rd(6)? as usize;
        match sign {
            0xC000 => return Ok(Numeric("NaN".into())),
            0xD000 => return Ok(Numeric("Infinity".into())),
            0xF000 => return Ok(Numeric("-Infinity".into())),
            _ => {}
        }
        let digits: Vec<u16> = (0..ndigits)
            .map(|i| rd(8 + 2 * i))
            .collect::<std::result::Result<_, _>>()?;
        // Base-10000 digit i is worth 10000^(weight - i).
        let mut int = String::new();
        for p in 0..=weight.max(0) {
            let d = if p <= weight {
                digits.get(p as usize).copied().unwrap_or(0)
            } else {
                0
            };
            if int.is_empty() {
                if d != 0 || p == weight {
                    int.push_str(&d.to_string());
                }
            } else {
                int.push_str(&format!("{d:04}"));
            }
        }
        if weight < 0 || int.is_empty() {
            int = "0".into();
        }
        let mut frac = String::new();
        let mut idx = weight + 1;
        while frac.len() < dscale {
            let d = if idx >= 0 {
                digits.get(idx as usize).copied().unwrap_or(0)
            } else {
                0
            };
            frac.push_str(&format!("{d:04}"));
            idx += 1;
        }
        frac.truncate(dscale);
        let neg = if sign == 0x4000 { "-" } else { "" };
        Ok(Numeric(if frac.is_empty() {
            format!("{neg}{int}")
        } else {
            format!("{neg}{int}.{frac}")
        }))
    }
    fn accepts(t: &Type) -> bool {
        *t == Type::NUMERIC
    }
}

struct Builders {
    cols: Vec<Col>,
    b: Vec<Box<dyn ArrayBuilder>>,
    schema: SchemaRef,
}

impl Builders {
    fn new(cols: Vec<Col>, schema: SchemaRef) -> Builders {
        let b = cols
            .iter()
            .map(|c| -> Box<dyn ArrayBuilder> {
                match *c {
                    Col::Bool => Box::new(BooleanBuilder::new()),
                    Col::I16 => Box::new(Int16Builder::new()),
                    Col::I32 => Box::new(Int32Builder::new()),
                    Col::I64 => Box::new(Int64Builder::new()),
                    Col::F32 => Box::new(Float32Builder::new()),
                    Col::F64 => Box::new(Float64Builder::new()),
                    Col::Text | Col::Uuid | Col::Json | Col::NumericText | Col::Enum => {
                        Box::new(StringBuilder::new())
                    }
                    Col::Date => Box::new(Date32Builder::new()),
                    Col::Time => Box::new(Time64MicrosecondBuilder::new()),
                    Col::Timestamp => Box::new(TimestampMicrosecondBuilder::new()),
                    Col::TimestampTz => Box::new(TimestampMicrosecondBuilder::new().with_timezone("UTC")),
                    Col::Bytes => Box::new(BinaryBuilder::new()),
                    Col::Decimal(p, s) => {
                        Box::new(Decimal128Builder::new().with_precision_and_scale(p, s).unwrap())
                    }
                }
            })
            .collect();
        Builders { cols, b, schema }
    }

    fn push(&mut self, row: &postgres::Row) -> Result<()> {
        macro_rules! put {
            ($i:expr, $bt:ty, $v:expr) => {
                self.b[$i]
                    .as_any_mut()
                    .downcast_mut::<$bt>()
                    .unwrap()
                    .append_option($v)
            };
        }
        let epoch = NaiveDate::from_ymd_opt(1970, 1, 1).unwrap();
        for i in 0..self.cols.len() {
            match self.cols[i] {
                Col::Bool => put!(i, BooleanBuilder, row.try_get::<_, Option<bool>>(i)?),
                Col::I16 => put!(i, Int16Builder, row.try_get::<_, Option<i16>>(i)?),
                Col::I32 => put!(i, Int32Builder, row.try_get::<_, Option<i32>>(i)?),
                Col::I64 => put!(i, Int64Builder, row.try_get::<_, Option<i64>>(i)?),
                Col::F32 => put!(i, Float32Builder, row.try_get::<_, Option<f32>>(i)?),
                Col::F64 => put!(i, Float64Builder, row.try_get::<_, Option<f64>>(i)?),
                Col::Text => put!(i, StringBuilder, row.try_get::<_, Option<String>>(i)?),
                Col::Uuid => put!(
                    i,
                    StringBuilder,
                    row.try_get::<_, Option<uuid::Uuid>>(i)?.map(|u| u.to_string())
                ),
                Col::Json => put!(
                    i,
                    StringBuilder,
                    row.try_get::<_, Option<Value>>(i)?.map(|v| v.to_string())
                ),
                Col::Enum => put!(
                    i,
                    StringBuilder,
                    row.try_get::<_, Option<Raw>>(i)?
                        .map(|r| String::from_utf8_lossy(&r.0).to_string())
                ),
                Col::NumericText => put!(
                    i,
                    StringBuilder,
                    row.try_get::<_, Option<Numeric>>(i)?.map(|n| n.0)
                ),
                Col::Decimal(_, s) => {
                    let v = match row.try_get::<_, Option<Numeric>>(i)? {
                        Some(n) => Some(dre_protocol::util::scaled_decimal(&n.0, s)?),
                        None => None,
                    };
                    put!(i, Decimal128Builder, v)
                }
                Col::Date => put!(
                    i,
                    Date32Builder,
                    row.try_get::<_, Option<NaiveDate>>(i)?
                        .map(|d| (d - epoch).num_days() as i32)
                ),
                Col::Time => put!(
                    i,
                    Time64MicrosecondBuilder,
                    row.try_get::<_, Option<NaiveTime>>(i)?
                        .map(|t| t.num_seconds_from_midnight() as i64 * 1_000_000
                            + (t.nanosecond() / 1000) as i64)
                ),
                Col::Timestamp => put!(
                    i,
                    TimestampMicrosecondBuilder,
                    row.try_get::<_, Option<NaiveDateTime>>(i)?
                        .map(|t| t.and_utc().timestamp_micros())
                ),
                Col::TimestampTz => put!(
                    i,
                    TimestampMicrosecondBuilder,
                    row.try_get::<_, Option<DateTime<Utc>>>(i)?
                        .map(|t| t.timestamp_micros())
                ),
                Col::Bytes => put!(i, BinaryBuilder, row.try_get::<_, Option<Vec<u8>>>(i)?),
            }
        }
        Ok(())
    }

    fn len(&self) -> usize {
        self.b.first().map_or(0, |b| b.len())
    }

    fn finish(&mut self) -> Result<RecordBatch> {
        let arrays: Vec<ArrayRef> = self.b.iter_mut().map(|b| b.finish()).collect();
        Ok(RecordBatch::try_new(self.schema.clone(), arrays)?)
    }
}

/// `host:port/database`, for connection errors.
fn server(c: &Map<String, Value>) -> String {
    let host = conn_str(c, "host").unwrap_or("localhost");
    let port = match c.get("port") {
        Some(Value::Number(n)) => n.to_string(),
        Some(Value::String(s)) => s.clone(),
        _ => "5432".into(),
    };
    let db = conn_str(c, "database")
        .or_else(|| conn_str(c, "dbname"))
        .unwrap_or_default();
    format!("{host}:{port}/{db}")
}

fn config(c: &Map<String, Value>) -> Result<(Config, Option<native_tls::TlsConnector>)> {
    let mut cfg = Config::new();
    cfg.host(conn_str(c, "host").unwrap_or("localhost"));
    let port = match c.get("port") {
        Some(Value::Number(n)) => n.as_u64().ok_or("`port` must be a number")? as u16,
        Some(Value::String(s)) => s.parse().map_err(|_| format!("invalid `port` `{s}`"))?,
        _ => 5432,
    };
    cfg.port(port);
    if let Some(u) = conn_str(c, "user") {
        cfg.user(u);
    }
    if let Some(p) = conn_str(c, "password") {
        cfg.password(p);
    }
    if let Some(d) = conn_str(c, "database").or_else(|| conn_str(c, "dbname")) {
        cfg.dbname(d);
    }
    if let Some(t) = c
        .get("connect_timeout")
        .and_then(|v| v.as_u64().or_else(|| v.as_str()?.parse().ok()))
    {
        cfg.connect_timeout(Duration::from_secs(t));
    }
    cfg.application_name("dre");
    let mode = conn_str(c, "sslmode").unwrap_or("prefer");
    let mut tls = native_tls::TlsConnector::builder();
    if let Some(root) = conn_str(c, "sslrootcert") {
        let pem = std::fs::read(root).map_err(|e| format!("can't read sslrootcert {root}: {e}"))?;
        tls.add_root_certificate(native_tls::Certificate::from_pem(&pem)?);
    }
    let connector = match mode {
        "disable" => {
            cfg.ssl_mode(postgres::config::SslMode::Disable);
            None
        }
        // libpq semantics: `prefer`/`require` encrypt without verifying the server certificate;
        // `verify-ca` checks the chain; `verify-full` also checks the host name.
        "prefer" | "require" => {
            cfg.ssl_mode(if mode == "prefer" {
                postgres::config::SslMode::Prefer
            } else {
                postgres::config::SslMode::Require
            });
            tls.danger_accept_invalid_certs(true)
                .danger_accept_invalid_hostnames(true);
            Some(tls.build()?)
        }
        "verify-ca" => {
            cfg.ssl_mode(postgres::config::SslMode::Require);
            tls.danger_accept_invalid_hostnames(true);
            Some(tls.build()?)
        }
        "verify-full" => {
            cfg.ssl_mode(postgres::config::SslMode::Require);
            Some(tls.build()?)
        }
        m => {
            return Err(format!(
                "unknown `sslmode` `{m}` (disable, prefer, require, verify-ca, verify-full)"
            )
            .into());
        }
    };
    Ok((cfg, connector))
}

fn quote_ident(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}

impl Postgres {
    fn client(&mut self) -> Result<&mut Client> {
        self.client.as_mut().ok_or_else(|| "no open session".into())
    }
}

impl Source for Postgres {
    fn identifier_quote(&self) -> Option<&'static str> {
        Some("\"")
    }

    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            ConnectionField::new("host", "server host name").default("localhost"),
            ConnectionField::new("port", "server port").default(5432),
            ConnectionField::new("user", "user name").required(),
            ConnectionField::new("password", "password").secret(),
            ConnectionField::new("database", "database name").required(),
            ConnectionField::new("sslmode", "disable, prefer, require, verify-ca or verify-full")
                .default("prefer"),
            ConnectionField::new("schema", "schema to put first on the search path"),
        ]
    }

    fn open(&mut self, c: &Map<String, Value>, read_only: bool) -> Result<()> {
        let (cfg, tls) = config(c)?;
        let mut client = match tls {
            Some(t) => cfg.connect(postgres_native_tls::MakeTlsConnector::new(t)),
            None => cfg.connect(NoTls),
        }
        .map_err(|e| format!("can't connect to Postgres at {}: {}", server(c), describe(&e)))?;
        if let Some(role) = conn_str(c, "role") {
            client.batch_execute(&format!("set role {}", quote_ident(role)))?;
        }
        if let Some(schema) = conn_str(c, "schema") {
            client.batch_execute(&format!("set search_path to {}, public", quote_ident(schema)))?;
        }
        if read_only {
            client.batch_execute("set session characteristics as transaction read only")?;
        }
        self.client = Some(client);
        Ok(())
    }

    /// Bulk load with `COPY ... FROM STDIN` into a temp table.
    fn load(&mut self, name: &str, data: &mut ResultSet<'_>) -> Result<Loaded> {
        if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("`{name}` isn't a valid table name").into());
        }
        let table = format!("dre_lookup_{name}");
        let mut columns = Vec::new();
        for f in data.schema.fields() {
            let t = match f.data_type() {
                DataType::Utf8 => "text",
                DataType::Int64 => "bigint",
                DataType::Float64 => "double precision",
                DataType::Boolean => "boolean",
                DataType::Date32 => "date",
                other => return Err(format!("can't load a column of type {other}").into()),
            };
            columns.push(format!("{} {t}", quote_ident(f.name())));
        }
        let client = self.client()?;
        client
            .batch_execute(&format!(
                "drop table if exists pg_temp.{table}; create temp table {table} ({})",
                columns.join(", ")
            ))
            .map_err(|e| describe(&e))?;
        let mut w = client
            .copy_in(&format!("copy {table} from stdin (format csv)"))
            .map_err(|e| describe(&e))?;
        let opts = FormatOptions::default();
        let mut line = String::new();
        while let Some(batch) = data.next_batch()? {
            let cols: Vec<ArrayFormatter<'_>> = batch
                .columns()
                .iter()
                .map(|c| ArrayFormatter::try_new(c.as_ref(), &opts))
                .collect::<std::result::Result<_, _>>()?;
            for row in 0..batch.num_rows() {
                line.clear();
                for (i, (c, f)) in batch.columns().iter().zip(&cols).enumerate() {
                    if i > 0 {
                        line.push(',');
                    }
                    // Unquoted empty is NULL in CSV mode; every value is quoted.
                    if !c.is_null(row) {
                        line.push('"');
                        line.push_str(&f.value(row).to_string().replace('"', "\"\""));
                        line.push('"');
                    }
                }
                line.push('\n');
                w.write_all(line.as_bytes())?;
            }
        }
        let rows = w.finish().map_err(|e| describe(&e))?;
        Ok(Loaded {
            relation: table,
            rows,
            warning: None,
        })
    }

    fn execute(&mut self, sql: &str, row_limit: Option<u64>, out: &mut dyn ResultSink) -> Result<()> {
        let client = self.client()?;
        let stmt = client.prepare(sql).map_err(|e| describe(&e))?;
        if stmt.columns().is_empty() {
            let n = client.execute(&stmt, &[]).map_err(|e| describe(&e))?;
            return out.no_result(Some(n));
        }
        let cols: Vec<Col> = stmt.columns().iter().map(classify).collect::<Result<_>>()?;
        let schema: SchemaRef = Arc::new(Schema::new(
            stmt.columns()
                .iter()
                .zip(&cols)
                .map(|(c, k)| Field::new(c.name(), arrow_type(*k), true))
                .collect::<Vec<_>>(),
        ));
        out.begin(schema.clone())?;
        let mut b = Builders::new(cols, schema);
        match row_limit {
            // A portal fetches only the rows a preview needs.
            Some(limit) => {
                let mut tx = client.transaction()?;
                let portal = tx.bind(&stmt, &[]).map_err(|e| describe(&e))?;
                let rows = tx
                    .query_portal(&portal, limit.min(i32::MAX as u64) as i32)
                    .map_err(|e| describe(&e))?;
                for r in &rows {
                    b.push(r)?;
                    if b.len() == BATCH_ROWS {
                        out.batch(b.finish()?)?;
                    }
                }
                if b.len() > 0 {
                    out.batch(b.finish()?)?;
                }
                tx.commit()?;
            }
            None => {
                let mut it = client
                    .query_raw(&stmt, std::iter::empty::<i32>())
                    .map_err(|e| describe(&e))?;
                while let Some(r) = it.next().map_err(|e| describe(&e))? {
                    b.push(&r)?;
                    if b.len() == BATCH_ROWS && !out.batch(b.finish()?)? {
                        return Ok(());
                    }
                }
                if b.len() > 0 {
                    out.batch(b.finish()?)?;
                }
            }
        }
        Ok(())
    }

    fn check(&mut self, sql: &str) -> Result<()> {
        self.client()?
            .batch_execute(&format!("explain {sql}"))
            .map_err(|e| describe(&e).into())
    }
}

/// The server's own message (with its detail/hint) rather than the driver's wrapper.
fn describe(e: &postgres::Error) -> String {
    match e.as_db_error() {
        Some(db) => {
            let mut s = format!("{}: {}", db.severity(), db.message());
            if let Some(d) = db.detail() {
                s.push_str(&format!(" ({d})"));
            }
            if let Some(h) = db.hint() {
                s.push_str(&format!("; hint: {h}"));
            }
            s
        }
        None => {
            let mut s = e.to_string();
            let mut src = e.source();
            while let Some(x) = src {
                s.push_str(&format!(": {x}"));
                src = x.source();
            }
            s
        }
    }
}

fn main() {
    let about = About::new("postgres", env!("CARGO_PKG_VERSION")).capabilities(&[
        CAP_SESSIONS,
        CAP_READ_ONLY,
        CAP_CHECK,
        CAP_LOAD,
    ]);
    serve_source(about, Postgres::default())
}
