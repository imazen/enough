# how-far checkpoint cost

Counts, with `perf`, what each checkpoint style (`step`, `live()`, `Paced`,
and in the matrix a bare `check()`) and a three-stage `Stages` plan cost around one `#[inline(never)]` PNG Sub
defilter, against the same loop with no checkpoints. Instruction counts do not
depend on machine load; cycles depend on it far less than wall time does.

```sh
python3 dev/how-far-checkpoint-cost/measure.py            # chunks 64,256,1024,4096
python3 dev/how-far-checkpoint-cost/measure.py 256,4096   # chosen chunks
python3 dev/how-far-checkpoint-cost/measure.py matrix     # report sinks × stop policies, and &NoPulse
```

The matrix crosses report sinks (none, a tree counter, a boxed callback) with
stop policies (`Unstoppable`, an `AtomicBool`, a stop callback) behind one
`&dyn Pulse` shell, adds a `PulseTree` with each stop, and measures `&NoPulse`
itself, which checkpoints recognize by its address.

Results:
[`how-far-checkpoint-cost-2026-10-01.md`](../../benchmarks/how-far-checkpoint-cost-2026-10-01.md),
[`how-far-checkpoint-matrix-2026-10-03.md`](../../benchmarks/how-far-checkpoint-matrix-2026-10-03.md),
and [`how-far-checkpoint-nopulse-2026-10-03.md`](../../benchmarks/how-far-checkpoint-nopulse-2026-10-03.md).
CI builds the harness so it keeps compiling; it does not run it.
