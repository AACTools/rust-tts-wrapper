use std::env;

fn main() {
    // Fail fast with a helpful message when the user enables `sapi` on a
    // non-Windows target. Without this the failure is a confusing
    // "crate not found" from the target-gated `windows` dependency.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let sapi_enabled = env::var("CARGO_FEATURE_SAPI").is_ok();
    assert!(
        !(sapi_enabled && target_os != "windows"),
        "The 'sapi' feature is only available on Windows (target_os = \"windows\"). \
         Current target OS: {target_os:?}. Remove --features sapi for this target."
    );

    let crate_dir = env::var("CARGO_MANIFEST_DIR").unwrap();
    let config = cbindgen::Config::from_file("cbindgen.toml").unwrap_or_default();
    match cbindgen::Builder::new()
        .with_crate(&crate_dir)
        .with_config(config)
        .generate()
    {
        Ok(bindings) => {
            bindings.write_to_file("include/tts_wrapper.h");
        }
        Err(e) => {
            eprintln!("cbindgen warning: {e}");
        }
    }

    // qwen3-local: link the user-built qwen3-tts.cpp shared library.
    // Nothing is vendored (the project + GGML are large); the feature is
    // opt-in and fails with instructions when the library is missing.
    let qwen3_local = env::var("CARGO_FEATURE_QWEN3_LOCAL").is_ok();
    if qwen3_local {
        let lib_dir = env::var("QWEN3_TTS_LIB").unwrap_or_default();
        let non_empty = |v: &str| !v.is_empty();
        assert!(
            non_empty(&lib_dir),
            "The 'qwen3-local' feature needs the qwen3-tts.cpp library. \
             Build it once (scripts/build-qwen3-local.sh clones + builds into \
             ~/spikes or a dir of your choice) and point QWEN3_TTS_LIB at that \
             repo root (containing build/libqwen3tts.so and \
             src/qwen3tts_c_api.h), e.g.: \
             QWEN3_TTS_LIB=/home/me/spikes/qwen3-tts.cpp cargo build \
             --features qwen3-local"
        );
        // The shared artifact is libqwen3tts.so (target name
        // qwen3tts_shared, OUTPUT_NAME qwen3tts). GGML may be linked
        // dynamically — expose its lib dirs for link + runtime.
        println!("cargo:rustc-link-search=native={lib_dir}/build");
        println!("cargo:rustc-link-search=native={lib_dir}/ggml/build/src");
        println!("cargo:rustc-link-lib=dylib=qwen3tts");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_dir}/build");
        println!("cargo:rustc-link-arg=-Wl,-rpath,{lib_dir}/ggml/build/src");
        println!("cargo:rerun-if-env-changed=QWEN3_TTS_LIB");
    }

    // The avsynth shim is only compiled on macOS (it wraps AVSpeechSynthesizer).
    // The `cc` crate is a macOS-only build-dependency for this reason.
    #[cfg(target_os = "macos")]
    {
        cc::Build::new()
            .file("extern/avsynth_shim.m")
            .compiler("clang")
            .flag("-fobjc-arc")
            .compile("avsynth_shim");
        println!("cargo:rustc-link-lib=framework=AVFAudio");
        println!("cargo:rustc-link-lib=framework=Foundation");
        println!("cargo:rustc-link-lib=objc");
    }
}
