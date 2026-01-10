# Node.js FFI Demo for `enough`

This demonstrates how to bridge JavaScript's `AbortController`/`AbortSignal` to Rust's cooperative cancellation system through FFI.

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

1. **Create**: `enough_cancellation_create()` allocates a Rust cancellation source
2. **Bridge**: `AbortSignal.addEventListener('abort', ...)` forwards JS abort to Rust
3. **Check**: Rust code periodically calls `stop.check()` or `stop.is_stopped()`
4. **Cleanup**: `enough_cancellation_destroy()` frees the Rust source

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
#[no_mangle]
pub extern "C" fn process_image(
    data: *const u8,
    len: usize,
    cancel: *const FfiCancellationSource,
) -> i32 {
    let token = unsafe { FfiCancellationToken::from_ptr(cancel) };

    // Use token with any library that accepts impl Stop
    match my_codec::decode(data, len, token) {
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
