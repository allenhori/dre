#!/usr/bin/env python3
"""Warn about plugin packages (and dre-protocol) whose code changed but whose version didn't.

    plugin_versions.py <base ref>

Each plugin package has its own version (plugins/<package>/Cargo.toml, go/databricks/VERSION)
and is released by tagging `<package>-v<version>`. A change to a package's code since <base ref>
without a version bump is reported as a GitHub Actions warning, so a fix doesn't sit unreleased
by accident. The same goes for the dre-protocol crate, which has its own version and is published
to crates.io only when that version is new. Tests and Markdown don't count. It never fails:
releasing stays a deliberate tag.
"""

import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
PACKAGES = json.loads((ROOT / ".github/scripts/packages.json").read_text())


def git(*args):
    return subprocess.run(["git", *args], cwd=ROOT, capture_output=True, text=True).stdout


def sources():
    """Each versioned unit: its name, directory, the file holding its version, and how it's released."""
    for package, about in PACKAGES.items():
        if about.get("go"):
            yield package, about["go"], f"{about['go']}/VERSION", f"tag {package}-v<version>"
        else:
            yield package, f"plugins/{package}", f"plugins/{package}/Cargo.toml", f"tag {package}-v<version>"
    yield "dre-protocol", "crates/dre-protocol", "crates/dre-protocol/Cargo.toml", "published with the next DRE release"


def version(text, path):
    if path.endswith("VERSION"):
        return text.strip() or None
    m = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
    return m.group(1) if m else None


def main(base):
    stale = []
    for package, directory, vfile, how in sources():
        changed = [
            f
            for f in git("diff", "--name-only", f"{base}...HEAD", "--", directory).split()
            if "/tests/" not in f and not f.endswith(".md") and not f.endswith("_test.go")
        ]
        if not changed:
            continue
        before = version(git("show", f"{base}:{vfile}"), vfile)
        after = version((ROOT / vfile).read_text(), vfile)
        if before == after:
            stale.append(package)
            print(
                f"::warning file={vfile}::{package}'s code changed but its version is still "
                f"{after}; bump it in {vfile} if this change should be released ({how})"
            )
    if not stale:
        print("Everything whose code changed has a new version.")


if __name__ == "__main__":
    main(sys.argv[1])
