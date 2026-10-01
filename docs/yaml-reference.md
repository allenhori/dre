---
title: "YAML reference"
description: "Every key of every YAML file a DRE project uses, generated from the JSON Schemas."
sidebar:
  order: 18
---

# YAML reference

A DRE project is YAML and SQL files. Each kind of YAML file has a reference page that lists every
key with its type, default and meaning. The pages are generated from the same
[JSON Schemas](editor-setup.md) that give editors autocomplete, so they can't miss a key.

| File | What it holds | Reference |
|---|---|---|
| `dre_project.yml` | The project: default profile and output, variables, week settings, folder config | [dre_project.yml](reference-project.md) |
| `reports/**/*.yml` | A report: queries, output, destinations, Sets, templates | [Report YAML](reference-report.md) |
| `sets.yml` | Named variants a report can run as | [sets.yml](reference-sets.md) |
| `schedules.yml` | Named schedules your orchestrator fires | [schedules.yml](reference-schedules.md) |
| `profiles.yml` | Connections: sources and destinations (kept outside the project) | [profiles.yml](reference-profiles.md) |
| `dependencies.yml` | Plugin packages and macro packages | [dependencies.yml](reference-dependencies.md) |
| `lookups/<name>.yml` | The config of a lookup file | [Lookup config](reference-lookups.md) |

The options of each format and destination (for example `delimiter`, or an email's `to`) are
documented with their plugins in [Plugins](plugins.md); that reference comes from the plugins
themselves.

For editor autocomplete and validation, see [Editor setup](editor-setup.md).
