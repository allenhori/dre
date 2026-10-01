<p align="center">
  <picture>
    <source media="(prefers-color-scheme: dark)" srcset="docs/assets/logo-dark.svg">
    <img src="docs/assets/logo.svg" alt="DRE logo" width="360">
  </picture>
</p>

# DRE

**DRE** stands for **Declarative Reporting Engine**.

DRE is SQL (plus a template) in, a correctly formatted file out. You declare reports as YAML and
`.sql` files in a dbt-shaped project. DRE runs the SQL against your warehouse, writes the result as
csv, delimited, fixed-width, parquet or xlsx (including multi-sheet workbooks, number formats,
formulas and totals rows, and branded Excel templates), and delivers the file wherever it needs
to go. It runs on whatever scheduler you already have: cron, Airflow, Dagster, Databricks Jobs.

Status: under active development. From 0.1.0 on, a patch release never breaks a project.

## Install

```bash
curl -fsSL https://github.com/get-dre/dre/releases/latest/download/install.sh | sh   # macOS and Linux
brew install get-dre/tap/dre                                                          # Homebrew
pip install dre-cli                                                                   # pip, uv, pipx
```

More ways, and Windows, in [Install](docs/install.md). Or let a coding agent do it: DRE's
[agent skills](skills/README.md) guide you from installing DRE to a delivered report.

```bash
claude plugin marketplace add get-dre/dre && claude plugin install dre@dre   # Claude Code
npx skills add get-dre/dre#skills-latest                                     # other agents
```

Then ask: "help me with dre".

## Quick start

```bash
dre init               # pick a source, enter its connection, start a project
cd my_reports
dre validate           # check the project and compile its SQL
dre run                # run every report; output lands in target/run/
```

## Documentation

- [Getting started](docs/getting-started.md) and [Install](docs/install.md)
- [Concepts](docs/concepts.md), [Build and run reports](docs/building-reports.md), [Schedules](docs/schedules.md)
- [Templates](docs/templates.md) and [Lookups](docs/lookups.md)
- [Plugins](docs/plugins.md), [Managing plugins](docs/managing-plugins.md), [the registry and `dre.lock`](docs/registry.md), [the plugin protocol](docs/protocol.md)
- [The target path](docs/target-path.md), [the manifest and `run_results.json`](docs/manifest.md)
- [Updating DRE](docs/updating.md), [Environment variables](docs/environment-variables.md), [Building from source](docs/building-from-source.md)
- [Practices: how to set up, write and run reports well](docs/practices.md)
- [Agent skills: DRE in your coding agent](skills/README.md)

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). Pull requests need a signed [CLA](CLA.md) and a
maintainer's approval.

## License

This project is licensed under the [GNU General Public License v3.0](LICENSE).

A commercial license — for embedding DRE into a product or service you distribute to third parties, without GPL's copyleft obligations — is also available. Contact the maintainer for details.
