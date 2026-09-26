# First-party plugins

Every plugin is declared in the project (`sources:`, `formats:`, `destinations:`) and configured
through a `profiles.yml` output of its `type`. Fields holding secrets can use `env_var()`.

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
| `token` | Personal access token or OAuth token. |
| `catalog`, `schema` | Defaults for the session. |
| `retry_timeout` | Seconds to keep waiting while a stopped warehouse starts. Default 900. |

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

`host` and `token`, the same fields as the `databricks` source, so one set of credentials can
serve both. Paths are `/Volumes/<catalog>/<schema>/<volume>/...`, uploaded through the Files API.

**When to use it**: runs outside Databricks, such as a laptop, Airflow or CI. Inside a Databricks
job or cluster, `/Volumes/...` is already a mounted path, so use the built-in `local` destination
with that path instead.

Every destination streams the file from `target/run/`. If an upload fails, the output stays
there and the run reports which Binding failed.
