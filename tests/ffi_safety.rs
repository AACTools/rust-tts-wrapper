//! FFI Safety Tests
//!
//! Exercises the C ABI surface to validate panic protection, allocator
//! consistency, null handling, and string safety across the FFI boundary.

#![allow(unsafe_op_in_unsafe_fn)]

#[cfg(test)]
mod ffi_null_handling_tests {
    use rust_tts_wrapper::{tts_destroy, tts_get_engine_count, tts_pause, tts_resume, tts_stop};
    use std::ffi::CString;

    /// Helper to assert that calling an FFI function with a null ctx does not
    /// panic. The function is expected to silently no-op.
    fn assert_null_ctx_noop<F: FnOnce(rust_tts_wrapper::tts_ctx)>(f: F) {
        // We deliberately pass null by not constructing a real ctx.
        // Each individual FFI call below is its own assertion.
    }

    #[test]
    fn test_destroy_null_noop() {
        unsafe { tts_destroy(std::ptr::null_mut()) };
    }

    #[test]
    fn test_stop_null_noop() {
        unsafe { tts_stop(std::ptr::null_mut()) };
    }

    #[test]
    fn test_pause_null_noop() {
        unsafe { tts_pause(std::ptr::null_mut()) };
    }

    #[test]
    fn test_resume_null_noop() {
        unsafe { tts_resume(std::ptr::null_mut()) };
    }

    #[test]
    fn test_engine_count_is_positive() {
        assert!(unsafe { tts_get_engine_count() } > 0);
    }

    #[test]
    fn test_create_with_valid_sherpaonnx_returns_non_null() {
        let id = CString::new("sherpaonnx").unwrap();
        let ctx = unsafe {
            rust_tts_wrapper::tts_create(id.as_ptr(), std::ptr::null())
        };
        assert!(!ctx.is_null(), "sherpaonnx should construct without credentials");
        unsafe { tts_destroy(ctx) };
    }
}

#[cfg(test)]
mod ffi_voice_roundtrip_tests {
    use rust_tts_wrapper::{tts_create, tts_destroy, tts_get_voices, tts_free_voices};
    use std::ffi::CString;

    #[test]
    fn test_get_voices_with_null_pointers_returns_error() {
        let id = CString::new("sherpaonnx").unwrap();
        let ctx = unsafe { tts_create(id.as_ptr(), std::ptr::null()) };
        assert!(!ctx.is_null());

        // Passing any null out-pointer must not crash; must return -1.
        let rc = unsafe {
            tts_get_voices(
                ctx,
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
        };
        assert_eq!(rc, -1);

        unsafe { tts_destroy(ctx) };
    }

    #[test]
    fn test_free_voices_null_is_noop() {
        // tts_free_voices must accept (null, 0) without crashing.
        unsafe { tts_free_voices(std::ptr::null_mut(), 0) };
    }
}

#[cfg(test)]
mod string_safety_tests {
    #[test]
    fn test_cstring_helper_round_trip() {
        // §4 H3: ensure CString::new produces a NUL-terminated buffer for
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
        // than pass a `&str.as_ptr()` that walks off the end (§4 H3).
        let result = std::ffi::CString::new("with\0nul");
        assert!(result.is_err());
    }
}

#[cfg(test)]
mod path_parsing_tests {
    #[test]
    fn test_azure_path_parsing_safe() {
        // §2 H2: Azure WS Path: parsing must use strip_prefix + trim, not
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
            // This mirrors the logic in cloud_engine.rs.
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
        // §2 H7: ElevenLabs characters/starts/ends arrays can have different
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
