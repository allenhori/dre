---
title: "Schedule occurrences: `dre schedule ls`"
description: "When a project's schedules fire, as a table or a versioned JSON document with the command for each firing, and its JSON Schema."
sidebar:
  order: 14
---

# Schedule occurrences: `dre schedule ls`

`dre schedule ls` works out when a project's schedules fire within a time window. Each firing is
an **occurrence**, and it comes with the exact command and environment that runs it, so an
orchestrator, a scheduler table or a cron job needs to know nothing about cron, rrule or DST.

The command is pure: it reads the project and nothing else. It needs no profiles, plugins or
network, and writes nothing, so it runs in CI right after `dre validate`. Keeping the calendar,
noticing changes, dispatching runs and tracking them is up to you (see the
[orchestration recipe](orchestration.md)).

```bash
dre schedule ls                                  # the next 5 firings of each schedule
dre schedule ls --schedule close_monthly --limit 12
dre schedule ls -s sales_summary                 # schedules that run this report
dre schedule ls --from 2026-09-01 --to 2026-10-01 --output json
```

```text
TIME                   SCHEDULE       REPORTS
2026-10-01 06:00 AEST  close_monthly  sales_summary/client_a, sales_summary/client_b
2026-10-01 07:00 AEST  flash_daily    sales_summary/client_a
```

| Option | |
|---|---|
| `--schedule <name>` | Only this schedule; repeat it for several. |
| `-s <selector>` | Only schedules that run one of the selected reports (report names, `tag:`, folders, as on `dre run`). |
| `--from <when>` | Start of the window: a date (`2026-09-01`, meaning 00:00 UTC) or an RFC 3339 date-time. Default: now, to the minute. It may be in the past, to plan a backfill. |
| `--to <when>` | End of the window, exclusive. Default: 35 days after `--from`, so a weekly refresh overlaps the last one. At most 366 days after `--from`. |
| `--limit <n>` | At most this many firings per schedule. Text output defaults to 5; JSON lists every firing in the window. |
| `--split` | One occurrence per report and Set instead of one per firing (see below). |
| `--output text\|json` | A table for people (problems go to stderr), or the JSON document below. |

The project has to load without errors (other than missing profiles): a schedule with an error
would be missing from the list, which a consumer would read as removed. Run `dre validate` first.

## When a schedule fires

