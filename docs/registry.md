---
title: "Plugin packages, the registry and `dre.lock`"
description: "Plugin packages, the registry index, dre.lock and macro packages."
sidebar:
  order: 11
---

# Plugin packages, the registry and `dre.lock`

Plugins come in **packages**: one download, one executable, serving every plugin the package
provides. The `databricks` package is the Databricks source and the Databricks destination;
`object_store` is the `s3`, `gcs` and `azure_blob` destinations; `csv` is the `csv` and
`delimited` formats. A project declares the packages it needs, once each, under `plugins:` in
`dependencies.yml` (or any project YAML file):

```yaml
plugins:
  - duckdb
  - databricks        # the databricks source and destination
  - object_store      # s3, gcs, azure_blob
  - csv: ">=0.0.1-alpha"
```

Every plugin a declared package provides can then be used as a profile's `type:` or an output's
`format:`. One that no declared package provides is an error (`undeclared-plugin`), which names
the package to add when DRE's registry has one. DRE only ever installs the packages a project
declares; it never infers them from a `type:`.

## The registry

DRE installs packages from a static JSON index. The index is a plain file, hosted alongside
GitHub Releases, and there's no registry service to run. The default location is:

```
https://github.com/get-dre/dre/releases/download/registry/packages.json
```

`DRE_REGISTRY_URL` points DRE somewhere else: an `https://` URL, a `file://` URL or a plain file
path. An internal mirror and an offline copy both work this way. (`index.json`, next to it, is
the index DRE 0.0.1-alpha-6 and earlier read. It lists the plugins as they were then and isn't
updated any more.)

### Index format

```json
{
  "schema": 2,
  "plugins": [
    {
      "name": "object_store",
      "description": "Amazon S3, Google Cloud Storage and Azure Blob Storage",
      "provides": ["destination/s3", "destination/gcs", "destination/azure_blob"],
      "versions": [
        {
          "version": "1.2.0",
          "protocol": 0,
          "artifacts": {
            "macos-aarch64":  { "url": "https://…/dre-plugin-object_store-1.2.0-macos-aarch64.tar.gz", "sha256": "…" },
            "linux-x86_64":   { "url": "https://…/dre-plugin-object_store-1.2.0-linux-x86_64.tar.gz",  "sha256": "…" },
            "windows-x86_64": { "url": "https://…/dre-plugin-object_store-1.2.0-windows-x86_64.exe",   "sha256": "…" }
          }
        }
      ]
    }
  ]
}
```

- `name` is the package name (`[a-z0-9_]+`), the name a project declares under `plugins:`.
- `provides` lists its plugins as `<kind>/<name>`: `kind` is `source`, `format` or
  `destination`, and `name` is the string used in `profiles.yml` `type:` or `output.format`.
- `version` is semver. A pre-release is installed when asked for explicitly
  (`dre plugin install duckdb@=1.3.0-rc.1`), or when no stable version matches: a package whose
  only releases are `0.0.1-alpha` installs that, and one with a stable `0.1.0` ignores a later
  `0.2.0-rc.1` until asked. Newest means semver order, except that numbers inside a pre-release
  label compare as numbers: `0.0.1-alpha-10` is newer than `0.0.1-alpha-9` (plain semver
  compares `alpha-10` as text and puts it first).
- `protocol` is the plugin protocol version the release speaks (see [protocol.md](protocol.md)).
  DRE skips versions it can't talk to.
- Artifacts are keyed by platform, `<os>-<arch>`, using Rust's names: `macos`, `linux` or
  `windows`, and `x86_64` or `aarch64`.
- An artifact is either the package's executable itself, or a `.tar.gz` containing it as
  `dre-plugin-<package>` (plus `.exe` on Windows).
- `sha256` is the hex SHA-256 of the downloaded file. A download that doesn't match is discarded
  and the install fails. Signatures come after V1.

An index in the earlier schema 1 still works: each entry there is one plugin, with `kind` and
`name` in place of `provides`, and reads as a package of that one plugin, named after it. Its
artifacts hold `dre-<kind>-<name>`.

## Publishing a release

