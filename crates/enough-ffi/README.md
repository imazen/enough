# enough-ffi

FFI helpers for the [`enough`](https://crates.io/crates/enough) cooperative cancellation trait.

[![CI](https://github.com/imazen/enough/actions/workflows/ci.yml/badge.svg)](https://github.com/imazen/enough/actions/workflows/ci.yml)
[![Crates.io](https://img.shields.io/crates/v/enough-ffi.svg)](https://crates.io/crates/enough-ffi)
[![Documentation](https://docs.rs/enough-ffi/badge.svg)](https://docs.rs/enough-ffi)
[![codecov](https://codecov.io/gh/imazen/enough/graph/badge.svg)](https://codecov.io/gh/imazen/enough)
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

These eight `enough_*` symbols are exported with `#[unsafe(no_mangle)] extern "C"`, but
this crate has **no `[lib] crate-type`** of its own — it builds as an `rlib`. The symbols
become linkable C entry points only when a **downstream crate that depends on `enough-ffi`
builds a `cdylib` or `staticlib`** (e.g. `crate-type = ["cdylib"]` in that crate's
`Cargo.toml`). Linking `enough-ffi` alone does not produce a `.so`/`.dll`/`.a`.

### Pure-C end-to-end example

Source on one thread, cancellation from another, all through the C ABI. Compile against
the `cdylib` your downstream crate produces (here called `mylib`):

```c
#include <stddef.h>
#include <stdio.h>
#include <pthread.h>
#include <unistd.h>

// Exported from your cdylib (which re-exports enough-ffi's symbols).
extern void* enough_cancellation_create(void);
extern void  enough_cancellation_cancel(void* source);
extern void  enough_cancellation_destroy(void* source);
extern void* enough_token_create(void* source);
extern void  enough_token_destroy(void* token);

// A Rust FFI function in your cdylib that takes the token pointer and polls it
// (it builds an FfiCancellationTokenView internally via from_ptr).
extern int my_operation(const unsigned char* data, size_t len, const void* token);

static void* canceller(void* source) {
    sleep(1);                           // let the worker start polling
    enough_cancellation_cancel(source); // cancel from a different thread — sound
    return NULL;
}

int main(void) {
    void* source = enough_cancellation_create();
    void* token  = enough_token_create(source);

    pthread_t t;
    pthread_create(&t, NULL, canceller, source);

    unsigned char data[100000] = {0};
    int rc = my_operation(data, sizeof data, token); // returns -1 once cancelled
    printf("my_operation -> %d\n", rc);

    pthread_join(t, NULL);
    enough_token_destroy(token);         // destroy token first,
    enough_cancellation_destroy(source); // then source (order is not required — both safe)
    return 0;
}
```

### Rust FFI Functions

When writing Rust FFI functions that receive a token pointer, turn the raw pointer
into a `FfiCancellationTokenView` and use it as `impl Stop`:

```rust
use enough_ffi::{FfiCancellationToken, FfiCancellationTokenView};
use enough::Stop;

// `#[no_mangle]` requires the `unsafe(...)` wrapper on edition 2024.
#[unsafe(no_mangle)]
pub extern "C" fn my_operation(
    data: *const u8,
    len: usize,
    token: *const FfiCancellationToken,
) -> i32 {
    // `from_ptr` is an associated fn on `FfiCancellationToken` that RETURNS a
    // `FfiCancellationTokenView` (a non-owning, Copy view) — it does NOT return a
    // `FfiCancellationToken`. It is `unsafe`: see the contract below.
    let stop: FfiCancellationTokenView = unsafe { FfiCancellationToken::from_ptr(token) };

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

**`from_ptr` signature** (associated fn on `FfiCancellationToken`):

```rust
pub unsafe fn from_ptr(ptr: *const FfiCancellationToken) -> FfiCancellationTokenView
```

- **It returns a `FfiCancellationTokenView`, not a `FfiCancellationToken`.** The view
  is the type listed below as "non-owning view for Rust FFI functions". The owning
  `FfiCancellationToken` (created by `enough_token_create`) is what the C side holds and
  passes by pointer; Rust borrows it as a view.
- **It is `unsafe`** — you assert the safety contract that follows.

#### Null-pointer and lifetime contract

- **`from_ptr(null)` is allowed and never cancels.** A view built from a null pointer
  reports `should_stop() == false` and `check() == Ok(())` — i.e. it behaves like a
  "never cancelled" token (equivalent to `FfiCancellationTokenView::never()`). No
  dereference happens on the null path, so it cannot fault.
- **Lifetime:** if `ptr` is non-null it must point to a valid `FfiCancellationToken`,
  and that token must stay alive (not be passed to `enough_token_destroy`) for as long
  as the view — and any copy of it — is used. The view borrows; it does not bump the
  `Arc` refcount or take ownership.

#### Thread safety

`FfiCancellationTokenView` is `Send + Sync` and the underlying state is an atomic behind
an `Arc`. **It is sound for one thread to call the cancel function while another thread is
running `should_stop()` / `check()` on a view of the same token** — that is exactly the
intended usage (a C callback cancels the source while a Rust worker polls the token). No
external locking is required.

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
| `FfiCancellationToken` | Owns a reference to the state; this is what crosses FFI by pointer. Can check cancellation; provides the `from_ptr` constructor |
| `FfiCancellationTokenView` | Non-owning `Copy` view for Rust FFI functions. **This is the return type of `FfiCancellationToken::from_ptr`** |

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
