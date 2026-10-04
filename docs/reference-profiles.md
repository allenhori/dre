---
title: "profiles.yml reference"
description: "Every key of profiles.yml: connections, destinations and their targets."
sidebar:
  order: 27
---

# profiles.yml reference

<!-- Generated from docs/schemas by .github/scripts/schema_docs.py. Edit the schema, not this page. -->

Connections and destinations, in `profiles.yml`: what reports read from and where outputs go. Kept outside the project (`~/.dre`, `--profiles-dir` or `DRE_PROFILES_DIR`). Every profile lists its targets (environments). Each profile the run uses picks one: `--target`, else `DRE_TARGET` (either sets every profile), else the profile's own `target`, else `dev`; a used profile without that entry is an error.

Where: `profiles.yml`.

For editor autocomplete and validation, add this as the first line of the file ([editor setup](editor-setup.md)):

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.2/profiles.schema.json
```

## Keys

| Key | Type | Default | Description |
|---|---|---|---|
| `connections` | map |  | Database connections, referenced by `default_profile`, `profile:` (report, query, Set, folder `+profile`) and a source's `profile`. |
| `sources` | map |  | DRE 0.1's name for `connections:`. Still read in 0.2.x, with a warning: rename it to `connections:`. |
| `destinations` | map |  | Delivery targets, referenced by `output.destination.profile`. |

## `connections.<name>`

A named connection with one entry per target (environment).

| Key | Type | Default | Description |
|---|---|---|---|
| `target` | string |  | This profile's entry when neither `--target` nor `DRE_TARGET` is set (dbt's key). Default: `dev`. It doesn't change `target.name`, the run's target. |
| `targets` (required) | map |  | The environments of this connection, by name (e.g. `dev`, `prod`). |

## `connections.<name>.targets.<name>`

One target of a profile: a connection or delivery configuration. `type` names the plugin; every other key is a field of that plugin (see the plugins reference), and secrets belong in `{{ env_var('NAME') }}`.

| Key | Type | Default | Description |
|---|---|---|---|
| `type` (required) | string |  | The plugin type of the connection, e.g. `duckdb`, `postgres`, `databricks`, `sftp`, `s3`. `local` needs no plugin. |
| _other keys_ | | | Options of the plugin that handles this block; see [Plugins](plugins.md). |

## `destinations.<name>`

A named destination with one entry per target (environment).

| Key | Type | Default | Description |
|---|---|---|---|
| `targets` (required) | map |  | The environments of this destination, by name (e.g. `dev`, `prod`). An entry is a delivery configuration, or `{deliver: false}` to deliver nowhere on that target. |
| `target` | string |  | This profile's entry when neither `--target` nor `DRE_TARGET` is set (dbt's key). Default: `dev`. It doesn't change `target.name`, the run's target. |

## `destinations.<name>.targets.<name>`

A destination entry is a delivery configuration (`type` and the plugin's fields, as for a connection), or `{deliver: false}`: it deliberately delivers nowhere. The output stays in the target path, the run logs it, and `run_results.json` records the delivery as `not_delivered`. Destinations only.

| Key | Type | Default | Description |
|---|---|---|---|
| `deliver` (required) | `false` |  | `false`: this target delivers nowhere. Takes no other keys. |
