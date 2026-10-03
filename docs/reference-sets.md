---
title: "sets.yml reference"
description: "Every key of a Sets file."
sidebar:
  order: 24
---

# sets.yml reference

<!-- Generated from docs/schemas by .github/scripts/schema_docs.py. Edit the schema, not this page. -->

Set names mapped to their definitions.

Where: `sets.yml`, or any YAML file of Sets.

For editor autocomplete and validation, add this as the first line of the file ([editor setup](editor-setup.md)):

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.2/sets.schema.json
```

The file maps names to entries; each entry has these keys.

## Keys

| Key | Type | Default | Description |
|---|---|---|---|
| `profile` | string |  | The connection a Binding with this Set runs on. May use Jinja with `var()`, `env_var()`, `run.*` and `target.name`. |
| `vars` | map |  | Variables, read in SQL and YAML with `var('name')`. Values can be strings, numbers, booleans, lists or maps. |
