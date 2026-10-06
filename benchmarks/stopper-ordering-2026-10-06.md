# `Stopper`'s memory ordering: Release `cancel()`, Acquire `is_cancelled()` (2026-10-06)

Until 0.4.5 every stop in these crates used Relaxed ordering, except
`SyncStopper` (Release/Acquire on everything). 0.4.5 makes `cancel()` a
Release store on `Stopper`, `StopSource`, `ChildStopper` and the FFI source,
and their `is_cancelled()` the Acquire query (`ChildStopper`: an Acquire fence
after its walk). Checks (`check()`, `should_stop()`, `StopRef`, `StopToken`,
the FFI tokens) stay one Relaxed load: the default check must not gain
instructions. `SyncStopper` is unchanged, an Acquire load on every check, for
code that wants the guarantee from the check itself. This record is the
evidence, including what the designs not taken would cost.

On aarch64 an Acquire load compiles to `ldapr` when the target enables RCpc
and to the stronger `ldar` when it doesn't. Of the aarch64 targets, only
`aarch64-apple-darwin` enables RCpc by default; the Linux, Windows, Android
and iOS targets don't, so their builds use `ldar` on every core, RCpc or not.
`-C target-cpu` changes this, so the N1 results below say which they used.

Hosts:
- `r5900xt`: Ryzen 9 5900XT (x86-64, Zen 3), native Ubuntu, rustc 1.99.0,
  default flags.
- `zen-arm-xl`: Ampere Altra (Neoverse-N1), rustc 1.97.1. The host's
  environment sets `RUSTFLAGS=-C target-cpu=neoverse-n1`, which enables RCpc.
  Runs marked **RCpc** used it. Runs marked **default** overrode it with
  `-C target-cpu=generic`, which is what an `aarch64-unknown-linux-gnu` build
  gets; their binaries contain `ldar` and no `ldapr`.
- `mac`: Apple M4 Pro, rustc 1.99.0, default flags (so `ldapr`).

## The guarantee: call `is_cancelled()` after the stop

`crates/almost-enough/examples/stop_ordering.rs` is a message-passing litmus
test:
- A writer stores a value (an `AtomicU32`, Relaxed) and then cancels a fresh
  stop.
- A reader first reads the value, so the old one is in its cache. It then
  waits until a check sees the stop and reads the value again.
- Seeing the old value after the stop is a stale read.

Writer and reader advance in lockstep, so every read races its write. Three
readers: a relaxed flag (`Stopper` before 0.4.5), `Stopper` read right after
the check, and `Stopper` read after `is_cancelled()`.

```text
cargo run --release -p almost-enough --example stop_ordering -- 50
```

| host | relaxed flag | `Stopper`, read after the check | `Stopper`, read after `is_cancelled()` |
| --- | ---: | ---: | ---: |
| x86-64 (Zen 3), 20M rounds | 0 | 0 | 0 |
| Neoverse-N1, default, 2 × 50M | 13,440,740 / 13,399,714 (27%) | 2,501,919 / 2,653,479 (5%) | 0 / 0 |
| Apple M4 Pro, 4 × 50M | 2,789 to 3,819 | 26,385 to 31,501 | 0 in all four |

**The Release store alone is not a mitigation.** It keeps the writer's
stores in order, but a Relaxed reader's loads may still pass each other, and
how often that happens depends on timing. Here `cancel()`'s Release cut stale
reads about 5× on N1 and raised them about 9× on the M4 Pro. In a separate
harness that kept the flags inline in a `Vec` instead of in `Stopper`'s own
allocations, it cut them 4.6× on N1 and 29× on the M4 Pro. Only the reader's
Acquire, `is_cancelled()`, removed them, in every run on every host. x86-64
can't reorder loads with loads or stores with stores, so no reader can read
stale there.

Earlier runs, against the first 0.4.5 draft whose checks loaded with
Acquire: the relaxed flag read stale 11,280,809 times in 50M rounds on N1
(RCpc), 12,745,669 and 13,253,211 on N1 (default) and 3,754 on the M4 Pro;
the Acquire checks never did. The N1 disassembly shows the reader reloading
the value after the flag loop with a plain `ldrb`, and the acquire load as
`ldaprb` (RCpc) or `ldarb` (default). So the stale reads are the hardware's,
not the compiler's.

