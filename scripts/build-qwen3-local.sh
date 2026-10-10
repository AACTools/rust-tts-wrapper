#!/usr/bin/env bash
# Build predict-woo/qwen3-tts.cpp (GGML) for the `qwen3-local` feature.
# Usage: scripts/build-qwen3-local.sh [target-dir]   (default: ~/spikes)
# Then:  QWEN3_TTS_LIB=<target-dir>/qwen3-tts.cpp cargo build --features qwen3-local
# Models (one-time): run their scripts/setup_pipeline_models.py — see README.
set -euo pipefail
TARGET_DIR="${1:-$HOME/spikes}"
REPO="$TARGET_DIR/qwen3-tts.cpp"

if [ ! -d "$REPO" ]; then
    git clone --recurse-submodules -j4 https://github.com/predict-woo/qwen3-tts.cpp "$REPO"
else
    git -C "$REPO" submodule update --init --recursive
fi

# GGML: enable the accelerators available on this machine (CPU always).
GGML_FLAGS=()
command -v nvcc >/dev/null 2>&1 && GGML_FLAGS+=(-DGGML_CUDA=ON)
[ "$(uname -s)" = "Darwin" ] && GGML_FLAGS+=(-DGGML_METAL=ON)
cmake -S "$REPO/ggml" -B "$REPO/ggml/build" -DCMAKE_BUILD_TYPE=Release "${GGML_FLAGS[@]}"
cmake --build "$REPO/ggml/build" -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)"

cmake -S "$REPO" -B "$REPO/build" -DCMAKE_BUILD_TYPE=Release
cmake --build "$REPO/build" -j"$(nproc 2>/dev/null || sysctl -n hw.ncpu)"

echo
echo "Built: $REPO/build/libqwen3tts.so"
echo "Next: QWEN3_TTS_LIB=$REPO cargo build --features qwen3-local"
