# ABLATION-enough — Conservative Public-API Review

**Date:** 2026-06-10
**Snapshot commit:** b06dcb303046 (main@origin)
**Snapshot file:** docs/public-api/enough.txt (74 default / 82 all-features items)
**Grep template:** `grep -rn "<SYMBOL>" /home/lilith/work/ --include="*.rs" 2>/dev/null | grep -v "/enough/" | grep -v "target/" | grep -v ".jj/" | grep -v "zen-arm-src/"`

## Summary

**0 items flagged.** The `enough` crate is the minimal core: the `Stop` trait (3 methods: `check`, `may_stop`, `should_stop`), `StopReason` enum (2 variants: `Cancelled`, `TimedOut` + 3 inquiry methods), `Unstoppable` struct, `Never = Unstoppable` type alias, and blanket `Stop` impls for `&T`, `&mut T`, `Option<T>`. The `std` feature adds `Box<T>` and `Arc<T>` impls (8 items).

zencodec re-exports this crate's types at `zencodec::StopToken` etc., making it a deeply embedded upstream dependency across the zen workspace.

## Items Investigated

All items are the narrow public surface of a trait definition crate. There are no free functions, no leaked internals, no zero-consumer items beyond the expected blanket impls (which are intentional API extension points).

- `Stop` trait: active consumer: zencodec, zenpng, zenjpeg, zenavif, every codec in the zen workspace.
- `StopReason`: discriminated by codec error types. Active.
- `Unstoppable` / `Never`: used in testing and no-op paths.
- `impl Stop for &T / &mut T / Option<T>`: standard ergonomics — enable pass-by-reference and optional cancellation without boxing.

## Flagged Items

None.

## Digest

- Snapshot: 74 (default) / 82 (all-features) items
- Flagged A: 0
- Flagged B: 0
- 0% of surface flagged
- Surface is minimal, coherent, and deeply consumed across the zen workspace
