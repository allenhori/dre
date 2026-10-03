---
title: "The manifest and `run_results.json`"
description: "The project manifest and the per-run results file, with the manifest JSON Schema."
sidebar:
  order: 12
---

# The manifest and `run_results.json`

DRE writes two kinds of JSON file into the [target path](target-path.md)
(`target/` unless you move it). They read as a pair:

- **`manifest.json`** says what the project *is*: every report, Set, Binding, schedule and
  plugin, as DRE resolves them. One file for the whole project.
- **`run/<report>/<binding>/run_results.json`** says what one run of one Binding *did*: its
  vars, rows, files, deliveries and status. It records the checksum of the manifest the run
  wrote, so a run's results can be matched to the exact project behind them.

The manifest (and `dre ls --output json`, which prints part of it) is DRE's supported way for
orchestrators, CI and other tools to read a project. Read it rather than DRE's YAML: it has the
layering (project < folders < report < Set) and schedule resolution already applied.

## The project manifest at a glance

`dre compile`, `dre validate` and `dre run` write `target/manifest.json`: the whole project as
DRE resolves it, whatever was selected. It lists every report with its Sets and Bindings (merged
vars, queries, output, destinations), every schedule with the Bindings it runs, the declared
plugins, and checksums for change detection. It's built offline, with no connection or
`profiles.yml`, never contains secrets or connection settings, and is the same byte for byte for
the same project. An orchestrator can generate one task per schedule from it (each runs
`dre run --schedule <name>`), and CI can compare two manifests to find the reports a change
touched. `dre ls` prints slices of it: `dre ls -s tag:regulatory`, `dre ls --schedule
close_monthly --output json`. See its [JSON Schema](manifest.schema.json).

## When it's written

`dre compile`, `dre validate` and `dre run` write `manifest.json` right after the project loads,
before any SQL is rendered or run, in every mode (selectors, `--set`, `--schedule`, `--dry-run`,
`--preview`). It always describes the whole project, whatever was selected. A run that fails
later still leaves the manifest it started from.

- A report with problems (a missing query file, a bad option) is listed with `"valid": false`
  and its `errors`; the manifest is still written. `dre validate` still fails. `valid` covers
  the checks made when the project loads; the ones that need plugins or rendering (option checks,
  compiling) come after the manifest is written and are reported by `dre validate` itself.
- A project that can't load at all (no `dre_project.yml`, unreadable YAML) writes no manifest and
  removes an old one, so a stale file is never taken as current.
- The file is written to a temporary name and renamed, so a reader never sees half of it.
- `dre ls` never writes it; `dre clean` removes it with the rest of the target folder.

It's built offline: no database connection, no `profiles.yml` and no plugins are needed, so it
works on a fresh CI runner with no credentials.

## What's in it

```json
{
  "schema": 1,
  "version": "0.0.1-alpha-12",
  "project": {"name": "acme_reports", "default_profile": "warehouse", "timezone": "UTC", "checksum": "…"},
  "reports": {
    "monthly": {
      "name": "monthly",
      "managed": true,
      "file": "reports/finance/monthly/monthly.yml",
      "folder": ["finance", "monthly"],
      "tags": ["regulatory"],
      "default_set": "client_a",
      "queries": [{"query": "m", "file": "reports/finance/monthly/m.sql", "tab": true}],
      "checksum": "…",
      "valid": true,
      "bindings": [
        {
          "set": "client_a",
          "profile": "warehouse",
          "vars": {"client": "client_a", "region": "emea"},
          "queries": [{"query": "m", "file": "reports/finance/monthly/m.sql", "tab": true}],
          "output": {"format": "csv", "options": {}},
          "destinations": [{"profile": "inbox", "path": "out/monthly-{{ run.date.yyyymmdd }}.csv"}],
          "schedules": ["close_a"]
        }
      ]
    }
  },
  "schedules": {
    "close_a": {
      "name": "close_a",
      "report": "monthly",
      "set": "client_a",
      "schedule": {"cron": "0 6 1 * *"},
      "vars": {},
      "bindings": [{"report": "monthly", "set": "client_a"}]
    }
  },
  "plugins": [{"package": "duckdb", "version": "*", "source": {"type": "registry"}}]
}
```

