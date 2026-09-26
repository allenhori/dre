//! DRE source plugin for DuckDB.
//!
//! Profile target fields: `path` (database file; default `:memory:`, relative to the project
//! directory), `threads`, `memory_limit`. One connection is held for the whole Binding, so temp
//! tables and settings persist across statements.

use std::sync::Arc;

use arrow::datatypes::{DataType, Schema};
use dre_protocol::msg::ConnectionField;
use dre_protocol::plugin::{About, Loaded, Result, ResultSet, ResultSink, Source, conn_str, serve_source};
use dre_protocol::{CAP_CHECK, CAP_LOAD, CAP_READ_ONLY, CAP_SESSIONS};
use duckdb::{AccessMode, Config, Connection};
use serde_json::{Map, Value};

#[derive(Default)]
struct DuckDb {
    conn: Option<Connection>,
}

/// Leading keywords of statements that produce a result set even when it looks like a row count.
const QUERY_KEYWORDS: &[&str] = &[
    "select",
    "with",
    "from",
    "values",
    "table",
    "show",
    "describe",
    "summarize",
    "explain",
    "pragma",
    "call",
    "(",
];

/// Lookup names come from core already checked; refuse anything that isn't a plain identifier.
fn checked_name(name: &str) -> Result<&str> {
    if !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        Ok(name)
    } else {
        Err(format!("`{name}` isn't a valid table name").into())
    }
}

fn leading_keyword(sql: &str) -> String {
    let mut s = sql.trim_start();
    loop {
        if let Some(r) = s.strip_prefix("--") {
            s = r.find('\n').map_or("", |i| &r[i..]).trim_start();
        } else if let Some(r) = s.strip_prefix("/*") {
            s = r.find("*/").map_or("", |i| &r[i + 2..]).trim_start();
        } else {
            break;
        }
    }
    if s.starts_with('(') {
        return "(".into();
    }
    s.split(|c: char| !c.is_ascii_alphabetic())
        .next()
        .unwrap_or("")
        .to_ascii_lowercase()
}

/// What DuckDB returns for statements without a result set: DDL, DML, CTAS and COPY answer with
/// one `Count` column; SET, DROP, BEGIN, ATTACH and the like with one `Success` column.
#[derive(PartialEq)]
enum Status {
    Count,
    Success,
}

fn status_only(schema: &Schema, sql: &str) -> Option<Status> {
    if schema.fields().len() != 1 || QUERY_KEYWORDS.contains(&leading_keyword(sql).as_str()) {
        return None;
    }
    match (schema.field(0).name().as_str(), schema.field(0).data_type()) {
        ("Count", DataType::Int64) => Some(Status::Count),
        ("Success", DataType::Boolean) => Some(Status::Success),
        _ => None,
    }
}

impl DuckDb {
    fn conn(&self) -> Result<&Connection> {
        self.conn.as_ref().ok_or_else(|| "no open session".into())
    }
}

impl Source for DuckDb {
    fn connection_fields(&self) -> Vec<ConnectionField> {
        vec![
            ConnectionField::new(
                "path",
                "DuckDB database file (relative to the project), or :memory:",
            )
            .default(":memory:"),
            ConnectionField::new("threads", "number of threads DuckDB may use"),
            ConnectionField::new("memory_limit", "DuckDB memory limit, e.g. 4GB"),
        ]
    }

    fn open(&mut self, c: &Map<String, Value>, read_only: bool) -> Result<()> {
        let path = conn_str(c, "path").unwrap_or(":memory:");
        let mut config = Config::default();
        if read_only && path != ":memory:" {
            config = config.access_mode(AccessMode::ReadOnly)?;
        }
        if let Some(t) = c
            .get("threads")
            .and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()))
        {
            config = config.threads(t)?;
        }
        if let Some(m) = conn_str(c, "memory_limit") {
            config = config.max_memory(m)?;
        }
        let conn = Connection::open_with_flags(path, config)
            .map_err(|e| format!("can't open DuckDB database `{path}`: {e}"))?;
        self.conn = Some(conn);
        Ok(())
    }

    /// Bulk load through DuckDB's appender into a temp table.
    fn load(&mut self, name: &str, data: &mut ResultSet<'_>) -> Result<Loaded> {
        let table = format!("dre_lookup_{}", checked_name(name)?);
        let columns = data
            .schema
            .fields()
            .iter()
            .map(|f| {
                let t = match f.data_type() {
                    DataType::Utf8 => "VARCHAR",
                    DataType::Int64 => "BIGINT",
                    DataType::Float64 => "DOUBLE",
                    DataType::Boolean => "BOOLEAN",
                    DataType::Date32 => "DATE",
                    other => return Err(format!("can't load a column of type {other}")),
                };
                Ok(format!("\"{}\" {t}", f.name()))
            })
            .collect::<std::result::Result<Vec<_>, String>>()?;
        let conn = self.conn()?;
        conn.execute_batch(&format!(
            "create or replace temp table {table} ({})",
            columns.join(", ")
        ))?;
        let mut appender = conn.appender(&table)?;
        let mut rows = 0;
        while let Some(batch) = data.next_batch()? {
            rows += batch.num_rows() as u64;
            appender.append_record_batch(batch)?;
        }
        appender.flush()?;
        Ok(Loaded {
            relation: table,
            rows,
            warning: None,
        })
    }

    fn execute(&mut self, sql: &str, _row_limit: Option<u64>, out: &mut dyn ResultSink) -> Result<()> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(sql)?;
        let stream = stmt.stream_arrow([])?;
        let schema = stream.get_schema();
        if schema.fields().is_empty() {
            return out.no_result(None);
        }
        let mut stream = stream;
        match status_only(&schema, sql) {
            Some(Status::Count) => {
                let n = next_batch(&mut stream)?
                    .and_then(|b| {
                        b.column(0)
                            .as_any()
                            .downcast_ref::<arrow::array::Int64Array>()
                            .map(|a| a.value(0))
                    })
                    .map(|n| n as u64);
                return out.no_result(n);
            }
            Some(Status::Success) => {
                while next_batch(&mut stream)?.is_some() {}
                return out.no_result(None);
            }
            None => {}
        }
        out.begin(Arc::new(Schema::new(schema.fields().clone())))?;
        while let Some(b) = next_batch(&mut stream)? {
            if !out.batch(b)? {
                break;
            }
        }
        Ok(())
    }

    fn check(&mut self, sql: &str) -> Result<()> {
        let conn = self.conn()?;
        let mut stmt = conn.prepare(&format!("EXPLAIN {sql}"))?;
        stmt.execute([])?;
        Ok(())
    }
}

/// duckdb-rs panics if fetching fails mid-stream; turn that into an error.
fn next_batch(stream: &mut duckdb::ArrowStream<'_>) -> Result<Option<arrow::array::RecordBatch>> {
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| stream.next())).map_err(|p| {
        let msg = p
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()));
        format!(
            "DuckDB failed while fetching results: {}",
            msg.unwrap_or_default()
        )
        .into()
    })
}

fn main() {
    let about = About::new("duckdb", env!("CARGO_PKG_VERSION")).capabilities(&[
        CAP_SESSIONS,
        CAP_READ_ONLY,
        CAP_CHECK,
        CAP_LOAD,
    ]);
    serve_source(about, DuckDb::default())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_the_leading_keyword_past_comments() {
        assert_eq!(leading_keyword("  -- c\n /* x */ Insert into t"), "insert");
        assert_eq!(leading_keyword("(select 1)"), "(");
    }
}
