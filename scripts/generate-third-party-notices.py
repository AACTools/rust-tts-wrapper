#!/usr/bin/env python3
"""Generate THIRD_PARTY_NOTICES.md for the release artifacts (#145).

Inventory of every licence that must travel with the shipped binaries:

  1. Statically linked native libraries (sherpa-onnx + ONNX Runtime and
     its bundled protobuf/abseil/re2/onnx, the bundled offline C++
     engines) — the NATIVE_COMPONENTS table below.
  2. Every Rust crate in the dependency graph — read from
     ``cargo metadata --locked --all-features`` so the inventory is the
     union of every feature combination (a superset of anything any
     release leg links; issue #143's lock pins the versions).

Licence texts live in ``scripts/licences/<SPDX-ID>.txt`` (extracted
from the crates themselves, not retyped). Compound SPDX expressions
are preserved verbatim in the inventory — we do not choose an arm of
an OR — and every distinct token's full text is included in the
appendix. A missing text file is a hard error: the release must not
ship a notice-less licence.

Usage:
    scripts/generate-third-party-notices.py > THIRD_PARTY_NOTICES.md
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
LICENCE_DIR = REPO / "scripts" / "licences"

# ── Statically linked / bundled native components ────────────────────
# (name, version, licence, upstream, which artifacts carry it)
NATIVE_COMPONENTS = [
    (
        "sherpa-onnx",
        "2.0.1",
        "Apache-2.0",
        "https://github.com/k2-fsa/sherpa-onnx",
        "Linux .so, macOS dylib, Windows DLLs, NuGet (statically linked "
        "into rust_tts_wrapper); also the sherpa-onnx-models registry "
        "downloads (Apache-2.0)",
    ),
    (
        "ONNX Runtime",
        "1.28.2 (as bundled in the sherpa-onnx 2.0.1 static archives)",
        "MIT",
        "https://github.com/microsoft/onnxruntime",
        "Linux .so, macOS dylib, Windows DLLs, NuGet (statically linked "
        "via the sherpa-onnx archives)",
    ),
    (
        "protobuf (bundled in ONNX Runtime)",
        "as bundled",
        "BSD-3-Clause",
        "https://github.com/protocolbuffers/protobuf",
        "statically linked inside the sherpa-onnx archives",
    ),
    (
        "abseil-cpp (bundled in ONNX Runtime)",
        "as bundled",
        "Apache-2.0",
        "https://github.com/abseil/abseil-cpp",
        "statically linked inside the sherpa-onnx archives",
    ),
    (
        "re2 (bundled in ONNX Runtime)",
        "as bundled",
        "BSD-3-Clause",
        "https://github.com/google/re2",
        "statically linked inside the sherpa-onnx archives",
    ),
    (
        "onnx (bundled in ONNX Runtime)",
        "as bundled",
        "Apache-2.0",
        "https://github.com/onnx/onnx",
        "statically linked inside the sherpa-onnx archives",
    ),
    (
        "kaldi / openfst / utf8proc / ssentencepiece (bundled in sherpa-onnx)",
        "as bundled",
        "Apache-2.0",
        "https://github.com/k2-fsa/sherpa-onnx",
        "statically linked inside the sherpa-onnx archives (individual "
        "upstream licences apply to each; sherpa-onnx distributes them "
        "under Apache-2.0)",
    ),
    (
        "qwen3-tts.cpp",
        "as built by the release (predict-woo fork)",
        "MIT",
        "https://github.com/predict-woo/qwen3-tts.cpp",
        "NuGet engine-libs (qwen3tts.dll) and the macOS engine-libs "
        "release asset",
    ),
    (
        "omnivoice.cpp",
        "as built by the release",
        "MIT",
        "https://github.com/ServeurpersoCom/omnivoice.cpp",
        "NuGet engine-libs (omnivoice.dll) and the macOS engine-libs "
        "release asset",
    ),
    (
        "ggml (linked by qwen3-tts.cpp / omnivoice.cpp)",
        "as built by the release",
        "MIT",
        "https://github.com/ggml-org/ggml",
        "NuGet engine-libs and the macOS engine-libs release asset",
    ),
    (
        "Higgs Audio v2 tokenizer weights (used by OmniVoice)",
        "as bundled upstream",
        "Apache-2.0",
        "https://huggingface.co/bosonai/higgs-audio-v2-tokenizer",
        "model files are user-downloaded, not shipped in the binaries; "
        "listed for completeness",
    ),
]

# SPDX tokens that need an election note rather than a text appendix.
# speech-dispatcher and r-efi are tri-licensed (LGPL arm + MIT/Apache
# arms) and ship no licence file in the registry: as redistributors we
# elect the MIT OR Apache-2.0 arm, so no LGPL obligations attach. The
# inventory still lists the full expression verbatim.
TOKEN_OVERRIDES: dict[str, str] = {
    "LGPL-2.1": (
        "Crate is multi-licensed (LGPL-2.1 OR MIT OR Apache-2.0). "
        "This distribution elects the MIT OR Apache-2.0 arm; the LGPL "
        "arm is not elected and no LGPL obligations attach. The LGPL "
        "text is available from the crate's upstream repository."
    ),
    "LGPL-2.1-or-later": (
        "Crate is multi-licensed (MIT OR Apache-2.0 OR "
        "LGPL-2.1-or-later). This distribution elects the MIT OR "
        "Apache-2.0 arm; the LGPL arm is not elected and no LGPL "
        "obligations attach. The LGPL text is available from the "
        "crate's upstream repository."
    ),
}


def spdx_tokens(expression: str) -> list[str]:
    """Extract the distinct SPDX licence ids from an expression.

    Deliberately dumb: split on 'OR', 'AND', 'WITH' and '/' (some
    crates use sloppy 'MIT/Apache-2.0' fields), strip parens. We keep
    the original expression in the inventory — this is only used to
    decide which appendix texts to include.
    """
    tokens = re.split(r"\b(?:OR|AND|WITH)\b|/", expression)
    out = []
    for t in tokens:
        t = t.strip(" ()\t")
        if t:
            out.append(t)
    return out


def main() -> int:
    meta = subprocess.run(
        # --all-features: the full union of every feature combination's
        # dependency graph — a superset of anything any release leg
        # links (loop-review finding: default-features-only metadata
        # missed 60+ crates statically linked into the shipped DLLs).
        # Over-inclusive is the safe direction for a licence inventory;
        # under-inclusive ships notice-less licences.
        [
            "cargo", "metadata",
            "--format-version", "1",
            "--locked",
            "--all-features",
        ],
        cwd=REPO,
        capture_output=True,
        text=True,
        check=False,
    )
    if meta.returncode != 0:
        print(f"cargo metadata failed: {meta.stderr}", file=sys.stderr)
        return 1
    packages = json.loads(meta.stdout)["packages"]

    # (license expression, name, version, repo) sorted by expression
    entries = sorted(
        (
            (
                p.get("license") or "NO-LICENCE-FILE (see repository)",
                p["name"],
                p["version"],
                (p.get("repository") or "").replace("\n", " "),
            )
            for p in packages
        ),
        key=lambda e: (e[0].lower(), e[1]),
    )

    # Which appendix texts do we need?
    needed: set[str] = set()
    for lic, *_ in entries:
        for tok in spdx_tokens(lic):
            if tok in TOKEN_OVERRIDES:
                continue
            if tok.startswith("NO-LICENCE-FILE"):
                continue
            needed.add(tok)
    for _, _, lic, _, _ in NATIVE_COMPONENTS:
        needed.update(spdx_tokens(lic))

    missing = sorted(t for t in needed if not (LICENCE_DIR / f"{t}.txt").is_file())
    if missing:
        print(
            "missing licence texts in scripts/licences/ for: "
            + ", ".join(missing),
            file=sys.stderr,
        )
        return 2

    w = sys.stdout.write
    w("# Third-Party Notices for rust-tts-wrapper release artifacts\n\n")
    w(
        "This file is the licence inventory for the binary artifacts of "
        "this release (the shared libraries, the NuGet package and the "
        "macOS XCFramework). `rust-tts-wrapper` itself is MIT; the "
        "components below are statically linked or bundled, and their "
        "licences require their notices to travel with the binaries.\n\n"
        "The Rust-crate inventory was generated from the committed "
        "`Cargo.lock` (issue #143), so it lists exactly the versions "
        "this release was built from.\n\n"
        "## Native components (statically linked / bundled)\n\n"
    )
    w("| Component | Version | Licence | Upstream | Carried by |\n")
    w("|---|---|---|---|---|\n")
    for name, ver, lic, url, carried in NATIVE_COMPONENTS:
        w(f"| {name} | {ver} | {lic} | {url} | {carried} |\n")

    w("\n## Rust crates (from Cargo.lock)\n\n")
    w("Grouped by the crate's declared licence expression, preserved "
      "verbatim. Dual-licensed crates (e.g. `MIT OR Apache-2.0`) are "
      "used under the terms of their expression; both texts are in the "
      "appendix.\n\n")
    current = None
    count = 0
    for lic, name, ver, repo in entries:
        if lic != current:
            if current is not None:
                w("\n")
            current = lic
            count = 0
            w(f"### {lic}\n\n")
        w(f"- {name} {ver}"
          + (f" — {repo}" if repo else "") + "\n")
        count += 1
    w("\n")

    w("## Appendix: licence texts\n\n")
    for tok in sorted(needed):
        if tok in TOKEN_OVERRIDES:
            w(f"### {tok}\n\n{TOKEN_OVERRIDES[tok]}\n\n")
            continue
        text = (LICENCE_DIR / f"{tok}.txt").read_text(encoding="utf-8").rstrip()
        w(f"### {tok}\n\n")
        w("The full text of this licence follows, as distributed by the "
          "upstream projects.\n\n")
        w("```\n" + text + "\n```\n\n")

    total = len(entries)
    w(f"---\n\n*{total} Rust crates inventoried; "
      f"{len(NATIVE_COMPONENTS)} native components; generated by "
      "`scripts/generate-third-party-notices.py`.*\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
