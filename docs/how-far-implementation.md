# how-far implementation and validation

## Crate boundary

* [`how-far`](../crates/how-far/README.md) is the library-author interface:
  `&dyn Pulse` combines `enough::Stop`, completed-work reporting, and fixed
  weighted child plans. `Steps` handles success, cancellation, failure, and
  skipped siblings for serial leaf work; primitive `split` remains available
  for parallel and nested branches. `NoPulse`, `Report`, and `IgnoreProgress`
  support the no-op and count-only cases. Its only dependency is `enough`; it has no feature
  flags, timers, or runtime. It always uses no_std + alloc.
  `ProgressExt::step(n)` reports completed work and checks cancellation at the
  same site; frequent `check()` calls remain independent and minimal.
* [`how-far-along`](../crates/how-far-along/README.md) is the opt-in consumer/test
  toolkit: `PulseTree`, `ProgressWithStop`, batching, weighted trees, snapshots, polling,
  and profiling.
  Library tests can use it as a dev-dependency without imposing it on users.
* Both forbid unsafe code at crate level. No macro generates the core interface
  or its forwarding implementations; no proc macros are required.

Both new crates support Rust 1.88; enough/almost-enough remain on Rust 1.85.
Existing cancellation APIs are unchanged. No downstream production codec is
ported here: it must report at its actual completed-work sites.

## Safe synchronization

The previous custom raw-pointer publication primitive has been removed. Metadata
now lives in immutable `Arc` values, with a short platform lock protecting only
handle cloning/replacement. Allocation, tree walks, old-version destruction,
application callbacks, and clock hooks happen outside that lock. Replaced metadata
is reclaimed when its last reader drops; retained total revision history remains
explicit in the current version.

Reporting and cancellation remain atomic. Shared callback dispatch uses a single
nonblocking claim, and busy/reentrant calls return immediately. Normal std tracking
uses standard mutexes for metadata. `Observer::try_snapshot` and
`PollEvent::try_snapshot` let UI callers skip a busy observation and retry next turn;
`snapshot` is the blocking native/worker convenience. No spin fallback is supplied. Native no_std-mode tests exercise the critical-section
backend with its std provider; Miri exercises the standard-mutex backend because
it does not resolve the upstream critical-section provider's foreign symbols.

