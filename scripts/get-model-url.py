#!/usr/bin/env python3
"""Resolve a SherpaOnnx model's download URL from the bundled registry.

Usage: get-model-url.py <model_id>

Exits 0 with the URL on stdout when the model is present, 1 with a
human-readable error on stderr otherwise. Used by the
sherpaonnx-live.yml GitHub Actions workflow so a registry rename produces
a clear error rather than an opaque Python traceback.

The registry path can be overridden via the RTW_MERGED_MODELS env var;
defaults to src/merged_models.json relative to the repo root.
"""

from __future__ import annotations

import json
import os
import sys
from pathlib import Path


def main() -> int:
    if len(sys.argv) != 2:
        print("usage: get-model-url.py <model_id>", file=sys.stderr)
        return 2

    model_id = sys.argv[1]
    registry_path = Path(
        os.environ.get(
            "RTW_MERGED_MODELS",
            Path(__file__).resolve().parent.parent / "src" / "merged_models.json",
        )
    )

    if not registry_path.is_file():
        print(f"registry not found at {registry_path}", file=sys.stderr)
        return 1

    try:
        with registry_path.open(encoding="utf-8") as f:
            registry = json.load(f)
    except json.JSONDecodeError as exc:
        print(f"registry JSON is malformed: {exc}", file=sys.stderr)
        return 1

    entry = registry.get(model_id)
    if entry is None:
        known = ", ".join(sorted(registry)[:5])
        print(
            f"model '{model_id}' not in registry. "
            f"First few known ids: {known}... ({len(registry)} total)",
            file=sys.stderr,
        )
        return 1

    url = entry.get("url")
    if not url:
        print(f"model '{model_id}' has no 'url' field", file=sys.stderr)
        return 1

    print(url)
    return 0


if __name__ == "__main__":
    sys.exit(main())
