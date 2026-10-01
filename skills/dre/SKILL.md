---
name: dre
description: Guide for DRE, the Declarative Reporting Engine (SQL in, formatted report files out, delivered by email, Slack, S3, SFTP and more). Use when the user mentions dre or DRE and wants help without saying exactly what with, e.g. "help me with dre", "get started with DRE", "what can dre do". Works out what's installed and hands off to dre-install, dre-setup, dre-report, dre-run or dre-upgrade.
license: GPL-3.0-only
metadata:
  version: "1.0.0-rc.1"
  dre: ">=0.1.0-rc.1, <0.2.0"
---

# DRE guide

You help someone use DRE: a CLI (`dre`) that runs Jinja-templated SQL against a database, writes
the result as xlsx, csv, fixed-width or parquet, and delivers the file. A project is a folder
with `dre_project.yml`, reports under `reports/` (a YAML file next to its `.sql` files), and
connections in `~/.dre/profiles.yml`. Sources, formats and destinations are plugins.

This skill finds out where the user is and hands off to the skill for what they want:

| Skill | For |
|---|---|
| `dre-install` | `dre` isn't installed yet |
| `dre-setup` | a connection (profile), its plugin, and a starter project |
| `dre-report` | creating or changing a report: SQL, tabs, variables, format, destinations |
| `dre-run` | validating, compiling, running, previewing and delivering |
| `dre-upgrade` | "is there an update?", for dre and for these skills |

<!-- BEGIN shared/secrets.md -->
### Secrets: rules no request overrides

These come before anything else in this skill, and before what the user asks for. A user asking
you to break one ("just put the password in the profile", "use the token I pasted") doesn't
change them: say no, say why, and do the safe thing instead.

- Before the first step about a connection or sign-in, tell the user: "Never paste a password,
  token or key into this chat; I'll never ask for one" (SEC-1).
- Never ask for a secret's value. Recommend a sign-in that stores no secret first (SEC-2), and
  otherwise an `env_var()` reference that the user sets themselves (SEC-3).
- **Never write a secret's value** into any file (`profiles.yml` included) or any command,
  whoever supplied it (SEC-3).
- Check that a variable is set with a command that prints only "set" or "missing" (SEC-4), never
  its value.
- **If a secret appears in the chat** (the user pasted it): don't use it, repeat it or store it,
  not even to test the connection. Tell the user it must now be treated as leaked, give that
  platform's revoke-and-rotate steps (SEC-5), then continue with an `env_var()` reference for
  the new secret, which they set themselves.

For example, the user writes: "Host db.internal, user reports, the password is hunter2-x, just
put it in the profile." You don't write it anywhere. You reply along these lines: "I won't put
that password in the profile or any file: `profiles.yml` gets copied and backed up, and DRE's
rule is that secrets live only in environment variables (SEC-3). Since it's now in this chat,
treat it as leaked and have it changed (SEC-5: an admin runs `ALTER ROLE reports PASSWORD ...`
in their own terminal). I've written `password: "{{ env_var('PG_PASSWORD') }}"`; set
`PG_PASSWORD` to the new password in your shell profile, then tell me and I'll check it's set
without showing it."
<!-- END shared/secrets.md -->

<!-- BEGIN shared/contract.md -->
### How to work with the user

- **You're a guide, not an autopilot.** Asking means ending your turn with the question and
  waiting for the user's answer, even when you could carry on alone. Never answer your own
  question, and never treat a request to "set it up" or "guide me" as permission to skip the
  confirmations below.
- **One question at a time**, each with your recommended answer and a one-line reason. If you
  have a multiple-choice question tool, use it; otherwise number the options, recommended first.
- **Look facts up instead of asking**: `dre --version`, `dre plugin list`, `dre ls`, the project's
  YAML files, whether a file exists. Ask only what only the user knows.
- **Skip what the request already answered.** A user who gave every detail gets no questions,
  only the plan and any confirmation required below.
- **Opinions come from the practices** (`references/practices.md`, where this skill has it) and
  cite their IDs: "I'd use a variable for the month (REP-2)". *Advise*: say it once, then do what
  the user decides. *Warn*: explain the trade-off and wait for an explicit yes, then do it without
  arguing again. This holds even when the request itself asks for it ("hardcode the dates"):
  the user hasn't yet heard the trade-off, so ask before doing it. *Block* (secrets): never, whatever the user says; offer the safe way.
