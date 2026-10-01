# how-far design

This document explains how `how-far` and `how-far-along` fit together and why
they are built the way they are. The crate READMEs show how to use them;
[the validation notes](how-far-validation.md) list what is tested and how.

## Two crates

**`how-far`** is the interface a library depends on. It defines `Pulse`
(cancellation, counting, and phase planning in one object-safe trait),
`Report`, `Stages`, and the no-op `NoPulse`. It is `no_std + alloc`, has no
feature flags, depends only on `enough`, and builds on Rust 1.85, the same
as `enough`. Adding it to a library adds one small crate to the build.

**`how-far-along`** is what applications and tests depend on. `PulseTree`
implements `Pulse` over a tree of phases that observers read from any
thread. It also provides application-planned trees, callback pollers, a
span profiler (`profile`), and checkpoint advice (`diagnostics`). It
requires Rust 1.88.

A library never depends on the tracker. The application chooses whether
reports become a progress bar, a log, a test assertion, or nothing.

## Who finishes what

Every phase has exactly one owner, and only the owner publishes its outcome.

- `Pulse::split` returns `Child` handles. `Child::finish` consumes the
  handle, so a child is finished at most once, and a child dropped
  unfinished (including during a panic) is recorded as `Abandoned`.
- The pulse a function receives belongs to its caller. `Pulse` has no
  `finish` method, so a library cannot finish what it was given.
- `Stages` finishes the stages it split and never the pulse it was given.
- The application finishes the root with `PulseTree::finish`.

This is what makes libraries composable. When library A runs library B
inside one of its stages, B splits and finishes its own children inside that
stage, then A's `Stages` finishes the stage. If B finished the stage itself,
A's finish would fail and the real result would be lost.

A failed finish never replaces an operation's own error. If a stage cannot
record its outcome (for example because a nested child was leaked), the
stage is recorded as abandoned and the work error is returned unchanged.

## Borrowed pulses and owned handles

`&dyn Pulse` is borrowed on purpose. It costs two words, needs no reference
counting, and works with scoped threads, Rayon, and any synchronous call.

Some code must own its stop policy or progress sink: a codec context that
stores `impl Stop + 'static`, a `std::thread::spawn` worker, a `rayon::spawn`
task, or an async task. `Pulse::handle` gives such code a `PulseHandle`: an
owned, cloneable, `'static` value that checks the same stop policy and
counts into the same phase. It cannot plan or finish anything, so handing one
out never weakens the ownership rules.

`PulseTree` owns its stop policy, so trees are `'static` too: an application
can move one into a worker thread and keep only the observer.

## Counting, totals, and outcomes

A phase either counts its own units (a leaf) or delegates to children (a
branch). Planning happens before the first report and only once, so a
split never has to guess what earlier counts meant.

Weights are relative among siblings and fixed at the split. A middle stage
weighted 30 of 100 keeps exactly 30% of its parent however many workers it
later uses, and a stage that discovers more work cannot silently shrink its
siblings. Work whose size is not known yet stays in a leaf with an
`Unknown` or `Estimated` total, which can be revised; every revision is kept.

Counting to a total does not finish a phase. Outcomes are explicit:
`Succeeded` and `Skipped` discharge a phase's share of the bar even with zero
work; `Cancelled`, `Failed`, and `Abandoned` keep the partial count. An
`Exact` total that is exceeded, and a counter that saturates, stay visible in
snapshots.

A fraction measures weighted, counted work. It is not elapsed time and not an
ETA. Snapshots also report `unresolved_fraction`, the share of the plan that
has no usable total yet, so a display can be honest about what it does not
know.

## Execution declarations

`Execution` describes scheduling without performing it. On a leaf, it says
how the leaf's own units are processed; `WorkPool` means several workers share
one count. On a branch, it says how the children run: `Sequence` or
`ForkJoin`. When a phase splits, the execution passed to `split` replaces the
one in its `PhaseSpec`. `WorkPool` holds a `NonZeroUsize`, so a pool of zero
workers cannot be described at all.

## Synchronization

Counting and cancellation use atomics only. A `Reporter` updates a counter
and two flags; it never locks, allocates, reads a clock, or walks the tree.

