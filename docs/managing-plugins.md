---
title: "Managing plugins"
description: "Plugin packages are installed on demand; how to have them in place before the first run."
sidebar:
  order: 10
---

# Managing plugins

However DRE is installed, only `dre` itself is. Every source, format and destination is a plugin
package with its own version and releases (`duckdb-v1.0.0`, `xlsx-v1.0.2`, ...): `dre init`,
`dre run`, `dre validate` and `dre compile` download the ones a project declares, from GitHub
Releases, and `dre.lock` pins them (see [the registry docs](registry.md)). A plugin's
version doesn't follow DRE's: any DRE runs any plugin release that speaks its
[protocol](protocol.md).

To have them in place before the first run, e.g. on a machine or job that starts empty each time:

- run `dre deps` in the project while building the image, or as its own CI or job step;
- or point `DRE_PLUGINS_DIR` at a folder that outlives the run (a Databricks Volume, a shared
  mount, a folder in the image) and run `dre deps` once to fill it.
