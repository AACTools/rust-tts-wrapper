#!/usr/bin/env bash
set -euo pipefail

if [[ $# != 4 && $# != 6 ]]; then
    echo "Usage: $0 OUTPUT_DIR MAC_ARM64 IOS_ARM64 SIM_ARM64 [MAC_X86_64 SIM_X86_64]" >&2
    exit 2
fi

output=$1
mac_arm64=$2
ios_arm64=$3
sim_arm64=$4
mac_x86_64=${5:-}
sim_x86_64=${6:-}
name=RustTtsNative.xcframework

mkdir -p "$output"
work=$(mktemp -d "$output/.xcframework.XXXXXX")
trap 'rm -rf "$work"' EXIT
mkdir -p "$work"/{macos,ios,simulator}

if [[ -n "$mac_x86_64" ]]; then
    xcrun lipo -create "$mac_arm64" "$mac_x86_64" -output "$work/macos/librust_tts_wrapper.a"
    xcrun lipo -create "$sim_arm64" "$sim_x86_64" -output "$work/simulator/librust_tts_wrapper.a"
else
    cp "$mac_arm64" "$work/macos/librust_tts_wrapper.a"
    cp "$sim_arm64" "$work/simulator/librust_tts_wrapper.a"
fi
cp "$ios_arm64" "$work/ios/librust_tts_wrapper.a"

# Publish only the archives. SwiftPM imports the separate C shim's header.
# Headers on a static XCFramework are copied into a shared include directory
# by Xcode, where they can overwrite another dependency's module map.
xcodebuild -create-xcframework \
    -library "$work/macos/librust_tts_wrapper.a" \
    -library "$work/ios/librust_tts_wrapper.a" \
    -library "$work/simulator/librust_tts_wrapper.a" \
    -output "$work/$name"

ditto -c -k --keepParent "$work/$name" "$work/$name.zip"
rm -rf "$output/$name"
mv "$work/$name" "$work/$name.zip" "$output/"
swift package compute-checksum "$output/$name.zip"
