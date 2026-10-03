---
title: "profiles.yml reference"
description: "Every key of profiles.yml: connections, destinations and their targets."
sidebar:
  order: 27
---

# profiles.yml reference

<!-- Generated from docs/schemas by .github/scripts/schema_docs.py. Edit the schema, not this page. -->

Connections and destinations, in `profiles.yml`: what reports read from and where outputs go. Kept outside the project (`~/.dre`, `--profiles-dir` or `DRE_PROFILES_DIR`). Every profile lists its targets (environments); the run picks one for all of them: `--target`, `DRE_TARGET`, `target` in dre_project.yml, else `dev`.

Where: `profiles.yml`.

For editor autocomplete and validation, add this as the first line of the file ([editor setup](editor-setup.md)):

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.1/profiles.schema.json
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
| `targets` (required) | map |  | The environments of this profile, by name (e.g. `dev`, `prod`). |
| `target` | string |  | Ignored since DRE 0.2 (with a warning): the run picks one target for every profile. |

## `connections.<name>.targets.<name>`

One target of a profile: a connection or delivery configuration. `type` names the plugin; every other key is a field of that plugin (see the plugins reference), and secrets belong in `{{ env_var('NAME') }}`.

| Key | Type | Default | Description |
|---|---|---|---|
| `type` (required) | string |  | The plugin type of the connection, e.g. `duckdb`, `postgres`, `databricks`, `sftp`, `s3`. `local` needs no plugin. |
| _other keys_ | | | Options of the plugin that handles this block; see [Plugins](plugins.md). |
