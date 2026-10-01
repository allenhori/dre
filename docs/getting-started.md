---
title: "Getting started"
description: "Start with a coding agent or from the command line."
sidebar:
  order: 1
---

# Getting started

Choose the path you want to use. Both create the same plain YAML and SQL project.

## With a coding agent

DRE is built to be driven by coding agents as well as people: everything is plain YAML and SQL
files, every command but `dre init` runs without prompts, and `--log-format json`, `dre validate --json`,
`dre ls --output json` and the [manifest](manifest.md) give tools machine-readable output.

DRE's [agent skills](../skills/README.md) turn Claude Code, Codex, Cursor, GitHub Copilot, Gemini CLI
or any agent that reads Agent Skills into a guide, from installing DRE to a delivered report. They
ask one question at a time with a recommendation, follow [DRE's practices](practices.md),
confirm before anything is delivered, and never ask for your passwords or tokens.

```bash
claude plugin marketplace add get-dre/dre && claude plugin install dre@dre   # Claude Code
npx skills add get-dre/dre#skills-latest                                     # other agents
```

Then ask: "help me with dre".

## From the command line

1. [Install DRE](install.md) for your platform.
2. Create a starter project:

   ```bash
   dre init
   cd my_reports
   ```

3. Check that the project works before editing it:

   ```bash
   dre validate
   ```

A successful validation means the project configuration and SQL compile. Continue with
[Build and run reports](building-reports.md) to add reports, preview output and deliver files.
