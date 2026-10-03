# how-far [![CI](https://img.shields.io/github/actions/workflow/status/imazen/enough/ci.yml?style=flat-square&label=CI)](https://github.com/imazen/enough/actions/workflows/ci.yml) [![crates.io](https://img.shields.io/crates/v/how-far?style=flat-square)](https://crates.io/crates/how-far) [![lib.rs](https://img.shields.io/crates/v/how-far?style=flat-square&label=lib.rs&color=blue)](https://lib.rs/crates/how-far) [![docs.rs](https://img.shields.io/docsrs/how-far?style=flat-square)](https://docs.rs/how-far) [![MSRV](https://img.shields.io/badge/MSRV-1.86-blue?style=flat-square)](https://doc.rust-lang.org/cargo/reference/manifest.html#the-rust-version-field) [![license](https://img.shields.io/crates/l/how-far?style=flat-square)](#license)

One interface for cancellation, progress, and weighted phases in libraries.

A library accepts `&dyn Pulse`. Through it, the library asks whether to
stop, counts finished work, and splits itself into phases with fixed
weights. The caller decides what those reports become: nothing, a progress
bar, a live tree that another thread can read, or a profile. The library's
signature is the same either way.

```toml
[dependencies]
how-far = "0.1"
```

`how-far` is `no_std + alloc`, forbids `unsafe`, has no feature flags, and
depends only on [`enough`](https://crates.io/crates/enough), whose `Stop`
trait it re-exports. Applications pick a tracker such as
[`how-far-along`](https://github.com/imazen/enough/blob/main/crates/how-far-along/README.md).

## A library

```rust
use how_far::{PhaseSpec, ProgressExt, Pulse, RunError, Stages, StopReason, Total};

pub fn thumbnail(rows: &[Vec<u8>], pulse: &dyn Pulse) -> Result<Vec<u8>, RunError<StopReason>> {
    let count = rows.len() as u64;
    let mut stages = Stages::new(pulse, &[
        PhaseSpec::new("decode", 1, Total::Exact(count)).units("rows"),
        PhaseSpec::new("resize", 4, Total::Exact(count)).units("rows"),
    ])?;
    let decoded = stages.run_stoppable(|stage| {
        stage.check()?; // once, before the loop
        let mut out = Vec::new();
        for row in rows {
            out.push(row.iter().map(|&b| b / 2).collect::<Vec<u8>>());
            stage.step(1)?; // count the finished row, then check for a stop
        }
        Ok(out)
    })?;
    let resized = stages.run_stoppable(|stage| {
        let mut out = Vec::new();
        for row in &decoded {
            out.push(row.iter().step_by(2).copied().sum::<u8>());
            stage.step(1)?;
        }
        Ok(out)
    })?;
    stages.finish()?;
    Ok(resized)
}

// Callers that don't care pass `&NoPulse`, which checkpoints skip without a call.
let _pixels = thumbnail(&vec![vec![8; 16]; 4], &how_far::NoPulse)?;
# Ok::<(), RunError<StopReason>>(())
```

A tracker records two phases with a 1:4 weight split, counts in rows, an
outcome for each stage, and a fraction a progress bar can show.

## Three rules

**Finish what you split, never what you were given.**
[`Pulse::split`](https://docs.rs/how-far/latest/how_far/trait.Pulse.html#tymethod.split)
returns owned `Child` handles; finishing one consumes it. The pulse a
function receives belongs to its caller, so a library never finishes it.
That is what lets one library call another inside a stage: the inner
library plans and finishes its own children, and the outer `Stages` finishes
the stage afterwards. The application finishes the root.

**Report completed work.** `advance(n)` counts finished units, including a
short final batch. `step(n)` counts and then checks for cancellation, so work
done before a stop request is still counted. Report actual counts, not
estimates; a `Total` can be `Exact`, `Estimated`, or `Unknown`.

**Borrow in hot loops, own in `'static` code.** `&dyn Pulse` costs two
words and borrows. Code that must own its stop policy or progress sink, such
as a codec context built with its own stop, a `std::thread::spawn` worker, or
an async task, takes [`Pulse::handle`](https://docs.rs/how-far/latest/how_far/trait.Pulse.html#tymethod.handle):
an owned, cloneable `'static` value that checks the same stop policy and
counts into the same phase.

## Stages

`Stages::new(pulse, &parts)` splits a pulse into sequential stages. Each
`run` hands its closure the next stage as `&dyn Pulse` and then finishes that
stage: `Succeeded` for `Ok`, otherwise the outcome its error maps to. After an
error, every later stage is finished as `Skipped`, and the original error is
returned unchanged.

| Method | An error from the closure means |
| --- | --- |
| `run` | the stage failed |
| `run_stoppable` | the stage was cancelled |
| `run_classified(is_stop, ..)` | cancelled if `is_stop(&error)`, otherwise failed |
| `run_nested(is_stop, ..)` | the same, for a closure that splits the stage itself and uses `?` on plan errors |

To call another library inside a stage, pass it the stage:
`stages.run_classified(CodecError::is_stop, |stage| codec::encode(image, stage))`.
The codec plans its own stages inside yours. A `&dyn Pulse` is also a
`&dyn Stop` and a `&dyn Report`, so code that only checks for cancellation
takes the stage as it is: `legacy::decode(input, stage)`.

## Parallel work

Workers that count one logical phase share it. A Rayon `par_iter`, scoped
threads, or a pool all call `stage.step(1)` on the same `&dyn Pulse`; join
them before the closure returns. Declare
`Execution::work_pool(threads)` on the phase so observers know.

Give workers their own child phases only when each needs its own total,
weight, or outcome:

```rust
use how_far::{Execution, Outcome, PhaseSpec, ProgressExt, Pulse, StopReason, Total};

fn tiles(pulse: &dyn Pulse) -> Result<(), StopReason> {
    let [left, right] = pulse
        .split_array(Execution::ForkJoin, [
            PhaseSpec::new("left", 1, Total::Exact(8)),
            PhaseSpec::new("right", 1, Total::Exact(8)),
        ])
        .expect("valid plan");
    std::thread::scope(|scope| {
        for child in [left, right] {
            scope.spawn(move || {
                let result = (0..8).try_for_each(|_| child.step(1));
                child.finish(Outcome::from_result(&result, |_| true)).expect("joined");
                result
            });
        }
    });
    Ok(())
}
tiles(&how_far::NoPulse)?;
# Ok::<(), StopReason>(())
```

`'static` workers, such as `std::thread::spawn`, `rayon::spawn`, or
`tokio::spawn`, cannot borrow a pulse. Give each one `pulse.handle()`.

## What it costs

`&dyn Pulse` is a data pointer and a vtable pointer: two words, however
large the implementation behind it. Every hot-path call takes those two words
(plus a `u64` for `advance`) and returns at most one byte, so arguments and
results travel in registers and nothing spills at the call itself. These
sizes are asserted at compile time on 32- and 64-bit targets:

| Value | Size |
| --- | --- |
| `&dyn Pulse`, `Child`, `Box<dyn Pulse>` | 2 words |
| `Paced` | 2 words + 16 bytes |
| `Result<(), StopReason>`, `Outcome` | 1 byte |
| `PulseHandle` | 4 words |
| `FnPulse` | 1 word |
| `NoPulse`, one static of type `Inert` | 1 byte |
| `NoReport`, `ProgressWithStop<Unstoppable, NoReport>` | 0 bytes |

When nobody listens, steps cost nothing. There is exactly one `NoPulse`, a
static, and `step`, `live()` and `Paced` recognize it by its address, so a
loop that steps on every iteration compiles to the bare loop: the one test
moves out of it. Where it cannot move, the test is two comparisons and no call.
Children and stages planned under `NoPulse` are the same static. A bare
`check()` through `&dyn Pulse` still makes one call.

`&dyn` does not stop a hot loop from spilling registers: any call the
compiler cannot inline forces the loop's live values out of caller-saved
registers, dynamic or not. The fix is cadence, not dispatch:

- Pace checkpoints. `let mut pace = pulse.paced(64 * 1024);` counts each
  `pace.step(n)?` in a local and reaches the pulse once per 64 Ki units: it
  reports them, then checks for cancellation. A step that does not reach the
  pulse is a subtraction and a branch. Choose the interval so the work
  between reaches takes at least a microsecond, and no longer than the
  cancellation latency you need. Give each worker its own.
- Gate other no-op pulses where pacing does not fit. `let pulse = pulse.live();`
  gives an `Option<&dyn Pulse>`, still two words, whose `check()`,
  `advance()` and `step()` make no call at all when the pulse neither stops
  nor reports. Bring the methods into scope with `use how_far::prelude::*;`.
- Otherwise check once per row, block, or tile, not per pixel or byte.

Counted with perf on one machine (a Ryzen 9 5900XT, rustc 1.99), a checkpoint
into a live tree costs about 56 instructions with `step` and about 2 with
`Paced`. On a 256 KiB PNG-style defilter checked every 256 bytes, that is
13.9% more instructions with `step` and 0.6% with `Paced`; every 4 KiB, 0.92%
and 0.13%. With `&NoPulse`, `step`, `live()` and `Paced` add no instructions.
A three-stage `Stages` plan adds about 870 instructions per operation with
`NoPulse` and about 7,000 with a live tree, so a live tree costs under 1% of
operations longer than about 70 µs. An `FnPulse` reaches its callback in
about 90 instructions per report plus what the callback does, and plans for
about 3,800 per three-stage operation
([measured](https://github.com/imazen/enough/blob/main/benchmarks/how-far-fnpulse-2026-10-03.md)). The perf counts
([2026-10-03](https://github.com/imazen/enough/blob/main/benchmarks/how-far-checkpoint-nopulse-2026-10-03.md),
[2026-10-01](https://github.com/imazen/enough/blob/main/benchmarks/how-far-checkpoint-cost-2026-10-01.md))
and earlier [wall-time results](https://github.com/imazen/enough/blob/main/benchmarks/how-far-overhead.md)
are committed.

At compile time, `how-far` has no build script, proc macros, or features, and
depends only on `enough`. Code that takes `&dyn Pulse` compiles once, whatever
pulse its callers pass, and each `Stages::run_*` call adds only a few lines to
the caller's crate; CI fails if that grows. `FnPulse` is about 40% of
`how-far`'s own release build, compiled once per build. See the
[build cost](https://github.com/imazen/enough/blob/main/docs/how-far-validation.md#build-cost).

## One callback for progress and cancellation

An application that wants a progress bar and a way to stop passes an
`FnPulse`. Its callback runs after each report and when a phase finishes. It
sees the whole job's fraction, weighted by every plan the libraries made, and
the phase that reported. Return an error to stop the work: from then on every
check returns it, so the library stops at its next checkpoint.

```rust
use how_far::{FnPulse, Pulse, StopReason};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

# fn thumbnail(_: &[Vec<u8>], _: &dyn Pulse) -> Result<(), StopReason> { Ok(()) }
let interrupted = Arc::new(AtomicBool::new(false)); // set from a Ctrl-C handler
let flag = Arc::clone(&interrupted);
let pulse = FnPulse::new(move |progress| {
    eprint!("\r{:3.0}% {}", progress.fraction * 100.0, progress.phase);
    if flag.load(Ordering::Relaxed) {
        Err(StopReason::Cancelled)
    } else {
        Ok(())
    }
});
thumbnail(&[vec![8; 16]], &pulse)?;
# Ok::<(), StopReason>(())
```

- The callback may run on several threads at once. It must be `'static`,
  because handles given to spawned threads reach it too.
- After it returns an error it is not called again; the first error wins.
- A phase with an `Exact` or `Estimated` total moves the fraction as it
  counts; one with an `Unknown` total moves it when it finishes.
- It runs as often as the library reports. A library that steps on every row
  calls it on every row, so keep it cheap or throttle inside it.

## No-op and count-only use

`&NoPulse` never stops, discards reports, and still validates plans, so
planning mistakes surface even when nobody watches. References, `Box` and
`Arc` of a pulse are pulses too, so `Box::new(&NoPulse)` is an owned no-op;
only `&NoPulse` itself is recognized and skipped. Algorithms that only count
can accept `impl Report` instead; `NoReport` discards counts, and references,
`Box`, `Arc`, and `Option` forward them. `ProgressWithStop` pairs any stop
policy with any sink.

## Implementing `Pulse`

Most applications need no implementation: `&NoPulse` ignores everything,
`FnPulse` turns one callback into a pulse, and `how-far-along` tracks a live
tree. A tracker of your own implements `Stop`, `Report`, `Pulse` and, for its
children, `ChildPulse`; the
[`Pulse` documentation](https://docs.rs/how-far/latest/how_far/trait.Pulse.html#implementing)
lists the rules no type enforces. Start `split` with
`PhaseSpec::validate_split`, which rejects the same plans every pulse
rejects, and hand out `Child::inert()` for a part nobody needs to watch or
stop.

## Tracking and testing

Applications and tests pass a
[`how-far-along`](https://github.com/imazen/enough/blob/main/crates/how-far-along/README.md)
`PulseTree` and read snapshots through an observer. Library tests can also
measure checkpoint cadence and stage weights without changing the library;
see the [testing and tuning guide](https://github.com/imazen/enough/blob/main/docs/how-far-testing-and-tuning.md).

## License

Licensed under either of [MIT](https://github.com/imazen/enough/blob/main/LICENSE-MIT)
or [Apache-2.0](https://github.com/imazen/enough/blob/main/LICENSE-APACHE), at your option.
