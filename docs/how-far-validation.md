# how-far validation

What is tested, where, and what the tests do not cover. See
[the design notes](how-far-design.md) for why things work the way they do.

## Test matrix

| Scenario | Where |
| --- | --- |
| Counting through every sink shape; original call sites survive forwarding | `crates/how-far/tests/interface.rs` |
| The `Pulse` contract on `NoPulse`: nested plans, validation, handles, `split_array`, outcomes from results | `crates/how-far/tests/pulse.rs` |
| A hand-written `Pulse` shared by scoped threads; `'static` threads through handles | `crates/how-far/tests/pulse_threads.rs` |
| `Stages` libraries calling each other: success, nested stop, failure versus cancellation, stage finish errors, abandonment | `crates/how-far/tests/composition.rs`, `crates/how-far-along/tests/pulse.rs` |
| Sizes of everything a caller holds or passes, asserted at compile time | `crates/how-far/tests/footprint.rs`, `crates/how-far-along/tests/footprint.rs` |
| A library crate (codec) and a library that calls it (pipeline), driven by an application, all in separate crates | `tests/test-how-far-app/tests/cross_crate.rs` |
| Scoped fork-join children, spawned `'static` threads, cancellation crossing threads, an observer on another thread, a tree moved into a thread | `tests/test-how-far-app/tests/threads.rs` |
| Rayon: a stage shared by 1–8 workers, nested parallelism, recursive `join`, `scope`, `'static` `spawn`, the global pool | `tests/test-how-far-app/tests/rayon_pools.rs` |
| `'static` ownership: codec contexts that own their stop, `Arc` and `Box` pulses, Tokio `spawn_blocking` with async cancellation, async tasks reporting | `tests/test-how-far-app/tests/statics.rs` |
| Diagnostics across crates, including checks inside a codec context credited to the right stage | `tests/test-how-far-app/tests/diagnose.rs` |
| Application-planned trees: serial → 30% parallel → serial, repeated joins, Rayon and manual threads sharing a counter, totals and revisions, overrun and overflow, frozen and abandoned records | `crates/how-far-along/tests/phases.rs` |
| A codec-style pipeline with two parallel waves, a terminal renderer, and a Tokio request whose client disconnects | `crates/how-far-along/tests/hosts.rs` |
| Pollers: thread-affine callbacks, lazy shared snapshots, busy and recursive dispatch, panics, posted delivery, workers stopped by a callback | `crates/how-far-along/tests/polling.rs` |
| Profiling: per-task gaps, call-site counts, overlap and stragglers, cancellation latency, bounded retention, clock faults, report timing on and off, workers sharing a span | `crates/how-far-along/tests/profiling.rs` |
| Diagnostics: report gaps versus stop gaps, covering spans, stage-weight candidates, negligible stages, callbacks | `crates/how-far-along/tests/diagnostics.rs` |
| Metadata replacement concurrent with snapshots, under Miri with strict provenance | `crates/how-far-along/src/sync.rs`, `crates/how-far-along/tests/phases.rs` |
| A real Wasm timer boundary, JSPI suspension and cancellation, progress posted from a worker | `dev/how-far-wasm/check.mjs` |
| A UI thread observing and cancelling a `wasm-bindgen-rayon` pool in Chromium and WebKit | `dev/how-far-browser/browser.spec.mjs` |
| `how-far` on Rust 1.85 and `how-far-along` on 1.88; `no_std` builds for Cortex-M and wasm32; every feature combination; i686, aarch64 Linux and Windows, Intel macOS | CI |

No test suite proves every consumer's behavior. There is no built-in ETA
model or executor; exported observations support them without claiming that
durations are CPU time or that fractions are elapsed time.

## Browsers and Wasm

The browser's main thread cannot block, and a worker running a synchronous
Rust loop cannot receive a message until the loop returns. There are three
ways to stay responsive:

