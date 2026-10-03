---
title: "Schedules"
description: "Name schedules in schedules.yml and run them from your orchestrator."
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
  vars: {period: day}
- name: close_monthly
  report: sales_summary
  set: client_a
  cron: "0 6 1 * *"
  vars: {period: month}
```

```bash
DRE_RUN_AT=2026-09-01T06:00:00Z dre run --schedule close_monthly
```

`--schedule` runs exactly the Bindings that schedule targets. Its `vars` sit above the report's and
below `--var`, and `run.schedule` renders as its name, so SQL can say
`{% if var('period') == 'day' %}...`. Pass the instant the run was scheduled for through
`DRE_RUN_AT` (or just the date through `DRE_RUN_DATE`) so reruns render the same: `DRE_RUN_AT`
pins `run.now` and `run.scheduled_at` too, so a rerun of the 18:00 firing at 21:00 still renders
as 18:00. `run_results.json`, the JSON events and `logs/dre.log` record the schedule, its
vars, the scheduled instant (`scheduled_at`), every var the run used and the command's parameters.
