# Checkpoints into `&NoPulse`, 2026-10-03

- Commands: `python3 dev/how-far-checkpoint-cost/measure.py matrix 64,256,4096`
  and `python3 dev/how-far-checkpoint-cost/measure.py 64,256,1024,4096`
- Before: `0a6c23c`. After: `e14e223`, with `47c7e9c`. Both measured with
  this commit's harness, which adds the `&NoPulse` rows; the before run's other
  cells repeat [the matrix run](how-far-checkpoint-matrix-2026-10-03.md).
- rustc 1.99.0, release profile, no `target-cpu=native`; AMD Ryzen 9 5900XT
- The machine was heavily loaded (load average about 30). Instruction counts
  are exact and are the numbers to rely on; cycles carry the other jobs' cache
  traffic.

What changed:

- `NoPulse` is one static, and `ProgressExt::step` and `live()` skip a pulse
  whose address and size are the static's (`e14e223`).
- `Paced` skips a pulse that can neither stop nor report, counts down, and
  keeps its state in the caller's registers (`47c7e9c`).

## Summary

Extra instructions per checkpoint at 64-byte chunks (4,096 checkpoints per
256 KiB buffer), against the same loop with no checkpoints:

| Pulse | `step` before | after | `live()` before | after | `Paced` before | after |
| --- | ---: | ---: | ---: | ---: | ---: | ---: |
| `&NoPulse` | 14.0 | −1.0 | 0.0 | −1.0 | 5.0 | 0.0 |
| Shell: `Unstoppable`, no report | 14.0 | 13.0 | 0.0 | −1.0 | 5.0 | 0.0 |
| Shell: `AtomicBool`, tree counter | 46.0 | 45.0 | 44.0 | 45.0 | 5.1 | 2.1 |
| `PulseTree`, `AtomicBool` | 57.0 | 56.0 | 55.0 | 56.0 | 5.1 | 2.1 |

- With `&NoPulse`, no style leaves checkpoint code in the loop. The loop with
  no checkpoints keeps one `black_box` store per chunk; the `step` and
  `live()` loops lack it and measure one instruction under that loop, and the
  `Paced` loop has a register move in its place and matches it.
- The shell with no stop and no report is a different type, so `step` still
  makes its two calls; `live()` and `Paced` ask it once and skip it.
- Into live pulses, every `step` cell fell by one instruction and every
  `live()` cell rose by one, at every chunk size. Neither is checkpoint code:
  the `NoPulse` test runs before the loop in both. With the test present, LLVM
  rotates these loops differently; the `live()` loop now tests its exit at the
  top. Taking the test out of both `step` and `live()` restores both numbers;
  taking it out of `live()` alone does not.
- A paced step into a live pulse is a subtraction and a branch: 2.1
  instructions instead of 5.1, under a cycle either way.

Where the test cannot leave the loop, as in a `step` the compiler inlines into
straight-line code, it is one comparison and branch before the two calls, or
for `&NoPulse` two of each and no call. A standalone `pulse.step(n)` on
`&dyn Pulse` compiles to (x86-64, prologue omitted):

```text
mov  rax, rsi
cmp  rdi, qword ptr [rip + NoPulse@GOT]   ; the address
jne  .live
cmp  qword ptr [rax + 8], 1               ; the size, from the vtable
jne  .live
mov  al, 2                                ; Ok(())
ret
.live:
...  call qword ptr [rax + 0x30]          ; advance
...  jmp  qword ptr [rax + 0x18]          ; check, a tail call
```

Per three-stage operation of 4 KiB, against the same work without a plan:

| | Before | After |
| --- | ---: | ---: |
| `NoPulse`, paced | 930 | 874 |
| `NoPulse`, stepping every chunk | 875 | 810 |
| Live tree, paced | 6,932 | 6,976 |
| Live tree, stepping every chunk | 6,998 | 7,038 |

The live tree's operations cost about 44 instructions more: each stage's
`Paced::new` tests for `NoPulse`, and finishing or fetching a stage now
branches on whether the `Child` holds a box.

## Raw output, after

The matrix runs below keep the per-checkpoint tables; the percentage and
cycle-spread tables that `measure.py matrix` also prints are left out.

