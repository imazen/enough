# ABLATION-almost-enough — Conservative Public-API Review

**Date:** 2026-06-10
**Snapshot commit:** b06dcb303046 (main@origin)
**Snapshot file:** docs/public-api/almost-enough.txt (365 default / 365 all-features items; identical — no feature-gated additions)
**Grep template:** `grep -rn "<SYMBOL>" /home/lilith/work/ --include="*.rs" 2>/dev/null | grep -v "/enough/" | grep -v "target/" | grep -v ".jj/" | grep -v "zen-arm-src/"`

## Summary

**0 items flagged.** The `almost-enough` crate is a rich but intentional combinator layer built on `enough::Stop`. Every exported type is a distinct cancellation primitive serving a different use case. Consumer grep confirmed active use across zenpng, zenavif, hdr-editor, zengif, and zencodec documentation.

Known consumers as of this scan:
- `OrStop`: zenpng/compress.rs (production use: combining deadline + cancel), pre-filter/zenpng
- `Stopper`: hdr-editor/app.rs, zengif/cancellation.rs (tests + docs)
- `StopExt`: zenavif/animation_decode.rs
- `zencodec::StopToken` (re-exports `almost_enough::StopToken` via zencodec)
- Downloaded crate mirror `almost-enough-0.3.1` confirms the full documented API surface

## Cancellation Primitive Roster (all KEEP)

| Type | Role | Consumer hits |
|------|------|--------------|
| `Stopper` | Arc-based thread-safe cancel handle (Relaxed ordering) | zengif, hdr-editor |
| `SyncStopper` | Arc-based with Acquire/Release ordering for memory-visible writes | — (distinct from Stopper by design) |
| `StopToken` | Clone-once type-erased handle for passing into tasks | zencodec re-export; zenavif |
| `StopSource` | Stack-based, zero-alloc, borrowed token via `StopRef` | — |
| `StopRef<'a>` | Borrowed view of `StopSource`; Copy + stack-only | — |
| `ChildStopper` | Hierarchical parent-child cancellation tree | — |
| `BoxedStop` | Type-erased dyn-dispatch Stop without Arc clone | — |
| `FnStop<F>` | Closure-based stop adapter | — |
| `OrStop<A, B>` | Combinator: stop if either A or B stops | zenpng (production) |

`SyncStopper`, `ChildStopper`, `BoxedStop`, `FnStop`, `StopSource`, `StopRef` — zero direct org hits outside the crate and downloaded mirror. However, these are each documented distinct primitives serving a specific niche (`SyncStopper` for cross-thread memory ordering; `ChildStopper` for hierarchical cancel trees; `FnStop` for closure adapters). All are legitimately KEEP under the conservative bar: "0 external consumers" alone is not enough to flag; the bar requires clear public-API mistakes. These are not mistakes.

## Trait Surface (all KEEP)

| Trait | Role |
|-------|------|
| `Cancellable` | Bound for types that can be cancelled (implemented by `Stopper`, `ChildStopper`); needed for `CancelGuard<C>` |
| `CloneStop` | Supertrait alias: `Stop + Clone + 'static`; bound for `into_token()` / `child()` |
| `StopExt` | Extension methods: `or()`, `into_boxed()`, `into_token()`, `child()` |
| `StopDropRoll` | RAII cancel-on-drop guard factory via `stop_on_drop()` |
| `DebouncedTimeoutExt` | Extension: `with_debounced_timeout()` / `with_debounced_deadline()` |
| `TimeoutExt` (in `time`) | Extension: `with_timeout()` / `with_deadline()` |

## Items with `time` module (KEEP)

`WithTimeout<T>` and `DebouncedTimeout<T>` are time-based Stop wrappers with tighten/deadline APIs. No external hits, but they are named extension-trait targets and are the stated purpose of `DebouncedTimeoutExt` / `TimeoutExt`. Their absence would make the extension traits useless.

`CancelGuard<C>` — RAII guard that calls `C::stop()` on drop unless `disarm()`'d. Correct companion to `Cancellable` / `StopDropRoll`.

## Flagged Items

None.

## Digest

- Snapshot: 365 (default) / 365 (all-features) items — feature-invariant
- Flagged A: 0
- Flagged B: 0
- 0% of surface flagged
- Wide ecosystem: every item is a named, documented cancellation primitive. The 0-hit items are variants serving specific niche use cases, not accidents.
