# FnPulse: what one callback costs, 2026-10-03

- Commands: `python3 dev/how-far-checkpoint-cost/measure.py matrix 64,256,4096`,
  `python3 dev/how-far-checkpoint-cost/measure.py 64,256,1024,4096`, and
  `python3 dev/bench-how-far-build.py --runs 3`
- how-far at `fe9316a`; build cost compared with `9f2f7d7`, the commit before
  `FnPulse` and the implementor changes
- rustc 1.99.0, release profile, no `target-cpu=native`; AMD Ryzen 9 5900XT
- The machine was heavily loaded (load average about 27). Instruction counts
  are exact; cycles carry the other jobs' cache traffic.

`FnPulse`'s callback here is a cold function that does nothing, so these are
the costs of reaching a callback, not of what it does.

## Per checkpoint

Extra instructions per checkpoint, 64-byte chunks (4,096 per 256 KiB buffer):

| Pulse | `step` | `live()` | `Paced` |
| --- | ---: | ---: | ---: |
| `FnPulse`, no plan | 89 | 89 | 2.1 |
| `FnPulse` stage, exact total | 91 | 91 | 2.1 |
| `PulseTree`, `AtomicBool` stop | 56 | 56 | 2.1 |
| Shell: report callback and stop callback | 33 | 33 | 2.1 |

A report into an `FnPulse` counts the units, moves the job's fraction,
builds a `Progress`, and calls the boxed callback; with `step`, a check then
loads the latched verdict. That is about 35 instructions more than a live
tree's locked counter, and about 58 more than the shell, which hands two
boxed callbacks nothing but a number. Paced steps reach it once per 64 KiB,
so they cost the same 2 instructions as any pulse.

Per three-stage operation of 4 KiB, against the same work without a plan:

| Pulse | Paced | Stepping every chunk |
| --- | ---: | ---: |
| `NoPulse` | 882 | 819 |
| A fresh `FnPulse` | 3,763 | 3,954 |
| A fresh `PulseTree` | 6,976 | 7,044 |

An `FnPulse` plan allocates each phase's node, its `Child` box and two name
copies, and builds no snapshots, so it costs about half what a fresh tree
does. On 256 KiB operations stepped every 1 KiB, `FnPulse` adds 6.8%
instructions and a tree 5.2%; paced, 1.2% and 2.0%.

## Build cost

rustc instructions in millions, medians of three:

| | check | debug | release |
| --- | ---: | ---: | ---: |
| `how-far` at `9f2f7d7` | 162.3 | 306.5 | 402.9 |
| `how-far` at `fe9316a` | 232.8 | 498.1 | 702.8 |
| change | +43% | +63% | +74% |
| A `Stages::run_stoppable` call site, before | 5.4 | 10.8 | 70.1 |
| A `Stages::run_stoppable` call site, after | 5.4 | 10.8 | 70.3 |
| `how-far-along` (std), before | 286.7 | 791.6 | 1705.2 |
| `how-far-along` (std), after | 285.0 | 769.2 | 1705.0 |

Almost all of `how-far`'s growth is `FnPulse`: about 290 lines of
non-generic code with atomics, `u128` arithmetic, two `Arc` types and their
drop glue. It is compiled once per build, in parallel with a library's other
dependencies; nothing is compiled per call site, which costs the same as
before. `how-far-along` now calls `PhaseSpec::validate_split` instead of its
own copy, so its unoptimized IR shrank from 15,773 to 14,923 lines.

## Raw output

The matrix run keeps the per-checkpoint tables and the `&NoPulse` and
`FnPulse` lines; the percentage and cycle-spread tables are left out.

