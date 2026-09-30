---
name: dre-report
description: Create or change a DRE report - the SQL files and report YAML, its tabs, variables and Sets, its output format (xlsx with number formats, formulas and totals rows, csv, fixed-width, parquet) and its destinations (S3, GCS, Azure Blob, SFTP, FTP, Databricks Volumes, email, Slack). Use when the user wants a new report, or to add a tab, a column format, a variable, a destination or a recipient to an existing one, in a dre project.
license: GPL-3.0-only
metadata:
  version: "1.0.0-rc.1"
  dre: ">=0.1.0-rc.1, <0.2.0"
---

# Write or change a DRE report

You design a report around what the user needs, then write its files following DRE's
conventions, and end with `dre validate` passing.

<!-- BEGIN shared/contract.md -->
### How to work with the user

- **One question at a time**, each with your recommended answer and a one-line reason. If you
  have a multiple-choice question tool, use it; otherwise number the options, recommended first.
- **Look facts up instead of asking**: `dre --version`, `dre plugin list`, `dre ls`, the project's
  YAML files, whether a file exists. Ask only what only the user knows.
- **Skip what the request already answered.** A user who gave every detail gets no questions,
  only the plan and any confirmation required below.
- **Opinions come from the practices** (`references/practices.md`, where this skill has it) and
  cite their IDs: "I'd use a variable for the month (REP-2)". *Advise*: say it once, then do what
  the user decides. *Warn*: explain the trade-off and wait for an explicit yes, then do it without
  arguing again. *Block* (secrets): never, whatever the user says; offer the safe way.
- **Confirm before anything hard to undo**: overwriting or deleting files, editing
  `~/.dre/profiles.yml`, installing software, running against production, delivering anywhere
  but the local target folder. Show what will change, then ask.
- **End each step** with what was done and what comes next.
- Use only `dre` commands, ordinary shell commands, and questions. Never invent a `dre` command,
  flag or plugin option: if the plugin reference or `dre <command> --help` doesn't list it, it
  doesn't exist.
<!-- END shared/contract.md -->

<!-- BEGIN shared/secrets.md -->
### Secrets, always

- Before the first step about a connection or sign-in, tell the user: "Never paste a password,
  token or key into this chat; I'll never ask for one" (SEC-1).
- Never ask for a secret's value. Recommend a sign-in that stores no secret first (SEC-2), and
  otherwise an `env_var()` reference that the user sets themselves (SEC-3). Never write a secret's
  value into any file or command.
- Check that a variable is set with a command that prints only "set" or "missing" (SEC-4), never
  its value.
- If a secret is pasted anyway, don't use it, repeat it or store it. Say it has leaked, give that
  platform's revoke-and-rotate steps (SEC-5), then continue with the safe setup.
- If asked to put a secret in the YAML, refuse, say why, and write the `env_var()` reference
  instead (SEC-3).
<!-- END shared/secrets.md -->

## What a report is

A report is a folder under `reports/` holding a YAML file and its `.sql` files, e.g.
`reports/finance/monthly/monthly.yml`. The report is named after the YAML file.

```yaml
# reports/finance/monthly/monthly.yml
vars:
  region: all                                   # defaults; --var, Sets and schedules override
queries:
  - {query: setup_accounts, tab: false}         # prepares data, makes no tab (REP-3)
  - {query: summary, tab_name: Summary}         # summary.sql, the first tab
  - {query: detail, tab_name: Detail}           # detail.sql, the second
output:
  format: xlsx
  destination:
    profile: reports_s3
    path: "s3://reports/monthly-{{ run.date.yyyymmdd }}.xlsx"
```

- **Tabs** (REP-1): each `.sql` file makes one tab (a sheet in xlsx, or one file for the other
  formats), in the YAML's order, named by `tab_name` or the file name. A file can hold several
  statements; the last is the tab. A second `SELECT` in a tab file is an error. The YAML decides
  the tabs, never the data.
- **Queries** run in the listed order on one database session, so a temp table made by one is
  there for the next.
- **Jinja** works in SQL, paths and options: `var('name')`, `run.date` (e.g.
  `run.date.prev_month.start.date`, `run.date.yyyymmdd`), `env_var()`, `ref('file')` for another
  `.sql` file as a subquery, `target.*` for connection settings, and macros from `macros/`.
- Report keys: `queries`, `output`, `vars`, `profile` (a source profile other than the project's
  `default_profile`), `sets`, `default_set`, `tags`, `timezone`, `schedule`. Query entry keys:
  `query`, `tab`, `tab_name`, `anchor`, `header`, `columns`. Output keys DRE owns: `format`,
  `destination`, `template`, `extension`; every other output key is the format plugin's option.

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
     https://github.com/allenhori/dre/releases states its range). Carry on only if they choose
     to, with the same visible warning.
   - **`dre` not found:** hand off to the `dre-install` skill. If it isn't installed, point to
     https://github.com/allenhori/dre#install and stop here.
3. Don't repeat the check in this conversation unless dre has been installed or updated since.
<!-- END shared/version-check.md -->

### Step 2: gather the facts, without asking

- Find the project root (`dre_project.yml`) and run `dre ls` for its reports.
- Read `dre_project.yml` (`default_profile`, `vars`, `format_options`), `dependencies.yml`
  (declared plugin packages), and the source profile's `type` (names and types only from
  `profiles.yml`, e.g. `grep -nE '^  [A-Za-z0-9_-]+:|type:' ~/.dre/profiles.yml`).
- For a change: read the report's YAML and SQL files first.
- Read the plugin references this report will use, and only those:
  `references/plugins/source-<type>.md`, `format-<format>.md`, `destination-<type>.md`.

