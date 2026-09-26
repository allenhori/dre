#!/usr/bin/env bash
# Build DRE and its first-party plugins from this checkout and publish them to a local plugin
# registry, so the whole install flow (dre init → dre deps → dre run) can be tried before
# anything is released to GitHub.
#
#   scripts/local-registry.sh            # build (release) and publish
#   scripts/local-registry.sh --no-build # publish what's already built
#
# Output goes to $DRE_LOCAL_REGISTRY (default ~/.cache/dre-local-registry):
#   index.json       the registry index, in the same format as the real one (docs/registry.md)
#   artifacts/       a copy of each plugin binary, so a later rebuild can't break a checksum
#   sandbox/         an empty profiles dir for a clean trial
# It finishes by printing the environment variables that point DRE at all of this.
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
registry="${DRE_LOCAL_REGISTRY:-$HOME/.cache/dre-local-registry}"
# Every plugin gets this version. Bump it to try `dre plugin update` and dre.lock pinning.
version="${DRE_LOCAL_VERSION:-0.0.1}"
protocol=0

if [[ "${1:-}" != "--no-build" ]]; then
  (cd "$root" && cargo build --release --workspace --bins)
fi
bin_dir="${CARGO_TARGET_DIR:-$root/target}/release"
[[ -x "$bin_dir/dre" ]] || { echo "no build in $bin_dir; run without --no-build" >&2; exit 1; }

case "$(uname -s)" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) echo "unsupported OS $(uname -s); on Windows use WSL" >&2; exit 1 ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) echo "unsupported architecture $(uname -m)" >&2; exit 1 ;;
esac
platform="$os-$arch"

sha256() {
  if command -v sha256sum >/dev/null; then sha256sum "$1" | cut -d' ' -f1; else shasum -a 256 "$1" | cut -d' ' -f1; fi
}

rm -rf "$registry/artifacts"
mkdir -p "$registry/artifacts" "$registry/sandbox/profiles"

# What `dre init` shows next to each plugin.
describe() {
  case "$1/$2" in
    source/databricks) echo "Databricks SQL warehouses" ;;
    source/duckdb) echo "DuckDB database files" ;;
    source/postgres) echo "PostgreSQL" ;;
    format/csv) echo "Comma-separated values" ;;
    format/delimited) echo "Delimited text with any separator" ;;
    format/fixed_width) echo "Fixed-width text" ;;
    format/parquet) echo "Parquet" ;;
    format/xlsx) echo "Excel workbooks, including templates" ;;
    destination/s3) echo "Amazon S3 and S3-compatible storage" ;;
    destination/gcs) echo "Google Cloud Storage" ;;
    destination/azure_blob) echo "Azure Blob Storage" ;;
    destination/sftp) echo "SFTP servers" ;;
    destination/ftp) echo "FTP and FTPS servers" ;;
    destination/email) echo "Email, with the output attached" ;;
    destination/slack) echo "A Slack channel" ;;
    destination/databricks_volumes) echo "Databricks Unity Catalog Volumes" ;;
    *) echo "$2 $1" ;;
  esac
}

entries=()
for exe in "$bin_dir"/dre-*-*; do
  file="$(basename "$exe")"
  [[ -x "$exe" && "$file" != *.d ]] || continue
  rest="${file#dre-}"
  kind="${rest%%-*}"
  name="${rest#*-}"
  case "$kind" in source | format | destination) ;; *) continue ;; esac
  [[ "$name" == fixture ]] && continue # test-only plugins
  artifact="$registry/artifacts/$file-$version-$platform"
  cp "$exe" "$artifact"
  entries+=("$(printf '    {"kind": "%s", "name": "%s", "description": "%s",\n     "versions": [{"version": "%s", "protocol": %d, "artifacts": {"%s": {"url": "%s", "sha256": "%s"}}}]}' \
    "$kind" "$name" "$(describe "$kind" "$name")" "$version" "$protocol" "$platform" "$artifact" "$(sha256 "$artifact")")")
done

{
  printf '{\n  "schema": 1,\n  "plugins": [\n'
  for i in "${!entries[@]}"; do
    printf '%s' "${entries[$i]}"
    [[ $i -lt $((${#entries[@]} - 1)) ]] && printf ','
    printf '\n'
  done
  printf '  ]\n}\n'
} >"$registry/index.json"

cat <<MSG
Published ${#entries[@]} plugins ($version, $platform) to $registry/index.json

To try the full flow, run in your shell (profiles go to a sandbox, not your ~/.dre/profiles.yml):

  export PATH="$bin_dir:\$PATH"
  export DRE_REGISTRY_URL="$registry/index.json"
  export DRE_PROFILES_DIR="$registry/sandbox/profiles"
  unset DRE_PLUGINS_DIR

then: dre init   (or: dre new my_reports && cd my_reports && dre deps && dre run)
Plugins install into each project's dre_deps/, via the download cache in ~/.dre/plugins.
Rebuilt plugins keep the same version with new checksums: delete the project's dre.lock (or set
DRE_LOCAL_VERSION) before \`dre deps\`.
To start over: rm -rf "$registry/sandbox"
MSG
