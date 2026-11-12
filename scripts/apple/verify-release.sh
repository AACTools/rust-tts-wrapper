#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
tag=$(awk -F '"' '/^let releaseTag = / { print $2 }' "$root/Package.swift")
checksum=$(awk -F '"' '/^let releaseChecksum = / { print $2 }' "$root/Package.swift")
if [[ "$tag" != "$1" || ! "$checksum" =~ ^[0-9a-f]{64}$ ]]; then
    echo "Pin the XCFramework for $1 before tagging. See docs/swift.md." >&2
    exit 1
fi

digest=$(gh release view "$tag" --json assets \
    --jq '.assets[] | select(.name == "RustTtsNative.xcframework.zip") | .digest')
if [[ "$digest" != "sha256:$checksum" ]]; then
    echo "The staged RustTtsNative.xcframework.zip for $tag does not match Package.swift." >&2
    exit 1
fi
