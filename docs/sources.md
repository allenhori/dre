---
title: "Sources"
description: "Declare the tables a project reads in dbt's sources format, read them with source(), and select and check by source."
sidebar:
  order: 4.4
---

# Sources

> **New in 0.2.** Sources declare the tables a project reads, as dbt's do, plus the
> connection they live on. See [Upgrading to 0.2](migrating-to-0.2.md).

A source is a set of tables in one schema of one system. Declare sources under a top-level
`sources:` key in any project YAML file (`sources/` is the usual folder). A dbt `sources.yml`
pastes in unchanged, `version: 2` included:

```yaml
# sources/shop.yml
version: 2

sources:
  - name: sales
    description: The shop's orders.
    profile: "{{ 'lakehouse' if target.name == 'prod' else 'warehouse' }}"   # DRE's addition
    schema: "{{ 'sales' if target.name == 'prod' else 'main' }}"
    tables:
      - name: orders
        identifier: raw_orders_v2     # the real table; SQL says `orders`
        columns:
          - {name: id, data_type: bigint}
          - {name: amount, data_type: "decimal(12,2)"}
      - name: customers
  - name: crm
    database: analytics
    profile: crm_pg
    quoting: {identifier: true}
    tables:
      - name: Accounts
```

```sql
select o.id, o.amount, c.name
from {{ source('sales', 'orders') }} o
join {{ source('sales', 'customers') }} c using (customer_id)
```

## What `source()` renders

- `identifier` defaults to the table's `name`, and `schema` to the source's `name`, as in dbt.
- With `database`, three parts (`database.schema.identifier`); without, two (`schema.identifier`),
  so one definition works on Postgres, DuckDB and Databricks.
- Unquoted by default. `quoting: {database, schema, identifier}` (on the source, or a table over
  it) quotes those parts with the connection's own quote character, which its plugin reports
  (`"` for DuckDB and Postgres, a backtick for Databricks), doubling it inside a name.
- `database`, `schema`, `identifier` and `profile` may use Jinja with `var()`, `env_var()`,
  `run.*` and `target.name` only (see [Jinja in profile values](connections.md#jinja-in-profile-values)).

`source()` works in query SQL, macros, `ref()`'d files and inside `run_query()`.

## The connection

A source's `profile` decides where a query using it runs: it overrides the report's, Set's,
folder's and project's default. A query's own `profile:` must agree with it, and one query can't
use sources on two connections; both are errors naming the two sides. A source with no
`profile` runs wherever its query runs, so sources can be used purely for naming (several Unity
Catalog catalogs on one connection, say). See
[Which connection a query runs on](connections.md#which-connection-a-query-runs-on). One source is
one system: tables on two systems are two sources.

## Keys

Supported: `name`, `description`, `database`, `schema`, `profile` (source only), `quoting`,
`tags`, `meta`, `tables` (`name`, `identifier`, `description`, `quoting`, `tags`, `meta`,
`columns`), and columns' `name`, `description` and `data_type`. Source names are unique; table
names are unique within a source.

dbt keys DRE doesn't use yet (`freshness`, `loaded_at_field`, `loader`, `data_tests`, `tests`,
`external`, `config`, `docs`, column `meta` and `tags`...) are accepted, and `dre validate` and the
run log note once per file that they're ignored. Any other key is an error. See the
[sources reference](reference-sources.md).

## Selecting by source

```bash
dre run -s source:sales            # every report a query of which reads a sales table
dre run -s source:sales.orders     # ...that reads sales.orders
dre validate -s source:sales.orders
dre ls -s source:sales --output json
```

`-s source:` works on `run`, `compile`, `validate` and `ls`, and combines with other selectors.
It follows what the parse pass found, so it includes reads through macros and `ref()`.

## Listing sources

```bash
dre ls --resource-type source
```

```
SOURCE           CONNECTION  RELATION                USED BY
crm.Accounts     crm_pg      analytics.crm.Accounts  pipeline
sales.customers  warehouse   main.customers          (unused)
sales.orders     warehouse   main.raw_orders_v2      daily, monthly
```

`(unused)` flags declarations no report reads. `--output json` gives the manifest's `sources`.

## Columns and `dre validate --live`

Columns are metadata: they're in the [manifest](manifest.md), and `dre validate --live` checks
them against the database. Each declared column must exist (compared case-insensitively) and,
with `data_type`, match what the database returns loosely. Tables with no declared columns aren't
checked. Columns never feed SQL generation; `columns()` still asks the database.

The comparison maps a declared SQL type to the Arrow types a plugin may return for it:

| Declared | Matches |
|---|---|
| `int`, `integer`, `bigint`, `smallint`, `tinyint`, `int2`/`int4`/`int8`, `hugeint`, `serial`... | any integer, `Decimal` |
| `float`, `double`, `real`, `float4`/`float8`, `double precision` | any float, `Decimal` |
| `decimal`, `numeric`, `number`, `money` | `Decimal`, floats, integers, `Utf8` |
| `varchar`, `char`, `text`, `string`, `uuid`, `nvarchar`, `bpchar`... | `Utf8`, `LargeUtf8`, `Utf8View` |
| `boolean`, `bool`, `bit` | `Boolean` |
| `date` | `Date32`, `Date64` |
| `timestamp`, `timestamptz`, `datetime`, `timestamp_ntz`... | `Timestamp`, `Date64` |
| `time`, `timetz` | `Time32`, `Time64` |
| `binary`, `varbinary`, `bytea`, `blob` | binary types |
| `interval` | `Interval`, `Duration` |
| `json`, `jsonb`, `variant`, `object` | text (JSON is returned as text) |
| `array`, `list`, `type[]` | lists |
| `struct`, `record`, `map` | `Struct`, `Map` |

Lengths and precisions (`varchar(20)`, `decimal(10,2)`) are ignored. A type not in the table is
reported as not comparable, and only the column's presence is checked.
