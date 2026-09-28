#!/usr/bin/env python3
"""Add one release's plugin packages to the registry index (docs/registry.md) and print it.

    registry_index.py <old packages.json, may be missing> <dist dir> <version> <download base URL>

Every `dre-plugin-<package>-<version>-<platform>.tar.gz` in the dist dir becomes an artifact of
that package's `<version>` entry. What each package provides, and its description, come from
packages.json next to this script. A version already in the old index is replaced, so re-running
a release keeps one entry per version. Other packages and versions are kept as they are.
"""

import hashlib
import json
import pathlib
import re
import sys

PROTOCOL = 0
SCHEMA = 2


def main(old_path, dist, version, base_url):
    packages = json.loads((pathlib.Path(__file__).parent / "packages.json").read_text())
    old = pathlib.Path(old_path)
    index = json.loads(old.read_text()) if old.exists() else {"schema": SCHEMA, "plugins": []}
    index["schema"] = SCHEMA

    pattern = re.compile(r"^dre-plugin-([a-z0-9_]+)-" + re.escape(version) + r"-([a-z0-9_]+-[a-z0-9_]+)\.tar\.gz$")
    found = {}
    for f in sorted(pathlib.Path(dist).iterdir()):
        m = pattern.match(f.name)
        if not m:
            continue
        name, platform = m.groups()
        if name not in packages:
            sys.exit(f"{f.name}: `{name}` isn't in packages.json")
        found.setdefault(name, {})[platform] = {
            "url": f"{base_url}/{f.name}",
            "sha256": hashlib.sha256(f.read_bytes()).hexdigest(),
        }
    missing = sorted(set(packages) - set(found))
    if missing:
        sys.exit(f"no archives for {', '.join(missing)} {version} in {dist}")

    by_name = {p["name"]: p for p in index["plugins"]}
    for name, artifacts in found.items():
        p = by_name.setdefault(name, {"name": name, "versions": []})
        p["description"] = packages[name]["description"]
        p["provides"] = packages[name]["provides"]
        p["versions"] = [v for v in p["versions"] if v["version"] != version]
        p["versions"].append({"version": version, "protocol": PROTOCOL, "artifacts": artifacts})

    index["plugins"] = [
        {k: p[k] for k in ("name", "description", "provides", "versions")}
        for p in sorted(by_name.values(), key=lambda p: p["name"])
    ]
    json.dump(index, sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    main(*sys.argv[1:])
