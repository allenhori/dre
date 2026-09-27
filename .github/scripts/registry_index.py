#!/usr/bin/env python3
"""Add one release's plugins to the registry index (docs/registry.md) and print the new index.

    registry_index.py <old index.json, may be missing> <dist dir> <version> <download base URL>

Every `dre-<kind>-<name>-<version>-<platform>.tar.gz` in the dist dir becomes an artifact of
that plugin's `<version>` entry. A version already in the old index is replaced, so re-running a
release keeps one entry per version. Other plugins and versions are kept as they are.
"""

import hashlib
import json
import pathlib
import re
import sys

PROTOCOL = 0

# What `dre init` shows next to each plugin.
DESCRIPTIONS = {
    "source/databricks": "Databricks SQL warehouses",
    "source/duckdb": "DuckDB database files",
    "source/postgres": "PostgreSQL",
    "format/csv": "Comma-separated values",
    "format/delimited": "Delimited text with any separator",
    "format/fixed_width": "Fixed-width text",
    "format/parquet": "Parquet",
    "format/xlsx": "Excel workbooks, including templates",
    "destination/s3": "Amazon S3 and S3-compatible storage",
    "destination/gcs": "Google Cloud Storage",
    "destination/azure_blob": "Azure Blob Storage",
    "destination/sftp": "SFTP servers",
    "destination/ftp": "FTP and FTPS servers",
    "destination/email": "Email, with the output attached",
    "destination/slack": "A Slack channel",
    "destination/databricks_volumes": "Databricks Unity Catalog Volumes",
    "destination/databricks_workspace": "Databricks workspace files",
}


def main(old_path, dist, version, base_url):
    old = pathlib.Path(old_path)
    index = json.loads(old.read_text()) if old.exists() else {"schema": 1, "plugins": []}

    pattern = re.compile(
        r"^dre-(source|format|destination)-([a-z0-9_]+)-" + re.escape(version) + r"-([a-z0-9_]+-[a-z0-9_]+)\.tar\.gz$"
    )
    found = {}
    for f in sorted(pathlib.Path(dist).iterdir()):
        m = pattern.match(f.name)
        if not m:
            continue
        kind, name, platform = m.groups()
        found.setdefault((kind, name), {})[platform] = {
            "url": f"{base_url}/{f.name}",
            "sha256": hashlib.sha256(f.read_bytes()).hexdigest(),
        }
    if not found:
        sys.exit(f"no plugin archives for {version} in {dist}")

    plugins = {(p["kind"], p["name"]): p for p in index["plugins"]}
    for (kind, name), artifacts in found.items():
        p = plugins.setdefault(
            (kind, name),
            {"kind": kind, "name": name, "description": "", "versions": []},
        )
        p["description"] = DESCRIPTIONS.get(f"{kind}/{name}", p["description"] or f"{name} {kind}")
        p["versions"] = [v for v in p["versions"] if v["version"] != version]
        p["versions"].append({"version": version, "protocol": PROTOCOL, "artifacts": artifacts})

    index["plugins"] = sorted(plugins.values(), key=lambda p: (p["kind"], p["name"]))
    json.dump(index, sys.stdout, indent=2)
    print()


if __name__ == "__main__":
    main(*sys.argv[1:])
