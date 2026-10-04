# Contributing to DRE

Thanks for helping. Bug reports, ideas and pull requests are all welcome.

## Before you start

- **Bugs and ideas:** open an issue first. For a bug, include the `dre --version` output, your
  OS, the plugin and its version, and the smallest project that shows the problem.
- **Larger changes** (a new feature, a new plugin, a change to the plugin protocol or to project
  YAML): open an issue to discuss it before writing the code, so we agree on the shape first.
- **Security problems:** don't open a public issue. Report them privately through the
  repository's **Security → Report a vulnerability** tab.

## How changes get in

`master` is protected. Nobody pushes to it directly:

1. Fork the repository and create a branch from `master`.
2. Make your change, with tests (see below).
3. Open a pull request against `master` and describe what it changes and why.
4. Sign the [Contributor License Agreement](CLA.md) (a bot asks you on your first pull request;
   see below).
5. CI must pass, and a maintainer must approve the pull request. New commits after an approval
   need a fresh one, and every review conversation must be resolved.
6. A maintainer merges it, squashing it into a single commit.

## Contributor License Agreement

DRE is released under the GPL-3.0, and is also offered under a commercial license. To be able
to do both with your contribution, we need you to sign the [CLA](CLA.md) once. You keep the
copyright in your work.

On your first pull request, the CLA check fails and the CLA bot posts a comment. To sign, post
this sentence as a new comment on the pull request, exactly as written and with nothing else in
the comment:

```text
I have read the CLA Document and I hereby sign the CLA
```

The bot records your signature and the check turns green. If it stays red, post a comment
containing only `recheck`. You sign once; later pull requests pass without it. Pull requests
can't be merged until every author has signed.

## Building and testing

You need a recent stable Rust toolchain, and Go for the Databricks package (`go/databricks`).

```sh
cargo build --workspace --bins
cargo test --workspace
```

Before you push, run the same checks as CI:

```sh
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
gofmt -l go                              # prints nothing when the Go code is formatted
(cd go/databricks && go vet ./... && go test ./...)
```

The plugin integration tests (Postgres, object store, SFTP, FTP, email) run against local
emulators, and each one skips itself when its service isn't configured. See the `integration`
job in [`.github/workflows/ci.yml`](.github/workflows/ci.yml) for the services and environment
variables they use.

## Guidelines

- **Keep pull requests focused:** one change per pull request.
- **Tests:** a bug fix comes with a test that fails without it; a feature comes with tests
  for it.
- **Docs:** update `README.md` or `docs/` when you change behaviour users can see. Practices
  and opinions go in [`docs/practices.md`](docs/practices.md).
- **Plugins** are versioned on their own. If you change a plugin's code, bump the `version` in
  that plugin's `Cargo.toml` (CI warns when you forget).
- **Compatibility:** DRE is pre-1.0 and still evolving, so fixing a design properly wins over
  keeping compatibility layers. A minor release (0.2 → 0.3) may break a project; so may a patch
  release that corrects a design mistake in the release just before it (as 0.2.1 did for
  0.2.0's targets). Every break goes in the release notes and the migration guide, and a
  removed key or name is an error that says what to write instead, never silently ignored.
  Renamed keys may keep working, with a warning, where that costs little.
- **Agent skills** in `skills/`: edit the sources, never the generated copies (the
  `references/` folders and the `shared/` blocks in `SKILL.md`), then run
  `.github/scripts/skills.py sync` and `generate`. See [`skills/README.md`](skills/README.md).
- **Your own work only:** submit only code you wrote, or code whose license allows it to be
  included here (say where it came from in the pull request). Don't copy code from other
  projects with incompatible licenses.

## Code of conduct

Be respectful and constructive. Maintainers may remove comments, or block people, who aren't.
