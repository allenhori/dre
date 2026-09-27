<p align="center">
  <img src="docs/assets/logo.png" alt="DRE logo" width="360">
</p>

# DRE

**DRE** stands for **Declarative Reporting Engine**.

DRE is SQL (plus a template) in, a correctly formatted file out. You declare reports as YAML and
`.sql` files in a dbt-shaped project. DRE runs the SQL against your warehouse, writes the result as
csv, delimited, fixed-width, parquet or xlsx (including multi-sheet workbooks and branded Excel
templates), and delivers the file wherever it needs to go. It runs on whatever scheduler you
already have: cron, Airflow, Dagster, Databricks Jobs.

Status: under active development. There is no released version yet.

## Concepts

- **Report**: one or more Jinja-templated SQL queries plus an output config, declared in YAML.
  A `.sql` file under `reports/` with no YAML is an *unmanaged* report, meant for quick tests.
- **Set** and **Binding**: one report can run as many named variants (clients, regions,
  departments). A Binding is a report paired with a Set, with its own profile, variables, query
  subset and output.
- **Jinja everywhere**: SQL, paths and options render with `var()`, `env_var()`, `run.*` and your
  macros in `macros/`. `run_query()` lets a macro query the report's own connection while
  rendering (list a table's columns, build a pivot from the distinct values). `ref('file')` reuses
  another `.sql` file as a subquery.
- **Lookups**: mapping tables you maintain as files in `lookups/` (csv, xlsx, xls, json, jsonl,
  yml) rather than in the database. `ref('countries')` makes one usable like a table: small ones
  are inlined into the SQL, larger ones (over 200 rows by default) are loaded into a temp table
  by the source plugin. `lookup('countries')` hands the rows to Jinja.
- **Plugins**: every source, format and destination is a separate executable that speaks DRE's
  [plugin protocol](docs/protocol.md). Plugins are declared per project and installed on demand.
- **Delivery**: one output can go to several destinations in a single run, e.g. object storage
  (S3, GCS, Azure Blob), SFTP/FTP, Databricks Volumes, an email with the file attached, or a
  Slack channel. See [plugins](docs/plugins.md).
- **Logs**: every run appends to `logs/dre.log` in the project, including the full SQL of each
  statement sent to the database (report queries, `run_query()`, lookup loads). The file rotates
  every 10,000 lines, keeping `dre.log.1` to `dre.log.5`.
- **Verification**: `dre validate` checks the project and compiles its SQL; with `-s` it also shows,
  per selected Binding, the compiled files, the source and target, the output file and every
  destination (non-dev targets stand out). `dre compile` just renders the SQL into
  `target/compiled/` and lists the files. `dre validate --live` checks every statement against
  the database. `--preview` and schema-drift detection check a report before it reaches anyone.
- **Selecting**: `run`, `compile` and `validate` take `-s`/`--select` with report names,
  `tag:<tag>`, folder names or dotted folder paths. Several match any of them: `-s daily monthly`,
  `-s daily,monthly`, or repeated `-s` (a semicolon works too, quoted: `-s "daily;monthly"`).

## Quick start

```bash
dre init                 # pick a source, enter its connection, optionally start a project
cd my_reports
dre validate             # check the project and compile its SQL
dre validate -s monthly  # ...and show where monthly's output would go
dre compile -s daily,monthly       # render the SQL into target/compiled/ and list the files
dre run                  # run every report; output lands in target/run/
dre run monthly --preview 50       # sample 50 rows, never delivered
dre run -s tag:regulatory --set all  # every regulatory report, for every Set
dre validate --live      # check every statement against the database without running it
```

A report is a YAML file next to its `.sql` files:

```yaml
# reports/finance/monthly/monthly.yml
queries:
  - setup_temp_accounts            # CREATE TEMP TABLE: runs first, produces no sheet
  - {query: summary, tab_name: Summary}
  - detail
output:
  format: xlsx
  destination:
    profile: reports_s3
    path: "s3://reports/{{ var('client') }}/monthly-{{ run.date.yyyymmdd }}.xlsx"
sets: [client_a, client_b]
default_set: client_a
```

Connections live in `~/.dre/profiles.yml`, outside the project. Database connections go under
`sources:` and delivery targets under `destinations:`. Each profile picks a default `target`
(environment) from its named `targets`:

```yaml
sources:
  warehouse:
    target: dev
    targets:
      dev: {type: duckdb, path: dev.duckdb}
      prod: {type: postgres, host: db.internal, user: reports, password: "{{ env_var('PG_PASSWORD') }}"}
destinations:
  reports_s3:
    target: prod
    targets:
      prod: {type: s3, bucket: reports}
```

`dre init` writes this file for you.

Plugins and macro packages are declared in `dependencies.yml` (or `packages.yml`, or both) and
installed into the project's `dre_deps/` folder by `dre deps`. Their exact versions and commits
are pinned in `dre.lock`. Package macros are called through the package's name
(`{{ dre_utils.star_except(...) }}`), and `dispatch()` lets a package offer per-database variants
that a project can override. See [the registry docs](docs/registry.md).

## Schedules

DRE doesn't fire schedules itself; your orchestrator does. `schedules.yml` names them, and one
report can have several, each with its own vars:

```yaml
- name: flash_daily
  report: sales_summary
  set: client_a
  cron: "0 7 * * *"
  vars: {period: day}
- name: close_monthly
  report: sales_summary
  set: client_a
  cron: "0 6 1 * *"
  vars: {period: month}
```

```bash
DRE_RUN_DATE=2026-09-01 dre run --schedule close_monthly
```

`--schedule` runs exactly the Bindings that schedule targets. Its `vars` sit above the report's and
below `--var`, and `run.schedule` renders as its name, so SQL can say
`{% if var('period') == 'day' %}...`. Pass the scheduled date through `DRE_RUN_DATE` so reruns
render the same. `run_results.json`, the JSON events and `logs/dre.log` record the schedule, its
vars, every var the run used and the command's parameters.

## Environment variables

| Variable | Effect |
|---|---|
| `DRE_PROFILES_DIR` | Directory holding `profiles.yml` (default `~/.dre`). `--profiles-dir` overrides it. |
| `DRE_PLUGINS_DIR` | One plugins directory for every project, instead of each project's `dre_deps/plugins`. |
| `DRE_REGISTRY_URL` | The plugin registry index (a URL or a local path). |
| `DRE_RUN_DATE` | The run date (`YYYY-MM-DD`) behind `run.date`, instead of today. |
| `DRE_LOG_MAX_LINES` | Lines per `logs/dre.log` before it rotates (default 10,000). |
| `DRE_PLUGIN_HANDSHAKE_TIMEOUT_MS` | How long to wait for a plugin to start (default 30,000). |
| `NO_COLOR` | Turns off coloured output. |
| `DRE_SECRET_*` | Values are masked as `*****` in the console, logs, JSON events, `run_results.json` and `target/compiled/` (`mask_secrets: false` in `dre_project.yml` turns this off). |

## Documentation

- [Plugins and their profile fields](docs/plugins.md)
- [Plugin protocol](docs/protocol.md), for writing a plugin in any language
- [Plugin registry and `dre.lock`](docs/registry.md)

## Building from source

Requires a recent stable Rust toolchain, and Go for the Databricks adapter.

```bash
cargo build --release
./target/release/dre --help
```

The first-party plugins are built from the same workspace (`target/release/dre-*`), except the
Databricks adapter in `go/databricks`, which is one Go program installed under both of its plugin
names:

```bash
cd go/databricks
go build -o ../../target/release/dre-source-databricks .
cp ../../target/release/dre-source-databricks ../../target/release/dre-destination-databricks_volumes
```

Put them in a project's `dre_deps/plugins/` (or point `DRE_PLUGINS_DIR` at them) to use them
without a registry. `scripts/local-registry.sh` builds everything, both languages included.

## License

This project is licensed under the [GNU General Public License v3.0](LICENSE).

A commercial license — for embedding DRE into a product or service you distribute to third parties, without GPL's copyleft obligations — is also available. Contact the maintainer for details.
