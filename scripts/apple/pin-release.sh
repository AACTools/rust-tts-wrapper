#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 2 || ! $1 =~ ^v[0-9]+\.[0-9]+\.[0-9]+([.-][[:alnum:].-]+)?$ ]]; then
    echo "Usage: $0 VERSION_TAG PATH_TO_RUST_TTS_XCFRAMEWORK_ZIP" >&2
    exit 2
fi

root=$(cd "$(dirname "$0")/../.." && pwd)
checksum=$(swift package compute-checksum "$2")
python3 - "$root/Package.swift" "$1" "$checksum" <<'PY'
from pathlib import Path
import re
import sys

manifest = Path(sys.argv[1])
source = manifest.read_text()
for name, value in (("releaseTag", sys.argv[2]), ("releaseChecksum", sys.argv[3])):
    source, count = re.subn(
        rf'^let {name} = "[^"]*"$',
        f'let {name} = "{value}"',
        source,
        flags=re.MULTILINE,
    )
    if count != 1:
        raise SystemExit(f"Expected one {name} assignment in {manifest}")
manifest.write_text(source)
PY
