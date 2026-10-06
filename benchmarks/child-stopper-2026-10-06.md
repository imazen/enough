# `ChildStopper` without a vtable call per level (2026-10-06)

Evidence for the change that walks `ChildStopper` parents directly. Each node
has a state byte (running, cancelled, or "check the stop above"), a nullable
parent pointer, and a `StopToken` for any non-`ChildStopper` parent; the first
level is peeled out of the walk.

## Instructions executed on a root's path

rustc 1.99.0, `-O`, `--emit asm`, `main` against the change:

| | main x86-64 | change x86-64 | main aarch64 | change aarch64 |
| --- | ---: | ---: | ---: | ---: |
| root `check()` | 9 | 9 | 8 | 8 |
| root `should_stop()` | 10 | 10 | 8 | 8 |
| depth 1 under a `Stopper`, `check()` | 18 | 18 | | |

Each `ChildStopper` level above costs a state load, a test, a pointer load
and a test. Only the path that returns "cancelled" is longer, by one byte
compare; it runs once.

## Time

`cargo bench -p almost-enough --bench child_stopper`, same file in both
builds. Two binaries lay code out differently, so compare each column against
its own `Stopper` row.

Neoverse-N1 (`zen-arm-xl`, rustc 1.97.1, `-C target-cpu=generic`):

| | main | change |
| --- | ---: | ---: |
| `Stopper` through `&dyn Stop` | 1.98 ns | 1.77 ns |
| `&dyn Stop`, depth 1 / 2 / 4 / 8 | 2.56 / 5.01 / 12.87 / 34.32 ns | 2.08 / 2.65 / 4.55 / 8.57 ns |
| `&dyn Stop`, depth 4 under a `Stopper` | 16.71 ns | 5.54 ns |
| `Stopper`, generic | 1.02 ns | 0.68 ns |
| generic, depth 1 / 2 / 4 / 8 | 1.71 / 4.46 / 12.30 / 34.70 ns | 1.36 / 2.05 / 3.98 / 8.54 ns |
| 1 KiB of work per check, depth 2 / 8, over a `Stopper` | +1.2–1.8% / +13.2–13.7% | −0.3–+0.5% / +2.8–3.5% |

x86-64 (`r5900xt`, Ryzen 9 5900XT, rustc 1.99.0, load average about 6), each
row as the overhead over that run's `Stopper`:

| | main | change |
| --- | ---: | ---: |
| generic, depth 1 / 8 | +54% / +1790% | +42% / +150% |
| `&dyn Stop`, depth 1 / 8 | +101% / +1400% | +79% / +190% |
| 1 KiB of work per check, depth 2 / 8 | +1.6–1.9% / +18.0–18.5% | +1.2–1.6% / +3.7–4.1% |
