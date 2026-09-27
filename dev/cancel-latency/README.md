# cancel-latency — cross-codec cancellation poll harness

Drives every zen codec/metric through an `almost_enough::PollMeter` on
deliberately adversarial inputs and prints the inter-poll gap report.
Two problem classes are flagged by the report itself:

- **SLOW** — a gap between polls ≥ 50 ms (cancellation unobserved that long)
- **STORM** — ≥ 1M polls with mean gap < 0.5 ms (the checks became a cost)

The harness adds one more verdict the meter can't see:

- **SILENT** — the operation ran > 50 ms wall-clock with < 2 polls total;
  there is no gap to measure because cancellation is simply never polled.

## Build & run

```sh
cd dev/cancel-latency
cargo build --release
cargo run --release -- list                 # list cases
cargo run --release -- <name>...            # run named cases
cargo run --release -- --histogram          # ASCII histograms for all
cargo run --release --features zenjxl,zensim -- zenjxl-decode-2048 zensim-1024
```

`zenjxl` and `zensim` are off by default (heavy dependency trees).

The `[patch.crates-io]` table in `Cargo.toml` is load-bearing: it unifies
every codec's `enough`/`almost-enough` deps onto the in-repo path copies —
otherwise the codecs' `enough::Stop` and the harness's meter would be
different package instances and the wrapper wouldn't typecheck. It also
mirrors the unpublished sibling patches (zenjxl-decoder 0.4.0, jxl-encoder
0.4.0, butteraugli 0.9.4, zenavif-serialize 0.2.0) that dependency
manifests cannot propagate.

## Measured results (2026-10-01, Zen 4 x86_64, release build)

| case | wall | polls | verdict |
|---|---|---|---|
| meter-overhead-1m | — | 1M | **62.7 ns/call** metered vs ~0 bare (Unstoppable elides) |
| zenflate effort 200, 256KB | 8.2 s | 184 | SLOW — 23 gaps ≥50ms, worst 80ms @ full_optimal.rs:1444 |
| zenflate effort 200, 16MB | 540 s | 2 760 | **SLOW — 2759/2759 gaps ≥50ms, worst 1.30s** — poll interval is positional, work per interval scales with input size |
| zenpng Maniac, 2048² | 111 s | 1 080 787 | **STORM (mean 103µs)** + SLOW — 124ms worst inside zenflate's recompress loop |
| zenpng decode, 2048² PNG | 38 ms | 2 048 | clean — one poll/row, max 45µs |
| zenjpeg encode progressive, 4K | 140 ms | 271 | clean — max 269µs |
| zenjpeg decode, 4K progressive | 88 ms | 493 | clean — max 8.2ms |
| zenwebp lossy m6, 1024² | 492 ms | 258 | SLOW — one 339ms gap ending in vp8l/encode.rs (alpha re-encode) |
| zenwebp lossless, 2048² | 1.34 s | 1 | **SILENT** — VP8L encoder never polls mid-operation |
| zengif encode, 64×512² frames | 4.5 s | 68 | SLOW — 17 gaps ≥50ms, worst **2.78s** in palette quantization (encoder.rs:543) |
| zenbitmaps pam roundtrip, 8K | 111 ms | 2 | SLOW — single 111ms gap; format writes are unchunked |
| zenavif decode, kodim03 | 16 ms | 16 | clean — max 1.4ms |
| butteraugli compare, 2048² | 637 ms | 1 | **SILENT** — compare_with_stop never polls mid-compare |
| fast-ssim2 compute, 2048² | 647 ms | 7 | SLOW — 3 gaps ≥50ms, worst 456ms @ pipeline/mod.rs:600 (per-stage polls only) |
| zenjxl decode, 2048² | 86 ms | 66 | clean — max 7.7ms |
| zensim codec_target, 1024² | 22 ms | 70 | clean — max 6.2ms |

### Reading the callsites

`#[track_caller]` on `enough::Stop` propagates through `&dyn Stop`,
`StopToken`, `Arc<dyn Stop>` and every almost-enough forwarding impl, so
`max gap A -> B` names the codec's poll sites, not the meter internals.
Callsites inside registry dependencies (e.g. zenflate-0.3.6 under zenpng)
resolve the same way.

### Runtime overhead

`meter-overhead-1m` measures the wrapper itself: **62.7 ns/call**
(Instant×2 + shared-mutex update + callsite record). At zenpng's storm
rate (~9.7k polls/s) that is ~0.6ms of added time over a 111s encode —
under any measurable threshold. The bare-`Unstoppable` arm optimizes to
~0ns, so the figure is the meter's full per-call cost.

## What the findings imply

- **zenflate**: `STOP_CHECK_INTERVAL` counts *input positions*, not time.
  At high effort the optimal-parse work per interval is super-linear in
  the chunk being processed; a time-based check (or checking inside the
  iterative-parse loop at full_optimal.rs:1444) would bound latency.
- **zenwebp**: `vp8l/encode.rs` takes `&dyn Stop` but only polls it at
  entry. Poll inside the backward-refs main loop.
- **zengif**: quantization (zenquant_impl) is a single uninstrumented
  phase; `encoder.rs:543` sees the resulting 2.78s gap.
- **butteraugli / zenbitmaps**: the stop is threaded but effectively
  unused — one poll at entry covers multi-hundred-ms operations.
- **fast-ssim2**: polls once per pipeline stage; the SIMD row loops
  inside each stage don't poll.
- **zenpng**: crusher iteration polls at ~103µs mean — a storm by the
  letter of the threshold but harmless at ~30ns/poll; the flag exists to
  catch *orders of magnitude worse* (e.g. polls per byte).