Each first-party package has its own version, independent of DRE's: `plugins/<package>/Cargo.toml`,
or `go/databricks/VERSION` for the Databricks package. Compatibility between DRE and a package
comes from the [protocol](protocol.md) version, never from matching numbers: DRE 0.1.0 runs
`duckdb` 1.0.0 and whatever `duckdb` releases later that speak the same protocol.

A package is released when it changes, by pushing a `<package>-v<version>` tag, e.g.
`duckdb-v1.0.1` or `object_store-v1.2.0-rc.1`. `.github/workflows/release.yml` checks the tag
against the package's version, builds that package alone for each platform, publishes it as a
GitHub Release (a pre-release when the version has a suffix such as `-rc.1`), and adds it to
`packages.json` on the `registry` release, which it creates the first time. A `v<version>` tag
releases DRE itself and touches no package. What each first-party package provides, and its
description, is in `.github/scripts/packages.json`.

The index update adds every package release it doesn't list yet, not only the one just made, so
several packages can be tagged at once. `gh workflow run release.yml -f tag=registry` runs it on
its own.

For a package released some other way:

1. Build the executable for every platform and upload the artifacts to a GitHub Release.
2. Compute each artifact's checksum with `shasum -a 256 <file>`.
3. Add a `versions` entry (version, protocol, one artifact per platform) to the index.
4. Upload the updated index to your registry location, replacing the old one.

Packages are versioned independently of DRE core. A package release never needs a core release.

## Where packages are installed

A project's packages live inside it, in `dre_deps/plugins/` (gitignore `dre_deps/`), next to its
macro packages in `dre_deps/packages/`. Versions sit side by side, each with a `plugin.json`
naming the executable and the plugins it provides:

```
my_reports/dre_deps/plugins/object_store/1.2.0/dre-plugin-object_store
my_reports/dre_deps/plugins/object_store/1.2.0/plugin.json
my_reports/dre_deps/plugins/object_store/1.3.0/...
```

Downloads go to a shared cache, `~/.dre/plugins`, and are hard-linked into each project (copied
when the cache is on another disk), so ten projects using DuckDB store it once. A project whose
`dre.lock` pins a version already in the cache links it without contacting the registry.

`DRE_PLUGINS_DIR` replaces both: every project uses, and installs into, that one directory. An
executable placed by hand directly in a plugins directory is used when no installed version is
pinned or matches: `dre-plugin-<package>` (DRE asks it what it provides), or
`dre-<kind>-<name>` for a package of that one plugin, called `<name>`. This is meant for
developing a plugin.

## `dre.lock`

`dre.lock` records the exact version of every package a project resolved, what it provides, and
the checksum of its build for each platform. Commit it, so every machine and CI runner uses the
same builds, whether it's a Mac, Linux or a Databricks job.

```yaml
# Generated by DRE: the exact plugin versions and package commits this project uses. Commit this file.
plugins:
  csv:
    version: 1.0.4
    sha256:
      linux-x86_64: 51b8…
      macos-aarch64: 9a01…
    provides:
    - format/csv
    - format/delimited
  duckdb:
    version: 1.2.0
    sha256:
      linux-aarch64: 77d2…
      linux-x86_64: 0c4e…
      macos-aarch64: 3f5c…
    provides:
    - source/duckdb
packages:
  dre_utils:
    git: https://github.com/acme/dre_utils.git
    revision: v1.0.0
    commit: 4be1c0…
```

How the commands use it:

- **`dre run`, `dre validate` and `dre compile`**: install whatever the project declares but
  hasn't installed, before they start, and log each install. Nothing needs to run first.
- **`dre deps`**: installs every declared package too, and also resolves again any package the
  lock doesn't pin, so it picks up the newest version allowed. Use it to refresh `dre.lock` on
  purpose, or to install everything in a separate CI or image-building step.
- **`--no-auto-install`**: nothing is downloaded. A missing package is a hard failure for
  `dre run`, and a warning for `dre validate`; what a package provides then comes from what's
  installed and from `dre.lock`.
- **`dre plugin install <package>[@req]`**: installs one package, respecting the project's
  declared constraint and pin, and pins the result when the project declares that package.
- **`dre plugin update <package>`**: installs the newest version the constraint allows and
  re-pins it.
