# Changelog

## [Unreleased]

### Added

- README: a complete construct-and-cancel example using
  `almost_enough::Stopper` — the producer side (how to make and flip a real
  cancellation token) was previously undocumented.
- Versioned public-API surface snapshots at `docs/public-api/<crate>.txt`
  for `enough`, `almost-enough`, `enough-tokio`, and `enough-ffi`,
  regenerated on every `cargo test` via
  `crates/enough/tests/public_api_doc.rs` (`ZEN_API_DOC=check` verifies in
  CI, `=off` skips; justfile recipes `api-doc` / `api-doc-check`).
