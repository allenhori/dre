#!/usr/bin/env python3
"""What a release tag releases, for the release workflow.

    release_plan.py <tag>

`v<version>` releases DRE itself (the `dre` CLI); `<package>-v<version>` releases one plugin
package, e.g. `duckdb-v1.0.0` or `object_store-v1.2.0-rc.1`. The tag's version must be the one
in the source: the workspace version for DRE, the package's own `Cargo.toml` version for a Rust
plugin, `go/databricks/VERSION` for the Databricks package.

Prints `key=value` lines for $GITHUB_OUTPUT: kind (core or plugin), package, version, prerelease
(true or false) and go (true for the Go package).
"""

import json
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parents[2]
PACKAGES = json.loads((ROOT / ".github/scripts/packages.json").read_text())
SEMVER = re.compile(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?")


def metadata(crate):
    """A workspace crate's metadata, as Cargo resolves it."""
    meta = json.loads(
        subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version", "1"],
            cwd=ROOT, check=True, capture_output=True, text=True,
        ).stdout
    )
    return next(p for p in meta["packages"] if p["name"] == crate)


def cargo_version(crate):
    return metadata(crate)["version"]


def plan(tag):
    if tag.startswith("v"):
        kind, package, version = "core", "dre", tag[1:]
        source, where = cargo_version("dre-cli"), "the workspace version"
    else:
        m = re.fullmatch(r"([a-z0-9_]+)-v(.+)", tag)
        if not m:
            sys.exit(f"tag {tag}: expected v<version> (DRE) or <package>-v<version> (a plugin)")
        kind, (package, version) = "plugin", m.groups()
        if package not in PACKAGES:
            sys.exit(f"tag {tag}: `{package}` isn't a plugin package (.github/scripts/packages.json)")
        if package == "databricks":
            f = ROOT / "go/databricks/VERSION"
            source, where = f.read_text().strip(), "go/databricks/VERSION"
        else:
            source = cargo_version(f"dre-plugin-{package}")
            where = f"plugins/{package}/Cargo.toml"
    if not SEMVER.fullmatch(version):
        sys.exit(f"tag {tag}: `{version}` isn't a version")
    if version != source:
        sys.exit(f"tag {tag} is version {version}, but {where} says {source}")
    if kind == "core":
        # dre-cli, as published to crates.io, must depend on the dre-core of the same release.
        req = next(d["req"] for d in metadata("dre-cli")["dependencies"] if d["name"] == "dre-core")
        if req != f"={version}":
            sys.exit(f"tag {tag}: the workspace's dre-core dependency is `{req}`; make it `={version}` in Cargo.toml")
    return {
        "kind": kind,
        "package": package,
        "version": version,
        "prerelease": str("-" in version).lower(),
        "go": str(package == "databricks").lower(),
    }


if __name__ == "__main__":
    for k, v in plan(sys.argv[1]).items():
        print(f"{k}={v}")
