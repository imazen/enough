#!/usr/bin/env python3
"""Compare fresh-target default builds, with warm toolchain/filesystem caches."""
import argparse
import json
import os
from pathlib import Path
import statistics
import subprocess
import tempfile
import time

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--runs", type=int, default=5)
args = parser.parse_args()
if args.runs < 1:
    parser.error("--runs must be positive")
root = Path(__file__).resolve().parent.parent
# Resolve the workspace outside measured intervals (no network during builds).
metadata = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--format-version=1", "--no-deps"], cwd=root))
interface = next(p for p in metadata["packages"] if p["name"] == "howfar")
assert interface["features"] == {}, "howfar's interface must not vary with features"
assert not [d for d in interface["dependencies"] if d["kind"] != "dev"], \
    "howfar must not compile consumer machinery or other normal/build dependencies"
print(subprocess.check_output(["rustc", "--version"], text=True).strip())
samples = {"enough": [], "howfar": []}
for run in range(args.runs):
    order = list(samples) if run % 2 == 0 else list(reversed(samples))
    for crate in order:
        with tempfile.TemporaryDirectory(prefix="howfar-build-") as target:
            env = dict(os.environ, CARGO_TARGET_DIR=target)
            started = time.perf_counter()
            subprocess.run(["cargo", "build", "--offline", "-p", crate],
                           cwd=root, env=env, check=True,
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            samples[crate].append(time.perf_counter() - started)
for crate, values in samples.items():
    print(f"{crate}: median {statistics.median(values):.3f}s; "
          + ", ".join(f"{value:.3f}" for value in values))
