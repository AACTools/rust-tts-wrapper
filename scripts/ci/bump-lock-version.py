#!/usr/bin/env python3
"""Bump the root package's version in Cargo.lock (issue #143).

`cargo build --locked` compares the root package version in Cargo.lock
against Cargo.toml, so the release Sync-version job must keep both in
step with the tag. The lock's dependency versions are NOT touched.

Usage: bump-lock-version.py <version>
"""

from __future__ import annotations

import re
import sys
from pathlib import Path

STANZA = re.compile(
    r'(?m)(name = "rust-tts-wrapper"\nversion = ")[^"]+', re.IGNORECASE
)


def main() -> int:
    if len(sys.argv) != 2 or not re.fullmatch(r"\d+\.\d+\.\d+.*", sys.argv[1]):
        print(f"usage: {sys.argv[0]} <version>", file=sys.stderr)
        return 2
    version = sys.argv[1]
    path = Path("Cargo.lock")
    text = path.read_text(encoding="utf-8")
    new_text, n = STANZA.subn(rf"\g<1>{version}", text, count=1)
    if n != 1:
        print(
            "rust-tts-wrapper stanza not found in Cargo.lock — "
            "the lock format changed?",
            file=sys.stderr,
        )
        return 1
    path.write_text(new_text, encoding="utf-8")
    print(f"Cargo.lock root version -> {version}")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
