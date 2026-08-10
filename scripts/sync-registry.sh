#!/usr/bin/env bash
#
# sync-registry.sh - pull a pinned, checksummed release of the sherpa-onnx
# TTS model registry into src/merged_models.json.
#
# The registry lives in its own repo (AACTools/sherpa-onnx-tts-models) and
# is published as a tagged GitHub release. This script fetches a specific
# tag, verifies the SHA-256, drops the JSON into src/, and records the
# provenance in src/registry-version.txt so every build is reproducible and
# auditable. Commit both files after running - the build itself needs no
# network.
#
# Usage:
#   ./scripts/sync-registry.sh v2026-08-10      # sync a specific tag
#   ./scripts/sync-registry.sh                   # re-sync the pinned tag
#
# Override the source repo with REGISTRY_REPO=owner/name.
#
# Requirements: curl, sha256sum (or shasum on macOS), python3.

set -euo pipefail

# Portable sha256: prefer coreutils sha256sum, fall back to macOS shasum.
sha256() {
  if command -v sha256sum >/dev/null 2>&1; then
    sha256sum "$1" | awk '{print $1}'
  else
    shasum -a 256 "$1" | awk '{print $1}'
  fi
}

REGISTRY_REPO="${REGISTRY_REPO:-AACTools/sherpa-onnx-tts-models}"
SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
SRC_DIR="$(cd "$SCRIPT_DIR/.." && pwd)/src"
VERSION_FILE="$SRC_DIR/registry-version.txt"
MODELS_FILE="$SRC_DIR/merged_models.json"

# Resolve the tag: explicit arg wins, else read the currently-pinned tag.
if [[ $# -ge 1 ]]; then
  TAG="$1"
elif [[ -f "$VERSION_FILE" ]]; then
  TAG="$(grep -E '^tag:' "$VERSION_FILE" | head -1 | awk '{print $2}')"
  if [[ -z "$TAG" ]]; then
    echo "error: no tag argument and $VERSION_FILE has no 'tag:' line" >&2
    exit 2
  fi
  echo "Re-syncing pinned tag $TAG"
else
  echo "usage: $0 <tag>   (e.g. v2026-08-10)" >&2
  echo "       first sync requires an explicit tag" >&2
  exit 2
fi

BASE_URL="https://github.com/${REGISTRY_REPO}/releases/download/${TAG}"
TMP="$(mktemp -d)"
trap 'rm -rf "$TMP"' EXIT

echo "Fetching registry $TAG from $REGISTRY_REPO..."

# --- Download models.json + its checksum ------------------------------------
curl -fsSL -o "$TMP/models.json"        "$BASE_URL/models.json"
# The checksum file is optional in older releases - warn, don't fail, if missing.
if curl -fsSL -o "$TMP/models.json.sha256" "$BASE_URL/models.json.sha256"; then
  EXPECTED="$(awk '{print $1}' "$TMP/models.json.sha256")"
else
  echo "  (no models.json.sha256 in this release - skipping checksum verification)"
  EXPECTED=""
fi

# --- Verify the checksum ----------------------------------------------------
ACTUAL="$(sha256 "$TMP/models.json")"
if [[ -n "$EXPECTED" ]]; then
  if [[ "$ACTUAL" != "$EXPECTED" ]]; then
    echo "error: checksum mismatch" >&2
    echo "  expected: $EXPECTED" >&2
    echo "  actual:   $ACTUAL" >&2
    exit 1
  fi
  echo "  checksum OK ($ACTUAL)"
fi

# --- Validate it parses + count entries -------------------------------------
if ! COUNT="$(python3 -c "import json; print(len(json.load(open('$TMP/models.json'))))" 2>/dev/null)"; then
  echo "error: downloaded models.json is not valid JSON" >&2
  exit 1
fi
echo "  $COUNT entries"

# --- Swap into place (atomic) + write provenance ---------------------------
mv "$TMP/models.json" "$MODELS_FILE"
cat > "$VERSION_FILE" <<EOF
# Provenance of src/merged_models.json - do not edit by hand.
# Regenerate with: ./scripts/sync-registry.sh
tag: ${TAG}
models.json.sha256: ${ACTUAL}
entries: ${COUNT}
source: https://github.com/${REGISTRY_REPO}
fetched_at: $(date -u +%Y-%m-%dT%H:%M:%SZ)
EOF

echo
echo "Synced ${TAG} -> src/merged_models.json"
echo "  $COUNT entries, sha256 $ACTUAL"
echo
echo "Next: review the diff and commit both files:"
echo "  git add src/merged_models.json src/registry-version.txt"
echo "  git commit -m \"sync sherpa-onnx registry ${TAG}\""
