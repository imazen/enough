# how-far checkpoint cost

Counts, with `perf`, what each checkpoint style (`step`, `live()`, `Paced`)
and a three-stage `Stages` plan cost around one `#[inline(never)]` PNG Sub
defilter, against the same loop with no checkpoints. Instruction counts do not
depend on machine load; cycles depend on it far less than wall time does.

```sh
python3 dev/how-far-checkpoint-cost/measure.py            # chunks 64,256,1024,4096
python3 dev/how-far-checkpoint-cost/measure.py 256,4096   # chosen chunks
```

Results: [`benchmarks/how-far-checkpoint-cost-2026-10-01.md`](../../benchmarks/how-far-checkpoint-cost-2026-10-01.md).
CI builds the harness so it keeps compiling; it does not run it.
