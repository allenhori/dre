# First-party plugins

Every plugin is declared in the project (`sources:`, `formats:`, `destinations:`) and configured
through a profile in `profiles.yml` (under `sources:` or `destinations:`) whose target has its
`type`. Fields holding secrets can use `env_var()`.

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

### `databricks`

| Field | Notes |
|---|---|
| `host` | Workspace host. |
| `http_path` | The SQL warehouse's HTTP path. |
| `auth_type` | `pat` (default) or `oauth`. |
| `token` | For `pat`: a personal access token, or any other bearer token. |
| `client_id` | For `oauth`: the OAuth client. Browser sign-in defaults to `databricks-cli`, which every workspace has. For a service principal, its application ID. |
| `client_secret` | For `oauth`: a service principal's OAuth secret. Without it, `oauth` signs you in through the browser. |
| `scopes` | For `oauth`: default `all-apis offline_access` for browser sign-in, `all-apis` for a service principal. |
| `redirect_port` | For browser sign-in: the localhost port the sign-in redirects to. Default 8020, which is what `databricks-cli` allows. |
| `catalog`, `schema` | Defaults for the session. |
| `retry_timeout` | Seconds to keep waiting while a stopped warehouse starts. Default 900. |

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

DRE signs in only when a report actually uses the profile: a source when its first query runs,
a destination when it delivers.

Browser sign-in opens your browser the first time and saves the session in
`~/.dre/oauth_sessions.json`, which only you can read. The file has one entry per workspace and
OAuth client, so a report can read from one workspace and deliver to another, and the `databricks`
source and `databricks_volumes` destination share one sign-in per workspace. After that the
refresh token renews the session, and the browser only opens again once the refresh token stops
working. Delete the file (or its entry) to sign out. Set `DRE_NO_BROWSER=1` to only print the
sign-in URL.

Service principal tokens stay in memory. With either kind, the access token is renewed before it
expires, so a long run keeps its session. Passwords, tokens and client secrets are never saved:
put them in environment variables and use `env_var()`.

Capabilities: `sessions`, `check` (via `EXPLAIN`). The plugin holds a real warehouse session,
so temp views and `SET`s last for the whole Binding. The session runs in UTC. Warehouses have no
read-only mode.

## Formats

| Format | Options |
|---|---|
| `csv`, `delimited` | `delimiter`, `quote`, `quoting`, `header`, `line_ending`, `encoding`, `null`, `byte_order_mark` |
| `fixed_width` | `columns` (`name`, `width`, `align`, `pad`, `truncate`), `line_ending`, `encoding` |
| `parquet` | none; Arrow types are preserved |
| `xlsx` | `header`, `max_rows_per_sheet`; per query `anchor`/`header`; `template` |

See the YAML schema for defaults.

## Destinations

The built-in `local` destination copies the file to a path, relative to the project. It needs no
plugin and no declaration.

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
out to use the ambient credential chain: environment, shared config, or an instance or container
role. `endpoint` and `allow_http` point it at S3-compatible stores. Paths are `s3://bucket/key`,
or a bare key in `bucket`.

### `gcs`

`bucket`, and `service_account_key_path` or `service_account_key`. Leave both out to use
application default credentials. `endpoint` is for emulators. Paths are `gs://bucket/key`.
Uploads use GCS's resumable protocol.

### `azure_blob`

`account_name`, `container`, and one of `connection_string`, `sas_token`, `access_key` or
`use_managed_identity: true`. `endpoint` is for emulators. Paths are `az://container/key`.

### `sftp`

`host`, `port` (22), `username`, and `password` or `private_key_path`
(+ `private_key_passphrase`). The host key is checked against `known_hosts_path` (default
`~/.ssh/known_hosts`) or a pinned `host_key_fingerprint` (`SHA256:...`). Unknown hosts are
refused unless `accept_unknown_host: true`. Missing directories are created.

### `ftp`

`host`, `port` (21), `username`, `password`, `passive` (default true), and `tls`: `none` or
`explicit` (FTPS). `tls_accept_invalid_certs` allows self-signed server certificates.

### `databricks_volumes`

`host` and the same sign-in fields as the `databricks` source (`auth_type`, `token`, `client_id`,
`client_secret`), so one set of credentials, and one OAuth session per workspace, can serve both. Paths are `/Volumes/<catalog>/<schema>/<volume>/...`, uploaded through the Files API.

**When to use it**: runs outside Databricks, such as a laptop, Airflow or CI. Inside a Databricks
job or cluster, `/Volumes/...` is already a mounted path, so use the built-in `local` destination
with that path instead.

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
- `im:write`: needed for `user`.

The bot must be a member of the channel. Invite it with `/invite @your-bot`.

If Slack rate-limits a call, the plugin retries it once after Slack's `Retry-After`, waiting at
most 60 seconds. Errors such
as a rejected token, a missing scope, or the bot not being in the channel are reported with what
to fix. The delivered location is the uploaded files' permalinks.

Every destination streams the file from `target/run/`. If an upload fails, the output stays
there and the run reports which Binding failed.
