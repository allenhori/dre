- Build `columns` from the receiving system's spec, one entry per field, in order. Confirm the
  layout back to the user as a table (name, width, alignment, padding) before writing it.
- Use `picture` when the spec is written in COBOL PIC clauses.
- A value too wide for its field fails the run unless `truncate: true`; numbers are never cut.
- Use `extension` when the file must end in something other than `.txt` (e.g. `aba`).
