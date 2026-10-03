---
title: "DRE plugin protocol, version 0"
description: "How a plugin talks to DRE, for writing a plugin in any language."
sidebar:
  order: 18
---

# DRE plugin protocol, version 0

Every source, format and destination in DRE is a plugin, served by a separate executable that
DRE core starts and talks to over stdin and stdout. Plugins ship in packages: one executable can
serve several plugins (the `databricks` package is a source and a destination; `object_store`
is three destinations), and core names the one it wants in the handshake. Plugins can be written
in any language. This document
is the contract between core and a plugin. Any change to it means a new protocol version.

The reference implementation is the [`dre-protocol`](https://crates.io/crates/dre-protocol) crate
(`cargo add dre-protocol`). It has its own version, apart from both DRE's and this protocol's.
It has three parts:

- the core side, `host`;
- a plugin SDK, `plugin`, which gives Rust authors framing, the handshake and error handling;
- a conformance suite, `conformance`, that any plugin binary can be checked against.

## The plugin interface

Every plugin, whatever its kind, follows one interface. Core finds a plugin by its kind and name
alone and knows nothing else about it, so a new source, format or destination needs no change
to core.

Every plugin:

1. **Answers the handshake** (`hello`) with its kind, name, version and capabilities.
2. **Describes itself** (`describe`): the connection fields a `profiles.yml` target of its type
   takes, and the options a report's config block for it takes (`option_fields`).
3. **Validates a config block** (`validate`): checks one block of options and replies with every
   problem found, without connecting or writing anything. `dre validate` and `dre run` send every
   block the project gives the plugin before anything runs.
4. **Does its one job**: a source runs statements (`open`, `execute`, and optionally `check` and
   `load`); a format writes files (`write`); a destination delivers them (`deliver`).
5. **Closes cleanly** on `close` or at the end of its input.

The config block a plugin owns:

| Kind | Block | Keys core owns (never sent) |
|---|---|---|
| format | a report's `output:` map, plus the project's `format_options.<format>` | `format`, `destination`, `template`, `extension` |
| destination | one entry of `output.destination` | `profile`, `path` |
| source | none; its settings are the profile's connection fields | |

Rules for option messages. Each problem is one sentence that names the key in backticks
(`` `quoting` must be one of `minimal`, `all`, `strings`, `none` ``). Core adds where it was found
(the file, the report and Set, the destination profile). A plugin rejects keys it doesn't
declare, so a misspelt key is an error rather than silently ignored.

In Rust, the `dre_protocol::plugin` SDK does all of this. A plugin implements one trait
(`Source`, `Format` or `Destination`) and is served with `serve_source`, `serve_format` or
`serve_destination`; a package of several passes them all to `serve_package`. A plugin declares its options as `OptionField`s (name, type,
allowed values, bounds, default, description), and adds any rule a declaration can't express in
`validate()`. The SDK answers `describe` and `validate` from those declarations, advertises
`validate`, and checks the options again before every `write` and `deliver`, so plugin code only
sees options that passed. The conformance suite checks every part of the interface.

## Naming and location

A plugin is identified by its kind and name, written `<kind>/<name>`:

- `kind` is `source`, `format` or `destination`.
- `name` matches `[a-z0-9_]+`. It is the value used in `profiles.yml` (`type: duckdb`) and in
  `output.format` (`format: xlsx`).

A package's executable is named `dre-plugin-<package>` (plus `.exe` on Windows), e.g.
`dre-plugin-object_store`. An executable serving one plugin may instead be named
`dre-<kind>-<name>`, e.g. `dre-source-duckdb`; it is then a package of that one plugin, called
`<name>`.

Core looks in the project's `dre_deps/plugins` (or `DRE_PLUGINS_DIR`), in two layouts:

- `<dir>/<package>/<version>/`: versioned installs, side by side, as the plugin manager lays
  them out. `plugin.json` there names the executable and the plugins it provides:
  `{"executable": "dre-plugin-object_store", "provides": ["destination/s3", ...]}`.
- `<dir>/dre-plugin-<package>` or `<dir>/dre-<kind>-<name>`: placed by hand, for development.
  Core asks a `dre-plugin-<package>` executable what it provides with a handshake.

## Streams

| Stream | Direction | Content |
|---|---|---|
| stdin | core → plugin | frames |
| stdout | plugin → core | frames, and nothing else |
| stderr | plugin → core | free-form UTF-8 log lines, shown in core's log and quoted in errors; a line starting `info: ` is shown to the person without `-v` (e.g. while waiting for a warehouse to start) |

A plugin must never write anything but frames to stdout.

Core sets `DRE_INTERACTIVE` in every plugin's environment: `1` when a person is at core's
terminal, `0` otherwise (a scheduler, CI). A plugin that needs a person, such as for a browser
sign-in, must fail with an explanation instead of waiting when it isn't `1`. A value already in
core's environment is passed through unchanged.

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

The first message core sends is `hello`, naming the plugin it wants served:

```json
{"type": "hello", "min_version": 0, "max_version": 0, "core_version": "…",
 "plugin": "destination/s3"}
```

The plugin picks the highest protocol version both sides support and replies as that plugin:

```json
{"type": "hello", "protocol_version": 0, "kind": "destination", "name": "s3",
 "version": "1.2.0", "capabilities": ["validate"],
 "provides": ["destination/s3", "destination/gcs", "destination/azure_blob"]}
```

- `plugin` is optional. Without it, an executable serves its first plugin (core leaves it out
  only to ask a hand-placed package what it provides).
- An executable that doesn't provide the plugin asked for replies `error` and exits non-zero.
  Core also refuses a reply whose `kind` and `name` aren't the plugin it asked for.
- `provides` lists every plugin the executable serves. It may be left out by an executable
  serving one plugin; that plugin is then its `kind` and `name`.
- An executable serving one plugin may ignore `plugin`, so plugins written before packages keep
  working unchanged.

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
| `validate` | Answers `validate`. Required: every first-party plugin advertises it (the Rust SDK does so itself). Core warns that a plugin without it predates option checks and should be updated. |

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
| `{"type":"describe"}` | `{"type":"describe","connection_fields":[{"name","description","required","secret","default","same_as_source"}],"option_fields":[{"name","type","description","required","default","choices","min","max"}]}` |
| `{"type":"validate","options":{…}}` | `{"type":"validated","errors":["…"]}` |
| `{"type":"close"}` | `{"type":"ok"}`, then the plugin exits 0 |

`describe` lists the fields a `profiles.yml` target of this plugin's type accepts. `dre init`
uses it to prompt for connection details. By default it offers fields marked `secret` as
`env_var()` references. A destination field with `"same_as_source": "<source type>"` defaults
to the value entered for a source profile of that type (for example one Databricks host for
both). Format plugins return an empty list.

`option_fields` lists the options the plugin takes (see [The plugin interface](#the-plugin-interface)).
`type` is one of `string`, `char` (exactly one character), `boolean`, `integer`, `number`,
`strings` (a string or a list of strings), `list`, `map` or `any`. `choices` limits a string to
those values; `min` and `max` bound a number, inclusive. It may be omitted when the plugin
takes no options.

`validate` checks one config block of options and replies `validated` with every problem found,
each a sentence naming the key; `errors` is empty when the block is fine. The plugin doesn't
connect, read or write anything. A destination's string values may still hold Jinja
(`{{ … }}`, `{% … %}`), which core renders only before `deliver`: such a value is checked for
presence only.

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

The report's YAML, not the reply, decides what goes in the output: each query entry makes one
tab (a sheet, or a file for single-table formats) from its file's last statement unless it has
`tab: false`. Core only checks the reply against that: a tab statement must reply `result` (zero
rows still gives a tab with its column names), and results of other statements are discarded.

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
                  "anchor": "A1", "header": true,
                  "columns": {"amount": {"format": "#,##0.00"}}}],
 "template": {…}}
