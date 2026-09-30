# DRE practices

The opinions DRE's agent skills give, and the reasons behind them. Every skill takes its advice
from this file and cites rules by ID (`SEC-1`, `REP-2`, ...), so you can look up the full
reasoning. They're good advice for people, too.

Each rule has a level:

- **advise**: the guide says "I'd do X, because Y" once, then does what you decide.
- **warn**: the guide explains the trade-off and needs your explicit "yes, do it anyway" before
  going ahead. Then it does what you decided, without arguing again.
- **block**: the guide won't do it, whatever you say, and offers the safe way instead. Only
  secrets are at this level.

## Secrets

Secrets are passwords, tokens, access keys, client secrets, connection strings with keys in them,
private keys and their passphrases. The rules hold for every source and destination.

### SEC-1: Never ask for a secret, and say so up front

**Level:** block

Before any step that involves a sign-in, the guide says: "Never paste a password, token or key
into this chat. I'll never ask for one; you'll set it yourself where I can't see it." It never
asks for a secret's value, in any form ("paste the token", "what's the password?").

*Why:* anything typed into a chat can end up in the agent's logs, history and provider, outside
your control. A secret that never enters the chat can't leak from it.

*Example:* setting up Postgres, the guide asks for the host, database and user, then says it'll
write `password: "{{ env_var('PG_PASSWORD') }}"` and how you set `PG_PASSWORD` yourself.

### SEC-2: Prefer a sign-in that stores no secret

**Level:** block (the order below is advice; storing a secret where a sign-in would do is not)

Recommend, in this order:

1. **A platform sign-in that stores no secret in DRE's files.**
   - Databricks: `auth_type: oauth` (a browser sign-in; `dre` keeps the session in
     `~/.dre/oauth_sessions.json`, readable only by you), or a `databricks auth login` session or
     `~/.databrickscfg` profile, which the default `auth_type: auto` finds.
   - S3: the AWS credential chain, e.g. `aws sso login` (leave the key fields out).
   - GCS: application default credentials, `gcloud auth application-default login` (leave the key
     fields out).
   - Azure Blob: `use_azure_cli: true` after `az login`, or `use_managed_identity: true` on Azure.
   - SFTP: a key pair (`private_key_path`), with its passphrase in an environment variable if it
     has one.
2. **An environment variable read with `env_var()`**, set by you (SEC-3), for what has no sign-in:
   Postgres and FTP passwords, SMTP passwords, Slack bot tokens, a Databricks service principal's
   secret for a scheduler.

*Why:* a sign-in expires and can be revoked centrally, and there's nothing to leak. A long-lived
secret in a file is the most common way credentials escape.

### SEC-3: Secrets only through `env_var()`, set by you

**Level:** block

A profile field that holds a secret is always written as an `env_var()` reference, e.g.
`token: "{{ env_var('SLACK_BOT_TOKEN') }}"`, never as the value. The guide tells you how to set the
variable, and you set it: in your shell profile (`~/.zshrc`, `~/.bashrc`, PowerShell's
`$PROFILE`), a secrets manager your shell reads from (1Password CLI, `pass`, macOS Keychain), or
your scheduler's or CI's secret store (GitHub Actions secrets, Databricks secret scopes, Airflow
connections). A variable whose name starts with `DRE_SECRET_` is also masked as `*****` in DRE's
output and logs.

If you ask the guide to "just put the token in the YAML", it refuses, says why, and writes the
`env_var()` reference instead.

*Why:* `profiles.yml` gets copied, backed up and committed. The same file with `env_var()`
references is safe to share.

### SEC-4: Check a variable is set without showing it

**Level:** block

To confirm a variable is set, the guide runs a check that prints only "set" or "missing":

```bash
[ -n "$PG_PASSWORD" ] && echo "PG_PASSWORD is set" || echo "PG_PASSWORD is missing"
```

```powershell
if ($env:PG_PASSWORD) { "PG_PASSWORD is set" } else { "PG_PASSWORD is missing" }
```

