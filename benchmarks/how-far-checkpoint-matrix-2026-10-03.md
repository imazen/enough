# Checkpoint cost by report sink and stop policy, 2026-10-03

- Command: `python3 dev/how-far-checkpoint-cost/measure.py matrix 64,256,4096`
- rustc 1.99.0, release profile, no `target-cpu=native`; AMD Ryzen 9 5900XT
- The machine was heavily loaded (load average 25 to 30). Instruction counts
  are exact and are the numbers to rely on. Cycles were inflated by the other
  jobs' cache and memory traffic; at 4 KiB, where only 64 checkpoints share
  each buffer, the per-checkpoint cycle figures are noise.
- The compiler changed since the 2026-10-01 runs (1.98.1), and with it the
  defilter itself, so compare within this file only.

Every variant is the same library loop over a 256 KiB PNG Sub defilter,
calling one `#[inline(never)]` work function per chunk, through the same
`&dyn Pulse` shell. Only what the shell's stop and report do differs:

| Report sink | |
| --- | --- |
| no report | `NoReport` |
| tree counter | the `Reporter` of an open `how-far-along` phase: the counting a live tree does |
| report callback | `Box<dyn Fn(u64)>` calling a `#[cold] #[inline(never)]` function that does nothing |

| Stop policy | |
| --- | --- |
| `Unstoppable` | never stops |
| `AtomicBool` | `almost_enough::Stopper`, an `AtomicBool` behind an `Arc` |
| stop callback | `almost_enough::FnStop` around a `Box<dyn Fn() -> bool>` calling a cold function that returns `false` |

`PulseTree` rows use the real tracker with each stop. The callbacks do nothing,
so these numbers are the cost of reaching user code, not of what it does.

## Summary

Extra instructions per checkpoint with `step` (the library calls
`pulse.step(n)` on every chunk):

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 14 | 18 | 26 |
| tree counter | 42 | 46 | 54 |
| report callback | 22 | 26 | 34 |
| `PulseTree` | 53 | 57 | 65 |

The parts add up. Reaching do-nothing `check` and `advance` through the
vtable is 14 instructions. An `AtomicBool` stop adds 4, a stop callback 12,
a report callback 8, and a shared tree counter 28: its locked update, not a
callback, is the expensive part. The full `PulseTree` adds another 11 for its
counting state and its boxed stop policy.

- `live()` costs the same as `step` minus about 2 instructions, except with no
  report and `Unstoppable`, where it removes the checkpoint entirely (0.0
  instructions, every chunk size).
- `Paced` (reaching the pulse once per 64 KiB) costs about 5 instructions per
  checkpoint at 64 and 256 bytes whatever the configuration, because the stop
  and the report only run once per 64 KiB. At 4 KiB chunks it is 6 to 11,
  because each buffer's 4 reaches and its `Paced::new` are shared by only 64
  steps.

Instruction overhead against the same loop with no checkpoints:

| Chunk | `step`, cheapest to costliest | `live()` | `Paced` |
| --- | --- | --- | --- |
| 64 B | +12.2% (no report, `Unstoppable`) to +56.5% (`PulseTree`, stop callback) | +0.01% to +54.8% | +4.4% |
| 256 B | +3.5% to +16.1% | +0.01% to +15.7% | +1.3% |
| 4 KiB | +0.23% to +1.06% | +0.01% to +1.04% | +0.10% to +0.19% |

In cycles (loaded machine), a `step` checkpoint costs about 11 cycles with no
report and `Unstoppable` and about 30 to 39 into a `PulseTree`, at 64 and 256
bytes; a `Paced` step 2 to 3.

## Raw output

