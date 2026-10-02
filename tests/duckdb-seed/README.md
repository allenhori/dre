# Private DuckDB test seeder

`dre-test-duckdb-seed` is an unpublished workspace binary used only by CLI integration
tests. No production package depends on it. Default workspace members and explicit
release builds continue to select the production packages.

The helper takes one native database-path argument and reads UTF-8 SQL from stdin.
`TestProject::duckdb(path, sql)` resolves the helper once per test executable and
supplies file-backed stdin, avoiding shell quoting and pipe deadlocks. Failures
include the executable, database path, SQL, exit status, and captured stderr.

Each invocation seeds a database in a unique temporary sibling directory. An existing
closed database is copied first. The connection closes before the staged database is
renamed to the requested path. Failed SQL, including a committed statement followed
by an error, cannot publish partial state. A database with a WAL is rejected: close
and checkpoint it first. Database access during seeding must be exclusive, as it is
for each isolated `TestProject` directory. External side effects of SQL are outside
this database-publication guarantee.

Run the process-level helper tests with:

```sh
cargo test -p dre-test-duckdb-seed --locked
```

The normal CLI test path uses the existing local `plugin-builds` target directory.
For workspace verification without duplicated local plugin builds, use CI's existing
prebuilt-binary path:

```sh
cargo build --workspace --bins --locked
DRE_TEST_PREBUILT_BINS=1 cargo test --workspace --locked
DRE_TEST_PREBUILT_BINS=1 cargo test -p dre-cli --test duckdb_seed --locked
```

The helper's own integration tests cause Cargo to build its binary for
`cargo test --workspace --no-run`. Its binary test harness is disabled, so test
coverage does not introduce a second DuckDB-linked helper harness. Ordinary CLI
integration tests have no DuckDB dependency.

## Development/test native build

The `libduckdb-sys` package override disables its debug information, including the
bundled C++ symbols, to keep clean workspace builds within constrained disk budgets.
Its upstream build script also defines `NDEBUG` when Cargo disables debug info, so
native DuckDB debug assertions use release behavior in development/tests. Other
Rust packages retain their debug information and assertions; no integration-test
assertions or cases are removed. The production release profile is unchanged.