In safe Rust this matters only when the cancelling thread hands over data
through something that does not synchronize on its own: Relaxed atomics, in
practice. Examples: a cancel reason, or a request to save progress, stored
in an atomic just before `cancel()`. Data behind a `Mutex`, sent on a
channel, or read after a thread `join` is already synchronized.

`is_cancelled_after_the_stop_sees_writes_made_before_cancel`, a unit test in
`stopper`, `source`, `tree` and `enough-ffi`, guards it: the reader waits
with Relaxed checks, then calls `is_cancelled()` before reading. All four
pass under Miri's weak-memory emulation over 16 seeds; with `Stopper`'s
`is_cancelled()` made Relaxed, or `ChildStopper`'s fence removed, Miri fails
them. CI's Miri job runs them over 8 seeds.

## What it costs

**Checks: nothing.** Every check path was compiled with `--emit asm` for
x86-64 and aarch64 (rustc 1.99.0, `-O`) against `main`: `Stopper`'s `check`
and `should_stop`, `StopSource` and `StopRef`, `ChildStopper`, a `StopToken`
of a `Stopper`, the FFI token, and calls through `&dyn Stop`. All are the same
instructions, except `ChildStopper::should_stop`, which now forwards its
caller's `#[track_caller]` location to its parent instead of loading a
constant one: one instruction fewer on x86-64, two on aarch64.

**`cancel()`:** `stlrb` instead of `strb` on aarch64, the same `mov` on
x86-64, a `dmb sy` on Cortex-M; once per cancellation.

**`is_cancelled()`:** an Acquire load (`ldarb` or `ldaprb` on aarch64, a plain
`mov` on x86-64), called after the work, not in loops. No zen crate calls
these `is_cancelled()` methods at all.

## What an Acquire on every check would cost

That is `SyncStopper`'s design, and the first 0.4.5 draft's for `Stopper`.

`cargo bench -p almost-enough --bench stopper_ordering` (zenbench 0.1.9,
paired and interleaved, gate disabled). Each cell is a 95% CI of an Acquire
check's difference from the relaxed flag's. The x86-64, N1 RCpc and M4 Pro
columns measured the first 0.4.5 draft, whose `Stopper` loaded with Acquire;
the N1 default column measured `acquire_flag`, which compiles the same.

The first four rows are compute-bound:
- isolated checks, through `&dyn Stop` and generic;
- a PNG Sub defilter over 64 KiB with a check every 64 B or 1 KiB.

The rest run over 256 MiB, beyond every last-level cache here, and are bound
by memory latency:
- **gather:** loads from random places, with addresses from a PRNG, so
  misses can overlap;
- **contended gather:** the same, while another thread clones and drops the
  stop, whose `Arc` counts share the flag's cache line;
- **chase:** a pointer chase around one random cycle, where each load waits
  for the last.

The walks carry their position across rounds and differ between variants:
each round reaches new lines, and no variant warms another's. Hops cost
135–155 ns on N1, 108 ns on the M4 Pro and 106 ns on x86-64.

| | x86-64 | N1, RCpc (`ldapr`) | N1, default (`ldar`) | Apple M4 Pro (`ldapr`) |
| --- | ---: | ---: | ---: | ---: |
| check through `&dyn Stop` | −33%\* | [−3.5%, +4.9%] | [−4.8%, −0.7%] | [−0.8%, +0.9%] |
| check, generic | [−0.1%, +0.4%] | [−0.3%, +0.2%] | [−0.0%, +0.9%] | [−0.1%, +0.5%] |
| defilter, check every 64 B | +15%\* | [+0.0%, +1.0%] | **[+3.4%, +4.3%]** | [−0.0%, +0.0%] |
| defilter, check every 1 KiB | −2.7%\* | [−0.6%, +0.0%] | [+0.3%, +0.8%] | [−0.0%, +0.3%] |
| gather, check every 8 loads | [−0.2%, +0.4%] | [−1.4%, +0.1%] | [−0.5%, +1.0%] | [−0.1%, +0.2%] |
| gather, check every 64 loads | [−0.3%, +0.4%] | [−0.8%, +0.7%] | [−0.5%, +0.7%] | [−0.0%, +0.1%] |
| gather every 8, stop's line contended | [−0.8%, +0.8%] | [−0.8%, +0.5%] | [−0.7%, +0.5%] | [−0.1%, +0.3%] |
| chase, check every 4 hops | [−0.3%, +0.3%] | [−0.6%, +0.3%] | [−0.7%, +0.6%] | [−0.0%, +0.3%] |
| chase, check every 64 hops | [−0.4%, +0.0%] | [−0.4%, +1.5%] | [−1.3%, +0.7%] | [−0.1%, +0.1%] |

