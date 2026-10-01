# how-far checkpoint overhead

Measured 2026-10-01 with `crates/how-far-along/benches/overhead.rs` (zenbench
0.1.9) on an AMD Ryzen 9 5900XT (16 cores, 32 threads), Linux 7.0, rustc
1.98.1, release profile, no `target-cpu=native`. Command:

```sh
~/work/zen/scripts/run-heavy --mem 8G -- cargo bench -p how-far-along --bench overhead
```

The machine was shared with other idle-to-light workloads (load average 0.4
to 0.7 during the runs). The bench disables zenbench's resource gate, because
zenbench 0.1.9 on Linux counts its own lock-heartbeat thread as a concurrent
benchmark and waits 30 s per round; interleaved rounds and paired statistics
still protect comparisons within a group. Three runs follow: run A is the
bench without the gated variants, run B adds them written out as
`(pulse.may_stop() || pulse.may_report()).then_some(pulse)`, and run C is the
bench as committed, which calls `ProgressExt::gated` (that same expression).
Times are per benchmark call: 10,000 calls in the isolated groups, one 256 KiB
buffer in the loop groups.

## Results

Per call, through `&dyn Pulse` or `&dyn Report` behind `#[inline(never)]`:

| Call | Run A | Run B | Run C |
| --- | ---: | ---: | ---: |
| `check()` → `NoPulse` | 1.83 ns | 1.63 ns | 1.64 ns |
| `check()` → `PulseTree` with `Stopper` | 1.83 ns | 1.63 ns | 1.43 ns |
| `check()` → `PulseTree` with `Unstoppable` | 2.45 ns | 2.24 ns | 2.05 ns |
| `check()` behind a `may_stop()` gate, when it cannot stop | ≈ 0 | ≈ 0 | ≈ 0 |
| `advance()` → `NoReport` | 1.86 ns | 1.86 ns | 1.65 ns |
| `advance()` → `Reporter` | 2.16 ns | 2.17 ns | 2.15 ns |
| `advance()` → `PulseTree` leaf | 3.24 ns | 3.23 ns | 3.26 ns |
| `advance()` → per-worker `Batch` of 64 | 0.54 ns | 0.57 ns | 0.55 ns |

Inside work: a 256 KiB PNG-style Sub defilter calling `check()` once and
`step(n)` after every chunk, through one `#[inline(never)]` function shared by
all variants, so only the vtable target differs. Overhead is versus `NoPulse`
in the same function:

| Checkpoint every | Checkpoints | Run A | Run B | Run C | Extra per checkpoint |
| ---: | ---: | ---: | ---: | ---: | ---: |
| 4096 bytes | 64 | +0.4 to +1.0% | +1.3 to +1.9% | +1.2 to +1.6% | about 3 ns |
| 256 bytes | 1024 | +8.8 to +11.2% | +11.2 to +12.8% | +9.6 to +12.4% | 1.9 to 3.1 ns |
| 64 bytes | 4096 | +15.5 to +21.8% | +43.8 to +45.4% | +42.1 to +46.4% | 1.6 to 3.0 ns |

The percentage at fine granularity depends on how fast the surrounding work
is: the defilter itself ran at different speeds in the two builds, while the
added cost per checkpoint stayed between 1.6 and 3.1 ns. Checking once per few
kilobytes of work keeps a live tree within about 2%.

A library can gate its calls: `pulse.gated()` gives an `Option<&dyn Pulse>`
whose `check` and `step` make no call at all for `NoPulse`. That gated loop
took 16.0 µs (run B) and 16.7 µs (run C) against 17.2 and 17.5 µs for a
monomorphized `impl Pulse` loop over `NoPulse`, so the no-observer path costs
nothing extra. Gating a live tree cost +3.3 to +5.4% over the ungated `&dyn`
loop.

## What this does not show

Comparisons between *different* functions are sensitive to code layout. The
monomorphized `impl Pulse` loop over a `PulseTree` ran 36% faster than the
`&dyn` loop in run A and the same speed in run B; the only change between the
two builds was one added function and two added benchmarks. No stable
dispatch penalty can be claimed from these runs, beyond the per-call costs
above. The numbers are wall time on one machine, not guarantees.

## Raw output

Run A:

