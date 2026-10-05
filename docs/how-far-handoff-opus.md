# how-far handoff to Claude Opus xhigh — 2026-10-05

Review and improve the **full intended delta from main**, then make it reviewable
for PR #15. Keep library-author ergonomics and compilation cost central. The
deliverable is not merely the delta from the callback branch, nor the older
Git-pinned diagnostics experiment.

## Read and compare

Read this file, [current API boundaries](how-far-api.md),
[the design](how-far-design.md), [testing/tuning](how-far-testing-and-tuning.md),
and [current validation](../benchmarks/howfar-results-validation.md). Then read
the public crate READMEs and the actual source and tests for any change.

The source checkout `/home/lilith/work/enough--howfar-v2` is clean at
`a1d0286` on `codex/howfar-v2`. A writable local copy with the documentation
refresh is prepared at `/tmp/enough-opus-handoff-20261005`. In that copy,
`origin` points to the local enough repository, not GitHub, and its
`origin/main` is the recorded main base `6bd6b96`. The prepared copy also merges
that main commit: original v2 branched at `9e1231a`, so it otherwise omits main's
18-line cancel-latency results table. The handoff preserves that existing main
documentation. Do not push to the copy's local origin expecting a PR update.

```sh
git log --oneline origin/main..HEAD
git diff --stat origin/main...HEAD
git diff origin/main...HEAD -- crates/how-far crates/how-far-along crates/how-far-really
```

These comparisons include the full implementation and the handoff documentation.
Most additions are new crates, tests, examples, fixtures and measurements.
The copy's merge base is now `6bd6b96`, so the PR-style three-dot comparison and
the endpoint comparison against recorded main agree on the intended file delta.
Refresh remote main before landing and preserve any newer unrelated changes.

## Branches, PR and concurrent work

| Reference | Recorded state | Meaning |
| --- | --- | --- |
| enough `main` / `origin/main` | `6bd6b96` | Base for the intended full delta |
| `codex/howfar-progress` / recorded remote | `d61d9a5` | Last PR #15 head checked in the prior session |
| `codex/howfar-diag-seams` | `3ca2b81` | Sonnet follow-up: stage entry timing, covering stop spans, negligible weights |
| `codex/howfar-callback` | `0adafe1` | Earlier callback/performance baseline |
| `codex/howfar-v2` | `a1d0286` | Latest local implementation; includes prior work and Result-based redesign |

Remote refs/PR status were not refreshed on 2026-10-05. PR #15 is
<https://github.com/imazen/enough/pull/15>. Its earlier `d61d9a5` CI run passed
all checks on 2026-10-01. That is not CI validation of v2. Check current remote
state before updating the PR, and preserve any subsequent work.

The original `/home/lilith/work/enough` checkout is on main with a large staged
and unstaged older howfar draft. Preserve it. Do not reset, overwrite, rename or
commit that mixed checkout as the intended v2 result. The callback checkout is
also a separate baseline. The former `/tmp/howfar-pr-20260929` checkout no longer
exists; its commits remain in the original repository.

The zenav1-svt checkout has five pending files on `codex/howfar-probe` at
`21583ed73`. Their contents are described in
[the measurement record](how-far-zenav1-measurement.md). Preserve them before any
branch/dependency migration. Zenresize's earlier draft integration is PR #16;
it remains on an older API and was not migrated by the v2 work.

## Intended contract and constraints

- Libraries depend only on `how-far`, expose `&dyn Pulse`, and return their own
  ordinary `Result<T, E>`. Do not introduce tracker or diagnostic dependencies
  into codec production paths. Keep hot codec loops non-generic over pulse types.
- `check()` is the frequent cancellation/checkpoint operation. `advance(n)`
  counts only; `step(n)` counts completed work, then checks. Keep independent
  cheap stop checks where reports are sparse. Consumer cadence and callback work
  determine presentation smoothness. The diagnostic defaults target 10 ms stop
  gaps, 50 ms report gaps, and 10 ms callback duration and start-to-start cadence.
- Completion belongs to a consuming owner. A library never finishes its borrowed
  input. `Stages` describes a sequence; `Phases` describes independently chosen
  attempts. Capture the enclosing result and explicitly hand it to
  `complete(result)` / `result.finish_phase(owner)`. Observation failures cannot
  replace the library's result. Explicit administration still reports errors.
- `share()` gives owned workers the full Pulse interface, retaining planning,
  checks and total revisions but no completion rights. Join workers before
  completing the owner. Unsupported sharing is explicit. Legacy `handle()` is
  weaker; prefer `share()` in new owned contexts.
- Preserve meaningful nested plans, serial → parallel → serial stages, asymmetric
  workers, repeated joins and shared-counter workloads. Do not make worker count
  change the containing phase's weight. Pacing batches reports/checkpoints and
  therefore affects cancellation cadence; finish the last batch explicitly.
- `how-far` is `no_std + alloc`, Rust 1.86, only `enough` as a production
  dependency. Its optional `adapters` and `checked` features add items without
  changing protocol semantics. Existing enough crates keep Rust 1.85.
- `how-far-along` is the tracker, Rust 1.88, default `std,json`, with additive
  `callback` and `adapters`. `how-far-really` is separate std-only profiling and
  diagnostics, Rust 1.88. Neither core nor tracker depends on it.
- Forbid unsafe code. Use native atomics for counters; std metadata locks or a
  correctly configured multicore critical-section provider for no_std. No lock
  spans user callbacks or host suspension. Counters do not publish codec buffers.
