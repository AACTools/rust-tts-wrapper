#!/usr/bin/env bash
# Download a sherpa-onnx prebuilt archive and verify it against the
# pinned SHA-256 (issue #144: the desktop release legs used to link
# whatever bytes the upstream CDN served). Prints the directory
# containing the verified archive on stdout — export it as
# SHERPA_ONNX_ARCHIVE_DIR so sherpa-onnx-sys's build script copies the
# verified archive instead of downloading its own.
#
# Usage: fetch-sherpa-archive.sh <archive-filename>
#   dir=$(scripts/ci/fetch-sherpa-archive.sh \
#           sherpa-onnx-v2.0.1-linux-x64-static-lib.tar.bz2)
#   echo "SHERPA_ONNX_ARCHIVE_DIR=$dir" >> "$GITHUB_ENV"
set -euo pipefail

archive=${1:?usage: fetch-sherpa-archive.sh <archive-filename>}
here=$(cd "$(dirname "$0")" && pwd)
manifest="$here/sherpa-archive-hashes.txt"
# Release tag: "sherpa-onnx-v2.0.1-..." -> "v2.0.1"
# (grep guards: under set -euo pipefail a no-match grep would exit
# silently before the friendly diagnostics below — loop-review finding)
tag=$(grep -o 'v[0-9][0-9.]*' <<<"$archive" | head -1 || true)
if [[ -z "$tag" ]]; then
  echo "fetch-sherpa-archive.sh: cannot parse a version tag from '$archive'" >&2
  exit 2
fi
base_url="https://github.com/k2-fsa/sherpa-onnx/releases/download/$tag"

expected=$(grep -F "  $archive" "$manifest" | head -1 | awk '{print $1}' || true)
if [[ -z "$expected" ]]; then
  echo "fetch-sherpa-archive.sh: no pinned hash for '$archive' in $manifest" >&2
  exit 2
fi

out="${RUNNER_TEMP:-/tmp}/sherpa-archive-verified"
mkdir -p "$out"

if [[ -f "$out/$archive" ]] &&
  (cd "$out" && printf '%s  %s\n' "$expected" "$archive" | shasum -a 256 -c --status); then
  echo "$out"
  exit 0
fi

rm -f "$out/$archive"
curl --fail --silent --show-error --location --retry 3 \
  "$base_url/$archive" -o "$out/$archive"
(cd "$out" && printf '%s  %s\n' "$expected" "$archive" | shasum -a 256 -c)
echo "$out"
