use std::env;

fn main() {
    // Fail fast with a helpful message when the user enables `sapi` on a
    // non-Windows target (§3 H1). Without this the failure is a confusing
    // "crate not found" from the target-gated `windows` dependency.
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let sapi_enabled = env::var("CARGO_FEATURE_SAPI").is_ok();
    if sapi_enabled && target_os != "windows" {
        panic!(
            "The 'sapi' feature is only available on Windows (target_os = \"windows\"). \
             Current target OS: {target_os:?}. Remove --features sapi for this target."
        );
    }

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

    if env::var("TARGET").unwrap_or_default().contains("apple") {
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
