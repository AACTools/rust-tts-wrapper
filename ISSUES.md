# rust-tts-wrapper — Issues, Gaps, and Bugs

Findings from a thorough audit of all source files on 2026-07-05. Line numbers are
accurate to the version of each file inspected. Severities:

- **Critical** — incorrect behaviour, UB, data loss, or compile failure on a supported target
- **High** — silent failure, broken engine, or significant API gap
- **Medium** — wrong/incomplete behaviour that still compiles and partly works
- **Low** — polish, missing convenience, maintenance hazard

---

## 1. SherpaOnnx engine (`src/sherpaonnx_engine.rs`)

### ✅ C1. FIXED - Only Kokoro models work — 188 of 191 models in the registry are unusable
- **Files / lines:** `src/sherpaonnx_engine.rs:167-192`
- **Issue:** `speak` *always* builds an `OfflineTtsKokoroModelConfig` and leaves
  `vits`, `matcha`, `kitten`, `zipvoice`, `pocket`, `supertonic` at
  `Default::default()` with empty paths. The registry contains:
  - 3 `kokoro` models (these work)
  - 184 `vits` models (will fail to load — empty `model`/`tokens`/`lexicon` paths)
  - 4 `matcha` models (will fail to load)
- **Why it matters:** The README and `lib.rs:5` advertise "191 local models", but
  only ~1.6% are actually functional. Selecting any non-Kokoro model fails with
  a generic "Failed to create SherpaOnnx TTS engine" from `OfflineTts::create`.
- **Severity:** Critical

### ✅ C2. FIXED - Hardcoded Kokoro file layout for every model
- **Files / lines:** `src/sherpaonnx_engine.rs:168-176`
- **Issue:** Always looks for `model.onnx`, `voices.bin`, `tokens.txt`, and
  `espeak-ng-data/`. VITS models use `lexicon.txt`, Matcha uses
  `model-steps-*.onnx` + `dict/jieba`, etc.
- **Why it matters:** Even if C1 were fixed for `model_type`, the file paths are wrong.
- **Severity:** Critical (compounds C1)

### ✅ H1. FIXED - Rate is applied twice (double-speed or half-speed)
- **Files / lines:** `src/sherpaonnx_engine.rs:177` (`length_scale = 1.0 / rate`)
  and `src/sherpaonnx_engine.rs:205` (`speed: rate`)
- **Issue:** Kokoro's `length_scale` controls duration inversely, and
  `GenerationConfig.speed` *also* controls playback rate. Setting both means the
  effective rate compounds (≈ `rate * rate` for Kokoro).
- **Why it matters:** Speech plays at the wrong speed for every non-default rate.
- **Severity:** High

### ✅ H2. FIXED - Pitch and volume silently ignored
- **Files / lines:** `src/sherpaonnx_engine.rs` (speak fn)
- **Issue:** No prosody control applied; the `gen_config` doesn't set pitch or
  volume either.
- **Why it matters:** Accessibility users (the target audience per
  `Cargo.toml:8`) cannot adjust pitch/volume on the only offline engine.
- **Fix:** Sherpa-ONNX has no native pitch/volume hooks, so we post-process
  the rendered samples: volume via amplitude scaling, pitch via linear
  interpolation resampling.
- **Severity:** High

### ✅ H3. FIXED - Hardcoded default model id `"kokoro-en-en-19"`
- **Files / lines:** `src/sherpaonnx_engine.rs` (new fn)
- **Issue:** If `modelId` is not supplied in credentials, defaults to
  `kokoro-en-en-19`, which forces a ~305 MB download the user never asked for.
- **Fix:** No default model id; `speak()` returns an error directing the user
  to pass `modelId` in credentials.
- **Severity:** Medium

### ✅ H4. FIXED - Hardcoded `num_threads: 2` and no provider option
- **Files / lines:** `src/sherpaonnx_engine.rs` (new fn)
- **Issue:** No way to tune CPU usage or select CoreML/CUDA/DirectML provider.
- **Fix:** Reads `numThreads` and `provider` from the credentials JSON.
- **Severity:** Medium

### ✅ M1. FIXED - `play_wav_file` is Linux-only and silently fails
- **Files / lines:** `src/sherpaonnx_engine.rs` (play_wav_file)
- **Issue:** Shells out to `aplay`. On Windows/macOS, or pipewire-only Linux
  distros, the audio is rendered to a temp file but never played; the failure is
  swallowed by `let _ =`.
- **Fix:** Dispatches to `aplay` (Linux), `afplay` (macOS), or PowerShell
  `Media.SoundPlayer` (Windows). Also writes the WAV header manually so we no
  longer depend on sherpa-onnx's `GeneratedAudio::save`.
- **Severity:** Medium

### ✅ M2. FIXED - Voice list mislabels `iso639_3` with 2-letter codes
- **Files / lines:** `src/sherpaonnx_engine.rs` (iso639_3 fn)
- **Issue:** Sets `iso639_3` to the 2-letter `lang_code` (e.g. `"en"`); ISO 639-3
  requires 3-letter codes (e.g. `"eng"`).
- **Fix:** Added an iso639_3() lookup covering every language in the registry.
- **Severity:** Medium

### ✅ L1. FIXED - Cancellation callback always returns `true`
- **Files / lines:** `src/sherpaonnx_engine.rs` (CANCEL_REQUESTED)
- **Issue:** Sherpa's progress callback returns `bool` (false = stop). Returning
  `true` unconditionally means `tts_stop` cannot actually cancel in-progress
  synthesis.
- **Fix:** `stop()` sets a static `AtomicBool`; the progress callback reads it
  and returns `false` to abort synthesis. (Limitation: the flag is process-wide,
  not per-engine — fine for the typical one-engine-per-process usage.)
- **Severity:** Low

### L2. No runtime model switching
- **Files / lines:** `src/sherpaonnx_engine.rs:15-19, 44-49`
- **Issue:** `loaded_model_id` is fixed at construction; switching models
  requires destroying and recreating the engine.
- **Severity:** Low

---

## 2. Cloud engine (`src/cloud_engine.rs`)

