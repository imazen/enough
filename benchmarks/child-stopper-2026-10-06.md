# `ChildStopper` without a vtable call per level (2026-10-06)

Evidence for the change that walks `ChildStopper` parents directly. Each node
has a state byte (running, cancelled, or "check the stop above"), a nullable
parent pointer, and a `StopToken` for any non-`ChildStopper` parent; the first
level is peeled out of the walk.

## Instructions executed per check

rustc 1.99.0 on `r5900xt`, release, `--emit asm`, for x86-64 and for
`aarch64-unknown-linux-gnu` (generic CPU), compiling `check()` and
`should_stop()` of a `&ChildStopper` in `#[inline(never)]` functions. `main`
is `5d53d06`; the change is #38 at `b57d603` (the bit test). Counted along
each path to its return, or to the indirect jump into a parent's vtable:

| path | main x86-64 | change x86-64 | main aarch64 | change aarch64 |
| --- | ---: | ---: | ---: | ---: |
| root, `check()` | 9 | 9 | 8 | 7 |
| root, `should_stop()` | 10 | 9 | 8 | 7 |
| child of a `Stopper`, `check()` | 18 | 17 | 15 | 14 |
| child of a `Stopper`, `should_stop()` | 17 | 17 | 14 | 13 |
| child of a vtable parent (`SyncStopper`, `WithTimeout`, ...), `check()` | 21 | 20 | 18 | 17 |
| child of a vtable parent, `should_stop()` | 21 | 21 | 19 | 18 |
| cancelled node, `check()` or `should_stop()` | 6 | 8 | 5 | 6 |

Each `ChildStopper` level above costs a state load, a test, a pointer load
and a test. Only the path that returns "cancelled" is longer; it runs once.
With a `match` on the state instead of the bit test (#38 at `61b6506`), a
child of a `Stopper` took 18 instructions in `should_stop()` on x86-64.

Each node's allocation grows by 8 bytes: `size_of::<TreeInner>()` is 32 on
`main` and 40 with the change (x86-64), so with the `Arc` counts 48 and 56.

## Time

`cargo bench -p almost-enough --bench child_stopper`, same file in both
builds. Two binaries lay code out differently, so compare each column against
its own `Stopper` row. These ran before #35 and #36 merged: "main" is
`ae5d376`, where a `ChildStopper` held its parent as a `BoxedStop` and
`StopToken` had four arms, and "change" is #38's `tree.rs` as of `1b3519a`
(the `match` form).

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
