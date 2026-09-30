# DRE agent skills

Skills for coding agents (Claude Code, Codex, Cursor, GitHub Copilot, Gemini CLI and any other
agent that reads [Agent Skills](https://agentskills.io)) that guide you from "never heard of
DRE" to a delivered report. They ask one question at a time, each with a recommended answer and
why; look up what they can instead of asking; give advice from one reviewed
[practices file](../docs/practices.md), citing its rule IDs; confirm before anything hard to
undo; and never ask for, repeat or store a password, token or key.

| Skill | What it does |
|---|---|
| `dre` | Works out where you are (dre installed? a project? profiles?) and hands off to the right skill. Start here: "help me with dre". |
| `dre-install` | Installs `dre` on macOS, Linux or Windows (install script, Homebrew, Scoop, uv, pipx, pip), and fixes PATH and Python problems. |
| `dre-setup` | Installs the source plugin, writes the connection profile with a safe sign-in, creates a starter project and validates it. |
| `dre-report` | Designs and writes a report around your goal: SQL, tabs, variables, format and destinations. Also changes existing reports. |
| `dre-run` | Validates, previews, runs and delivers, confirming before production runs and real deliveries, and explains failures. |
| `dre-upgrade` | Checks for updates to `dre` and to these skills, and applies the ones you choose. The only skill that uses the network. |

Each skill checks your installed `dre` against the versions it supports before doing anything,
and warns if they don't match.

## Install

The install channels serve the newest skills **release**, never unreleased work on `master`.

**Claude Code:**

```bash
claude plugin marketplace add allenhori/dre
claude plugin install dre@dre
```

(or `/plugin marketplace add allenhori/dre` and `/plugin install dre@dre` inside a session). To
update: `claude plugin marketplace update dre`, then `claude plugin update dre@dre`, and restart.

**Codex, Cursor, GitHub Copilot, Gemini CLI and other agents**, with the
[`skills`](https://github.com/vercel-labs/skills) installer (it asks which agents to install
for; `-a <agent>` picks one, `-g` installs for your user rather than the project):

```bash
npx skills add allenhori/dre#skills-latest
```

To update: `npx skills update`.

**A pinned version**: install a release's tag instead, e.g.
`npx skills add allenhori/dre#skills-v1.0.0`. This also works for Claude Code
(`-a claude-code`), in place of the marketplace, when you want to stay on one release.

Then ask your agent something like "help me with dre", "install dre", or "add a Slack
destination to my monthly report".

## Versions

The skills have their own version, released as `skills-v<version>` tags, apart from DRE's and
the plugins'. Each release supports a range of DRE minor versions (every `SKILL.md` states it as
`metadata.dre`, and each release's notes repeat it):

| Skills | Supports dre |
|---|---|
| 1.x | 0.1.x |

A new DRE minor gets a skills release that supports it. Dropping support for an older DRE minor
is a new major skills version, so if you stay on an older DRE, pin the skills release that
matches it. `dre-upgrade` tells you where you stand.

## For maintainers

The skills are one [Claude Code plugin](.claude-plugin/plugin.json) whose skill folders are the
folders here; the marketplace entry is [`.claude-plugin/marketplace.json`](../.claude-plugin/marketplace.json)
at the repository root. Each folder is also a standalone Agent Skill, so an installed skill has
only its own folder. Content shared between skills is therefore written once and copied in by
`.github/scripts/skills.py`:

| Source | Copied to |
|---|---|
| `shared/contract.md`, `shared/secrets.md`, `shared/version-check.md` | each `SKILL.md`, between its `<!-- BEGIN shared/... -->` and `<!-- END shared/... -->` markers |
| [`docs/practices.md`](../docs/practices.md) | `<skill>/references/practices.md` |
| each plugin's `describe` reply, its sections of [`docs/plugins.md`](../docs/plugins.md), and `shared/guide-notes/<kind>-<name>.md` | `<skill>/references/plugins/<kind>-<name>.md`, generated |

Never edit the copies. After changing a source, a plugin's fields or options, or the plugin
docs, build `dre` and the plugins and regenerate:

```bash
cargo build --workspace --bins
(cd go/databricks && go build -o ../../target/debug/dre-plugin-databricks .)
python3 .github/scripts/skills.py sync
python3 .github/scripts/skills.py generate --plugins-dir target/debug
python3 .github/scripts/skills.py check --dre target/debug/dre --plugins-dir target/debug
```

The Skills workflow runs `check` on every change to the skills, the plugin docs, the CLI or a
plugin. It fails when a copy is stale; a plugin declares a field or option that its section of
the plugin docs doesn't name, or that has no description; a skill names a `dre` command or flag
that the built `dre` doesn't have, a practice ID that doesn't exist, or a plugin that doesn't
exist; a frontmatter is invalid; or a skill's `metadata.version` or `metadata.dre` doesn't
match the plugin version and the workspace's DRE version.

Rules for the content: only `dre` commands, ordinary shell and questions to the user (no
agent-specific features); opinions go in the practices file, never only in a skill; nothing is
written about `dre_utils` until it's released.

**Releasing:** bump `version` in `.claude-plugin/plugin.json` and `metadata.version` in every
`SKILL.md` (and `metadata.dre` when the supported DRE range changes), merge, then push an
annotated tag:

```bash
git tag -a skills-v1.0.1 -m "Fixes the Slack scopes advice"
git push origin skills-v1.0.1
```

The Skills release workflow checks the tag against the version, runs the checks at the tag's
commit, creates the GitHub release with the tag's message and the supported range, and moves
the `skills-latest` branch (what the channels serve) to the tag when it's the newest stable
release, or a pre-release before any stable one exists.
