---
title: "Schedules"
description: "Name schedules in schedules.yml, share timings, see when they fire, and run them from your orchestrator."
sidebar:
  order: 5
---

# Schedules

DRE doesn't fire schedules itself; your orchestrator does. `schedules.yml` names them, and one
report can have several, each with its own vars:

```yaml
- name: flash_daily
  report: sales_summary
  set: client_a
  cron: "0 7 * * *"
  timezone: Australia/Sydney
  vars: {period: day}
- name: close_monthly
  report: sales_summary
  set: client_a
  cron: "0 6 1 * *"
  timezone: Australia/Sydney
  vars: {period: month}
```

```bash
DRE_RUN_AT=2026-08-31T20:00:00Z dre run --schedule close_monthly
```

`--schedule` runs exactly the Bindings that schedule targets. Its `vars` sit above the report's and
below `--var`, and `run.schedule` renders as its name, so SQL can say
`{% if var('period') == 'day' %}...`. Pass the instant the run was scheduled for through
`DRE_RUN_AT` (or just the date through `DRE_RUN_DATE`) so reruns render the same: `DRE_RUN_AT`
pins `run.now` and `run.scheduled_at` too, so a rerun of the 18:00 firing at 21:00 still renders
as 18:00. `run_results.json`, the JSON events and `logs/dre.log` record the schedule, its
vars, the scheduled instant (`scheduled_at`), every var the run used and the command's parameters.

## See when schedules fire

`dre schedule ls` works out every firing in a window, so you can check a timing means what you
think, and hand the list to an orchestrator:

```bash
dre schedule ls                                # the next 5 firings of each schedule
dre schedule ls -s sales_summary --limit 10    # when does this report run?
dre schedule ls --output json                  # every firing in the next 35 days, with its command
```

```text
TIME                   SCHEDULE       REPORTS
2026-09-01 06:00 AEST  close_monthly  sales_summary/client_a
2026-09-01 07:00 AEST  flash_daily    sales_summary/client_a
```

Each JSON occurrence carries the exact command that runs it (`argv` and `env`, with `DRE_RUN_AT`
and `DRE_RUN_DATE` set), stable keys and hashes for change detection. See
[Schedule occurrences](schedule-ls.md) for the format, and the
[orchestration recipe](orchestration.md) for loading it into a scheduler.

## Share a timing

When many schedules fire at the same time ("the 1st of the month, 06:00 Sydney"), name the timing
once in `timings.yml` and refer to it with `timing:`. Each schedule keeps its own report, Set and
vars:

```yaml
# timings.yml
month_start:
  cron: "0 6 1 * *"
  timezone: Australia/Sydney
  except: ["2027-01-01"]
```

```yaml
# schedules.yml
- {name: close_client_a, report: sales_summary, set: client_a, timing: month_start, vars: {period: month}}
- {name: close_client_b, report: sales_summary, set: client_b, timing: month_start, vars: {period: month}}
```

A schedule has exactly one of `timing`, `cron`, `rrule` or `every`. With `timing`, it can't set any
of the timing's own keys (`timezone`, `starting`, `at`, `except`, `also`): change the timing, or give
the schedule its own. Timings and schedules are separate namespaces, and timing names follow the
schedule-name rule. An unknown timing is an error and an unused one a warning. The
[timings.yml reference](reference-timings.md) lists every key.

## Skip dates, add dates, pause

```yaml
- name: weekday_flash
  report: sales_summary
  cron: "0 7 * * MON-FRI"
  timezone: Australia/Sydney
  except: ["2026-12-25", "2026-12-28"]   # holidays
  also: ["2027-01-02"]                   # a one-off Saturday run
- name: legacy_feed
  report: legacy_feed
  cron: "0 5 * * *"
  enabled: false                         # paused
```