1. **Keep CPU work in workers** and read progress from the UI thread with
   `Observer::try_snapshot`, retrying a busy read on the next frame. A
   cancel request reaches the worker's loop through shared memory: a stop
   policy such as `almost_enough::Stopper`, flipped from the UI thread.
   Terminating a worker is a hard abort that runs no Rust cleanup.
2. **Return to the host at safe boundaries.** A resumable algorithm runs a
   bounded chunk, returns, and is called again on the next turn. A time budget
   is checked only at those boundaries, so it cannot shorten one indivisible
   chunk.
3. **Suspend the stack** at a Wasm import with JSPI, or with an Asyncify
   build, and keep synchronous Rust source.

JSPI's host side looks like this:

```js
const imports = {
  host: {
    checkpoint: new WebAssembly.Suspending(async (completed) => {
      reportProgress(completed);
      if (performance.now() >= deadline) {
        await new Promise(resolve => setTimeout(resolve, 0));
        deadline = performance.now() + 10;
      }
      return cancelled ? 1 : 0;
    }),
  },
};
const { instance } = await WebAssembly.instantiate(bytes, imports);
const completed = await WebAssembly.promising(instance.exports.run)(units);
```

The callback must reach a suspending import, and the outer call must go
through the Wasm export itself; ordinary JavaScript frames in between, such as
an arbitrary wasm-bindgen closure trampoline, can prevent suspension. Do not
re-enter a mutably borrowed encoder while it is suspended, and do not hold
application locks across a suspension. See the
[JSPI proposal](https://github.com/WebAssembly/js-promise-integration/blob/main/proposals/js-promise-integration/Overview.md).
Safari 27 added JSPI according to the
[WebKit release notes](https://webkit.org/blog/18325/webkit-features-for-safari-27-0/#webassembly);
feature-detect `WebAssembly.Suspending` and `WebAssembly.promising`, and fall
back to workers, chunks, or
[Asyncify](https://emscripten.org/docs/porting/asyncify.html). A callback that
schedules a timer and returns to the same Rust loop does not yield, and
neither does an already-resolved promise.

The [Wasm probe](../dev/how-far-wasm/README.md) runs real callbacks in Wasm
under Node with JSPI, and checks the timer boundary, suspension and
cancellation, and progress posted from a worker. The
[browser fixture](../dev/how-far-browser/README.md) runs a
`wasm-bindgen-rayon` pool in a worker, in Chromium and Playwright's WebKit.
A second binding context on the UI thread reads the same Rust tree and
cancels through shared memory. Threaded Wasm needs shared memory, cross-origin
isolation, a worker-pool initializer, and a standard library rebuilt with
atomics; see [wasm-bindgen-rayon](https://github.com/RReverser/wasm-bindgen-rayon).
The crates do not configure that build for you. Playwright's WebKit is not
Apple's Safari, and Asyncify builds are not covered.

The profiler needs `std` but no OS threads. On the web, supply a
`performance.now()` clock instead of `StdClock`, record on a worker, and read
traces from the UI with `Profiler::try_snapshot`.

## Build cost

A library that adopts `how-far` adds one small crate to its build. Fresh
target directory, default features, warm toolchain and file caches, five runs
each, measured 2026-10-01 on an AMD Ryzen 9 5900XT with rustc 1.98.1:

| Crate | Median | Runs (seconds) |
| --- | ---: | --- |
| `enough` | 0.125 s | 0.126, 0.122, 0.123, 0.125, 0.127 |
| `how-far` (including `enough`) | 0.217 s | 0.223, 0.217, 0.217, 0.214, 0.211 |

So `how-far` adds about 90 ms to a cold build, process startup included. These
are one machine's numbers, not guarantees. Reproduce with
`python3 dev/bench-how-far-build.py --runs 5`, which alternates crate order,
uses a new target directory per sample, and first checks that `how-far` has no
features and depends only on `enough`.

Runtime cost is in [the overhead results](../benchmarks/how-far-overhead.md).
