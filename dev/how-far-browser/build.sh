#!/usr/bin/env bash
set -euo pipefail
cd "$(dirname "$0")"

# Rebuild std with atomic shared-memory support, as zenpipe/demo does.
# This nightly is only for the browser fixture; how-far's MSRV remains stable 1.88.
export RUSTFLAGS='-C target-feature=+atomics,+bulk-memory,+mutable-globals -C link-arg=--shared-memory -C link-arg=--import-memory -C link-arg=--max-memory=1073741824 -C link-arg=--export=__wasm_init_tls -C link-arg=--export=__tls_size -C link-arg=--export=__tls_align -C link-arg=--export=__tls_base -C link-arg=--export=__heap_base'
cargo +nightly-2026-09-02 build --locked --release --target wasm32-unknown-unknown -Z build-std=std,panic_abort
"${HOW_FAR_BINDGEN:-wasm-bindgen}" --target web --out-dir pkg target/wasm32-unknown-unknown/release/how_far_browser_probe.wasm
