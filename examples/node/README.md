# Node.js FFI Demo for `enough`

This demonstrates how to bridge JavaScript's `AbortController`/`AbortSignal` to Rust's cooperative cancellation system through FFI.

## Safety Model

The Rust implementation uses Arc-based reference counting, making it safe even if:
- The JS handle is garbage collected while Rust code is still using the token
- The source is destroyed while tokens exist

A `FinalizationRegistry` is used as a safety net to free native resources if `dispose()` is not called.

## Key Concepts

### 1. Synchronous Usage

```javascript
import { runWithCancellation } from './enough.js';

const result = runWithCancellation(
    (handle) => nativeLib.process_data(data, handle)
);
```

### 2. Async Pattern with AbortSignal

```javascript
import { runWithCancellationAsync } from './enough.js';

const controller = new AbortController();

// Can cancel later with: controller.abort()
const result = await runWithCancellationAsync(
    (handle) => nativeLib.process_data(data, handle),
    controller.signal
);
```

### 3. Timeout Pattern

```javascript
import { runWithTimeout } from './enough.js';

try {
    const result = await runWithTimeout(
        (handle) => nativeLib.slow_operation(handle),
        5000 // 5 second timeout
    );
} catch (e) {
    if (e.name === 'AbortError') {
        console.log('Operation timed out');
    }
}
```

### 4. Manual Handle Management

```javascript
import { CancellationHandle } from './enough.js';

const handle = new CancellationHandle(signal);
try {
    const result = nativeLib.process(handle.handle);
} finally {
    handle.dispose();
}
```

## How It Works

1. **Create Source**: `enough_cancellation_create()` allocates a Rust cancellation source
2. **Create Token**: `enough_token_create(source)` creates a token from the source
3. **Bridge**: `AbortSignal.addEventListener('abort', ...)` forwards JS abort to Rust via `enough_cancellation_cancel(source)`
4. **Check**: Rust code receives the token pointer and calls `stop.is_stopped()` or `stop.check()`
5. **Cleanup**: `enough_token_destroy(token)` then `enough_cancellation_destroy(source)` frees resources

The source and token separation allows safe destruction order - tokens hold Arc references to shared state.

## Installation

```bash
npm install
```

## Building the Rust Library

```bash
cd ../..
cargo build --release -p enough-ffi
```

Set the library path:
```bash
export ENOUGH_LIB_PATH=/path/to/target/release/libenough_ffi
```

## Running the Demo

```bash
npm run demo
# or
node demo.js
```

## Integration with Your Library

Replace the mock library with ffi-napi bindings to your actual Rust library:

```javascript
import ffi from 'ffi-napi';
import ref from 'ref-napi';

const lib = ffi.Library('your_rust_lib', {
    'process_image': ['int', ['pointer', 'size_t', 'pointer']],
});

// Use with enough
const result = runWithCancellation(
    (handle) => lib.process_image(dataPtr, dataLen, handle),
    signal
);
```

Your Rust function would look like:

```rust
use enough_ffi::FfiCancellationToken;
use enough::Stop;

#[no_mangle]
pub extern "C" fn process_image(
    data: *const u8,
    len: usize,
    token: *const FfiCancellationToken,
) -> i32 {
    // Create a non-owning view from the token pointer
    let stop = unsafe { FfiCancellationToken::from_ptr(token) };

    // Use stop with any library that accepts impl Stop
    match my_codec::decode(data, len, stop) {
        Ok(_) => 0,
        Err(e) if e.is_cancelled() => -1,
        Err(_) => -2,
    }
}
```

## Notes

- The `ffi-napi` library requires Node.js native addon build tools
- On Windows, you need Visual Studio Build Tools
- On macOS/Linux, you need gcc/clang
- Consider using N-API directly for production use
