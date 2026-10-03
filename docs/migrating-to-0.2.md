---
title: "Upgrading to 0.2"
description: "Every rename and removed name in DRE 0.2 (connections, one target per run, sources, template names), with what to write instead."
sidebar:
  order: 15.5
---

# Upgrading to 0.2

DRE 0.2 lets each query run on its own connection, adds dbt-style [sources](sources.md), and gives
each word one meaning: a **connection** is what you read from, a **destination** where output
goes, a **source** a declared table, and the **target** the environment, one per run. Most
projects upgrade by renaming one key in `profiles.yml` and a few template names; DRE's messages
say exactly what to write. There's no migration command.

Source plugins report the identifier quote character DRE 0.2 uses for `quoting:`: update them
with `dre plugin update duckdb` (1.1.0), `postgres` (1.1.0) and `databricks` (1.1.0). Without
`quoting:`, older plugins keep working.

## profiles.yml

| 0.1 | 0.2 |
|---|---|
| `sources:` | `connections:`. `sources:` still works in 0.2.x, with a warning. |
| a profile's `target: dev` | Remove it: the run has one target for every profile (below). It's ignored in 0.2.x, with a warning. |

```yaml
# 0.1                                  # 0.2
sources:                               connections:
  warehouse:                             warehouse:
    target: dev                            targets:
    targets:                                 dev: {type: duckdb, path: dev.duckdb}
      dev: {type: duckdb, path: dev.duckdb}
```

`dre init` writes the 0.2 form, and adds to an 0.1 file's `sources:` section if it has one.

## One target per run

In 0.1 each profile picked its own default target, so a plain `dre run` could read a `dev`
database and deliver to a `prod` bucket. In 0.2 the run has one target: `--target`, then the
new `DRE_TARGET`, then the new `target:` key in `dre_project.yml`, then `dev`. A destination with
no entry for the target is skipped (and logged), so a dev run delivers only where a destination
defines `dev`. To keep delivering from a plain run, give the destination a `dev` entry, or set
`target: prod` in `dre_project.yml` (or `DRE_TARGET=prod` where reports run for real).

See [Connections and targets](connections.md).

## Template names

| 0.1 | 0.2 |
|---|---|
| `target.<field>` (`target.schema`, `target.catalog`, `target.host`...) | `connection.<field>`: the query's connection. Or `profile('name').<field>`. |
| `target.type` | `connection.type` |
| `target.profile` | `connection.name` |
| `target.name` | unchanged: the run's target |
| `run.profile` | `connection.name` |
| `run.source_type` | `connection.type` |
| `profile('x', role='source')` | `profile('x', role='connection')` |
| `profile('x').name` (the target's name) | `profile('x').target`; `.name` is now the profile's name |
| `run_query(sql)`, `columns(rel)` | unchanged; they also take `profile=` |

Each removed name is an error that says what to write instead, in `dre validate` and when a
template renders. In an output `path` or a template value, `connection.*` is the Binding's
inherited connection, and `destination.*` (new) is the destination being rendered.

## Project YAML

- `sources:` in project YAML now declares tables (dbt's format). In DRE 0.0.x a list of plugin
  names there declared plugins; that's still an error pointing at `plugins:`.
- `queries[].profile` is new: a query can run on its own connection.
- Every `profile:` value may use Jinja (`var()`, `env_var()`, `run.*`, `target.name`).
- A report no longer needs a connection of its own when every query has one (a query
  `profile:` or a source's). A query with none is the error `no-connection`.

## Schedules

- `schedule-needs-anchor`, `schedule-too-frequent` and `schedule-seconds` are now errors in
  `dre validate` (0.1.x warned): add `starting` to `every` and anchored rules, and use whole
  minutes (`FREQ=HOURLY` with `BYMINUTE`, or cron) instead of `SECONDLY`, `MINUTELY` or
  `BYSECOND`.

## The manifest and run results

- The [manifest](manifest.md) is schema 2: a `sources` section, `project.target`, and per query
  in each Binding `connection` and `depends_on.sources`. Like dbt's, it's resolved for the run's
  inputs (target, vars, environment variables), so compare manifests built with the same ones.
  Published schemas are under `https://getdre.com/schemas/v0.2/`.
- `run_results.json` records `target` (always), the Binding's inherited `profile`, the
  `connections` it used, and each result set's `connection`.
- `dre validate --json` adds `target`.
