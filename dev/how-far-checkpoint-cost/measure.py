#!/usr/bin/env python3
"""What how-far costs, for each observer an application passes x each way a
library uses it, counted with perf.

usage: measure.py [CHUNK]   (bytes of work per checkpoint; default 1024)

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
PARALLEL = {"pool-step", "pool-paced", "fork-join"}


def counts(observer, scenario, chunk, iterations):
    out = subprocess.run(
        ["perf", "stat", "-x,", "-e", "instructions:u,cycles:u", str(BIN), observer, scenario,
         str(chunk), str(iterations)], capture_output=True, text=True, check=True).stderr
    values = {}
    for line in out.splitlines():
        fields = line.split(",")
        if len(fields) > 2 and fields[2].startswith(("instructions", "cycles")):
            values[fields[2].split(":")[0]] = int(fields[0])
    return values


def main():
    chunk = int(sys.argv[1]) if len(sys.argv) > 1 else 1024
    subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=HERE, check=True)
    cells = [("none", "step"), ("none", "pool-step")]
    cells += [(o, s) for o, _ in OBSERVERS for s, _ in SCENARIOS]
    samples = {}
    for run in range(RUNS):
        for cell in cells if run % 2 == 0 else cells[::-1]:
            low, high = counts(*cell, chunk, LOW), counts(*cell, chunk, HIGH)
            for kind in ("instructions", "cycles"):
                samples.setdefault((cell, kind), []).append(
                    (high[kind] - low[kind]) / (HIGH - LOW))
    med = {key: statistics.median(values) for key, values in samples.items()}

    def base(scenario, kind):
        return med[("none", "pool-step" if scenario in PARALLEL else "step"), kind]

    print(subprocess.check_output(["rustc", "--version"], text=True).strip())
    checkpoints = 256 * 1024 // chunk
    print(f"\nOne 256 KiB buffer per operation, {chunk} bytes of work per checkpoint "
          f"({checkpoints} checkpoints). Without how-far: "
          f"{base('step', 'instructions'):,.0f} instructions serially, "
          f"{base('pool-step', 'instructions'):,.0f} on four workers.")
    for kind, title in (("instructions", "Extra instructions per buffer (and overhead)"),
                        ("cycles", "Extra cycles per buffer (and overhead)")):
        print(f"\n{title}:\n")
        print("| Observer | " + " | ".join(label for _, label in SCENARIOS) + " |")
        print("| --- | " + " | ".join("---:" for _ in SCENARIOS) + " |")
        for observer, label in OBSERVERS:
            cells_text = []
            for scenario, _ in SCENARIOS:
                extra = med[(observer, scenario), kind] - base(scenario, kind)
                cells_text.append(f"{extra:,.0f} ({100 * extra / base(scenario, kind):+.1f}%)")
            print(f"| {label} | " + " | ".join(cells_text) + " |")


if __name__ == "__main__":
    main()
