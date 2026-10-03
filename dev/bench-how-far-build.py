#!/usr/bin/env python3
"""Guard the build cost of how-far and how-far-along.

Fails the run if how-far gains features beyond additive adapters, a build script or a dependency other
than `enough`; if one `Stages::run_*` call site adds more than --ir-budget
lines of how-far's unoptimized LLVM IR to the caller's crate; or if
how-far-along's own unoptimized IR, with default features or with
`diagnostics`, grows past its budget.

With `perf`, also reports rustc instructions (as rustc-perf does; they do not
depend on machine load): how-far and how-far-along against an empty no_std
crate, and one `Stages::run_stoppable` call site against a plain function
call, for check, debug and release builds.
"""
import argparse
import json
import os
import re
import shlex
import shutil
import statistics
import subprocess
import tempfile
from pathlib import Path

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--runs", type=int, default=3, help="perf samples per measurement")
parser.add_argument("--ir-budget", type=int, default=120, help="IR lines per call site")
parser.add_argument("--along-budget", type=int, nargs=2, default=[18_000, 55_000],
                    metavar=("STD", "DIAGNOSTICS"), help="how-far-along IR lines")
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent

meta = json.loads(subprocess.check_output(
    ["cargo", "metadata", "--format-version=1", "--no-deps"], cwd=root))
how_far = next(p for p in meta["packages"] if p["name"] == "how-far")
assert how_far["features"] == {"adapters": []}, "only additive adapters are allowed in core"
assert [(d["name"], d["kind"]) for d in how_far["dependencies"] if d["kind"] != "dev"] \
    == [("enough", None)], "how-far may depend only on the lightweight enough interface"
assert all("custom-build" not in t["kind"] for t in how_far["targets"]), \
    "how-far must not have a build script"
print(subprocess.check_output(["rustc", "--version"], text=True).strip())

WORK = """
#[inline(never)]
fn work{i}(data: &mut [u8], pulse: &dyn Pulse) -> Result<u64, StopReason> {{
    let pulse = pulse.live();
    let mut sum = 0_u64;
    for chunk in data.chunks_mut(64) {{
        for byte in chunk.iter_mut() {{
            *byte = byte.wrapping_mul({m}).rotate_left(3);
            sum = sum.wrapping_add(u64::from(*byte));
        }}
        pulse.step(1)?;
    }}
    Ok(sum)
}}
"""


def probe(n, stages):
    body = "use how_far::prelude::*;\nuse how_far::StopReason;\n"
    body += "".join(WORK.format(i=i, m=2 * i + 3) for i in range(n))
    if stages:
        body += "use how_far::{PhaseSpec, RunError, Stages, Total};\n"
        plan = ", ".join(f'PhaseSpec::new("s{i}", 1, Total::Unknown)' for i in range(n))
        calls = "".join(f"    sum += stages.run_stoppable(|stage| work{i}(data, stage))?;\n"
                        for i in range(n))
        return body + ("pub fn run(data: &mut [u8], pulse: &dyn Pulse) -> Result<u64, RunError<StopReason>> {\n"
                       f"    let mut stages = Stages::new(pulse, &[{plan}])?;\n    let mut sum = 0;\n"
                       f"{calls}    stages.finish()?;\n    Ok(sum)\n}}\n")
    calls = "".join(f"    sum += work{i}(data, pulse)?;\n" for i in range(n))
    return body + ("pub fn run(data: &mut [u8], pulse: &dyn Pulse) -> Result<u64, StopReason> {\n"
                   f"    let mut sum = 0;\n{calls}    Ok(sum)\n}}\n")


SMALL, LARGE = 8, 32
PROBES = {
    "empty": "#![no_std]\nextern crate alloc;\npub use enough::Stop;\n",
    f"plain{SMALL}": probe(SMALL, False), f"plain{LARGE}": probe(LARGE, False),
    f"stages{SMALL}": probe(SMALL, True), f"stages{LARGE}": probe(LARGE, True),
}
PROFILES = {"check": ["check"], "debug": ["build"], "release": ["build", "--release"]}
# how-far-along feature sets, each built in its own workspace so cargo does
# not unify their features.
ALONG = {"std": "", "diagnostics": ', features = ["diagnostics"]'}


def perf_instructions(argv, cwd):
    out = subprocess.run(["perf", "stat", "-x,", "-e", "instructions:u", "--", *argv],
                         cwd=cwd, capture_output=True, text=True)
    if out.returncode != 0:
        raise RuntimeError(out.stderr[-2000:])
    return int(next(l for l in out.stderr.splitlines() if "instructions" in l).split(",")[0])


