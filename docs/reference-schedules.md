---
title: "schedules.yml reference"
description: "Every key of the schedules file."
sidebar:
  order: 23
---

# schedules.yml reference

<!-- Generated from docs/schemas by .github/scripts/schema_docs.py. Edit the schema, not this page. -->

The schedules file, `schedules.yml`: a list of named schedules.

Where: `schedules.yml`.

For editor autocomplete and validation, add this as the first line of the file ([editor setup](editor-setup.md)):

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.1/schedules.schema.json
```

The file is a list; each entry has these keys.

## Keys

| Key | Type | Default | Description |
|---|---|---|---|
| `name` (required) | string |  | The schedule's name: letters, digits and `_`, not starting with a digit. Unique across the project. |
| `report` | string |  | The report to run. Use `report` or `select`, not both. |
| `set` | string |  | The Set of `report` to run. Only with `report`. |
| `select` | string |  | A selector for the reports to run, e.g. `tag:regulatory`. Use `report` or `select`, not both. |
| `vars` | map |  | Variables for the run: above the report's own and below `--var`. |
| `timing` | string |  | The name of a timing in `timings.yml` to fire on. Use `timing` or one of `cron`, `every`, `rrule`; with `timing`, the schedule sets none of the timing's keys (`timezone`, `starting`, `at`, `except`, `also`). |
| `timezone` | string |  | The timezone it fires in, and the one `run.date` and `run.now` use, an IANA name such as `Australia/Sydney`. Default: the project's `timezone:` for firing (the report's for the run), then UTC. |
| `enabled` | boolean |  | `false` pauses the schedule: it keeps its name and settings but `dre schedule ls` lists no occurrences for it. Default: `true`. |
| `cron` | string |  | A cron expression (5 fields, or a macro such as `@daily`). A schedule needs exactly one of `timing`, `cron`, `every` or `rrule`. |
| `every` | map (see below) |  | Every N days, weeks or months, with exactly one unit, e.g. `{days: 3}`. |
| `rrule` | string |  | An iCalendar recurrence rule, e.g. `FREQ=MONTHLY;BYDAY=2TU`. Use `starting` for a rule with `INTERVAL` above 1 or `COUNT`, and `at` for its time of day. |
| `starting` | string |  | With `every` or `rrule`: the first date, `YYYY-MM-DD`. `every` needs it, and so does a rule with `INTERVAL` above 1, a `COUNT`, or a day it takes from its start. |
| `at` | string |  | With `every` or `rrule`: the time of day, `HH:MM` (24-hour), unless the rule sets `BYHOUR`/`BYMINUTE`. Default: 00:00. |
| `except` | list of string |  | Dates (`YYYY-MM-DD`, in its timezone) it doesn't fire on, e.g. holidays. |
| `also` | list of string |  | Extra dates (`YYYY-MM-DD`) it fires on, at its time of day in its timezone. Needs a timing that fires at one time of day. |

## `every`

Every N days, weeks or months, with exactly one unit, e.g. `{days: 3}`.

| Key | Type | Default | Description |
|---|---|---|---|
| `days` | integer |  | Every this many days. |
| `weeks` | integer |  | Every this many weeks. |
| `months` | integer |  | Every this many months. |
