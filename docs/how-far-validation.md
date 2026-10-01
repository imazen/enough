# how-far validation

What is tested, where, and what the tests do not cover. See
[the design notes](how-far-design.md) for why things work the way they do.

## Test matrix

| Scenario | Where |
| --- | --- |
| Counting through every sink shape; original call sites survive forwarding | `crates/how-far/tests/interface.rs` |
| The `Pulse` contract on `NoPulse`: nested plans, validation, handles, `split_array`, outcomes from results | `crates/how-far/tests/pulse.rs` |
| `Paced`: never reaching a no-op pulse, exact counts per interval, stops seen at the next reach, pending units counted after an early return, saturation, call sites | `crates/how-far/tests/paced.rs` |
| A hand-written `Pulse` shared by scoped threads; `'static` threads through handles | `crates/how-far/tests/pulse_threads.rs` |
| `Stages` libraries calling each other: success, nested stop, failure versus cancellation, stage finish errors, abandonment | `crates/how-far/tests/composition.rs`, `crates/how-far-along/tests/pulse.rs` |
| Sizes of everything a caller holds or passes, asserted at compile time | `crates/how-far/tests/footprint.rs`, `crates/how-far-along/tests/footprint.rs` |
| A library crate (codec) and a library that calls it (pipeline), driven by an application, all in separate crates | `tests/test-how-far-app/tests/cross_crate.rs` |
| Scoped fork-join children, spawned `'static` threads, cancellation crossing threads, an observer on another thread, a tree moved into a thread | `tests/test-how-far-app/tests/threads.rs` |
| Rayon: a stage shared by 1–8 workers, nested parallelism, recursive `join`, `scope`, `'static` `spawn`, the global pool | `tests/test-how-far-app/tests/rayon_pools.rs` |
| `'static` ownership: codec contexts that own their stop, `Arc` and `Box` pulses, Tokio `spawn_blocking` with async cancellation, async tasks reporting | `tests/test-how-far-app/tests/statics.rs` |
| Diagnostics across crates, including checks inside a codec context credited to the right stage | `tests/test-how-far-app/tests/diagnose.rs` |
| Application-planned trees: serial → 30% parallel → serial, repeated joins, Rayon and manual threads sharing a counter, totals and revisions, overrun and overflow, every outcome, frozen and abandoned records, a report racing with the finish | `crates/how-far-along/tests/phases.rs`, `crates/how-far-along/src/tree.rs` |
| A codec-style pipeline with two parallel waves, a terminal renderer, and a Tokio request whose client disconnects | `crates/how-far-along/tests/hosts.rs` |
| Pollers: thread-affine callbacks, lazy shared snapshots, busy and recursive dispatch, panics, posted delivery, workers stopped by a callback | `crates/how-far-along/tests/polling.rs` |
| Profiling: per-task gaps, call-site counts, overlap and stragglers, cancellation latency, bounded retention, clock faults, report timing on and off, workers sharing a span | `crates/how-far-along/tests/profiling.rs` |
| Diagnostics: report gaps versus stop gaps, covering spans, stage-weight candidates, negligible stages, callbacks | `crates/how-far-along/tests/diagnostics.rs` |
| Metadata replacement concurrent with snapshots, under Miri with strict provenance | `crates/how-far-along/src/sync.rs`, `crates/how-far-along/tests/phases.rs` |
| A real Wasm timer boundary, JSPI suspension and cancellation, progress posted from a worker | `dev/how-far-wasm/check.mjs` |
| A UI thread observing and cancelling a `wasm-bindgen-rayon` pool in Chromium and WebKit | `dev/how-far-browser/browser.spec.mjs` |
| `enough` on Rust 1.85, `how-far` on 1.86, and `how-far-along` on 1.88; `no_std` builds for Cortex-M and wasm32; every feature combination; i686, aarch64 Linux and Windows, Intel macOS | CI |

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

