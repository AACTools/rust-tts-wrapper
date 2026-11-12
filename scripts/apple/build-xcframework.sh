#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
cd "$root"
export MACOSX_DEPLOYMENT_TARGET=13.0
export IPHONEOS_DEPLOYMENT_TARGET=16.0
export CARGO_TARGET_DIR="$root/target"

targets=(
    aarch64-apple-darwin
    x86_64-apple-darwin
    aarch64-apple-ios
    aarch64-apple-ios-sim
    x86_64-apple-ios
)

rustup target add "${targets[@]}"
# sherpa-g2p (#82): the wrapper-side G2P front-end for non-English
# piper — voicegarden bundle chain, FST-only floravox-g2p (no second
# ONNX runtime). Apple validation of this line is tracked in the
# Phase 3 PR checklist.
cargo build --release --no-default-features --features cloud,sherpaonnx,sherpa-g2p \
    --target aarch64-apple-darwin --target x86_64-apple-darwin
for target in aarch64-apple-ios aarch64-apple-ios-sim x86_64-apple-ios; do
    scripts/build-ios-sherpa.sh "$target" release
done

scripts/apple/package-xcframework.sh target/apple \
    target/aarch64-apple-darwin/release/librust_tts_wrapper.a \
    target/aarch64-apple-ios/release/librust_tts_wrapper.a \
    target/aarch64-apple-ios-sim/release/librust_tts_wrapper.a \
    target/x86_64-apple-darwin/release/librust_tts_wrapper.a \
    target/x86_64-apple-ios/release/librust_tts_wrapper.a
