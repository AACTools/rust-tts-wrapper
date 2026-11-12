#!/usr/bin/env bash
set -euo pipefail

target=${1:-}
profile=${2:-release}
case "$target" in
  aarch64-apple-ios) slice=ios-arm64; arch=arm64 ;;
  aarch64-apple-ios-sim) slice=ios-arm64_x86_64-simulator; arch=arm64 ;;
  x86_64-apple-ios) slice=ios-arm64_x86_64-simulator; arch=x86_64 ;;
  *) echo "Usage: $0 <aarch64-apple-ios|aarch64-apple-ios-sim|x86_64-apple-ios> [release|debug]" >&2; exit 2 ;;
esac
case "$profile" in
  release) set -- --release ;;
  debug) set -- ;;
  *) echo "Expected release or debug, got: $profile" >&2; exit 2 ;;
esac

cd "$(dirname "$0")/.."
# Keep these paired with the sherpa-onnx-sys pin in Cargo.toml.
sherpa_version=2.0.1
onnx_version=1.28.2
sherpa_sha=88aa569f71b7903e0bbdfbe6e2e114b3b95862b1ef0759eea3024f82926d7374
onnx_sha=2c2299acbb461d26d4bac4bc85985d40e7c7177ed6072703ae0846d88b0b4599

cache="${CARGO_TARGET_DIR:-target}/sherpa-ios-$sherpa_version-onnx-$onnx_version"
mkdir -p "$cache"
cache=$(cd "$cache" && pwd)
lib_dir="$cache/$target-thin"
scratch=$(mktemp -d "$cache/.prepare.XXXXXX")
trap 'rm -rf "$scratch"' EXIT

download() {
  local url=$1 filename=$2 checksum=$3
  if [[ -f "$cache/$filename" ]] &&
    (cd "$cache" && printf '%s  %s\n' "$checksum" "$filename" | shasum -a 256 -c --status); then
    return
  fi

  curl --fail --silent --show-error --location --retry 3 "$url" -o "$scratch/$filename"
  (cd "$scratch" && printf '%s  %s\n' "$checksum" "$filename" | shasum -a 256 -c)
  mv -f "$scratch/$filename" "$cache/$filename"
}

thin_archive() {
  local source=$1 output=$2
  # Even a one-architecture device library can use the fat container format.
  # Rust needs a plain archive to bundle the native objects into a staticlib.
  case "$(LC_ALL=C xcrun lipo -info "$source")" in
    "Non-fat file:"*) cp "$source" "$output" ;;
    *) xcrun lipo "$source" -thin "$arch" -output "$output" ;;
  esac
}

if [[ ! -f "$lib_dir/libsherpa-onnx-c-api.a" || ! -f "$lib_dir/libonnxruntime.a" ]]; then
  download "https://github.com/k2-fsa/sherpa-onnx/releases/download/xcframework/sherpa-onnx-v$sherpa_version-ios-static.xcframework.zip" sherpa.zip "$sherpa_sha"
  download "https://github.com/csukuangfj/onnxruntime-libs/releases/download/v$onnx_version/onnxruntime-ios-static-xcframework-$onnx_version.xcframework.zip" onnx.zip "$onnx_sha"
  ditto -x -k "$cache/sherpa.zip" "$scratch/sherpa"
  ditto -x -k "$cache/onnx.zip" "$scratch/onnx"

  thin_archive "$scratch/sherpa/sherpa-onnx.xcframework/$slice/SherpaOnnxC.framework/SherpaOnnxC" "$scratch/libsherpa-onnx-c-api.a"
  thin_archive "$scratch/onnx/onnxruntime.xcframework/$slice/onnxruntime.framework/onnxruntime" "$scratch/libonnxruntime.a"
  mkdir -p "$lib_dir"
  mv -f "$scratch/libsherpa-onnx-c-api.a" "$scratch/libonnxruntime.a" "$lib_dir/"
fi

# sherpa-onnx-sys 2.0.1 chooses shared linkage on iOS. Cargo's links override
# skips that build script for this target and bundles the two static archives.
link_path=${lib_dir//\\/\\\\}
link_path=${link_path//\"/\\\"}
cat > "$scratch/sherpa.toml" <<TOML
[target.$target.sherpa-onnx]
rustc-link-search = ["native=$link_path"]
rustc-link-lib = [
  "static=sherpa-onnx-c-api", "static=onnxruntime", "dylib=c++",
  "framework=Accelerate", "framework=CoreFoundation", "framework=CoreML", "framework=Foundation",
]
TOML

export IPHONEOS_DEPLOYMENT_TARGET=${IPHONEOS_DEPLOYMENT_TARGET:-15.0}
cargo rustc --config "$scratch/sherpa.toml" --target "$target" "$@" \
  --no-default-features --features cloud,sherpaonnx --lib --crate-type staticlib
