---
name: dre-run
description: Validate, compile, run and preview DRE reports, check the output files, and deliver them - confirming before production runs or real deliveries - and explain errors when a run fails. Use when the user wants to run, test, preview or deliver a dre report, or asks why a dre run, validate or delivery failed.
license: GPL-3.0-only
metadata:
  version: "1.0.0-rc.1"
  dre: ">=0.1.0-rc.1, <0.2.0"
---

# Run, check and deliver DRE reports

You run reports the safe way round: check, preview, run locally, look at the output, and only
then deliver or run against production. Explain what each step shows, so the user learns the
workflow.

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

- The project root (`dre_project.yml`); `dre ls` for its reports, or `dre ls <report>` for one
  report's Bindings (a report paired with each of its Sets).
- What the user asked for: which reports, which Set (`--set`), which environment (`--target`),
  whether it should deliver.

Selecting: report names, `tag:<tag>` or folder names, as arguments (`dre run daily monthly`) or
with `-s`. `--set <name>` runs one Set, `--set all` every Set. `--var name=value` overrides a
variable for this run.

### Step 3: validate

Run `dre validate -s <report>`. It checks the project, compiles the SQL, checks every format and
destination option, and for each selected Binding shows the compiled files, the source and its
target, the output file, and every destination. Non-dev targets stand out. Read that list back
to the user: it's what a run will do.

`dre compile -s <report>` only renders the SQL into `target/compiled/` and lists the files, to
read the SQL exactly as the database will get it. `dre validate -s <report> --live` also checks
every statement against the database without running it.

### Step 4: preview

Run `dre run <report> --preview` (100 rows; `--preview 20` for fewer). It runs the queries with a
row limit and writes the output to `target/run/`, and **never delivers**. It's safe to run
without asking, unless the source is a production target.

### Step 5: look at the output

List what it wrote under `target/run/` and check it against what the user asked for:

- the files and their names;
- for xlsx, the sheet names in order and each sheet's header row (with Python and `openpyxl` if
  available, or ask the user to open it);
- for csv, delimited and fixed-width, the first lines (`head -5 <file>`), checking delimiter,
  quoting, header and record width;
- the row counts and a few values that should be right.

Say what matches and what doesn't. A mismatch goes back to `dre-report`.

### Step 6: the real run

A run without `--preview` writes the full output to `target/run/` and **delivers it to every
destination** in the report. Before any run that delivers anywhere but the local folder, or
reads a production target (`--target prod`, or a profile whose default target isn't dev),
confirm first (RUN-2):

1. show what will happen, from step 3's `dre validate -s <report>` (source, target, output,
   each destination with its recipients, channel or path);
2. ask for an explicit yes;
3. run `dre run <report>` with the same selection, Set and target.

If a report hasn't been through steps 3 to 5 and the user wants to schedule it or run it in
production, that's a warning (RUN-1): explain, and go ahead only after an explicit yes.

To run everything up to formatting without delivering, keep to `--preview`, or remove the
destination for the test.

### Step 7: read the result

The summary shows each Binding's status and each delivery's. `target/run_results.json` has the
detail (`deliveries` with `status` and `location`), and `logs/dre.log` the full SQL of every
statement. Report what was delivered where, and what failed.

End with what ran and what's next: fixing a failure, or scheduling (`dre run --schedule <name>`
from the user's orchestrator, with `DRE_RUN_DATE` for the logical date, RUN-3; a lasting
`--target-path` on ephemeral runners, RUN-4).

## If this fails

Read the error: DRE names the report, Binding, file and line. Then:

- **Connection or sign-in errors:** the source's reference (`references/plugins/source-<type>.md`)
  has the sign-in order and known errors. Databricks: a stopped warehouse is started and waited
  for (DRE says so every 30 seconds); with no one at the terminal, browser sign-in fails at once
  and lists what would work.
- **"environment variable `X` is not set":** the user sets it themselves (SEC-3), in the shell
  `dre` runs in; check with SEC-4.
- **SQL errors from the database:** show the compiled SQL (`target/compiled/`, or `logs/dre.log`)
  and the database's message; fix it in the report's `.sql` file (`dre-report`).
- **A column a format option names isn't in the query** (xlsx `columns:`, a formula's `{name}`):
  the error names the sheet and column; fix the option or the SQL.
- **A number format doesn't fit its column** (a date code on a number): fix the format, or cast
  in SQL.
- **Postgres numbers arrive as text:** an unconstrained `numeric`; cast to `numeric(18,2)`.
- **The output's schema changed since the last successful run:** the run stops before delivering.
  Find out why (RUN-5) before running again with `--accept-schema-change`.
- **A delivery failed:** the other destinations were still attempted, the output stays in
  `target/run/`, and the run exits non-zero. Use the destination's reference for its known
  errors: email (no recipients, attachment too large), Slack (bot not in the channel, a missing
  scope, the DM tab turned off), SFTP (unknown host key), FTP (path relative to the login
  folder), object storage (credentials, bucket, region).
- **A plugin can't be installed or started:** `dre deps` installs the project's plugins and says
  what failed; the machine must reach GitHub.