```text
rustc 1.99.0 (b940084d7 2026-09-28)
### One checkpoint per 64 bytes (4096 per 256 KiB)
No checkpoints at all: 471,085 instructions, 178,079 cycles per buffer.

`step`, extra per checkpoint, instructions / cycles:
| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 13.0 / 9.9 | 17.0 / 13.2 | 25.0 / 19.3 |
| tree counter | 41.0 / 22.7 | 45.0 / 20.5 | 53.0 / 33.1 |
| report callback | 21.0 / 14.5 | 25.0 / 14.0 | 33.0 / 20.7 |
| `PulseTree` | 52.0 / 26.6 | 56.0 / 26.9 | 64.0 / 33.7 |
`&NoPulse` (no stop, no report): -0.99 instructions, -0.11 cycles per checkpoint (-0.86% instructions).
`FnPulse`, no plan (one cold callback): 89.01 instructions, 47.19 cycles per checkpoint (+77.39% instructions).
`FnPulse` stage, exact total (one cold callback): 91.01 instructions, 44.03 cycles per checkpoint (+79.13% instructions).

`live`, extra per checkpoint, instructions / cycles:
| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | -1.0 / 0.0 | 17.0 / 10.3 | 25.0 / 16.7 |
| tree counter | 41.0 / 22.8 | 45.0 / 21.5 | 53.0 / 32.0 |
| report callback | 21.0 / 15.2 | 25.0 / 14.8 | 33.0 / 22.5 |
| `PulseTree` | 52.0 / 27.4 | 56.0 / 27.4 | 64.0 / 34.9 |
`&NoPulse` (no stop, no report): -0.99 instructions, -0.17 cycles per checkpoint (-0.86% instructions).
`FnPulse`, no plan (one cold callback): 89.01 instructions, 44.24 cycles per checkpoint (+77.40% instructions).
`FnPulse` stage, exact total (one cold callback): 91.01 instructions, 42.39 cycles per checkpoint (+79.13% instructions).

`paced`, extra per checkpoint, instructions / cycles:
| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 0.0 / -2.1 | 2.1 / 0.0 | 2.1 / -0.0 |
| tree counter | 2.1 / 0.1 | 2.1 / 0.0 | 2.1 / 0.1 |
| report callback | 2.1 / -0.0 | 2.1 / 0.0 | 2.1 / 0.0 |
| `PulseTree` | 2.1 / 0.1 | 2.1 / -0.1 | 2.1 / 0.2 |
`&NoPulse` (no stop, no report): 0.02 instructions, -2.16 cycles per checkpoint (+0.02% instructions).
`FnPulse`, no plan (one cold callback): 2.13 instructions, 0.27 cycles per checkpoint (+1.85% instructions).
`FnPulse` stage, exact total (one cold callback): 2.13 instructions, 0.05 cycles per checkpoint (+1.85% instructions).
### One checkpoint per 256 bytes (1024 per 256 KiB)
No checkpoints at all: 412,717 instructions, 149,240 cycles per buffer.

`step`, extra per checkpoint, instructions / cycles:
| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 13.0 / 9.9 | 17.0 / 10.7 | 25.0 / 16.9 |
| tree counter | 41.0 / 22.2 | 45.0 / 18.8 | 53.0 / 28.8 |
| report callback | 21.0 / 13.3 | 25.0 / 14.0 | 33.0 / 25.0 |
| `PulseTree` | 52.0 / 26.8 | 56.0 / 26.9 | 64.1 / 34.0 |
`&NoPulse` (no stop, no report): -0.96 instructions, -0.64 cycles per checkpoint (-0.24% instructions).
`FnPulse`, no plan (one cold callback): 89.05 instructions, 40.15 cycles per checkpoint (+22.09% instructions).
`FnPulse` stage, exact total (one cold callback): 91.04 instructions, 41.14 cycles per checkpoint (+22.59% instructions).

`live`, extra per checkpoint, instructions / cycles:
| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | -1.0 / 0.1 | 17.0 / 7.1 | 25.1 / 17.7 |
| tree counter | 41.1 / 19.3 | 45.0 / 18.1 | 53.1 / 29.8 |
| report callback | 21.0 / 12.2 | 25.0 / 12.7 | 33.1 / 19.0 |
| `PulseTree` | 52.1 / 23.3 | 56.1 / 24.6 | 64.1 / 32.1 |
`&NoPulse` (no stop, no report): -0.97 instructions, -0.34 cycles per checkpoint (-0.24% instructions).
`FnPulse`, no plan (one cold callback): 89.05 instructions, 41.21 cycles per checkpoint (+22.09% instructions).
`FnPulse` stage, exact total (one cold callback): 91.05 instructions, 39.13 cycles per checkpoint (+22.59% instructions).

`paced`, extra per checkpoint, instructions / cycles:
| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 0.1 / -2.3 | 2.2 / -0.2 | 2.3 / 0.7 |
| tree counter | 2.3 / 0.0 | 2.3 / -0.3 | 2.4 / 0.2 |
| report callback | 2.2 / 0.1 | 2.3 / 0.6 | 2.3 / 0.7 |
| `PulseTree` | 2.4 / -0.0 | 2.4 / 0.2 | 2.4 / 0.3 |
`&NoPulse` (no stop, no report): 0.07 instructions, -2.25 cycles per checkpoint (+0.02% instructions).
`FnPulse`, no plan (one cold callback): 2.51 instructions, 0.71 cycles per checkpoint (+0.62% instructions).
`FnPulse` stage, exact total (one cold callback): 2.52 instructions, 0.32 cycles per checkpoint (+0.62% instructions).
### One checkpoint per 4096 bytes (64 per 256 KiB)
No checkpoints at all: 394,477 instructions, 135,801 cycles per buffer.

`step`, extra per checkpoint, instructions / cycles:
| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 13.6 / 12.8 | 17.6 / 14.7 | 25.8 / 18.5 |
| tree counter | 41.6 / 29.4 | 45.6 / 20.3 | 53.8 / 36.3 |
| report callback | 21.6 / 19.9 | 25.6 / 9.1 | 33.8 / 14.9 |
| `PulseTree` | 52.7 / 32.1 | 56.8 / 36.0 | 64.9 / 34.8 |
`&NoPulse` (no stop, no report): -0.38 instructions, 8.67 cycles per checkpoint (-0.01% instructions).
`FnPulse`, no plan (one cold callback): 89.71 instructions, 51.32 cycles per checkpoint (+1.46% instructions).
`FnPulse` stage, exact total (one cold callback): 91.63 instructions, 49.13 cycles per checkpoint (+1.49% instructions).

`live`, extra per checkpoint, instructions / cycles:
| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | -0.4 / -1.9 | 17.7 / 8.2 | 25.9 / 21.1 |
| tree counter | 41.9 / 25.1 | 45.8 / 23.1 | 53.9 / 40.7 |
| report callback | 21.8 / 21.2 | 25.7 / 16.4 | 33.9 / 30.5 |
| `PulseTree` | 53.1 / 28.5 | 57.0 / 21.1 | 65.1 / 38.7 |
`&NoPulse` (no stop, no report): -0.51 instructions, 4.38 cycles per checkpoint (-0.01% instructions).
`FnPulse`, no plan (one cold callback): 89.82 instructions, 54.65 cycles per checkpoint (+1.46% instructions).
`FnPulse` stage, exact total (one cold callback): 91.74 instructions, 39.65 cycles per checkpoint (+1.49% instructions).

`paced`, extra per checkpoint, instructions / cycles:
| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 1.3 / 0.5 | 5.6 / 9.1 | 6.3 / 8.4 |
| tree counter | 7.3 / 7.5 | 7.4 / 5.8 | 8.0 / -0.4 |
| report callback | 5.9 / 4.1 | 6.2 / 7.8 | 6.8 / 10.4 |
| `PulseTree` | 8.2 / 7.1 | 8.3 / 11.1 | 9.0 / 17.1 |
`&NoPulse` (no stop, no report): 1.12 instructions, 4.97 cycles per checkpoint (+0.02% instructions).
`FnPulse`, no plan (one cold callback): 10.22 instructions, 14.95 cycles per checkpoint (+0.17% instructions).
`FnPulse` stage, exact total (one cold callback): 10.26 instructions, 12.93 cycles per checkpoint (+0.17% instructions).
```

