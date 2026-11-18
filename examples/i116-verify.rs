// Throwaway #116 verification: install kitten + matcha into FRESH
// dirs (as the Windows team would after updating the NuGet package),
// then speak through the engine. Exercises: install → bundle
// provisioning (#106) → auto-lexicon generation → tokenization.
use rust_tts_wrapper::factory::create_engine;
use std::ffi::CString;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

unsafe extern "C" fn on_progress(
    _pct: f64,
    _msg: *const std::ffi::c_char,
    _ud: *mut std::ffi::c_void,
) {
}
unsafe extern "C" fn not_cancelled(_ud: *mut std::ffi::c_void) -> bool {
    false
}

fn main() {
    for id in ["kitten-nano-en-v0_1-fp16", "matcha-icefall-en-ljspeech"] {
        let engine = CString::new("sherpaonnx").unwrap();
        let model = CString::new(id).unwrap();
        let ok = rust_tts_wrapper::tts_model_install(
            engine.as_ptr(),
            model.as_ptr(),
            Some(on_progress),
            Some(not_cancelled),
            std::ptr::null_mut(),
        );
        if !ok {
            let e = rust_tts_wrapper::tts_get_last_error(std::ptr::null_mut());
            let d = unsafe { std::ffi::CStr::from_ptr(e) }.to_string_lossy();
            panic!("{id} install failed: {d}");
        }
        println!("{id}: installed + bundle provisioned");

        let creds = format!(r#"{{"modelId":"{id}"}}"#);
        let engine = create_engine("sherpaonnx", &creds).expect("engine creation");
        let bytes = Arc::new(AtomicUsize::new(0));
        let b2 = bytes.clone();
        let mut cb = move |chunk: &[u8]| {
            b2.fetch_add(chunk.len(), Ordering::SeqCst);
        };
        engine
            .speak(
                "Hello, I have forty two houses.",
                None,
                1.0,
                1.0,
                1.0,
                Some(&mut cb),
                None,
                None,
            )
            .expect("speak");
        let total = bytes.load(Ordering::SeqCst);
        assert!(total > 10_000, "{id} audio too small: {total}");
        println!("{id}: SPEAKS ({total} bytes)");
    }
    println!("VERIFY: PASS");
}
