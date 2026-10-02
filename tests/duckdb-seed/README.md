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

## Issue 74 clean-build measurements

Measured on the same Linux cloud runner with Rust/Cargo 1.99.0, four build jobs,
and `cargo test --workspace --no-run --locked`. Each disposable target directory
started empty; runs were sequential. Peak target disk was sampled with `du` every
two seconds. The final row includes the explicitly documented package-level native
debug override; other development/test settings were unchanged. Cargo registry
downloads were cached after the initial baseline, so wall times are descriptive,
not a claim of statistically comparable CI improvement.

| Implementation | Wall time | Sampled peak target | Final target | debug/deps | Outcome |
| --- | ---: | ---: | ---: | ---: | --- |
| Upstream, unmodified | 8m06s | 28.699 GiB | 28.699 GiB | 23.652 GiB | FAIL, disk exhausted while linking |
| Helper only | 7m46s | 19.293 GiB | 19.293 GiB | 14.183 GiB | PASS |
| Helper plus native debug override | 6m22s | 14.025 GiB | 14.025 GiB | 11.872 GiB | PASS |

The final build uses 51.1% less disk than the freshly measured upstream baseline.
It uses 48.1% less than the issue's historical 27 GiB figure, missing the historical
13.5 GiB absolute threshold by 0.525 GiB. The baseline exhausted the 32 GB runner,
so its elapsed time is time to failure rather than a successful build duration.
No 25% Ubuntu timing improvement across three uncached runs is established by
these results; that requires comparable successful baseline and PR CI runs.

| Executable | Upstream baseline | Final implementation | DuckDB engine/FFI symbols after |
| --- | ---: | ---: | --- |
| run_xlsx_formats | 728.181 MiB | 69.958 MiB | absent |
| output | 687.155 MiB | 28.360 MiB | absent |
| templates_profiles | not produced before baseline failure | 26.713 MiB | absent |
| dre-test-duckdb-seed | not present | 234.018 MiB | present, intentionally |

`cargo tree -p dre-cli --edges normal,dev --locked` contains no `duckdb` or
`libduckdb-sys`. The production-only CLI dependency graph is identical before and
after. `nm -C` inspected all 29 CLI integration-test executables: none contain
`libduckdb_sys` or `duckdb_open` symbols. The private helper contains both.