\* On x86-64 both orderings compile to plain `mov`s, so these can't come from
the ordering. They come from how each type's code is laid out and inlined,
and they flip sign between rows. Earlier N1 (RCpc) runs of the 64 B defilter
row measured [+2.2%, +3.1%] and [−1.1%, +0.2%], and the same effects may be
behind them.

On N1 with `ldar`, the 64 B defilter row came out slower than the relaxed
flag in all four runs: [+3.4%, +4.3%], [+5.3%, +7.2%], [+7.0%, +8.6%] (with
every function aligned to 64 bytes), and [+7.8%, +9.6%] for `Stopper`
itself. The size is uncertain: in one run, two variants whose checks compile
to the same instructions differed by 10 points in that row. Isolated checks
measured anywhere from no difference to +34%, moving with code placement.

So an Acquire on every check would cost:
- **x86-64:** nothing; both orderings are the same instructions.
- **aarch64 with RCpc** (macOS builds, or `-C target-cpu` on a core that has
  it): nothing measurable on the M4 Pro or N1, compute-bound or memory-bound.
- **aarch64 without RCpc** (default Linux, Windows, Android and iOS builds):
  on N1, a compute-bound loop that checks every 64 bytes ran 3–10% slower;
  checking every 1 KiB, or in loops bound by memory, nothing measurable.

Neither acquire load is a fence: each keeps the accesses after it from
passing the flag's own load. Each is one instruction more per check through `&dyn Stop`,
because neither takes an address offset.

Not measured:
- other aarch64 cores with `ldar`, such as Graviton or Cortex-X;
- in-order cores, such as Cortex-A55;
- microcontrollers, where an Acquire check executes a full barrier (below).

## How soon another core sees the stop

The ordering doesn't change it. Cache coherence carries a store to other
cores the same way at any ordering; the ordering only decides what else is
visible along with it. And at any ordering, a thread that has seen the stop
never reads the flag as unset again.

`crates/almost-enough/examples/cross_core_latency.rs` measures it: one
thread stores a sequence number, another spins until it loads it and
replies the same way, and half a round trip is the time from a store to the
load that sees it. Medians of 15 interleaved repetitions of 200,000 round
trips each:

| host | cores | Relaxed | Release/Acquire | SeqCst |
| --- | --- | ---: | ---: | ---: |
| x86-64 (Zen 3) | same CCD (0, 1) | 39.3 ns | 39.2 ns | 39.2 ns |
| x86-64 (Zen 3) | across CCDs (0, 8) | 219.0 ns | 219.8 ns | 219.3 ns |
| Neoverse-N1, default | 2, 3 | 147.8 ns | 145.2 ns | 145.1 ns |
| Neoverse-N1, default | 2, 14 | 138.1 ns | 137.4 ns | 137.5 ns |
| Apple M4 Pro | unpinned | 37.3 ns | 36.9 ns | 36.9 ns |

How often the work checks decides how soon it stops. `dev/cancel-latency`
measured gaps between checks of up to 45 µs in a 2048² PNG decode (one check
per row) and up to 269 µs in a 4K progressive JPEG encode.

The x86 runs waited for the host's load average to fall below 4.

## Rejected: a Relaxed load and a fence on stop

A Relaxed load followed by an Acquire fence only when it reads `true` gives
the same guarantee: the fence synchronizes with the Release store that the
load read. Under Miri it passed the ordering test on all 8 seeds, and failed
on all 8 with the fence removed. Until the stop, every check is then a plain
load and a branch, with the barrier off the hot path (`dmb ishld` on
aarch64, `dmb sy` on Cortex-M, `fence r,rw` on RISC-V).

Measured as `fenced_flag`, it was no cheaper than `ldar`, and on x86-64 it
costs a test and branch that an Acquire load doesn't. Against the relaxed
flag, "aligned" meaning `-C llvm-args=-align-all-functions=6`:

