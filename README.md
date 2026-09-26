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
- **Verification**: `dre validate` checks a whole project offline. `dre run --dry-run`,
  `--preview` and schema-drift detection check a report before it reaches anyone.

## Building from source

Requires a recent stable Rust toolchain.

```bash
cargo build --release
./target/release/dre --help
```

## License

This project is licensed under the [GNU General Public License v3.0](LICENSE).

A commercial license — for embedding DRE into a product or service you distribute to third parties, without GPL's copyleft obligations — is also available. Contact the maintainer for details.
