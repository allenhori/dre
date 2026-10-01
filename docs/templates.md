---
title: "Templates"
description: "Jinja in SQL, paths and options: target, profile(), columns(), dates and timezones."
sidebar:
  order: 6
---

# Templates

SQL files, output paths, destination options and template values all render through the same
Jinja environment ([minijinja](https://github.com/mitsuhiko/minijinja)). Beyond your own macros
in `macros/` and packages, every template sees:

| Name | What it is |
|---|---|
| `var('name', default)` | A variable: `--var`, then the schedule, Set, report, folders, project. |
| `env_var('NAME', default)` | An environment variable. |
| `run.*` | The run: `report`, `set`, `target`, `profile`, `source_type`, `schedule`, `date`, `now`, `timezone`. |
| `target.*` | The Binding's source connection: its target's fields (below). |
| `profile('name').*` | Any profile's active target (below). |
| `run_query(sql)` | Rows from the report's own connection. |
| `columns(rel)` | A relation's columns (below). |
| `ref('file')` / `lookup('name')` | Another `.sql` file as a subquery; a lookup's rows. |
| `date()`, `datetime()`, `period()`, `month_of()`, ... | Calendar values (below). |
| `dispatch('macro', 'package')` | A package macro's per-database variant. |
| `raise_error('message')` | Stop rendering with this error, e.g. from a macro that checks its arguments. |

## Names from profiles: `target` and `profile()`

Build names from the connection instead of hard-coding them:

```sql
select * from {{ target.catalog }}.{{ target.schema }}.orders   -- client_a_catalog.sales_dev.orders
```

- `target.name` is the active target (`dev`, `prod`, or `--target`), `target.type` the plugin
  type, `target.profile` the profile's name, and every other field of the target is there by its
  own name. Fields are read after `env_var()` has been applied.
- `profile('reports_s3').bucket` reads another profile's active target the same way. It looks in
  `sources:` and `destinations:`; when both have the name, say which:
  `profile('shared', role='destination')`.
- **Secrets can't be read.** A field whose value comes from a `DRE_SECRET_*` variable, or that the
  plugin's `describe` marks secret, is an error to read, so a password never reaches compiled SQL,
  logs or a file path. When the plugin isn't installed to ask, fields named like `password`,
  `secret`, `token`, `key` or `credential` count as secret.

Keep the naming rule in one place with your own macro:

```sql
{# macros/names.sql #}
{% macro crm(t) %}{{ var('client') }}_catalog.crm_{{ target.name }}.{{ t }}{% endmacro %}

select * from {{ crm('orders') }}
```

## Columns: `columns(rel)`

`columns('orders')` returns the relation's columns in `select *` order, each with `name` and
`type` (the Arrow type, e.g. `Int64`, `Utf8`, `Date32`):

```sql
select {% for c in columns('orders') if c.name != '_etl_ts' %}{{ c.name }}{% if not loop.last %}, {% endif %}{% endfor %}
from orders
```

`rel` is anything that can follow `from`: a table, a fully qualified name, a temp table made by an
earlier query, or `ref('file')`. DRE asks the database with `select * from <rel> as _dre_cols
where 1=0`, once per relation in each file it renders. Like `run_query()`, it connects only when a template
calls it, so `dre compile` connects for reports that use it. Packages build on it:
`dre_utils.star()` is one.

`dre run` renders each query just before running it, so a template sees what the queries before
it made. `dre compile`, `--dry-run` and `dre validate` run nothing, so there a temp table made by
an earlier query doesn't exist yet: `columns()` or `run_query()` on it fails there, and works in
`dre run`. `dre validate` reports such a report as a warning (it can only be checked by
`dre run`) and still checks everything else.

## String literals and Databricks

Databricks SQL doesn't read `''` inside a string literal as an escaped quote: `'O''Brien'` is two
adjacent literals that Databricks joins into `OBrien`. It reads backslashes as escapes
instead: `'O\'Brien'`. Postgres and DuckDB are the other way round. A macro that writes values
into SQL as literals should `dispatch()` a Databricks variant:

```sql
{% macro literal(v) %}{{ dispatch('literal', 'my_macros')(v) }}{% endmacro %}
{% macro default__literal(v) %}'{{ v | replace("'", "''") }}'{% endmacro %}
{% macro databricks__literal(v) %}'{{ v | replace("\\", "\\\\") | replace("'", "\\'") }}'{% endmacro %}
```

Lookups (`ref('countries')`) are inlined in a form every engine reads the same way.

## Dates and times

`run.date` is a date, not a string. It's `DRE_RUN_DATE` when set, else today in the run's
timezone. Everything below is worked out when the template renders, so the compiled SQL holds
plain literals, and a rerun with the same `DRE_RUN_DATE` renders the same SQL.

```sql
where txn_date between '{{ run.date.prev_month.start.date }}' and '{{ run.date.prev_month.end.date }}'
-- where txn_date between '2026-08-01' and '2026-08-31'
```

### Timezone

The run's timezone decides what "today" is and what midnight means. It's **UTC** unless you set
one, nearest first:

1. `--timezone Australia/Sydney`
2. `DRE_TIMEZONE`
3. `timezone:` on the `schedules.yml` entry (with `--schedule`)
4. `timezone:` in the report's YAML
5. `+timezone:` in folder config
6. `timezone:` in `dre_project.yml`

Names are IANA timezone names (`Europe/London`, `America/New_York`, `UTC`). `run.timezone`
renders the one in use, and `run_results.json` and the JSON events record it.

### Dates

A date renders as `YYYY-MM-DD`.

| | |
|---|---|
| Parts | `year`, `month`, `day`, `quarter`, `week`, `week_year`, `weekday` (1 = Monday ... 7), `day_of_year`, `days_in_month` |
| Neighbours | `prev_day`, `next_day`, `add(days=, weeks=, months=, years=)` (negative to go back; 31 Jan + 1 month is 28/29 Feb) |
| Boundaries | `week_start`, `week_end`, `month_start`, `month_end`, `quarter_start`, `quarter_end`, `year_start`, `year_end` |
| Periods | `this_week`, `prev_week`, `next_week`, `this_month`, `prev_month`, `next_month`, `this_quarter`, `prev_quarter`, `next_quarter`, `this_year`, `prev_year`, `next_year`, `as_period()` (the day itself) |
| Formats | `iso`, `yyyymmdd`, `ddmmyyyy`, `yyyy`, `mm`, `dd`, `format('%d/%m/%Y')` (strftime) |
| As time | `start`, `end` (its first and last instant), `unix`, `unix_ms` (its midnight) |

### Periods

A period is a run of whole days: a week, a month, a quarter, a year, or any range.

| | |
|---|---|
| `start`, `end` | Its first and last instant: `2026-08-01 00:00:00`, `2026-08-31 23:59:59.999999`. |
| `start.date`, `end.date` | Its first and last day: `2026-08-01`, `2026-08-31`. Also `first_day`, `last_day`. |
| `next`, `prev` | The adjacent period of the same kind (`prev_month.prev` is two months back). |
| `days` | How many days it has. |

Filter a `date` column with the days and a `timestamp` column with the instants:

```sql
where txn_date between '{{ p.start.date }}' and '{{ p.end.date }}'
where txn_ts   between '{{ p.start }}'      and '{{ p.end }}'
where txn_ts   >=      '{{ p.start }}'      and txn_ts < '{{ p.next.start }}'
```

The last form is the safest for timestamps: some databases keep fewer than 6 decimal places and
round `23:59:59.999999` up to the next day.

### Datetimes

A datetime is an instant, shown in a timezone. It renders as `YYYY-MM-DD HH:MM:SS`, with
`.ffffff` only when there's a fraction.

| | |
|---|---|
| Parts | `date`, `time`, `year`, `month`, `day`, `hour`, `minute`, `second`, `timezone`, `offset` |
| Other zones | `utc`, `tz('Europe/London')`: the same instant, shown elsewhere |
| Formats | `iso` (with offset: `2026-08-01T00:00:00+10:00`), `unix`, `unix_ms`, `format('%H:%M')` |
| Moving | `add(days=, weeks=, months=, years=, hours=, minutes=, seconds=)` |

`run.now` is the instant the run started. Unlike `run.date` it isn't reproducible, and
`DRE_RUN_DATE` doesn't change it.

A day's first instant depends on the zone: with `timezone: Australia/Sydney`,
`run.date.start.utc` is 13:00 or 14:00 the previous day, and `run.date.unix` is Sydney's
midnight. Use `.utc` when the table stores UTC timestamps but the report is about Sydney days.

### Building dates

| | |
|---|---|
| `date('2026-03-15')`, `date(2026, 3, 15)` | A date. |
| `'2026-03-15' \| as_date` | A date from a string var (`YYYY-MM-DD` or `YYYYMMDD`). |
| `datetime('2026-03-15 10:30:00')`, `\| as_datetime` | A datetime, in the run's zone unless the string has an offset (`...+10:00`, `...Z`). |
| `month_of(2026, 2)`, `quarter_of(2026, 1)`, `year_of(2026)` | A period. |
| `week_of(2026, 12)` | Week 12 of 2026, in the project's week numbering. |
| `date_range('2026-01-05', '2026-01-18')` | Any run of days; its `next` is the following run of the same length. |

### Named periods: `period()`

`period(name)` is a period relative to `run.date`, so a schedule var can pick a report's range:

```yaml
# schedules.yml
- {name: flash_daily, report: sales, cron: "0 7 * * *", vars: {period: yesterday}}
- {name: close_monthly, report: sales, cron: "0 6 1 * *", vars: {period: last_month}}
```

```sql
{% set p = period(var('period')) %}
where sale_date between '{{ p.start.date }}' and '{{ p.end.date }}'
```

| Name | Period |
|---|---|
| `today`, `yesterday` | That one day. |
| `this_week`, `last_week` | The whole week (see `week_start`). |
| `this_month`, `last_month`, `this_quarter`, `last_quarter`, `this_year`, `last_year` | The whole month, quarter or year. |
| `mtd`, `qtd`, `ytd` | From the start of the month, quarter or year through `run.date` itself. |
| `last_n_days` | The `n` days before `run.date`: `period('last_n_days', n=7)`. |
| `last_n_months` | The `n` whole months before `run.date`'s month. |

`as_of=` counts from another date: `period('last_month', as_of=var('as_at'))`.

### Weeks

```yaml
# dre_project.yml
week_start: monday      # or sunday: moves week_start, week_end and the week periods
week_numbering: iso     # or us
```

- `iso` (default): week 1 holds the year's first Thursday, so the first days of January can
  belong to the previous year's week 52 or 53. `week_year` gives that year.
- `us`: week 1 holds 1 January.