```text
rustc 1.99.0 (b940084d7 2026-09-28)

Per 256 KiB buffer, one checkpoint per 64 bytes (4096 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 471,085 | +0.00% | 178,294 | +0.00% | 177,175–178,552 |
| step-nopulse | 467,020 | -0.86% | 178,016 | -0.16% | 177,484–178,738 |
| live-nopulse | 467,011 | -0.86% | 178,294 | +0.00% | 176,774–178,826 |
| paced-nopulse | 471,146 | +0.01% | 169,856 | -4.73% | 169,311–170,059 |
| step-tree | 700,505 | +48.70% | 289,699 | +62.48% | 289,321–295,107 |
| live-tree | 700,519 | +48.70% | 277,908 | +55.87% | 275,310–280,011 |
| paced-tree | 479,673 | +1.82% | 178,314 | +0.01% | 177,219–178,812 |

Per 256 KiB buffer, one checkpoint per 256 bytes (1024 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 412,717 | +0.00% | 149,087 | +0.00% | 148,240–149,804 |
| step-nopulse | 411,723 | -0.24% | 148,779 | -0.21% | 148,527–149,437 |
| live-nopulse | 411,715 | -0.24% | 148,797 | -0.19% | 148,538–149,876 |
| paced-nopulse | 412,779 | +0.01% | 146,703 | -1.60% | 146,370–146,847 |
| step-tree | 470,102 | +13.90% | 176,639 | +18.48% | 175,684–176,830 |
| live-tree | 470,117 | +13.91% | 173,558 | +16.41% | 172,857–173,891 |
| paced-tree | 415,161 | +0.59% | 149,483 | +0.27% | 149,336–149,576 |

Per 256 KiB buffer, one checkpoint per 1024 bytes (256 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 398,125 | +0.00% | 141,084 | +0.00% | 140,568–142,047 |
| step-nopulse | 397,900 | -0.06% | 140,963 | -0.09% | 140,473–141,479 |
| live-nopulse | 397,891 | -0.06% | 141,586 | +0.36% | 141,193–141,996 |
| paced-nopulse | 398,186 | +0.02% | 140,500 | -0.41% | 140,278–141,055 |
| step-tree | 412,501 | +3.61% | 148,145 | +5.01% | 147,931–149,563 |
| live-tree | 412,517 | +3.61% | 147,607 | +4.62% | 147,224–149,463 |
| paced-tree | 399,033 | +0.23% | 142,352 | +0.90% | 141,028–142,609 |

Per 256 KiB buffer, one checkpoint per 4096 bytes (64 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 394,477 | +0.00% | 136,104 | +0.00% | 135,170–136,326 |
| step-nopulse | 394,444 | -0.01% | 136,055 | -0.04% | 135,834–136,979 |
| live-nopulse | 394,435 | -0.01% | 136,246 | +0.10% | 135,015–136,302 |
| paced-nopulse | 394,538 | +0.02% | 135,785 | -0.23% | 135,430–136,087 |
| step-tree | 398,101 | +0.92% | 138,051 | +1.43% | 135,613–142,114 |
| live-tree | 398,116 | +0.92% | 137,808 | +1.25% | 136,423–138,513 |
| paced-tree | 395,001 | +0.13% | 136,624 | +0.38% | 136,330–139,218 |

Per operation of 4 KiB, three stages, one checkpoint per 1024 bytes:

| Variant | Instructions | vs op-none | Cycles | vs op-none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| op-none | 6,347 | +0.00% | 2,240 | +0.00% | 1,997–2,723 |
| op-nopulse | 7,229 | +13.90% | 3,196 | +42.67% | 2,704–3,335 |
| op-tree | 13,323 | +109.92% | 6,243 | +178.70% | 6,100–6,644 |
| op-fn | 10,110 | +59.28% | 4,048 | +80.69% | 3,907–4,701 |
| opstep-nopulse | 7,166 | +12.90% | 2,727 | +21.73% | 2,591–3,063 |
| opstep-tree | 13,391 | +110.97% | 6,073 | +171.10% | 5,985–7,009 |
| opstep-fn | 10,301 | +62.30% | 4,302 | +92.05% | 4,186–5,100 |

Per operation of 256 KiB, three stages, one checkpoint per 1024 bytes:

| Variant | Instructions | vs op-none | Cycles | vs op-none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| op-none | 398,207 | +0.00% | 143,261 | +0.00% | 142,571–155,785 |
| op-nopulse | 398,837 | +0.16% | 143,246 | -0.01% | 143,050–167,312 |
| op-tree | 406,157 | +2.00% | 148,685 | +3.79% | 148,653–173,341 |
| op-fn | 403,059 | +1.22% | 146,186 | +2.04% | 145,741–147,330 |
| opstep-nopulse | 398,773 | +0.14% | 143,014 | -0.17% | 142,953–144,665 |
| opstep-tree | 418,856 | +5.19% | 157,097 | +9.66% | 150,673–178,756 |
| opstep-fn | 425,099 | +6.75% | 158,174 | +10.41% | 157,801–159,010 |
```

