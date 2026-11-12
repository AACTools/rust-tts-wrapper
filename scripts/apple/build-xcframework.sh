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
cargo build --release --no-default-features --features cloud,sherpaonnx \
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
