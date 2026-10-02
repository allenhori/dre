---
title: "Environment variables"
description: "Every DRE_* variable and what it does."
sidebar:
  order: 14
---

# Environment variables

| Variable | Effect |
|---|---|
| `DRE_PROFILES_DIR` | Directory holding `profiles.yml` (default: the project directory if it has one, else `~/.dre`). `--profiles-dir` overrides it. |
| `DRE_PLUGINS_DIR` | One plugins directory for every project, instead of each project's `dre_deps/plugins`. |
| `DRE_REGISTRY_URL` | The plugin package registry index (a URL or a local path). |
| `DRE_TARGET_PATH` | Where DRE writes its generated files (default: `target/` in the project). `--target-path` overrides it; it overrides `target_path:` in `dre_project.yml`. |
| `GITHUB_TOKEN` | Sent to GitHub by `dre system update` and `github:` plugin sources (private repositories, rate limits). |
| `DRE_GITHUB_API_URL` | The GitHub API for `github:` plugin sources and `dre system update` (GitHub Enterprise, a mirror). |
| `DRE_RUN_DATE` | The run date (`YYYY-MM-DD`) behind `run.date`, instead of today. |
| `DRE_TIMEZONE` | The run's timezone (IANA name), above every `timezone:` setting. `--timezone` overrides it. |
| `DRE_LOG_MAX_LINES` | Lines per `logs/dre.log` before it rotates (default 10,000). |
| `DRE_PLUGIN_HANDSHAKE_TIMEOUT_MS` | How long to wait for a plugin to start (default 30,000). |
| `NO_COLOR` | Turns off coloured output. |
| `DRE_SECRET_*` | Values are masked as `*****` in the console, logs, JSON events, the manifest, `run_results.json`, schema snapshots and `target/compiled/`. If a source rejects SQL containing a secret, its opaque error detail is omitted because databases may truncate or transform the value. `mask_secrets: false` in `dre_project.yml` turns masking off. Query result files are not altered. |