Metadata (plans, totals, outcomes) lives in immutable `Arc`s. Writers build
the new version first, then swap one `Arc` for another under a short lock,
and drop the old version after releasing it. Readers clone the current `Arc`
under that lock and do everything else (copying, walking the tree, allocating
snapshots, running callbacks) outside it. `Observer::try_snapshot` returns
`None` instead of waiting if a swap is in progress, so a UI thread can skip a
frame rather than block.

There is no spinning fallback. A spinning lock on a browser's main thread
cannot yield to the event loop, and a preempted lock holder would leave
readers spinning; see
[Spinlocks Considered Harmful](https://matklad.github.io/2020/01/02/spinlocks-considered-harmful.html).

Without `std`, the same API runs on the application's
[`critical-section`](https://docs.rs/critical-section) provider. The provider
must exclude every thread or core that touches the tree, and its entry
latency is its own; only an `Arc` clone or swap happens inside it. Counters
use 64-bit atomics where the target has them and saturate at `usize::MAX`
elsewhere, recording the overflow.

## Profiling and diagnostics

The profiler is opt-in and bounded. Each span keeps counts and at most 64
call sites; finished spans beyond the capacity are dropped and counted, so
every export shows incomplete coverage. Clock reads happen only in
instrumented calls, through a `Clock` the application supplies (browsers use
`performance.now()`). Report-gap timing reads the clock at every report, so
it is a runtime switch that `DiagnosticPulse` turns on rather than a feature
flag; features only add items.

Workers that share a span reach its lock in no particular order, so their
clock readings can arrive out of order. That is concurrency, not a clock
fault: it counts as a zero gap. Clock regressions are counted only where an
order is guaranteed, within one call and at finish.

## Cost and footprint

`&dyn Pulse` is a data pointer and a vtable pointer, so every call takes two
words of argument however large the implementation is, plus a `u64` for
`advance`, and returns at most one byte. Arguments and results travel in
registers. `crates/how-far/tests/footprint.rs` asserts these sizes at compile
time on every target.

Dynamic dispatch does not by itself cause register spills; any call the
compiler cannot inline does, because the loop's live values must survive it.
The remedies are cadence and gating: check once per row, block, or tile;
skip no-op pulses with `(pulse.may_stop() || pulse.may_report()).then_some(pulse)`;
and batch reports from
many workers. Measured on one machine, a checkpoint into a live tree costs 1.6
to 3.1 ns more than into `NoPulse`, which is about 1 to 2% of a 256 KiB
codec loop checked once per 4 KiB; see
[the overhead results](../benchmarks/how-far-overhead.md).

## API evolution

These rules keep both crates additive after their first release:

- **Data types** (`Snapshot`, `Trace`, `SpanRecord`, `Stats`, `Options`,
  `Finding`, `PhaseSpec`, ...) are `#[non_exhaustive]` with public fields.
  Fields may be added; none is renamed, retyped, or removed within a
  compatible release. Build `PhaseSpec` with `new` and `Options` from
  `Default`. Public fields hold plain values; when a representation may need
  to change, it is a method instead.
- **Enums** (`Outcome`, `Total`, `Execution`, `PlanError`, `RunError`,
  `Status`, `Kind`, ...) are `#[non_exhaustive]`; match with a wildcard arm.
  `Execution::WorkPool` is a non-exhaustive variant built with
  `Execution::work_pool`, so it can gain fields.
- **Traits** (`Pulse`, `ChildPulse`, `Report`, `Clock`, and `enough::Stop`)
  are open, because wrappers implement them. Methods added later get default
  bodies; a new required method would be a breaking change.
- **Features** only add items. No feature changes another feature's types or
  what it records.
- **Finding text** is for people: only `Kind` is a contract.
- **JSON** gains keys and never loses them. `schema_version` changes only if
  a key is removed or its meaning changes, and readers ignore keys they do
  not know.
- **One copy of `Stop`.** `how-far` re-exports `enough::Stop`, so a build
  must resolve to one `enough`. `enough` stays small and stable; if a
  breaking release is ever unavoidable, it will re-export the old major's
  traits from the new one so both remain the same trait.

The `docs/public-api/*.txt` snapshots (`just api-doc`) make every surface
change visible in review. After their first release, both crates join the CI
`cargo semver-checks` job, and breaking changes queue in `CHANGELOG.md`.