```text
  check_isolated  30 rounds × 81 calls
                                  mean ±mad µs  95% CI vs base      checks/s
  ├─ &dyn Pulse → NoPulse         18.3 ±0.1µs  [18.3–18.4]µs         546M
  ├─ &dyn Pulse → tree(Unsto…     24.5 ±0.1µs  [+33.2%–+33.8%]       409M
  ├─ &dyn Pulse → tree(Stopp…     18.3 ±0.0µs  [-0.3%–+0.2%]         546M [1]
  ├─ may_stop gate → tree(Un…      0.0 ±0.0µs  [-100.2%–-99.8%]     4679G
  ╰─ impl Stop → Unstoppable       0.0 ±0.0µs  [-100.2%–-99.8%]    11351G
  advance_isolated  30 rounds × 45 calls
                                mean ±mad µs  95% CI vs base     reports/s
  ├─ &dyn Report → NoReport     18.6 ±0.2µs  [18.6–18.7]µs       537M
  ├─ &dyn Report → Reporter     21.6 ±0.2µs  [+15.5%–+16.4%]     462M
  ├─ &dyn Pulse → tree leaf     32.4 ±0.3µs  [+73.3%–+74.2%]     308M
  ╰─ Batch(64) → Reporter        5.4 ±0.1µs  [-71.4%–-70.8%]    1.86G
  codec_loop_chunk_4096  30 rounds × 36 calls
                         mean ±mad µs  95% CI vs base     iB/s
  ├─ NoPulse             27.3 ±0.1µs  [27.2–27.4]µs     8.94G
  ├─ tree(Unstoppable)   27.5 ±0.2µs  [+0.7%–+1.0%]     8.88G [1]
  ╰─ tree(Stopper)       27.5 ±0.1µs  [+0.4%–+0.9%]     8.89G [2]
  codec_loop_chunk_256  40 rounds × 30 calls
                         mean ±mad µs  95% CI vs base      iB/s
  ├─ NoPulse             30.4 ±0.1µs  [30.2–30.8]µs      8.03G
  ├─ tree(Unstoppable)   33.6 ±0.1µs  [+11.0%–+11.2%]    7.26G
  ╰─ tree(Stopper)       33.1 ±0.1µs  [+8.8%–+9.1%]      7.37G
  codec_loop_chunk_64  30 rounds × 26 calls
                         mean ±mad µs  95% CI vs base      iB/s
  ├─ NoPulse             40.4 ±0.2µs  [40.1–40.7]µs      6.04G
  ├─ tree(Unstoppable)   49.4 ±0.5µs  [+20.9%–+21.8%]    4.94G
  ╰─ tree(Stopper)       47.1 ±0.3µs  [+15.5%–+16.2%]    5.18G
  dyn_vs_generic_chunk_256  30 rounds × 50 calls
                              mean ±mad µs  95% CI vs base      iB/s
  ├─ &dyn Pulse → tree        31.8 ±0.2µs  [31.7–31.9]µs      7.68G
  ├─ impl Pulse → tree        20.1 ±0.1µs  [-36.9%–-36.4%]    12.1G
  ╰─ impl Pulse → NoPulse     17.1 ±0.4µs  [-46.9%–-46.1%]    14.3G
```

Run B:

```text
  check_isolated  30 rounds × 98 calls
                                  mean ±mad µs  95% CI vs base      checks/s
  ├─ &dyn Pulse → NoPulse         16.3 ±0.1µs  [16.3–16.4]µs        613M
  ├─ &dyn Pulse → tree(Unsto…     22.4 ±0.1µs  [+37.3%–+37.7%]      446M
  ├─ &dyn Pulse → tree(Stopp…     16.3 ±0.0µs  [-0.3%–-0.0%]        615M [1]
  ├─ may_stop gate → tree(Un…      0.0 ±0.0µs  [-100.1%–-99.9%]    4269G
  ╰─ impl Stop → Unstoppable       0.0 ±0.0µs  [-100.1%–-99.9%]    9324G
  advance_isolated  30 rounds × 46 calls
                                mean ±mad µs  95% CI vs base     reports/s
  ├─ &dyn Report → NoReport     18.6 ±0.2µs  [18.6–18.7]µs       537M
  ├─ &dyn Report → Reporter     21.7 ±0.2µs  [+15.6%–+16.1%]     461M
  ├─ &dyn Pulse → tree leaf     32.3 ±0.3µs  [+73.5%–+73.9%]     309M
  ╰─ Batch(64) → Reporter        5.7 ±0.1µs  [-71.0%–-70.5%]    1.77G
  codec_loop_chunk_4096  30 rounds × 70 calls
                         mean ±mad µs  95% CI vs base     iB/s
  ├─ NoPulse             14.0 ±0.0µs  [14.0–14.1]µs     17.4G
  ├─ tree(Unstoppable)   14.2 ±0.1µs  [+1.3%–+1.9%]     17.2G
  ╰─ tree(Stopper)       14.2 ±0.1µs  [+1.3%–+1.7%]     17.2G
  codec_loop_chunk_256  30 rounds × 49 calls
                         mean ±mad µs  95% CI vs base      iB/s
  ├─ NoPulse             18.1 ±0.1µs  [18.0–18.1]µs      13.5G
  ├─ tree(Unstoppable)   20.1 ±0.1µs  [+11.2%–+11.7%]    12.1G
  ╰─ tree(Stopper)       20.3 ±0.1µs  [+12.4%–+12.8%]    12.0G
  codec_loop_chunk_64  30 rounds × 26 calls
                         mean ±mad µs  95% CI vs base      iB/s
  ├─ NoPulse             26.8 ±0.2µs  [26.5–27.1]µs      9.12G
  ├─ tree(Unstoppable)   38.5 ±0.3µs  [+44.7%–+45.4%]    6.34G
  ╰─ tree(Stopper)       38.3 ±0.4µs  [+43.8%–+44.6%]    6.38G
  dyn_vs_generic_chunk_256  30 rounds × 49 calls
                                  mean ±mad µs  95% CI vs base      iB/s
  ├─ &dyn Pulse → tree            20.4 ±0.2µs  [20.3–20.4]µs      12.0G
  ├─ impl Pulse → tree            20.4 ±0.2µs  [-0.5%–+0.2%]      12.0G [1]
  ├─ impl Pulse → NoPulse         17.2 ±0.5µs  [-16.5%–-14.7%]    14.2G
  ├─ gated &dyn Pulse → NoPu…     16.0 ±0.2µs  [-22.0%–-21.5%]    15.3G
  ╰─ gated &dyn Pulse → tree      21.1 ±0.2µs  [+3.3%–+3.9%]      11.6G
```

