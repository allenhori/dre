---
title: "timings.yml reference"
description: "Every key of the timings file: named timings schedules share."
sidebar:
  order: 24
---

# timings.yml reference

<!-- Generated from docs/schemas by .github/scripts/schema_docs.py. Edit the schema, not this page. -->

The timings file, `timings.yml`: timing names mapped to timings that any schedule can use with `timing: <name>`.

Where: `timings.yml`.

For editor autocomplete and validation, add this as the first line of the file ([editor setup](editor-setup.md)):

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.1/timings.schema.json
```

The file maps names to entries; each entry has these keys.

## Keys

| Key | Type | Default | Description |
|---|---|---|---|
| `cron` | string |  | A cron expression (5 fields, or a macro such as `@daily`). A timing needs exactly one of `cron`, `every` or `rrule`. |
| `every` | map (see below) |  | Every N days, weeks or months, with exactly one unit, e.g. `{days: 3}`. |
| `rrule` | string |  | An iCalendar recurrence rule, e.g. `FREQ=MONTHLY;BYDAY=2TU`. Use `starting` for a rule with `INTERVAL` above 1 or `COUNT`, and `at` for its time of day. |
| `starting` | string |  | With `every` or `rrule`: the first date, `YYYY-MM-DD`. `every` needs it, and so does a rule with `INTERVAL` above 1, a `COUNT`, or a day it takes from its start. |
| `at` | string |  | With `every` or `rrule`: the time of day, `HH:MM` (24-hour), unless the rule sets `BYHOUR`/`BYMINUTE`. Default: 00:00. |
| `except` | list of string |  | Dates (`YYYY-MM-DD`, in its timezone) it doesn't fire on, e.g. holidays. |
| `also` | list of string |  | Extra dates (`YYYY-MM-DD`) it fires on, at its time of day in its timezone. Needs a timing that fires at one time of day. |
| `timezone` | string |  | The timezone it fires in, an IANA name such as `Australia/Sydney`; schedules using the timing also run in it. Default: the project's `timezone:`, then UTC. |

## `every`

Every N days, weeks or months, with exactly one unit, e.g. `{days: 3}`.

| Key | Type | Default | Description |
|---|---|---|---|
| `days` | integer |  | Every this many days. |
| `weeks` | integer |  | Every this many weeks. |
| `months` | integer |  | Every this many months. |
