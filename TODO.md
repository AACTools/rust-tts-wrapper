# rust-tts-wrapper TODO

## Done this session

- Separated sherpaonnx clippy step with `continue-on-error` so 504s don't block publish workflow
- Created `Dockerfile.cross-aarch64` and `Cross.toml` to attempt aarch64-linux cross builds (got past headers/libclang, hit upstream bug)

## Blocked

### Windows SAPI engine (`sapi` feature)
- **Status**: All 3 Windows builds fail on CI
- **Root cause**: `windows` crate 0.61 API incompatibility — `SPVOICE_CLSID`, `SpEnumTokens`, `SPEAK_FLAGS` not found
- **Fix**: Migrate to `windows` crate 0.62 (or fix API usage for 0.61)
- **Note**: Best done on a Windows machine — SAPI has never been tested locally
- **Files**: `src/sapi_engine.rs`, `Cargo.toml` (bump `windows` dep)

### aarch64-linux cross-compilation
- **Status**: `system-cloud-aarch64-linux` fails on CI
- **Root cause**: Upstream `speech-dispatcher` crate has type mismatches when bindgen runs with newer libclang (e.g., `u32` vs `i32` for `spd_set_voice_type_uid` parameter)
- **Progress so far**: 
  - Headers now copied into cross sysroot via Dockerfile.cross-aarch64
  - Upgraded from libclang 3.8 (xenial image) to clang 10 (`:main` cross image)
  - Bindgen succeeds but compilation of `speech-dispatcher` crate fails due to type mismatches
- **Options**:
  1. Pin libclang version that generates types matching what `speech-dispatcher` expects
  2. Patch/override `speech-dispatcher` crate locally
  3. Wait for upstream fix
  4. Skip `system` feature on aarch64, only ship `cloud` variant
- **Files**: `Dockerfile.cross-aarch64`, `Cross.toml`

## Known transient issues

### sherpa-onnx 504 downloads
- sherpa-onnx GitHub release server intermittently returns 504 for `.tar.bz2` archive downloads
- CI uses `continue-on-error: true` on build matrix so this doesn't block releases
- Re-running the workflow usually succeeds on the next attempt
- Not fixable on our side — their server issue

## Future

- [ ] Test SAPI engine locally on Windows
- [ ] Consider adding retry logic for sherpa-onnx downloads (custom build script wrapper?)
- [ ] Consider publishing to crates.io once Windows/Linux are green
- [ ] Add more integration tests for cloud providers