```text
rustc 1.99.0 (b940084d7 2026-09-28)

### One checkpoint per 64 bytes (4096 per 256 KiB)

No checkpoints at all: 471,085 instructions, 164,743 cycles per buffer.

`step`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 14.0 / 11.3 | 18.0 / 11.6 | 26.0 / 20.8 |
| tree counter | 42.0 / 21.8 | 46.0 / 25.1 | 54.0 / 33.1 |
| report callback | 22.0 / 15.0 | 26.0 / 15.9 | 34.0 / 22.0 |
| `PulseTree` | 53.0 / 29.2 | 57.0 / 30.9 | 65.0 / 38.9 |

`step`, overhead against no checkpoints, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | +12.18% / +28.2% | +15.66% / +28.9% | +22.61% / +51.7% |
| tree counter | +36.52% / +54.3% | +40.00% / +62.5% | +46.96% / +82.2% |
| report callback | +19.13% / +37.4% | +22.61% / +39.5% | +29.57% / +54.7% |
| `PulseTree` | +46.09% / +72.7% | +49.57% / +76.7% | +56.53% / +96.7% |

`live`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 0.0 / 2.0 | 16.0 / 13.3 | 24.0 / 21.7 |
| tree counter | 40.0 / 20.7 | 44.0 / 23.2 | 52.0 / 30.4 |
| report callback | 20.0 / 15.8 | 24.0 / 15.6 | 32.0 / 23.2 |
| `PulseTree` | 51.0 / 28.3 | 55.0 / 29.2 | 63.0 / 36.7 |

`live`, overhead against no checkpoints, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | +0.01% / +4.9% | +13.92% / +33.1% | +20.88% / +53.9% |
| tree counter | +34.79% / +51.4% | +38.27% / +57.6% | +45.22% / +75.6% |
| report callback | +17.40% / +39.4% | +20.87% / +38.7% | +27.83% / +57.7% |
| `PulseTree` | +44.36% / +70.5% | +47.83% / +72.7% | +54.79% / +91.2% |

`paced`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 5.0 / 2.1 | 5.0 / 2.3 | 5.1 / 2.1 |
| tree counter | 5.1 / 2.1 | 5.1 / 2.1 | 5.1 / 2.1 |
| report callback | 5.1 / 2.2 | 5.1 / 2.4 | 5.1 / 2.3 |
| `PulseTree` | 5.1 / 2.5 | 5.1 / 2.3 | 5.1 / 2.3 |

`paced`, overhead against no checkpoints, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | +4.36% / +5.3% | +4.39% / +5.7% | +4.40% / +5.3% |
| tree counter | +4.41% / +5.1% | +4.41% / +5.3% | +4.42% / +5.2% |
| report callback | +4.39% / +5.4% | +4.40% / +6.0% | +4.41% / +5.8% |
| `PulseTree` | +4.43% / +6.1% | +4.43% / +5.8% | +4.44% / +5.8% |

Cycle spread per variant (min–max of five):

| Variant | Cycles median | Spread |
| --- | ---: | ---: |
| none | 164,743 | 160,620–165,348 |
| m-step-none-none | 211,199 | 210,426–222,083 |
| m-step-none-flag | 212,361 | 210,208–226,651 |
| m-step-none-call | 249,923 | 238,362–271,046 |
| m-step-count-none | 254,204 | 251,886–255,208 |
| m-step-count-flag | 267,730 | 113,868–287,037 |
| m-step-count-call | 300,158 | 294,606–302,394 |
| m-step-call-none | 226,293 | 224,761–245,917 |
| m-step-call-flag | 229,841 | 228,094–246,633 |
| m-step-call-call | 254,844 | 254,252–256,722 |
| m-live-none-none | 172,891 | 172,256–173,352 |
| m-live-none-flag | 219,215 | 212,456–232,361 |
| m-live-none-call | 253,622 | 238,611–255,635 |
| m-live-count-none | 249,354 | 245,299–256,330 |
| m-live-count-flag | 259,715 | 258,671–263,796 |
| m-live-count-call | 289,242 | 288,408–299,418 |
| m-live-call-none | 229,576 | 228,440–231,760 |
| m-live-call-flag | 228,516 | 227,079–238,503 |
| m-live-call-call | 259,869 | 251,938–281,557 |
| m-paced-none-none | 173,459 | 173,360–174,310 |
| m-paced-none-flag | 174,185 | 70,881–175,399 |
| m-paced-none-call | 173,430 | 92,376–175,032 |
| m-paced-count-none | 173,173 | 172,261–175,036 |
| m-paced-count-flag | 173,399 | 172,985–175,852 |
| m-paced-count-call | 173,254 | 172,822–173,398 |
| m-paced-call-none | 173,601 | 172,460–175,542 |
| m-paced-call-flag | 174,692 | 173,104–201,183 |
| m-paced-call-call | 174,303 | 172,230–175,652 |
| t-step-none | 284,543 | 276,932–301,169 |
| t-step-flag | 291,120 | 286,934–302,440 |
| t-step-call | 324,094 | 317,661–334,102 |
| t-live-none | 280,837 | 276,693–282,514 |
| t-live-flag | 284,479 | 283,299–288,694 |
| t-live-call | 314,996 | 308,597–317,849 |
| t-paced-none | 174,861 | 173,528–175,361 |
| t-paced-flag | 174,264 | 172,877–174,890 |
| t-paced-call | 174,266 | 173,812–175,111 |

### One checkpoint per 256 bytes (1024 per 256 KiB)

No checkpoints at all: 412,717 instructions, 143,397 cycles per buffer.

`step`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 14.0 / 12.6 | 18.0 / 12.7 | 26.0 / 17.7 |
| tree counter | 42.0 / 21.1 | 46.0 / 24.0 | 54.0 / 31.6 |
| report callback | 22.0 / 16.4 | 26.0 / 16.1 | 34.0 / 22.4 |
| `PulseTree` | 53.0 / 28.8 | 57.0 / 30.5 | 65.0 / 38.1 |

`step`, overhead against no checkpoints, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | +3.48% / +9.0% | +4.47% / +9.0% | +6.46% / +12.6% |
| tree counter | +10.43% / +15.1% | +11.42% / +17.2% | +13.41% / +22.5% |
| report callback | +5.46% / +11.7% | +6.46% / +11.5% | +8.44% / +16.0% |
| `PulseTree` | +13.16% / +20.6% | +14.15% / +21.8% | +16.14% / +27.2% |

`live`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 0.0 / 2.3 | 16.0 / 13.4 | 24.0 / 18.0 |
| tree counter | 40.0 / 20.4 | 44.0 / 24.0 | 52.0 / 31.5 |
| report callback | 20.0 / 15.7 | 24.0 / 16.4 | 32.0 / 22.4 |
| `PulseTree` | 51.1 / 28.1 | 55.1 / 29.2 | 63.1 / 36.0 |

`live`, overhead against no checkpoints, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | +0.01% / +1.6% | +3.98% / +9.6% | +5.97% / +12.8% |
| tree counter | +9.93% / +14.6% | +10.93% / +17.1% | +12.91% / +22.5% |
| report callback | +4.97% / +11.2% | +5.96% / +11.7% | +7.95% / +16.0% |
| `PulseTree` | +12.67% / +20.1% | +13.66% / +20.9% | +15.65% / +25.7% |

`paced`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 5.1 / 2.7 | 5.2 / 2.8 | 5.2 / 2.9 |
| tree counter | 5.3 / 2.8 | 5.3 / 3.1 | 5.3 / 3.1 |
| report callback | 5.2 / 3.0 | 5.2 / 2.7 | 5.3 / 2.9 |
| `PulseTree` | 5.4 / 2.9 | 5.4 / 3.1 | 5.4 / 3.1 |

`paced`, overhead against no checkpoints, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | +1.26% / +1.9% | +1.29% / +2.0% | +1.30% / +2.1% |
| tree counter | +1.31% / +2.0% | +1.32% / +2.2% | +1.33% / +2.2% |
| report callback | +1.29% / +2.2% | +1.30% / +1.9% | +1.31% / +2.1% |
| `PulseTree` | +1.33% / +2.1% | +1.33% / +2.2% | +1.34% / +2.2% |

Cycle spread per variant (min–max of five):

| Variant | Cycles median | Spread |
| --- | ---: | ---: |
| none | 143,397 | 74,612–144,606 |
| m-step-none-none | 156,309 | 83,983–157,852 |
| m-step-none-flag | 156,354 | 84,071–171,195 |
| m-step-none-call | 161,511 | 90,215–164,497 |
| m-step-count-none | 165,053 | 88,465–167,146 |
| m-step-count-flag | 168,001 | 88,738–179,516 |
| m-step-count-call | 175,713 | 94,457–176,860 |
| m-step-call-none | 160,198 | 158,619–177,195 |
| m-step-call-flag | 159,875 | 159,499–161,150 |
| m-step-call-call | 166,311 | 165,761–173,626 |
| m-live-none-none | 145,714 | 143,407–145,904 |
| m-live-none-flag | 157,122 | 154,887–168,435 |
| m-live-none-call | 161,788 | 161,504–173,952 |
| m-live-count-none | 164,328 | 161,113–165,689 |
| m-live-count-flag | 167,932 | 165,559–174,010 |
| m-live-count-call | 175,639 | 173,585–179,515 |
| m-live-call-none | 159,479 | 159,154–160,653 |
| m-live-call-flag | 160,156 | 159,794–162,909 |
| m-live-call-call | 166,341 | 164,929–166,534 |
| m-paced-none-none | 146,188 | 146,002–146,581 |
| m-paced-none-flag | 146,245 | 145,977–147,196 |
| m-paced-none-call | 146,355 | 145,959–146,749 |
| m-paced-count-none | 146,294 | 146,243–146,435 |
| m-paced-count-flag | 146,591 | 145,913–159,325 |
| m-paced-count-call | 146,537 | 145,395–146,711 |
| m-paced-call-none | 146,518 | 146,160–146,831 |
| m-paced-call-flag | 146,143 | 145,986–146,866 |
| m-paced-call-call | 146,385 | 146,155–146,645 |
| t-step-none | 172,877 | 171,739–174,558 |
| t-step-flag | 174,619 | 173,770–177,439 |
| t-step-call | 182,376 | 180,812–183,367 |
| t-live-none | 172,204 | 168,047–173,499 |
| t-live-flag | 173,297 | 171,793–173,974 |
| t-live-call | 180,305 | 178,859–182,524 |
| t-paced-none | 146,374 | 142,471–146,662 |
| t-paced-flag | 146,544 | 140,690–146,961 |
| t-paced-call | 146,619 | 146,135–147,096 |

### One checkpoint per 4096 bytes (64 per 256 KiB)

No checkpoints at all: 394,476 instructions, 132,880 cycles per buffer.

`step`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 14.3 / 7.8 | 18.3 / 3.1 | 26.5 / 14.2 |
| tree counter | 42.3 / 19.5 | 46.4 / 26.0 | 54.5 / 33.2 |
| report callback | 22.3 / 7.9 | 26.4 / 10.3 | 34.5 / 36.4 |
| `PulseTree` | 53.4 / 34.5 | 57.5 / 45.8 | 65.6 / 48.3 |

`step`, overhead against no checkpoints, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | +0.23% / +0.4% | +0.30% / +0.2% | +0.43% / +0.7% |
| tree counter | +0.69% / +0.9% | +0.75% / +1.3% | +0.88% / +1.6% |
| report callback | +0.36% / +0.4% | +0.43% / +0.5% | +0.56% / +1.8% |
| `PulseTree` | +0.87% / +1.7% | +0.93% / +2.2% | +1.06% / +2.3% |

`live`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 0.4 / 0.6 | 16.6 / 20.1 | 24.7 / 29.7 |
| tree counter | 40.6 / 26.3 | 44.6 / 32.7 | 52.7 / 45.6 |
| report callback | 20.6 / 19.6 | 24.6 / 34.9 | 32.7 / 38.6 |
| `PulseTree` | 51.9 / 42.1 | 55.8 / 32.7 | 63.9 / 55.5 |

`live`, overhead against no checkpoints, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | +0.01% / +0.0% | +0.27% / +1.0% | +0.40% / +1.4% |
| tree counter | +0.66% / +1.3% | +0.72% / +1.6% | +0.85% / +2.2% |
| report callback | +0.33% / +0.9% | +0.40% / +1.7% | +0.53% / +1.9% |
| `PulseTree` | +0.84% / +2.0% | +0.91% / +1.6% | +1.04% / +2.7% |

`paced`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 6.0 / 8.9 | 8.2 / 9.8 | 8.8 / 6.5 |
| tree counter | 9.8 / 16.6 | 9.9 / 7.1 | 10.6 / 27.0 |
| report callback | 8.5 / 24.1 | 8.7 / 7.2 | 9.3 / 8.3 |
| `PulseTree` | 10.8 / 12.5 | 10.9 / 13.2 | 11.5 / 16.6 |

`paced`, overhead against no checkpoints, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | +0.10% / +0.4% | +0.13% / +0.5% | +0.14% / +0.3% |
| tree counter | +0.16% / +0.8% | +0.16% / +0.3% | +0.17% / +1.3% |
| report callback | +0.14% / +1.2% | +0.14% / +0.3% | +0.15% / +0.4% |
| `PulseTree` | +0.17% / +0.6% | +0.18% / +0.6% | +0.19% / +0.8% |

Cycle spread per variant (min–max of five):

| Variant | Cycles median | Spread |
| --- | ---: | ---: |
| none | 132,880 | 132,299–133,656 |
| m-step-none-none | 133,378 | 132,888–134,490 |
| m-step-none-flag | 133,081 | 132,532–133,979 |
| m-step-none-call | 133,787 | 133,509–134,495 |
| m-step-count-none | 134,129 | 133,231–159,614 |
| m-step-count-flag | 134,541 | 127,401–151,942 |
| m-step-count-call | 135,007 | 134,561–155,898 |
| m-step-call-none | 133,387 | 132,942–134,369 |
| m-step-call-flag | 133,536 | 129,504–135,114 |
| m-step-call-call | 135,211 | 133,439–159,087 |
| m-live-none-none | 132,915 | 128,775–133,646 |
| m-live-none-flag | 134,165 | 132,663–154,986 |
| m-live-none-call | 134,778 | 134,202–152,324 |
| m-live-count-none | 134,562 | 134,303–145,931 |
| m-live-count-flag | 134,970 | 134,305–151,161 |
| m-live-count-call | 135,799 | 134,403–146,617 |
| m-live-call-none | 134,134 | 128,761–150,822 |
| m-live-call-flag | 135,116 | 132,668–153,508 |
| m-live-call-call | 135,350 | 133,971–160,404 |
| m-paced-none-none | 133,448 | 127,869–156,124 |
| m-paced-none-flag | 133,509 | 132,515–162,849 |
| m-paced-none-call | 133,297 | 132,501–133,782 |
| m-paced-count-none | 133,941 | 132,694–151,411 |
| m-paced-count-flag | 133,336 | 132,347–134,378 |
| m-paced-count-call | 134,606 | 128,853–162,453 |
| m-paced-call-none | 134,419 | 133,339–154,065 |
| m-paced-call-flag | 133,342 | 127,345–134,083 |
| m-paced-call-call | 133,409 | 128,891–134,188 |
| t-step-none | 135,090 | 129,555–148,579 |
| t-step-flag | 135,812 | 134,076–136,276 |
| t-step-call | 135,974 | 135,726–136,796 |
| t-live-none | 135,572 | 134,227–136,017 |
| t-live-flag | 134,973 | 134,126–135,661 |
| t-live-call | 136,430 | 135,360–137,140 |
| t-paced-none | 133,679 | 133,020–134,724 |
| t-paced-flag | 133,724 | 133,342–160,438 |
| t-paced-call | 133,945 | 132,861–134,610 |
```
