#!/usr/bin/env python3
"""What how-far's checkpoints and plans cost, counted with perf.

Two tables:

- per checkpoint: one 256 KiB PNG Sub defilter, checked and counted once per
  chunk, with `step` through `&dyn Pulse`, with `live()`, and with `Paced`,
  into `NoPulse` and into a live `PulseTree`, against the same loop with no
  checkpoints;
- per operation: a three-stage `Stages` plan around that work, with
  `NoPulse` and with a fresh `PulseTree` per operation, pacing checkpoints
  (`op-*`) or stepping through the stage on every chunk (`opstep-*`).

`measure.py matrix [CHUNKS]` instead crosses report sinks (none, a tree
counter, a boxed callback) with stop policies (`Unstoppable`, an `AtomicBool`
`Stopper`, an `FnStop` calling a boxed callback) behind one `&dyn Pulse`
shell, for `step`, `live()` and `Paced`, plus a `PulseTree` with each stop.
Callbacks are cold functions that do nothing, so the grids show what reaching
them costs, not what they do.

Every variant calls the same `#[inline(never)]` defilter, so all run the same
hot loop at the same address. Counts are the slope between two iteration
counts, which removes process startup, and medians of five runs.
Instructions are deterministic. `cycles:u` are counted only while this process
runs, so they depend far less on other jobs than wall time does, but not
nothing: compare cycle differences against the spread column.
"""
import os
from pathlib import Path
import statistics
import subprocess
import sys

HERE = Path(__file__).resolve().parent
BIN = HERE / "target" / "release" / "how-far-checkpoint-cost"
LOW, HIGH, RUNS = 200, 1000, 5
CHECKPOINTS = ["none", "step-nopulse", "live-nopulse", "paced-nopulse",
               "step-tree", "live-tree", "paced-tree"]
OPERATIONS = ["op-none", "op-nopulse", "op-tree", "opstep-nopulse", "opstep-tree"]


def counts(variant, chunk, iterations, size):
    out = subprocess.run(
        ["perf", "stat", "-x,", "-e", "instructions:u,cycles:u", str(BIN), variant, str(chunk), str(iterations)],
        env=dict(os.environ, BUF=str(size)), capture_output=True, text=True, check=True).stderr
    values = {}
    for line in out.splitlines():
        fields = line.split(",")
        if len(fields) > 2 and fields[2].startswith(("instructions", "cycles")):
            values[fields[2].split(":")[0]] = int(fields[0])
    return values


def sample(variants, chunk, size):
    """Per-iteration counts: the slope between LOW and HIGH iterations."""
    samples = {}
    for run in range(RUNS):
        for variant in variants if run % 2 == 0 else variants[::-1]:
            low, high = counts(variant, chunk, LOW, size), counts(variant, chunk, HIGH, size)
            for kind in ("instructions", "cycles"):
                samples.setdefault((variant, kind), []).append((high[kind] - low[kind]) / (HIGH - LOW))
    return samples


STYLES = ["step", "live", "paced"]
REPORTS = [("none", "no report"), ("count", "tree counter"), ("call", "report callback")]
STOPS = [("none", "`Unstoppable`"), ("flag", "`AtomicBool`"), ("call", "stop callback")]


def matrix(chunks):
    variants = ["none"] + [f"m-{style}-{r}-{s}" for style in STYLES for r, _ in REPORTS for s, _ in STOPS]
    variants += [f"t-{style}-{s}" for style in STYLES for s, _ in STOPS]
    for chunk in chunks:
        samples = sample(variants, chunk, 256 * 1024)
        checkpoints = 256 * 1024 // chunk
        med = {key: statistics.median(values) for key, values in samples.items()}
        base_i, base_c = med["none", "instructions"], med["none", "cycles"]
        rows = [(r, label) for r, label in REPORTS] + [("tree", "`PulseTree`")]
        def variant(style, r, s):
            return f"t-{style}-{s}" if r == "tree" else f"m-{style}-{r}-{s}"
        print(f"\n### One checkpoint per {chunk} bytes ({checkpoints} per 256 KiB)\n")
        print(f"No checkpoints at all: {base_i:,.0f} instructions, {base_c:,.0f} cycles per buffer.\n")
        for style in STYLES:
            print(f"`{style}`, extra per checkpoint, instructions / cycles:\n")
            print("| | " + " | ".join(label for _, label in STOPS) + " |")
            print("| --- | " + " | ".join("---:" for _ in STOPS) + " |")
            for r, label in rows:
                cells = [f"{(med[variant(style, r, s), 'instructions'] - base_i) / checkpoints:.1f} / "
                         f"{(med[variant(style, r, s), 'cycles'] - base_c) / checkpoints:.1f}" for s, _ in STOPS]
                print(f"| {label} | " + " | ".join(cells) + " |")
            print(f"\n`{style}`, overhead against no checkpoints, instructions / cycles:\n")
            print("| | " + " | ".join(label for _, label in STOPS) + " |")
            print("| --- | " + " | ".join("---:" for _ in STOPS) + " |")
            for r, label in rows:
                cells = [f"{100 * (med[variant(style, r, s), 'instructions'] / base_i - 1):+.2f}% / "
                         f"{100 * (med[variant(style, r, s), 'cycles'] / base_c - 1):+.1f}%" for s, _ in STOPS]
                print(f"| {label} | " + " | ".join(cells) + " |")
            print()
        print("Cycle spread per variant (min–max of five):\n")
        print("| Variant | Cycles median | Spread |")
        print("| --- | ---: | ---: |")
        for v in variants:
            values = samples[v, "cycles"]
            print(f"| {v} | {statistics.median(values):,.0f} | {min(values):,.0f}–{max(values):,.0f} |")


def table(title, variants, chunk, size):
    samples = sample(variants, chunk, size)
    base = {kind: statistics.median(samples[variants[0], kind]) for kind in ("instructions", "cycles")}
    print(f"\n{title}\n")
    print(f"| Variant | Instructions | vs {variants[0]} | Cycles | vs {variants[0]} | Cycle spread |")
    print("| --- | ---: | ---: | ---: | ---: | ---: |")
    for variant in variants:
        instructions = statistics.median(samples[variant, "instructions"])
        cycles = samples[variant, "cycles"]
        print(f"| {variant} | {instructions:,.0f} | {100 * (instructions / base['instructions'] - 1):+.2f}% "
              f"| {statistics.median(cycles):,.0f} | {100 * (statistics.median(cycles) / base['cycles'] - 1):+.2f}% "
              f"| {min(cycles):,.0f}–{max(cycles):,.0f} |")


subprocess.run(["cargo", "build", "--release", "--quiet"], cwd=HERE, check=True)
print(subprocess.check_output(["rustc", "--version"], text=True).strip())
if len(sys.argv) > 1 and sys.argv[1] == "matrix":
    matrix([int(c) for c in (sys.argv[2] if len(sys.argv) > 2 else "64,256,4096").split(",")])
    raise SystemExit(0)
for chunk in [int(c) for c in (sys.argv[1] if len(sys.argv) > 1 else "64,256,1024,4096").split(",")]:
    table(f"Per 256 KiB buffer, one checkpoint per {chunk} bytes ({256 * 1024 // chunk} checkpoints):",
          CHECKPOINTS, chunk, 256 * 1024)
for size in (4 * 1024, 256 * 1024):
    table(f"Per operation of {size // 1024} KiB, three stages, one checkpoint per 1024 bytes:",
          OPERATIONS, 1024, size)
