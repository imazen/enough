# how-far-really

Opt-in diagnostics and checkpoint tuning for applications and library tests.
This crate depends on `how-far` and `how-far-along`; neither depends on it.
Rust 1.88 and `std` are required. No executor or background thread is started.

```rust
use how_far::{prelude::*, StopReason, Total, Unstoppable};
use how_far_along::{Phase, PulseTree};
use how_far_really::{diagnostics::{DiagnosticPulse, Options}, profile::{Profiler, StdClock}};

let profiler = Profiler::new(StdClock::new(), 128);
let pulse = DiagnosticPulse::new(
    PulseTree::new(Phase::new("encode", Total::Exact(1)), Unstoppable), &profiler);
let observer = pulse.observer();
let result = pulse.step(1); // or your_library::encode(input, &pulse)
result.finish_phase(pulse)?;
profiler.operation_returned();
let trace = profiler.snapshot().with_progress(observer.snapshot());
for finding in trace.diagnose(&Options::default()) {
    eprintln!("{finding}");
}
# Ok::<(), StopReason>(())
```

`profile` collects bounded spans, source sites, checkpoint/report gaps, callback
costs and cancellation timing. `diagnostics` adds `DiagnosticPulse`, protocol
incidents and findings. Explicitly record the host's cancellation request and
operation return to measure end-to-end cancellation latency. Per-worker spans
are needed to identify workers whose silence a shared phase might conceal.

Observations cannot prove program correctness. A missing result handoff produces
abandonment; a swallowed observed stop produces conflicting evidence; reporting
fewer units than an exact successful total produces a count mismatch. The
library's `Result` is always returned unchanged. A failed child inside a successful
parent may be a recovered attempt. An untouched phase inferred as skipped may be
optional or forgotten required work: no runtime observer can distinguish those
without more information. Findings describe evidence, not replacement errors.

Incident retention is bounded separately from spans, using the same configured
capacity. `dropped_incidents` and `dropped_spans` expose missing evidence. Timing
instrumentation has overhead and is intended for tests, diagnosis and tuning.
Report timing can be disabled. Finding text, thresholds and suggested weights
may evolve; match non-exhaustive `Kind` values with a wildcard.

Cancellation adapters have an observation boundary. A `WithStop` outside a
`DiagnosticPulse` can stop after the inner diagnostic check succeeds; replacing
the policy bypasses that check entirely. For stop timing, instrument the added
source with a profiler span as well. A trace without an observed stop is not proof
that an uninstrumented policy never stopped.

Only the `profile` and `diagnostics` modules are public. Import protocol types
from `how_far`, tracker types from `how_far_along`, and diagnostic types here.

```compile_fail,E0432
use how_far_really::Pulse;
```

```compile_fail,E0432
use how_far_really::PulseTree;
```

Trace JSON uses schema version 2 and embeds a complete versioned tracking JSON
document under `progress`. Node/span identifiers are local to the process/trace;
integer counts are strings to avoid JavaScript precision loss.
See the [example crates](../../examples/how-far-app/README.md) and
[API boundaries](../../docs/how-far-api.md).
