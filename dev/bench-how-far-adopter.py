#!/usr/bin/env python3
"""What adopting how-far costs a real library's build: zenresize, before and after.

zenresize 8f73fe6 checks cancellation through `&dyn enough::Stop`; 3987153
adds `try_resize*_with_pulse` on how-far, ported here to the current API.
Both build against this checkout's enough, how-far and how-far-along. Reports:

- a clean build of zenresize's dependency graph: when how-far finishes, and
  when zenresize itself can start;
- rustc instructions for zenresize;
- rustc instructions for a crate that calls zenresize, because `Resizer<B>` is
  generic and its code is compiled in the caller's crate.

Builds with --offline, so zenresize's dependencies must already be in the
cargo cache. Instruction counts need perf. Takes a few minutes; not run in CI.
"""
import argparse
import json
import os
from pathlib import Path
import re
import shlex
import statistics
import subprocess
import tempfile

parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument("--repo", default="https://github.com/imazen/zenresize")
parser.add_argument("--before", default="8f73fe6")
parser.add_argument("--after", default="3987153")
parser.add_argument("--runs", type=int, default=3)
args = parser.parse_args()
root = Path(__file__).resolve().parent.parent
crates = root / "crates"
print(subprocess.check_output(["rustc", "--version"], text=True).strip())

CONFIG = """use zenresize::{Filter, PixelDescriptor, ResizeConfig, Resizer};
fn config() -> ResizeConfig {
    ResizeConfig::builder(128, 128, 64, 64)
        .filter(Filter::Lanczos)
        .format(PixelDescriptor::RGBA8_SRGB)
        .post_sharpen(0.6)
        .post_blur(0.4)
        .build()
}
"""
STOP = CONFIG + """pub fn resize(input: &[u8], stop: &dyn enough::Stop) -> Result<Vec<u8>, enough::StopReason> {
    Resizer::new(&config()).try_resize(input, stop)
}
"""
PULSE = CONFIG + """pub fn resize(input: &[u8], pulse: &dyn how_far::Pulse) -> Result<Vec<u8>, zenresize::ResizePulseError> {
    Resizer::new(&config()).try_resize_with_pulse(input, pulse)
}
"""
GIT = 'git = "https://github.com/imazen/enough", rev = "4b915e5beeba6dafa5830ae26cf7594ebebdf0ae"'
PATCHES = f"""
[patch."https://github.com/imazen/enough"]
enough = {{ path = "{crates}/enough" }}
how-far = {{ path = "{crates}/how-far" }}
how-far-along = {{ path = "{crates}/how-far-along" }}

[patch.crates-io]
enough = {{ path = "{crates}/enough" }}
"""


def run(cmd, cwd, env=None, text=True):
    return subprocess.run(cmd, cwd=cwd, env=env, check=True, capture_output=True, text=text)


def replace(path, old, new):
    text = path.read_text()
    assert text.count(old) == 1, f"{path}: expected one {old!r}"
    path.write_text(text.replace(old, new))


def project(path, name, dependencies, source):
    (path / "src").mkdir(parents=True)
    (path / "src" / "lib.rs").write_text(source)
    (path / "Cargo.toml").write_text(
        f'[package]\nname = "{name}"\nversion = "0.0.0"\nedition = "2024"\n\n'
        f"[dependencies]\n{dependencies}\n\n[workspace]\n\n[profile.dev]\nopt-level = 1\n{PATCHES}")


def rustc_command(cwd, cargo_args, crate, target):
    """The environment and argv cargo uses to compile `crate`'s library."""
    env = dict(os.environ, CARGO_TARGET_DIR=str(target), CARGO_INCREMENTAL="0")
    log = run(["cargo", *cargo_args, "--lib", "--offline", "-vv", "--color=never"], cwd, env).stderr
    for line in log.splitlines():
        if "Running `" in line and f"--crate-name {crate} " in line and "--crate-type lib" in line:
            words = shlex.split(line.split("Running `", 1)[1].rstrip("`"))
            variables = {}
            while "=" in words[0] and not words[0].startswith("/"):
                key, value = words.pop(0).split("=", 1)
                variables[key] = value
            return variables, [w for w in words if not w.startswith("--json") and w != "--error-format=json"]
    raise SystemExit(f"cargo did not compile {crate} in {cwd}")


def instructions(cwd, variables, argv):
    out = subprocess.run(["perf", "stat", "-x,", "-e", "instructions:u", "--", *argv], cwd=cwd,
                         env=dict(os.environ, **variables), capture_output=True, text=True)
    if out.returncode != 0:
        raise RuntimeError(out.stderr[-2000:])
    return int(next(l for l in out.stderr.splitlines() if "instructions" in l).split(",")[0])


