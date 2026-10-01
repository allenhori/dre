#!/bin/sh
# Install the dre CLI from a GitHub Release.
#
#   curl -fsSL https://github.com/get-dre/dre/releases/latest/download/install.sh | sh
#
# DRE_VERSION     the release to install, e.g. v0.0.1-alpha (default: the newest, pre-releases
#                 included)
# DRE_INSTALL_DIR where to put dre (default: ~/.local/bin)
#
# Plugins aren't installed here: `dre deps` (or `dre init`) downloads the ones a project needs.
set -eu

repo="get-dre/dre"
dir="${DRE_INSTALL_DIR:-$HOME/.local/bin}"

fail() {
  echo "install.sh: $*" >&2
  exit 1
}

case "$(uname -s)" in
  Darwin) os=macos ;;
  Linux) os=linux ;;
  *) fail "unsupported OS $(uname -s); on Windows, download dre-<version>-windows-<arch>.zip from https://github.com/$repo/releases" ;;
esac
case "$(uname -m)" in
  arm64 | aarch64) arch=aarch64 ;;
  x86_64 | amd64) arch=x86_64 ;;
  *) fail "unsupported architecture $(uname -m)" ;;
esac

if command -v curl >/dev/null 2>&1; then
  get() { curl -fsSL "$1"; }
  get_to() { curl -fsSL -o "$2" "$1"; }
elif command -v wget >/dev/null 2>&1; then
  get() { wget -qO- "$1"; }
  get_to() { wget -qO "$2" "$1"; }
else
  fail "needs curl or wget"
fi

tag="${DRE_VERSION:-}"
if [ -z "$tag" ]; then
  # GitHub's "latest release" skips pre-releases, so take the newest v* tag from the list. The
  # repository also releases its plugins (duckdb-v1.0.0, ...), which can fill whole pages.
  page=1
  while [ -z "$tag" ] && [ "$page" -le 10 ]; do
    list="$(get "https://api.github.com/repos/$repo/releases?per_page=100&page=$page")" ||
      fail "can't list the releases of $repo"
    tag="$(printf '%s\n' "$list" | sed -n 's/.*"tag_name": *"\(v[^"]*\)".*/\1/p' | head -n 1)"
    printf '%s\n' "$list" | grep -q '"tag_name"' || break
    page=$((page + 1))
  done
  [ -n "$tag" ] || fail "can't find a release of $repo"
fi
case "$tag" in v*) ;; *) tag="v$tag" ;; esac
version="${tag#v}"

archive="dre-$version-$os-$arch.tar.gz"
base="https://github.com/$repo/releases/download/$tag"
tmp="$(mktemp -d)"
trap 'rm -rf "$tmp"' EXIT

echo "Downloading dre $tag for $os-$arch"
get_to "$base/$archive" "$tmp/$archive" || fail "can't download $base/$archive"
get_to "$base/SHA256SUMS" "$tmp/SHA256SUMS" || fail "can't download $base/SHA256SUMS"

want="$(awk -v f="$archive" '$2 == f || $2 == "*" f { print $1 }' "$tmp/SHA256SUMS")"
if command -v sha256sum >/dev/null 2>&1; then
  got="$(sha256sum "$tmp/$archive" | cut -d' ' -f1)"
else
  got="$(shasum -a 256 "$tmp/$archive" | cut -d' ' -f1)"
fi
[ -n "$want" ] && [ "$want" = "$got" ] || fail "checksum mismatch for $archive; nothing was installed"

tar -xzf "$tmp/$archive" -C "$tmp"
mkdir -p "$dir"
mv "$tmp/dre" "$dir/dre"
chmod 755 "$dir/dre"
# The receipt tells `dre system update` this is a direct install it may replace.
if [ -f "$tmp/dre-receipt.json" ]; then
  mv "$tmp/dre-receipt.json" "$dir/dre-receipt.json"
fi
echo "Installed $("$dir/dre" --version) to $dir/dre"

case ":$PATH:" in
  *":$dir:"*) ;;
  *) echo "$dir isn't on your PATH; add it, e.g.: export PATH=\"$dir:\$PATH\"" ;;
esac
