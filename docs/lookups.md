---
title: "Lookups"
description: "Mapping tables kept as files: typed columns, inline or temp table."
sidebar:
  order: 8
---

# Lookups

A lookup is a small table you keep as a file in the project's `lookups/` folder (a mapping
table, a code list, a client's account numbers) instead of in the database. SQL uses it like a
table, and Jinja can read its rows:

```sql
select o.*, c.region
from orders o
join {{ ref('countries') }} c on c.code = o.country_code
```

```sql
{% for c in lookup('countries') %}
  sum(case when country_code = '{{ c.code }}' then amount end) as {{ c.code | lower }}_amount,
{% endfor %}
```

## Files

`lookups/<name>.<ext>`, where `<name>` is letters, digits and `_` (not starting with a digit):

| File | Rows |
|---|---|
| `countries.csv` | a header row, then the rows |
| `countries.xlsx`, `countries.xls` | the first sheet (or `sheet:`), header in the first row |
| `countries.json` | an array of objects |
| `countries.jsonl` | one object per line |
| `countries.yml` | a list of maps, or a map with `rows:` and the config below |

Column names must be usable unquoted in SQL (letters, digits and `_`).

## Config: `<name>.yml`

Without a config, **every value is text**, so arithmetic or a join on a number column needs a
cast, and on some databases (DuckDB, for one) comparing a text column with a number fails. A
config next to the data file gives columns types and says how the lookup is handed to SQL:

```yaml
# lookups/countries.yml, next to lookups/countries.csv
columns:
  code: string
  population: integer
  gdp_per_capita: number
  eu_member: boolean
  joined: date            # YYYY-MM-DD
sheet: Countries          # xlsx/xls only: which sheet (default: the first)
load: auto                # auto (default) | inline | temp_table
```

- `columns`: `string`, `integer`, `number`, `boolean` or `date`, per column. Columns you don't
  list stay text. An empty cell is `null`.
- `sheet`: for xlsx and xls.
- `load`: how `ref()` hands the lookup to SQL.
  - `inline`: `(select * from (values ...) as t(...))` in the SQL itself.
  - `temp_table`: the source plugin loads the rows into a temporary table on the report's
    session, and `ref()` names it.
  - `auto`: inline up to `lookup_inline_max_rows` rows (200 by default, set in
    `dre_project.yml`), a temp table above that. A source that can't load temp tables gets the
    rows inline, with a warning.

A `.yml` lookup holds its rows and config together:

```yaml
# lookups/tiers.yml
columns: {tier: string, min_spend: number}
rows:
  - {tier: bronze, min_spend: 0}
  - {tier: silver, min_spend: 1000}
  - {tier: gold, min_spend: 5000}
```

or, with no config, just the list:

```yaml
- {tier: bronze, min_spend: "0"}
- {tier: silver, min_spend: "1000"}
```

## Quoting

Inlined values are written as SQL literals that work on every engine DRE supports: a `'` inside
a value is built with `chr(39)` rather than `''` or `\'`, because Databricks doesn't read `''` as
an escaped quote. If you build literals from lookup values yourself in Jinja, see
[Databricks string literals](templates.md#string-literals-and-databricks).
