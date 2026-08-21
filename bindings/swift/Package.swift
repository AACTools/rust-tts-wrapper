// swift-tools-version:5.9
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
        // C shim over the cbindgen header (single source of truth: the
        // header is a symlink to ../../include/tts_wrapper.h — CI verifies
        // it matches). Linking is runtime-agnostic: the dylib must be on
        // the loader path at run time (or use the staticlib).
        .target(
            name: "CRustTtsWrapper",
            linkerSettings: [
                .linkedLibrary("rust_tts_wrapper"),
                .unsafeFlags(["-L\(libDir)"]),
            ]
        ),
        .target(
            name: "RustTtsWrapper",
            dependencies: ["CRustTtsWrapper"],
            path: "."
        ),
        .testTarget(
            name: "RustTtsWrapperTests",
            dependencies: ["RustTtsWrapper"],
            path: "Tests/RustTtsWrapperTests"
        ),
    ]
)
