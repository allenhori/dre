---
name: dre-upgrade
description: Check for and apply updates to DRE (the `dre` CLI) and to these DRE agent skills - reports installed and latest versions, whether they're compatible, and the right update command for how each was installed. Use when the user asks "is there an update?", "upgrade dre", "update the dre skills", or another DRE skill found the installed dre outside the skills' supported range.
license: GPL-3.0-only
metadata:
  version: "2.1.0"
  dre: ">=0.2.1, <0.3.0"
---

# Update DRE and its skills

You answer "is there an update?" for both `dre` and these skills, and apply the updates the user
chooses. This is the only DRE skill that uses the network, and only because the user asked.

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

### Step 1: what's installed

- **dre:** `dre --version` (not found: hand off to `dre-install`).
- **These skills:** the `version` and `dre` range in this skill's frontmatter (`metadata`).
- **How the skills were installed**, from where this `SKILL.md` lives:
  - under `~/.claude/plugins/`: the Claude Code plugin (`dre@dre`);
  - under a `skills` folder such as `.claude/skills/`, `.agents/skills/` or `~/.agents/skills/`,
    with a `skills-lock.json` or `.skill-lock.json` nearby: the `npx skills` installer;
  - otherwise: copied by hand. Ask only if you can't tell.

### Step 2: what's available

- **dre:** `dre system update --check`. It says whether a newer release exists and, since it
  knows how dre was installed (install script, Homebrew, Scoop, pip, uv, pipx, cargo), how to
  update it. It changes nothing.
- **The skills:** the newest `skills-v*` release of `get-dre/dre`, with `gh` if it's installed:

  ```bash
  gh release list -R get-dre/dre --limit 100 --json tagName,isPrerelease,publishedAt \
    --jq '[.[] | select(.tagName | startswith("skills-v"))] | .[0]'
  ```

  otherwise with `curl` on GitHub's public API:

  ```bash
  curl -fsSL "https://api.github.com/repos/get-dre/dre/releases?per_page=100" \
    | grep -o '"tag_name": *"skills-v[^"]*"' | head -1
  ```

  Each skills release's notes state the dre range it supports: read them with
  `gh release view skills-v<version> -R get-dre/dre`, or from the same API reply's `body`.
  Prefer stable releases; mention a newer pre-release only if the installed skills are
  themselves a pre-release.

If the network or GitHub can't be reached (`gh` or `curl` fails), say so, report what's
installed, and stop: nothing else needs the network.

### Step 3: report, and offer

Tell the user, in a short table: installed and latest dre, installed and latest skills, and
whether each combination is compatible (dre's version inside the skills' range). Then offer one
choice, with a recommendation:

1. update dre;
2. update the skills;
3. both;
4. neither.

Recommend what keeps them compatible: if the latest dre is outside the installed skills' range,
update both; if only one has an update, that one. A new dre minor (0.2 after 0.1) may change
project files: say so, and point to its release notes before updating.

### Step 4: update, after confirming

Show each command and run it only after an explicit yes.

- **dre:** `dre system update`. For an install script or release zip it replaces itself; for
  Homebrew, Scoop, pip, uv, pipx or cargo it prints that manager's command (`brew upgrade dre`,
  `scoop update dre`, `uv tool upgrade dre-cli`, `pipx upgrade dre-cli`, ...), which you then run
  after confirming. `dre system update <version>` installs a given release instead. Plugins are
  separate: in a project, `dre plugin update <package>` moves one to its newest allowed version.
- **The skills, Claude Code plugin:**

  ```bash
  claude plugin marketplace update dre
  claude plugin update dre@dre
  ```

  Then Claude Code must be restarted to load them.
- **The skills, `npx skills`:** `npx skills update`. To move to a given release (or back to an
  older one that matches an older dre), install that tag again:
  `npx skills add get-dre/dre#skills-v<version>`.
- **Copied by hand:** download the release's source and replace the skill folders.

### Step 5: verify

Run `dre --version` again, and read the skills' version from their `SKILL.md` after the update
(a new session may be needed to load them). Say what changed and whether the pair is now
compatible.

## If this fails

- **`dre system update` refuses:** a `dre` built from source isn't replaced; update the checkout
  and rebuild, or install a release (`dre-install`).
- **A package manager's command fails:** run it as printed in a new terminal; for pip, use the
  same Python that `dre` was installed with (`dre system update --check` names it).
- **GitHub's rate limit** (HTTP 403 from the API): `gh` uses the user's sign-in and avoids it;
  otherwise wait, or have the user set `GITHUB_TOKEN` themselves.
- **The skills didn't change after updating:** start a new agent session; skills load when a
  session starts.
