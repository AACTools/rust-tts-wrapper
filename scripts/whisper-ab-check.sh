#!/usr/bin/env bash
# whisper-ab-check.sh — the objective A/B ear-check for #82 Phase 4.
#
# Transcribes the two WAVs produced by examples/sherpa_g2p_probe.rs
# (ab_text.wav = sherpa's lexicon front-end, ab_codepoints.wav = the
# wrapper G2P front-end) with faster-whisper and prints both
# transcripts. The #82 difference must show up here: the lexicon path
# SILENTLY DROPS out-of-vocabulary/number words, the G2P path speaks
# them — whisper should report the extra words only on the B side.
# (Method per the voicegarden-lexicons README, which validated its
# bundles with whisper round-trips.)
#
# Usage:
#   scripts/whisper-ab-check.sh <dir-with-ab-text-and-ab_codepoints-wavs> [model]
#
# Requirements: uv (creates/reuses a venv with faster-whisper at
# ~/.cache/rust-tts-wrapper/whisper-venv), ~500 MB model download on
# first run (whisper "small", int8 CPU). WAVs are decoded with the
# stdlib — no PyAV involved (its API drifted under faster-whisper).

set -euo pipefail

DIR="${1:?usage: whisper-ab-check.sh <earcheck-dir> [model]}"
MODEL="${2:-small}"
for f in "$DIR/ab_text.wav" "$DIR/ab_codepoints.wav"; do
    [ -f "$f" ] || { echo "missing $f (run examples/sherpa_g2p_probe first)" >&2; exit 2; }
done

VENV="$HOME/.cache/rust-tts-wrapper/whisper-venv"
if [ ! -x "$VENV/bin/python" ]; then
    echo "creating faster-whisper venv at $VENV (one-off)…" >&2
    uv venv "$VENV" -q
    uv pip install -q --python "$VENV/bin/python" faster-whisper numpy
fi

"$VENV/bin/python" - "$DIR" "$MODEL" << 'EOF'
import sys
import wave
from pathlib import Path

import numpy as np
from faster_whisper import WhisperModel

directory, model_name = Path(sys.argv[1]), sys.argv[2]


def load(path: Path) -> np.ndarray:
    """Decode mono 16-bit WAV and resample to whisper's 16 kHz."""
    with wave.open(str(path), "rb") as w:
        rate, channels = w.getframerate(), w.getnchannels()
        assert channels == 1, f"{path}: expected mono"
        pcm = np.frombuffer(w.readframes(w.getnframes()), dtype=np.int16)
    x = pcm.astype(np.float32) / 32768.0
    if rate == 16000:
        return x
    n = int(len(x) * 16000 / rate)
    return np.interp(np.linspace(0, len(x) - 1, n), np.arange(len(x)), x).astype(np.float32)


model = WhisperModel(model_name, device="cpu", compute_type="int8")
for name, label in (
    ("ab_text.wav", "A lexicon path (text + generated lexicon.txt)"),
    ("ab_codepoints.wav", "B G2P path (wrapper phoneme_codepoints)"),
):
    segments, _ = model.transcribe(load(directory / name), language="de")
    text = " ".join(s.text.strip() for s in segments)
    print(f"{label}:")
    print(f"  {text}")
    print()
print(
    "expected: numbers/OOV words appear ONLY in B (A drops them);\n"
    "if B's pauses sound wrong to a human, that is the contract knob."
)
EOF
