# Security policy

## Reporting a vulnerability

Please report a vulnerability privately, never in a public issue or discussion:

- through GitHub's [private vulnerability reporting](https://github.com/get-dre/dre/security/advisories/new), or
- by email to [security@getdre.com](mailto:security@getdre.com).

Include what you found, how to reproduce it, and the DRE version (`dre --version`). Please give us
a reasonable time to fix it before you share it publicly.

## Supported versions

Security fixes go into the newest release. From 0.1.0 on, a patch release (0.1.x) never breaks a
project, so updating to the newest patch is always safe.

## What to report

Anything that lets someone read or write what they shouldn't, run code they shouldn't, or learn a
secret: for example a secret leaking into a log, `run_results.json` or the manifest; a path that
escapes the target folder; or a weakness in how DRE downloads and verifies plugins or its own updates.
