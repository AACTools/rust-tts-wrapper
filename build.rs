use std::env;

fn main() {
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