### DONE C1. FIXED - Watson Basic-auth header is malformed
- **Files / lines:** `src/cloud_engine.rs:254-257`
- **Issue:** The code emits `Basic <base64(apiKey)>:` (trailing colon *outside*
  the base64). Watson expects `Basic <base64("apikey:" + apiKey)>`.
- **Why it matters:** Every Watson request will fail auth.
- **Severity:** Critical

### DONE C2. FIXED - Amazon Polly is fundamentally non-functional
- **Files / lines:** `src/cloud_engine.rs:287-294`
- **Issue:** Polly requires AWS Signature V4. This implementation sends an
  unauthenticated JSON POST. The region from credentials is *never read*, so the
  URL is hard-pinned to `us-east-1` (`cloud_engine.rs:288`). Polly's actual
  response schema (`DescribeVoices` for listing, `SynthesizeSpeech` for synth) is
  also different from the generic handler.
- **Why it matters:** Every Polly call returns 403. The engine is dead weight
  and gives users the impression Polly is supported.
- **Severity:** Critical

### ✅ C3. FIXED - PlayHT puts `userId` in the JSON body, but the API requires `X-User-ID` header
- **Files / lines:** `src/cloud_engine.rs` (playht branch)
- **Issue:** `extra_body` puts `user_id` as a JSON field. PlayHT v2 requires
  `X-User-ID` header + `Authorization: Bearer <secret>`.
- **Fix:** Reads `userId` from credentials and emits it in `extra_headers`
  under `X-User-ID`. Covered by `test_playht_config_has_user_id_header`.
- **Severity:** Critical

### ✅ H1. FIXED - Azure WebSocket `X-RequestId` is the wrong format
- **Files / lines:** `src/cloud_engine.rs` (Azure WS branch)
- **Issue:** Azure requires a 32-char hex UUID with **no dashes**. Code uses
  `Uuid::new_v4().to_string().to_lowercase()`, which keeps the dashes.
- **Fix:** Uses `Uuid::new_v4().simple().to_string()` which yields the
  32-char hex form.
- **Severity:** High

### ✅ H2. FIXED - Azure WS response `Path:` parser assumes ASCII and unsafe slicing
- **Files / lines:** `src/cloud_engine.rs` (Azure WS loop)
- **Issue:** `l[5..]` is byte slicing on a UTF-8 `&str`. If Azure ever sends a
  non-ASCII character before `Path:`, this panics. Should use
  `l.strip_prefix("Path:")` and `.trim()`.
- **Severity:** High (panic across FFI = UB, see §4 C1)

### ✅ H3. FIXED - Azure WS lacks required `X-Timestamp` header
- **Files / lines:** `src/cloud_engine.rs` (Azure WS branch)
- **Issue:** Azure protocol requires `X-Timestamp:<ISO 8601>` in every message.
  The code only sets `X-RequestId`, `Content-Type`, and `Path`. May cause the
  service to drop the message.
- **Fix:** Every Azure WS message now carries an `X-Timestamp` header built
  from a UTC `SystemTime` (no chrono dependency).
- **Severity:** High

### ✅ H4. FIXED - Azure WS doesn't handle `turn.start`, `response`, or error paths
- **Files / lines:** `src/cloud_engine.rs` (Azure WS loop)
- **Issue:** Only handles `turn.end`, `audio.metadata`, `word-boundary`. Errors
  arrive as `Path:response` with a JSON body containing `Error`; they will be
  silently ignored and the loop will hang until the connection closes.
- **Fix:** The loop now parses the JSON body of every text message and, if it
  contains an `Error` object (or `reason`), surfaces it as a `TtsError` and
  closes the socket cleanly.
- **Severity:** High

### H5. Azure WS `Uuid` is `to_lowercase()` *after* `to_string()` (minor) but value is fine
- **Files / lines:** `src/cloud_engine.rs:559`
- **Note:** Not a bug per se, but combined with H1 means the value is the wrong
  shape.

### ✅ H6. FIXED - Azure SSML drops volume entirely
- **Files / lines:** `src/cloud_engine.rs` (build_azure_ssml)
- **Issue:** `build_azure_ssml` takes `rate` and `pitch` only. Volume is never
  rendered into `<prosody volume="...">`, even though it's plumbed through
  `speak(...)`. The Azure WS path (`cloud_engine.rs:583`) and HTTP path both
  silently lose volume.
- **Fix:** `build_azure_ssml` now takes a `volume: f32` and emits a
  `volume="x-soft".."x-loud"` prosody attribute. Both call sites updated.
- **Severity:** High (accessibility regression)

### ✅ H7. FIXED - ElevenLabs alignment parser can index out of bounds
- **Files / lines:** `src/cloud_engine.rs:805-808`
- **Issue:** Loop bounds are `0..chars.len()`, but it indexes `starts[i]` and
  `ends[i]`. If ElevenLabs returns `characters` longer than the time arrays
  (which can happen), the code panics. See §4 C1 for panic-safety implications.
- **Severity:** High

### ✅ H8. FIXED - ElevenLabs voice list parser: `labels` is an object, not a string
- **Files / lines:** `src/cloud_engine.rs` (generic voice parser)
- **Issue:** The generic voice parser does
  `v.get("gender").or(v.get("labels")).and_then(|v| v.as_str())`. ElevenLabs'
  `labels` is an object (`{"gender": "female", "age": "young", ...}`), so
  `.as_str()` returns `None` and gender falls back to `Unknown`.
- **Fix:** When `labels` is an object, dive into `labels.gender`. Also resolves
  `labels.language` for the language code.
- **Severity:** Medium (gender is wrong but engine still works)

