# Browser integration tests

This isolated application tests `howfar` with zenpipe's binding versions:
`wasm-bindgen 0.2.123` and `wasm-bindgen-rayon 1.3`, using `no-bundler`, a
worker-owned Rayon pool and shared Wasm memory. It is excluded from ordinary
workspace/library builds; none of its browser dependencies become dependencies
of either library. The fixture opts into `howfar-tracker` with std.

```sh
rustup toolchain install nightly-2026-09-02 --component rust-src
rustup target add wasm32-unknown-unknown --toolchain stable
cargo +stable install wasm-bindgen-cli --version 0.2.123 --locked
bash dev/howfar-browser/build.sh
cargo +stable build --manifest-path dev/howfar-wasm/Cargo.toml --target wasm32-unknown-unknown
cd dev/howfar-browser
npm ci
npx playwright install --with-deps chromium webkit
npm test
```

Override `HOWFAR_BINDGEN` if the matching CLI is installed outside PATH. The
nightly/build-std requirement belongs to this threaded Wasm application;
`howfar` and `howfar-tracker` support stable Rust 1.88, and `enough` still supports Rust 1.85.

The HTTP server supplies COOP/COEP headers. Tests run in Chromium and Playwright
WebKit and verify:

* Serial preparation, a four-worker Rayon phase, join, and serial output.
* Nonblocking UI-thread snapshots while workers report, retrying busy reads on
  a later event-loop turn, including actual DOM updates.
* UI-thread cancellation through shared Rust control while the owner Worker is
  inside a synchronous encode-like loop; no cancel message handler is required.
* Stable terminal counts after every worker has joined.
* Std-backed profiler state on a Worker with a JavaScript clock, and nonblocking
  trace observations on the UI.
* Main-thread event-loop turns and cancellation using JSPI where available, and
  a resumable chunk adapter on engines without native stack suspension.

The WebKit tests exercise a WebKit engine, not Apple's packaged Safari browser.
The compute loop models a codec's asymmetric scheduling; it does not compile or
port a production codec. Asyncify transforms and arbitrary wasm-bindgen closure
trampolines are not covered. The JSPI test uses the raw Wasm import/export probe
so its suspension boundary is explicit.
