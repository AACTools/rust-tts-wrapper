//! FFI Safety Tests
//!
//! Exercises the C ABI surface to validate panic protection, allocator
//! consistency, null handling, and string safety across the FFI boundary.
#![allow(clippy::all, clippy::pedantic)]

#[cfg(test)]
mod ffi_null_handling_tests {
    #[cfg(any(
        feature = "cloud",
        feature = "sherpaonnx",
        feature = "system",
        feature = "cloning",
        feature = "floravox",
        feature = "pocket-timing",
        feature = "qwen3-local",
        feature = "omnivoice-local"
    ))]
    use rust_tts_wrapper::tts_get_engine_count;
    use rust_tts_wrapper::{tts_destroy, tts_pause, tts_resume, tts_stop};

    #[test]
    fn test_destroy_null_noop() {
        tts_destroy(std::ptr::null_mut());
    }

    #[test]
    fn test_stop_null_noop() {
        tts_stop(std::ptr::null_mut());
    }

    #[test]
    fn test_pause_null_noop() {
        tts_pause(std::ptr::null_mut());
    }

    #[test]
    fn test_resume_null_noop() {
        tts_resume(std::ptr::null_mut());
    }

    #[test]
    fn test_engine_count_is_positive() {
        // zero-engine builds are legitimate (a lone front-end feature
        // compiles no engines) — see factory_reachability
        #[cfg(any(
            feature = "cloud",
            feature = "sherpaonnx",
            feature = "system",
            feature = "cloning",
            feature = "floravox",
            feature = "pocket-timing",
            feature = "qwen3-local",
            feature = "omnivoice-local",
            all(feature = "sapi", target_os = "windows"),
            all(feature = "avsynth", target_os = "macos")
        ))]
        assert!(tts_get_engine_count() > 0);
    }

    #[test]
    #[cfg(feature = "sherpaonnx")]
    fn test_create_with_valid_sherpaonnx_returns_non_null() {
        let id = std::ffi::CString::new("sherpaonnx").unwrap();
        let ctx = rust_tts_wrapper::tts_create(id.as_ptr(), std::ptr::null());
        assert!(
            !ctx.is_null(),
            "sherpaonnx should construct without credentials"
        );
        tts_destroy(ctx);
    }
}

#[cfg(test)]
mod ffi_voice_roundtrip_tests {
    use rust_tts_wrapper::tts_free_voices;
    #[cfg(feature = "sherpaonnx")]
    use rust_tts_wrapper::{tts_create, tts_destroy, tts_get_voices};

    #[test]
    #[cfg(feature = "sherpaonnx")]
    fn test_get_voices_with_null_pointers_returns_error() {
        let id = std::ffi::CString::new("sherpaonnx").unwrap();
        let ctx = tts_create(id.as_ptr(), std::ptr::null());
        assert!(!ctx.is_null());

        // Passing any null out-pointer must not crash; must return -1.
        let rc = tts_get_voices(ctx, std::ptr::null_mut(), std::ptr::null_mut());
        assert_eq!(rc, -1);

        tts_destroy(ctx);
    }

    #[test]
    fn test_free_voices_null_is_noop() {
        // tts_free_voices must accept (null, 0) without crashing.
        tts_free_voices(std::ptr::null_mut(), 0);
    }
}

#[cfg(test)]
mod string_safety_tests {
    #[test]
    fn test_cstring_helper_round_trip() {
        // ensure CString::new produces a NUL-terminated buffer for
        // every test input we care about. CString::new returns Err only when
        // the input contains an interior NUL byte.
        for input in ["hello", "Hello, World!", "trëma ünîcode", "测试"] {
            let c = std::ffi::CString::new(input).unwrap();
            let bytes = c.as_bytes_with_nul();
            assert_eq!(bytes.last(), Some(&0u8));
            assert_eq!(&bytes[..bytes.len() - 1], input.as_bytes());
        }
    }

