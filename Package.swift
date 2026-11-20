// swift-tools-version: 5.9
import Foundation
import PackageDescription

// scripts/apple/pin-release.sh updates these values for the release tag.
let releaseTag = "v1.16.1"
let releaseChecksum = "584caaa1137d5001524fe115bc3c1383449c1fe71558ba66ead4f39b7b79f341"

let native: Target
if releaseTag.isEmpty || ProcessInfo.processInfo.environment["RUST_TTS_USE_LOCAL_XCFRAMEWORK"] == "1" {
    native = .binaryTarget(
        name: "RustTtsNative",
        path: "target/apple/RustTtsNative.xcframework"
    )
} else {
    native = .binaryTarget(
        name: "RustTtsNative",
        url: "https://github.com/AACTools/rust-tts-wrapper/releases/download/\(releaseTag)/RustTtsNative.xcframework.zip",
        checksum: releaseChecksum
    )
}

let package = Package(
    name: "RustTtsWrapper",
    platforms: [.macOS(.v13), .iOS(.v16)],
    products: [
        .library(name: "RustTtsWrapper", targets: ["RustTtsWrapper"])
    ],
    targets: [
        native,
        .target(
            name: "CRustTtsWrapper",
            path: "bindings/swift/Sources/CRustTtsWrapper"
        ),
        .target(
            name: "RustTtsWrapper",
            dependencies: ["CRustTtsWrapper", "RustTtsNative"],
            path: "bindings/swift/Sources/RustTtsWrapper",
            linkerSettings: [
                .linkedFramework("Security"),
                .linkedFramework("SystemConfiguration"),
                .linkedFramework("Accelerate"),
                .linkedFramework("CoreFoundation"),
                .linkedFramework("CoreML"),
                .linkedFramework("Foundation"),
                .linkedLibrary("c++")
            ]
        ),
        .testTarget(
            name: "RustTtsWrapperTests",
            dependencies: ["RustTtsWrapper"],
            path: "bindings/swift/Tests/RustTtsWrapperTests"
        )
    ]
)
