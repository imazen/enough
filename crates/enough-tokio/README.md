# enough-tokio

Tokio integration for the [`enough`](https://crates.io/crates/enough) cooperative cancellation trait.

[![Crates.io](https://img.shields.io/crates/v/enough-tokio.svg)](https://crates.io/crates/enough-tokio)
[![Documentation](https://docs.rs/enough-tokio/badge.svg)](https://docs.rs/enough-tokio)
[![License](https://img.shields.io/crates/l/enough-tokio.svg)](LICENSE-MIT)

This crate bridges tokio's `CancellationToken` with the `Stop` trait, allowing you to use tokio's cancellation system with any library that accepts `impl Stop`.

## Use Cases

- **spawn_blocking with cancellation**: Pass cancellation into CPU-intensive sync code
- **Unified cancellation**: Use the same `Stop` trait across async and sync code
- **Library integration**: Use tokio cancellation with codecs, parsers, and other `impl Stop` libraries

## Quick Start

```rust
use enough_tokio::TokioStop;
use enough::Stop;
use tokio_util::sync::CancellationToken;

#[tokio::main]
async fn main() {
    let token = CancellationToken::new();
    let stop = TokioStop::new(token.clone());

    // Use in spawn_blocking for CPU-intensive work
    let handle = tokio::task::spawn_blocking({
        let stop = stop.clone();
        move || {
            for i in 0..1_000_000 {
                if i % 1000 == 0 && stop.should_stop() {
                    return Err("cancelled");
                }
                // do work...
            }
            Ok("completed")
        }
    });

    // Cancel from async context
    tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    token.cancel();

    let result = handle.await.unwrap();
    println!("Result: {:?}", result);
}
```

## Features

### Wrapping CancellationToken

```rust
use enough_tokio::TokioStop;
use tokio_util::sync::CancellationToken;

let token = CancellationToken::new();
let stop = TokioStop::new(token.clone());

// Check cancellation
assert!(!stop.should_stop());

// Cancel
token.cancel();
assert!(stop.should_stop());
```

### Extension Trait

```rust
use enough_tokio::CancellationTokenExt;
use tokio_util::sync::CancellationToken;

let token = CancellationToken::new();
let stop = token.as_stop();  // Creates TokioStop
```

### Async Waiting

```rust
use enough_tokio::TokioStop;
use tokio_util::sync::CancellationToken;

async fn wait_for_cancellation(stop: TokioStop) {
    // Wait until cancelled
    stop.cancelled().await;
    println!("Cancelled!");
}
```

### Child Tokens

```rust
use enough_tokio::TokioStop;
use tokio_util::sync::CancellationToken;

let parent = TokioStop::new(CancellationToken::new());
let child = parent.child();

// Child is cancelled when parent is cancelled
parent.cancel();
assert!(child.should_stop());
```

### Use with `tokio::select!`

```rust
use enough_tokio::TokioStop;
use tokio_util::sync::CancellationToken;

async fn do_work_with_cancellation(stop: TokioStop) -> Result<(), &'static str> {
    tokio::select! {
        _ = stop.cancelled() => Err("cancelled"),
        result = async_work() => Ok(result),
    }
}

async fn async_work() {
    tokio::time::sleep(std::time::Duration::from_secs(1)).await;
}
```

## Integration with Libraries

Any library that accepts `impl Stop` works seamlessly:

```rust
use enough_tokio::TokioStop;
use enough::Stop;
use tokio_util::sync::CancellationToken;

// Example library function
fn process_data(data: &[u8], stop: impl Stop) -> Result<Vec<u8>, &'static str> {
    let mut output = Vec::new();
    for (i, chunk) in data.chunks(1024).enumerate() {
        if i % 16 == 0 && stop.should_stop() {
            return Err("cancelled");
        }
        output.extend_from_slice(chunk);
    }
    Ok(output)
}

#[tokio::main]
async fn main() {
    let token = CancellationToken::new();
    let stop = TokioStop::new(token.clone());

    let data = vec![0u8; 100_000];

    let handle = tokio::task::spawn_blocking(move || {
        process_data(&data, stop)
    });

    // Cancel after a short delay
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_micros(100)).await;
        token.cancel();
    });

    let result = handle.await.unwrap();
    println!("Result: {:?}", result);
}
```

## API Reference

### `TokioStop`

| Method | Description |
|--------|-------------|
| `new(token)` | Create from `CancellationToken` |
| `token()` | Get reference to underlying token |
| `into_token()` | Consume and return underlying token |
| `cancelled()` | Async wait for cancellation |
| `child()` | Create a child `TokioStop` |
| `cancel()` | Trigger cancellation |
| `should_stop()` | Check if cancelled (from `Stop` trait) |
| `check()` | Check with `Result` return (from `Stop` trait) |

### `CancellationTokenExt`

Extension trait for `CancellationToken`:

| Method | Description |
|--------|-------------|
| `as_stop()` | Convert to `TokioStop` |

## Conversions

```rust
use enough_tokio::TokioStop;
use tokio_util::sync::CancellationToken;

let token = CancellationToken::new();

// From CancellationToken
let stop: TokioStop = token.clone().into();

// Back to CancellationToken
let token2: CancellationToken = stop.into();
```

## Thread Safety

`TokioStop` is `Send + Sync` and can be safely shared across threads and tasks.

## License

Licensed under either of Apache License, Version 2.0 or MIT license at your option.
