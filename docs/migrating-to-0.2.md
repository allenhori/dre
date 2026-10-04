---
title: "Upgrading to 0.2"
description: "Every rename and removed name in DRE 0.2 (connections, targets, sources, template names) and what changed in 0.2.1, with what to write instead."
sidebar:
  order: 15.5
---

# Upgrading to 0.2

DRE 0.2 lets each query run on its own connection, adds dbt-style [sources](sources.md), and gives
each word one meaning: a **connection** is what you read from, a **destination** where output
goes, a **source** a declared table, and a **target** an environment. Most projects upgrade by
renaming one key in `profiles.yml` and a few template names; DRE's messages say exactly what to
write. There's no migration command. Coming from 0.2.0? See
[0.2.0 to 0.2.1](#020-to-021).

Source plugins report the identifier quote character DRE 0.2 uses for `quoting:`: update them
with `dre plugin update duckdb` (1.1.0), `postgres` (1.1.0) and `databricks` (1.1.0). Without
`quoting:`, older plugins keep working.

## profiles.yml

| 0.1 | 0.2 |
|---|---|
| `sources:` | `connections:`. `sources:` still works in 0.2.x, with a warning. |
| a profile's `target: dev` | Unchanged since 0.2.1: the profile's default entry, below `--target` and `DRE_TARGET` (0.2.0 ignored it). |

```yaml
# 0.1                                  # 0.2
sources:                               connections:
  warehouse:                             warehouse:
    target: dev                            target: dev
    targets:                               targets:
      dev: {type: duckdb, path: dev.duckdb}  dev: {type: duckdb, path: dev.duckdb}
```

`dre init` writes the 0.2 form, and adds to an 0.1 file's `sources:` section if it has one.

## Targets

As in 0.1, each profile picks its own entry: `--target`, then the new `DRE_TARGET` (either sets
every profile), then the profile's own `target:`, then `dev`. New in 0.2 (0.2.1):

- A profile the run uses with no entry for its target is an error before anything runs, where
  0.1 failed only when a query reached it. A destination is never skipped silently.
- A destination entry `{deliver: false}` delivers nowhere on that target, on purpose: write
  `dev: {deliver: false}` for a destination that should only deliver from production.
- `target.name` is the run's target (`--target`, `DRE_TARGET`, else `dev`), never a profile's
  default. A profile's own entry is `connection.target` or `destination.target`.

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

## The CLI

- `dre new` takes the plugin as `--type <plugin>`; the 0.1 name, `--source`, still works.

## 0.2.0 to 0.2.1

0.2.0 gave the run one target for every profile and moved the default into `dre_project.yml`.
0.2.1 returns to dbt's model. Every change:

| 0.2.0 | 0.2.1 |
|---|---|
| A profile's own `target:` was ignored, with the warning `profile-target-ignored`. | It's the profile's default entry, below `--target` and `DRE_TARGET`. The warning is gone. |
| `target:` in `dre_project.yml` chose the run's target. | Removed: it's an error (`removed-key`) naming the replacement. Give each profile its `target:` in `profiles.yml`, or set `DRE_TARGET` where reports run. |
| A destination with no entry for the target was skipped and logged; the run succeeded. | An error before anything runs, in `dre run`, `dre compile` and `dre validate`, naming each profile that lacks the entry and the entries it has. Profiles the run doesn't use aren't checked. |
| (none) | `{deliver: false}` on a destination entry delivers nowhere on that target, logged on each run and recorded in `run_results.json` as `not_delivered`. Rejected on connections. |
| `run_results.json` recorded a skipped destination as `skipped`. | `not_delivered`, for a `deliver: false` entry; each delivery also records its `target`. |
| `target.name` was the one target of every profile. | The run's target: `--target`, `DRE_TARGET`, else `dev`. `connection.target` and `destination.target` are each profile's entry; the manifest's `project.target` is the run's target. |
| `dre run -v` and `dre validate` printed `Target  dev (from the default)`. | `dre run` and `dre validate` print `Target  dev (default)`, plus each profile whose entry differs, and warn (`target-mismatch`) when every profile is on one other target. |

To upgrade a 0.2.0 project:

1. Remove `target:` from `dre_project.yml`. If it was `prod`, set `DRE_TARGET=prod` where reports
   run for real, or give the profiles `target: prod`.
2. Give each destination that has no `dev` entry a `dev: {deliver: false}` (or a real `dev`
   location), so local runs keep not delivering.
3. Run `dre validate`: it names every used profile still missing an entry.

A common local setup, reading production data and delivering nowhere with no flags:

```yaml
connections:
  warehouse:
    target: prod
    targets:
      dev: {type: duckdb, path: dev.duckdb}
      prod: {type: databricks, host: "{{ env_var('DATABRICKS_HOST') }}", http_path: /sql/1.0/warehouses/prod}
destinations:
  reports_s3:
    targets:
      dev: {deliver: false}
      prod: {type: s3, bucket: reports}
```

Production jobs pass `--target prod` (or set `DRE_TARGET=prod`), which puts every profile on
`prod`.

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
