//! Process boundary for database seeding; ordinary CLI tests never link DuckDB.
use anyhow::{Context, Result, bail};
use std::io::{Seek, Write};
use std::path::Path;
use std::process::{Command, Stdio};

pub fn seed(helper: &Path, database: &Path, sql: &str) -> Result<()> {
    seed_inner(helper, database, sql).with_context(|| {
        format!("DuckDB seed helper {helper:?}, database {database:?}, SQL via stdin: {sql:?}")
    })
}

fn seed_inner(helper: &Path, database: &Path, sql: &str) -> Result<()> {
    // File-backed stdin avoids pipe deadlocks and shell quoting for arbitrary SQL.
    let mut input = tempfile::tempfile().context("creating SQL input")?;
    input.write_all(sql.as_bytes()).context("writing SQL input")?;
    input.rewind().context("rewinding SQL input")?;
    let output = Command::new(helper)
        .arg(database)
        .stdin(Stdio::from(input))
        .output()
        .context("invoking DuckDB seed helper")?;
    if !output.status.success() {
        bail!(
            "helper exited with {}\nstderr:\n{}\nstdout:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
            String::from_utf8_lossy(&output.stdout)
        );
    }
    Ok(())
}
