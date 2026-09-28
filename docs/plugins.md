# First-party plugins

Plugins come in packages, declared once each under `plugins:` in `dependencies.yml` (see
[the registry docs](registry.md)). A source or destination is configured through a profile in
`profiles.yml` (under `sources:` or `destinations:`) whose target has its `type`. Fields holding
secrets can use `env_var()`.

| Package | Provides |
|---|---|
| `duckdb` | the `duckdb` source |
| `postgres` | the `postgres` source |
| `databricks` | the `databricks` source, and the `databricks` destination (Volumes and workspace files) |
| `csv` | the `csv` and `delimited` formats |
| `fixed_width` | the `fixed_width` format |
| `parquet` | the `parquet` format |
| `xlsx` | the `xlsx` format |
| `object_store` | the `s3`, `gcs` and `azure_blob` destinations |
| `sftp` | the `sftp` destination |
| `ftp` | the `ftp` destination |
| `email` | the `email` destination |
| `slack` | the `slack` destination |

```yaml
# dependencies.yml
plugins:
  - databricks
  - xlsx
  - object_store
```

## Sources

### `duckdb`

| Field | Notes |
|---|---|
| `path` | Database file, relative to the project directory. Default `:memory:`. |
| `threads`, `memory_limit` | Passed to DuckDB. |

Capabilities: `sessions`, `read_only`, `check` (via `EXPLAIN`).

### `postgres`

| Field | Notes |
|---|---|
| `host`, `port` | Default `localhost`, `5432`. |
| `user`, `password` | |
| `database` (or `dbname`) | |
| `sslmode` | `disable`, `prefer` (default), `require`, `verify-ca`, `verify-full`, with libpq's meanings. |
| `sslrootcert` | CA certificate for `verify-ca` / `verify-full`. |
| `connect_timeout` | Seconds. |
| `schema` | Put first on the search path. |
| `role` | `SET ROLE` after connecting. |

Capabilities: `sessions`, `read_only`, `check` (via `EXPLAIN`). `numeric(p,s)` becomes a decimal
column. An unconstrained `numeric` becomes exact text, so cast it (`::numeric(18,2)`) when you
want a typed column. Types DRE can't map ask for a cast, e.g. `interval_col::text`.

Postgres drops `numeric(p,s)`'s precision and scale in `VALUES` lists and across `UNION`, so
their numbers arrive as plain `numeric`, i.e. text. Cast in the outer query:

```sql
select code, amount::numeric(18,2) as amount
from (values ('a', 1.50), ('b', 2.25)) as t(code, amount)
```

