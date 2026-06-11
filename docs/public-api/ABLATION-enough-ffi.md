# ABLATION-enough-ffi — Conservative Public-API Review

**Date:** 2026-06-10
**Snapshot commit:** b06dcb303046 (main@origin)
**Snapshot file:** docs/public-api/enough-ffi.txt (54 default / 54 all-features items; identical)
**Grep template:** `grep -rn "<SYMBOL>" /home/lilith/work/ --include="*.rs" 2>/dev/null | grep -v "/enough/" | grep -v "target/" | grep -v ".jj/" | grep -v "zen-arm-src/"`

## Summary

**0 items flagged.** The FFI surface is a minimal C ABI for cancellation tokens: two `#[repr(C)]` structs (`FfiCancellationSource`, `FfiCancellationToken`), a borrowed view type (`FfiCancellationTokenView`), and 8 `#[no_mangle]` extern C functions (`enough_cancellation_create/cancel/destroy/is_cancelled`, `enough_token_create/create_never/destroy/is_cancelled`). No internals leak; every item is a necessary part of the C API contract.

`FfiCancellationTokenView` is the Rust-side borrowed view returned by `FfiCancellationToken::from_ptr` — needed for callers that hold a raw pointer to a token without taking ownership. Correct design.

No external Rust consumer hits found in the workspace (FFI clients are likely C/C++ consumers not visible in the `.rs` scan, or this is a planned surface not yet wired in).

## Flagged Items

None.

## Digest

- Snapshot: 54 items (feature-invariant)
- Flagged A: 0
- Flagged B: 0
- 0% of surface flagged
- Clean C ABI surface; every exported symbol is intentional
