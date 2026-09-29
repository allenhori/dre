#!/usr/bin/env python3
"""Warn about plugin packages whose code changed but whose version didn't.

    plugin_versions.py <base ref>

Each plugin package has its own version (plugins/<package>/Cargo.toml, go/databricks/VERSION)
and is released by tagging `<package>-v<version>`. A change to a package's code since <base ref>
without a version bump is reported as a GitHub Actions warning, so a fix doesn't sit unreleased
by accident. Tests and Markdown don't count. It never fails: releasing stays a deliberate tag.
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


def source(package):
    """The package's directory and the file holding its version."""
    if package == "databricks":
        return "go/databricks", "go/databricks/VERSION"
    return f"plugins/{package}", f"plugins/{package}/Cargo.toml"


def version(text, path):
    if path.endswith("VERSION"):
        return text.strip() or None
    m = re.search(r'^version\s*=\s*"([^"]+)"', text, re.M)
    return m.group(1) if m else None


def main(base):
    stale = []
    for package in PACKAGES:
        directory, vfile = source(package)
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
                f"::warning file={vfile}::the {package} plugin's code changed but its version is still "
                f"{after}; bump it in {vfile} if this change should be released (tag {package}-v<version>)"
            )
    if not stale:
        print("Every plugin whose code changed has a new version.")


if __name__ == "__main__":
    main(sys.argv[1])
