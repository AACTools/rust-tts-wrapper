# MMS word aligner tooling (issue #109)

The wrapper's word-timing aligner (feature `word-align`,
`src/word_aligner.rs` + `src/word_aligner_rt.rs`) runs Meta's MMS
multilingual uroman CTC aligner as ONNX through `ort`. This directory
holds the conversion + validation tooling.

**License:** the MMS model weights are **CC-BY-NC 4.0** (non-commercial).
The model is an optional, separately-fetched artifact — nothing in the
crate embeds it, and commercial builds keep the boundary-estimate path.

## One-time setup

```bash
# 1. Download the model + dictionary (1.26 GB, fp32):
mkdir -p ~/.cache/rust-tts-wrapper/mms-aligner && cd $_
curl -LO https://dl.fbaipublicfiles.com/mms/torchaudio/ctc_alignment_mling_uroman/model.pt
curl -LO https://dl.fbaipublicfiles.com/mms/torchaudio/ctc_alignment_mling_uroman/dictionary.txt

# 2. Export to ONNX (needs a venv with torch, torchaudio, onnxruntime,
#    onnxscript; round-trip-verified against torch to ~1e-4):
python3 -m venv venv && venv/bin/pip install torch torchaudio \
    --index-url https://download.pytorch.org/whl/cpu
venv/bin/pip install numpy onnxruntime onnxscript
venv/bin/python scripts/aligner/export_mms_aligner_onnx.py
```

## Cross-validation (the proof harness)

The Rust port was proven against the reference implementation twice:

1. **Oracle match** — the pure Viterbi core aligns the reference
   model's emissions identically to `torchaudio.functional.forced_align`:

```bash
venv/bin/python scripts/aligner/reference_oracle.py   # dumps emissions + oracle.json
WORD_ALIGNER_ORACLE=~/.cache/rust-tts-wrapper/mms-aligner \
    cargo test --lib word_aligner -- --ignored --nocapture
```

2. **End-to-end match** — the full Rust pipeline (ort inference on the
   ONNX + the Viterbi port) reproduces the reference word timings:

```bash
WORD_ALIGNER_ORACLE=~/.cache/rust-tts-wrapper/mms-aligner \
    cargo test --features word-align --lib word_aligner -- --ignored --nocapture
```

Both print per-word timings vs the oracle and pass within tolerance
(one frame for the oracle match; 60 ms for end-to-end, covering the
ort-vs-torch logits delta).

## Status

The core + runner are proven; the engine integration (firing
`estimated=false` boundaries through the hybrid rule, the model
distribution manifest, uroman for non-Latin scripts) is the follow-up
tracked on issue #109.
