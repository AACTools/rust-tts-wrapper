# rust-tts-wrapper TODO

## Done this session

- Separated sherpaonnx clippy step with `continue-on-error` so 504s don't block publish workflow
- Created `Dockerfile.cross-aarch64` and `Cross.toml` to attempt aarch64-linux cross builds (got past headers/libclang, hit upstream bug)

## Blocked

### Windows SAPI engine (`sapi` feature) ✅ FIXED
- **Status**: API issues resolved, toolchain issue remains (dlltool.exe not found)
- **Root cause**: `windows` crate 0.61 API incompatibility — bare `SpVoice` & `SpObjectTokenCategory` constants don't exist
- **Fix applied**: Updated to `windows` crate 0.62, fixed CLSID references (`SPVOICE_CLSID`, `SPCATTOKENCATEGORY_CLSID`)
- **Remaining issue**: MinGW toolchain needs dlltool.exe (workaround: use MSVC toolchain or install MinGW-w64)
- **Files**: `src/sapi_engine.rs:30,37`, `Cargo.toml:39`

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
