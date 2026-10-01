---
title: "profiles.yml reference"
description: "Every key of profiles.yml: sources, destinations and their targets."
sidebar:
  order: 24
---

# profiles.yml reference

<!-- Generated from docs/schemas by .github/scripts/schema_docs.py. Edit the schema, not this page. -->

Connections, in `profiles.yml`: database sources and delivery destinations. Kept outside the project (`~/.dre`, `--profiles-dir` or `DRE_PROFILES_DIR`).

Where: `profiles.yml`.

For editor autocomplete and validation, add this as the first line of the file ([editor setup](editor-setup.md)):

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.1/profiles.schema.json
```

## Keys

| Key | Type | Default | Description |
|---|---|---|---|
| `sources` | map |  | Database connections, referenced by `default_profile`, `profile:` and Sets. |
| `destinations` | map |  | Delivery targets, referenced by `output.destination.profile`. |

## `sources.<name>`

A named connection with one or more targets (environments).

| Key | Type | Default | Description |
|---|---|---|---|
| `target` (required) | string |  | The default target, one of `targets`. |
| `targets` (required) | map |  | The environments of this profile, by name (e.g. `dev`, `prod`). |

## `sources.<name>.targets.<name>`

One target of a profile: a connection or delivery configuration. `type` names the plugin; every other key is a field of that plugin (see the plugins reference), and secrets belong in `{{ env_var('NAME') }}`.

| Key | Type | Default | Description |
|---|---|---|---|
| `type` (required) | string |  | The plugin type of the connection, e.g. `duckdb`, `postgres`, `databricks`, `sftp`, `s3`. `local` needs no plugin. |
| _other keys_ | | | Options of the plugin that handles this block; see [Plugins](plugins.md). |
