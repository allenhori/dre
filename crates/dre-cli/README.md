# dre-cli

The `dre` command of [DRE](https://github.com/allenhori/dre), the Declarative Reporting Engine:
SQL in, a correctly formatted file out. You declare reports as YAML and `.sql` files; DRE runs the
SQL against your warehouse, writes csv, delimited, fixed-width, parquet or xlsx, and delivers the
file (object storage, SFTP/FTP, Databricks Volumes, email, Slack).

```bash
cargo install dre-cli --locked
dre --help
```

Sources, formats and destinations are plugins that `dre` downloads on demand for the projects that
declare them. See the [README](https://github.com/allenhori/dre#readme) for everything else, and
for the other ways to install DRE (install.sh, pip, Homebrew, Scoop).
