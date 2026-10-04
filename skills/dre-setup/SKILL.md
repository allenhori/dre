---
name: dre-setup
description: Set up DRE step by step - pick and install the database plugin (DuckDB, Postgres, Databricks), write the connection profile in ~/.dre/profiles.yml with a safe sign-in, add destinations, create a starter project with `dre new` and check it with `dre validate`. Use when the user wants to connect dre to a database, add or change a profile, set up dev and prod environments, or start a DRE project.
license: GPL-3.0-only
metadata:
  version: "2.1.0"
  dre: ">=0.2.1, <0.3.0"
---

# Set up a DRE connection and project

You take the user from an installed `dre` to a starter project that validates against a working
connection. You do what `dre init` does, one step at a time where the user can see it. The
result: a connection profile (and any destination profiles) in `~/.dre/profiles.yml`, a project made
by `dre new`, and `dre validate` passing.

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
   `>=0.1.0, <0.2.0`: any 0.1 release). A pre-release of the upper bound
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

- Existing profiles: names and `type`s only, from `~/.dre/profiles.yml` (or `$DRE_PROFILES_DIR`),
  e.g. `grep -nE '^  [A-Za-z0-9_-]+:|type:' ~/.dre/profiles.yml`. Never print the whole file.
- Installed plugins: `dre plugin list`.
- Whether the current folder is already a project (`dre_project.yml`).

If a usable profile and project already exist, say so and ask whether they want another
connection, a second environment, or to move on (`dre-report`).

### Step 3: the source

Tell the user up front never to paste a secret into the chat (SEC-1). Then ask which database
their reports read from:

| They have | Plugin (package) | Reference |
|---|---|---|
| Databricks SQL warehouse | `databricks` | `references/plugins/source-databricks.md` |
| PostgreSQL | `postgres` | `references/plugins/source-postgres.md` |
| A DuckDB file, or nothing yet (trying DRE out) | `duckdb` | `references/plugins/source-duckdb.md` |

Recommend `duckdb` for trying DRE out: it needs no server and no sign-in. DRE has no first-party
plugin for other databases yet; say so rather than guessing.

Read the chosen plugin's reference now; everything you write comes from it.

### Step 4: install the plugin

Confirm, then run `dre plugin install <package>`, e.g. `dre plugin install postgres`. Outside a
project it goes to `~/.dre/plugins`; a project installs its own copy on its first
`dre validate`. Check with `dre plugin list`, and compare the version with the range the
reference covers.

### Step 5: the connection fields

Go through the reference's **Profile fields** table. For each field:

