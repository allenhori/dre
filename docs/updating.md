---
title: "Updating DRE"
description: "dre system update, and how each install method updates."
sidebar:
  order: 15
---

# Updating DRE

`dre system update` checks GitHub Releases and updates DRE the way it was installed.
`dre system update --check` only reports whether a newer release exists, and
`dre system update <version>` installs that release (to pin one, or to roll back):

- **install.sh or the release zip**: `dre` downloads the new release, checks it against the
  release's `SHA256SUMS`, and replaces its own binary. A failed update leaves the old one as it
  was. Plugins aren't touched; `dre plugin update` updates those.
- **Homebrew or Scoop**: `dre` changes nothing and prints `brew upgrade dre` or `scoop update dre`.
- **pip, uv or pipx**: `dre` changes nothing and prints the command to run:
  `<python> -m pip install -U dre-cli`, `uv tool upgrade dre-cli` or `pipx upgrade dre-cli`.
  If `dre-cli` is pinned in a project's or a Databricks job's dependencies, bump the pin there.
- **cargo**: `dre` changes nothing and prints the `cargo install` command for the new version.
- **A build of your own** (`cargo build`, `cargo install --path`): `dre` refuses to overwrite it.

While every release is a pre-release, "newest" includes pre-releases. Once there are stable
releases, a stable `dre` updates to the newest stable one. Only `dre system update` checks for
new versions; no other command calls the network for it. `GITHUB_TOKEN` is sent when set, which
avoids GitHub's anonymous rate limit on shared IPs and CI runners.
