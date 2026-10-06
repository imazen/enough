# Release/Acquire for `Stopper`: when it matters, what it costs (2026-10-06)

`Stopper` used Relaxed ordering until 0.4.5; `SyncStopper` was the same flag
with Release/Acquire. 0.4.5 gives `Stopper` (and `StopSource`, `ChildStopper`
and the FFI flag) Release/Acquire, and makes `SyncStopper` a deprecated
wrapper. This record is the evidence.

The harness compares a local relaxed flag against `Stopper`. The relaxed
flag is laid out as the old `Stopper` was: an `Arc` holding an `AtomicBool`.
Hosts:
- `r5900xt`: Ryzen 9 5900XT (x86-64, Zen 3), native Ubuntu, rustc 1.99.0;
- `zen-arm-xl`: Ampere Altra (Neoverse-N1), rustc 1.97.1;
- `mac`: Apple M4 Pro, rustc 1.99.0.

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
| Neoverse-N1 | 11,280,809 in 50,000,000 (23%) | 0 in 50,000,000 |
| Apple M4 Pro | 3,754 in 50,000,000 (1 in 13,300) | 0 in 50,000,000 |

An earlier run, of `Stopper` while it was Relaxed against `SyncStopper`,
read stale 10,483,993 times on N1 and 4,761 times on the M4 Pro, again
50,000,000 rounds each; `SyncStopper` never did. x86-64 can't reorder loads
with loads or stores with stores, so neither ordering can read stale there.

The N1 disassembly shows the reader reloading the value after the flag loop:
a plain `ldrb` for the relaxed flag, `ldaprb` (an acquire load) for the
ordered one. So the stale reads are the hardware's, not the compiler's.

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
paired and interleaved, gate disabled). `Stopper` against the relaxed flag,
as a 95% CI of the difference.

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

The walks carry their position across rounds and differ between the two
variants: each round reaches new lines, and neither variant warms the
other's. Hops cost 147 ns on N1, 108 ns on the M4 Pro and 106 ns on x86-64.

| | x86-64 | Neoverse-N1 | Apple M4 Pro |
| --- | ---: | ---: | ---: |
| check through `&dyn Stop` | −33%\* | [−3.5%, +4.9%] | [−0.8%, +0.9%] |
| check, generic | [−0.1%, +0.4%] | [−0.3%, +0.2%] | [−0.1%, +0.5%] |
| defilter, check every 64 B | +15%\* | [+0.0%, +1.0%] | [−0.0%, +0.0%] |
| defilter, check every 1 KiB | −2.7%\* | [−0.6%, +0.0%] | [−0.0%, +0.3%] |
| gather, check every 8 loads | [−0.2%, +0.4%] | [−1.4%, +0.1%] | [−0.1%, +0.2%] |
| gather, check every 64 loads | [−0.3%, +0.4%] | [−0.8%, +0.7%] | [−0.0%, +0.1%] |
| gather every 8, stop's line contended | [−0.8%, +0.8%] | [−0.8%, +0.5%] | [−0.1%, +0.3%] |
| chase, check every 4 hops | [−0.3%, +0.3%] | [−0.6%, +0.3%] | [−0.0%, +0.3%] |
| chase, check every 64 hops | [−0.4%, +0.0%] | [−0.4%, +1.5%] | [−0.1%, +0.1%] |

\* On x86-64 both orderings compile to plain `mov`s, so these can't come from
the ordering. They come from how each type's code is laid out and inlined,
and they flip sign between rows. Earlier runs on N1 of the 64 B defilter row
measured [+2.2%, +3.1%] and [−1.1%, +0.2%], and the same effects may be
behind them.

So nothing measurable, compute-bound or memory-bound. On aarch64 an Acquire
load is one-way ordering on the flag's own load (`ldapr`), not a fence, and
it is one instruction more per check through `&dyn Stop` (perf on N1: it
takes no address offset). On x86-64 it is free.

Not measured:
- cores without RCpc, where Rust emits the stronger `ldar`;
- in-order cores, such as Cortex-A55;
- microcontrollers, where each check now executes a full barrier (below).

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

The N1 runs shared the box with three niced fuzzers. The x86 runs started
once its load average had fallen to 3.
