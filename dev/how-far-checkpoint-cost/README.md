# how-far overhead matrix

Counts, with `perf`, what how-far costs for each observer an application
passes (`&NoPulse`, `StopOnly`, `PulseTree` with three stop policies, an
`FnPulse` callback, a `SharedPulse`, `WithStop`, `DiagnosticPulse`) crossed with
each way a library uses it (`check` or `step` per chunk, `live`, `Paced`, a
three-stage `Stages` plan, four workers sharing a stage, four fork-join
children). Every cell runs the same `#[inline(never)]` PNG Sub defilter over a
256 KiB buffer and is compared with the same work done without how-far.

```sh
python3 dev/how-far-checkpoint-cost/measure.py        # 1024 bytes per checkpoint
python3 dev/how-far-checkpoint-cost/measure.py 256    # another granularity
python3 dev/how-far-checkpoint-cost/measure.py stop   # enough stop policies alone
```

`stop` crosses every `enough`, `almost-enough` and `enough-tokio` stop policy
with six ways a loop checks it: through `&dyn Stop`, generic, `should_stop()`,
`may_stop()` asked once, every 16th chunk, and four workers sharing it.

Instructions are deterministic for serial work; thread spawn and join make the
parallel columns vary by a few thousand instructions per buffer. Cycles also
show contention between workers and depend somewhat on machine load. Results:
[`how-far-2026-10-06.md`](../../benchmarks/how-far-2026-10-06.md). CI builds
the harness so it keeps compiling; it does not run it.