- `except` drops firings on those dates (in the schedule's timezone). A date it doesn't fire on
  changes nothing.
- `also` adds firings on those dates, at the schedule's time of day. It needs a timing that fires at
  one time of day: a cron with one minute and hour, `at`, or a rule with one `BYHOUR`. A date it
  already fires on is still one firing, and a date in both lists fires.
- `enabled: false` pauses a schedule without deleting it. It keeps its name and settings,
  `dre schedule ls` lists it as paused with no occurrences, and `dre run --schedule` still runs it
  by hand.

## Run one report or Set of a schedule

A schedule that runs several Bindings can run just one of them, keeping the schedule's vars and
timezone:

```bash
dre run --schedule close_monthly -s sales_summary --set client_b
```

Selecting something the schedule doesn't run is an error that lists what it does run. This is the
command `dre schedule ls --split` puts in each occurrence, so an orchestrator can run, retry and
track each report and Set on its own.

## What's supported

### Timezone and DST

A schedule fires in, nearest first:

1. its `timezone:` (or its timing's),
2. the project's `timezone:` in `dre_project.yml`,
3. UTC.

A report's own timezone and `--timezone`/`DRE_TIMEZONE` never move a firing. When the run starts,
the [run's timezone](templates.md#timezone) follows its usual order, where the schedule's (or its
timing's) `timezone:` sits above the report's. So a schedule without a `timezone:` fires in the
project's timezone but runs in the report's: `dre validate` warns when they differ. Setting
`timezone:` on the schedule or timing keeps them together.

- A time that doesn't exist (the clocks go forward over it) fires at the first instant after the
  gap: 02:30 on the day Sydney moves from 02:00 to 03:00 fires at 03:00.
- A time that happens twice (the clocks go back) fires once, the first time.
- Firings that land on the same instant are one firing.
- The run date is the firing's date in its timezone: 06:00 on the 1st in Sydney is the run for the
  1st, though it's still the 31st in UTC.

### cron

Five fields, `minute hour day-of-month month day-of-week`:

| Field | Values | Names |
|---|---|---|
| minute | 0-59 | |
| hour | 0-23 | |
| day-of-month | 1-31 | |
| month | 1-12 | `jan`-`dec` |
| day-of-week | 0-7 (0 and 7 are Sunday) | `sun`-`sat` |

Each field takes `*`, a value, a list (`1,15`), a range (`1-5`, `mon-fri`) and a step (`*/15`,
`0-30/10`). Names are case-insensitive. The macros `@yearly` (`@annually`), `@monthly`,
`@weekly`, `@daily` (`@midnight`) and `@hourly` stand for their usual expressions.

**Both day fields.** When either day field starts with `*`, a day must match both (one of them
being every day, or a step over every day): `0 6 */2 * MON` is odd-numbered days that are
Mondays. When both are restricted, a day matching either is enough: `0 6 1,15 * MON` is the 1st,
the 15th and every Monday. This is classic cron's rule.

Not supported: seconds and year fields, and the `L`, `W`, `#` and `?` extensions. Use an `rrule`
for "the last weekday" or "the 2nd Tuesday".

### rrule

An [iCalendar recurrence rule](https://datatracker.ietf.org/doc/html/rfc5545#section-3.3.10)
(the `RRULE:` prefix is optional):

| Part | |
|---|---|
| `FREQ` | `HOURLY`, `DAILY`, `WEEKLY`, `MONTHLY` or `YEARLY` (required). |
| `INTERVAL` | Every N periods: `FREQ=WEEKLY;INTERVAL=2` is every other week. Needs `starting`. |
| `COUNT` | Stop after N firings, counted from `starting` (which it needs). |
| `UNTIL` | Stop after this date (`20261231`) or date-time (`20261231T180000`), read in the schedule's timezone. Not with `COUNT`. |
| `BYMONTH` | 1-12. |
| `BYMONTHDAY` | 1-31, or -1 for the last day, -2 for the one before, ... |
| `BYYEARDAY` | 1-366 or negative. |
| `BYWEEKNO` | ISO week 1-53 or negative, with `FREQ=YEARLY`. |
| `BYDAY` | `MO`-`SU`, optionally numbered in the month or year: `2TU` (2nd Tuesday), `-1FR` (last Friday). |
| `BYHOUR`, `BYMINUTE` | 0-23 and 0-59: the time of day, instead of `at`. |
| `BYSETPOS` | Pick from each period's set: `BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1` is the last weekday. |
| `WKST` | The first day of the week, for `INTERVAL` with weeks (default `MO`). |

Rejected: `FREQ=SECONDLY`, `FREQ=MINUTELY` and `BYSECOND`; a minute is the finest grain. They're
errors (warnings on 0.1.x). A part that doesn't fit its frequency, such as `BYMONTHDAY` with
`FREQ=WEEKLY`, shows up as a problem in `dre schedule ls`.

### every

`every: {days: N}`, `{weeks: N}` or `{months: N}` counts from `starting`, at `at`. A start on the
29th-31st skips months without that day; for month ends, use `rrule: "FREQ=MONTHLY;BYMONTHDAY=-1"`.

### Anchors: `starting` and `at`

- `starting` (`YYYY-MM-DD`) is the first date for `every` and rules. `every` needs it, and so does a
  rule with `INTERVAL` above 1, a `COUNT`, or one that takes its day from the start (a `WEEKLY`,
  `MONTHLY` or `YEARLY` rule without `BYDAY`, `BYMONTHDAY`, `BYYEARDAY` or `BYWEEKNO`). Without it
  the firings would depend on when you look.
- `at` (`HH:MM`) is the time of day, unless the rule sets `BYHOUR` or `BYMINUTE` itself.
- With no time given, a schedule fires at 00:00 in its timezone, and `dre validate` warns.
- Cron schedules take neither: the expression sets the days and the time.

A missing `starting` is an error (a warning on 0.1.x).

### Worked examples

Firings from 3 October 2026, in Sydney:

| Timing | YAML | Fires |
|---|---|---|
| Every 2nd Tuesday at 07:00 | `rrule: "FREQ=MONTHLY;BYDAY=2TU"`, `at: "07:00"` | 13 Oct, 10 Nov, 8 Dec |
| The first weekday of the year | `rrule: "FREQ=YEARLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=1"`, `at: "08:00"` | Fri 1 Jan 2027 |
| The first Friday of March | `rrule: "FREQ=YEARLY;BYMONTH=3;BYDAY=1FR"`, `at: "09:00"` | 5 Mar 2027 |
| The first Friday of every month | `rrule: "FREQ=MONTHLY;BYDAY=1FR"`, `at: "09:00"` | 6 Nov, 4 Dec, 1 Jan |
| Every 5 days from 2 October | `every: {days: 5}`, `starting: "2026-10-02"`, `at: "06:00"` | 7 Oct, 12 Oct, 17 Oct |
| The last weekday of the month | `rrule: "FREQ=MONTHLY;BYDAY=MO,TU,WE,TH,FR;BYSETPOS=-1"`, `at: "18:00"` | 30 Oct, 30 Nov, 31 Dec |
| 29 February | `rrule: "FREQ=YEARLY;BYMONTH=2;BYMONTHDAY=29"`, `at: "06:00"` | 29 Feb 2028 |
| Every other Monday | `rrule: "FREQ=WEEKLY;INTERVAL=2;BYDAY=MO"`, `starting: "2026-10-05"`, `at: "09:00"` | 5 Oct, 19 Oct, 2 Nov |
| Three runs, then stop | `rrule: "FREQ=DAILY;COUNT=3"`, `starting: "2026-10-05"`, `at: "10:00"` | 5, 6 and 7 Oct |
| Fridays until the end of October | `rrule: "FREQ=WEEKLY;BYDAY=FR;UNTIL=20261031"`, `starting: "2026-10-01"`, `at: "16:00"` | 9, 16, 23 and 30 Oct |
| Weekdays at 07:00 | `cron: "0 7 * * MON-FRI"` | every weekday |
| 06:00 on the 1st and 15th | `cron: "0 6 1,15 * *"` | 15 Oct, 1 Nov, 15 Nov |

Check any timing with `dre schedule ls --schedule <name>` before relying on it.
