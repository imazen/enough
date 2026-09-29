# Wasm host-boundary probe

```sh
cargo build --manifest-path dev/howfar-wasm/Cargo.toml --target wasm32-unknown-unknown
node dev/howfar-wasm/check.mjs
```

Use a JSPI-capable Node (tested with 26.7.0). The test executes actual synchronous
Rust/`howfar` Wasm through an ordinary import, a `WebAssembly.Suspending` import,
and a worker. It verifies that ordinary callbacks do not run queued timer tasks,
JSPI suspends/resumes the Rust call stack and permits cancellation, and worker
callbacks post progress outward.

This probe deliberately uses raw Wasm exports. Its resumable `begin_chunks` /
`run_chunk` exports also supply the fallback used by the
[browser integration tests](../howfar-browser/README.md), which exercise a real
wasm-bindgen-rayon pool in Chromium and WebKit. Neither fixture certifies arbitrary
closure trampolines, Asyncify transforms, or Apple's packaged Safari.
The raw probe has no browser-binding dependencies and is excluded from ordinary
workspace builds. See [integration notes](../../docs/howfar-implementation.md).
