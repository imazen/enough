# C# FFI Demo for `enough`

This demonstrates how to bridge .NET's `CancellationToken` to Rust's cooperative cancellation system through FFI.

## Key Concepts

### 1. Synchronous Usage

```csharp
using var handle = new CancellationHandle(cancellationToken);
var result = NativeLib.process_data(data, handle.Handle);
```

### 2. Async-Async Pattern

When you have async C# code that needs to call blocking Rust code:

```csharp
var result = await CancellationExtensions.RunWithCancellationAsync(
    handle => NativeLib.process_data(data, handle),
    cancellationToken);
```

This:
- Runs the blocking Rust code on a thread pool
- Bridges the .NET CancellationToken to Rust
- Throws OperationCanceledException if cancelled

### 3. Timeout Pattern

```csharp
using var cts = new CancellationTokenSource(TimeSpan.FromSeconds(5));
try
{
    var result = await CancellationExtensions.RunWithCancellationAsync(
        handle => NativeLib.slow_operation(handle),
        cts.Token);
}
catch (OperationCanceledException)
{
    Console.WriteLine("Operation timed out");
}
```

## How It Works

1. **Create**: `enough_cancellation_create()` allocates a Rust cancellation source
2. **Bridge**: `CancellationToken.Register()` forwards .NET cancellation to Rust
3. **Check**: Rust code periodically calls `stop.check()` or `stop.is_stopped()`
4. **Cleanup**: `enough_cancellation_destroy()` frees the Rust source

## Building

```bash
# Build the Rust library first
cd ../..
cargo build --release -p enough-ffi

# Then build the C# demo
dotnet build
dotnet run
```

## Integration with Your Library

Replace `MockRustLibrary` with P/Invoke to your actual Rust FFI:

```csharp
[DllImport("your_rust_lib")]
public static extern int process_image(
    byte[] data,
    int length,
    IntPtr cancellationHandle,
    out IntPtr result);
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