### ✅ H9. FIXED - Generic voice parser doesn't handle PascalCase fields (Polly, AWS)
- **Files / lines:** `src/cloud_engine.rs` (generic voice parser)
- **Issue:** Polly's `DescribeVoices` returns `{"VoiceId": "...", "Gender": "...",
  "LanguageCode": "..."}`. The parser only looks for lowercase `id`, `voice_id`,
  `name`, `gender`. Polly voice listing will return an empty list even if
  signing were fixed.
- **Fix:** The parser now also accepts `VoiceId`, `Name`, `Gender`,
  `LanguageCode` (case-insensitive on the field lookup).
- **Severity:** Medium (compounds C2)

### DONE H10. FIXED - Deepgram API shape is wrong
- **Files / lines:** `src/cloud_engine.rs:155-164`
- **Issue:** Deepgram's `/v1/speak` takes `model` as a query parameter (or in
  the new body shape `{ "model": "aura-asteria-en", "text": "..." }`). Current
  code sends `{ "voice": "aura-asteria-en", "text": "..." }`, which Deepgram
  will reject.
- **Severity:** High

### DONE H11. FIXED - Hume TTS body shape is wrong
- **Files / lines:** `src/cloud_engine.rs:189-197`
- **Issue:** Hume TTS expects
  `{ "text": "...", "voice": {"name": "..."}, "audio_format": "wav" }`. Current
  code sends `voice` as a string.
- **Severity:** High

### ✅ H12. FIXED - Azure SSML voice name is not escaped
- **Files / lines:** `src/cloud_engine.rs` (build_azure_ssml)
- **Issue:** Text is XML-escaped but `voice` is interpolated raw inside single
  quotes. A `'` in a voice name would break the SSML.