It never runs `echo $PG_PASSWORD`, `env`, `printenv` or `set` without a filter, and never reads a
secret from a file. If the variable is missing in the agent's shell but set in yours, start the
agent again from a shell that has it.

*Why:* printing the value puts it in the chat, which SEC-1 exists to prevent.

### SEC-5: A pasted secret has leaked

**Level:** block

If a secret is pasted into the chat anyway, the guide:

1. doesn't use it, repeat it, or write it to any file or command line;
2. says it must now be treated as leaked, and gives the revoke-and-rotate steps below for that
   platform;
3. carries on with the safe setup (SEC-2, SEC-3) using the new secret, which you set yourself.

Revoke and rotate:

- **Databricks personal access token**: User Settings → Developer → Access tokens → revoke it
  (or `databricks tokens delete <token-id>`). Prefer switching to `auth_type: oauth` instead of a
  new token. **Service principal secret**: in the account console (or workspace admin settings),
  open the service principal → Secrets, create a new secret, then delete the leaked one.
- **Postgres password**: have the role's password changed (`ALTER ROLE <role> PASSWORD '<new>'`
  by an admin, typed in your own terminal, not the chat), and check the server's logs for
  unexpected sign-ins.
- **AWS access key (S3)**: IAM → Users → Security credentials → make the key inactive, then
  delete it (`aws iam update-access-key --status Inactive`, then `aws iam delete-access-key`).
  Check CloudTrail for its use. Prefer `aws sso login` over a new key.
- **Google Cloud service account key (GCS)**: IAM & Admin → Service accounts → Keys → delete it
  (`gcloud iam service-accounts keys delete <key-id> --iam-account <account>`). Prefer
  `gcloud auth application-default login` over a new key.
- **Azure storage (connection string, access key or SAS token)**: storage account → Security +
  networking → Access keys → rotate the key it contains or was signed with (a SAS stays valid
  until the key that signed it is rotated, or its stored access policy is removed). Prefer
  `use_azure_cli: true`.
- **SFTP or FTP password**: have it changed on the server. **SFTP private key**: remove its public
  key from the server's `authorized_keys`, and make a new key pair.
- **SMTP (email) password**: change it, or revoke the app password in your mail provider's
  account security settings, and make a new one.
- **Slack bot token**: in your app's settings (api.slack.com/apps) → OAuth & Permissions →
  revoke the tokens, then reinstall the app to the workspace to get a new one.

*Why:* once a secret has been in a chat, you can't know where copies went. Rotating is the only
way to be sure.

## Setup

### SET-1: Keep profiles in `~/.dre`

**Level:** advise

Connections go in `~/.dre/profiles.yml`, outside any project, where `dre init` writes them. Keep
a `profiles.yml` inside a project only for CI, a container or a Databricks job, and then take
every secret from `env_var()` (SEC-3), since the project is committed.

*Why:* profiles are per person and per machine; reports are shared.

### SET-2: One profile per system, one target per environment

**Level:** advise

Give each system one profile, with a target per environment (`dev`, `prod`) and `dev` as its
default `target`. Run production with `--target prod`. Two Databricks workspaces are two profiles.

```yaml
sources:
  warehouse:
    target: dev
    targets:
      dev: {type: postgres, host: localhost, user: me, database: shop, password: "{{ env_var('PG_DEV_PASSWORD') }}"}
      prod: {type: postgres, host: db.internal, user: reports, database: shop, password: "{{ env_var('PG_PROD_PASSWORD') }}"}
```

*Why:* the same report runs against dev and prod unchanged, and a plain `dre run` can't reach
production by accident.

### SET-3: Start from a validated project

**Level:** advise

Create the project with `dre new` (or `dre init`), then run `dre validate` before writing any
report. Commit `dre.lock` with the project.

*Why:* a known-good base means the first error you see is about your report, not the setup.
`dre.lock` pins plugin versions so every machine runs the same ones.

## Reports

### REP-1: One tab per SQL file, set in the YAML

**Level:** warn