```text
rustc 1.99.0 (b940084d7 2026-09-28)

### One checkpoint per 64 bytes (4096 per 256 KiB)

No checkpoints at all: 471,085 instructions, 165,121 cycles per buffer.

`step`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 13.0 / 12.7 | 17.0 / 11.9 | 25.0 / 21.6 |
| tree counter | 41.0 / 22.9 | 45.0 / 25.7 | 53.0 / 33.3 |
| report callback | 21.0 / 18.3 | 25.0 / 16.1 | 33.0 / 22.6 |
| `PulseTree` | 52.0 / 31.5 | 56.0 / 32.1 | 64.0 / 40.1 |

`&NoPulse` (no stop, no report): -0.99 instructions, -0.26 cycles per checkpoint (-0.86% instructions).

`live`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | -1.0 / -0.2 | 17.0 / 12.4 | 25.0 / 18.5 |
| tree counter | 41.0 / 23.3 | 45.0 / 24.3 | 53.0 / 31.7 |
| report callback | 21.0 / 16.2 | 25.0 / 13.4 | 33.0 / 21.0 |
| `PulseTree` | 52.0 / 31.5 | 56.0 / 31.7 | 64.0 / 36.2 |

`&NoPulse` (no stop, no report): -1.00 instructions, -0.38 cycles per checkpoint (-0.87% instructions).

`paced`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 0.0 / 1.8 | 2.1 / -0.1 | 2.1 / -0.2 |
| tree counter | 2.1 / -0.3 | 2.1 / -0.3 | 2.1 / -0.0 |
| report callback | 2.1 / -0.1 | 2.1 / -0.1 | 2.1 / 0.1 |
| `PulseTree` | 2.1 / 0.0 | 2.1 / -0.1 | 2.1 / -0.2 |

`&NoPulse` (no stop, no report): 0.01 instructions, 1.96 cycles per checkpoint (+0.01% instructions).

### One checkpoint per 256 bytes (1024 per 256 KiB)

No checkpoints at all: 412,717 instructions, 143,916 cycles per buffer.

`step`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 13.0 / 11.1 | 17.0 / 12.1 | 25.0 / 17.8 |
| tree counter | 41.0 / 22.4 | 45.0 / 27.0 | 53.0 / 34.2 |
| report callback | 21.0 / 17.4 | 25.0 / 16.5 | 33.0 / 23.1 |
| `PulseTree` | 52.0 / 31.7 | 56.0 / 34.5 | 64.0 / 38.6 |

`&NoPulse` (no stop, no report): -0.97 instructions, 0.43 cycles per checkpoint (-0.24% instructions).

`live`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | -1.0 / -0.0 | 17.0 / 8.6 | 25.0 / 15.2 |
| tree counter | 41.0 / 22.2 | 45.0 / 24.6 | 53.0 / 32.6 |
| report callback | 21.0 / 14.0 | 25.0 / 13.7 | 33.0 / 20.2 |
| `PulseTree` | 52.1 / 30.7 | 56.1 / 33.4 | 64.1 / 37.6 |

`&NoPulse` (no stop, no report): -0.98 instructions, 0.33 cycles per checkpoint (-0.24% instructions).

`paced`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 0.1 / 2.2 | 2.2 / 0.4 | 2.3 / 0.5 |
| tree counter | 2.3 / 0.6 | 2.3 / 0.2 | 2.4 / 0.0 |
| report callback | 2.2 / 1.3 | 2.2 / 0.0 | 2.3 / 1.0 |
| `PulseTree` | 2.4 / 0.4 | 2.4 / 0.8 | 2.4 / 0.7 |

`&NoPulse` (no stop, no report): 0.06 instructions, 2.01 cycles per checkpoint (+0.01% instructions).

### One checkpoint per 4096 bytes (64 per 256 KiB)

No checkpoints at all: 394,477 instructions, 132,611 cycles per buffer.

`step`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 13.4 / 24.0 | 17.5 / 20.7 | 25.6 / 23.7 |
| tree counter | 41.4 / 26.0 | 45.5 / 35.8 | 53.6 / 47.4 |
| report callback | 21.4 / 22.6 | 25.5 / 23.4 | 33.6 / 15.4 |
| `PulseTree` | 52.5 / 45.3 | 56.6 / 43.3 | 64.7 / 56.1 |

`&NoPulse` (no stop, no report): -0.56 instructions, 1.05 cycles per checkpoint (-0.01% instructions).

`live`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | -0.5 / -2.4 | 17.6 / 10.4 | 25.7 / 19.1 |
| tree counter | 41.7 / 17.1 | 45.6 / 25.4 | 53.7 / 38.2 |
| report callback | 21.6 / 26.9 | 25.6 / 24.4 | 33.7 / 33.4 |
| `PulseTree` | 52.9 / 29.2 | 56.8 / 43.3 | 64.9 / 46.4 |

`&NoPulse` (no stop, no report): -0.71 instructions, 0.72 cycles per checkpoint (-0.01% instructions).

`paced`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 1.1 / 10.3 | 5.5 / 1.7 | 6.1 / 5.3 |
| tree counter | 7.1 / 9.5 | 7.2 / 10.5 | 7.9 / 7.5 |
| report callback | 5.8 / 8.6 | 6.0 / 13.4 | 6.6 / 2.2 |
| `PulseTree` | 8.1 / 15.8 | 8.2 / 6.6 | 8.8 / 21.9 |

`&NoPulse` (no stop, no report): 0.95 instructions, -2.40 cycles per checkpoint (+0.02% instructions).
```