- **Fix:** Voice name is now escaped (&, <, >, ', ") before interpolation.
  Covered by `test_azure_ssml_escapes_voice_name`.
- **Severity:** Low (voice names are typically ASCII identifiers)

### M1. Continuous rate/pitch buckets discard precision
- **Files / lines:** `src/cloud_engine.rs:315-328`
- **Issue:** Azure only gets 5 discrete prosody buckets (`x-slow`…`x-fast`). A
  rate of 1.2 and 1.4 are the same SSML. Azure supports percentage prosody
  (`rate="+20%"`) which would preserve precision.
- **Severity:** Medium

### M2. Google timepoint `words_list` rebuilt every iteration
- **Files / lines:** `src/cloud_engine.rs:361-367`
- **Issue:** Inside the `for (i, w) in words.iter().enumerate()` loop, line 366
  rebuilds `words_list` from `words` on every iteration. Functional but
  wasteful; should move the assignment outside the loop.
- **Severity:** Low

### M3. No timeouts on Azure WebSocket
- **Files / lines:** `src/cloud_engine.rs:567-568`
- **Issue:** `tungstenite::connect` blocks indefinitely if Azure hangs. There is
  no read timeout in the message loop either. A stalled Azure connection will
  hang `tts_speak` forever (which holds the engine Mutex, blocking every other
  caller — see §4 H1).
- **Severity:** Medium

### M4. `socket.close(None)` on `turn.end` may discard trailing audio
- **Files / lines:** `src/cloud_engine.rs:609-612`
- **Issue:** Closing immediately on `turn.end` may drop any final binary frames
  still in flight.
- **Severity:** Medium

### M5. Azure WS output format hardcoded
- **Files / lines:** `src/cloud_engine.rs:570`
- **Issue:** `audio-24khz-96kbitrate-mono-mp3` is the only format. No way to
  request PCM, WAV, or higher bitrates.
- **Severity:** Medium

### M6. Polly/fish/mistral/etc. lack `model_default`
- **Files / lines:** `src/cloud_engine.rs:155-294`
- **Issue:** Several engines require a `model` field that isn't set; users must
  supply it via `extra` even though the Swift/dotnet wrappers set sensible
  defaults.
- **Severity:** Medium

### L1. Cloud engines that don't provide `voices_url` always return `[]`
- **Files / lines:** `src/cloud_engine.rs:927-929`
- **Issue:** 15 of 19 cloud engines have no voice list endpoint configured
  (deepgram, playht, fishaudio, hume, mistral, murf, resemble, unrealspeech,
  upliftai, watson, witai, xai, modelslab, polly, openai). For those,
  `get_voices()` returns an empty Vec with no error.
- **Severity:** Low

### L2. `compute_durations` defined with `#[cfg(feature = "cloud")]`
- **Files / lines:** `src/cloud_engine.rs:502-527`
- **Issue:** Only callable from the cloud impl; it's effectively private.
  Inconsistent visibility.
- **Severity:** Low

---

## 3. SAPI engine (`src/sapi_engine.rs`)

### ✅ C1. FIXED - Bare `SpVoice` / `SpObjectTokenCategory` are not valid `windows` crate symbols
- **Files / lines:** `src/sapi_engine.rs:30`, `src/sapi_engine.rs:37`
- **Issue:** The windows-rs 0.61 API does not expose these as bare constants.
  `windows` 0.61 expects `SPVOICE_CLSID` (or `windows::core::GUID::from_u128`)
  for the CLSID, and the SAPI category class is similarly namespaced.
  `Cargo.toml:39` pins `windows = "0.61"`.
- **Why it matters:** Documented in `TODO.md:10-15` — **all three Windows CI
  builds fail**. The SAPI feature does not compile.
- **Severity:** Critical

### C2. `COINIT_MULTITHREADED` and supporting constants may be deprecated in 0.61
- **Files / lines:** `src/sapi_engine.rs:29`
- **Issue:** Same root cause as C1. The `windows` crate moves APIs rapidly; the
  current code was written against an older binding.
- **Severity:** Critical

### ✅ H1. FIXED - `sapi` feature has no platform guard
- **Files / lines:** `Cargo.toml`, `build.rs`
- **Issue:** `sapi = ["windows"]` enables the feature on any platform, but
  `windows` is only a dependency under
  `[target.'cfg(target_os = "windows")'.dependencies]`. Enabling `sapi` on
  Linux/macOS produces a "crate not found" error rather than a helpful message.
  (The `lib.rs:38` module gate does protect the source file, but the dep
  resolution fails first.)
- **Fix:** `build.rs` now emits a clear panic when `--features sapi` is set on
  a non-Windows target, listing the condition that wasn't met.
- **Severity:** High

### H2. Real SAPI word-boundary events not implemented
- **Files / lines:** `src/sapi_engine.rs` (speak fn)
- **Issue:** SAPI exposes real word boundaries via `ISpEventSource::SetNotify`
  / `ISpVoice::GetStatus` / `SPEI_WORD_BOUNDARY`. The implementation falls back
  to `estimate_word_boundaries`, giving inaccurate timing.
- **Status:** Still pending; estimate fallback is unchanged. Implementing real
  events requires wiring up `ISpEventSource::SetNotify` + a callback sink,
  which is a substantial change.
- **Severity:** Medium (functional but inaccurate)

### ✅ H3. FIXED - `Speak` return value discarded
- **Files / lines:** `src/sapi_engine.rs` (speak fn)
- **Issue:** `let _ = sp_voice.Speak(...)` swallows errors. Failures (e.g.
  invalid voice id, COM re-entrancy) become silent `Ok(())`.
- **Fix:** Both Speak call sites now propagate `windows::core::Error` as
  `TtsError`.
- **Severity:** Medium

### ✅ H4. FIXED - `CoInitializeEx` is never paired with `CoUninitialize`
- **Files / lines:** `src/sapi_engine.rs` (Drop impl)
- **Issue:** Each `SapiEngine::new()` increments the COM init count without
  decrementing. Drops also don't uninitialize. Slow leak of COM refs.
- **Fix:** `SapiEngine` now tracks whether it successfully called
  `CoInitializeEx` and the `Drop` impl calls `CoUninitialize` exactly once
  when matched.
- **Severity:** Medium

### ✅ M1. FIXED - Pitch implemented as SSML wrapper around the *entire* text
- **Files / lines:** `src/sapi_engine.rs` (speak fn)
- **Issue:** `<pitch absmiddle="..."/>` is prepended to the body. SAPI's pitch
  tag uses `<prosody pitch="...">`, not `<pitch absmiddle>`. The latter is a
  non-standard extension that older SAPI 5.1 may not honour.
- **Fix:** Switched to standard SSML `<prosody pitch="+N%">` inside a proper
  `<speak>` envelope.
- **Severity:** Medium

### M2. `find_voice_by_id` is called under the voice Mutex during `speak`
- **Files / lines:** `src/sapi_engine.rs:94-98`
- **Issue:** COM token enumeration can be slow (registry access); the engine
  Mutex is held for the duration, blocking other callers. Should cache the token
  after `set_voice`.
- **Severity:** Medium

---

## 4. C ABI / FFI (`src/lib.rs`, `include/tts_wrapper.h`, `bindings/`)

### ✅ C1. FIXED - Most FFI functions do not catch panics — undefined behaviour on unwind
- **Files / lines:** `src/lib.rs:97` (`tts_create`), `src/lib.rs:320`
  (`tts_get_voices`) are the **only** functions wrapped in `catch_unwind`.
  Everything else — `tts_speak` (`lib.rs:168`), `tts_speak_sync` (`lib.rs:233`),
  `tts_stop` (`lib.rs:295`), `tts_synth_to_bytes` (`lib.rs:624`), `tts_set_*`,
  `tts_destroy` — can panic and unwind across the FFI boundary.
- **Sources of panic:**
  - `Mutex::lock().unwrap()` — system_engine.rs:48, 91, 100, 108;
    sapi_engine.rs:84, 92, 150, 164, 174; avsynth_engine.rs:66, 73, 108, 116,
    124, 132, 192; sherpaonnx_engine.rs: none directly but the engine Mutex in
    `lib.rs` itself.
  - `Layout::array::<...>(len).unwrap()` — lib.rs:346, 408, 568, 653, 678.
  - `CString::new(...).unwrap()` — lib.rs:354-360, 532-537 (panics if a voice
    name or id contains `\0`).
  - `cloud_engine.rs:807` (potential out-of-bounds, see §2 H7).
  - `cloud_engine.rs:607` (string slice panic, see §2 H2).
- **Why it matters:** Rust's calling convention does not support unwinding
  through `extern "C"`. Doing so is **undefined behaviour** in stable Rust.
  Any panic can corrupt the host process.
- **Severity:** Critical

### DONE C2. FIXED - `tts_get_engines` writes into caller-allocated memory but `tts_free_engine_info` calls `std::alloc::dealloc`
- **Files / lines:** `lib.rs:521-542` (write into caller buffer) vs
  `lib.rs:550-572` (dealloc with Rust allocator)
- **Issue:** The API contract is: caller allocates the array (C `malloc`,
  Swift `UnsafeMutablePointer`, C# `Marshal.AllocHGlobal`, etc.). Then
  `tts_free_engine_info` calls Rust's `std::alloc::dealloc` on that pointer.
  Mixing allocators is UB. (Contrast with `tts_get_voices`, which allocates
  with Rust and is freed with Rust — internally consistent.)
- **Why it matters:** Freeing memory through the wrong allocator is classic
  heap corruption.
- **Severity:** Critical

### DONE C3. FIXED - `tts_get_last_error` reads a stale global, never the per-context error
- **Files / lines:** `lib.rs:75` (`static LAST_ERROR`), `lib.rs:77-81`
  (`set_error` writes the global), `lib.rs:218, 283, 372, 663` (per-ctx
  `last_error` field written but never read), `lib.rs:578-586` (returns the
  global)
- **Issue:** Only `tts_create` failures set the global `LAST_ERROR`. All other
  errors (speak, synth_to_bytes, get_voices) write to `ctx_ref.last_error`
  which is **never read** by any FFI function. So a failed `tts_speak` followed
  by `tts_get_last_error` returns either null or the stale error from a
  previous `tts_create` call.
- **Why it matters:** C-level diagnostics are broken; every binding inherits
  the bug.
- **Severity:** Critical

### H1. `tts_speak` holds the engine Mutex for the entire (synchronous) synthesis
- **Files / lines:** `src/lib.rs` (tts_speak / tts_speak_sync / tts_synth_to_bytes)
- **Issue:** `engine.lock().unwrap()` is held for the duration of
  `engine.speak(...)`. For the cloud engine, that's an HTTP request (or a full
  WebSocket session — seconds to tens of seconds). Any concurrent call
  (`tts_stop`, `tts_set_voice`, `tts_destroy`) blocks.
- **Status:** Partially mitigated — all other Mutexes (voice/rate/pitch/volume/
  callbacks) are released before the engine Mutex is acquired, and the engine
  Mutex is held only for the synthesis call itself. A deeper fix would replace
  `Mutex<Box<dyn TtsEngine>>` with `Arc<dyn TtsEngine + Send + Sync>` (engines
  already synchronise internally); deferred because the trait object bounds
  ripple through factory.rs.
- **Severity:** High

### ✅ H2. FIXED - `tts_set_on_audio` updates `cb` and `userdata` in two separate critical sections
- **Files / lines:** `src/lib.rs` (AudioCallback / BoundaryCallback)
- **Issue:** A reader (`tts_speak`) can observe a new callback
  paired with the old userdata (or vice versa). Result: callback invoked with a
  dangling `userdata` pointer.
- **Fix:** Each callback type now bundles `(cb, userdata)` into a single
  Mutex-protected struct with `#[derive(Clone, Copy)]`. Both setters and
  readers take a single lock, so the snapshot is always consistent.
- **Severity:** High (potential UB if userdata lifetimes differ)

### ✅ H3. FIXED - AvSynth FFI passes non-null-terminated UTF-8 to NSString
- **Files / lines:** `src/avsynth_engine.rs` (speak fn), `extern/avsynth_shim.m`
- **Issue:** `&str::as_ptr()` is **not** null-terminated. `stringWithUTF8String:`
  reads until `\0`, walking past the end of the Rust string into adjacent heap
  memory.
- **Fix:** Rust now wraps `text` and `voice_id` in `CString::new(...)?` (which
  surfaces interior-NUL errors rather than truncating). The extern signatures
  use `*const c_char` to make the contract explicit. Covered by FFI tests.
- **Severity:** Critical (macOS only, but the only macOS path)

### ✅ H4. FIXED - `tts_destroy` does not stop in-progress speech or close resources
- **Files / lines:** `src/lib.rs` (tts_destroy)
- **Issue:** Just `Box::from_raw` + drop. `SystemEngine` has no `Drop`, so the
  speech-dispatcher connection is leaked (and any in-flight utterance
  continues). SAPI's `Drop` doesn't `Speak("", PURGEBEFORESPEAK)`.
- **Fix:** `tts_destroy` now calls `engine.stop()` before dropping the context,
  wrapped in `catch_unwind`. Engine `Drop` impls remain responsible for any
  further cleanup (closing sockets, releasing COM refs, etc.).
- **Severity:** Medium

### M1. `tts_get_voices_inner` leaks partial allocations on panic
- **Files / lines:** `lib.rs:346-364`
- **Issue:** If CString allocation panics mid-loop, the previously-allocated
  string pointers and the outer array are leaked. `catch_unwind` in the caller
  turns the panic into `-1` but the memory is gone.
- **Severity:** Medium

### M2. `tts_destroy` accepts null as no-op, but other functions are inconsistent
- **Files / lines:** `lib.rs` (everywhere)
- **Issue:** Some functions early-return on null ctx (`tts_speak`, `tts_stop`,
  etc.); some don't validate at all (none skip, but each re-implements the
  check). No central validation helper.
