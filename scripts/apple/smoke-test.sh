#!/usr/bin/env bash
set -euo pipefail

root=$(cd "$(dirname "$0")/../.." && pwd)
smoke=$(mktemp -d)
trap 'rm -rf "$smoke"' EXIT

cd "$smoke"
ln -s "$root" wrapper
mkdir -p Sources/RustTtsAppleSmoke
cat > Package.swift <<'SWIFT'
// swift-tools-version: 5.9
import PackageDescription

let package = Package(
    name: "RustTtsAppleSmoke",
    platforms: [.macOS(.v13), .iOS(.v16)],
    dependencies: [.package(name: "RustTtsWrapper", path: "wrapper")],
    targets: [
        .executableTarget(
            name: "RustTtsAppleSmoke",
            dependencies: [.product(name: "RustTtsWrapper", package: "RustTtsWrapper")]
        )
    ]
)
SWIFT
cat > Sources/RustTtsAppleSmoke/main.swift <<'SWIFT'
import RustTtsWrapper

let engines = try TtsClient.listEngines()
precondition(engines.contains { $0.id == "openai" })
let client = try TtsClient(
    engineId: "openai",
    credentials: ["apiKey": "local-smoke-key"]
)
defer { client.close() }
let voices = try client.getVoices()
precondition(!voices.isEmpty)
SWIFT

swift run RustTtsAppleSmoke
for destination in 'generic/platform=iOS' 'generic/platform=iOS Simulator'; do
    xcodebuild -scheme RustTtsAppleSmoke -destination "$destination" \
        -derivedDataPath "$smoke/DerivedData" -quiet build CODE_SIGNING_ALLOWED=NO
done
