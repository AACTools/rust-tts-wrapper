# Dev stubs

Minimal C stubs of the optional external runtime libraries, for local
development and testing of engine features without building (or
downloading) the real C++ libraries. CI's `offline-engines` jobs build
the **real** runtimes — the stubs exist for quick local iteration
(clippy/test without a cmake build).

## omnivoice_stub.c — omnivoice.cpp (ABI v4)

Implements the `ov_*` symbols the `omnivoice-local` feature links
against, with a language table that mirrors the **real** runtime's
shape (verified against `omnivoice.cpp` `src/lang-map.h`):

- ids are bare ISO 639-3 codes — no regional ids (`pt-br`/`zh-cn` do
  not exist), no macrolanguage `ar` (only `arb`), so the GH #64
  regression tests exercise reality;
- `ov_language_name` takes the same **index** as `ov_language_id`
  (the mismatched-pointer variant of this call was a real UB bug).

Synthesis returns ~1 s of silence; `ov_synthesize` answers
`unknown language '<lang>'` for anything outside the table (drives the
auto-detect fallback test).

```bash
gcc -shared -fPIC -o build/libomnivoice.so omnivoice_stub.c
cd ../../..   # repo root
OMNIVOICE_LIB=$PWD/scripts/dev-stubs cargo test --no-default-features \
  --features omnivoice-local,cloud --lib
# Engine-level (ov_init) tests: any path works against the stub
OMNIVOICE_TEST_MODEL=/dev/null OMNIVOICE_TEST_CODEC=/dev/null \
OMNIVOICE_LIB=$PWD/scripts/dev-stubs cargo test --no-default-features \
  --features omnivoice-local,cloud --lib gh64
```

## qwen3tts_stub.c — qwen3-tts.cpp (`libqwen3tts`)

Link-only stub: every `qwen3_tts_*` symbol exists, all calls fail
softly (enables `cargo check`/`clippy`/`--lib` tests under the
`qwen3-local` feature without the GGML build).

```bash
gcc -shared -fPIC -o build/libqwen3tts.so qwen3tts_stub.c
QWEN3_TTS_LIB=$PWD/scripts/dev-stubs cargo check --features qwen3-local
```

For integration tests against the **real** runtimes, build them
(omnivoice.cpp: `-DOMNIVOICE_SHARED=ON`; qwen3-tts.cpp:
`scripts/build-qwen3-local.sh`) and point `OMNIVOICE_LIB` /
`QWEN3_TTS_LIB` (and `OMNIVOICE_TEST_MODEL`/`OMNIVOICE_TEST_CODEC`
GGUFs for the fallback tests) at them — see `VOICE_CLONING_PLAN.md`.
