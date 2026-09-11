# Handoff: Windows FFI `last_error` regression

Session crashed mid-investigation. Picking up on another machine? Start here.

## Current state of `main`

- HEAD: `466a638` — `fix(ffi): tts_get_last_error was returning a dangling pointer (use-after-free)`
- Working tree: clean, in sync with `origin/main`
- CI: **failing on Windows only** (ubuntu/macos green)
  - `Test Suite (windows-latest, stable)` — run `28848757745`
  - `CI / windows-build` — run `28848757709`
  - Same root cause in both. Latest ~10 failing runs all collapse to this one test.

## The failing test

`tests/ffi_lifecycle.rs:140` — `test_ffi_synth_to_bytes_fails_deterministically_offline`

```rust
let id = CString::new("openai").unwrap();
let creds = CString::new(r#"{"apiKey":"dummy","synthUrl":"http://127.0.0.1:1/test"}"#).unwrap();
let ctx = tts_create(id.as_ptr(), creds.as_ptr());
let text = CString::new("hello").unwrap();
let mut out_bytes: *mut u8 = std::ptr::null_mut();
let mut out_len: usize = 0;
let rc = tts_synth_to_bytes(ctx, text.as_ptr(), &mut out_bytes, &mut out_len);
assert_eq!(rc, -1, ...);                                  // line 157 — PASSES
...
let err_ptr = tts_get_last_error(ctx);
assert!(!err_ptr.is_null(), ...);                         // line 163 — PASSES
let err = unsafe { std::ffi::CStr::from_ptr(err_ptr) }.to_string_lossy().into_owned();
assert!(!err.is_empty(), "last_error message must be non-empty ...");  // line 169 — FAILS
```

So: `tts_synth_to_bytes` returned `-1` (Err path was taken), `tts_get_last_error` returns a
**non-null pointer to an empty string**. Something between "write error" and "read error" is
losing the message.

## What commit `466a638` already did

- Changed `tts_ctx.last_error` from `Mutex<String>` → `Mutex<CString>` (src/lib.rs:91)
- `tts_get_last_error` (src/lib.rs:996-1020) now returns `guard.as_ptr()` directly under the
  lock instead of constructing a fresh CString whose backing allocation was dropped at end of
  scope (the original use-after-free).
- All three writers (`tts_speak_impl`, `tts_speak_sync`, `tts_synth_to_bytes`) now construct
  the `CString` once and assign it into the Mutex.

The fix is **correct on paper** for the dangling-pointer bug it targeted. The test still
fails on Windows, which means there is a second bug, or the fix has a subtle flaw.

## Hypotheses to investigate (in order of likelihood)

### 1. The write is happening, the read sees an empty CString

`CStr::is_empty()` returns true when the buffer is just the NUL terminator. For the test to
see non-null + empty, `tts_get_last_error` must be returning a pointer into an empty CString.
That implies `last_error` was *not* written by the synth failure, OR was reset to empty
between write and read.

- Check whether `tts_synth_to_bytes` actually reaches the `Err(e) =>` arm at src/lib.rs:1099.
  Add a temporary `eprintln!("synth_to_bytes err: {e:?}")` inside that arm and rerun on Windows.
- Possible silent path: `catch_unwind(...).unwrap_or(-1)` at src/lib.rs:1106 returns `-1`
  **without touching `last_error`** if the closure panics. If `engine.synth_to_bytes` panics
  on Windows (instead of returning `Err`), we get `-1` with an empty error. Wrap an
  `eprintln!` at the top of the closure and in a `.map_err` on the panic path:

  ```rust
  .unwrap_or_else(|panic| {
      eprintln!("tts_synth_to_bytes panicked: {panic:?}");
      -1
  })
  ```

### 2. reqwest on Windows returns Ok(empty body) instead of Err on connection refused

Test asserts `rc == -1` first (line 157) and that passes, so this is unlikely — but verify.
If `engine.synth_to_bytes` returned `Ok(vec![])`, `tts_synth_to_bytes` returns `0`, not `-1`.
Since `rc == -1` held, the Err path *was* entered. So the loss is between the Err arm and
the getter.

### 3. The CString pointer returned is being invalidated before the caller reads it

`guard.as_ptr()` is computed under the lock; the guard drops on return; the CString itself
lives in the Mutex so it should remain valid. This *should* be fine — but Windows' allocator
behaviour was the original culprit. Worth re-checking:

- Is the `Mutex<CString>` being moved or its container (`tts_ctx`) being freed between the
  write and the read? The test holds `ctx` alive across both calls, so no.
- Is there any `unsafe { &*ctx }` aliasing issue? `tts_create` returns `Box::into_raw`;
  `tts_synth_to_bytes` and `tts_get_last_error` both take `&*ctx` shared refs. Fine.

### 4. The error message itself contains a NUL byte

Unlikely for a `format!("HTTP error: {e}")` string, but `CString::new` would fail and the
`unwrap_or_else` falls back to `"error"` — which is non-empty. So even this path should
satisfy the assertion. Rule it out by inspection, not a real suspect.

## Reproducing

```bash
# Windows feature set (matches CI):
cargo test --no-default-features --features sapi,cloud,sherpaonnx \
    --test ffi_lifecycle test_ffi_synth_to_bytes_fails_deterministically_offline -- --nocapture

# Or the full FFI safety suite CI runs:
cargo test --no-default-features --features sapi,cloud,sherpaonnx --test ffi_safety --verbose
```

CI workflow: `.github/workflows/test.yml` — Windows uses
`FEATURES=--no-default-features --features sapi,cloud,sherpaonnx` (line 42).

Failed runs to diff against:
- `gh run view 28848757745 --log-failed`  (Test Suite, windows-latest)
- `gh run view 28848757709 --log-failed`  (CI, windows-build)

## Background context

The user was midway through cleaning up after a code review when the previous session died.
The series of recent commits shows the trajectory:

```
466a638 fix(ffi): tts_get_last_error was returning a dangling pointer (use-after-free)   ← HEAD, broke Windows
cac913e fix: Watson auth bug, check_credentials honesty, test-quality cleanup
0c862cb test: close FFI lifecycle, trait-method, and enum-variant gaps; fix README FFI table
a3f7d07 ci: drop cron from sherpaonnx-live workflow (manual + PR-only)
9efeb76 fix(tests): remove duplicate SAPI whitespace test, ignore network-dependent FFI test
e86a484 fix(tests): FFI lifecycle uses cloud engine (works on all CI feature sets)
323a3f8 test: expand suite across API layer and all engines; fix WAV header
```

`cac913e` tightened the test to assert `last_error` is non-empty. That assertion exposed a
*real* memory-safety bug (dangling pointer in `tts_get_last_error`). `466a638` fixed the
dangling pointer — but Windows still shows empty error strings, so there is a second bug
in the same area that the tightened assertion is now catching.

## Suggested next step

Add diagnostic `eprintln!`s inside `tts_synth_to_bytes` (top of closure, inside the Err arm,
and on the `catch_unwind` panic fallback) and run the test on a Windows machine or GH Actions
windows runner. The output will pin down whether:

- the Err arm runs and writes non-empty data (→ bug is in the getter/storage), or
- the Err arm doesn't run and the panic fallback is producing `-1` (→ bug is in
  `engine.synth_to_bytes` panicking instead of returning Err on Windows), or
- something else entirely.

Delete this file once the regression is fixed.
