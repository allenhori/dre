---
title: "Orchestration recipe"
description: "Self-host scheduling: load occurrences into a database after every merge, refresh weekly, and run what's due. With Airflow, Databricks Jobs and cron."
sidebar:
  order: 6
---

# Orchestration recipe

DRE works out *when* schedules fire ([`dre schedule ls`](schedule-ls.md)); running them on time is
your orchestrator's job. This page sets that up with a database table and three small files:

1. **After every merge** (and once a week), CI lists the next five weeks of occurrences and
   upserts them into a table. Schedules that didn't change are left alone; ones that changed get
   new rows; removed and paused ones are retired.
2. **Every minute**, an executor claims the earliest due occurrence and runs its command.

Everything here is plain SQL, shell and Python's standard library, so it's easy to adapt. The
snippets are tested: the DRE demo project runs them, unchanged, against Postgres. Secrets never
appear in them: the database URL and DRE's own credentials come from your environment or secret
store.

## The table (Postgres)

```sql title="create-tables.sql"
-- One row per occurrence. `key` is the occurrence's natural key from `dre schedule ls`.
create table if not exists dre_occurrences (
  key             text primary key,
  project         text not null,
  schedule        text not null,
  fires_at        timestamptz not null,
  run_date        date not null,
  definition_hash text not null,
  argv            jsonb not null,
  env             jsonb not null,
  status          text not null default 'pending',  -- pending, running, done, failed, retired
  started_at      timestamptz,
  finished_at     timestamptz
);
create index if not exists dre_occurrences_due on dre_occurrences (status, fires_at);
```

## Load occurrences

`load-occurrences.sql` takes one `dre schedule ls --output json` document and applies it in one
transaction:

```sql title="load-occurrences.sql"
-- Load one `dre schedule ls --output json` document, passed as the psql variable `doc`.
begin;
create temp table incoming on commit drop as select (:'doc')::jsonb as d;

-- Retire the pending future occurrences of every schedule that changed (its definition hash),
-- was paused, or was removed. Only a complete listing says a schedule was removed.
update dre_occurrences o
   set status = 'retired'
  from incoming i
 where o.project = i.d->>'project'
   and o.status = 'pending'
   and o.fires_at >= (i.d->'window'->>'from')::timestamptz
   and case
         when i.d->'schedules' ? o.schedule then
           i.d->'schedules'->o.schedule->>'definition_hash' <> o.definition_hash
           or not (i.d->'schedules'->o.schedule->>'enabled')::boolean
         else (i.d->>'complete')::boolean
       end;

-- Add every listed occurrence. Ones already there are left alone, except retired ones, which the
-- schedule's new definition brings back.
insert into dre_occurrences (key, project, schedule, fires_at, run_date, definition_hash, argv, env)
select o->>'key', i.d->>'project', o->>'schedule', (o->>'fires_at')::timestamptz,
       (o->>'run_date')::date, i.d->'schedules'->(o->>'schedule')->>'definition_hash',
       o->'invocation'->'argv', o->'invocation'->'env'
  from incoming i, jsonb_array_elements(i.d->'occurrences') o
    on conflict (key) do update
   set definition_hash = excluded.definition_hash, argv = excluded.argv, env = excluded.env,
       run_date = excluded.run_date, status = 'pending'
 where dre_occurrences.status = 'retired';
commit;
```

What it does with each kind of change:

