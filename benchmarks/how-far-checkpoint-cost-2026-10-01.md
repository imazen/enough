# how-far checkpoint and plan costs, counted with perf, 2026-10-01

- Command: `python3 dev/how-far-checkpoint-cost/measure.py`
- enough at `5bdfa84` (this repository); rustc 1.98.1, release profile, no
  `target-cpu=native`; AMD Ryzen 9 5900XT
- The machine was busy with other jobs (load average 2.2 to 2.6), so these are
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
| `pulse.step(n)` through `&dyn Pulse` | 62 | 18 |
| `pulse.live()` then `step(n)` | 60 | 17 |
| `Paced::step(n)`, reaching the tree every 64 KiB | 5 | 1 |

Unobserved (`NoPulse`), `live()` adds nothing (643,145 instructions against
643,117 with no checkpoints at all) and `Paced` adds 5 instructions per step.

So a checkpoint stays under 1% of the work when the work between two `Paced`
steps takes more than about 120 cycles: 256 bytes of this defilter, which runs
at about 0.3 cycles per byte. A plain `step` into a live tree needs about 2,000
cycles of work per call to stay under 1%.

Per operation, a three-stage `Stages` plan costs about 850 instructions with
`NoPulse` (one allocation for the children) and about 6,800 with a fresh live
tree per operation, mostly allocating named nodes. That is under 1% for
operations longer than about 5 µs and 70 µs respectively. Before finishing
became allocation-free (`5bdfa84`), the live tree cost about 16,100.

## Raw output

```text
rustc 1.98.1 (48a229cea 2026-09-01)

Per 256 KiB buffer, one checkpoint per 64 bytes (4096 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 643,117 | +0.00% | 118,054 | +0.00% | 117,572–121,979 |
| step-nopulse | 700,482 | +8.92% | 149,552 | +26.68% | 147,796–155,714 |
| live-nopulse | 643,145 | +0.00% | 119,543 | +1.26% | 116,880–122,682 |
| paced-nopulse | 663,661 | +3.19% | 126,347 | +7.02% | 121,810–128,988 |
| step-tree | 897,103 | +39.49% | 193,053 | +63.53% | 190,715–194,693 |
| live-tree | 888,932 | +38.22% | 188,664 | +59.81% | 186,219–190,166 |
| paced-tree | 663,994 | +3.25% | 122,591 | +3.84% | 122,207–126,192 |

Per 256 KiB buffer, one checkpoint per 256 bytes (1024 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 455,725 | +0.00% | 85,069 | +0.00% | 80,436–177,389 |
| step-nopulse | 470,082 | +3.15% | 89,959 | +5.75% | 87,877–95,834 |
| live-nopulse | 455,753 | +0.01% | 84,269 | -0.94% | 83,263–87,882 |
| paced-nopulse | 460,909 | +1.14% | 86,311 | +1.46% | 83,959–96,824 |
| step-tree | 519,247 | +13.94% | 103,731 | +21.94% | 102,453–109,548 |
| live-tree | 517,220 | +13.49% | 101,896 | +19.78% | 99,531–105,739 |
| paced-tree | 461,242 | +1.21% | 83,009 | -2.42% | 78,504–86,773 |

Per 256 KiB buffer, one checkpoint per 1024 bytes (256 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 408,877 | +0.00% | 76,168 | +0.00% | 73,357–80,599 |
| step-nopulse | 412,482 | +0.88% | 77,608 | +1.89% | 69,504–80,304 |
| live-nopulse | 408,905 | +0.01% | 74,035 | -2.80% | 71,540–77,941 |
| paced-nopulse | 410,221 | +0.33% | 75,708 | -0.60% | 72,864–76,198 |
| step-tree | 424,784 | +3.89% | 81,112 | +6.49% | 77,695–95,745 |
| live-tree | 424,292 | +3.77% | 88,757 | +16.53% | 77,638–142,163 |
| paced-tree | 410,554 | +0.41% | 75,649 | -0.68% | 73,638–98,865 |

Per 256 KiB buffer, one checkpoint per 4096 bytes (64 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 397,165 | +0.00% | 70,006 | +0.00% | 67,312–73,595 |
| step-nopulse | 398,082 | +0.23% | 70,750 | +1.06% | 66,181–76,408 |
| live-nopulse | 397,193 | +0.01% | 70,737 | +1.04% | 66,003–71,495 |
| paced-nopulse | 397,550 | +0.10% | 67,835 | -3.10% | 62,965–69,242 |
| step-tree | 401,167 | +1.01% | 71,318 | +1.87% | 65,708–72,261 |
| live-tree | 401,060 | +0.98% | 72,584 | +3.68% | 68,110–78,049 |
| paced-tree | 397,882 | +0.18% | 70,675 | +0.96% | 68,723–73,323 |

Per operation of 4 KiB, three stages, one checkpoint per 1024 bytes:

| Variant | Instructions | vs op-none | Cycles | vs op-none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| op-none | 6,536 | +0.00% | 1,627 | +0.00% | 1,585–1,749 |
| op-nopulse | 7,393 | +13.11% | 1,873 | +15.15% | 1,768–1,905 |
| op-tree | 13,328 | +103.92% | 3,930 | +141.56% | 3,821–4,150 |

Per operation of 256 KiB, three stages, one checkpoint per 1024 bytes:

| Variant | Instructions | vs op-none | Cycles | vs op-none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| op-none | 408,980 | +0.00% | 78,230 | +0.00% | 76,220–83,415 |
| op-nopulse | 411,181 | +0.54% | 77,645 | -0.75% | 77,026–80,630 |
| op-tree | 417,367 | +2.05% | 81,551 | +4.25% | 80,480–82,412 |
```
