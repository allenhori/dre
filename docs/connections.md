---
title: "Connections and targets"
description: "profiles.yml connections and destinations, which target each profile uses, and which connection each query runs on."
sidebar:
  order: 4.2
---

# Connections and targets

> **Changed in 0.2.** `profiles.yml` calls database connections `connections:` (it was
> `sources:`), and each query can run on its own connection. **Changed in 0.2.1:** each profile
> has its own default `target:` again, a missing entry is an error, and `{deliver: false}` marks
> a destination that delivers nowhere. See [Upgrading to 0.2](migrating-to-0.2.md).

Four words, one meaning each:

- a **connection** is what queries read from (a database), under `connections:` in `profiles.yml`;
- a **destination** is where output goes, under `destinations:`;
- a **source** is a declared table, read with `{{ source() }}` (see [Sources](sources.md));
- a **target** is an environment (`dev`, `prod`): each profile has one entry per target, and the
  run's target (`target.name`) is the one `--target` or `DRE_TARGET` names, else `dev`.

## profiles.yml

```yaml
connections:
  warehouse:
    targets:
      dev: {type: duckdb, path: dev.duckdb}
      prod: {type: postgres, host: db.internal, user: reports, password: "{{ env_var('PG_PASSWORD') }}"}
  lakehouse:
    target: prod             # this profile's default entry (dbt's key); otherwise `dev`
    targets:
      dev: {type: databricks, host: "{{ env_var('DATABRICKS_HOST') }}", http_path: /sql/1.0/warehouses/dev}
      prod: {type: databricks, host: "{{ env_var('DATABRICKS_HOST') }}", http_path: /sql/1.0/warehouses/prod}
destinations:
  reports_s3:
    targets:
      dev: {deliver: false}  # deliberately deliver nowhere on dev
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

## Which target each profile uses

Each profile the run uses picks its entry, highest first:

1. `--target`
2. `DRE_TARGET`
3. the profile's own `target:` in `profiles.yml`
4. `dev`

`--target` and `DRE_TARGET` set every profile at once; there's no per-profile flag. Without
them, each profile uses its own default, as in dbt. So the profiles above, with no flags, read
`warehouse` on `dev`, `lakehouse` on `prod`, and deliver nowhere; `--target prod` (or
`DRE_TARGET=prod` where reports run for real) puts all three on `prod`.

Only the profiles the run uses are checked. Each needs an entry for its target: one without is
an error before anything runs, in `dre run`, `dre compile` and `dre validate`, naming every
profile that lacks it and the entries it has. So `--target prd` fails loudly instead of
delivering nothing:

```text
error: connection `warehouse` has no `prd` entry (it has: dev, prod); `prd` comes from --target; nothing was run
```

### Delivering nowhere

A destination entry `{deliver: false}` delivers nowhere on that target, on purpose:

```yaml
destinations:
  reports_s3:
    targets:
      dev: {deliver: false}
      prod: {type: s3, bucket: reports}
```

The output stays in the target folder, each run logs it (`destination `reports_s3`: `dev`
delivers nowhere`), and `run_results.json` records the delivery as `not_delivered`, which isn't
a failure. `deliver: false` takes no other keys, and connections can't have it. A common setup
for developing locally against production data: the connection's `target: prod`, every
destination's `dev: {deliver: false}`, and no flags.

### The run's target

`target.name` (and `run.target`) is the run's target: `--target`, else `DRE_TARGET`, else `dev`.
It's one value for the whole run and never a profile's default, so in the setup above
`target.name` is `dev` while the connection reads `prod`. Each profile's own entry is
`connection.target` and `destination.target`.

`dre run` and `dre validate` print the run's target, where it came from, and each profile whose
entry differs:

```text
    Target  dev (default); connection `lakehouse`: prod
```

When every profile the run uses is on one target other than the run's, DRE warns
(`target-mismatch`): templates that test `target.name` would see `dev` while every profile reads
`prod`. Pass `--target prod` or set `DRE_TARGET` to make them agree.

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

A query the parse pass can't render (a `--var` its template rejects, say) makes its own report
invalid in the manifest, and `dre validate` reports it, but it doesn't stop `dre run` or
`dre compile` of other reports. `raise_error()` doesn't fire in the parse pass (it may only mean
`run_query()` returned nothing), but when rendering then fails, its message is the one reported.

## In templates

| Name | What it is |
|---|---|
| `target.name` | The run's target: `--target`, `DRE_TARGET`, else `dev`. `target` has no other fields. |
| `connection.*` | The query's connection: `name` (also `profile`), `type`, `target` (its entry for this run) and every non-secret field. Outside query SQL (a path, a subject), the Binding's inherited connection. |
| `destination.*` | The destination being rendered, only in its `path` and options: `name`, `type`, `target` (its entry for this run) and its fields. |
| `profile('name', role=)` | Any profile's fields; `role` is `connection` or `destination` when both sections have the name. |
| `run_query(sql, profile=)`, `columns(rel, profile=)` | Default to the query's connection in query SQL and the inherited one elsewhere; a source the SQL reads decides otherwise, and an explicit `profile=` must agree with it. |

See [Templates](templates.md). Secret fields stay unreadable everywhere.