```text
rustc 1.99.0 (b940084d7 2026-09-28)

Per 256 KiB buffer, one checkpoint per 64 bytes (4096 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 471,085 | +0.00% | 164,076 | +0.00% | 163,021–164,452 |
| step-nopulse | 467,020 | -0.86% | 162,828 | -0.76% | 162,595–165,157 |
| live-nopulse | 467,011 | -0.86% | 164,682 | +0.37% | 163,060–165,064 |
| paced-nopulse | 471,146 | +0.01% | 172,543 | +5.16% | 172,237–173,840 |
| step-tree | 700,504 | +48.70% | 292,846 | +78.48% | 291,384–299,654 |
| live-tree | 700,520 | +48.70% | 306,694 | +86.92% | 287,887–308,543 |
| paced-tree | 479,673 | +1.82% | 164,297 | +0.13% | 163,507–166,136 |

Per 256 KiB buffer, one checkpoint per 256 bytes (1024 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 412,717 | +0.00% | 143,144 | +0.00% | 142,726–144,339 |
| step-nopulse | 411,724 | -0.24% | 143,267 | +0.09% | 142,521–144,221 |
| live-nopulse | 411,715 | -0.24% | 142,753 | -0.27% | 142,502–143,722 |
| paced-nopulse | 412,778 | +0.01% | 145,195 | +1.43% | 144,805–146,550 |
| step-tree | 470,103 | +13.90% | 175,837 | +22.84% | 175,132–179,210 |
| live-tree | 470,117 | +13.91% | 174,439 | +21.86% | 173,911–177,573 |
| paced-tree | 415,161 | +0.59% | 144,163 | +0.71% | 143,421–145,296 |

Per 256 KiB buffer, one checkpoint per 1024 bytes (256 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 398,125 | +0.00% | 138,091 | +0.00% | 137,726–138,164 |
| step-nopulse | 397,900 | -0.06% | 138,027 | -0.05% | 137,322–138,489 |
| live-nopulse | 397,891 | -0.06% | 137,738 | -0.26% | 137,209–138,334 |
| paced-nopulse | 398,186 | +0.02% | 138,195 | +0.08% | 137,907–138,588 |
| step-tree | 412,501 | +3.61% | 146,944 | +6.41% | 145,666–147,273 |
| live-tree | 412,516 | +3.61% | 146,204 | +5.88% | 145,732–146,585 |
| paced-tree | 399,033 | +0.23% | 138,199 | +0.08% | 138,021–138,335 |

Per 256 KiB buffer, one checkpoint per 4096 bytes (64 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 394,477 | +0.00% | 132,722 | +0.00% | 131,879–132,970 |
| step-nopulse | 394,444 | -0.01% | 132,591 | -0.10% | 131,570–133,115 |
| live-nopulse | 394,435 | -0.01% | 132,633 | -0.07% | 131,916–133,056 |
| paced-nopulse | 394,538 | +0.02% | 132,890 | +0.13% | 132,226–133,901 |
| step-tree | 398,101 | +0.92% | 135,071 | +1.77% | 134,026–135,762 |
| live-tree | 398,116 | +0.92% | 135,710 | +2.25% | 134,556–136,161 |
| paced-tree | 395,001 | +0.13% | 133,520 | +0.60% | 132,800–133,893 |

Per operation of 4 KiB, three stages, one checkpoint per 1024 bytes:

| Variant | Instructions | vs op-none | Cycles | vs op-none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| op-none | 6,347 | +0.00% | 2,305 | +0.00% | 2,176–2,342 |
| op-nopulse | 7,221 | +13.77% | 2,731 | +18.49% | 2,426–2,744 |
| op-tree | 13,323 | +109.90% | 6,570 | +185.07% | 6,349–6,759 |
| opstep-nopulse | 7,157 | +12.76% | 2,690 | +16.74% | 2,560–2,962 |
| opstep-tree | 13,385 | +110.89% | 6,580 | +185.54% | 6,441–6,644 |

Per operation of 256 KiB, three stages, one checkpoint per 1024 bytes:

| Variant | Instructions | vs op-none | Cycles | vs op-none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| op-none | 398,207 | +0.00% | 141,180 | +0.00% | 74,555–142,986 |
| op-nopulse | 398,829 | +0.16% | 141,991 | +0.57% | 74,529–142,914 |
| op-tree | 406,152 | +2.00% | 148,329 | +5.06% | 78,340–149,196 |
| opstep-nopulse | 398,765 | +0.14% | 141,913 | +0.52% | 75,260–142,987 |
| opstep-tree | 418,855 | +5.19% | 156,728 | +11.01% | 78,034–172,766 |
```