def timings(cwd, cargo_args, scratch):
    """One clean build: total wall time, how-far's end, zenresize's start."""
    with tempfile.TemporaryDirectory(dir=scratch) as target:
        env = dict(os.environ, CARGO_TARGET_DIR=target, CARGO_INCREMENTAL="0")
        run(["cargo", *cargo_args, "--lib", "--offline", "--timings", "--color=never"], cwd, env)
        html = (Path(target) / "cargo-timings" / "cargo-timing.html").read_text()
    units = json.loads(re.search(r"const UNIT_DATA = (\[.*?\]);\n", html, re.S).group(1))
    libraries = {u["name"]: u for u in units if u["mode"] == "todo" and not u.get("target")}
    how_far = libraries.get("how-far")
    return {"total": max(u["start"] + u["duration"] for u in units),
            "how-far": how_far["start"] + how_far["duration"] if how_far else None,
            "zenresize": libraries["zenresize"]["start"]}


with tempfile.TemporaryDirectory(prefix="how-far-adopter-") as scratch:
    scratch = Path(scratch)
    run(["git", "clone", "--quiet", args.repo, str(scratch / "repo")], scratch)
    trees = {}
    for label, rev in (("before", args.before), ("after", args.after)):
        tree = scratch / label
        tree.mkdir()
        archive = run(["git", "archive", rev], scratch / "repo", text=False).stdout
        subprocess.run(["tar", "-x", "-C", str(tree)], input=archive, check=True)
        with open(tree / "Cargo.toml", "a") as manifest:
            manifest.write(PATCHES)
        trees[label] = tree
    # Port the adoption to the current how-far API.
    resize = trees["after"] / "src" / "resize.rs"
    replace(resize, "how_far::IgnoreProgress", "how_far::NoReport")
    replace(resize, "use how_far::{PhaseSpec, ProgressExt, Steps, Total};",
            "use how_far::{PhaseSpec, ProgressExt, Stages, Total};")
    replace(resize, "Steps::new(", "Stages::new(")

    consumers = {
        "Stop API, before adoption": (trees["before"], 'enough = { version = "0.4", default-features = false }', STOP),
        "Stop API, after adoption": (trees["after"], f"enough = {{ {GIT}, default-features = false }}", STOP),
        "Pulse API": (trees["after"], f"enough = {{ {GIT}, default-features = false }}\nhow-far = {{ {GIT} }}", PULSE),
    }
    for i, (label, (tree, dependency, source)) in enumerate(consumers.items()):
        path = scratch / f"consumer{i}"
        project(path, "consumer", f'zenresize = {{ path = "{tree}" }}\n{dependency}', source)
        consumers[label] = path

    profiles = {"check": ["check"], "debug": ["build"], "release": ["build", "--release"]}
    print("\nClean build of zenresize's dependency graph (seconds, medians):")
    print("| Tree | Profile | Total | how-far done | zenresize starts |")
    print("| --- | --- | ---: | ---: | ---: |")
    for profile in ("debug", "release"):
        for label, tree in trees.items():
            samples = [timings(tree, profiles[profile], scratch) for _ in range(args.runs)]
            done = [s["how-far"] for s in samples if s["how-far"] is not None]
            total = statistics.median(s["total"] for s in samples)
            start = statistics.median(s["zenresize"] for s in samples)
            how_far = f"{statistics.median(done):.2f}" if done else "-"
            print(f"| {label} | {profile} | {total:.2f} | {how_far} | {start:.2f} |")

    measured = {f"zenresize {label}": (tree, "zenresize") for label, tree in trees.items()}
    measured.update({f"caller, {label}": (path, "consumer") for label, path in consumers.items()})
    commands = {(label, p): rustc_command(cwd, cargo_args, crate, scratch / f"t{i}{p}")
                for i, (label, (cwd, crate)) in enumerate(measured.items())
                for p, cargo_args in profiles.items()}
    try:
        counts = {key: [] for key in commands}
        for _ in range(args.runs):
            for (label, p), (variables, argv) in commands.items():
                counts[label, p].append(instructions(measured[label][0], variables, argv))
    except (RuntimeError, FileNotFoundError):
        raise SystemExit("perf cannot count instructions here; skipping them")
    print("\nrustc instructions for one crate (millions, medians):")
    print("| Crate | check | debug (opt-level 1) | release |")
    print("| --- | ---: | ---: | ---: |")
    for label in measured:
        print(f"| {label} | " + " | ".join(
            f"{statistics.median(counts[label, p]) / 1e6:.1f}" for p in profiles) + " |")