```text
rustc 1.99.0 (b940084d7 2026-09-28)
how-far IR per Stages::run_stoppable call site: 64 lines (budget 120)
how-far-along IR, std: 15773 lines (budget 18000)
how-far-along IR, diagnostics: 43691 lines (budget 55000)
rustc instructions (millions)                  check   debug release
empty no_std crate                              13.8    16.1    19.0
how-far                                        162.3   306.5   402.9
how-far-along (std)                            286.7   791.6  1705.2
how-far-along (diagnostics)                    700.4  2107.0  5425.3
plain function call, per call site               4.4     7.7    57.1
Stages::run_stoppable, per call site             5.4    10.8    70.1
```

```text
rustc 1.99.0 (b940084d7 2026-09-28)
how-far IR per Stages::run_stoppable call site: 64 lines (budget 120)
how-far-along IR, std: 14923 lines (budget 18000)
how-far-along IR, diagnostics: 42841 lines (budget 55000)
rustc instructions (millions)                  check   debug release
empty no_std crate                              13.8    16.1    19.0
how-far                                        232.8   498.1   702.8
how-far-along (std)                            285.0   769.2  1705.0
how-far-along (diagnostics)                    698.8  2075.1  5425.8
plain function call, per call site               4.4     7.7    57.2
Stages::run_stoppable, per call site             5.4    10.8    70.3
```
