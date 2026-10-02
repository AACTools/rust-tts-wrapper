#!/bin/bash
# Build the dual-target npm package into js/npm/
set -euo pipefail
cd "$(dirname "$0")"
export RUSTUP_TOOLCHAIN=stable
wasm-pack build --target web --release
wasm-pack build --target nodejs --release --out-dir pkg-node
rm -rf npm/browser npm/node
mkdir -p npm/browser npm/node
cp pkg/rust_tts_wrapper_js.js pkg/rust_tts_wrapper_js_bg.wasm pkg/rust_tts_wrapper_js.d.ts npm/browser/
cp -r pkg/snippets npm/browser/
cp pkg-node/rust_tts_wrapper_js.js pkg-node/rust_tts_wrapper_js_bg.wasm pkg-node/rust_tts_wrapper_js.d.ts npm/node/
cp -r pkg-node/snippets npm/node/
cp README.md npm/README.md
echo "package ready in npm/ — npm publish from there"
