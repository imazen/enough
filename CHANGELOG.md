# Changelog

## [Unreleased]

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
