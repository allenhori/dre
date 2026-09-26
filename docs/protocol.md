# DRE plugin protocol, version 0

Every source, format and destination in DRE is a plugin: a separate executable that DRE core
starts and talks to over stdin and stdout. Plugins can be written in any language. This document
is the contract between core and a plugin. Any change to it means a new protocol version.

The reference implementation is the `dre-protocol` crate. It has three parts:

- the core side, `host`;
- a plugin SDK, `plugin`, which gives Rust authors framing, the handshake and error handling;
- a conformance suite, `conformance`, that any plugin binary can be checked against.

## Naming and location

A plugin executable is named `dre-<kind>-<name>` (plus `.exe` on Windows).

- `kind` is `source`, `format` or `destination`.
- `name` matches `[a-z0-9_]+`. It is the value used in `profiles.yml` (`type: duckdb`) and in
  `output.format` (`format: xlsx`).

Examples: `dre-source-duckdb`, `dre-format-xlsx`, `dre-destination-azure_blob`.

Core looks in the project's `dre_deps/plugins` (or `DRE_PLUGINS_DIR`), in two layouts:

- `<dir>/<kind>/<name>/<version>/dre-<kind>-<name>`: versioned installs, side by side, as the
  plugin manager lays them out.
- `<dir>/dre-<kind>-<name>`: a plugin placed by hand, for development.

## Streams

| Stream | Direction | Content |
|---|---|---|
| stdin | core → plugin | frames |
| stdout | plugin → core | frames, and nothing else |
| stderr | plugin → core | free-form UTF-8 log lines, shown in core's log and quoted in errors |

A plugin must never write anything but frames to stdout.

## Frames

A frame is a 4-byte **big-endian** unsigned length `N`, then `N` bytes of body. `N` is between 1
and 2^30. The first body byte is the frame type:

| Byte | Type | Rest of the body |
|---|---|---|
| `0x4A` (`J`) | control message | one UTF-8 JSON object with a `type` field |
| `0x41` (`A`) | data | one Arrow IPC **stream** (schema message, record batches, end-of-stream marker) |

Each data frame is self-contained: it carries its own schema, so it can be decoded alone. Large
result sets are sent as many data frames, one or more batches each.

A frame with an unknown type byte, a bad length or invalid JSON is malformed. The receiver
reports it and stops.

## Handshake

The first message core sends is `hello`:

```json
{"type": "hello", "min_version": 0, "max_version": 0, "core_version": "…"}
```

The plugin picks the highest protocol version both sides support and replies:

```json
{"type": "hello", "protocol_version": 0, "kind": "source", "name": "duckdb",
 "version": "1.2.0", "capabilities": ["sessions", "read_only", "check"]}
```

If the ranges don't overlap, it replies with its own range and exits non-zero:

```json
{"type": "version_mismatch", "min_version": 1, "max_version": 2}
```

Core reports both ranges to the user. It waits 30 seconds for the hello reply
(`DRE_PLUGIN_HANDSHAKE_TIMEOUT_MS` overrides this). A plugin that hasn't answered by then is
reported as not responding.

Capabilities:

| Capability | Meaning |
|---|---|
| `sessions` | Source: one session (connection) is held across every request until `close`, so temp tables and session settings persist. Core refuses to run a Binding with more than one statement on a source without it. |
| `read_only` | Source: honours `read_only: true` on `open`. |
| `check` | Source: supports `check` (verify a statement without executing it). |
| `load` | Source: supports `load` (rows into a temporary table on the session). |
| `multi_file` | Destination: takes every file of one output in a single `deliver` (`files`), e.g. one email carrying every attachment. |

## Requests and replies

Every request gets exactly one reply. Any request may be answered with an error:

```json
{"type": "error", "message": "human-readable explanation"}
```

After an error reply the plugin keeps serving. Unknown request types, and requests meant for
another plugin kind, are answered with `error`.

### All kinds

| Request | Reply |
|---|---|
| `{"type":"describe"}` | `{"type":"describe","connection_fields":[{"name","description","required","secret","default","same_as_source"}]}` |
| `{"type":"close"}` | `{"type":"ok"}`, then the plugin exits 0 |

`describe` lists the fields a `profiles.yml` target of this plugin's type accepts. `dre init`
uses it to prompt for connection details. By default it offers fields marked `secret` as
`env_var()` references. A destination field with `"same_as_source": "<source type>"` defaults
to the value entered for a source profile of that type (for example one Databricks host for
both). Format plugins return an empty list.

When stdin closes, the plugin exits.

### Source

```json
{"type": "open", "connection": {…profile target fields…}, "read_only": false}
```

`open` starts the session. `connection` holds every field of the selected `profiles.yml` target
except `type`, with `env_var()` already rendered by core. The reply is `ok`.

```json
{"type": "execute", "sql": "select …", "row_limit": 100}
```

`execute` runs exactly one statement. Core splits files into statements itself. `row_limit` is
optional; when present, the plugin returns at most that many rows. The reply is one of:

