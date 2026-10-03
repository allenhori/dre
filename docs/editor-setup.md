---
title: "Editor setup"
description: "Autocomplete and validation for DRE's YAML files in VS Code and JetBrains, from the JSON Schemas."
sidebar:
  order: 21
---

# Editor setup

DRE publishes a [JSON Schema](https://json-schema.org) for every YAML file a project uses. With
one comment line at the top of a file, your editor completes keys, shows each key's description
and underlines mistakes before you run `dre validate`.

`dre new` writes the line into the files it creates. For other files, add the line for their
kind (the [YAML reference](yaml-reference.md) lists them):

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.2/report.schema.json
```

The URL carries DRE's minor version (`v0.2`). A patch release never changes what a schema
accepts, so `v0.2` always serves the newest 0.2.x schema. Use the minor version of the DRE you
run. The schemas are also in the `dre` repository, in `docs/schemas/`, at every release tag.

## VS Code

Install the [YAML extension](https://marketplace.visualstudio.com/items?itemName=redhat.vscode-yaml)
from Red Hat. It reads the `# yaml-language-server: $schema=...` line.

To apply a schema to every file of a kind without a comment line, map them in your settings:

```json
{
  "yaml.schemas": {
    "https://getdre.com/schemas/v0.2/report.schema.json": "reports/**/*.yml",
    "https://getdre.com/schemas/v0.2/project.schema.json": "dre_project.yml",
    "https://getdre.com/schemas/v0.2/dependencies.schema.json": ["dependencies.yml", "packages.yml"],
    "https://getdre.com/schemas/v0.2/schedules.schema.json": "schedules.yml",
    "https://getdre.com/schemas/v0.2/timings.schema.json": "timings.yml",
    "https://getdre.com/schemas/v0.2/sets.schema.json": "sets.yml",
    "https://getdre.com/schemas/v0.2/sources.schema.json": "sources/*.yml"
  }
}
```

## JetBrains IDEs

IntelliJ IDEA, PyCharm and the other JetBrains IDEs understand JSON Schemas natively, but they
don't read the comment line. Open **Settings → Languages & Frameworks → Schemas and DTDs → JSON
Schema Mappings**, add a schema with the URL above, and map it to the files it applies to.

## What the schemas check, and what they don't

The schemas check the shape of a file: which keys exist, their types, allowed values and
required keys. `dre validate` checks the rest: that queries and profiles exist, that SQL compiles,
that options match the plugin that handles them. Options of a format or a destination
(`delimiter`, `to`, `subject`, ...) belong to their plugin, so the schemas let them through;
they are documented in [Plugins](plugins.md).

The schemas describe the shape of the files, and `dre validate` stays the authority. When they
disagree, `dre validate` is right; please [report it](https://github.com/get-dre/dre/issues).