- **Severity:** Low

### M3. `tts_get_engines` API is awkward
- **Files / lines:** `lib.rs:521-542`
- **Issue:** Caller must call `tts_get_engine_count` first, then allocate the
  exact-size array, then call `tts_get_engines`. The function doesn't take a
  capacity parameter or return the count. Combined with C2, this API is
  unusable from C.
- **Severity:** Medium

### M4. AvSynth extern declarations duplicated
- **Files / lines:** `src/avsynth_engine.rs:8-33` and
  `include/tts_wrapper.h:272-298`
- **Issue:** Two sources of truth for the avsynth FFI surface. They could
  diverge silently.
- **Severity:** Low

### M5. `engine.rs:12-19` declares unused callbacks
- **Files / lines:** `src/engine.rs:13-19`
- **Issue:** `OnStartCallback`, `OnEndCallback`, `OnErrorCallback` are defined
  but never used by any engine or exposed via FFI.
- **Severity:** Low

### M6. Missing FFI functions vs dotnet-tts-wrapper feature set
- **Files / lines:** `lib.rs` (overall)
- **Issue:** The dotnet/Swift wrappers expose `check_credentials`,
  `synth_with_boundaries` (returning audio+boundaries together), per-engine
  introspection, and start/end/error callbacks. The C ABI only exposes
  `synth_to_bytes`, `get_voices`, `speak`, `speak_sync`. `synth_with_boundaries`
  (declared in `engine.rs:171-182`) is **not** wired to FFI.
- **Severity:** Medium

### M7. `synth_with_boundaries` discards real boundary data
- **Files / lines:** `src/engine.rs:171-182`
- **Issue:** The default impl calls `synth_to_bytes` (no boundary callback)
  then estimates boundaries from text. For engines that have *real* boundaries
  (Azure, ElevenLabs, Google, SherpaOnnx Kokoro), this returns worse data than
  what the engine knows. No engine overrides it.
- **Severity:** Medium

---

## 5. Cloud engine voice-listing (`src/cloud_engine.rs`)

### H1. ElevenLabs `labels` gender handled wrong (see §2 H8)
### H2. Polly voice parser misses PascalCase fields (see §2 H9)

---

## 6. System engine (`src/system_engine.rs`)

### ✅ M1. FIXED - `get_voices` always returns empty
- **Files / lines:** `src/system_engine.rs` (get_voices)
- **Issue:** speech-dispatcher supports `list_synthesis_voices()`. Returning
  empty means the C `tts_get_voices` reports no voices on Linux, even when
  voices are installed.
