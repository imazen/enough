# howfar implementation and validation

The crate is in `crates/howfar`; start with its [README](../crates/howfar/README.md).
This implements the revised design with a deliberately small core: `Report`,
`NoProgress`, and `Work<Stop, Report>`. Extensions add `step` and worker-local
batching. Trees, polling, and profiling are separate layers. Existing `enough`
and `almost-enough` APIs are unchanged. Downstream codecs were reviewed but have
not been ported in this change; reporting still requires an explicit seam at
their completed-work sites.

## Decisions after the synchronization feedback

There are **no new runtime dependencies**. Even with every feature enabled,
`cargo tree -p howfar --edges normal --all-features` contains only `howfar → enough`.
Rayon, Tokio and almost-enough are development dependencies for host/compatibility
tests. Browser binding and Playwright dependencies live in an excluded dev fixture.

Counters, cancellation, yield flags, and dispatch claims use `core::sync::atomic`.
Phase metadata uses immutable publication: a phase's unique owner publishes a
new immutable version, and readers acquire-load it. Old versions stay allocated
until the node is dropped. Neither readers nor reporters wait for a preempted
writer. The small raw-pointer primitive is isolated in `src/sync.rs`, with safety
comments, drop-count tests, concurrent readers, and strict-provenance Miri checks.
Published metadata is never modified or reclaimed while a reader can reference it.

This avoids both OS blocking and a spin-lock fallback in `no_std + alloc` trees.
It trades bounded-by-job retention for simple reads: plan/total/outcome revisions
retain previous metadata, including revision-history copies. Do not use metadata
revisions as a per-item event stream. A new operation/attempt gets a new tree;
drop old observers when the application's history retention ends.

Shared subscribers are configured before dispatch, so their registry needs no
lock. The dispatch claim is a single nonblocking CAS, not a spin loop; another
poll returns busy and still observes cancellation. No internal tree/registry lock
survives into application callbacks or a suspension-enabled host import.

The optional profiler uses `std::sync::Mutex`; it does not silently substitute
spinning on a platform that cannot block. The built-in collector therefore
requires `std`. `Profiler::try_snapshot()` is the nonblocking UI read path.
Instrumentation and profiler mutation should run on native threads or browser
workers. No-std applications can wrap `Stop`/`Report` with their own instrumentation.

This follows the scheduling concern in [Spinlocks Considered Harmful](https://matklad.github.io/2020/01/02/spinlocks-considered-harmful.html):
a short critical section can still be preempted. Making a spin lock a little
shorter does not remove priority inversion or interrupt deadlock.

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
instances treated as OS worker IDs. Core progress observation requires no mutex,
including if a UI and workers share a Wasm memory through correctly initialized
bindings. The [wasm-bindgen-rayon documentation](https://github.com/RReverser/wasm-bindgen-rayon)
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

The standalone [Wasm probe](../dev/howfar-wasm/README.md) executes actual howfar
callbacks in Wasm under Node 26.7.0. It validates ordinary timer behavior,
JSPI suspension/resumption/cancellation, and worker-posted progress.

The [browser fixture](../dev/howfar-browser/README.md) additionally runs in
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
| No-op generic paths and existing Stop forwarding | `tests/core.rs`, `tests/polling.rs` |
| Strided completed work, empty input, partial final batch, overflow | `tests/core.rs`, `tests/phases.rs` |
| Serial → middle 30% parallel → join → serial, nested/repeated joins | `tests/phases.rs` |
| Manual threads and asymmetric Rayon work sharing one counter | `tests/phases.rs` |
| Codec-style geometry, strided preparation, two parallel waves, serial filter, cancellation and joined output | `tests/hosts.rs` |
| CLI terminal output and Tokio client disconnect cancelling/joining blocking CPU work | `tests/hosts.rs` |
| Unknown/estimated/exact/zero totals, revisions, overrun, skip/fail/cancel | `tests/phases.rs` |
| Frozen terminal records, abandoned parents, stale handles and new attempts | `tests/phases.rs` |
| Metadata publication concurrent with observations | `tests/phases.rs`, strict-provenance Miri |
| Thread-affine FnMut, arbitrary callback work, memoized/deferred snapshots | `tests/polling.rs` |
| Cancellation during busy dispatch, recursion, panic recovery, posted delivery | `tests/polling.rs` |
| StopToken/Option/reference/Arc/builder compatibility and same-check cancellation | `tests/polling.rs` |
| Per-task entry/exit gaps, storms, original call sites, callback cost | `tests/profiling.rs` |
| Straggler overlap, nested spans, queue/join/yield/callback classification | `tests/profiling.rs` |
| Cancellation request → observation → join/cleanup return | `tests/profiling.rs` |
| Bounded retention, abandoned spans, counter/clock diagnostics, JSON escaping | `tests/profiling.rs` |
| Actual Wasm timer boundary, JSPI yield and cancel, worker posts | `dev/howfar-wasm/check.mjs` |
| Real browser UI observations/cancellation during wasm-bindgen-rayon work; native JSPI/chunk fallback | `dev/howfar-browser/browser.spec.mjs` (Chromium + WebKit) |
| Rust 1.88, core-only, no_std+alloc, Cortex-M and wasm32 | CI feature/MSRV/target jobs |

Test scopes are explicit; no test suite proves every possible consumer behavior.
There is no built-in ETA/model fitter or executor. Exported observations support
those consumers without claiming durations are CPU time or fractions are runtime.

## Local performance observations

Measured with rustc 1.98.1 on the local Linux host, using separate empty Cargo
target directories, offline, building the library and its only dependency:

| Features | Clean debug build | Unchanged build |
| --- | ---: | ---: |
| Core only | 0.125 s | 0.030 s |
| alloc | 0.295 s | 0.035 s |
| std | 0.335 s | 0.030 s |
| All, including profile | 0.422 s | 0.031 s |

These include process startup and are observations, not cross-machine promises.
Excluded dev probes, test dependencies, rustdoc, and downloading toolchains are
not part of normal consumer compilation.

`cargo bench -p howfar --bench overhead` is a small smoke benchmark. One local
run measured 0.24 ns/iteration for both the baseline and `NoProgress`, 2.39 ns
for an uncontended saturating `Progress::advance`, 0.53 ns per buffered unit with
batches of 64, and 19.27 ns for `Instant::now`. These are a single host/run, not
contention results or hard latency guarantees. The counter uses saturating CAS,
so shared-counter contention can cost more; per-worker batches reduce that cost.
Clock reads remain a consumer choice, separate from cancellation and accounting.