No-std + alloc retains the same tree/polling API via the small `critical-section`
dependency. Applications provide the HAL/RTOS synchronization implementation and
must ensure it excludes all participating threads/cores. Entry latency is a
property of that provider; its critical section encloses only a handle operation.
See [the upstream provider contract](https://docs.rs/critical-section/1.2.0/critical_section/).
The tracker depends on how-far, enough, and critical-section. Rayon/Tokio and browser
bindings are test dependencies; the interface crate depends only on enough.

This follows the scheduling concern in
[Spinlocks Considered Harmful](https://matklad.github.io/2020/01/02/spinlocks-considered-harmful.html):
a preempted owner must not make UI readers spin. Optional profiling uses standard
mutexes and supports a nonblocking UI snapshot as well.

## The existing Wasm/Rayon deployment

Reviewed local files:

* `~/work/zen/zenpipe/demo/crate/Cargo.toml`: optional `wasm-bindgen-rayon = "1.3"`
  with `no-bundler`, and optional Rayon.
* `~/work/zen/zenpipe/demo/crate/src/lib.rs`: exports `init_thread_pool`.
* `~/work/zen/zenpipe/demo/worker.js`: loads the module inside a Web Worker,
  initializes up to eight Rayon workers when cross-origin isolated, and posts
  results to the UI. The main thread creates that worker through
  `demo/js/worker-client.js`.
* `~/work/zen/zenav1-aom/crates/aom-dsp/src/par.rs`: uses a shared Rayon pool or
  scoped manual threads, collects every result and joins; its worker count is a
  concurrency ceiling rather than a dedicated-core guarantee.

Keep that worker-owned pattern. Share `Progress` among Rayon jobs, and post
snapshots/notifications to the UI through the host's bounded/coalesced transport.
Use separate profiling spans for logical tasks/chunks, not `for_each_init`
instances treated as OS worker IDs. UI progress observation uses `Observer::try_snapshot()` and retries a busy
metadata mutex on a later turn, including when UI and workers share Wasm memory
through correctly initialized bindings. The [wasm-bindgen-rayon documentation](https://github.com/RReverser/wasm-bindgen-rayon)
describes the host requirements: shared memory/cross-origin isolation, worker-pool
initialization, and rebuilding the standard library with Wasm atomics enabled.
The crate does not configure that application build on the consumer's behalf.

`wasm_safe_mutex` was reviewed but not added. Its synchronous main-thread fallback
spins when blocking is unavailable, and its Wasm build adds binding/runtime
dependencies. That behavior does not provide an event-loop yield. See its
[README](https://github.com/drewcrawford/wasm_safe_mutex) and
[manifest](https://github.com/drewcrawford/wasm_safe_mutex/blob/main/Cargo.toml).

## Main-thread and worker responsiveness

There are three host strategies:

1. Keep CPU work in workers; post progress outward. An incoming cancel message
   cannot interrupt a worker's synchronous Rust loop. Use resumable chunks,
   suspension, or an application-provided shared cancellation flag. Terminating
   the worker is a hard abort and does not run Rust cleanup.
2. Use a resumable algorithm and honor `Control::Yield` by returning/awaiting at
   a safe boundary. A 10 ms budget is checked at those boundaries; it cannot
   cap an indivisible chunk that itself takes longer.
3. Keep synchronous Rust source and provide stack suspension at the actual Wasm
   import/export boundary using JSPI or an Asyncify build.

JSPI's basic host shape is:

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

The callback must reach a suspension-enabled import, and the outer call must use
the actual Wasm export. Ordinary intervening JavaScript frames can prevent
suspension; an arbitrary wasm-bindgen closure trampoline is not automatically
compatible. Avoid reentering a mutably borrowed encoder while its invocation is
suspended, and do not hold application locks across a host yield. See the
[JSPI proposal](https://github.com/WebAssembly/js-promise-integration/blob/main/proposals/js-promise-integration/Overview.md).

Safari 27 added JSPI according to the [WebKit release notes](https://webkit.org/blog/18325/webkit-features-for-safari-27-0/#webassembly).
Feature-detect `WebAssembly.Suspending` and `WebAssembly.promising`. For older
Safari, use workers/resumable chunks or an explicit
[Asyncify transform and compatible host glue](https://emscripten.org/docs/porting/asyncify.html).
An ordinary callback scheduling `setTimeout` and immediately returning to the
same Rust loop does not yield. Neither does an already-resolved promise provide
a task-level scheduling boundary.

The standalone [Wasm probe](../dev/how-far-wasm/README.md) executes actual how-far
callbacks in Wasm under Node 26.7.0. It validates ordinary timer behavior,
JSPI suspension/resumption/cancellation, and worker-posted progress.

The Wasm target supports all of `core`/`alloc` and a subset of `std`, as described
in the [Rust target documentation](https://doc.rust-lang.org/rustc/platform-support/wasm32-unknown-unknown.html).
This tracker uses the supported collections, atomics, and mutex operations. It
does not spawn Rust OS threads or require filesystem/network APIs. Browser
profiling supplies a JavaScript `performance.now()` clock instead of `StdClock`;
the fixture records on one owner Worker and reads traces nonblockingly on the UI.

The [browser fixture](../dev/how-far-browser/README.md) additionally runs in
Chromium and Playwright WebKit, using zenpipe's wasm-bindgen 0.2.123 and
wasm-bindgen-rayon 1.3 versions. A worker owns the Rayon pool; a second binding
context on the UI thread observes the same Rust progress tree and requests
cancellation through shared memory. The tests cover success, cancellation during
parallel work, joins, terminal snapshots, UI timer/DOM activity, and main-thread
yielding through native JSPI or resumable chunks when JSPI is unavailable.
Playwright WebKit is not Apple's packaged Safari; Asyncify builds and arbitrary
wasm-bindgen callback trampolines remain outside this fixture's scope.

## Test matrix

| Scenario | Automated evidence |
| --- | --- |
| Featureless interface, ignored/borrowed/owned reporting, original call sites | `crates/how-far/tests/interface.rs` |
| No-op generic paths and existing Stop forwarding | `crates/how-far-along/tests/core.rs`, `crates/how-far-along/tests/polling.rs` |
| Stop-only checks leave counts alone; report-and-check preserves completed units while observing cancellation | `crates/how-far/tests/pulse_threads.rs`, `crates/how-far-along/tests/core.rs` |
| Sequential `Steps` success, cancellation, failure, skipped siblings, and unfinished parent | `crates/how-far/tests/pulse.rs`, `crates/how-far-along/tests/pulse.rs` |
| Strided completed work, empty input, partial final batch, overflow | `crates/how-far-along/tests/core.rs`, `crates/how-far-along/tests/phases.rs` |
| Serial → middle 30% parallel → join → serial, nested/repeated joins | `crates/how-far-along/tests/phases.rs` |
| Manual threads and asymmetric Rayon work sharing one counter | `crates/how-far-along/tests/phases.rs` |
| Codec-style geometry, strided preparation, two parallel waves, serial filter, cancellation and joined output | `crates/how-far-along/tests/hosts.rs` |
| CLI terminal output and Tokio client disconnect cancelling/joining blocking CPU work | `crates/how-far-along/tests/hosts.rs` |
| Unknown/estimated/exact/zero totals, revisions, overrun, skip/fail/cancel | `crates/how-far-along/tests/phases.rs` |
| Frozen terminal records, abandoned parents, stale handles and new attempts | `crates/how-far-along/tests/phases.rs` |
| Metadata replacement concurrent with observations | `crates/how-far-along/tests/phases.rs`, strict-provenance Miri |
| Busy child metadata skips a UI snapshot without blocking reports or cancellation | `crates/how-far-along/src/tree.rs` |
| Thread-affine FnMut, arbitrary callback work, memoized/deferred snapshots | `crates/how-far-along/tests/polling.rs` |
| Cancellation during busy dispatch, recursion, panic recovery, posted delivery | `crates/how-far-along/tests/polling.rs` |
| StopToken/Option/reference/Arc/builder compatibility and same-check cancellation | `crates/how-far-along/tests/polling.rs` |
| Per-task entry/exit gaps, storms, original call sites, callback cost | `crates/how-far-along/tests/profiling.rs` |
| Straggler overlap, nested spans, queue/join/yield/callback classification | `crates/how-far-along/tests/profiling.rs` |
| Cancellation request → observation → join/cleanup return | `crates/how-far-along/tests/profiling.rs` |
| Bounded retention, abandoned spans, counter/clock diagnostics, JSON escaping | `crates/how-far-along/tests/profiling.rs` |
| Opt-in report-gap source pairs, stop/report rates, 10 ms callback duration and cadence, stage-weight candidates, and a wrapped library `&dyn Pulse` | `crates/how-far-along/tests/diagnostics.rs` |
| Actual Wasm timer boundary, JSPI yield and cancel, worker posts | `dev/how-far-wasm/check.mjs` |
| Real browser UI observations/cancellation during wasm-bindgen-rayon work; native JSPI/chunk fallback | `dev/how-far-browser/browser.spec.mjs` (Chromium + WebKit) |
| Rust 1.88, fixed no_std+alloc interface, tracker backends, Cortex-M and wasm32 | CI feature/MSRV/target jobs |

Test scopes are explicit; no test suite proves every possible consumer behavior.
There is no built-in ETA/model fitter or executor. Exported observations support
those consumers without claiming durations are CPU time or fractions are runtime.

## Cold compilation and reporting cost

Fresh-target default library builds on the local Linux host (rustc 1.98.1), three
runs each with warm toolchain/filesystem caches:

| Crate | Median | Samples (seconds) |
| --- | ---: | --- |
| enough | 0.098 s | 0.098, 0.097, 0.098 |
| how-far interface (including enough) | 0.178 s | 0.178, 0.176, 0.179 |

The former combined progress crate took 0.287 s by the same measurement method
(three-run median). The interface adds about 80 ms to the fresh `enough` build
without compiling the consumer toolkit. These include process startup; they are host
observations, not timing guarantees on arbitrary CI runners.

Reproduce with `python3 dev/bench-how-far-build.py`. The script alternates crate
order and uses a new Cargo target directory for every sample. Normal consumer
dependency graphs can be inspected with `cargo tree -p how-far --edges normal`.

`cargo bench -p how-far-along --bench overhead` measures the opt-in adapter path,
including atomic reporting, worker-local batching, and clock reads. There is no
hidden debouncing or clock read in ordinary `Report::advance`; the separate
`diagnostics` feature deliberately reads a clock at instrumented report sites.