A connection error names the server (`host:port/database`). On macOS the plugin uses the system
TLS stack: TLS 1.2 at most, and with `verify-ca`/`verify-full` a server certificate valid for more
than 825 days is rejected (Apple's limit), so issue server certificates for 825 days or less.

### `databricks`

| Field | Notes |
|---|---|
| `host` | Workspace host. |
| `http_path` | The SQL warehouse's HTTP path. |
| `auth_type` | `auto` (default), `pat` or `oauth`. |
| `token` | A personal access token, or any other bearer token. Optional with `auto`. |
| `profile` | A `~/.databrickscfg` profile to sign in with (for `auto`). |
| `client_id` | For `oauth`: the OAuth client. Browser sign-in defaults to `databricks-cli`, which every workspace has. For a service principal, its application ID. |
| `client_secret` | For `oauth`: a service principal's OAuth secret. Without it, `oauth` signs you in through the browser. |
| `scopes` | For `oauth`: default `all-apis offline_access` for browser sign-in, `all-apis` for a service principal. |
| `redirect_port` | For browser sign-in: the localhost port the sign-in redirects to. Default 8020, which is what `databricks-cli` allows. |
| `catalog`, `schema` | Defaults for the session. |
| `retry_timeout` | Seconds to keep waiting while a stopped warehouse starts. Default 900. While it waits, DRE says so every 30 seconds. A host that doesn't resolve, or refuses the connection, fails at once. |

```yaml
sources:
  warehouse:
    target: dev
    targets:
      dev:        # you, through the browser
        type: databricks
        host: dbc-123.cloud.databricks.com
        http_path: /sql/1.0/warehouses/abc
        auth_type: oauth
      prod:       # a service principal, for the orchestrator
        type: databricks
        host: dbc-123.cloud.databricks.com
        http_path: /sql/1.0/warehouses/abc
        auth_type: oauth
        client_id: "{{ env_var('DATABRICKS_CLIENT_ID') }}"
        client_secret: "{{ env_var('DATABRICKS_CLIENT_SECRET') }}"
```

With `auth_type: auto` (the default) most setups need no sign-in fields at all. DRE uses, in order:

1. `token` in the profile, or `client_id` + `client_secret` (a service principal);
2. whatever Databricks' own tools would use, through Databricks' Go SDK: `DATABRICKS_TOKEN`, or
   `DATABRICKS_CLIENT_ID` + `DATABRICKS_CLIENT_SECRET`; a `~/.databrickscfg` profile (`profile:`,
   or `DATABRICKS_CONFIG_PROFILE`); a `databricks auth login` session; the VS Code extension; CI
   OIDC tokens (GitHub Actions, Azure DevOps); Azure and Google credentials;
3. DRE's own saved sign-in, then a browser sign-in if a person is at the terminal.

When no one is at the terminal (a scheduler, CI, a Databricks job), DRE never waits for a
browser: it fails at once and lists what would work. In a Databricks job, give it
`DATABRICKS_TOKEN` or a service principal.

DRE signs in only when a report actually uses the profile: a source when its first query runs,
a destination when it delivers.

Browser sign-in opens your browser the first time and saves the session in
`~/.dre/oauth_sessions.json`, which only you can read. The file has one entry per workspace and
OAuth client, so a report can read from one workspace and deliver to another, and the `databricks`
source and destination share one sign-in per workspace. After that the refresh token renews the
session, and the browser only opens again once the refresh token stops
working. Delete the file (or its entry) to sign out. Set `DRE_NO_BROWSER=1` to only print the
sign-in URL.

Service principal tokens stay in memory. With either kind, the access token is renewed before it
expires, so a long run keeps its session. Passwords, tokens and client secrets are never saved:
put them in environment variables and use `env_var()`.

Capabilities: `sessions`, `check` (via `EXPLAIN`), `load`. The plugin holds a real warehouse
session, so temp views and `SET`s last for the whole Binding. The session runs in UTC. Warehouses
have no read-only mode.

The `databricks` package is written in Go, on Databricks' official Go connector
(`databricks-sql-go`): SQL warehouses only hold sessions for Databricks' own clients, and the
connector identifies itself with `dre` appended. One program serves as this source and the
`databricks` destination. Set `DATABRICKS_LOG_LEVEL=debug` to see the connector's own log.

Databricks SQL reads backslashes as escapes in string literals and doesn't read `''` as an
escaped quote: `'O''Brien'` is two literals, `'O'` and `'Brien'`, which Databricks joins into
`OBrien`. Jinja that builds literals from values should escape for Databricks
(`'O\'Brien'`); `dre_utils` does this through `dispatch()`, and lookups are inlined portably.

## Formats

Each format plugin declares and checks its own options: `dre validate` and `dre run` send every
report's `output:` keys to the plugin before anything runs, and report each problem with the
report it came from. A format no declared package provides, or whose package isn't installed, is
an error. For one no declared package provides, `dre validate` and `dre run` look it up in DRE's
plugin registry and name the package to add under `plugins:` in `dependencies.yml`.

Project-wide defaults for a format go in `dre_project.yml` under `format_options`, keyed by
format. They apply under every output of that format, whatever folder or report chose it, and a
report's own keys win:

```yaml
format_options:
  delimited: {delimiter: "|", quoting: strings}
  csv: {quoting: all}
```

| Format | Options |
|---|---|
| `csv`, `delimited` | `delimiter`, `quote`, `quoting`, `header`, `line_ending`, `encoding`, `null`, `byte_order_mark` |
| `fixed_width` | `columns` (see [Fixed-width columns](#fixed-width-columns)), `header`, `line_ending`, `encoding`, `line_breaks` |
| `parquet` | none; Arrow types are preserved |
| `xlsx` | `header`, `max_rows_per_sheet`; per query `anchor`/`header`; `template` |

Every format but xlsx also takes `extension`: the output file's extension (`aba`, `dat`, ...), or
`""` for none. The file is written the same way; only its name changes.

- `quoting` (csv, delimited) picks which fields are wrapped in `quote`:
  - `minimal` (the default): only fields holding the delimiter, the quote or a line break;
  - `all`: every field but nulls;
  - `strings`: every value of a text, date, time or timestamp column, and the header; numbers,
    booleans and nulls stay bare unless they hold the delimiter;
  - `none`: no field. A value that can't be written without quotes fails the run, naming the row
    and column.

  A doubled quote escapes a quote inside a quoted field. For tab- or pipe-separated text, use
  `delimited` with `delimiter: "\t"` and, say, `extension: tsv`.
- `null: "NULL"` (csv, delimited) writes that marker for nulls instead of an empty field. It can be
  written unquoted as above: DRE reads a YAML `null:` key as the option `null`.
- Timestamps with a timezone are written in their zone with the offset,
  `2026-01-01 11:00:00+11:00`; timestamps without one as `2026-01-01 00:00:00`.
- `fixed_width` refuses a value with a line break, since it would split the record, naming the
  row and column. `line_breaks: replace` writes a space instead. Tabs and other characters are
  written as they are.
- `xlsx` keeps every value exact. What Excel can't store as a number or date is written as text,
  with one warning per column: numbers with more than 15 significant digits (large integers,
  wide decimals), numbers beyond Excel's range, and dates or timestamps before 1900-03-01 or
  after 9999-12-31 (as ISO text).

### Fixed-width columns

Write the query as normal SQL; `columns:` lays each result column out, in order. Every key but
`name` and `width` (or `picture`) is optional. By default every field is left-aligned and
space-filled, whatever the column's type; nothing else happens unless you ask for it.

| Key | Meaning |
|---|---|
| `name` | the result-set column; the same column can appear more than once |
| `width` | the field's width in characters |
| `picture` | a COBOL PIC clause in place of `width`: see below |
| `header` | the column's label in the header record (default: `name`) |
| `type` | `number` lays the value out as a number (below) with no other number option; `text` never does. Default: `number` when the column has `decimals`, `decimal_point`, `sign` or a 9 `picture`, otherwise `text` |
| `align` | `left` (default) or `right` |
| `pad` | the fill character, e.g. `"0"`. Default: a space. With `align: right` and `pad: "0"` a leading minus goes before the zeros: `-00042` |
| `truncate` | `true` cuts a value that's too wide instead of failing. A number (a column with `type: number` or a number option) is never cut: one that doesn't fit is an error naming the row and column |
| `decimals` | round numbers to this many decimal places, half away from zero (`2.345` → `2.35`) |
| `decimal_point` | `.` (default), `,`, or `implied`: the digits are written without a point and the last `decimals` digits are the decimals (`123.45` with `decimals: 2` → `12345`) |
| `sign` | `leading` (default: `-` for negatives only), `always` (`+` or `-` first), `trailing` (`+` or `-` last), `overpunch` (the last digit carries the sign, COBOL zoned decimal), `none` (unsigned: a negative number is an error) |
| `date_format` | a strftime pattern for a date, time or timestamp column, e.g. `"%Y%m%d"` |
| `null_fill` | the character that fills a NULL field. Default: the pad, so NULL is blank unless `pad` is set; `null_fill: " "` keeps a zero-padded column blank for NULL |

The output option `header: true` writes a first record of the labels, each left-aligned and cut
to its column's width.

```yaml
output:
  format: fixed_width
  header: true
  columns:
    - {name: account_id, width: 10, align: right, pad: "0"}   # 42 → 0000000042
    - {name: account_name, width: 30, truncate: true}         # left-aligned, space-filled
    - {name: amount, width: 12, align: right, pad: "0", decimals: 2, decimal_point: implied, sign: trailing}
                                                              # -123.456 → 00000012346-
    - {name: posted_on, width: 8, date_format: "%Y%m%d"}      # 20260928
    - {name: discount, width: 6, align: right, pad: "0", decimals: 2, null_fill: " "}
                                                              # 5 → 005.00; NULL → blank
    - {name: rate, picture: "9(3)V9(4)"}                      # 1.23456 → 0012346
```

`picture` takes the COBOL layout that mainframe and bank file specs are written in:

| Picture | Width | Meaning | `123.456` | `-12.3` |
|---|---|---|---|---|
| `X(10)` | 10 | text | | |
| `9(5)` | 5 | unsigned whole number | `00123` | error |
| `9(7)V99` | 9 | implied decimal point, 2 decimals | `000012346` | error |
| `S9(5)V99` | 7 | signed; the last digit carries the sign (overpunch) | `001234F` | `000123}` |
| `9(5).99` | 8 | a visible point | `00123.46` | error |

A 9 picture is right-aligned and zero-filled, as in COBOL; `align` and `pad` still override it.
`9(3)` is short for `999`. A `sign` beside a picture replaces its sign: `leading`, `always` or
`trailing` adds one character to the width for the sign (COBOL `SIGN SEPARATE`). Overpunch
writes the last digit 0–9 as `{`, `A`–`I` for positive numbers and `}`, `J`–`R` for negative
ones. Packed decimal (`COMP-3`) is binary, not text, and isn't supported.

## Destinations

The built-in `local` destination copies the file to a path, relative to the project. It needs no
plugin and no declaration.

A destination entry's keys other than `profile` and `path` are the plugin's options, and the
plugin checks them the same way formats do, against the destination profile's output for the
active target (`--target`). A value holding Jinja is checked once it's rendered, at delivery.

### Several destinations

`output.destination` takes one destination or a list. Each entry names a profile, an optional
`path`, and any options its plugin takes (recipients, a channel, a message). Options are rendered
with the same Jinja context as paths:

```yaml
output:
  format: xlsx
  destination:
    - profile: reports_s3
      path: "s3://reports/{{ var('client') }}/monthly-{{ run.date.yyyymmdd }}.xlsx"
    - profile: finance_mail
      to: "{{ var('client') }}-finance@example.com"
      subject: "Monthly report {{ run.date.iso }}"
    - profile: team_slack
      channel: "#finance-reports"
      message: "Monthly report for {{ var('client') }}"
```

- Entries are delivered in order. If one fails, the rest are still attempted; the Binding then
  fails and the run exits non-zero.
- Each entry follows `--target` on its own: an entry whose profile has no output for the active
  target is skipped and logged, while the others are delivered.
- `run_results.json` lists every entry under `deliveries`, with `profile`, `type`, `status`
  (`delivered`, `skipped` or `failed`), `location` and `error`.
- A Set can replace the whole list. Overriding only `path:` works when exactly one destination
  is inherited; with several, override the full list.
- The local file is named after the first entry's `path`. `--output-path` and `--output-name`
  apply to every entry that has a path.
- Credentials stay in `profiles.yml`. Options belong to the report, so a Set can address its own
  recipients.
- A destination that takes no options (`local`, `s3`, `sftp`, ...) fails the delivery if its
  entry has any other key, so a misspelt `path` is caught instead of ignored.

### `s3`

`bucket`, `region`, and `access_key_id` + `secret_access_key` (+ `session_token`). Leave the keys
out to use AWS's default credential chain, the same as the AWS CLI: environment variables, the
shared config and credentials files (the profile named by `profile:`, else `AWS_PROFILE`), SSO,
`credential_process`, web identity, and container or instance roles. `AWS_EC2_METADATA_DISABLED`
is honoured, and with no credentials anywhere the delivery fails at once, listing what it tried.
The region comes from `region:`, else the AWS config. `endpoint` and `allow_http` point it at
S3-compatible stores. Paths are `s3://bucket/key`, or a bare key in `bucket`.

### `gcs`

`bucket`, and `service_account_key_path` or `service_account_key`. Leave both out to use
application default credentials: `GOOGLE_APPLICATION_CREDENTIALS`, the file
`gcloud auth application-default login` writes, or the metadata server on Google Cloud. `endpoint` is for emulators. Paths are `gs://bucket/key`.
Uploads use GCS's resumable protocol.

### `azure_blob`

`account_name`, `container`, and one of `connection_string`, `sas_token`, `access_key`,
`use_managed_identity: true`, or `use_azure_cli: true` (the `az login` session). `endpoint` is for emulators. Paths are `az://container/key`.

### `sftp`

`host`, `port` (22), `username`, and `password` or `private_key_path`
(+ `private_key_passphrase`). The host key is checked against `known_hosts_path` (default
`~/.ssh/known_hosts`) or a pinned `host_key_fingerprint` (`SHA256:...`). Unknown hosts are
refused unless `accept_unknown_host: true`. Missing directories are created.

### `ftp`

`host`, `port` (21), `username`, `password`, `passive` (default true), and `tls`: `none` or
`explicit` (FTPS). `tls_accept_invalid_certs` allows self-signed server certificates.

Paths are relative to the folder the login starts in; a leading `/` means the server's root,
which on many servers isn't the login folder (`/reports/x.csv` vs `reports/x.csv`). FTPS data
connections reuse the control connection's TLS session, which vsftpd, ProFTPD and FileZilla
Server require by default. A failed upload removes the partial file from the server when it can.

### `databricks`

Unity Catalog Volumes and workspace files, chosen by the path. `host` and the same sign-in fields
as the `databricks` source (`auth_type`, `token`, `client_id`, `client_secret`), so one set of
credentials, and one OAuth session per workspace, serves both. It's the same program as the
source.

```yaml
destinations:
  lakehouse:
    target: prod
    targets:
      prod: {type: databricks, host: dbc-123.cloud.databricks.com}
```

- **`/Volumes/<catalog>/<schema>/<volume>/...`**: uploaded to the Volume through the Files API.
  Missing directories under the volume are created. Use it anywhere, for any size of file.
- **`/Workspace/Users/<user>/...`, `/Workspace/Shared/...` or `/Workspace/Repos/...`** (the
  `/Workspace` prefix is optional): a workspace file, for outputs people open from the workspace
  browser, next to notebooks and dashboards. Missing folders are created, the file replaces one
  already at the path, and it's always a plain file: a `.sql` or `.py` output isn't turned into a
  notebook. Workspace files are meant for small files (the import API takes up to about 10 MB);
  use a Volume for large outputs.

On Databricks compute, where `/Volumes` and `/Workspace` are mounted, the file is copied there
directly instead: no API call and no sign-in, with the job's own access. The same report works
outside Databricks (a laptop, Airflow, CI), where it uploads, and in a Databricks job or cluster.

### `email`

Sends the output as attachments on one email over SMTP. If a report produces several files, they
all go on the same message.

Profile fields: `host`, `port` (587 for `starttls`, 465 for `implicit`, 25 for `none`), `tls`
(`starttls` by default, `implicit` or `none`), `username` and `password`, and `from`
(`reports@example.com` or `"Reports <reports@example.com>"`). Optional fields:
- `to`, `cc`, `bcc`: default recipients.
- `max_attachment_mb`: default 20.
- `tls_accept_invalid_certs`: allows a self-signed server certificate.

Destination options:

| Option | Meaning |
|---|---|
| `to`, `cc`, `bcc` | An address, a comma-separated string or a list. Each one replaces the profile's default. |
| `subject` | Default `Report: <file names>`. |
| `body` | Plain text. Default `Attached: <file names>`. |
| `attachment_name` | Renames the attachment. Only allowed when the output is a single file. |

```yaml
destination:
  - profile: finance_mail
    to: ["{{ var('client') }}-finance@example.com"]
    bcc: archive@example.com
    subject: "Monthly report {{ run.date.iso }}"
    body: "Attached is this month's report for {{ var('client') }}."
```

The plugin checks the email before it connects. It fails without sending anything when there are
no recipients, an address is invalid, an option is unknown, or the attachments exceed
`max_attachment_mb`. Most mail servers cap a message at 20–25 MB. For bigger files, deliver to
object storage and email a link in `body`. The password is never logged.

### `slack`

Uploads the output to a Slack channel, or to one person's DM, as a single post with a message.
If a report produces several files, they all go in the same post.

The profile holds `token`, a bot token (`xoxb-...`), which is never logged. It can also hold a
default `channel`. Destination options:

| Option | Meaning |
|---|---|
| `channel` | A channel ID (`C0123ABCD`) or `#name`. A name is looked up among the channels the bot can see. |
| `user` | A user ID (`U0123ABCD`). The file goes to the bot's DM with that person. |
| `message` | The post's text. |

Give exactly one of `channel` or `user`. If you give neither, the profile's `channel` is used.

```yaml
destination:
  - profile: team_slack
    channel: "#finance-reports"
    message: "Monthly report for {{ var('client') }} ({{ run.date.iso }})"
```

Slack app setup: create an app, add a bot user, install it to the workspace, and use its bot
token. Bot scopes:
- `files:write`: always needed.
- `channels:read` and `groups:read`: needed to post to a `#name`.
- `im:write` and `chat:write`: needed for `user`.

A DM also needs the app's Messages tab turned on (App Home > Show Tabs > Messages Tab). With it
off, Slack accepts a file for the DM and then silently drops it, so before uploading the plugin
checks that the DM accepts messages. The check posts nothing, and if the tab is off the delivery
fails and says what to change.

The bot must be a member of the channel. Invite it with `/invite @your-bot`.

If Slack rate-limits a call, the plugin retries it once after Slack's `Retry-After`, waiting at
most 60 seconds. Errors such
as a rejected token, a missing scope, or the bot not being in the channel are reported with what
to fix. The delivered location is the uploaded files' permalinks.

Every destination streams the file from `target/run/`. If an upload fails, the output stays
there and the run reports which Binding failed.