| | N1 default | N1 default, aligned | x86-64 | x86-64, aligned |
| --- | ---: | ---: | ---: | ---: |
| check, generic: Acquire | [−0.0%, +0.9%] | [+20.1%, +20.7%] | [−0.2%, +0.3%] | [−0.3%, +0.2%] |
| check, generic: fenced | [+32.8%, +34.0%] | [+22.0%, +24.7%] | [+24.5%, +25.1%] | [+32.8%, +33.5%] |
| defilter, every 64 B: Acquire | [+3.4%, +4.3%] | [+7.0%, +8.6%] | [+14.6%, +15.0%]\* | [−12.9%, −12.5%]\* |
| defilter, every 64 B: fenced | [+4.5%, +5.6%] | [+8.7%, +9.9%] | [+19.5%, +20.0%]\* | [+4.0%, +4.3%]\* |

\* On x86-64 the Acquire and relaxed flags compile to the same instructions,
so these rows move with code placement alone. The generic rows, where those
two match, show the fenced check's extra branch.

## Measured: a Relaxed arm in `StopToken`

Measured against the first 0.4.5 draft, whose `Stopper` checked with
Acquire: `StopToken::relax(stopper)` would give one token a Relaxed load while
the `Stopper` and its other clones keep Acquire. The prototype is on the
`investigate/stoptoken-relax` branch. Its bench compares a relaxed token
with `StopToken::from(stopper)`; both run the same `StopToken::check`, so
code placement can't move their gap.

On N1 with default codegen, the relaxed token was slower:

| | relaxed token against Acquire token |
| --- | ---: |
| check, generic | [+24.5%, +25.9%] |
| check through `&dyn Stop` | [+0.6%, +1.9%] |
| defilter, check every 64 B | [+2.3%, +4.1%] |
| defilter, check every 1 KiB | [−0.9%, −0.5%] |

Reaching a fourth arm takes another compare and a taken branch, which costs
as much as `ldar` saves. On x86-64 the two tokens matched within 0.5%, but
four arms compile the `match` to a jump table, so every check, Acquire
tokens included, becomes an indirect jump instead of two conditional
branches. `StopToken` compiled that way before 0.4.5, when it had four arms.

A build can get `ldapr` without any API: `-C target-feature=+rcpc` makes an
aarch64 Linux build emit it. The binary then runs only on cores with RCpc,
such as Neoverse-N1 and later; older ones, such as Cortex-A72, fault on it.

## Other targets

The ordering changes only the instructions emitted, never which targets
build. `enough` and `almost-enough` built with the same results with and
without Release/Acquire flags for `thumbv6m-none-eabi`, `riscv32imc-unknown-none-elf`,
`thumbv7em-none-eabihf` and `wasm32-unknown-unknown`, with no default
features and with `alloc`. With `alloc`, both fail on the first two targets,
before and after: they have no compare-and-swap, so no `alloc::sync`.

A flag load and store, compiled with rustc 1.99.0 and `-O`:

| target | Relaxed load | Acquire load | Relaxed store | Release store |
| --- | --- | --- | --- | --- |
| `wasm32-unknown-unknown` | `i32.load8_u` | `i32.load8_u` | `i32.store8` | `i32.store8` |
| the same, `+atomics` | `i32.atomic.load8_u` | `i32.atomic.load8_u` | `i32.atomic.store8` | `i32.atomic.store8` |
| Cortex-M (`thumbv6m`, `thumbv7m`, `thumbv7em`) | `ldrb` | `ldrb`, `dmb sy` | `strb` | `dmb sy`, `strb` |
| `riscv32imc` | `lb` | `lb`, `fence r,rw` | `sb` | `fence rw,w`, `sb` |

Wasm has only sequentially consistent atomics, so both orderings emit the
same instruction, with or without threads. On Cortex-M and 32-bit RISC-V,
an Acquire load or a Release store carries a full barrier. With Release
`cancel()` and Acquire `is_cancelled()`, that is once per cancellation and
once per query, never per check (`SyncStopper`'s checks excepted). No
microcontroller was available to measure the per-check cost.

The N1 runs shared the box with three niced fuzzers (load average about 6).
The x86 runs started once its load average had fallen below 5.