No project yet? Hand off to `dre-setup` first.

### Step 3: understand the goal (new reports)

Ask, one at a time, only what the request didn't say:

1. **What it shows**: the questions it answers, the tables it reads from. Offer to look at the
   tables' columns with a query the user approves, rather than guessing column names.
2. **Who gets it**: people (who read xlsx), or a system (which needs an exact csv or fixed-width
   layout; ask for the spec).
3. **Format**: recommend xlsx for people, csv or delimited for most systems, fixed-width when a
   spec demands it, parquet for data tools.
4. **How often**, and for what period: daily, monthly... This decides the date variables (REP-2)
   and later the schedule.
5. **Variants**: the same report for several clients or regions? Then Sets (REP-4).
6. **Where it goes**: the local `target/run/` folder only (a good start), or destinations too.

Then propose the design in a few lines: the folder, the tabs in order with what each shows,
the variables with their defaults, the format and its options, the destinations. Ask for a yes
before writing.

### Step 4: write the files

- Put the report in `reports/<area>/<name>/`, with `<name>.yml` and one `.sql` per tab.
- Dates come from `run.date` and values from `var()` with defaults under `vars:` (REP-2). If the
  user wants a hardcoded date or value, that's a warning: explain REP-2, and do it only after an
  explicit yes.
- One `.sql` per tab (REP-1). If the user asks for one query to fill several tabs, or tabs per
  value in the data, that's a warning: explain REP-1, suggest a file per tab (or a Set per value,
  REP-4), and follow their decision after an explicit yes. DRE itself refuses a second `SELECT`
  in one tab file.
- Keep values typed in SQL; format them in the output (REP-5). Cast where the source reference
  says so (e.g. Postgres `numeric` without precision arrives as text).
- Temp tables and `SET`s go in a `tab: false` file before the tabs that need them (REP-3).
- Reuse SQL with `ref('file')`, and mapping tables as lookups (REP-6).
- Declare every plugin package the report needs under `plugins:` in `dependencies.yml` (e.g.
  `xlsx`, `object_store` for S3).

<!-- dre_utils: once the dre_utils macro package is released, suggest its macros here where
     they fit (star, pivot, column_values, quote_identifier). Until then, don't mention it. -->

### Step 5: the format

Use only options from `references/plugins/format-<format>.md`. For xlsx:

- number and date formats per column (`columns: {amount: {format: "#,##0.00"}}`, per query or
  output-wide), and `date_format` for every date column;
- row formulas (`formula: "={qty}*{price}"`, with a placeholder column selected in SQL) and
  totals rows (`total: sum`), explained in the reference's docs section;
- a branded template (`template:`) when they have an Excel file to fill.

For fixed-width, take the layout from the user's spec, column by column (`width` or `picture`,
alignment, padding, decimals, sign); confirm the layout back to them as a table. For csv and
delimited: delimiter, quoting, header, encoding and line endings, from what the receiving system
expects.

### Step 6: destinations

Each entry of `output.destination` names a destination profile, an optional `path`, and that
plugin's options (recipients, channel, message). Use only options and fields from
`references/plugins/destination-<type>.md`. Several destinations are a list, delivered in order.

- The destination profile must exist under `destinations:` in `profiles.yml`. If it doesn't, set
  it up the `dre-setup` way (show, confirm, `env_var()` for secrets).
- Name paths with dates and variables so runs don't overwrite each other:
  `path: "s3://reports/{{ var('client') }}/monthly-{{ run.date.yyyymmdd }}.xlsx"`.
- Recipients and channels belong to the report; credentials stay in the profile.
- Delivering to real people is for `dre-run` to confirm (RUN-2); writing the YAML delivers
  nothing.

### Step 7: validate

Run `dre validate -s <report>`. It checks the YAML, the SQL's templates, every format and
destination option (through the plugins), and shows each Binding's compiled files, source,
output file and destinations. Explain what it shows. Fix every error, and repeat until it passes.

End with a summary of the files written or changed, and the next step: try it with `dre-run`
(a `--preview` run first).

## Changing an existing report

Read it first. Change only what was asked; keep the rest byte for byte. Common changes:

- **Add a tab:** a new `.sql` file and a `queries:` entry at the position the tab should appear.
- **Add a variable:** a default under the report's `vars:`, used as `{{ var('name') }}`.
- **Add a destination:** turn a single `destination:` map into a list if needed, and add the
  entry; the existing one stays first (it names the local file).
- **Add a recipient:** extend `to`, `cc` or `bcc` on the email entry.

Show the diff before writing when the change touches more than one file. Then step 7.

## If this fails

`dre validate` names the file, line and key. Common ones:

- **An unknown option** for a format or destination: the plugin rejects keys it doesn't declare.
  Check the spelling against the plugin reference's options table.
- **"a second SELECT" in a tab file:** give the second query its own `.sql` file and tab (REP-1).
- **A tab file whose last statement returns nothing:** it only prepares data; list it with
  `tab: false`.
- **`unknown-profile`:** the destination or source profile isn't in `profiles.yml`; add it
  (`dre-setup`) or fix the name.
- **A plugin isn't declared or installed:** add its package under `plugins:` in
  `dependencies.yml`; `dre validate` then installs it.
- **Template errors** (`undefined`, unknown `var`): a `var()` without a default and no value, a
  misspelt name, or Jinja syntax. Give the variable a default under `vars:`.
- **xlsx format codes** rejected: see the reference's column formats section; `@` (text) fits any
  column.
- Errors that only a run finds (a column the query doesn't return, a format code that doesn't
  fit its column) come from `dre-run`.