- `{"type":"no_result","rows_affected":3}`: the statement returned no result set (DDL, DML,
  `SET`, …). `rows_affected` is optional.
- `{"type":"result","columns":["a","b"]}`, followed by **one or more** data frames (the first
  one carries the schema, even for zero rows), followed by `{"type":"result_end","rows":42}`.
  An `error` may replace `result_end` if the stream fails part-way through.

A statement produces a sheet or file in the output only if its reply is `result`.

`run_query()` in templates, `--preview` (with `row_limit`) and `dre validate --live` all use this
same request.

```json
{"type": "check", "sql": "select …"}
```

`check` verifies a statement without executing it, in the dialect's own way (for example
`EXPLAIN`). The reply is `ok`, or an `error` explaining what's wrong. Only sent to plugins that
advertise `check`.

```json
{"type": "load", "name": "countries"}
```

`load` puts rows into a temporary table (or view) on the session, for a lookup too large to
inline. Core then streams the rows as one or more data frames (the first carries the schema) and
a `{"type":"result_set_end"}`. Column types are `Utf8`, `Int64`, `Float64`, `Boolean` and
`Date32`. The plugin replies:

```json
{"type": "loaded", "relation": "dre_lookup_countries", "rows": 5000, "warning": "…"}
```

`relation` is what core puts in the SQL wherever the lookup is referenced. Set `warning` when the
database has no bulk path and the load went through ordinary SQL; core shows it to the user.
First-party plugins: DuckDB uses its appender, Postgres `COPY`, and Databricks a temporary view
built from one `VALUES` statement, with a warning. Only sent to plugins advertising `load`.

### Format

```json
{"type": "write", "path": "/…/target/run/monthly/client_a/monthly.xlsx", "format": "xlsx",
 "options": {…format options…},
 "result_sets": [{"name": "Summary", "query": "query_01", "result_index": 1,
                  "anchor": "A1", "header": true}],
 "template": {…}}
```

After `write`, core streams each result set in the order listed. Each one is sent as one or more
data frames (the first carries the schema), then `{"type":"result_set_end"}`. After the last
result set, core sends `{"type":"finish"}`. The plugin writes the file(s) and replies:

```json
{"type": "written", "files": ["/…/monthly.xlsx"]}
```

`options` are the report's resolved format options (see the YAML schema). `template` is present
only for an xlsx template output: `{"file": "<absolute path>", "bindings": [...],
"values": {"<sheet>!<cell>": "<rendered value>"}}`, with single-cell `value`s already rendered
by core.

### Destination

```json
{"type": "deliver", "local_path": "/…/target/run/…/monthly.xlsx",
 "remote_path": "s3://bucket/monthly-20260125.xlsx", "connection": {…},
 "options": {…}}
```

`deliver` copies the local file to `remote_path`. The path is already rendered, and may be absent
when the destination profile alone says where. The reply is
`{"type":"delivered","location":"<where it landed>"}`. The local file is never removed. It stays
in `target/` whatever the outcome.

`options` holds the destination entry's plugin options: every key of the entry in
`output.destination` other than `profile` and `path` (for example `to` and `subject` for email,
`channel` and `message` for Slack). Core renders their Jinja before sending, so string values
arrive final. It is `{}` when the entry has none. A plugin should reject keys it doesn't know, so
a misspelt key in a report is an error rather than silently dropped; the SDK's default does this
for plugins that take no options.

A destination that advertises the `multi_file` capability receives every file of one output in a
single request, in place of `local_path`/`remote_path`:

```json
{"type": "deliver",
 "files": [{"local_path": "/…/daily_orders.csv", "remote_path": "…/daily_orders.csv"},
           {"local_path": "/…/daily_refunds.csv", "remote_path": "…/daily_refunds.csv"}],
 "connection": {…}, "options": {…}}
```

It replies with one `delivered` for the whole set. Exactly one of `local_path` or `files` is
present. Core only sends `files` to a plugin that advertised `multi_file`; any other plugin gets
one `deliver` per file. A single-file output is always sent in the `local_path` form.

## Errors and failure

- A plugin that exits, crashes or closes stdout unexpectedly is reported with its exit status and
  its last stderr lines.
- A malformed frame from the plugin is reported as a protocol error.
- A plugin that panics or hits an internal error should reply `error` before exiting. The SDK
  does this.

## Versioning

The protocol version is a single integer. Core and plugins release independently, so each side
states the range it supports and the handshake picks the highest common version. Adding a new
optional field to a message does not change the version; anything else does.

## Conformance

`dre_protocol::conformance::run(path)` checks a plugin binary. It covers the handshake and
identity, refusal of an unsupported version, `describe`, error replies for unknown and wrong-kind
requests, behaviour on a malformed frame, and a clean exit on `close` and on end of input. For a
destination it also sends a `deliver` carrying `options` and, when `multi_file` is advertised, a
`files` delivery, and expects a reply to each (`delivered` or `error`) with the plugin still
serving. `conformance::run_with_env` runs the suite with extra environment variables. Every
first-party plugin runs the suite in its tests.