    #[test]
    fn test_cstring_interior_nul_is_detected() {
        // A stray NUL byte must surface as Err rather than silently
        // truncating. The FFI layer should treat this as an error rather
        // than pass a `&str.as_ptr()` that walks off the end.
        let result = std::ffi::CString::new("with\0nul");
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod path_parsing_tests {
    #[test]
    fn test_azure_path_parsing_safe() {
        // Azure WS Path: parsing must use strip_prefix + trim, not
        // raw byte slicing. Validate the same logic against tricky inputs.
        let cases = [
            ("Path:turn.end", "turn.end"),
            ("Path:audio.metadata", "audio.metadata"),
            ("Path:word-boundary", "word-boundary"),
            ("Path:response", "response"),
            // Non-ASCII must not panic.
            ("Path:tëst", "tëst"),
            ("Path:测试", "测试"),
            // Edge cases.
            ("Path:", ""),
            ("Path:   ", ""),
            // Lines that don't start with Path: should return "".
            ("Content-Type:application/json", ""),
        ];
        for (input, expected) in cases {
            // This mirrors the logic in cloud_engine/mod.rs (azure_ws_extract_path).
            let path = input
                .lines()
                .find(|l| l.starts_with("Path:"))
                .and_then(|l| l.strip_prefix("Path:"))
                .map(str::trim)
                .unwrap_or("");
            assert_eq!(path, expected, "input: {input:?}");
        }
    }
}

#[cfg(test)]
mod bounds_check_tests {
    #[test]
    fn test_elevenlabs_alignment_safe_iteration() {
        // ElevenLabs characters/starts/ends arrays can have different
        // lengths. Use .get(i) instead of [i] to avoid panicking.
        let chars = ["H", "e", "l", "l", "o"];
        let starts = [0.0_f32, 0.1, 0.2, 0.3, 0.4];
        let ends = [0.1_f32, 0.2, 0.3, 0.4, 0.5];

        for i in 0..chars.len() {
            // All three arrays same length here — fine.
            let _ = (chars[i], starts[i], ends[i]);
        }

        // Now simulate a server response where `characters` outgrows the
        // time arrays. Old code would panic; new code uses .get().
        let short_starts = [0.0_f32];
        let short_ends = [0.1_f32];
        for i in 0..chars.len() {
            let _char = chars.get(i);
            let _start = short_starts.get(i);
            let _end = short_ends.get(i);
        }
    }
}

#[test]
fn test_null_ctx_is_tolerated_across_the_surface() {
    // Every extern fn null-checks its context/pointers; spot-check a
    // representative set (all are covered by the uniform catch wrapper).
    let null = std::ptr::null_mut();
    rust_tts_wrapper::tts_set_volume(null, 1.5);
    rust_tts_wrapper::tts_set_rate(null, 1.5);
    rust_tts_wrapper::tts_set_pitch(null, 1.5);
    rust_tts_wrapper::tts_set_voice(null, std::ptr::null());
    rust_tts_wrapper::tts_speak(null, std::ptr::null());
    rust_tts_wrapper::tts_set_on_audio(null, None, std::ptr::null_mut());

    // get_last_error on a null context: no crash; null ("no error") or a
    // readable C string from the global slot.
    let p = rust_tts_wrapper::tts_get_last_error(null);
    if !p.is_null() {
        let bytes = unsafe { std::ffi::CStr::from_ptr(p) }.to_bytes();
        assert!(bytes.len() < 4096, "error string unreasonably long");
    }
}

#[test]
fn test_measured_synthesis_refuses_null_arguments() {
    // Each argument null in turn, against an otherwise valid call: an
    // error, never a crash, and nothing to free. The record's out-pointer
    // too: the record is the point of the call, and a null one must not
    // turn it into a synthesis nobody measured.
    use rust_tts_wrapper as w;
    type Text = *const std::os::raw::c_char;
    type OutBytes = *mut *mut u8;
    type OutLen = *mut usize;
    type OutRecord = *mut *mut std::os::raw::c_char;

    fn nothing_written(bytes: *mut u8, len: usize, record: *mut std::os::raw::c_char) -> bool {
        bytes.is_null() && len == 0 && record.is_null()
    }

    let id = std::ffi::CString::new("openai").unwrap();
    let creds = std::ffi::CString::new(r#"{"apiKey":"dummy-key-for-ffi-tests"}"#).unwrap();
    let ctx = w::tts_create(id.as_ptr(), creds.as_ptr());
    assert!(
        !ctx.is_null(),
        "a cloud engine with a dummy key is created offline"
    );

    let text = std::ffi::CString::new("hello").unwrap();
    let mut bytes: *mut u8 = std::ptr::null_mut();
    let mut len: usize = 0;
    let mut record: *mut std::os::raw::c_char = std::ptr::null_mut();

    let cases: [(&str, *mut w::tts_ctx, Text, OutBytes, OutLen, OutRecord); 5] = [
        (
            "null context",
            std::ptr::null_mut(),
            text.as_ptr(),
            &mut bytes,
            &mut len,
            &mut record,
        ),
        (
            "null text",
            ctx,
            std::ptr::null(),
            &mut bytes,
            &mut len,
            &mut record,
        ),
        (
            "null audio out-pointer",
            ctx,
            text.as_ptr(),
            std::ptr::null_mut(),
            &mut len,
            &mut record,
        ),
        (
            "null length out-pointer",
            ctx,
            text.as_ptr(),
            &mut bytes,
            std::ptr::null_mut(),
            &mut record,
        ),
        (
            "null record out-pointer",
            ctx,
            text.as_ptr(),
            &mut bytes,
            &mut len,
            std::ptr::null_mut(),
        ),
    ];
    for (what, ctx, text, out_bytes, out_len, out_record) in cases {
        assert_eq!(
            w::tts_synth_to_bytes_measured(ctx, text, out_bytes, out_len, out_record),
            -1,
            "{what}"
        );
        assert!(
            nothing_written(bytes, len, record),
            "{what}: nothing to free after a refused call"
        );
    }
    w::tts_destroy(ctx);
}