- **Fix:** Implemented via `Connection::list_synthesis_voices`, mapping each
  speech-dispatcher voice into the wrapper `Voice` struct (gender defaults to
  Unknown since speech-dispatcher doesn't expose it).
- **Severity:** Medium

### M2. Mutex held during `conn.say(...)` which queues async speech
- **Files / lines:** `src/system_engine.rs:48-61`
- **Issue:** Less severe than §4 H1 (speech-dispatcher calls return quickly),
  but the lock pattern is the same. Calling `pause` while speech is queued
  blocks on the same Mutex.
- **Severity:** Low

### L1. `Priority::Important` is hardcoded
- **Files / lines:** `src/system_engine.rs:61`
- **Issue:** No way for the caller to choose `Priority::Notification` etc.
- **Severity:** Low

---

## 7. AvSynth engine (`src/avsynth_engine.rs`)

### C1. Non-null-terminated strings passed across FFI (see §4 H3)
- **Files / lines:** `src/avsynth_engine.rs:76-78`
- **Severity:** Critical

### M1. `Arc<Mutex<*mut c_void>>` with raw pointer
- **Files / lines:** `src/avsynth_engine.rs:37-39`
- **Issue:** Manual `unsafe impl Send + Sync`. Acceptable for a single-threaded
  use, but if the engine is ever invoked from multiple threads the underlying
  `AVSpeechSynthesizer` is not guaranteed thread-safe.
- **Severity:** Medium

### M2. Fixed-size voice buffers (256/64 bytes)
- **Files / lines:** `src/avsynth_engine.rs:144-146`
- **Issue:** If a voice id or name exceeds the buffer, it's silently truncated.
- **Severity:** Low

---

## 8. Trait / types (`src/engine.rs`, `src/types.rs`)

### M1. `SherpaModelInfo` missing registry fields
- **Files / lines:** `src/types.rs:221-241` vs the JSON which has `developer`,
  `quality`
- **Issue:** Registry JSON has `developer` and `quality` (see
  `merged_models.json` lines 4-5) that aren't parsed.
- **Severity:** Low

### M2. `EngineDescriptor::credential_keys_json` is a JSON-encoded string
- **Files / lines:** `src/types.rs:217`
- **Issue:** Stored as a `String` containing JSON. Consumers must re-parse to
  get the list. Encoded form leaks through the abstraction.
- **Severity:** Low

### L1. `AudioFormat` enum has no use site
- **Files / lines:** `src/types.rs:71-80`, `SpeakOptions.format`
- **Issue:** Field exists in `SpeakOptions` but no engine honours it; output
  format is always whatever the engine returns (typically MP3).
- **Severity:** Low

---

## 9. Factory (`src/factory.rs`)

### ✅ H1. FIXED - Cloud catch-all masks missing-feature errors
- **Files / lines:** `src/factory.rs` (create_engine)
- **Issue:** When the `cloud` feature is on but `system`/`sherpaonnx` are off,
  calling `tts_create("system", ...)` falls into the cloud catch-all which
  returns `None` because "system" isn't a cloud provider. The user gets
  "Unknown engine: system" rather than "system feature not enabled".
- **Fix:** `create_engine` now matches each gated engine id explicitly. When
  the feature is disabled it `eprintln!`s a clear rebuild hint. For unknown
  ids, the warning lists the engines actually available in this build.
- **Severity:** Medium

### L1. Engine list hardcoded twice (factory.rs and cloud_engine.rs)
- **Files / lines:** `src/factory.rs:92-122` and `src/cloud_engine.rs:74-296`
- **Issue:** Two parallel lists of cloud provider IDs. Could drift.
- **Severity:** Low

---

## 10. Build config (`build.rs`, `cbindgen.toml`, `Cargo.toml`)

### M1. `cc` is an unconditional build-dependency but only used on macOS
- **Files / lines:** `Cargo.toml:43`, `build.rs:19-28`
- **Issue:** Should be under
  `[target.'cfg(target_os = "macos")'.build-dependencies]`. Adds compile time
  on Windows/Linux.
- **Severity:** Low

### M2. `build.rs` uses `env::var(...).unwrap()` and `unwrap_or_default()` for cbindgen
- **Files / lines:** `build.rs:4-5`
- **Issue:** `CARGO_MANIFEST_DIR` is always set by Cargo, but `unwrap()` is
  fragile if the build script is ever invoked manually. The cbindgen
  `unwrap_or_default()` silently masks a malformed `cbindgen.toml`.
- **Severity:** Low

### M3. `windows = "0.61"` pinned despite TODO saying 0.62 is needed
- **Files / lines:** `Cargo.toml:39`, `TODO.md:13`
- **Issue:** CI is broken on Windows; the fix is documented but not applied.
- **Severity:** High (compounds §3 C1)

### M4. No upper bounds on dep versions
- **Files / lines:** `Cargo.toml:25-36`
- **Issue:** Default `^` semver means a breaking change in sherpa-onnx 1.14,
  tungstenite 0.30, or windows 0.62 silently breaks the build.
- **Severity:** Low

### M5. `crate-type = ["cdylib", "staticlib", "lib"]`
- **Files / lines:** `Cargo.toml:12`
- **Issue:** Building all three unconditionally is unusual. For downstream Rust
  consumers `lib` is fine, but it triples artifact size in release packaging.
- **Severity:** Low

### M6. No `[profile.release]` tuning
- **Files / lines:** `Cargo.toml` (absent)
- **Issue:** No `lto`, `codegen-units = 1`, or `strip = true`. Release binaries
  are larger and slower than necessary for an embedded library.
- **Severity:** Low

### L1. cbindgen.toml has no `[parse]` or `[exports]` section
- **Files / lines:** `cbindgen.toml:1-6`
- **Issue:** Generates the avsynth externs into the same header on all
  platforms (see `include/tts_wrapper.h:272-298`), polluting the Windows/Linux
  header.
- **Severity:** Low

---

## 11. CI (`/.github/workflows/ci.yml`, `publish.yml`)

### ✅ C1. FIXED - `windows-build` job has `continue-on-error: true`
- **Files / lines:** `.github/workflows/ci.yml`
- **Issue:** All Windows SAPI builds are silently allowed to fail. PR merge
  gate appears green even though Windows is completely broken (per §3 C1).
- **Fix:** Previous session set `continue-on-error: false` and added a
  dedicated `lint.yml`/`test.yml` with explicit failures. We're no longer
  masking Windows failures.
- **Severity:** Critical (masks Critical bug)

### H1. `sherpa-onnx` is not exercised by CI at all
- **Files / lines:** `.github/workflows/ci.yml` (entire file)
- **Issue:** No clippy, build, or test step uses `--features sherpaonnx`. Only
  `publish.yml:46-48` runs sherpa clippy, with `continue-on-error: true`. The
  bugs in §1 (C1, C2, H1) are never caught.
- **Status:** Sherpa clippy still has continue-on-error due to upstream
  sherpa-onnx 504 download failures on the GitHub release server (see TODO.md).
  The SherpaOnnx fixes from this session will need a successful sherpa build
  to fully validate.
- **Severity:** High

### H2. `publish.yml` build job has job-level `continue-on-error: true`
- **Files / lines:** `.github/workflows/publish.yml:57`
- **Issue:** Every release artifact is allowed to fail. The release job uses
  `fail_on_unmatched_files: false` (`publish.yml:187`), so a release can ship
  with zero artifacts and CI reports success.
- **Severity:** High

### M1. CI never runs the integration tests under sherpaonnx feature
- **Files / lines:** `.github/workflows/ci.yml:30`
- **Issue:** `cargo test --no-default-features --features system,cloud` skips
  all `#[cfg(feature = "sherpaonnx")]` tests in `tests/integration.rs:67-175`.
- **Severity:** Medium

### M2. macOS CI doesn't validate the avsynth shim
- **Files / lines:** `.github/workflows/ci.yml:41-56`
- **Issue:** Builds `avsynth,cloud` but doesn't run any test that exercises
  the FFI bug in §4 H3 (no test exists).
- **Severity:** Medium

### M3. CI `cargo fmt --all -- --check` runs only on Linux
- **Files / lines:** `.github/workflows/ci.yml:21`
- **Issue:** macOS/Windows don't run fmt check.
- **Severity:** Low

---

## 12. C header (`include/tts_wrapper.h`)

### H1. `tts_get_engines` signature is broken-by-design (see §4 C2)
- **Files / lines:** `include/tts_wrapper.h:216`
- **Issue:** Takes a single pointer with no count, allocator mismatch on free.
- **Severity:** Critical

### M1. AvSynth externs always present in cross-platform header
- **Files / lines:** `include/tts_wrapper.h:272-298`
- **Issue:** Windows/Linux consumers see declarations for symbols that don't
  exist. They'll only fail at link time, but it's confusing.
- **Severity:** Low

### M2. Header does not document the lifetime contract of `tts_get_last_error`
- **Files / lines:** `include/tts_wrapper.h:227-232`
- **Issue:** Says "valid until the next call to any TTS function", but per §4 C3
  this is also wrong because most errors don't surface there at all.
- **Severity:** Low

---

## 13. Tests (`tests/integration.rs`)

### ✅ H1. FIXED - No integration test calls `tts_speak`, `tts_synth_to_bytes`, or any FFI function
- **Files / lines:** `tests/integration_tests.rs`, `tests/ffi_safety.rs`,
  `tests/sherpaonnx_models.rs`
- **Issue:** Tests cover factory creation, types, and estimator math. No test
  actually synthesises audio, exercises the C ABI, validates memory
  free/unfree, or runs cloud SSML/JSON building against a mock server. The
  critical bugs in §1, §2, §4 are invisible to the suite.
- **Fix:** Integration tests now exercise the public FFI surface
  (`tts_create`/`tts_destroy`/`tts_get_engine_count`/`tts_get_voices`/...) with
  null pointers, unknown engines, and successful round-trips. Cloud config
  builders have unit tests for Watson auth, PlayHT headers, Deepgram/Hume body
  shape, Polly being None. SherpaOnnx registry parsing is validated against
  the embedded JSON (count > 100, all model_types have dispatch branches).
- **Status:** Still no mock HTTP server for actual cloud synth — that's a
  larger follow-up.
- **Severity:** High

### M1. `test_create_all_cloud_engines` omits 3 engines
- **Files / lines:** `tests/integration.rs:116-135`
- **Issue:** Missing `watson`, `polly`, `elevenlabs` from the loop. (watson and
  polly have dedicated tests but elevenlabs is uncovered.)
- **Severity:** Low

### ✅ M2. FIXED - No tests for Watson auth header, Polly signing, PlayHT header, ElevenLabs indexing
- **Files / lines:** `src/cloud_engine.rs` (mod tests), `tests/ffi_safety.rs`
- **Issue:** The bugs in §2 C1, C2, C3, H7 are uncaught.
- **Fix:** Unit tests added: `test_watson_auth_header_format`,
  `test_playht_config_has_user_id_header`, `test_deepgram_uses_model_param`,
  `test_hume_voice_is_object_in_extra_body`,
  `test_polly_unsupported_returns_none`, `test_azure_ssml_escapes_voice_name`.
  ElevenLabs alignment indexing covered by `bounds_check_tests`.
- **Severity:** High

### ✅ M3. FIXED - Tests don't validate `merged_models.json` parse completeness
- **Files / lines:** `tests/sherpaonnx_models.rs`
- **Issue:** `load_models()` silently returns an empty HashMap on parse error
  (`sherpaonnx_engine.rs:69-73`). If the JSON schema changes, every test
  passes with 0 models.
- **Fix:** `test_registry_loads_nonzero_models` asserts the JSON parses to
  >100 models, and `test_known_model_ids_are_present` pins a handful of
  well-known ids. `test_every_model_has_supported_type` ensures the dispatch
  table covers every model_type present in the registry.
- **Severity:** Medium

---

## 14. .NET bindings (`bindings/dotnet/TtsClient.cs`)

### H1. `tts_get_engines` and `tts_free_engine_info` are not declared
- **Files / lines:** `bindings/dotnet/TtsClient.cs:9-27`
- **Issue:** The P/Invoke block declares 19 functions but omits
  `tts_get_engines`, `tts_free_engine_info`. C# consumers cannot enumerate
  engines (note: even if they did, §4 C2 makes the function unsafe to use).
- **Severity:** Medium

### H2. `SynthToBytes` uses `len.ToUInt32()` — overflow on >2GB audio
- **Files / lines:** `bindings/dotnet/TtsClient.cs:62`
- **Issue:** `UIntPtr` to `uint` throws on 32-bit overflow. Theoretical but
  worth a checked cast.
- **Severity:** Low

### M1. Callbacks (`AudioCallback`, `BoundaryCallback`) declared but never wired
- **Files / lines:** `bindings/dotnet/TtsClient.cs:30-31`
- **Issue:** No `SetOnAudio` / `SetOnBoundary` wrapper in `TtsClient`. The
  P/Invoke exists (`Native.tts_set_on_audio`) but the class doesn't expose it.
- **Severity:** Medium

### M2. No marshalling for `tts_voice` struct
- **Files / lines:** `bindings/dotnet/TtsClient.cs` (absent)
- **Issue:** No `[StructLayout(LayoutKind.Sequential)]` voice struct, so
  `GetVoices` isn't usable from C# without hand-rolled marshalling.
- **Severity:** Medium

### M3. `Speak`/`SpeakSync` discard the integer return value
- **Files / lines:** `bindings/dotnet/TtsClient.cs:46-50`
- **Issue:** `Native.tts_speak` returns `int` (0/-1) but `TtsClient.Speak`
  returns `void`. Failures are silent. (And per §4 C3, `tts_get_last_error`
  wouldn't help anyway.)
- **Severity:** Medium

---

## 15. Other bindings

### Python (`bindings/python/tts_wrapper.py`)
- **M1.** `list_engines()` returns only the count, not the list (line 157-159).
  No binding for `tts_get_engines` or `tts_free_engine_info`.
- **M2.** `on_audio` / `on_boundary` register a local function as the callback
  (line 146, 153). The CFUNCTYPE wrap is stored on `self`, but the GC lifetime
  is fragile if the user replaces the callback.
- **L1.** No `Voice` parser for `tts_get_voices` — the binding declares
  `tts_get_voices` but `TTSClient` doesn't expose it.

### Swift (`bindings/swift/TtsClient.swift`)
- **H1.** No declarations for `tts_get_voices`, `tts_free_voices`,
  `tts_set_on_audio`, `tts_set_on_boundary`, `tts_synth_to_bytes`,
  `tts_get_last_error`. The Swift wrapper covers ~30% of the API surface.
- **M1.** `synthToBytes` (line 76) calls `rust_tts_wrapper.tts_free_bytes(buf,
  UInt(length))` — but `tts_free_bytes` takes `uintptr_t`. On 64-bit this is
  fine; just worth verifying the marshalling.
- **L1.** The `@_cdecl` re-exports at the top shadow the Rust symbols with Swift
  wrappers that don't validate inputs.

---

## 16. Cross-cutting / project-level

### C1. CI reports green while 3 of 4 target platforms fail
- **Files / lines:** `.github/workflows/ci.yml:60`, `.github/workflows/publish.yml:57, 47, 187`
- **Issue:** Combination of `continue-on-error: true` on Windows builds,
  sherpa-onnx clippy, the publish build matrix, and `fail_on_unmatched_files:
  false`. Every release since these flags were added has shipped untested.
- **Severity:** Critical

### H1. `TODO.md` documents Critical issues but they remain unfixed
- **Files / lines:** `TODO.md:10-29`
- **Issue:** Windows SAPI failure and aarch64-linux failure are tracked but
  blocking. SAPI fix is a one-line Cargo dep bump per TODO.
- **Severity:** High

### H2. No locking discipline documented
- **Files / lines:** Throughout
- **Issue:** Multiple Mutexes on `tts_ctx` (engine, voice_id, rate, pitch,
  volume, last_error, on_audio, on_audio_userdata, on_boundary,
  on_boundary_userdata) with no documented acquisition order. Combined with
  the engine Mutex held during speak (§4 H1) and the per-callback userdata
  race (§4 H2), this is a deadlock/UB minefield.
- **Severity:** High

### M1. README claims "21 engines" but `engine_list()` returns 21 only on Linux
- **Files / lines:** `README.md` (claims) vs `src/factory.rs:55-131`
- **Issue:** On macOS you get 22 (adds avsynth). On Windows you get 22 (adds
  sapi). The README should clarify platform variance.
- **Severity:** Low

### M2. `lib.rs:5` doc comment advertises "191 local models" but only 3 work
- **Files / lines:** `src/lib.rs:5`
- **Issue:** Documentation contradicts runtime behaviour (see §1 C1).
- **Severity:** Medium

### L1. No CHANGELOG, no version pinning guidance
- **Issue:** 0.1.0 with substantial Critical bugs makes the
  `repository = "https://github.com/AACTools/rust-tts-wrapper"` link a
  usability hazard for early adopters.
- **Severity:** Low

---

## Summary by severity

| Severity | Count | Notable items |
|----------|-------|---------------|
| Critical | 12    | SherpaOnnx 188/191 models broken; Watson auth; Polly unsigned; PlayHT wrong header; Windows SAPI doesn't compile; FFI panic-safety; allocator mismatch on `tts_free_engine_info`; stale `tts_get_last_error`; AvSynth non-NUL-terminated strings; CI masks all of this |
| High     | 22    | SherpaOnnx double-rate + no pitch/volume; Azure WS request-id format; Azure WS unsafe slicing; ElevenLabs OOB index; Deepgram/Hume API shapes; SAPI feature not platform-guarded; Mutex held during synthesis; userdata race; missing FFI tests; `windows = "0.61"` not bumped |
| Medium   | 30    | Volume dropped in Azure SSML; voice-list parsers; system engine empty voices; missing FFI functions; EngineInfo API awkward; partial alloc leaks; `Cargo.toml` config gaps |
| Low      | 20    | Hardcoded priorities; dead callback types; duplicated lists; missing CHANGELOG; minor docs |

## Recommended fix order

1. Wrap every `extern "C"` fn in `catch_unwind` and propagate errors via
   `tts_get_last_error` (fixes §4 C1, C3, and most UB surface).
2. Replace `tts_get_engines` with an allocator-consistent API matching
   `tts_get_voices` (§4 C2, §12 H1).
3. Fix SherpaOnnx `model_type` dispatch (§1 C1, C2, H1) — needed for the
   offline story to be real.
4. Fix Watson, Polly, PlayHT, Deepgram, Hume cloud configs (§2 C1, C2, C3,
   H10, H11).
5. Fix AvSynth null-termination (§4 H3).
6. Bump `windows` to 0.62 and fix SAPI CLSID references (§3 C1, §10 M3).
7. Remove `continue-on-error` from CI once the above land (§11 C1, H2).
8. Add real integration tests for FFI memory + at least one cloud mock (§13 H1).
