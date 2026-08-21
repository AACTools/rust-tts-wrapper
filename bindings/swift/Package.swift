// swift-tools-version:5.9
import Foundation
import PackageDescription

// Build the Rust library first and point the build at it:
//   cargo build --no-default-features --features avsynth,cloud
//   TTS_WRAPPER_LIB_DIR=$PWD/target/debug swift test
let libDir = ProcessInfo.processInfo.environment["TTS_WRAPPER_LIB_DIR"] ?? "../target/debug"

let package = Package(
    name: "RustTtsWrapper",
    products: [
        .library(name: "RustTtsWrapper", targets: ["RustTtsWrapper"]),
    ],
    targets: [
        // C shim over the cbindgen header. The header is a committed copy
        // of include/tts_wrapper.h — CI diffs the two to prevent drift;
        // refresh it with:
        //   cp ../../include/tts_wrapper.h Sources/CRustTtsWrapper/include/
        .target(
            name: "CRustTtsWrapper",
            linkerSettings: [
                .linkedLibrary("rust_tts_wrapper"),
                .unsafeFlags(["-L\(libDir)"]),
            ]
        ),
        .target(
            name: "RustTtsWrapper",
            dependencies: ["CRustTtsWrapper"]
        ),
        .testTarget(
            name: "RustTtsWrapperTests",
            dependencies: ["RustTtsWrapper"],
            path: "Tests/RustTtsWrapperTests"
        ),
    ]
)
