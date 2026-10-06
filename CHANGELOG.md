# Changelog

## [Unreleased]

### Added

- `almost-enough`: `PollMeter<S>` poll-latency instrumentation behind the
  opt-in `poll-meter` feature (implies `std`; ~10-15 ms compile cost, zero by
  default). Records inter-`check()`/`should_stop()` gaps into a 1 ms × 100
  bucket histogram with per-call-site attribution via `#[track_caller]`, flags
  gaps >= 50 ms (`PollProblem::SlowGap`) and >= 1M mostly-sub-0.5 ms calls
  (`PollProblem::PollStorm`), renders an ASCII histogram via
  `PollReport::histogram_ascii` / `{:#}`. `StopExt::metered()` wraps any stop.
- `enough`: `Stop::check` / `should_stop` / forwarding impls are now
  `#[track_caller]` so instrumented wrappers can attribute polls to the
  caller's source location — additive, no signature change.
- `dev/cancel-latency`: cross-codec adversarial harness driving every zen
  codec through `PollMeter` (excluded workspace; own `[patch.crates-io]`
  unifies codecs onto in-repo `enough`/`almost-enough`). First-run findings:
  zenflate effort-200 gaps scale with input size (1.3 s worst at 16 MB),
  zenwebp lossless and butteraugli compare are cancellation-blind mid-op,
  zengif quantization gaps reach 2.78 s, zenpng Maniac polls 1.08M times
  (~103 µs mean), fast-ssim2 has 456 ms per-stage gaps. PollMeter's own cost
  measures 62.7 ns/call. See `dev/cancel-latency/README.md`.

### Changed

- Dependency requirements written out in full instead of truncated to two
  components, at the versions already locked and tested: `tokio` 1.43 →
  1.53.1 and `tokio-util` 0.7 → 0.7.19 (in `enough-tokio` and `test-tokio`),
  `rayon` 1.10 → 1.12.0 (in `test-rayon`). `zenutils-apidoc` 0.1.0 → 0.1.1 in
  the workspace-excluded apidoc runner. Lockfile refreshed; a
  package-by-package diff confirms no zen-family crate moved (`zenbench` stays
  at 0.1.9, and its `0.1.6` requirement is deliberately left alone). Test suite
  unchanged at 29 suites / 417 passed / 0 failed.

### Added

- README: a complete construct-and-cancel example using
  `almost_enough::Stopper` — the producer side (how to make and flip a real
  cancellation token) was previously undocumented.

### Changed

- README: badge row moved inline on the H1 (dropped `branch=`, added lib.rs and
  the shared crosslink footer, license badge → `#license`) and a top-level
  `## Quick start` added; the crates.io README is now a generated, badge-free
  `README.crates.md` with absolute links (`readme = "../../README.crates.md"`).

### Fixed

- `almost-enough`: `DebouncedTimeout` reads the clock at least every 64
  checks. Calibrated on fast checks it used to read it only every ~94,000,
  so when checks then slowed down it stopped seconds to minutes late (60 s+
  at 1 ms per check). Now it is at most 64 of the slower checks late. A
  countdown replaces the per-check division, so a check costs fewer
  instructions (23 → 18); at 64 bytes of work per check the extra clock
  reads cost about 2 cycles per check. The docs' claim that overshoot
  never exceeds the target interval is corrected. See
  `benchmarks/debounced-timeout-2026-10-06.md`.
- TRADEOFFS.md / README.md: removed criterion-era hot-loop perf claims that
  the zenbench migration (PR #8) contradicted — the "StopToken(Stopper) 25%
  faster than generic" / "2.57µs beats 3.41µs" / "impl Stop is the slowest
  path" claims were code-layout artifacts of the old per-function harness.
  Docs now state the layout-immune codec finding (dispatch path is within
  noise on real workloads); the confirmed WithTimeout/table timings stay (#9).
- `enough-tokio` README: added the consumer `[dependencies]` block a copy-paster
  needs — `enough` (not re-exported, required for the `Stop` trait), `tokio-util`
  (provides `CancellationToken`), and the `tokio` features the examples use
  (`rt-multi-thread`/`macros`/`time`); previously only the crate's own dev-dep
  manifest was shown.
- `enough-ffi` README: reconciled `FfiCancellationToken::from_ptr` — documented its
  real signature and that it returns a `FfiCancellationTokenView` (not a
  `FfiCancellationToken`), is `unsafe`, treats a null pointer as never-cancelled,
  and is safe to poll from one thread while another cancels. Added a pure-C
  end-to-end snippet and a note that the `enough_*` symbols export only via a
  downstream `cdylib`/`staticlib`.
- Versioned public-API surface snapshots at `docs/public-api/<crate>.txt`
  for `enough`, `almost-enough`, `enough-tokio`, and `enough-ffi`,
  regenerated on every `cargo test` via
  `crates/enough/tests/public_api_doc.rs` (`ZEN_API_DOC=check` verifies in
  CI, `=off` skips; justfile recipes `api-doc` / `api-doc-check`).
