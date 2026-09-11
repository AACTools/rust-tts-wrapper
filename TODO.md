# Rust FFI Issues TODO

## Critical FFI Safety Issues

### 1. Use-After-Free in Error Message Handling
**Location**: `src/lib.rs:1003-1027`
**Issue**: The `tts_get_last_error` function returns a pointer to a CString that lives only as long as the MutexGuard, creating a dangling pointer risk.
**Fix Required**: Ensure error strings remain valid until explicitly cleared or copied by caller.

### 2. Callback Registration Thread Safety
**Location**: `src/lib.rs:760-773`
**Issue**: Callback registration is not atomic with userdata - if callback is invoked during update, it might use old callback with new userdata or vice versa.
**Fix Required**: Make callback registration atomic with userdata updates.

### 3. String Conversion Panic Points
**Location**: `src/lib.rs:624-638`
**Issue**: Multiple `CString::new(...).unwrap()` calls that will panic if strings contain interior nulls.
**Fix Required**: Replace `.unwrap()` with proper error handling that returns error codes instead of panicking.

### 4. Double-Free Risk in Voice Array Cleanup
**Location**: `src/lib.rs:663-691`
**Issue**: Cleanup function doesn't protect against double-free if called twice with same pointer.
**Fix Required**: Add protection against multiple cleanup calls.

### 5. Memory Allocation Overflow Risk
**Location**: `src/lib.rs:616-646`
**Issue**: Voice array allocation using `std::alloc::Layout::array` could overflow with extremely large (malicious) input.
**Fix Required**: Add bounds checking for array allocations.

## Integration Issues with C++ Code

### 6. Panic Propagation Across FFI Boundary
**Location**: `src/lib.rs:174-178`
**Issue**: Some FFI functions don't use the panic catching macro, causing undefined behavior when panics occur.
**Fix Required**: Ensure all FFI functions use the `ffi_catch!` macro.

## Priority Order
1. Fix panic propagation (safety critical)
2. Fix string conversion panic points (stability)
3. Fix callback registration thread safety (race conditions)
4. Fix use-after-free in error handling (memory safety)
5. Fix double-free risk (memory safety)
6. Add memory allocation overflow checks (input validation)