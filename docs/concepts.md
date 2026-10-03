---
title: "Concepts"
description: "Reports, Sets, Bindings, templates, lookups, plugins, delivery, logs, verification and selecting."
sidebar:
  order: 3
---

# Concepts

- **Report**: one or more Jinja-templated SQL queries plus an output config, declared in YAML.
  A `.sql` file under `reports/` with no YAML is an *unmanaged* report, meant for quick tests.
- **Set** and **Binding**: one report can run as many named variants (clients, regions,
  departments). A Binding is a report paired with a Set, with its own profile, variables, query
  subset and output.
- **Connections, destinations, sources and the target**: a *connection* is what queries read
  from and a *destination* where output goes, both in `profiles.yml`. A *source* is a declared
  table (dbt's `sources:`), read with `{{ source('sales', 'orders') }}`. The *target* is the
  environment (`dev`, `prod`), one per run. Each query runs on its own connection when it names
  one (`profile:` on the query, or a source's), else on the report's, so one workbook can hold
  DuckDB, Postgres and Databricks tabs. See [connections](connections.md) and
  [sources](sources.md).
- **Jinja everywhere**: SQL, paths and options render with `var()`, `env_var()`, `run.*` and your
  macros in `macros/`. `connection.*` (the query's connection) and `profile('name')` read
  connection settings, so names can follow the environment:
  `{{ connection.catalog }}.{{ connection.schema }}.orders`. `run.date` is a date
  you can navigate (`run.date.prev_month.start.date`), in the run's timezone (UTC unless you set
  one). `run_query()` and `columns()` let a macro query the query's own connection while
  rendering. `ref('file')` reuses another `.sql` file as a subquery. See
  [templates](templates.md).
- **Lookups**: mapping tables you maintain as files in `lookups/` (csv, xlsx, xls, json, jsonl,
  yml) rather than in the database. `ref('countries')` makes one usable like a table: small ones
  are inlined into the SQL, larger ones (over 200 rows by default) are loaded into a temp table
  by the source plugin. `lookup('countries')` hands the rows to Jinja. Values are text unless a
  `lookups/<name>.yml` config gives columns types; see [lookups](lookups.md).
- **Plugins**: every source, format and destination is a plugin that speaks DRE's
  [plugin protocol](protocol.md). Plugins ship in packages, one per system: `databricks`
  is the Databricks source and destination, `object_store` is S3, GCS and Azure Blob. A project
  declares each package once and DRE installs it on demand.
- **Delivery**: one output can go to several destinations in a single run, e.g. object storage
  (S3, GCS, Azure Blob), SFTP/FTP, Databricks Volumes or workspace files, an email with the file
  attached, or a Slack channel. See [plugins](plugins.md).
- **Logs**: every run appends to `logs/dre.log` in the project, including the full SQL of each
  statement sent to the database (report queries, `run_query()`, lookup loads). The file rotates
  every 10,000 lines, keeping `dre.log.1` to `dre.log.5`.
- **Verification**: `dre validate` checks the project and compiles its SQL; with `-s` it also shows,
  per selected Binding, the compiled files, the target, each query's connection, the output file and every
  destination (non-dev targets stand out). `dre compile` just renders the SQL into
  `target/compiled/` and lists the files. `dre validate --live` checks every statement against
  the database. `--preview` and schema-drift detection check a report before it reaches anyone.
- **Generated files**: everything DRE writes goes in the *target path*, `target/` in the project
  by default: compiled SQL, each run's output files, schema snapshots, `run_results.json` and the
  [manifest](manifest.md). `--target-path`, `DRE_TARGET_PATH` or `target_path:` in
  `dre_project.yml` move it (see [below](target-path.md)).
- **Selecting**: `run`, `compile`, `validate` and `ls` take report names, `tag:<tag>`,
  `source:<source>` or `source:<source>.<table>`, folder names or dotted folder paths, as arguments (`dre run daily monthly`) or with `-s`/`--select`. Several
  match any of them: `-s daily monthly`, `-s daily,monthly`, or repeated `-s` (a semicolon works
  too, quoted: `-s "daily;monthly"`).
