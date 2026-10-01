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
| `timezone` | string |  | The timezone `run.date` and `run.now` use, an IANA name such as `Australia/Sydney`. Default: UTC. |
| `cron` | string |  | A cron expression. A schedule needs exactly one of `cron`, `every` or `rrule`. |
| `every` | map (see below) |  | Every N days, weeks or months, with exactly one unit, e.g. `{days: 3}`. |
| `rrule` | string |  | An iCalendar recurrence rule, e.g. `FREQ=WEEKLY;BYDAY=MO;BYHOUR=5`. |
| `starting` | string |  | With `every`: the first date, `YYYY-MM-DD`. |
| `at` | string |  | With `every`: the time of day, `HH:MM` (24-hour). |

## `every`

Every N days, weeks or months, with exactly one unit, e.g. `{days: 3}`.

| Key | Type | Default | Description |
|---|---|---|---|
| `days` | integer |  | Every this many days. |
| `weeks` | integer |  | Every this many weeks. |
| `months` | integer |  | Every this many months. |