- **`schema`**: the format's version (see [Versioning](#versioning)). **`version`**: the DRE
  that wrote it.
- **`project`**: its name, default source profile, `timezone:`, and the project-wide
  [checksum](#checksums).
- **`reports`**, by name: whether it's managed (declared in YAML) or a bare `.sql`, its defining
  file, folder segments, tags, timezone, default Set, queries (with tab settings and column
  options), [checksum](#checksums), validity, and its Bindings.
- **Each Binding**: its Set (`null` for a report without Sets), source profile name, fully merged
  vars, queries, output (format, options, extension, template file), destinations in delivery
  order (profile name and the path template, unrendered), and the schedules that run it.
- **`schedules`**, by name: the report or selector and Set it targets, the schedule as declared
  (`cron`, `every`, `rrule`, ...), its vars and timezone, and the Bindings it runs, resolved the way
  `dre run --schedule <name>` resolves them. An orchestrator makes one task per schedule, each
  running `dre run --schedule <name>` with `DRE_RUN_AT` set to the scheduled instant (`dre schedule ls` lists them, with the exact
  command).
- **`plugins`**: the declared packages, each with its version requirement and source
  (`registry`, `github` or `local`, plus a `location` unless it's DRE's own registry).

Paths are relative to the project root with forward slashes on every OS. Keys are sorted. There
are no timestamps, host names or absolute paths, so the same project gives the same bytes on any
machine, and moving the target path doesn't change the file.

### What's left out

- Anything that only exists after rendering: compiled SQL (that's `compiled/`), rendered output
  and destination paths, `target.*` values.
- Connection settings and credentials. Profiles appear by name only.
- Secrets: values of `DRE_SECRET_*` variables are masked as `*****`, as in `run_results.json`
  and the compiled SQL (`mask_secrets: false` in `dre_project.yml` turns that off everywhere).
- Engine internals that might change meaning. A field is left out rather than shipped with a
  meaning that could shift.

### Checksums

Every checksum is a SHA-256 in hex, over file contents only (not timestamps), in a fixed order.

- A **report's** `checksum` covers its defining file (YAML, or the `.sql` of an unmanaged report),
  each of its query files, and its template file.
- The **project's** `checksum` covers the shared inputs: `dre_project.yml`, folder config,
  `schedules.yml`, `dependencies.yml` and any other YAML that isn't a report's own, everything
  under `macros/` and `lookups/`, and every `.sql` under `reports/` that isn't a declared query
  (the usual `ref()` targets, including unmanaged reports).

To find what a change touched, compare two manifests: if the project checksum differs, treat
every report as changed; otherwise the reports whose checksum differs are the ones to preview or
validate (`dre validate -s a,b`).

## `dre ls`

`dre ls` prints the reports and Bindings a selection covers, with the same selectors as `run`:

```bash
dre ls                              # every Binding
dre ls -s tag:regulatory --set all  # a selection
dre ls --schedule close_monthly     # exactly what that schedule runs
dre ls --schedule close_monthly --output json
```

The default output is one Binding per line (report, Set, format, destinations).
`--output json` prints a document in the manifest's shape holding only the matching reports and
Bindings (and, with `--schedule`, that schedule). Data goes to stdout and messages to stderr; a
selector or schedule that matches nothing exits non-zero. `dre ls` needs no connection or
`profiles.yml` and writes nothing.

`dre validate --json` includes the same document under `"project"`.

## `run_results.json`

Each Binding a `dre run` executes writes `run/<report>/<set or default>/run_results.json` in the
target path. It records the report, Set, profile and target, the schedule and its vars, every
var the run used, the run date and timezone, the command's parameters, the status and any error,
each result set (rows and columns), each output file (`path`, relative to the project root, or to
the target path when that's outside the project), each delivery, schema drift, the resolved
`target_path`, and `manifest_checksum`: the SHA-256 of the `manifest.json` bytes that run wrote.

## Versioning

The manifest's format is a public contract. [manifest.schema.json](manifest.schema.json) is the
JSON Schema for schema 1.

- Adding an optional field keeps the schema number. Ignore fields you don't know.
- Removing, renaming or re-typing a field, or changing what a field means, bumps it. Check
  `schema` and refuse a number you don't support.

The idea of a project manifest comes from dbt; the format and code are DRE's own.