- Required fields: ask for the value, unless it can be looked up (e.g. a Databricks host from
  `~/.databrickscfg`). Give a recommended value where one fits (`port` 5432, `sslmode`
  `require` for a server outside the user's machine).
- Optional fields: use the default, and mention only those that matter for this user (the
  reference's guide notes say which).
- **Secret fields** (the table's Secret column): never ask. Follow the reference's guide notes for
  the sign-in to recommend. Where a secret is needed, write `"{{ env_var('<NAME>') }}"` with a
  clear name (`PG_PASSWORD`, `WAREHOUSE_PASSWORD`), and tell the user how to set it themselves
  (SEC-3). Then check it's set without showing it (SEC-4).
- Never write a field the table doesn't list, except those the reference's docs section
  describes.

Recommend the profile name `warehouse` (it's `dre new`'s default) and the target `dev` (SET-2).

### Step 6: write the profile

Show the exact YAML you'll add and where, e.g.:

```yaml
connections:
  warehouse:
    targets:
      dev:
        type: postgres
        host: db.internal
        database: shop
        user: reports
        sslmode: require
        password: "{{ env_var('PG_PASSWORD') }}"
```

Then ask before writing. Rules:

- Add to `~/.dre/profiles.yml` (SET-1); create it with this content if it doesn't exist.
- Edit the file in place: add the profile at the end of the `connections:` section (or add the
  section), keeping every other line, comment and profile as it was. A file from DRE 0.1 has
  `sources:` instead: suggest renaming it to `connections:` (dre warns about it), and never have
  both. Keep a profile's own `target:` line if there is one: it's that profile's default entry.
- If a profile with that name already exists, don't overwrite it without an explicit yes; offer
  another name, or a new target inside it (SET-2).
- Never write a secret's value (SEC-3).

**Several environments** (dev and prod): add another target to the same profile, with its own
`env_var()` names. Each profile the run uses picks its entry: `--target`, else `DRE_TARGET` (either
sets every profile), else the profile's own `target:`, else `dev`. So a plain local run uses each
profile's default, and production runs set `DRE_TARGET=prod` or pass `--target prod` (SET-2). To
read production data locally, give the connection `target: prod` instead of passing a flag. A
used profile with no entry for its target is an error, so every destination needs a `dev` entry:
a dev location, or `dev: {deliver: false}` when it should deliver only from production. Never put
`target:` in `dre_project.yml` (DRE 0.2.1 removed it). Two Databricks workspaces, or two unrelated databases, are two profiles; a
report names the one it uses with `profile:` (a query or a source can name its own), or the
project's `default_profile`.

### Step 7: destinations (optional)

Ask whether reports should be delivered anywhere besides the local `target/run/` folder now, or
later (recommend later, once a report works, unless they already said). For each destination,
the same as steps 4 to 6, under `destinations:`, from its reference, and give it
`dev: {deliver: false}` unless it has a real dev location (SET-2):
`destination-s3.md`, `-gcs`, `-azure_blob` (package `object_store`), `-sftp`, `-ftp`,
`-databricks`, `-email`, `-slack`. A Databricks destination reuses the source's host and
sign-in.

### Step 8: the starter project

Ask for the folder name (recommend `my_reports`), confirm, and run
`dre new <folder> --type <package> --profile <profile>`, e.g.
`dre new my_reports --type postgres --profile warehouse`. It writes `dre_project.yml`,
`dependencies.yml` (the source's package and `csv`), a `hello` example report, and a
`.gitignore`. Add each destination's package under `plugins:` in `dependencies.yml`.

### Step 9: validate

In the project folder, run `dre validate`. It installs the declared plugins, checks the project
and compiles its SQL, without connecting. Explain what it printed. Then offer
`dre validate --live`, which connects and checks each statement without running it: the first
real test of the connection and sign-in (a Databricks browser sign-in opens here).

End with what was set up (profile, targets, project folder) and the next step: writing a report
(`dre-report`), or running the example (`dre-run`).

## The alternative: `dre init` in the user's own terminal

Offer this once, early: some people prefer a wizard. `dre init` is interactive, so the user runs
it in their own terminal, not through you. Coach them through its prompts:

1. **Source**: the number or name of their database.
2. **Profile name** (`warehouse`) and **target** (`dev`): Enter keeps them.
3. **Connection details**, one per field. Secret fields offer an `env_var()` reference: press
   Enter to keep it, and never type the secret there. For Databricks, `auth_type` offers `pat`;
   type `oauth` for a browser sign-in (SEC-2), or `auto` to use an existing Databricks CLI
   sign-in.
4. **Destinations**: numbers separated by commas, or Enter for none.
5. **Starter project**: `y`, then the folder.

Afterwards, carry on from step 9 here.

## If this fails

- **`dre plugin install` can't download:** the machine must reach GitHub (`github.com` and its
  release downloads). Behind a proxy, set `HTTPS_PROXY`. GitHub's anonymous rate limit on
  shared IPs: have the user set `GITHUB_TOKEN` themselves.
- **`unknown-profile` from `dre validate`:** the profile name in `dre_project.yml`
  (`default_profile`) or a report isn't under `connections:` in the profiles file `dre validate`
  names on its `Profiles` line. Fix the name, or check which `profiles.yml` it read (a project's
  own `profiles.yml` and `$DRE_PROFILES_DIR` come before `~/.dre`).
- **A YAML error in `profiles.yml`:** usually indentation or an unquoted `{{ env_var(...) }}`,
  which must be in quotes. Show the lines around the error (not secret values) and fix them.
- **"environment variable `X` is not set and no default is given":** the variable isn't in the environment `dre` runs in. Have the user
  set it (SEC-3) and start a new shell, then check it (SEC-4).
- **Connection errors from `dre validate --live`:** use the reference's docs section and guide
  notes for that plugin (host, port, TLS, sign-in). A Databricks warehouse that's stopped starts
  on the first query and can take minutes; DRE waits and says so.
- **Databricks sign-in with no browser** (a server, a CI runner): browser sign-in needs a person at
  the terminal. Use a Databricks CLI profile, `DATABRICKS_TOKEN`, or a service principal for
  unattended runs, as the reference's guide notes explain.
