# how-far checkpoint and plan costs, counted with perf, 2026-10-01

- Command: `python3 dev/how-far-checkpoint-cost/measure.py`
- enough at `d587e13` (this repository); rustc 1.98.1, release profile, no
  `target-cpu=native`; AMD Ryzen 9 5900XT
- The machine was busy with other jobs (load average 1.7 to 2.3), so these are
  perf counts, not wall time. Instructions are deterministic. `cycles:u` are
  counted only while the process runs, which keeps other jobs' scheduling out
  but not their cache and memory traffic: read cycle differences against the
  spread column. Each figure is the slope between 200 and 1000 iterations
  (removing process startup), median of five.

Every variant calls the same `#[inline(never)]` defilter, so all run the same
hot loop at the same address and differ only in their checkpoint code. An
earlier draft inlined the defilter into each variant: the `live()` loops then
ran the same instructions as the no-checkpoint loop but took up to twice the
cycles, a code-layout effect rather than a cost of `live()`.

## Summary

Per checkpoint, into a live `PulseTree` with a `Stopper`:

| Checkpoint | Extra instructions | Extra cycles |
| --- | ---: | ---: |
| `pulse.step(n)` through `&dyn Pulse` | 58 | 16 |
| `pulse.live()` then `step(n)` | 56 | 14 |
| `stage.step(n)` inside `Stages::run_stoppable` | 47 | – |
| `Paced::step(n)`, reaching the tree every 64 KiB | 5 | 0 to 1 |

Unobserved (`NoPulse`), `live()` adds nothing (643,145 instructions against
643,116 with no checkpoints at all) and `Paced` adds 5 instructions per step.
The step through a stage is the slope between `opstep-tree` and `op-tree`
below; its cycles are within noise of the plain `step`.

So a checkpoint stays under 1% of the work when the work between two `Paced`
steps takes more than about 120 cycles: 256 bytes of this defilter, which runs
at about 0.3 cycles per byte. A plain `step` into a live tree needs about
1,600 cycles of work per call.

Per operation, a three-stage `Stages` plan costs about 850 instructions with
`NoPulse` (one allocation for the children) and about 6,800 with a fresh live
tree per operation, mostly allocating named nodes: under 1% for operations
longer than about 5 µs and 70 µs respectively.

How these moved during this change, in instructions:

| | Before | After | Commit |
| --- | ---: | ---: | --- |
| Checkpoint with `Paced` instead of `step` | 62 | 5 | `d3797cf` |
| Live tree, three-stage operation | 16,100 | 6,800 | `5bdfa84` |
| Step through a stage (one vtable instead of two) | 60 | 50 | `c398ed3` |
| Report to a counting phase (one locked operation) | 62 / 50 | 58 / 47 | `d587e13` |

## Raw output

```text
rustc 1.98.1 (48a229cea 2026-09-01)

Per 256 KiB buffer, one checkpoint per 64 bytes (4096 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 643,116 | +0.00% | 124,253 | +0.00% | 122,227–127,475 |
| step-nopulse | 700,484 | +8.92% | 141,042 | +13.51% | 133,450–144,011 |
| live-nopulse | 643,147 | +0.00% | 124,700 | +0.36% | 123,122–127,684 |
| paced-nopulse | 663,664 | +3.19% | 124,226 | -0.02% | 121,897–132,660 |
| step-tree | 880,721 | +36.95% | 191,550 | +54.16% | 187,616–196,319 |
| live-tree | 872,550 | +35.68% | 179,878 | +44.77% | 178,745–180,748 |
| paced-tree | 663,982 | +3.24% | 123,696 | -0.45% | 121,998–127,172 |

Per 256 KiB buffer, one checkpoint per 256 bytes (1024 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 455,725 | +0.00% | 84,752 | +0.00% | 83,317–86,958 |
| step-nopulse | 470,084 | +3.15% | 88,430 | +4.34% | 85,874–94,594 |
| live-nopulse | 455,755 | +0.01% | 84,485 | -0.31% | 81,477–86,799 |
| paced-nopulse | 460,912 | +1.14% | 83,266 | -1.75% | 81,236–87,972 |
| step-tree | 515,153 | +13.04% | 98,250 | +15.93% | 95,428–98,809 |
| live-tree | 513,126 | +12.60% | 98,199 | +15.87% | 95,497–101,205 |
| paced-tree | 461,229 | +1.21% | 84,906 | +0.18% | 77,979–88,709 |

Per 256 KiB buffer, one checkpoint per 1024 bytes (256 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 408,877 | +0.00% | 76,067 | +0.00% | 74,458–78,789 |
| step-nopulse | 412,484 | +0.88% | 75,574 | -0.65% | 75,462–77,848 |
| live-nopulse | 408,907 | +0.01% | 75,725 | -0.45% | 71,755–79,874 |
| paced-nopulse | 410,224 | +0.33% | 76,152 | +0.11% | 72,713–80,033 |
| step-tree | 423,761 | +3.64% | 82,247 | +8.12% | 78,962–85,332 |
| live-tree | 423,270 | +3.52% | 80,305 | +5.57% | 77,405–82,692 |
| paced-tree | 410,542 | +0.41% | 76,920 | +1.12% | 73,856–77,359 |

Per 256 KiB buffer, one checkpoint per 4096 bytes (64 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 397,165 | +0.00% | 68,887 | +0.00% | 67,269–74,476 |
| step-nopulse | 398,084 | +0.23% | 70,374 | +2.16% | 64,643–76,103 |
| live-nopulse | 397,195 | +0.01% | 70,586 | +2.47% | 67,489–71,634 |
| paced-nopulse | 397,552 | +0.10% | 71,949 | +4.44% | 65,426–74,604 |
| step-tree | 400,913 | +0.94% | 70,382 | +2.17% | 69,718–75,821 |
| live-tree | 400,806 | +0.92% | 69,412 | +0.76% | 67,604–70,640 |
| paced-tree | 397,869 | +0.18% | 70,290 | +2.04% | 66,219–71,373 |

Per operation of 4 KiB, three stages, one checkpoint per 1024 bytes:

| Variant | Instructions | vs op-none | Cycles | vs op-none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| op-none | 6,512 | +0.00% | 1,808 | +0.00% | 1,669–1,920 |
| op-nopulse | 7,360 | +13.03% | 2,134 | +18.05% | 1,932–2,212 |
| op-tree | 13,318 | +104.52% | 3,966 | +119.38% | 3,901–4,223 |
| opstep-nopulse | 7,306 | +12.20% | 1,952 | +7.96% | 1,818–2,173 |
| opstep-tree | 13,387 | +105.59% | 4,073 | +125.30% | 3,886–4,215 |

Per operation of 256 KiB, three stages, one checkpoint per 1024 bytes:

| Variant | Instructions | vs op-none | Cycles | vs op-none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| op-none | 408,955 | +0.00% | 78,137 | +0.00% | 76,111–81,991 |
| op-nopulse | 411,485 | +0.62% | 78,624 | +0.62% | 76,931–81,582 |
| op-tree | 417,653 | +2.13% | 82,753 | +5.91% | 79,655–89,946 |
| opstep-nopulse | 413,026 | +1.00% | 80,469 | +2.98% | 76,020–84,229 |
| opstep-tree | 429,690 | +5.07% | 88,892 | +13.77% | 84,806–92,400 |
```
