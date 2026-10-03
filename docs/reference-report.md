---
title: "Report YAML reference"
description: "Every key of a report YAML file: queries, output, destinations, Sets and templates."
sidebar:
  order: 23
---

# Report YAML reference

<!-- Generated from docs/schemas by .github/scripts/schema_docs.py. Edit the schema, not this page. -->

A report: SQL queries plus an output, in a YAML file under `reports/`.

Where: a `.yml` file under `reports/`.

For editor autocomplete and validation, add this as the first line of the file ([editor setup](editor-setup.md)):

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.1/report.schema.json
```

## Keys

| Key | Type | Default | Description |
|---|---|---|---|
| `name` | string |  | The report's name. Default: the name of the folder the YAML file is in. Report names are unique across the project. |
| `tags` | list of string |  | Tags to select the report with, `-s tag:<tag>`. |
| `queries` | list of string or map (see below) |  | The `.sql` files whose results make the report's tabs, in the order they run, on one database session. |
| `output` | map (see below) |  | How a report's result is written and where it goes. Besides the keys below, each format takes its own options (for example `delimiter` for `delimited`, `columns` for `fixed_width`); they are documented with the plugins. |
| `profile` | string |  | The connection (in `profiles.yml`) the queries run on, unless a query's own `profile:` or a source's says otherwise. Default: the folder's `+profile`, then `default_profile`. Can't be combined with `sets`. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `sets` | list of string or map (see below) |  | The Sets the report can run as, by name (declared in `sets.yml`) or declared here. |
| `default_set` | string |  | The Set a plain `dre run` uses. Must be one of `sets`. |
| `vars` | map |  | Variables, read in SQL and YAML with `var('name')`. Values can be strings, numbers, booleans, lists or maps. |
| `timezone` | string |  | The timezone `run.date` and `run.now` use, an IANA name such as `Australia/Sydney`. Default: UTC. |
| `plugins` | list of plugin packages: a name, `name: "<version>"`, or a map (see below) |  | The plugin packages this project uses. DRE installs them on demand into `dre_deps/` and pins them in `dre.lock`. May be written in any project YAML file; `dependencies.yml` is the usual place. |
| `sources` | map, as in [the sources reference](reference-sources.md) |  | dbt-style source declarations (see the sources schema). May be written in any project YAML file. |

## `queries[]`

A query with settings, instead of just its name.

| Key | Type | Default | Description |
|---|---|---|---|
| `query` (required) | string |  | The name of a `.sql` file under `reports/`, without folder or extension. |
| `profile` | string |  | The connection this query runs on, over the report's, Set's, folder's and project's. Must agree with the `profile` of any source it uses. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `tab` | boolean | `true` | `false` runs the file only for what it does (temp tables, `SET`s) and discards any result, so it gets no tab. |
| `tab_name` | string |  | The tab (sheet) name. Default: the file's name. Not allowed with `tab: false`. |
| `anchor` | string |  | Where the data starts on the sheet (xlsx only). Default: `A1`. |
| `header` | boolean |  | Whether to write the column names as the first row (xlsx only). Default: the output's `header`. |
| `columns` | map |  | Per-column settings for this tab, by column name (xlsx only). |

## `queries[].columns.<name>`

Settings for one column of an xlsx tab.

| Key | Type | Default | Description |
|---|---|---|---|
| `format` | string |  | The Excel number format of the column, e.g. `#,##0.00` or `dd/mm/yyyy`. See the xlsx column formats in the plugins reference. |
| `formula` | string |  | An Excel formula for each row of this column; `{name}` stands for that column's cell on the same row, e.g. `=ROUND({qty}*{unit_price},2)`. The SQL selects a placeholder column where the formula goes. |
| `total` | string |  | Puts a total under the column: one of `sum`, `count`, `average`, `min`, `max`, or a formula such as `=SUM({net:*})`. |

## `output`

How a report's result is written and where it goes. Besides the keys below, each format takes its own options (for example `delimiter` for `delimited`, `columns` for `fixed_width`); they are documented with the plugins.

