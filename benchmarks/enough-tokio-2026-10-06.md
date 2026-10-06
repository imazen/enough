# `TokioStop` check cost, before and after the cached flag (2026-10-06)

`TokioStop::check` used to call `CancellationToken::is_cancelled`, which locks
the token's mutex (tokio-util 0.7.19, `tree_node::is_cancelled`). It now reads
a flag that a waker registered with the token sets on cancellation.

Method: the stop half of `dev/how-far-checkpoint-cost` from PR #27
(`measure.py stop CHUNK`), built against this branch, with the version on
`main` (`6bd6b96`) added as a second row. Each cell runs an `#[inline(never)]`
PNG Sub defilter over a 256 KiB buffer and checks after every CHUNK bytes. It
is compared with the same work done without checks. The counts are the slope
between 200 and 1,000 iterations, taking the median of five interleaved runs.
Measured with `perf stat -e instructions:u,cycles:u` on x86-64 (Ryzen 9
9950X3D, WSL2), rustc 1.99.0.

Instructions are deterministic. At 1 KiB the cycle counts are within the
box's noise (about ±2%), so only the 64-byte table shows them.

## Extra instructions per 256 KiB buffer, 1 KiB per check (256 checks)

| | `&dyn Stop`, `check()?` | generic `impl Stop` | `should_stop()` | `may_stop()` hoisted | every 16th chunk | 4 workers share it |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `Stopper` | 2,324 (+0.6%) | 270 (+0.1%) | 2,071 (+0.5%) | 2,336 (+0.6%) | 1,224 (+0.3%) | 228 (+0.1%) |
| `TokioStop` on `main` | 11,796 (+3.0%) | 9,998 (+2.5%) | 10,519 (+2.6%) | 11,808 (+3.0%) | 1,816 (+0.5%) | 9,720 (+2.2%) |
| `TokioStop` (this change) | 2,324 (+0.6%) | 278 (+0.1%) | 2,327 (+0.6%) | 2,336 (+0.6%) | 1,224 (+0.3%) | 239 (+0.1%) |

Per check, through `&dyn Stop`, that is 46 instructions before and 9 after,
the same as `Stopper`. In generic code it is 39 before and 1 after.

## 64 bytes per check (4,096 checks)

Extra instructions:

| | `&dyn Stop`, `check()?` | generic `impl Stop` | `should_stop()` | `may_stop()` hoisted | every 16th chunk | 4 workers share it |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `Stopper` | 36,884 (+7.8%) | 4,109 (+0.9%) | 32,791 (+6.9%) | 36,896 (+7.8%) | 19,224 (+4.0%) | 38,647 (+7.6%) |
| `TokioStop` on `main` | 188,436 (+39.7%) | 159,758 (+33.6%) | 167,959 (+35.3%) | 188,448 (+39.7%) | 28,696 (+6.0%) | 191,110 (+37.8%) |
| `TokioStop` (this change) | 36,884 (+7.8%) | 4,116 (+0.9%) | 36,887 (+7.8%) | 36,896 (+7.8%) | 19,224 (+4.0%) | 38,648 (+7.6%) |

Extra cycles:

| | `&dyn Stop`, `check()?` | generic `impl Stop` | `should_stop()` | `may_stop()` hoisted | every 16th chunk | 4 workers share it |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `Stopper` | 12,480 (+13.7%) | -4,716 (-5.2%) | 16,462 (+18.1%) | 12,735 (+14.0%) | 5,502 (+6.1%) | 3,857 (+2.0%) |
| `TokioStop` on `main` | 45,411 (+50.0%) | 36,665 (+40.4%) | 38,078 (+41.9%) | 45,877 (+50.5%) | 6,976 (+7.7%) | 86,218 (+43.8%) |
| `TokioStop` (this change) | 12,032 (+13.2%) | 1,984 (+2.2%) | 16,398 (+18.0%) | 12,255 (+13.5%) | 7,136 (+7.9%) | 4,985 (+2.5%) |

With four workers checking one token, the mutex on `main` cost 44% more
cycles. The flag costs what `Stopper` costs.

## Construction

`new` now makes three allocations and registers the waker. A loop that clones
a token, wraps it, checks once and drops it measures 997 instructions per
iteration. The same loop with only `token.clone()`, `is_cancelled()` and the
drop measures 162, and the empty loop 16 (slope between 1,000 and 11,000
iterations). Construction therefore costs about as much as 25 checks did on
`main`.