## Raw output, before

```text
rustc 1.99.0 (b940084d7 2026-09-28)

### One checkpoint per 64 bytes (4096 per 256 KiB)

No checkpoints at all: 471,085 instructions, 175,382 cycles per buffer.

`step`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 14.0 / 9.6 | 18.0 / 11.9 | 26.0 / 16.3 |
| tree counter | 42.0 / 22.3 | 46.0 / 26.7 | 54.0 / 28.4 |
| report callback | 22.0 / 16.8 | 26.0 / 14.9 | 34.0 / 22.6 |
| `PulseTree` | 53.0 / 25.7 | 57.0 / 30.8 | 65.0 / 33.9 |

`&NoPulse` (no stop, no report): 14.01 instructions, 9.10 cycles per checkpoint (+12.18% instructions).

`live`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 0.0 / -0.0 | 16.0 / 9.9 | 24.0 / 16.0 |
| tree counter | 40.0 / 22.5 | 44.0 / 25.1 | 52.0 / 27.5 |
| report callback | 20.0 / 13.8 | 24.0 / 14.1 | 32.0 / 20.7 |
| `PulseTree` | 51.0 / 26.9 | 55.0 / 27.6 | 63.0 / 33.7 |

`&NoPulse` (no stop, no report): 0.01 instructions, 0.14 cycles per checkpoint (+0.01% instructions).

`paced`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 5.0 / 0.6 | 5.1 / 0.9 | 5.1 / 1.0 |
| tree counter | 5.1 / 0.8 | 5.1 / 1.1 | 5.1 / 1.0 |
| report callback | 5.1 / 0.7 | 5.1 / 0.7 | 5.1 / 0.8 |
| `PulseTree` | 5.1 / 1.0 | 5.1 / 1.1 | 5.1 / 1.0 |

`&NoPulse` (no stop, no report): 5.02 instructions, 0.51 cycles per checkpoint (+4.36% instructions).

### One checkpoint per 256 bytes (1024 per 256 KiB)

No checkpoints at all: 412,717 instructions, 145,279 cycles per buffer.

`step`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 14.0 / 9.9 | 18.0 / 9.6 | 26.0 / 16.4 |
| tree counter | 42.0 / 22.3 | 46.0 / 25.3 | 54.0 / 31.3 |
| report callback | 22.0 / 14.2 | 26.0 / 14.3 | 34.0 / 22.2 |
| `PulseTree` | 53.0 / 25.5 | 57.0 / 26.4 | 65.0 / 35.1 |

`&NoPulse` (no stop, no report): 14.02 instructions, 9.39 cycles per checkpoint (+3.48% instructions).

`live`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 0.0 / 0.5 | 16.0 / 9.6 | 24.0 / 15.6 |
| tree counter | 40.0 / 22.7 | 44.0 / 25.4 | 52.0 / 27.8 |
| report callback | 20.0 / 13.2 | 24.0 / 14.5 | 32.0 / 21.5 |
| `PulseTree` | 51.1 / 26.4 | 55.1 / 27.2 | 63.1 / 33.9 |

`&NoPulse` (no stop, no report): 0.03 instructions, -0.40 cycles per checkpoint (+0.01% instructions).

`paced`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 5.1 / 1.3 | 5.2 / 0.7 | 5.2 / 1.8 |
| tree counter | 5.3 / 1.0 | 5.3 / 0.9 | 5.3 / 0.6 |
| report callback | 5.2 / 0.3 | 5.2 / 0.7 | 5.3 / 1.0 |
| `PulseTree` | 5.4 / 0.9 | 5.4 / 1.1 | 5.4 / 1.1 |

`&NoPulse` (no stop, no report): 5.06 instructions, 1.87 cycles per checkpoint (+1.26% instructions).

### One checkpoint per 4096 bytes (64 per 256 KiB)

No checkpoints at all: 394,477 instructions, 132,568 cycles per buffer.

`step`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 14.3 / 23.7 | 18.4 / 22.2 | 26.5 / 27.6 |
| tree counter | 42.3 / 43.6 | 46.4 / 31.7 | 54.5 / 41.2 |
| report callback | 22.3 / 20.2 | 26.4 / 23.5 | 34.5 / 36.3 |
| `PulseTree` | 53.4 / 42.4 | 57.5 / 45.6 | 65.6 / 67.4 |

`&NoPulse` (no stop, no report): 14.32 instructions, 7.90 cycles per checkpoint (+0.23% instructions).

`live`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 0.4 / -7.5 | 16.6 / 15.2 | 24.7 / 28.2 |
| tree counter | 40.7 / 22.7 | 44.6 / 36.2 | 52.7 / 40.4 |
| report callback | 20.6 / 27.4 | 24.6 / 24.9 | 32.7 / 39.4 |
| `PulseTree` | 52.0 / 43.1 | 55.8 / 43.7 | 64.0 / 54.2 |

`&NoPulse` (no stop, no report): 0.42 instructions, 7.24 cycles per checkpoint (+0.01% instructions).

`paced`, extra per checkpoint, instructions / cycles:

| | `Unstoppable` | `AtomicBool` | stop callback |
| --- | ---: | ---: | ---: |
| no report | 6.0 / 10.3 | 8.2 / 9.2 | 8.8 / 14.4 |
| tree counter | 9.8 / 12.7 | 10.0 / 12.7 | 10.6 / 9.7 |
| report callback | 8.5 / 16.1 | 8.7 / 18.7 | 9.3 / 13.5 |
| `PulseTree` | 10.8 / 13.7 | 10.9 / 21.2 | 11.5 / 21.0 |

`&NoPulse` (no stop, no report): 6.03 instructions, 5.93 cycles per checkpoint (+0.10% instructions).
```

