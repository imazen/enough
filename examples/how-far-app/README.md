# Libraries, application, and intentional mistakes

Run from the workspace root:

```sh
cargo run -p how-far-example-app
cargo run -p how-far-example-app -- --json # one trace document per line
cargo test -p how-far-example-app
```

These are three separate crates with real dependency boundaries:

| Crate | Role | Production dependencies |
| --- | --- | --- |
| `how-far-example-codec` | Fast decode, optional fallback, own error enum | `how-far` |
| `how-far-example-pipeline` | Decode → transform → optional sharpen; own outer error | Core + codec |
| `how-far-example-app` | Chooses cancellation, tracking, diagnostics and final result handoff | Both libraries + all three how-far crates |

Both libraries are `no_std + alloc`. Their public signatures expose only core
`&dyn Pulse`, ordinary inputs, and their own `Result<T, Error>`. The codec uses
`Phases` for independently selected attempts; the pipeline uses `Stages` for
sequential work. Both convert `StopReason` into their own errors for `?`, and
implement `TryFrom<&Error> for StopReason` for borrowed classification. The outer
error delegates classification to its codec error without losing it.

The pipeline runs its body through `stages.complete_with(|stages| ...)`, which
hands the body's result to the plan even after an early `?`; the codec, with one
expression, calls `result.finish_phase(attempts)`. Every nested runner closes only the children it
owns. The application finally closes the root. Neither Rust's `?` nor `map` nor
Drop automatically communicates a plain Result to a progress owner.

| Execution | Codec attempts | Remaining pipeline stages | Root/result |
| --- | --- | --- | --- |
| Fast success | Succeeded, Skipped (inferred) | Transform succeeds; optional sharpen skipped | Succeeded / Ok |
| Recoverable unsupported fast path | Failed, Succeeded | Normal success | Succeeded / Ok, recovery evidence retained |
| Fatal decode | Failed, NotRun | NotRun | Failed / original nested codec error |
| Fallback also fails | Failed, Failed | NotRun | Failed / fallback's error |
| Cancellation | Cancelled, NotRun | NotRun | Cancelled / original nested stop error |

`convert_with_stop` accepts both a caller's child pulse and an additional borrowed
`&dyn Stop`. It uses `WithStop::borrowed`; the caller's stop and local stop both
apply through the pipeline and codec's child plans. The extra policy requires
neither an owned token nor a tracking-specific argument.

The app also runs deliberately buggy library variants:

| Omission or misuse | What the caller receives | Observation / diagnosis |
| --- | --- | --- |
| Drop plan without final handoff | Original Ok | Unused children Abandoned |
| `?` escapes before handoff | Original library error | Unused independent phases Abandoned |
| Drop root without result | No new error | Root Abandoned |
| Start child, omit its completion | Original parent Ok | Child Abandoned, parent Succeeded |
| Omit reports | Original Ok | Exact-total CountMismatch |
| Swallow cancellation | Buggy Ok preserved | StopOutcomeMismatch if instrumented stop was observed |
| Reclassify cancellation as input error | Buggy error preserved | StopOutcomeMismatch |
| Reuse a stage index | Original work result | ProtocolMisuse; fallback retains cancellation |
| Count before planning | Original work result | Plan rejected, diagnosed; untracked children retain checks |
| Report to branch / finished shared view | Original work result | InvalidReport; terminal snapshot unchanged |
| Omit required untouched phase | Original Ok | InferredCompletion; indistinguishable from optional work |
| Forget final `Paced::finish` | Potentially Ok after stop request | Drop counts last batch; host timing may flag UnobservedCancellation |

Diagnostics cannot recover intent or fix a library that swallows its own errors.
Tests assert both the original result and the observation, including the limits
of what can be inferred. A sink that rejects every plan confirms that rejected
tracking cannot replace even a bare `Result<(), StopReason>`.
