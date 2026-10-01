- The easiest way to try DRE: no server and no sign-in, so nothing secret (SEC-2).
- `path` is relative to the project directory. Leave `threads` and `memory_limit` out unless the
  machine is shared or small.
- Only one process can write to a DuckDB file at a time: close other programs that have it open
  before a run, or the run fails to open it.