- **Timezone.** A schedule fires in its own `timezone:` (or its timing's), else the project's
  `timezone:`, else UTC. A report's timezone and `--timezone`/`DRE_TIMEZONE` never move a firing.
  `dre validate` warns when a schedule fires in one timezone and a report it runs renders in
  another.
- **DST.** A time that doesn't exist on the day the clocks go forward fires at the first instant
  after the gap (02:30 becomes 03:00). A time that happens twice when they go back fires once, the
  first time. Two times that land on the same instant are one firing.
- **Run date.** Each occurrence's `run_date` is the firing's date in its timezone: "06:00 on the
  1st, Sydney" is the run for the 1st, though it's still the 31st in UTC.

The schedules guide's [What's supported](schedules.md#whats-supported) lists every cron field, rule
part and option.

## The JSON document

```json
{
  "dre_schedule_version": 1,
  "dre_version": "0.1.2",
  "project": "acme_reports",
  "window": {"from": "2026-09-30T00:00:00Z", "to": "2026-10-01T00:00:00Z"},
  "split": false,
  "complete": true,
  "project_hash": "4f0c…",
  "schedules": {
    "close_monthly": {
      "definition_hash": "9a1e…",
      "enabled": true,
      "timing": null,
      "timezone": "Australia/Sydney",
      "vars": {"period": "month"},
      "bindings": [
        {"report": "sales_summary", "set": "client_a", "binding": "client_a"},
        {"report": "sales_summary", "set": "client_b", "binding": "client_b"}
      ]
    }
  },
  "occurrences": [
    {
      "key": "close_monthly/2026-09-30T20:00:00Z",
      "schedule": "close_monthly",
      "timing": null,
      "fires_at": "2026-09-30T20:00:00Z",
      "fires_at_local": "2026-10-01T06:00:00+10:00",
      "timezone": "Australia/Sydney",
      "run_date": "2026-10-01",
      "bindings": [
        {"report": "sales_summary", "set": "client_a", "binding": "client_a"},
        {"report": "sales_summary", "set": "client_b", "binding": "client_b"}
      ],
      "vars": {"period": "month"},
      "invocation": {
        "argv": ["dre", "run", "--schedule", "close_monthly"],
        "env": {"DRE_RUN_AT": "2026-09-30T20:00:00Z", "DRE_RUN_DATE": "2026-10-01"}
      }
    }
  ],
  "problems": []
}
```

| Field | |
|---|---|
| `dre_schedule_version` | The format's version, `1`. |
| `window` | The firings listed: from `from` (inclusive) to `to` (exclusive), in UTC. |
| `split` | Whether occurrences are per firing (`false`) or per report and Set (`--split`). |
| `complete` | `true` when every schedule is listed (no `--schedule` or `-s`). Only then does a missing schedule mean it was removed. |
| `project_hash` | One hash over every schedule in the project, whatever you asked for. Unchanged means no schedule changed: a refresh can stop there. |
| `schedules` | The schedules you asked for, by name, including paused ones and ones under `problems`. |
| `schedules.*.definition_hash` | Over the schedule's resolved timing (named or inline), firing timezone, vars, `enabled` and the Bindings it runs. When it changes, replace that schedule's future occurrences. |
| `schedules.*.enabled` | `false` for a paused schedule (`enabled: false`), which has no occurrences. |
| `schedules.*.timing` | The [shared timing](reference-timings.md) it uses, or `null`. |
| `occurrences` | In firing order, then by key. |
| `occurrences.*.key` | The natural key: `<schedule>/<fires_at>`, plus `/<report>/<binding>` with `--split`. Upsert on it. |
| `occurrences.*.fires_at`, `fires_at_local` | The instant in UTC, and in the firing timezone with its offset. |
| `occurrences.*.run_date` | The date the run renders as `run.date`. |
| `occurrences.*.invocation` | The command that runs it, from the project directory: `argv` (no shell, no quoting) and `env`. `DRE_RUN_AT` pins `run.now` and `run.scheduled_at`; `DRE_RUN_DATE` pins `run.date`. It never names a target, profile or credentials: your deployment adds those (`--target prod`, its own environment). |
| `problems` | Schedules that can't be expanded, each with `schedule`, `code` and `message`. They have no occurrences. |

### Identity and change

- A schedule is identified by its name, the same name `dre run --schedule` takes. A rename is a
  removal plus an addition.
- A schedule missing from a `complete` listing has been removed or renamed: retire its future
  occurrences.
- Hashes are opaque. The only promise is that the same definition gives the same hash within a
  format version.

### Split occurrences

With `--split`, each firing gives one occurrence per report and Set it runs, so each can be run,
retried and tracked on its own. The key adds `/<report>/<binding>`, `bindings` holds one entry, and
the invocation narrows the run to it:

```json
"argv": ["dre", "run", "--schedule", "close_monthly", "-s", "sales_summary", "--set", "client_a"]
```

That run keeps the schedule's vars and timezone, and renders exactly what the same Binding renders
in the full scheduled run.

### Problems

On 0.1.x these are warnings in `dre validate` and problems here; from 0.2.0 they're errors:

| Code | |
|---|---|
| `schedule-needs-anchor` | `every`, or a rule with `INTERVAL` above 1, a `COUNT`, or a day it takes from its start, has no `starting`. Its occurrences would depend on when you look. |
| `schedule-too-frequent` | `FREQ=SECONDLY` or `FREQ=MINUTELY`. A minute is the finest grain. |
| `schedule-seconds` | `BYSECOND`. Schedules fire on whole minutes. |
| `schedule-not-expandable` | The rule can't be expanded (e.g. a rule part that doesn't fit its `FREQ`). |

`dre run --schedule` still runs such a schedule.

### Compatibility

The document is described by a JSON Schema,
[`schedule-ls.schema.json`](https://github.com/get-dre/dre/blob/master/docs/schedule-ls.schema.json).
Within format 1, fields may be added in any release, so ignore fields you don't know. Removing or
renaming a field, or changing what a hash covers, needs a new `dre_schedule_version`, and only
happens in a minor release.
