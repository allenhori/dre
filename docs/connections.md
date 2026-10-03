---
title: "Connections and targets"
description: "profiles.yml connections and destinations, the run's one target, and which connection each query runs on."
sidebar:
  order: 4.2
---

# Connections and targets

> **Changed in 0.2.** `profiles.yml` calls database connections `connections:` (it was
> `sources:`), every run has one target, and each query can run on its own connection. See
> [Upgrading to 0.2](migrating-to-0.2.md).

Four words, one meaning each:

- a **connection** is what queries read from (a database), under `connections:` in `profiles.yml`;
- a **destination** is where output goes, under `destinations:`;
- a **source** is a declared table, read with `{{ source() }}` (see [Sources](sources.md));
- the **target** is the environment (`dev`, `prod`), one for the whole run.

## profiles.yml

```yaml
connections:
  warehouse:
    targets:
      dev: {type: duckdb, path: dev.duckdb}
      prod: {type: postgres, host: db.internal, user: reports, password: "{{ env_var('PG_PASSWORD') }}"}
  lakehouse:
    targets:
      dev: {type: databricks, host: "{{ env_var('DATABRICKS_HOST') }}", http_path: /sql/1.0/warehouses/dev}
      prod: {type: databricks, host: "{{ env_var('DATABRICKS_HOST') }}", http_path: /sql/1.0/warehouses/prod}
destinations:
  reports_s3:
    targets:
      prod: {type: s3, bucket: reports}
```

DRE looks for the file in `--profiles-dir`, `DRE_PROFILES_DIR`, the project directory (next to
`dre_project.yml`), then `~/.dre`, the same order as dbt. `dre validate` and `dre run -v` say
which file they used. Each profile lists one entry per target; `type` names the plugin and every
other key belongs to it (see [the plugins](plugins.md)). Targets can share settings with YAML
anchors and merge keys:

```yaml
connections:
  warehouse:
    targets:
      dev: &pg {type: postgres, host: db.internal, user: reports, database: shop}
      prod:
        <<: *pg
        database: shop_prod
```

`profile:` stays the name of every key that points at a profile: a report's, a query's, a Set's,
a folder's `+profile`, `default_profile`, a source's and `output.destination[].profile`.

## One target per run

The run picks one target for every profile:

1. `--target`
2. `DRE_TARGET`
3. `target:` in `dre_project.yml`
4. `dev`

`run.target` and `target.name` are that target. A plain local `dre run` therefore reads `dev`
data and delivers only where a destination has a `dev` entry: a destination with no entry for the
target is skipped and logged (`destination profile `reports_s3` has no `dev` target: not
delivered`), and the output stays in the target folder. A connection with no entry for the
target is an error when a query needs it. `dre validate` and `dre run` print the target and where
it came from.

## Which connection a query runs on

A query's connection is decided per query:

- **Explicit**: the query's own `profile:` and the `profile:` of every [source](sources.md) it
  uses. These must agree (compared as rendered names); a query whose `profile:` disagrees with a
  source it uses, or that uses sources on two connections, is an error naming both sides.
- **Inherited**, when nothing is explicit: the Set's `profile`, else the report's, else the
  folder's `+profile`, else `default_profile`. A source's `profile` overrides an inherited one;
  a source without `profile` never conflicts and runs wherever its query runs.

```yaml
# reports/finance/overview/overview.yml: one workbook, three systems
profile: warehouse                       # the report's default
queries:
  - {query: setup, tab: false}           # on warehouse
  - {query: revenue, tab_name: Revenue}  # on warehouse
  - {query: pipeline, profile: crm_pg}   # its own connection
  - customers                            # reads {{ source('lake', 'customers') }}, which names `lakehouse`
output: {format: xlsx}
```

`--profile` on `dre run` replaces the inherited connection for that run.

### Sessions

DRE opens one session per connection the Binding uses, when its first query needs it, and holds
it until the report ends. Queries run one at a time in strict YAML order, even across
connections, so logs and side effects are predictable. Temp tables, `SET`s and loaded
[lookups](lookups.md) are visible only on their own connection; a lookup loaded into a temp table
is loaded into each session that uses it. `dre validate` warns when a `tab: false` query runs on
a connection no later tab uses while later tabs run elsewhere: its setup can't reach them.
Queries never run in parallel.

### Jinja in profile values

Every `profile:` value may use Jinja, so dev and prod can use different connections:

```yaml
profile: "{{ 'lakehouse' if target.name == 'prod' else 'warehouse' }}"
```

These values choose a connection, so they're rendered before any connection is open: only
`var()`, `env_var()`, `run.*` and `target.name` exist there. `connection.*`, `run_query()`,
`columns()` and the like are an error naming the key. Key names are never templated.

### The parse pass

To know each query's connection without connecting, DRE renders every query once without a
database, as dbt's parse finds `ref()` and `source()`: `run_query()` returns no rows, `columns()`
none, `connection.*` nothing, and `raise_error()` doesn't fire. That's how `dre ls`, `dre validate
-s` and the [manifest](manifest.md) show each tab's connection offline. A `source()` reached
only while really rendering (inside a branch on `run_query()` results, say) is an error: call it
where the parse pass reaches it too.

## In templates

| Name | What it is |
|---|---|
| `target.name` | The run's target. `target` has no other fields. |
| `connection.*` | The query's connection: `name` (also `profile`), `type`, `target` and every non-secret field. Outside query SQL (a path, a subject), the Binding's inherited connection. |
| `destination.*` | The destination being rendered, only in its `path` and options. |
| `profile('name', role=)` | Any profile's fields; `role` is `connection` or `destination` when both sections have the name. |
| `run_query(sql, profile=)`, `columns(rel, profile=)` | Default to the query's connection in query SQL and the inherited one elsewhere; a source the SQL reads decides otherwise, and an explicit `profile=` must agree with it. |

See [Templates](templates.md). Secret fields stay unreadable everywhere.