| Change | Effect |
|---|---|
| Nothing changed | Only occurrences new to the window are added. Run it as often as you like. |
| A schedule's timing, timezone, vars or Bindings changed | Its pending future rows are retired and its new occurrences added (a row with the same key comes back with the new command). |
| A schedule was paused (`enabled: false`) | Its pending future rows are retired. |
| A schedule was removed or renamed | Its pending future rows are retired (a rename adds the new name's rows). |
| Past, running, done or failed rows | Never touched. |

`refresh.sh` runs it. Call it from the project directory, after `dre validate`:

```bash title="refresh.sh"
#!/usr/bin/env bash
# Load the next five weeks of occurrences into the scheduler database. Run it from the project
# directory after every merge (after `dre validate`) and once a week. Arguments go to
# `dre schedule ls`, e.g. --split.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
dre schedule ls --output json "$@" > occurrences.json
psql "$DRE_SCHEDULER_DB" -v ON_ERROR_STOP=1 -q -v doc="$(cat occurrences.json)" -f "$here/load-occurrences.sql"
```

`DRE_SCHEDULER_DB` is a Postgres connection URL, from your CI's secret store. Five weeks with a
weekly refresh means a missed refresh costs nothing. Use `--split` to get one row per report and
Set, so each can be run and retried on its own; keep split and unsplit documents in separate
tables.

## Run what's due

`run_due.py` runs the earliest due occurrence and records how it went. Run it every minute (cron,
a loop, a scheduled job); several copies can run at once, since each claims its row with
`for update skip locked`:

```python title="run_due.py"
#!/usr/bin/env python3
"""Run the earliest due occurrence, if any: claim it, run its command, record how it went.

Run it every minute from the project directory. Needs `psql` and `dre` on PATH, and the
scheduler database's URL in DRE_SCHEDULER_DB. DRE_RUN_ARGS adds what your deployment decides,
e.g. "--target prod"; the occurrence never names a target, profile or credentials.
"""
import json
import os
import subprocess
import sys

DB = os.environ["DRE_SCHEDULER_DB"]
EXTRA = os.environ.get("DRE_RUN_ARGS", "").split()


def psql(sql, **variables):
    args = ["psql", DB, "-v", "ON_ERROR_STOP=1", "-q", "-At"]
    for name, value in variables.items():
        args += ["-v", f"{name}={value}"]
    return subprocess.run(args, input=sql, capture_output=True, text=True, check=True).stdout.strip()


claimed = psql("""
update dre_occurrences set status = 'running', started_at = now()
 where key = (select key from dre_occurrences
               where status = 'pending' and fires_at <= now()
               order by fires_at, key limit 1 for update skip locked)
returning json_build_object('key', key, 'argv', argv, 'env', env);
""")
if not claimed:
    sys.exit(0)
occurrence = json.loads(claimed)
print(f"running {occurrence['key']}", flush=True)
run = subprocess.run(occurrence["argv"] + EXTRA, env={**os.environ, **occurrence["env"]})
psql(
    "update dre_occurrences set status = :'status', finished_at = now() where key = :'key';",
    status="done" if run.returncode == 0 else "failed",
    key=occurrence["key"],
)
sys.exit(run.returncode)
```

The occurrence's `env` sets `DRE_RUN_AT` and `DRE_RUN_DATE`, so a run that starts late, or a
retry the next day, renders exactly what it would have rendered on time. To retry a failed one, set
its status back to `pending`.

## Wire it up

### CI on merge and weekly (GitHub Actions)

```yaml
# .github/workflows/schedules.yml
on:
  push:
    branches: [main]
  schedule:
    - cron: "0 3 * * 1"   # Mondays, 03:00 UTC
jobs:
  refresh:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
      - run: curl -fsSL https://getdre.com/install.sh | DRE_VERSION=v0.1.2 sh && echo "$HOME/.local/bin" >> "$GITHUB_PATH"
      - run: dre validate
      - run: scheduler/refresh.sh
        env:
          DRE_SCHEDULER_DB: ${{ secrets.DRE_SCHEDULER_DB }}
```

`dre schedule ls` needs no profiles or credentials; `dre validate` needs them only for reports
whose templates query the database.

### cron

```text
# m h dom mon dow  command
* * * * *  cd /srv/reports && DRE_RUN_ARGS="--target prod" scheduler/run_due.py >> logs/scheduler.log 2>&1
```

`DRE_SCHEDULER_DB` and DRE's own credentials come from the environment cron runs with (e.g.
`/etc/environment` or a wrapper that reads your secret store). Without a database, a crontab line
per schedule also works, as long as each line passes the instant it was meant for:
`CRON_TZ=Australia/Sydney` plus `DRE_RUN_AT=$(date -u +%Y-%m-%dT%H:%M:00Z) dre run --schedule
close_monthly`. cron's own DST rules then decide, not DRE's.

### Airflow

Run the executor every minute:

