- The same options as `csv`, for other delimiters: `delimiter: "\t"` with `extension: tsv`, or
  `delimiter: "|"`.
- Set `quoting` from the receiving system's spec. `none` fails the run on a value that would need
  quotes, naming the row and column, which is safer than a broken file.
