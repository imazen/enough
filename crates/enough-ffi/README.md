# enough-ffi

FFI helpers for the [`enough`](https://crates.io/crates/enough) cooperative cancellation trait.

[![Crates.io](https://img.shields.io/crates/v/enough-ffi.svg)](https://crates.io/crates/enough-ffi)
[![Documentation](https://docs.rs/enough-ffi/badge.svg)](https://docs.rs/enough-ffi)
[![License](https://img.shields.io/crates/l/enough-ffi.svg)](LICENSE-MIT)

This crate provides C-compatible functions and types for bridging cancellation across language boundaries. Use it to integrate Rust libraries with C#/.NET, Python, Node.js, and other languages that can call C APIs.

## Safety Model

This crate uses Arc-based reference counting internally to prevent use-after-free:

- Sources and tokens share state through `Arc`
- Destroying a source while tokens exist is **safe** - tokens remain valid
- Tokens that outlive their source will never become cancelled (no one can call cancel)
- Each token must be explicitly destroyed when no longer needed

## Quick Start

### C FFI Functions

```c
// Source management
void* enough_cancellation_create(void);
void  enough_cancellation_cancel(void* source);
bool  enough_cancellation_is_cancelled(void* source);
void  enough_cancellation_destroy(void* source);

// Token management
void* enough_token_create(void* source);
void* enough_token_create_never(void);
bool  enough_token_is_cancelled(void* token);
void  enough_token_destroy(void* token);
```

### Rust FFI Functions

When writing Rust FFI functions that receive a token pointer:

```rust
use enough_ffi::FfiCancellationToken;
use enough::Stop;

#[no_mangle]
pub extern "C" fn my_operation(
    data: *const u8,
    len: usize,
    token: *const FfiCancellationToken,
) -> i32 {
    // Create a non-owning view from the pointer
    let stop = unsafe { FfiCancellationToken::from_ptr(token) };

    // Use with any library that accepts impl Stop
    for i in 0..len {
        if i % 100 == 0 && stop.should_stop() {
            return -1; // Cancelled
        }
        // do work...
    }
    0
}
```

### C# Integration

```csharp
public class CancellationHandle : IDisposable
{
    [DllImport("mylib")] static extern IntPtr enough_cancellation_create();
    [DllImport("mylib")] static extern void enough_cancellation_cancel(IntPtr source);
    [DllImport("mylib")] static extern void enough_cancellation_destroy(IntPtr source);
    [DllImport("mylib")] static extern IntPtr enough_token_create(IntPtr source);
    [DllImport("mylib")] static extern void enough_token_destroy(IntPtr token);

    private IntPtr _source, _token;
    private CancellationTokenRegistration _registration;

    public CancellationHandle(CancellationToken ct)
    {
        _source = enough_cancellation_create();
        _token = enough_token_create(_source);
        _registration = ct.Register(() => enough_cancellation_cancel(_source));
    }

    public IntPtr TokenHandle => _token;

    public void Dispose()
    {
        _registration.Dispose();
        enough_token_destroy(_token);
        enough_cancellation_destroy(_source);
    }
}
```

### Node.js Integration

```javascript
import ffi from 'ffi-napi';

const lib = ffi.Library('mylib', {
    'enough_cancellation_create': ['pointer', []],
    'enough_cancellation_cancel': ['void', ['pointer']],
    'enough_cancellation_destroy': ['void', ['pointer']],
    'enough_token_create': ['pointer', ['pointer']],
    'enough_token_destroy': ['void', ['pointer']],
});

function withCancellation(signal, operation) {
    const source = lib.enough_cancellation_create();
    const token = lib.enough_token_create(source);

    const onAbort = () => lib.enough_cancellation_cancel(source);
    signal?.addEventListener('abort', onAbort);

    try {
        return operation(token);
    } finally {
        signal?.removeEventListener('abort', onAbort);
        lib.enough_token_destroy(token);
        lib.enough_cancellation_destroy(source);
    }
}
```

## Types

| Type | Description |
|------|-------------|
| `FfiCancellationSource` | Owns cancellation state, can trigger cancellation |
| `FfiCancellationToken` | Holds reference to state, can check cancellation |
| `FfiCancellationTokenView` | Non-owning view for Rust FFI functions |

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
