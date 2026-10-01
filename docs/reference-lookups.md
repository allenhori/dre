---
title: "Lookup config reference"
description: "Every key of a lookup's config file."
sidebar:
  order: 26
---

# Lookup config reference

<!-- Generated from docs/schemas by .github/scripts/schema_docs.py. Edit the schema, not this page. -->

The optional config of a lookup, `lookups/<name>.yml` next to a csv, xlsx, xls, json or jsonl file. (A `.yml` lookup that holds the rows themselves can also set `rows`.)

Where: `lookups/<name>.yml`, next to the lookup's data file.

For editor autocomplete and validation, add this as the first line of the file ([editor setup](editor-setup.md)):

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.1/lookup.schema.json
```

## Keys

| Key | Type | Default | Description |
|---|---|---|---|
| `columns` | map |  | Types for columns; without it every value is text. |
| `sheet` | string |  | For xlsx and xls lookups: the sheet to read. Default: the first. |
| `load` | `auto` or `inline` or `temp_table` | `auto` | How the lookup reaches the database: inlined into the SQL, loaded into a temp table, or chosen by size (`auto`). |
| `rows` | list of map |  | For a `.yml` lookup that holds its own data: the rows. |