```python
from airflow import DAG
from airflow.operators.bash import BashOperator
import pendulum

with DAG("dre_run_due", schedule="* * * * *", start_date=pendulum.datetime(2026, 1, 1), catchup=False, max_active_runs=4):
    BashOperator(
        task_id="run_due",
        bash_command="cd /opt/reports && scheduler/run_due.py",
        env={"DRE_RUN_ARGS": "--target prod"},
        append_env=True,  # DRE_SCHEDULER_DB and credentials come from the worker's environment
    )
```

Or skip the table: map one task per occurrence of a `--split` listing, each running its `argv`
with its `env`.

### Databricks Jobs

Run `run_due.py` from a job on a one-minute schedule, or give each DRE schedule its own job with
the same timing in Quartz syntax and pass the trigger time as `DRE_RUN_AT`:

```yaml
# databricks.yml (excerpt): the close_monthly schedule, `0 6 1 * *` in Sydney
resources:
  jobs:
    close_monthly:
      name: close_monthly
      schedule:
        quartz_cron_expression: "0 0 6 1 * ?"
        timezone_id: Australia/Sydney
      tasks:
        - task_key: run
          spark_python_task:
            python_file: scheduler/run_schedule.py   # sets DRE_RUN_AT from --at, then runs dre
            parameters: ["--schedule", "close_monthly", "--at", "{{job.trigger.time.iso_datetime}}"]
```

With a job per schedule, Databricks' scheduler decides the firing times, including around DST.
Check them against `dre schedule ls` when you set the job up.

### Databricks SQL instead of Postgres

The same upsert as a Delta `MERGE` (illustrative; `:doc` is the JSON document, e.g. a job
parameter or a file read with `read_files`):

```sql
CREATE TABLE IF NOT EXISTS dre_occurrences (
  key STRING, project STRING, schedule STRING, fires_at TIMESTAMP, run_date DATE,
  definition_hash STRING, argv ARRAY<STRING>, env MAP<STRING, STRING>,
  status STRING, started_at TIMESTAMP, finished_at TIMESTAMP);

CREATE OR REPLACE TEMP VIEW incoming AS
SELECT from_json(:doc, 'project STRING, complete BOOLEAN, window STRUCT<from: STRING, to: STRING>,
  schedules MAP<STRING, STRUCT<definition_hash: STRING, enabled: BOOLEAN>>,
  occurrences ARRAY<STRUCT<key: STRING, schedule: STRING, fires_at: STRING, run_date: STRING,
    invocation: STRUCT<argv: ARRAY<STRING>, env: MAP<STRING, STRING>>>>') AS d;

UPDATE dre_occurrences SET status = 'retired'
WHERE status = 'pending'
  AND project = (SELECT d.project FROM incoming)
  AND fires_at >= (SELECT to_timestamp(d.window.from) FROM incoming)
  AND (SELECT CASE WHEN d.schedules[schedule] IS NOT NULL
                   THEN d.schedules[schedule].definition_hash <> definition_hash
                        OR NOT d.schedules[schedule].enabled
                   ELSE d.complete END FROM incoming);

MERGE INTO dre_occurrences t
USING (SELECT o.key, i.d.project, o.schedule, to_timestamp(o.fires_at) AS fires_at,
              to_date(o.run_date) AS run_date, i.d.schedules[o.schedule].definition_hash AS definition_hash,
              o.invocation.argv AS argv, o.invocation.env AS env
       FROM incoming i LATERAL VIEW explode(i.d.occurrences) AS o) s
ON t.key = s.key
WHEN MATCHED AND t.status = 'retired' THEN UPDATE SET
  definition_hash = s.definition_hash, argv = s.argv, env = s.env, run_date = s.run_date, status = 'pending'
WHEN NOT MATCHED THEN INSERT (key, project, schedule, fires_at, run_date, definition_hash, argv, env, status)
  VALUES (s.key, s.project, s.schedule, s.fires_at, s.run_date, s.definition_hash, s.argv, s.env, 'pending');
```

## What DRE does and doesn't do

`dre schedule ls` is pure computation: the same project and window always give the same document,
and it never stores, dispatches or tracks anything. Keeping the table, retrying, catching up after
an outage and alerting are this recipe's job, or DRE Cloud's, which reads the same public JSON.