A library that adopts `how-far` pays twice: once to compile `how-far`, and
again for the generic `how-far` code compiled into the library itself.
`dev/bench-how-far-build.py` measures both and runs in CI. It fails if
`how-far` gains a feature, a build script, or a dependency other than
`enough`, or if one `Stages::run_*` call site adds more than 120 lines of
`how-far`'s unoptimized LLVM IR to the caller's crate (81 with rustc
1.98.1, 91 with 1.88). It also fails if `how-far-along` itself compiles to
more than 18,000 lines of unoptimized IR with default features (15,530 with
1.98.1, 16,722 with 1.88; 19,048 before the reductions below) or 55,000 with
`diagnostics` (43,748 and 45,976; 85,667 before).

With `perf` available, it also counts rustc's instructions, which unlike wall
time do not depend on machine load (the metric
[rustc-perf](https://perf.rust-lang.org/) uses). Measured 2026-10-01 with
rustc 1.98.1, medians of five, in millions:

| rustc instructions | check | debug | release |
| --- | ---: | ---: | ---: |
| An empty `no_std` crate | 14.2 | 16.4 | 19.1 |
| `how-far` | 161.3 | 312.6 | 399.4 |
| `how-far-along`, default features | 315.2 | 846.2 | 1816.9 |
| `how-far-along` with `diagnostics` | 747.3 | 2243.1 | 5986.7 |
| A plain function call, per call site | 4.5 | 8.1 | 45.5 |
| A `Stages::run_stoppable` call site | 5.6 | 12.2 | 59.6 |

The last two rows come from caller crates with 8 and 32 call sites, each
running the same small stage function. On an AMD Ryzen 9 5900XT, rustc takes
70 ms to check `how-far` and 99 ms for a debug build of it, against 34 ms and
33 ms for an empty crate; most of an empty crate's time is process startup.
`how-far` depends only on `enough`, so in a real build it compiles while a
library's larger dependencies are still compiling.

Before `Stages` moved its bookkeeping out of its generic methods, a call site
added 312 lines of IR and cost 16.7 and 70.5 million instructions in debug
and release builds; `how-far` itself cost 161.1, 302.8 and 382.0. The
reasoning is in [the design notes](how-far-design.md#compile-time-cost).

`how-far-along` is what applications and tests compile. Before its analysis
code moved from iterator adapters and per-type sorts to loops and one merge
sort, it cost 322.8, 1004.0 and 2275.1 million instructions with default
features, 566.0, 1825.6 and 4872.4 with `profile`, and 798.6, 3609.2 and
12669.4 with `diagnostics`; with `profile` it now costs 561.8, 1517.6 and
3537.8.

Reproduce with `python3 dev/bench-how-far-build.py --runs 5`.

### In a real library

`dev/bench-how-far-adopter.py` builds
[zenresize](https://github.com/imazen/zenresize) before and after it adopted
`how-far`, against this checkout. On 2026-10-01
([results](../benchmarks/how-far-adopter-2026-10-01.md)):

- A clean build of zenresize's dependency graph took the same time either way
  (7.0 s debug, 6.8 s release). `how-far` finished 0.2 to 0.3 s in, while
  zenresize itself could not start before 4.9 s.
- zenresize's own compile grew by 1.1% to check and under 0.2% to build.
- Its resize methods are generic, so they compile in the caller's crate. A
  caller of the new `Pulse` API compiled 146 million more instructions in
  debug and 209 million more in release than one calling the old `Stop` API.
  About a third of that is `how-far`'s `Stages`; the rest is zenresize's own
  pulse path, mostly a second copy of its row loop made by a helper generic
  over its report sink.

## Runtime cost

`dev/how-far-checkpoint-cost` counts, with perf, what each checkpoint style
and a three-stage plan cost around the same `#[inline(never)]` defilter;
`python3 dev/how-far-checkpoint-cost/measure.py` reruns it. Instruction counts
do not depend on machine load, and cycles depend on it far less than wall time
does. [The 2026-10-01 counts](../benchmarks/how-far-checkpoint-cost-2026-10-01.md)
put a paced step at about 5 instructions and 1 cycle, a plain `step` into a
live tree at about 58 and 16, a step through a stage at about 47
instructions, and a live tree's three-stage plan at about 6,800 instructions
per operation. Wall-time results from zenbench are in
[the overhead results](../benchmarks/how-far-overhead.md).
