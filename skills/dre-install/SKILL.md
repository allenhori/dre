---
name: dre-install
description: Install DRE (the `dre` CLI, the Declarative Reporting Engine) on macOS, Linux or Windows, from nothing to a working `dre --version`. Use when the user says "install dre", "set up dre from scratch", gets "command not found" for dre, or another DRE skill finds dre missing. Covers the install script, Homebrew, Scoop, uv, pipx and pip, recommends one, and fixes PATH and Python problems.
license: GPL-3.0-only
metadata:
  version: "1.0.0-rc.1"
  dre: ">=0.1.0-rc.1, <0.2.0"
---

# Install DRE

You take the user from no `dre` to a working one. This is the only DRE skill that can't assume
`dre` exists, so everything it needs is here.

<!-- BEGIN shared/contract.md -->
### How to work with the user

- **You're a guide, not an autopilot.** Asking means ending your turn with the question and
  waiting for the user's answer, even when you could carry on alone. Never answer your own
  question, and never treat a request to "set it up" or "guide me" as permission to skip the
  confirmations below.
- **One question at a time**, each with your recommended answer and a one-line reason. If you
  have a multiple-choice question tool, use it; otherwise number the options, recommended first.
- **Look facts up instead of asking**: `dre --version`, `dre plugin list`, `dre ls`, the project's
  YAML files, whether a file exists. Ask only what only the user knows.
- **Skip what the request already answered.** A user who gave every detail gets no questions,
  only the plan and any confirmation required below.
- **Opinions come from the practices** (`references/practices.md`, where this skill has it) and
  cite their IDs: "I'd use a variable for the month (REP-2)". *Advise*: say it once, then do what
  the user decides. *Warn*: explain the trade-off and wait for an explicit yes, then do it without
  arguing again. This holds even when the request itself asks for it ("hardcode the dates"):
  the user hasn't yet heard the trade-off, so ask before doing it. *Block* (secrets): never, whatever the user says; offer the safe way.
- **Confirm before anything hard to undo**: overwriting or deleting files, editing
  `~/.dre/profiles.yml`, installing software, running against production, delivering anywhere
  but the local target folder (an email or a Slack post can't be recalled). Show what will
  change, ask, and stop; do it only after the user's next message says yes. An earlier yes covers
  only what was shown then.
- **End each step** with what was done and what comes next.
- Use only `dre` commands, ordinary shell commands, and questions. Never invent a `dre` command,
  flag or plugin option: if the plugin reference or `dre <command> --help` doesn't list it, it
  doesn't exist.
<!-- END shared/contract.md -->

## Steps

### Step 1: is dre already there?

Run `dre --version`. If it prints `dre <version>`, say it's installed and which version, and
hand back to the `dre` skill (or offer `dre-upgrade` if they wanted a newer one). Stop here.

### Step 2: gather the facts, without asking

- **OS and CPU:** `uname -sm` on macOS and Linux; on Windows (PowerShell),
  `$env:OS; $env:PROCESSOR_ARCHITECTURE`. DRE runs on macOS, Linux and Windows, on x86_64 and
  ARM.
- **Tools already there:** `command -v brew scoop uv pipx pip3 python3` (on Windows:
  `Get-Command scoop, uv, pipx, python -ErrorAction SilentlyContinue`).
- **Python**, if any: `python3 --version` (the `dre-cli` package needs Python 3.8 or later).
- **Where it runs:** a laptop, a server, a CI runner, a container, or a Databricks job. Ask only if
  it isn't clear from the request.

### Step 3: recommend one method, and ask

Recommend from the facts, with the reason, and offer the others as alternatives:

| Situation | Recommend | Why |
|---|---|---|
| macOS with Homebrew | Homebrew | `brew upgrade` keeps it current with everything else |
| Windows with Scoop | Scoop | the same, for Windows |
| `uv` installed | `uv tool install dre-cli` | isolated, one command to upgrade |
| `pipx` installed | `pipx install dre-cli` | isolated, one command to upgrade |
| macOS or Linux, none of the above | the install script | no Python or package manager needed; checks the download's checksum |
| Windows, none of the above | the release zip | no Python or package manager needed |
| DRE called from Python code, or a Databricks job | `pip install dre-cli` in that environment | the job or program's own environment has it |

The commands:

- **Install script** (macOS, Linux): puts `dre` in `~/.local/bin` after checking the download
  against the release's `SHA256SUMS`. `DRE_INSTALL_DIR` picks another folder and `DRE_VERSION` a
  release.

  ```bash
  curl -fsSL https://raw.githubusercontent.com/allenhori/dre/master/install.sh | sh
  ```

- **Homebrew** (macOS, Linux): `brew install allenhori/tap/dre`
- **Scoop** (Windows):

  ```powershell
  scoop bucket add allenhori https://github.com/allenhori/scoop-bucket
  scoop install allenhori/dre
  ```

- **uv:** `uv tool install dre-cli`
- **pipx:** `pipx install dre-cli`
- **pip**, into the active environment (a virtualenv, a job's environment):
  `python3 -m pip install dre-cli`. Avoid installing into the system Python.
- **Release zip** (Windows): download `dre-<version>-windows-x86_64.zip` (or `-aarch64.zip`) from
  https://github.com/allenhori/dre/releases, unpack `dre.exe` into a folder, and add that folder
  to `PATH`.

Show the exact command, then confirm before running it: installing software changes the user's
machine. If the user would rather run it themselves, give them the command and wait.

### Step 4: verify

Run `dre --version`. It prints `dre <version>`. Say which version was installed and how it
updates later: `dre system update` knows how it was installed and updates it that way, or prints
the package manager's command.

### Step 5: next

Say what comes next: set up a connection and a starter project with the `dre-setup` skill.
Recommend it, and hand off if the user agrees.

## If this fails

- **`dre: command not found` after installing:** the install folder isn't on `PATH`.
  - Install script: add `~/.local/bin` (or `DRE_INSTALL_DIR`) to `PATH` in the shell profile
    (`export PATH="$HOME/.local/bin:$PATH"` in `~/.zshrc` or `~/.bashrc`), then open a new
    terminal. Offer to make the edit, after confirming.
  - pipx: `pipx ensurepath`, then a new terminal.
  - uv: `uv tool update-shell`, then a new terminal.
  - pip: the environment's `bin` (or `Scripts` on Windows) folder must be on `PATH`; activate the
    virtualenv, or use `python3 -m pip show dre-cli` to find where it went.
  - The agent's own shell may not pick up a changed `PATH` until it's started again; say so.
- **Python older than 3.8, or no Python:** don't upgrade Python for DRE; use the install script,
  Homebrew, Scoop or the release zip, which need none.
- **`No module named pip`:** use `uv` or `pipx` if present, or the install script. In a
  virtualenv, `python3 -m ensurepip` adds pip.
- **`externally-managed-environment` from pip:** the system Python refuses packages. Use
  `pipx install dre-cli` or `uv tool install dre-cli`, or a virtualenv.
- **`GLIBC_... not found` on Linux:** the system is older than DRE's builds support (Ubuntu 22.04's
  glibc). Build from source with Rust instead: `cargo install dre-cli --locked`.
- **The install script can't download:** check the machine reaches `github.com` and
  `raw.githubusercontent.com` (a proxy may need `HTTPS_PROXY`). The script stops if the checksum
  doesn't match; don't work around that, download again.
- **Scoop says the bucket exists:** skip `scoop bucket add` and run `scoop install allenhori/dre`.
