# `Stopper` (Relaxed) vs `SyncStopper` (Release/Acquire): when it matters, what it costs (2026-10-06)

## When it matters

`crates/almost-enough/examples/stop_ordering.rs` runs a message-passing
litmus test with the real types:
- A writer stores a value (`AtomicU32`, Relaxed) and then calls `cancel()`.
- A reader first reads the value, so the old one is in its cache. It then
  waits until `should_stop()` returns true and reads the value again.
- Seeing the old value after the stop is a stale read.

Writer and reader advance in lockstep, so every read races its write.

```text
cargo run --release -p almost-enough --example stop_ordering -- 50
```

| host | `Stopper` (Relaxed) | `SyncStopper` (Release/Acquire) |
| --- | ---: | ---: |
| `r5900xt`: Ryzen 9 5900XT (x86-64, Zen 3), rustc 1.99.0 | 0 in 2,000,000 | 0 in 2,000,000 |
| `zen-arm-xl`: Ampere Altra (Neoverse-N1), rustc 1.97.1 | 10,483,993 in 50,000,000 (21%) | 0 in 50,000,000 |
| `mac`: Apple M4 Pro, rustc 1.99.0 | 4,761 in 50,000,000 (1 in 10,500) | 0 in 50,000,000 |

On x86-64, loads are not reordered with other loads, nor stores with other
stores, so neither type can read stale there. On the ARM cores the reader's
second load of the value can be satisfied before its load of the flag. The
disassembly on Neoverse-N1 confirms that the stale reads are the hardware's,
not the compiler's:
- The `Stopper` reader reloads the value (`ldr`) after the loop that loads the
  flag with a plain `ldrb`.
- `SyncStopper` loads the flag with `ldaprb`, an acquire load. Its extra `add`
  (acquire loads take no offset) is the one extra instruction per check
  measured earlier.

In safe Rust this matters only when the cancelling thread hands over data
through something that does not synchronize on its own: Relaxed atomics, in
practice. Data behind a `Mutex`, sent on a channel, or read after a thread
`join` is already synchronized. Examples: a cancel reason or "save progress"
request stored in an atomic just before `cancel()`, which the worker reads
once it sees the stop.

## What it costs

`cargo bench -p almost-enough --bench stopper_ordering` (zenbench 0.1.9,
paired and interleaved, gate disabled). Isolated checks, and a PNG Sub
defilter over 64 KiB with a check through `&dyn Stop` every 64 B or 1 KiB.
The `vs base` column is `SyncStopper` against `Stopper`, as a 95% CI:

| | x86-64 (Zen 3) | Neoverse-N1 | Apple M4 Pro |
| --- | ---: | ---: | ---: |
| check through `&dyn Stop` | 1.44 vs 2.06 ns (−30%)* | 2.25 vs 2.24 ns [−4.0%, +4.9%] | 0.51 vs 0.51 ns [−0.1%, +0.4%] |
| check, generic | 0.82 vs 0.82 ns [−0.4%, +0.3%] | 1.02 vs 1.01 ns [−0.0%, +0.9%] | 0.29 vs 0.29 ns [−1.4%, +0.2%] |
| defilter, check every 64 B | 7.4 vs 8.0 µs (−8%)* | 18.9 vs 18.4 µs [+2.2%, +3.1%] | 4.6 vs 4.6 µs [−0.1%, +0.5%] |
| defilter, check every 1 KiB | 7.0 vs 7.0 µs [−0.7%, −0.6%] | 16.6 vs 16.7 µs [−0.7%, −0.2%] | 6.4 vs 6.4 µs [−0.0%, +0.1%] |

\* On x86-64 the two compile to the same instructions (an Acquire load is a
plain `mov`), so these differences are code placement, not ordering. Effects
of that size can also move the N1 numbers.

Measured with perf on Neoverse-N1 (the `enough` stop table in PR #27's
`benchmarks/how-far-2026-10-06.md`), `SyncStopper` costs one instruction more per check
through `&dyn Stop` and the same in generic code. Cycles were within noise.
