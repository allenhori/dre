- Ask what the receiving system expects before changing the defaults: delimiter, quoting,
  header, encoding, line endings. Excel users often need `byte_order_mark: true` for non-ASCII
  text to show correctly.
- For tab- or pipe-separated files, use the `delimited` format with `delimiter` and `extension`.
- Project-wide defaults go under `format_options.csv` in `dre_project.yml`.