Run C:

```text
  check_isolated  30 rounds × 98 calls
                                  mean ±mad µs  95% CI vs base      checks/s
  ├─ &dyn Pulse → NoPulse         16.4 ±0.1µs  [16.3–16.4]µs         610M
  ├─ &dyn Pulse → tree(Unsto…     20.5 ±0.1µs  [+24.9%–+25.2%]       488M
  ├─ &dyn Pulse → tree(Stopp…     14.3 ±0.1µs  [-12.8%–-12.6%]       699M
  ├─ may_stop gate → tree(Un…      0.0 ±0.0µs  [-100.2%–-99.7%]     4283G [1]
  ╰─ impl Stop → Unstoppable       0.0 ±0.0µs  [-100.2%–-99.8%]    11368G [2]
  advance_isolated  30 rounds × 46 calls
                                mean ±mad µs  95% CI vs base     reports/s
  ├─ &dyn Report → NoReport     16.5 ±0.1µs  [16.4–16.5]µs       607M
  ├─ &dyn Report → Reporter     21.5 ±0.1µs  [+30.1%–+30.6%]     466M
  ├─ &dyn Pulse → tree leaf     32.6 ±0.2µs  [+97.4%–+98.1%]     307M
  ╰─ Batch(64) → Reporter        5.5 ±0.1µs  [-67.7%–-67.2%]    1.83G
  codec_loop_chunk_4096  30 rounds × 69 calls
                         mean ±mad µs  95% CI vs base     iB/s
  ├─ NoPulse             14.0 ±0.1µs  [14.0–14.0]µs     17.4G
  ├─ tree(Unstoppable)   14.2 ±0.0µs  [+1.2%–+1.3%]     17.2G
  ╰─ tree(Stopper)       14.2 ±0.1µs  [+1.4%–+1.6%]     17.1G
  codec_loop_chunk_256  30 rounds × 49 calls
                         mean ±mad µs  95% CI vs base      iB/s
  ├─ NoPulse             18.3 ±0.1µs  [18.3–18.4]µs      13.3G
  ├─ tree(Unstoppable)   20.2 ±0.2µs  [+9.6%–+10.2%]     12.1G
  ╰─ tree(Stopper)       20.5 ±0.2µs  [+12.0%–+12.4%]    11.9G
  codec_loop_chunk_64  30 rounds × 26 calls
                         mean ±mad µs  95% CI vs base      iB/s
  ├─ NoPulse             26.7 ±0.1µs  [26.6–26.9]µs      9.13G
  ├─ tree(Unstoppable)   37.9 ±0.1µs  [+42.1%–+42.4%]    6.44G
  ╰─ tree(Stopper)       38.9 ±0.2µs  [+45.1%–+46.4%]    6.28G [1]
  dyn_vs_generic_chunk_256  30 rounds × 49 calls
                                  mean ±mad µs  95% CI vs base      iB/s
  ├─ &dyn Pulse → tree            20.5 ±0.2µs  [20.4–20.5]µs      11.9G
  ├─ impl Pulse → tree            20.4 ±0.2µs  [-0.5%–+0.3%]      11.9G [1]
  ├─ impl Pulse → NoPulse         17.5 ±0.6µs  [-15.5%–-13.4%]    14.0G
  ├─ gated &dyn Pulse → NoPu…     16.7 ±0.4µs  [-18.8%–-17.6%]    14.6G
  ╰─ gated &dyn Pulse → tree      21.4 ±0.2µs  [+4.4%–+5.4%]      11.4G
```