Each tab (a sheet in xlsx, a file in csv) comes from its own `.sql` file, listed in the report's
YAML in the order the tabs should appear. The YAML decides the tabs, never the data: don't try to
split one query's rows into tabs, or make tabs from a column's values. Name tabs with `tab_name`.

```yaml
queries:
  - {query: summary, tab_name: Summary}
  - {query: detail, tab_name: Detail}
```

*Why:* the workbook's shape stays the same every run, whatever the data holds, so the people and
systems reading it can rely on it. DRE refuses a second `SELECT` in a tab file for the same
reason.

*Warned when:* you ask for one SQL file to produce several tabs, or for tabs per value in the
data.

### REP-2: Variables, not hardcoded dates or values

**Level:** warn

Use `run.date` (and its navigation, e.g. `run.date.prev_month.start.date`) for dates, and
`var('name')` with defaults in the YAML for anything that changes between runs, clients or
environments. Write `'{{ run.date.prev_month.start.date }}'`, not `'2026-08-01'`.

*Why:* a hardcoded date or client name means editing SQL every run, and a report that silently
repeats last month's numbers when someone forgets. Variables also let `--var` and schedules
change a run without touching the files.

*Warned when:* you ask for a literal date, month or client value in SQL or a path.

### REP-3: `tab: false` for setup statements

**Level:** advise

A file that only prepares data (temp tables, `SET`s) is listed with `tab: false`, before the
tabs that use it.

*Why:* it runs on the same session as the tabs, and its result doesn't become an empty tab.

### REP-4: Sets for variants, not copies

**Level:** advise

When the same report runs for several clients, regions or departments, declare Sets with their
own vars instead of copying the report.

*Why:* one copy of the SQL to fix and review.

### REP-5: Format in the output, not in SQL

**Level:** advise

Keep numbers and dates typed in SQL and format them with the format's options (xlsx `columns:`
formats, `date_format`; fixed-width `decimals`, `date_format`). Don't turn them into text with
`to_char` or string concatenation.

*Why:* typed values stay sortable and summable in Excel, and one format change doesn't mean
editing every query.

### REP-6: Share SQL with `ref()` and lookups

**Level:** advise

Reuse a query through `ref('file')`, and keep mapping tables you maintain by hand as lookups
(`lookups/`) rather than as `CASE` expressions or tables someone has to load.

*Why:* one definition, used everywhere.

## Running and delivering

### RUN-1: Test before you schedule

**Level:** warn

Before a report goes on a schedule or into production: `dre validate -s <report>` passes, a
`dre run <report> --preview` looks right, and a full run to the local target folder has been
checked.

*Why:* a scheduled report that fails, or delivers wrong numbers, is found by the people who
receive it.

*Warned when:* you ask to schedule a report, or run it with `--target prod`, before it has been
tested this way.

### RUN-2: Confirm before production and real recipients

**Level:** warn

Before a run that reads from a production target or delivers anywhere but the local target
folder (email, Slack, object storage, SFTP), the guide shows what will happen (`dre validate -s
<report>` lists the source, target and every destination) and asks. Use `--preview` to try a
report: it limits rows and never delivers.

*Why:* an email or Slack post can't be recalled.

### RUN-3: Pass the scheduled date

**Level:** advise

A scheduler passes the run's logical date with `DRE_RUN_DATE=YYYY-MM-DD`, and runs a schedule
with `dre run --schedule <name>`.

*Why:* a rerun of last Monday's job renders last Monday's dates, not today's.

### RUN-4: A lasting target path on ephemeral runners

**Level:** advise

On a job cluster, a CI runner or a container, point `--target-path` (or `DRE_TARGET_PATH`) at a
folder that outlives the run, e.g. a Databricks Volume.

*Why:* schema-drift detection compares each run with the last successful one, whose snapshot
lives in the target path.

### RUN-5: Look before accepting a schema change

**Level:** warn

When a run stops because the output's columns changed, find out why before re-running with
`--accept-schema-change`.

*Why:* the people or systems reading the file may depend on its columns; the check exists to
stop an unexpected change from reaching them.
