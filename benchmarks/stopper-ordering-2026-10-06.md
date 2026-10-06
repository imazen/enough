# Release/Acquire for `Stopper`: when it matters, what it costs (2026-10-06)

`Stopper` used Relaxed ordering until 0.4.5; `SyncStopper` was the same flag
with Release/Acquire. 0.4.5 gives `Stopper` (and `StopSource`, `ChildStopper`
and the FFI flag) Release/Acquire, and makes `SyncStopper` a deprecated
wrapper. This record is the evidence.

The harness compares `Stopper` against local flags laid out as it is, an
`Arc` holding an `AtomicBool`, that differ only in the check: a relaxed flag
(the old `Stopper`), an Acquire flag (the same instructions as `Stopper`),
and a fenced flag (an alternative, rejected below).

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

## When it matters

`crates/almost-enough/examples/stop_ordering.rs` is a message-passing litmus
test:
- A writer stores a value (an `AtomicU32`, Relaxed) and then cancels a fresh
  stop.
- A reader first reads the value, so the old one is in its cache. It then
  waits until it sees the stop and reads the value again.
- Seeing the old value after the stop is a stale read.

Writer and reader advance in lockstep, so every read races its write.

```text
cargo run --release -p almost-enough --example stop_ordering -- 50
```

| host | relaxed flag | `Stopper` (Release/Acquire) |
| --- | ---: | ---: |
| x86-64 (Zen 3) | 0 in 20,000,000 | 0 in 20,000,000 |
| Neoverse-N1, RCpc | 11,280,809 in 50,000,000 (23%) | 0 in 50,000,000 |
| Neoverse-N1, default | 12,745,669 in 50,000,000 (25%) | 0 in 50,000,000 |
| Apple M4 Pro | 3,754 in 50,000,000 (1 in 13,300) | 0 in 50,000,000 |

Other runs: 13,253,211 stale reads in 50,000,000 on N1 (default), and, of
`Stopper` while it was Relaxed against `SyncStopper`, 10,483,993 on N1 (RCpc)
and 4,761 on the M4 Pro; `SyncStopper` never read stale. x86-64 can't
reorder loads with loads or stores with stores, so neither ordering can read
stale there.

The N1 disassembly shows the reader reloading the value after the flag loop:
a plain `ldrb` for the relaxed flag; `ldaprb` (RCpc) or `ldarb` (default), both
acquire loads, for the ordered one. So the stale reads are the hardware's,
not the compiler's.

In safe Rust this matters only when the cancelling thread hands over data
through something that does not synchronize on its own: Relaxed atomics, in
practice. Examples: a cancel reason, or a request to save progress, stored
in an atomic just before `cancel()`. Data behind a `Mutex`, sent on a
channel, or read after a thread `join` is already synchronized.

A unit test, `a_clone_that_sees_the_stop_sees_writes_made_before_cancel`,
guards the guarantee. Under Miri's weak-memory emulation a relaxed `Stopper`
fails it on most seeds, and CI's Miri job runs it over 8 seeds.

## What it costs

`cargo bench -p almost-enough --bench stopper_ordering` (zenbench 0.1.9,
paired and interleaved, gate disabled). Each cell is a 95% CI of the
difference from the relaxed flag: `Stopper`'s, except in the N1 default
column, which is the Acquire flag's (the same instructions, from the
four-variant run).

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

So:
- **x86-64:** free; both orderings are the same instructions.
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
- microcontrollers, where each check now executes a full barrier (below).

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

## Other targets

The ordering changes only the instructions emitted, never which targets
build. `enough` and `almost-enough` built with the same results before and
after this change for `thumbv6m-none-eabi`, `riscv32imc-unknown-none-elf`,
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
every check through a `Stopper`, `StopSource` or `ChildStopper` now ends in
a full barrier. No microcontroller was available to measure what that costs.
It is the price of the guarantee on multi-core parts (an RP2040 has two
Cortex-M0+ cores); the compiler can't tell a single-core target from them.

The N1 runs shared the box with three niced fuzzers (load average about 6).
The x86 runs started once its load average had fallen below 5.
