#!/usr/bin/env bash
# Lay out the pinned Tectonic engine as <out-dir>/engine-<KEY>/ with the binary, the whole package
# bundle and the format files built for them, all read-only. This is the same layout
# `galley engine install` makes; the image build and CI use this script so they need no Galley
# binary.
#
#   deploy/tectonic-engine.sh <out-dir> [bundle-tar]
#
# Every download is checked against deploy/tectonic.pins before anything is unpacked. Give a local
# copy of the pinned bundle tar to skip its 2.9 GB download. TARGETARCH (amd64, arm64) overrides the
# machine's architecture, as in a Docker build.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
pins="$here/tectonic.pins"
pin() {
  local v
  v="$(sed -n "s/^$1=//p" "$pins")"
  [[ -n "$v" ]] || { echo "missing pin $1" >&2; exit 1; }
  printf '%s' "$v"
}

[[ $# -ge 1 ]] || { echo "usage: $0 <out-dir> [bundle-tar]" >&2; exit 2; }
out="$1"
bundle_tar="${2:-}"

case "${TARGETARCH:-$(uname -m)}" in
  amd64 | x86_64) arch=X86_64 triple=x86_64-unknown-linux-musl ;;
  arm64 | aarch64) arch=AARCH64 triple=aarch64-unknown-linux-musl ;;
  *) echo "no pinned Tectonic build for ${TARGETARCH:-$(uname -m)}" >&2; exit 1 ;;
esac
version="$(pin TECTONIC_VERSION)"
key="$(pin "KEY_$arch")"
digest="$(pin BUNDLE_DIGEST)"
target="$out/engine-$key"

if [[ "$(cat "$target/bundle/SHA256SUM" 2>/dev/null)" == "$digest" ]] && compgen -G "$target/cache/formats/*.fmt" >/dev/null; then
  echo "Tectonic engine already at $target"
  exit 0
fi

mkdir -p "$out"
# Named like Galley's own partial folders, so a stopped run is swept by Galley later.
work="$(mktemp -d "$out/.engine-script-XXXXXX.partial")"
cleanup() {
  chmod -R u+w "$work" 2>/dev/null || true
  rm -rf "$work"
}
trap cleanup EXIT
engine="$work/engine"
mkdir -p "$engine/bundle" "$engine/cache" "$work/format"

fetch() {
  curl --proto '=https' --proto-redir '=https' --max-redirs 5 --fail --silent --show-error --location \
    --retry 2 -o "$2" "$1"
}

echo "Downloading Tectonic $version…"
fetch "https://github.com/tectonic-typesetting/tectonic/releases/download/tectonic%40$version/tectonic-$version-$triple.tar.gz" \
  "$work/tectonic.tar.gz"
echo "$(pin "TECTONIC_${arch}_ARCHIVE_SHA256")  $work/tectonic.tar.gz" | sha256sum --check --quiet
tar -xzf "$work/tectonic.tar.gz" -C "$work" tectonic
echo "$(pin "TECTONIC_${arch}_BINARY_SHA256")  $work/tectonic" | sha256sum --check --quiet
install -m 0555 "$work/tectonic" "$engine/tectonic"

if [[ -z "$bundle_tar" ]]; then
  echo "Downloading the package bundle (2.9 GB)…"
  bundle_tar="$work/bundle.tar"
  fetch "$(pin BUNDLE_URL)" "$bundle_tar"
fi
[[ "$(stat -c %s "$bundle_tar")" == "$(pin BUNDLE_SIZE)" ]] || { echo "the bundle is not the pinned size" >&2; exit 1; }
echo "$(pin BUNDLE_SHA256)  $bundle_tar" | sha256sum --check --quiet
# The hash is checked, so this is the pinned tar: flat, regular files only.
[[ "$(tar -tf "$bundle_tar" | grep -vc '^[^/]*$' || true)" == "0" ]] || { echo "the bundle has paths" >&2; exit 1; }
[[ "$(tar -tf "$bundle_tar" | wc -l)" == "$(pin BUNDLE_ENTRIES)" ]] || { echo "the bundle is not the pinned file count" >&2; exit 1; }
echo "Unpacking the package bundle…"
tar -xf "$bundle_tar" -C "$engine/bundle" --no-same-owner --no-same-permissions
[[ "$bundle_tar" != "$work/bundle.tar" ]] || rm -f "$bundle_tar"
find "$engine/bundle" -type f -exec chmod 0444 {} +
[[ "$(cat "$engine/bundle/SHA256SUM")" == "$digest" ]] || { echo "the bundle's SHA256SUM differs" >&2; exit 1; }

echo "Preparing the formats…"
printf '\\documentclass{article}\n\\begin{document}\nGalley\n\\end{document}\n' >"$work/format/galley-format.tex"
build_formats() {
  env TECTONIC_CACHE_DIR="$engine/cache" XDG_CACHE_HOME="$work/format" HOME="$work/format" SOURCE_DATE_EPOCH=0 \
    "$@" "$engine/tectonic" -X compile -b "$engine/bundle" --only-cached -o "$work/format" "$work/format/galley-format.tex" \
    >/dev/null
}
# Without network when bubblewrap works here; a docker build cannot, and its inputs are all pinned.
build_formats bwrap --dev-bind / / --unshare-net --die-with-parent -- 2>/dev/null || build_formats
compgen -G "$engine/cache/formats/*.fmt" >/dev/null || { echo "Tectonic left no format file" >&2; exit 1; }
chmod 0444 "$engine"/cache/formats/*
chmod 0555 "$engine/cache/formats" "$engine/cache" "$engine/bundle"

if [[ -e "$target" ]]; then
  chmod -R u+w "$target"
  rm -rf "$target"
fi
mv "$engine" "$target"
# Only now: moving a folder to another parent needs it writable.
chmod 0555 "$target"
echo "Tectonic engine ready at $target"
