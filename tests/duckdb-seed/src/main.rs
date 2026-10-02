//! Private database seeder for CLI integration tests, never a production dependency.

use anyhow::{Context, Result, bail};
use std::io::Read;
use std::path::{Path, PathBuf};

fn main() -> Result<()> {
    let mut args = std::env::args_os().skip(1);
    let path = PathBuf::from(
        args.next()
            .context("usage: dre-test-duckdb-seed DATABASE < SQL")?,
    );
    if args.next().is_some() {
        bail!("usage: dre-test-duckdb-seed DATABASE < SQL");
    }
    let mut sql = String::new();
    std::io::stdin()
        .read_to_string(&mut sql)
        .context("reading SQL from stdin")?;
    seed(&path, &sql).with_context(|| format!("seeding DuckDB database {}", path.display()))
}

fn seed(path: &Path, sql: &str) -> Result<()> {
    let mut wal = path.as_os_str().to_os_string();
    wal.push(".wal");
    if Path::new(&wal)
        .try_exists()
        .context("checking for an existing database WAL")?
    {
        bail!("database has a WAL; close and checkpoint it before seeding");
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    // A failed SQL batch (even one containing COMMIT) must not publish partial state.
    let staging = tempfile::tempdir_in(parent).context("creating staging directory")?;
    let staged_path = staging.path().join("seed.duckdb");
    match std::fs::copy(path, &staged_path) {
        Ok(_) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !path.exists() => {}
        Err(error) => return Err(error).context("copying existing database for seeding"),
    }
    let connection = duckdb::Connection::open(&staged_path).context("opening staged database")?;
    connection
        .execute_batch(sql)
        .context("executing SQL from stdin")?;
    connection
        .close()
        .map_err(|(_, error)| error)
        .context("closing staged database")?;
    std::fs::rename(&staged_path, path).context("publishing seeded database")?;
    Ok(())
}