- **`dre plugin remove <package>[@version]`**: removes installed versions, and drops the pin if
  the pinned version is removed.
- **`dre plugin list`**: the installed packages, what each provides, and their versions.

The checksums are the ones the registry publishes for every platform of that version. When a
source publishes none (a GitHub release without `.sha256` files), each platform's first
download is recorded, and later downloads on that platform must match it. A `dre.lock` from
before per-platform checksums has a single `sha256:` value: it's accepted when it matches the
registry's checksum for any platform of that version, and rewritten in the new form.

## Other places to install from

A `plugins:` entry can name where a package comes from instead of the default registry. Bare
names and `name: "<version>"` keep working; the map form adds a source:

```yaml
plugins:
  - duckdb                                              # the default registry
  - {name: foo, github: acme/dre-foo, version: ">=1.2, <2"}
  - {name: bar, local: ../dre-bar/target/release/dre-plugin-bar}
  - {name: baz, registry: https://plugins.acme.internal/packages.json, version: "^1"}
```

- **`github: owner/repo`**: the repo's GitHub Releases. A release tagged `v1.2.0` (or `1.2.0`)
  offers version 1.2.0 for every platform it has an asset for, named like the registry's
  artifacts: `dre-plugin-<name>-<version>-<os>-<arch>.tar.gz`, or the bare executable (`.exe` on
  Windows). A single plugin's release named `dre-<kind>-<name>-<version>-<os>-<arch>` works too.
  Drafts and tags that aren't versions are skipped. DRE asks the installed executable what it
  provides. The checksum comes from a `<asset>.sha256` file in the same release; without one,
  the first download's checksum is pinned in `dre.lock` and every later download must match it.
  `GITHUB_TOKEN` is sent when set (private repos, rate limits), and `DRE_GITHUB_API_URL` points
  at GitHub Enterprise.
- **`local: <path>`**: an executable on disk, relative to the project root, used where it is and
  never copied. It has no version. This is the way to try a package you're developing.
- **`registry: <url or path>`**: another index in the [format above](#index-format), for this
  package only.

`dre.lock` records where each package came from (`from: github:acme/dre-foo`); local packages go
under `local:`. When an entry's source changes, the old pin no longer counts and the package is
resolved again. `dre plugin install` and `update` use the declared source, and `update` has
nothing to do for a local package. A package declared in several files must name the same source
in each.

Before packages, projects declared plugins one by one under `sources:`, `formats:` and
`destinations:`. Those blocks are now an error that says to use `plugins:`.

## Macro packages

A macro package is a folder with a `dre_package.yml` (`name: dre_utils`) and a `macros/` folder.
Declare packages under `packages:` in `dependencies.yml` or `packages.yml` at the project root
(either file, or both):

```yaml
packages:
  - git: https://github.com/acme/dre_utils.git
    revision: v1.0.0            # a tag, branch or commit
  - local: ../shared/finance_macros
```

- **`dre deps`** (and `dre run`/`dre validate`, unless `--no-auto-install`) clones git packages
  into `dre_deps/packages/<name>` and pins the commit in `dre.lock`. A later `dre deps` reinstalls
  that exact commit until the declared `revision` changes. `local` packages are read in place.
- **Calling**: package macros are called through the package's name,
  `{{ dre_utils.star_except('customers', ['client']) }}`. The project's own macros keep plain
  names, so a local `star_except` and `dre_utils.star_except` never collide.
- **Per-database variants**: a package macro can hand over to `dispatch('name', 'dre_utils')`,
  which picks `<source type>__name` (e.g. `databricks__name`) or else `default__name`, looking in
  the project first and then the package. A project overrides a package's variant by defining
  the same `<source type>__name` or `default__name` in its own `macros/`. `dispatch:` in
  `dre_project.yml` changes the search order:

  ```yaml
  dispatch:
    - macro_namespace: dre_utils
      search_order: [dre_utils]        # ignore project overrides
  ```

- **Trust**: package macros can query your database through `run_query()`, so install packages
  you trust; `dre.lock` makes sure you keep getting the commit you reviewed.
- Registry packages (`package: dre_utils`) aren't available yet.
