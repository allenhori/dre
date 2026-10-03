---
title: "dre_project.yml reference"
description: "Every key of dre_project.yml, the project file."
sidebar:
  order: 22
---

# dre_project.yml reference

<!-- Generated from docs/schemas by .github/scripts/schema_docs.py. Edit the schema, not this page. -->

The project file, `dre_project.yml`, at the project root.

Where: dre_project.yml.

For editor autocomplete and validation, add this as the first line of the file ([editor setup](editor-setup.md)):

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.2/project.schema.json
```

## Keys

| Key | Type | Default | Description |
|---|---|---|---|
| `name` (required) | string |  | The project's name. Required. |
| `default_profile` | string |  | The connection (in `profiles.yml`) reports use when nothing else names one. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `target` | string |  | The run's target (environment) when neither `--target` nor `DRE_TARGET` sets one. Default: `dev`. |
| `default_output` | map, as in [the report reference](reference-report.md) |  | The output every report starts from (the built-in default is `format: csv`). A report's own `output` is merged on top. |
| `format_options` | map |  | Default options per output format, under every output of that format. A report's own keys win. |
| `default_set` | string |  | The Set used when a report has Sets and none is chosen. |
| `vars` | map |  | The lowest level of `var()`: folders, reports, Sets, schedules and `--var` override these. |
| `run_query_max_rows` | integer | `10000` | `run_query()` refuses results larger than this. |
| `lookup_inline_max_rows` | integer | `200` | Lookups over this many rows are loaded into a temp table instead of inlined into the SQL. |
| `dispatch` | list of map (see below) |  | Which macro packages `dispatch()` searches, and in what order. |
| `mask_secrets` | boolean | `true` | Whether `DRE_SECRET_*` values are masked as `*****` in the console, logs, JSON events, `run_results.json` and `target/compiled/`. |
| `timezone` | string |  | The timezone `run.date` and `run.now` use, an IANA name such as `Australia/Sydney`. Default: UTC. |
| `week_start` | `monday` or `sunday` | `monday` | The first day of the week for `run.date` week arithmetic. |
| `week_numbering` | `iso` or `us` | `iso` | How weeks are numbered: ISO 8601 or US style. |
| `reports` | map (see below) |  | Folder config: settings for the report folders, by folder name, nested to match the folders under `reports/`. |
| `target_path` | string |  | Where DRE writes its generated files (compiled SQL, run outputs, the manifest). Default: `target/` in the project. `--target-path` and `DRE_TARGET_PATH` override it. |
| `plugins` | list of plugin packages: a name, `name: "<version>"`, or a map (see below) |  | The plugin packages this project uses. DRE installs them on demand into `dre_deps/` and pins them in `dre.lock`. May be written in any project YAML file; `dependencies.yml` is the usual place. |
| `sources` | map, as in [the sources reference](reference-sources.md) |  | dbt-style source declarations (see the sources schema). May be written in any project YAML file. |

## `dispatch[]`

One macro namespace.

| Key | Type | Default | Description |
|---|---|---|---|
| `macro_namespace` (required) | string |  | The macro package the dispatching macro belongs to, e.g. `dre_utils`. |
| `search_order` (required) | list of string |  | Packages searched for the variant, first match wins: this project's name and installed packages. |

## `reports`

Folder config: settings for the report folders, by folder name, nested to match the folders under `reports/`.

| Key | Type | Default | Description |
|---|---|---|---|
| `+tags` | list of string |  | Tags added to every report in the folder. |
| `+output` | map, as in [the report reference](reference-report.md) |  | Output settings every report in the folder starts from; a report's own keys win. |
| `+profile` | string |  | The connection for reports in the folder. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `+vars` | map |  | Variables, read in SQL and YAML with `var('name')`. Values can be strings, numbers, booleans, lists or maps. |
| `+timezone` | string |  | The timezone `run.date` and `run.now` use, an IANA name such as `Australia/Sydney`. Default: UTC. |

## `plugins[]`

A package with its source.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` (required) | string |  | The package name: lowercase letters, digits and `_`. |
| `version` | string |  | A version constraint such as `1.2.0` or `>=1.0`. Not allowed with `local`. |
| `github` | string |  | Install from the releases of this GitHub repository, `owner/repo`. |
| `local` | string |  | Use the package folder at this path as it is. |
| `registry` | string |  | Install from this registry index (a URL or a path) instead of the default one. |
