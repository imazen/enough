# Changelog

## [Unreleased]

### Added

- `impl core::error::Error for StopReason` — re-added (it was removed in 0.4.0).
  `StopReason` stays a leaf cause (`source()` is `None`); the impl lets a
  consumer expose a wrapped `StopReason` through its own error's `source()`
  chain so a generic caller can classify cancellation/timeout by downcast
  (e.g. `zencodec::CodecErrorExt::cancelled` / `stop_reason`) without naming
  the concrete error enum. `core::error::Error` (Rust 1.81+, MSRV here is 1.85)
  is available in `no_std` with no feature flag, so the `no_std` surface is
  unchanged. Additive — re-adding the impl is non-breaking.
- README: a complete construct-and-cancel example using
  `almost_enough::Stopper` — the producer side (how to make and flip a real
  cancellation token) was previously undocumented.

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
