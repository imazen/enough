# `TokioStop`: check cost, size, and creation-heavy use (2026-10-06)

`TokioStop::check` used to call `CancellationToken::is_cancelled`, which locks
the token's mutex (tokio-util 0.7.19, `tree_node::is_cancelled`). It now
counts its checks. After 32 of them it registers a waker with the token,
and from then on it reads the flag that waker sets on cancellation.

PR #28's first version registered eagerly in `new`. That made checks
cheapest, but it regressed the common server pattern: many short-lived stops
on one shutdown token. Both versions are measured here against `main`
(`6bd6b96`).

All numbers are from `r5900xt` (AMD Ryzen 9 5900XT, Zen 3, native Ubuntu,
Linux 7.0) with rustc 1.99.0 and `perf stat -e instructions:u,cycles:u`.
Instructions are deterministic; cycles vary with load, so they are compared
only within one run.

## Size and memory per live stop

| | `main` | eager | lazy (this PR) |
| --- | ---: | ---: | ---: |
| `size_of::<TokioStop>()` | 8 | 16 | 40 |
| heap per stop, never checked | 0 | 3 allocations, 128 B | 0 |
| heap per stop, registered | — | 3 allocations, 128 B | 2 allocations, 96 B |
| max RSS, 1M stops never checked | 9.9 MB | 173 MB | 41 MB |
| max RSS, 1M registered stops | — | 174 MB | 150 MB |

Per-stop heap is DHAT's bytes per stop at 1,000 vs 2,000 live stops, less
the stop's slot in the holding `Vec`. A registration is a 1-byte flag in an
`Arc` and the boxed 72-byte `WaitForCancellationFutureOwned`. Peak memory is
from `/usr/bin/time -v`; with no stops it is 2.3 MB.

## Creating a stop

Per stop, cloning a shared token into a new `TokioStop`, then K checks, then
drop. Values are instructions / cycles, the median of five slopes between
20,000 and 120,000 stops:

| K | `main` | eager | lazy |
| ---: | ---: | ---: | ---: |
| 0 | 102 / 47 | 971 / 346 | 127 / 64 |
| 1 | 156 / 64 | 989 / 386 | 195 / 82 |
| 10 | 588 / 211 | 1,106 / 372 | 789 / 227 |
| 32 | 1,644 / 564 | 1,392 / 417 | 3,066 / 916 |
| 100 | 4,908 / 1,666 | 2,276 / 611 | 4,154 / 1,188 |
| 1,000 | 48,108 / 16,165 | 13,976 / 2,793 | 18,554 / 6,258 |
| clone + drop | 100 / 48 | 18 / 32 | 129 / 78 |

Before it registers, a lazy stop's check costs about 66 instructions (`main`:
48); the extra is the counting. Registering costs about 830 instructions,
about 18 of `main`'s checks. Once registered, a check costs 16 instructions.

## Many threads creating stops on one token

Each thread creates a stop from the shared token, checks it once and drops
it, 200,000 times. ns per stop per thread, three runs:

| threads | `main` | eager | lazy |
| ---: | ---: | ---: | ---: |
| 1 | 16–17 | 83–92 | 20 |
| 8 | 1,102–1,202 | 4,386–5,048 | 1,131–1,377 |
| 16 | 2,601–2,658 | 12,456–12,504 | 3,022–3,074 |

Eager registration took about three times as many locks on the token's shared
mutexes per stop. A lazy stop that is never checked often takes the same
locks as `main`.

## `cancel()` latency

`token.cancel()` timed with N live stops (µs, three runs):

| N | `main` | eager | lazy, never checked | lazy, all registered |
| ---: | ---: | ---: | ---: | ---: |
| 1,000 | 0.3–0.8 | 5.4–6.6 | 0.3 | 4.7–5.8 |
| 100,000 | 0.3 | 730–1,240 | 0.3–0.6 | 626–864 |
| 1,000,000 | 0.3 | 14,111–14,664 | 0.7–0.9 | 9,539–9,794 |

`cancel` wakes every registered stop before it returns. Lazily, only stops
checked more than 32 times register: typically the few that are in a loop on
a worker when the cancel comes.

## Check cost, steady state

Method: the stop half of imazen/how-far's `dev/how-far-checkpoint-cost` (then on PR #27)
(`measure.py stop CHUNK`), built against this branch, with `main`'s version
added as a second row. Each cell runs an `#[inline(never)]` PNG Sub defilter
over a 256 KiB buffer with a check after every CHUNK bytes, against the same
work without checks. One stop is reused across iterations, so it is
registered. Counts are the slope between 200 and 1,000 iterations, median of
five interleaved runs.

Extra instructions per buffer, 1 KiB per check (256 checks):

| | `&dyn Stop`, `check()?` | generic `impl Stop` | `should_stop()` | `may_stop()` hoisted | every 16th chunk | 4 workers share it |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `Stopper` | 2,582 (+0.6%) | 528 (+0.1%) | 2,329 (+0.6%) | 2,594 (+0.7%) | 1,482 (+0.4%) | 257 (+0.1%) |
| `TokioStop` on `main` | 12,054 (+3.0%) | 10,256 (+2.6%) | 10,777 (+2.7%) | 12,066 (+3.0%) | 2,074 (+0.5%) | 9,721 (+2.2%) |
| `TokioStop`, this PR | 3,861 (+1.0%) | 1,559 (+0.4%) | 3,353 (+0.8%) | 3,874 (+1.0%) | 1,562 (+0.4%) | 1,604 (+0.4%) |

64 bytes per check (4,096 checks), extra instructions:

| | `&dyn Stop`, `check()?` | generic `impl Stop` | `should_stop()` | `may_stop()` hoisted | every 16th chunk | 4 workers share it |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `Stopper` | 40,982 (+8.7%) | 8,207 (+1.7%) | 36,889 (+7.8%) | 40,993 (+8.7%) | 23,321 (+5.0%) | 38,664 (+7.6%) |
| `TokioStop` on `main` | 192,534 (+40.9%) | 163,856 (+34.8%) | 172,057 (+36.5%) | 192,546 (+40.9%) | 32,793 (+7.0%) | 194,098 (+38.4%) |
| `TokioStop`, this PR | 61,461 (+13.0%) | 24,598 (+5.2%) | 53,273 (+11.3%) | 61,474 (+13.0%) | 24,602 (+5.2%) | 59,248 (+11.7%) |

64 bytes per check, extra cycles:

| | `&dyn Stop`, `check()?` | generic `impl Stop` | `should_stop()` | `may_stop()` hoisted | every 16th chunk | 4 workers share it |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `Stopper` | 13,013 (+8.6%) | -4,038 (-2.7%) | 13,685 (+9.1%) | 9,662 (+6.4%) | 1,863 (+1.2%) | 9,698 (+4.0%) |
| `TokioStop` on `main` | 39,381 (+26.0%) | 22,319 (+14.8%) | 26,947 (+17.8%) | 36,501 (+24.1%) | 3,918 (+2.6%) | 184,777 (+76.4%) |
| `TokioStop`, this PR | 23,273 (+15.4%) | 117 (+0.1%) | 23,441 (+15.5%) | 17,497 (+11.6%) | 2,027 (+1.3%) | 23,051 (+9.5%) |

Through `&dyn Stop`, a registered check costs 15 instructions against 47 on
`main` and 10 for `Stopper`. The eager version measured 9; the difference is
the lazy cell's check that the registration exists. With four workers on one
token, `main`'s mutex added 76% to the cycles; the flag adds 9.5%.
