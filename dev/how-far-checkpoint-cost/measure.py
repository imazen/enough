#!/usr/bin/env python3
"""What how-far costs, for each observer an application passes x each way a
library uses it, counted with perf.

usage: measure.py [CHUNK]        how-far observers x library scenarios
       measure.py stop [CHUNK]   enough stop policies x check patterns
(CHUNK: bytes of work per checkpoint; default 1024)

Every cell runs the same #[inline(never)] defilter over a 256 KiB buffer and
is compared with the same work done without how-far: serially for the serial
scenarios and on four scoped threads for the parallel ones. Counts are the
slope between two iteration counts, which removes process startup, and
medians of five interleaved runs. Instructions are deterministic; cycles also
show contention between workers, and depend somewhat on machine load.
"""
from pathlib import Path
import statistics
import subprocess
import sys

HERE = Path(__file__).resolve().parent
BIN = HERE / "target" / "release" / "how-far-checkpoint-cost"
LOW, HIGH, RUNS = 200, 1000, 5
OBSERVERS = [
    ("nopulse", "`&NoPulse`"),
    ("stop-only", "`StopOnly` (an `AtomicBool` stopper)"),
    ("tree", "`PulseTree`, `Unstoppable`"),
    ("tree-stop", "`PulseTree` + stopper"),
    ("tree-stop-callback", "`PulseTree` + stop callback"),
    ("callback", "`FnPulse` (cold callback)"),
    ("shared", "`SharedPulse` of a tree + stopper"),
    ("with-stop", "`WithStop::borrowed` over a tree + stopper"),
    ("diagnostic", "`DiagnosticPulse` over a tree + stopper"),
]
SCENARIOS = [
    ("check", "`check` / chunk"),
    ("step", "`step` / chunk"),
    ("live", "`live` + `step`"),
    ("paced", "`paced(64 KiB)`"),
    ("stages", "3 `Stages`, paced"),
    ("pool-step", "4 workers, `step`"),
    ("pool-paced", "4 workers, paced"),
    ("fork-join", "4 children, paced"),
]
STOPS = [
    ("unstoppable", "`Unstoppable`"),
    ("stopper", "`Stopper`"),
    ("sync-stopper", "`SyncStopper`"),
    ("stop-ref", "`StopRef` (of a `StopSource`)"),
    ("child", "`ChildStopper`, depth 2"),
    ("fn-stop", "`FnStop` (cold function)"),
    ("or", "`OrStop` of two `Stopper`s"),
    ("timeout", "`WithTimeout<Stopper>`"),
    ("debounced", "`DebouncedTimeout<Stopper>`"),
    ("token", "`StopToken` of a `Stopper`"),
    ("boxed", "`BoxedStop` of a `Stopper`"),
    ("poll-meter", "`PollMeter<Stopper>`"),
    ("tokio", "`TokioStop`"),
]
PATTERNS = [
    ("dyn", "`&dyn Stop`, `check()?`"),
    ("generic", "generic `impl Stop`"),
    ("should-stop", "`should_stop()`"),
    ("gated", "`may_stop()` hoisted"),
    ("sparse", "every 16th chunk"),
    ("pool", "4 workers share it"),
]
PARALLEL = {"pool", "pool-step", "pool-paced", "fork-join"}


def counts(prefix, row, column, chunk, iterations):
    out = subprocess.run(
        ["perf", "stat", "-x,", "-e", "instructions:u,cycles:u", str(BIN), *prefix, row, column,
         str(chunk), str(iterations)], capture_output=True, text=True, check=True).stderr
    values = {}
    for line in out.splitlines():
        fields = line.split(",")
        if len(fields) > 2 and fields[2].startswith(("instructions", "cycles")):
            values[fields[2].split(":")[0]] = int(fields[0])
    return values


def grid(prefix, rows, columns, chunk, title):
    """One table per counter: each row x column against the same work without it."""
    serial, parallel = columns[0][0], next(c for c, _ in columns if c in PARALLEL)
    cells = [("none", serial), ("none", parallel)] + [(r, c) for r, _ in rows for c, _ in columns]
    samples = {}
    for run in range(RUNS):
        for cell in cells if run % 2 == 0 else cells[::-1]:
            low, high = counts(prefix, *cell, chunk, LOW), counts(prefix, *cell, chunk, HIGH)
            for kind in ("instructions", "cycles"):
                samples.setdefault((cell, kind), []).append(
                    (high[kind] - low[kind]) / (HIGH - LOW))
    med = {key: statistics.median(values) for key, values in samples.items()}

    def base(column, kind):
        return med[("none", parallel if column in PARALLEL else serial), kind]

    print(f"\n{title}, {chunk} bytes of work per check ({256 * 1024 // chunk} per 256 KiB "
          f"buffer). Without them: {base(serial, 'instructions'):,.0f} instructions serially, "
          f"{base(parallel, 'instructions'):,.0f} on four workers.")
    for kind in ("instructions", "cycles"):
        print(f"\nExtra {kind} per buffer (and overhead):\n")
        print("| | " + " | ".join(label for _, label in columns) + " |")
        print("| --- | " + " | ".join("---:" for _ in columns) + " |")
        for row, label in rows:
            text = []
            for column, _ in columns:
                extra = med[(row, column), kind] - base(column, kind)
                text.append(f"{extra:,.0f} ({100 * extra / base(column, kind):+.1f}%)")
            print(f"| {label} | " + " | ".join(text) + " |")


def main():
    args = sys.argv[1:]
    stop = bool(args) and args[0] == "stop"
    chunk = int(args[1 if stop else 0]) if len(args) > int(stop) else 1024
    subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=HERE, check=True)
    print(subprocess.check_output(["rustc", "--version"], text=True).strip())
    if stop:
        grid(["stop"], STOPS, PATTERNS, chunk, "enough stop policies x check patterns")
    else:
        grid([], OBSERVERS, SCENARIOS, chunk, "how-far observers x library scenarios")


if __name__ == "__main__":
    main()
