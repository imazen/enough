#!/usr/bin/env python3
"""What how-far's checkpoints and plans cost, counted with perf.

Two tables:

- per checkpoint: one 256 KiB PNG Sub defilter, checked and counted once per
  chunk, with `step` through `&dyn Pulse`, with `live()`, and with `Paced`,
  into `NoPulse` and into a live `PulseTree`, against the same loop with no
  checkpoints;
- per operation: a three-stage `Stages` plan around that work, with
  `NoPulse` and with a fresh `PulseTree` per operation.

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
OPERATIONS = ["op-none", "op-nopulse", "op-tree"]


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


def table(title, variants, chunk, size):
    samples = {}
    for run in range(RUNS):
        for variant in variants if run % 2 == 0 else variants[::-1]:
            low, high = counts(variant, chunk, LOW, size), counts(variant, chunk, HIGH, size)
            for kind in ("instructions", "cycles"):
                samples.setdefault((variant, kind), []).append((high[kind] - low[kind]) / (HIGH - LOW))
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
for chunk in [int(c) for c in (sys.argv[1] if len(sys.argv) > 1 else "64,256,1024,4096").split(",")]:
    table(f"Per 256 KiB buffer, one checkpoint per {chunk} bytes ({256 * 1024 // chunk} checkpoints):",
          CHECKPOINTS, chunk, 256 * 1024)
for size in (4 * 1024, 256 * 1024):
    table(f"Per operation of {size // 1024} KiB, three stages, one checkpoint per 1024 bytes:",
          OPERATIONS, 1024, size)
