---
title: "Install"
description: "Install DRE with the install script, Homebrew, Scoop, pip or cargo."
sidebar:
  order: 2
---

# Install

macOS and Linux (x86_64 and ARM):

```bash
curl -fsSL https://getdre.com/install.sh | sh
```

This puts `dre` in `~/.local/bin`, after checking the download against the release's
`SHA256SUMS`. `DRE_INSTALL_DIR` picks another folder and `DRE_VERSION` a release (default: the
newest, pre-releases included):

```bash
curl -fsSL https://getdre.com/install.sh | DRE_VERSION=v0.1.2 DRE_INSTALL_DIR=/usr/local/bin sh
```

The same line works in a Databricks job (a cluster init script or a `%sh` cell), a CI runner or a
container build. On Windows, download `dre-<version>-windows-x86_64.zip` (or `-aarch64.zip`)
from [Releases](https://github.com/get-dre/dre/releases) and put `dre.exe` on your `PATH`.

## With Homebrew or Scoop

From 0.1.0 on:

```bash
brew install get-dre/tap/dre                                   # macOS and Linux
```

```powershell
scoop bucket add get-dre https://github.com/get-dre/scoop-bucket   # Windows
scoop install get-dre/dre
```

## With pip

The `dre-cli` package holds `dre` (Linux x86_64 and aarch64, macOS, Windows; Python 3.8+):

```bash
pip install dre-cli          # or: uv tool install dre-cli
dre --version
```

Each release attaches the wheels (`dre_cli-*.whl`), which install the same way with
`pip install <wheel URL>` before the version reaches PyPI. From Python, `dre_cli.run(["run",
"-s", "daily"])` runs `dre` and returns the finished process. In a Databricks job (serverless
included), add `dre-cli` to the job's environment dependencies and run `dre` from a script or
notebook.

## With cargo

```bash
cargo install dre-cli --locked          # builds `dre` from crates.io
```

A pre-release installs only when named: `cargo install dre-cli --locked --version 0.2.0-rc.1`.