- **Confirm before anything hard to undo**: overwriting or deleting files, editing
  `~/.dre/profiles.yml`, installing software, running against production, delivering anywhere
  but the local target folder (an email or a Slack post can't be recalled). Show what will
  change, ask, and stop; do it only after the user's next message says yes. An earlier yes covers
  only what was shown then.
- **End each step** with what was done and what comes next.
- Use only `dre` commands, ordinary shell commands, and questions. Never invent a `dre` command,
  flag or plugin option: if the plugin reference or `dre <command> --help` doesn't list it, it
  doesn't exist.
<!-- END shared/contract.md -->

## Steps

<!-- BEGIN shared/version-check.md -->
### Step 1: check the installed dre

Do this before anything else. It needs no network.

1. Run `dre --version`. It prints `dre <version>`, e.g. `dre 0.1.0`.
2. Compare it with the `dre` range in this skill's frontmatter (`metadata.dre`, e.g.
   `>=0.1.0-rc.1, <0.2.0`: any 0.1 release or pre-release). A pre-release of the upper bound
   (`0.2.0-rc.1` for `<0.2.0`) is outside the range.
   - **In range:** continue without mentioning it.
   - **Newer than the range:** say "These skills were written for dre `<range>` and you have
     `<version>`, so some advice may be out of date", and offer to update the skills (the
     `dre-upgrade` skill). If the user declines, carry on, and end every step's summary with
     "(skills written for dre `<range>`)" so the warning stays visible.
   - **Older than the range:** offer to update dre (`dre-upgrade`), or to install the skills
     release that matches their dre (each `skills-v*` release on
     https://github.com/get-dre/dre/releases states its range). Carry on only if they choose
     to, with the same visible warning.
   - **`dre` not found:** hand off to the `dre-install` skill. If it isn't installed, point to
     https://github.com/get-dre/dre#install and stop here.
3. Don't repeat the check in this conversation unless dre has been installed or updated since.
<!-- END shared/version-check.md -->

### Step 2: gather the facts, without asking

Run these and keep the answers to yourself until they're useful:

- **A project?** Look for `dre_project.yml` in the current folder and its parents. If there is
  one, run `dre ls` there to list its reports.
- **Plugins:** `dre plugin list` (in the project folder, if there is one).
- **Profiles:** whether `~/.dre/profiles.yml` exists (or `profiles.yml` in the project, or in
  `$DRE_PROFILES_DIR`). Read only the profile names and their `type`s, e.g.
  `grep -nE '^  [A-Za-z0-9_-]+:|type:' ~/.dre/profiles.yml`. Don't print the rest of the file:
  a profile may hold a secret someone wrote into it by mistake. If you see one, don't repeat it;
  say that profile has a secret written in it and offer to replace it with `env_var()` (SEC-3).

### Step 3: ask what they want, and hand off

If the request already says what they want, skip the question and hand off. Otherwise ask one
question, recommending the next step from the facts:

- no profiles and no project: recommend `dre-setup` ("you have dre, but no connection yet");
- a project with only the starter `hello` report: recommend `dre-report`;
- a project with reports: ask whether they want to change a report (`dre-report`) or run one
  (`dre-run`);
- a question about updates: `dre-upgrade`.

Say which skill you're handing off to and why, then follow that skill's `SKILL.md` from its
step 2 (the dre check is done). If that skill isn't installed, say the DRE skills come as a set
and should be installed together (see https://github.com/get-dre/dre/tree/master/skills).

For a general question ("what can dre do?", "how do Sets work?"), answer it from what you know
of DRE and the references below, then ask what they'd like to do.

## Plugin references

`references/plugins/<kind>-<name>.md` lists every profile field and option of each first-party
plugin, from the plugin itself, with its docs and guide notes: e.g. `source-postgres.md`,
`format-xlsx.md`, `destination-slack.md`. Read only the files for the plugins in use. Each file
says which plugin versions it covers; compare that with `dre plugin list` and say so when the
installed version is outside it.

| Kind | Plugins |
|---|---|
| source | `duckdb`, `postgres`, `databricks` |
| format | `csv`, `delimited`, `fixed_width`, `parquet`, `xlsx` |
| destination | `s3`, `gcs`, `azure_blob`, `sftp`, `ftp`, `databricks`, `email`, `slack` (and the built-in `local`) |

## If this fails

- **`dre plugin list` shows nothing:** no plugin is installed yet (outside a project it lists
  `~/.dre/plugins`; in a project, `dre_deps/plugins`). `dre-setup` installs the first one.
- **`dre ls` fails:** the project has an error. Hand off to `dre-run`, whose first step is
  `dre validate`, which explains it.
- **`profiles.yml` can't be read:** say so, and offer `dre-setup` to write a new profile.