| Key | Type | Default | Description |
|---|---|---|---|
| `format` | string |  | The output format: `csv`, `delimited`, `fixed_width`, `parquet` or `xlsx` (each a plugin). Default: `csv`, or the project's `default_output`. |
| `destination` | map (see below) or list of map (see below) |  | Where to deliver the file: one destination, or a list to deliver to several in one run. Default: the file stays in the target path. |
| `template` | map (see below) |  | Fills a branded Excel workbook instead of creating a new one (xlsx only). |
| `extension` | string or boolean or null |  | File extension for text formats (e.g. `aba`), instead of the format's own. `""` or `false` means none. Not for xlsx. |
| _other keys_ | | | Options of the plugin that handles this block; see [Plugins](plugins.md). |

## `output.destination[]`

Where a file is delivered: the name of a destination profile in `profiles.yml`, an optional `path`, and the options of that destination's plugin (e.g. `to`, `subject` and `body` for email). The built-in profile `local` copies the file to a local path.

| Key | Type | Default | Description |
|---|---|---|---|
| `profile` (required) | string |  | The destination profile in `profiles.yml` (under `destinations:`) to deliver with. `local` is built in. A destination with no entry for the run's target is skipped. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `path` | string |  | Where to put the file: a path, or a URL such as `s3://bucket/key`, depending on the destination. Rendered with Jinja, so it can use `var()`, `run.*`, macros and `destination.*` (this destination's settings). |
| _other keys_ | | | Options of the plugin that handles this block; see [Plugins](plugins.md). |

## `output.template`

Fills a branded Excel workbook instead of creating a new one (xlsx only).

| Key | Type | Default | Description |
|---|---|---|---|
| `file` (required) | string |  | Path of the `.xlsx` template, relative to the project. |
| `bindings` | list of map (see below) |  | Where each query's data goes in the template. Default: none, so the template is copied as it is. |

## `output.template.bindings[]`

One block of data in an xlsx template: a table of a query's result, or a single cell.

| Key | Type | Default | Description |
|---|---|---|---|
| `sheet` (required) | string |  | The template sheet to write into. |
| `query` | string |  | The query whose result goes here. Must be one of the Binding's queries. |
| `result_index` | integer |  | Which result set of the query to use, counting from 1. Default: the last. |
| `anchor` | string |  | The top-left cell of a table block. A table block needs a `query`. |
| `header` | boolean |  | Whether to write the column names above the data in a table block. |
| `columns` | list of string |  | The columns of the query to write in a table block, in order. Default: all. |
| `cell` | string |  | Makes this a single-cell binding: the cell to write. Needs exactly one of `value`, or `query` with `column`. |
| `value` | string |  | A fixed value (rendered with Jinja) for a single-cell binding. |
| `column` | string |  | The column of the query's first row to write into a single-cell binding. |

## `sets[]`

A Set declared in the report: a named variant of it.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` (required) | string |  | The Set's name. A report can also name Sets declared in `sets.yml`. |
| `profile` | string |  | The connection this Set runs on. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `vars` | map |  | Variables, read in SQL and YAML with `var('name')`. Values can be strings, numbers, booleans, lists or maps. |
| `exclude` | list of string |  | Queries to leave out of this Set, by name. |
| `queries` | list of string or map (see below) |  | Replaces the report's queries for this Set. |
| `tab_names` | map |  | Renames tabs for this Set: query name to tab name. |
| `output` | map (see below) |  | Output settings that replace or add to the report's for this Set. |

## `sets[].queries[]`

A query with settings.

| Key | Type | Default | Description |
|---|---|---|---|
| `query` (required) | string |  | The query's name. |
| `tab` | boolean |  | Whether the query makes a tab. |
| `tab_name` | string |  | The tab name. |
| _other keys_ | | | Options of the plugin that handles this block; see [Plugins](plugins.md). |

## `plugins[]`

A package with its source.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` (required) | string |  | The package name: lowercase letters, digits and `_`. |
| `version` | string |  | A version constraint such as `1.2.0` or `>=1.0`. Not allowed with `local`. |
| `github` | string |  | Install from the releases of this GitHub repository, `owner/repo`. |
| `local` | string |  | Use the package folder at this path as it is. |
| `registry` | string |  | Install from this registry index (a URL or a path) instead of the default one. |