with tempfile.TemporaryDirectory(prefix="how-far-build-") as tmp:
    tmp = Path(tmp)
    for name, source in PROBES.items():
        (tmp / name / "src").mkdir(parents=True)
        (tmp / name / "src" / "lib.rs").write_text(source)
        (tmp / name / "Cargo.toml").write_text(
            f'[package]\nname = "{name}"\nversion = "0.0.0"\nedition = "2024"\n[dependencies]\n'
            f'enough = {{ path = "{root}/crates/enough", default-features = false, features = ["alloc"] }}\n'
            f'how-far = {{ path = "{root}/crates/how-far" }}\n')
    (tmp / "Cargo.toml").write_text(
        "[workspace]\nresolver = \"3\"\nmembers = [" + ", ".join(f'"{n}"' for n in PROBES) + "]\n")
    env = dict(os.environ, CARGO_TARGET_DIR=str(tmp / "target"), CARGO_INCREMENTAL="0")
    for name, features in ALONG.items():
        (tmp / "along" / name / "src").mkdir(parents=True)
        (tmp / "along" / name / "src" / "lib.rs").write_text("")
        (tmp / "along" / name / "Cargo.toml").write_text(
            f'[package]\nname = "along-{name}"\nversion = "0.0.0"\nedition = "2024"\n[dependencies]\n'
            f'how-far-along = {{ path = "{root}/crates/how-far-along"{features} }}\n[workspace]\n')
    commands = {}
    for profile, cargo_args in PROFILES.items():
        for cwd, tag in [(tmp, ""), *((tmp / "along" / name, f"/{name}") for name in ALONG)]:
            log = subprocess.run(["cargo", *cargo_args, "--offline", "-v", "--color=never", "--workspace"],
                                 cwd=cwd, env=env, capture_output=True, text=True, check=True).stderr
            for line in log.splitlines():
                if "Running `" in line and "--crate-name" in line:
                    argv = shlex.split(line.split("Running `", 1)[1].rstrip("`"))
                    argv = [a for a in argv if not a.startswith("--json") and a != "--error-format=json"]
                    crate = argv[argv.index("--crate-name") + 1]
                    if crate == "how_far_along":
                        commands[profile, crate + tag] = argv
                    elif not tag:  # how-far as a library sees it: enough without std.
                        commands[profile, crate] = argv

    def ir_lines(name, only_how_far=True):
        """Unoptimized IR lines, without debug-info records, of a debug build."""
        argv = [a for a in commands["debug", name] if not a.startswith("--emit")]
        out = tmp / "ir" / name.replace("/", "-")
        out.mkdir(parents=True)
        argv[argv.index("--out-dir") + 1] = str(out)
        subprocess.run([*argv, "--emit=llvm-ir"], cwd=tmp, env=env, check=True, capture_output=True)
        ir = next(out.glob("*.ll")).read_text()
        return sum(
            sum(1 for line in body.splitlines()[1:] if line.strip() and not line.lstrip().startswith("#dbg"))
            for symbol, body in ((m.group(1), m.group(0)) for m in
                                 re.finditer(r"^define [^\n]*?@(\S+?)\(.*?\n}\n", ir, re.S | re.M))
            if not only_how_far or "7how_far" in symbol)

    per_site = (ir_lines(f"stages{LARGE}") - ir_lines(f"stages{SMALL}")) / (LARGE - SMALL)
    print(f"how-far IR per Stages::run_stoppable call site: {per_site:.0f} lines "
          f"(budget {args.ir_budget})")
    assert per_site <= args.ir_budget, "Stages::run_* instantiates too much code per call site"
    for name, budget in zip(ALONG, args.along_budget):
        lines = ir_lines(f"how_far_along/{name}", only_how_far=False)
        print(f"how-far-along IR, {name}: {lines} lines (budget {budget})")
        assert lines <= budget, f"how-far-along with {name} compiles to more code than its budget"

    if shutil.which("perf") is None:
        print("perf not found; skipping instruction counts")
        raise SystemExit(0)
    try:
        crates = ["how_far", *PROBES, *(f"how_far_along/{name}" for name in ALONG)]
        samples = {(p, c): [] for p in PROFILES for c in crates}
        for _ in range(args.runs):
            for key in samples:
                samples[key].append(perf_instructions(commands[key], tmp))
    except RuntimeError:
        print("perf cannot count instructions here (see kernel.perf_event_paranoid); skipping them")
        raise SystemExit(0)
    m = {key: statistics.median(values) / 1e6 for key, values in samples.items()}
    print(f"{'rustc instructions (millions)':44} {'check':>7} {'debug':>7} {'release':>7}")
    rows = {
        "empty no_std crate": lambda p: m[p, "empty"],
        "how-far": lambda p: m[p, "how_far"],
        "how-far-along (std)": lambda p: m[p, "how_far_along/std"],
        "how-far-along (diagnostics)": lambda p: m[p, "how_far_along/diagnostics"],
        "plain function call, per call site": lambda p:
            (m[p, f"plain{LARGE}"] - m[p, f"plain{SMALL}"]) / (LARGE - SMALL),
        "Stages::run_stoppable, per call site": lambda p:
            (m[p, f"stages{LARGE}"] - m[p, f"stages{SMALL}"]) / (LARGE - SMALL),
    }
    for label, value in rows.items():
        print(f"{label:44} " + " ".join(f"{value(p):7.1f}" for p in PROFILES))
