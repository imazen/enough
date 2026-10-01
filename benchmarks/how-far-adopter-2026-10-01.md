# how-far in a real library: zenresize build cost, 2026-10-01

- Command: `python3 dev/bench-how-far-adopter.py --repo ~/work/zen/zenresize --runs 3`
- enough at `a79aead` (this repository); zenresize before `8f73fe6`, after
  `3987153` (imazen/zenresize, branch `codex/howfar-along-resizer`), ported to
  the current how-far API by the script (`IgnoreProgress` → `NoReport`,
  `Steps` → `Stages`)
- rustc 1.98.1, AMD Ryzen 9 5900XT, other jobs running (load average 2.5)

zenresize's port is two renames in the library. Its five how-far tests pass
after porting them to the current ownership rules: the application owns and
finishes the root, and children come from `split_array`.

## Clean build of zenresize's dependency graph

Seconds from the start of a clean `cargo build --lib`, medians of three:

| Tree | Profile | Total | how-far done | zenresize starts |
| --- | --- | ---: | ---: | ---: |
| before | debug | 6.99 | - | 4.85 |
| after | debug | 7.03 | 0.28 | 4.92 |
| before | release | 6.82 | - | 5.03 |
| after | release | 6.81 | 0.22 | 5.00 |

how-far finishes about 4.6 s before zenresize can start, which waits on the
`syn` → `archmage` → `magetypes` → `linear-srgb` chain. The totals differ by
less than the run-to-run noise.

## rustc instructions for one crate

Millions, medians of three. zenresize's `dev` profile uses `opt-level = 1`,
and so do the callers here.

| Crate | check | debug | release |
| --- | ---: | ---: | ---: |
| zenresize before | 2590.0 | 33277.0 | 28577.3 |
| zenresize after | 2618.5 | 33309.6 | 28622.4 |
| caller, Stop API, before adoption | 26.1 | 425.7 | 2591.5 |
| caller, Stop API, after adoption | 26.2 | 440.3 | 2617.9 |
| caller, Pulse API | 26.6 | 571.8 | 2800.3 |

zenresize itself grows by 1.1% to check and under 0.2% to build: its resize
methods are on `impl<B: Background> Resizer<B>`, so their code is compiled in
the crate that calls them. The callers each compile one resize call and
nothing else, so the percentages there are of a very small crate.

- **A caller that keeps using the Stop API** pays 3.4% more in debug
  (14.6 million instructions). The adoption split the row loop into
  `resize_rows<R: Report>`, which the Stop path now calls with `NoReport`.
- **A caller that uses the Pulse API** pays 146 million instructions more in
  debug than the Stop API before adoption (+34%) and 209 million more in
  release (+8%). In the caller's IR at `opt-level = 1`, the pulse path adds
  about 530 lines. About 330 of them are zenresize's own code:
  `try_resize_into_with_pulse` and a second copy of `resize_rows`, for
  `dyn Pulse`. The other 200 or so are how-far's: three `run_stoppable`
  copies (129 lines), `complete` (20), and `Stages` drop glue and the
  `PhaseSpec` vector.

The second `resize_rows` comes from making a hot helper generic over its
report sink. Taking `&dyn Report` (or `&dyn Pulse`) instead compiles the loop
once, at the cost of an indirect call per reported batch.