```text
rustc 1.99.0 (b940084d7 2026-09-28)

Per 256 KiB buffer, one checkpoint per 64 bytes (4096 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 471,085 | +0.00% | 176,140 | +0.00% | 174,995–176,840 |
| step-nopulse | 528,452 | +12.18% | 213,064 | +20.96% | 211,055–225,295 |
| live-nopulse | 471,115 | +0.01% | 175,219 | -0.52% | 174,706–176,662 |
| paced-nopulse | 491,632 | +4.36% | 177,654 | +0.86% | 177,019–179,514 |
| step-tree | 704,601 | +49.57% | 297,377 | +68.83% | 282,952–300,781 |
| live-tree | 696,433 | +47.84% | 289,536 | +64.38% | 284,857–305,570 |
| paced-tree | 491,944 | +4.43% | 179,935 | +2.15% | 178,860–180,346 |

Per 256 KiB buffer, one checkpoint per 256 bytes (1024 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 412,717 | +0.00% | 146,418 | +0.00% | 145,366–146,669 |
| step-nopulse | 427,076 | +3.48% | 154,907 | +5.80% | 154,118–156,653 |
| live-nopulse | 412,747 | +0.01% | 145,903 | -0.35% | 145,047–146,940 |
| paced-nopulse | 417,904 | +1.26% | 146,306 | -0.08% | 146,178–146,992 |
| step-tree | 471,122 | +14.15% | 173,547 | +18.53% | 172,183–176,708 |
| live-tree | 469,095 | +13.66% | 174,609 | +19.25% | 173,102–175,370 |
| paced-tree | 418,216 | +1.33% | 147,263 | +0.58% | 146,214–147,549 |

Per 256 KiB buffer, one checkpoint per 1024 bytes (256 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 398,125 | +0.00% | 138,927 | +0.00% | 92,246–139,265 |
| step-nopulse | 401,732 | +0.91% | 141,684 | +1.98% | 80,579–142,220 |
| live-nopulse | 398,155 | +0.01% | 138,944 | +0.01% | 72,732–139,352 |
| paced-nopulse | 399,472 | +0.34% | 138,905 | -0.02% | 58,280–139,078 |
| step-tree | 412,752 | +3.67% | 146,256 | +5.28% | 76,659–149,543 |
| live-tree | 412,261 | +3.55% | 146,481 | +5.44% | 145,944–147,707 |
| paced-tree | 399,784 | +0.42% | 139,222 | +0.21% | 138,531–139,636 |

Per 256 KiB buffer, one checkpoint per 4096 bytes (64 checkpoints):

| Variant | Instructions | vs none | Cycles | vs none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| none | 394,476 | +0.00% | 131,977 | +0.00% | 131,646–133,436 |
| step-nopulse | 395,396 | +0.23% | 132,965 | +0.75% | 131,013–134,122 |
| live-nopulse | 394,507 | +0.01% | 132,813 | +0.63% | 52,984–133,962 |
| paced-nopulse | 394,864 | +0.10% | 133,409 | +1.08% | 132,903–133,579 |
| step-tree | 398,161 | +0.93% | 135,853 | +2.94% | 132,820–145,743 |
| live-tree | 398,053 | +0.91% | 132,834 | +0.65% | 112,582–135,131 |
| paced-tree | 395,177 | +0.18% | 134,048 | +1.57% | 53,133–137,245 |

Per operation of 4 KiB, three stages, one checkpoint per 1024 bytes:

| Variant | Instructions | vs op-none | Cycles | vs op-none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| op-none | 6,347 | +0.00% | 2,291 | +0.00% | 2,240–2,390 |
| op-nopulse | 7,277 | +14.66% | 2,792 | +21.87% | 2,733–2,840 |
| op-tree | 13,279 | +109.23% | 6,688 | +191.92% | 6,127–6,949 |
| opstep-nopulse | 7,222 | +13.80% | 2,701 | +17.89% | 2,557–2,738 |
| opstep-tree | 13,345 | +110.26% | 6,530 | +185.02% | 6,373–6,990 |

Per operation of 256 KiB, three stages, one checkpoint per 1024 bytes:

| Variant | Instructions | vs op-none | Cycles | vs op-none | Cycle spread |
| --- | ---: | ---: | ---: | ---: | ---: |
| op-none | 398,208 | +0.00% | 142,450 | +0.00% | 140,599–143,233 |
| op-nopulse | 400,397 | +0.55% | 142,307 | -0.10% | 142,033–144,317 |
| op-tree | 406,607 | +2.11% | 150,444 | +5.61% | 148,056–151,064 |
| opstep-nopulse | 402,358 | +1.04% | 147,613 | +3.62% | 145,801–148,421 |
| opstep-tree | 418,813 | +5.17% | 156,840 | +10.10% | 156,537–159,191 |
```
