---
title: "Building from source"
description: "Build dre and the first-party plugins with cargo and Go."
sidebar:
  order: 19
---

# Building from source

Requires a recent stable Rust toolchain, and Go for the Databricks package.

```bash
cargo build --release
./target/release/dre --help
```

The first-party plugin packages are built from the same workspace
(`target/release/dre-plugin-*`), except the Databricks package in `go/databricks`:

```bash
cd go/databricks
go build -o ../../target/release/dre-plugin-databricks .
```

Put them in a project's `dre_deps/plugins/` (or point `DRE_PLUGINS_DIR` at them) to use them
without a registry.
