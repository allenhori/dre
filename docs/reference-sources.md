---
title: "Sources reference"
description: "Every key of a sources declaration: sources, tables and columns, as in dbt."
sidebar:
  order: 27.5
---

# Sources reference

<!-- Generated from docs/schemas by .github/scripts/schema_docs.py. Edit the schema, not this page. -->

dbt-style source declarations: the tables a project reads, with DRE's `profile:` for the connection they live on. In any project YAML file under a top-level `sources:` key (`sources/` is the conventional folder); dbt's `version: 2` is accepted beside it. Use a table in SQL with `{{ source('<source>', '<table>') }}`.

Where: any project YAML file with a top-level `sources:` key, e.g. `sources/<name>.yml`.

For editor autocomplete and validation, add this as the first line of the file ([editor setup](editor-setup.md)):

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.2/sources.schema.json
```

## Keys

| Key | Type | Default | Description |
|---|---|---|---|
| `sources` (required) | list of map (see below) |  | The sources this file declares. |
| `version` | any |  | dbt's file version (`2`); accepted and ignored. |

## `sources[]`

A source: tables in one schema of one system.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` (required) | string |  | The source's name, the first argument of `source()`. Unique in the project. |
| `description` | string |  | What the source is. |
| `database` | string |  | The database (catalog). Set: `source()` renders `database.schema.table`; unset: `schema.table`. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `schema` | string |  | The schema; the source's name unless set. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `profile` | string |  | DRE's addition: the connection (in `profiles.yml`) the source lives on. A query using the source runs there; it overrides the report's, Set's, folder's and project's default. Unset: the source runs wherever its query runs. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `quoting` | map (see below) |  | Which parts `source()` quotes, with the connection's identifier quote character. Unset parts inherit (table from source), else aren't quoted. |
| `tags` | string or list of string |  | Tags, for documentation. |
| `meta` | map |  | Free-form metadata, kept in the manifest. |
| `tables` | list of map (see below) |  | The source's tables. |
| `loader` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `loaded_at_field` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `loaded_at_query` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `config` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `overrides` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `freshness` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `docs` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |

## `sources[].quoting`

Which parts `source()` quotes, with the connection's identifier quote character. Unset parts inherit (table from source), else aren't quoted.

| Key | Type | Default | Description |
|---|---|---|---|
| `database` | boolean |  | Quote the database. |
| `schema` | boolean |  | Quote the schema. |
| `identifier` | boolean |  | Quote the table name. |

## `sources[].tables[]`

One table of the source.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` (required) | string |  | The name `source()` uses. |
| `identifier` | string |  | The real table name, when it differs from `name`. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `description` | string |  | What the table holds. |
| `quoting` | map (see below) |  | Which parts `source()` quotes, with the connection's identifier quote character. Unset parts inherit (table from source), else aren't quoted. |
| `tags` | string or list of string |  | Tags, for documentation. |
| `meta` | map |  | Free-form metadata, kept in the manifest. |
| `columns` | list of map (see below) |  | Declared columns, checked by `dre validate --live`. |
| `loaded_at_field` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `loaded_at_query` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `tests` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `data_tests` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `freshness` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `external` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `config` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `docs` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |

## `sources[].tables[].columns[]`

A column of the table. `dre validate --live` checks that it exists and, with `data_type`, that its type matches loosely.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` (required) | string |  | The column's name (compared case-insensitively). |
| `description` | string |  | What the column holds. |
| `data_type` | string |  | The column's SQL type, e.g. `bigint`, `varchar`, `timestamp`; compared loosely with what the database returns. |
| `meta` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `tags` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `quote` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `tests` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `data_tests` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `constraints` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `config` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `docs` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
| `granularity` | any |  | A dbt key DRE accepts but doesn't use yet (noted by `dre validate`). |