- Callback views are lazy and can be inspected on the calling worker. Host-driven
  pollers preserve UI-thread callbacks. Synchronous Wasm callbacks do not yield;
  workers, resumable chunks or a real JSPI suspension boundary provide yielding.
  The browser evidence covers Playwright WebKit, not packaged Safari.
- Keep extensible enums/records non-exhaustive, make public ownership/errors
  clear, and disclose optional machinery progressively. Don't add heavy default
  dependencies, macros, code generation or a runtime to the library interface.

## What is implemented

V2 unifies tracker and callback accounting, adds owned full-capability views,
explicit start/revised totals, frozen terminal observations and owner-only
completion. `NotRun` prevents failure tails from earning progress; `Skipped`
discharges unnecessary work; missing handoffs are `Abandoned`. Failed attempts
can remain under a recovered successful parent. JSON schema 2 preserves that
state and inferred-completion provenance.

The latest result helpers preserve non-Clone error identity and use borrowed
cancellation classification. `WithStop` supports owned/borrowed additional
policies and explicit replacement; replacement also bypasses inner callbacks.
Optional `TryStages`/`RunError` is the checked legacy layer. Separate example
crates exercise actual dependency boundaries and intentional mistakes.

Diagnostics measures source sites, report gaps and stop gaps separately,
covering spans, callback costs/cadence, stage weights, incidents and missing
evidence. Sequential stages are timed from entry even before their first report.
The Sonnet fix keeps negligible stage weights rather than overfitting a tiny
flush. Shared-worker spans can conceal individual tails; use per-worker spans
when that distinction matters. A trace cannot infer whether omitted work was
optional, prove fallback correctness, or observe an uninstrumented stop source.

## zenav1-svt findings and remaining adoption work

The real 512×512 patterned two-frame probe took about 100–103 ms and reported
only twice, leaving an 84–87 ms plateau. A separate inner stop measurement made
1,558 checks in about 78–85 ms; its final maximum gap was 0.475328 ms. This
supports finer reporting inside a frame, not automatically more cancellation
checks. Those were diagnostic runs on one synthetic input, not a calibrated
throughput or ETA benchmark. No subscriber callback was measured there.

The pending old probe pins `d61d9a5` and still uses the obsolete tracker
diagnostics feature. Migrate it to the current three-crate API. A current stage's
instrumented `share()` can enter the encoder's existing owned `with_stop`
context. Keep all timings on one profiler if using covering-span evidence.
Git-source and registry-source enough traits currently have different crate
identities; use a test-only source unification or an explicit bridge, not an
unnecessary production dependency migration.

Then identify actual completed units inside mode-decision / superblock walks,
filters and entropy work. Report their success boundaries, including parallel
tile joins, without cloning codec loops or making a check count into progress.
Measure representative inputs and presets before setting stage weights. The
pending 99:1 frame/flush change is provisional and must not become a general
encoder default solely from this experiment.

## Validation and next steps

The 2026-10-04 [validation report](../benchmarks/howfar-results-validation.md)
records a full workspace run (634 tests/doctests), focused final tests, feature
combinations, MSRV builds, no_std cross-builds, six browser tests, the Wasm host
probe, trace JSON parsing and three real zenpng adoption tests. These are dated
local results, not newly run tests or remote CI today. Cold-build samples and
source fingerprints are linked there; preserve them as evidence for that build.

The current build guard caps core overhead at 120 IR lines per `Stages::run`
site, tracker at 22,000, diagnostics at 35,000. Recorded final values are
79 / 18,657 / 31,694 with Rust 1.99. Final default core release cold-build median
was 0.318 s versus 0.263 s for the older callback baseline on that host. Don't
present the earlier redesign's smaller core as the final result. The zenpng
feature-on release compiler-instruction increment was 0.70% for its full graph.

Proceed with an adversarial review of the full main delta and the current
library-facing examples. Fix warranted issues, update API snapshots/docs, and
run checks appropriate to the resulting changes. Validate real integrations
against the actual head before claiming migration. If publishing/PR updating is
part of the follow-up, first inspect remote ancestry and CI; do not merge or
publish crates as part of this handoff.

```sh
cargo test --workspace --all-features
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo hack check --feature-powerset --no-dev-deps -p how-far -p how-far-along -p how-far-really
cargo +1.86 test -p how-far --all-features
cargo +1.88 test -p how-far-along --all-features -p how-far-really -p how-far-example-app
ZEN_API_DOC=check cargo test --manifest-path apidoc/Cargo.toml
python3 dev/bench-how-far-build.py --runs 3
```

Run measurements serially; don't repeat all costly checks for prose-only edits.
Zenav1 precheck was incomplete because its C reference submodule and PyYAML were
missing, and targeted warnings-denied Clippy failed in existing DSP code. Do not
erase those limitations or claim codec parity from these progress experiments.

## This handoff's environment

On 2026-10-05 the session permits writes only in `/home/lilith/work/enough` and
`/tmp`, with the original `.git` read-only. Hence the docs refresh is in the
writable copy, and a patch is saved in the original checkout's docs directory.
That patch applies to source head `a1d0286` and includes the existing main table
along with the documentation refresh. It does not overwrite the dirty main
checkout or commit the zenav1-svt experiment.
The Herdr socket call was denied by the sandbox, so no new Opus tab was created.
`/home/lilith/work/enough/dev/launch-how-far-opus.py` is a prepared user-run launcher for a new tab in
the copy, selecting `--model opus --effort xhigh`. It does not auto-answer a
startup trust/approval prompt. The prior user-approved trust was for the former
PR checkout; preserve normal Herdr agent-control and repository trust rules.