```

`result_index` is always 1: each query contributes at most one result set. `anchor`, `header`
and `columns` (the query entry's per-column options: `format`, `formula`, `total`) are left out
when not set.

After `write`, core streams each result set in the order listed. Each one is sent as one or more
data frames (the first carries the schema), then `{"type":"result_set_end"}`. After the last
result set, core sends `{"type":"finish"}`. The plugin writes the file(s) and replies:

```json
{"type": "written", "files": ["/…/monthly.xlsx"], "warnings": ["…"]}
```

`warnings` (optional) are shown to the person, e.g. a value the format had to write as text.

If the write fails part-way, the plugin replies `{"type": "error", ...}` at once, without waiting
for `finish`, and then reads (and discards) the rest of the stream up to `finish`, sending
nothing more. Core stops sending data when it sees the error, so a large result isn't streamed
into a plugin that gave up. That error is the request's only reply.

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
arrive final. It is `{}` when the entry has none. Core has already had them checked with
`validate`, and the Rust SDK checks them again before calling the plugin.

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
requests, `validate` (advertised, answered, and refusing an option the plugin doesn't declare),
behaviour on a malformed frame, and a clean exit on `close` and on end of input. For a
destination it also sends a `deliver` carrying `options` and, when `multi_file` is advertised, a
`files` delivery, and expects a reply to each (`delivered` or `error`) with the plugin still
serving. `conformance::run_with_env` runs the suite with extra environment variables. Every
first-party plugin runs the suite in its tests.
