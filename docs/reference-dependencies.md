---
title: "dependencies.yml reference"
description: "Every key of dependencies.yml: plugin packages and macro packages."
sidebar:
  order: 25
---

# dependencies.yml reference

<!-- Generated from docs/schemas by .github/scripts/schema_docs.py. Edit the schema, not this page. -->

Plugin packages and macro packages, in `dependencies.yml` or `packages.yml` at the project root.

Where: `dependencies.yml` or `packages.yml`.

For editor autocomplete and validation, add this as the first line of the file ([editor setup](editor-setup.md)):

```yaml
# yaml-language-server: $schema=https://getdre.com/schemas/v0.1/dependencies.schema.json
```

## Keys

| Key | Type | Default | Description |
|---|---|---|---|
| `plugins` | list of plugin packages: a name, `name: "<version>"`, or a map (see below) |  | The plugin packages this project uses. DRE installs them on demand into `dre_deps/` and pins them in `dre.lock`. May be written in any project YAML file; `dependencies.yml` is the usual place. |
| `packages` | list of any |  | Macro packages (git or local). Their macros are called through the package name, e.g. `{{ dre_utils.star(...) }}`. Exact commits are pinned in `dre.lock`. |

## `plugins[]`

A package with its source.

| Key | Type | Default | Description |
|---|---|---|---|
| `name` (required) | string |  | The package name: lowercase letters, digits and `_`. |
| `version` | string |  | A version constraint such as `1.2.0` or `>=1.0`. Not allowed with `local`. |
| `github` | string |  | Install from the releases of this GitHub repository, `owner/repo`. |
| `local` | string |  | Use the package folder at this path as it is. |
| `registry` | string |  | Install from this registry index (a URL or a path) instead of the default one. |

## `packages[]`

A macro package, from git or from a folder.

| Key | Type | Default | Description |
|---|---|---|---|
| `git` | string |  | The git URL of the package. Needs `revision`. |
| `revision` | string |  | A tag, branch or commit of the git package. |
| `local` | string |  | The path of a package folder in this project's tree. |
