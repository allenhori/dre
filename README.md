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
- **Plugins**: every source, format and destination is a separate executable that speaks DRE's
  [plugin protocol](docs/protocol.md). Plugins are declared per project and installed on demand.
- **Verification**: `dre validate` checks a whole project offline, and `dre validate --live`
  checks every statement against the database. `dre run --dry-run`, `--preview` and
  schema-drift detection check a report before it reaches anyone.

## Quick start

```bash
dre init                 # pick a source, enter its connection, optionally start a project
cd my_reports
dre validate             # check the whole project offline
dre run                  # run every report; output lands in target/run/
dre run monthly --preview 50       # sample 50 rows, never delivered
dre run tag:regulatory --set all   # every regulatory report, for every Set
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

Connections live in `~/.dre/profiles.yml`, outside the project, in dbt's shape (`target` and
`outputs`). Plugins are declared in `plugins.yml` and installed on demand (`dre deps`). Their
exact versions are pinned in `dre.lock`.

## Documentation

- [Plugins and their profile fields](docs/plugins.md)
- [Plugin protocol](docs/protocol.md), for writing a plugin in any language
- [Plugin registry and `dre.lock`](docs/registry.md)

## Building from source

Requires a recent stable Rust toolchain.

```bash
cargo build --release
./target/release/dre --help
```

The first-party plugins are built from the same workspace (`target/release/dre-*`). Put them in
`~/.dre/plugins/` (or `DRE_PLUGINS_DIR`) to use them without a registry.

## License

This project is licensed under the [GNU General Public License v3.0](LICENSE).

A commercial license — for embedding DRE into a product or service you distribute to third parties, without GPL's copyleft obligations — is also available. Contact the maintainer for details.
