# `DebouncedTimeout`: how late it stops, and what a check costs (2026-10-06)

`DebouncedTimeout` reads the clock every N checks, choosing N so that reads
come about once per target interval (100 µs). On `main` (`6bd6b96`), N had no
upper bound, and a slowdown was noticed only at the next clock read. This
change caps N at 64 and replaces the `count % N` division with a countdown.

Measured on `r5900xt` (AMD Ryzen 9 5900XT, Zen 3, native Ubuntu), rustc 1.99.0, release build.

## How late it stops after checks slow down

Check as fast as possible for 20 ms, then make every check take a fixed time
(a spin), with the deadline 50 ms after creation:

| check time after the slowdown | `main`: checks per clock read | `main`: stopped late by | capped: checks per clock read | capped: stopped late by |
| ---: | ---: | ---: | ---: | ---: |
| 10 µs | 92,286 | 139.7 ms | 64 | 0.0 ms |
| 100 µs | 50,000 | 1,226.1 ms | 64 | 0.0 ms |
| 1 ms | 94,093 | 59,930 ms (and counting: stopped by the 60 s guard) | 64 | 0.0 ms |

The capped version can still stop up to 64 slow checks late, if the slowdown
lands right after a clock read and the deadline passes before the next. In
these runs an earlier read had already recalibrated it. The regression test
`a_slowdown_after_calibration_stops_within_64_slow_checks` asserts the bound.

## Cost per check

Method: the stop half of `dev/how-far-checkpoint-cost` from PR #27
(`measure.py stop CHUNK`), built against this branch and against `main`. An
`#[inline(never)]` PNG Sub defilter runs over a 256 KiB buffer, with a check
after every CHUNK bytes. The counts are the slope between 200 and 1,000
iterations, taking the median of five interleaved runs.

Extra instructions per buffer:

| | CHUNK | `&dyn Stop` | generic | `should_stop()` | 4 workers |
| --- | ---: | ---: | ---: | ---: | ---: |
| `Stopper` | 1 KiB | 2,582 | 528 | 2,329 | 219 |
| `WithTimeout<Stopper>` | 1 KiB | 28,694 | 25,624 | 28,441 | 26,310 |
| `DebouncedTimeout<Stopper>`, `main` | 1 KiB | 6,034 | 3,724 | 6,012 | 4,109 |
| `DebouncedTimeout<Stopper>`, capped | 1 KiB | 4,726 | 2,926 | 4,953 | 2,391 |
| `Stopper` | 64 B | 40,981 | 8,207 | 36,889 | 38,648 |
| `WithTimeout<Stopper>` | 64 B | 458,777 | 409,626 | 454,683 | 456,491 |
| `DebouncedTimeout<Stopper>`, `main` | 64 B | 94,371 | 57,488 | 94,348 | 92,674 |
| `DebouncedTimeout<Stopper>`, capped | 64 B | 75,286 | 46,486 | 78,872 | 73,480 |

So the instruction count per check drops from about 23 to 18 through
`&dyn Stop`.

Extra cycles per buffer at 64 B per check (4,096 checks, about 22 cycles of
work each). At 1 KiB the cycle differences are within the box's noise.

| | `&dyn Stop` | generic | `should_stop()` | 4 workers |
| --- | ---: | ---: | ---: | ---: |
| `Stopper` | 15,507 (+16.5%) | -2,003 (-2.1%) | 9,321 (+9.9%) | 14,303 (+7.2%) |
| `WithTimeout<Stopper>`, `main` | 347,536 (+381.2%) | 343,527 (+376.8%) | 351,110 (+385.1%) | 338,724 (+157.1%) |
| `DebouncedTimeout<Stopper>`, `main` | 25,145 (+27.6%) | 7,549 (+8.3%) | 25,140 (+27.6%) | 53,164 (+24.7%) |
| `DebouncedTimeout<Stopper>`, capped | 33,235 (+35.3%) | 9,794 (+10.4%) | 33,602 (+35.7%) | 45,449 (+22.7%) |

Reading the clock every 64 checks costs about 2 cycles per check in this
extreme loop. That is the price of the bound. A cap of 256 costs about 0.7
cycles per check but allows up to 256 slow checks of lateness (128 ms at
1 ms per check in the same test).
