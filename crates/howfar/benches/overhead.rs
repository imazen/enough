use howfar::ext::ReportExt;
use howfar::{NoProgress, Phase, Report, Total};
use std::{hint::black_box, num::NonZeroU64, time::Instant};

fn measure(label: &str, mut f: impl FnMut()) {
    const N: u32 = 1_000_000;
    for _ in 0..10_000 {
        f();
    }
    let start = Instant::now();
    for _ in 0..N {
        f();
    }
    println!(
        "{label}: {:.2} ns/op",
        start.elapsed().as_nanos() as f64 / f64::from(N)
    );
}
fn main() {
    measure("loop/black_box baseline", || {
        black_box(1_u64);
    });
    measure("NoProgress", || NoProgress.advance(black_box(1)));
    let phase = Phase::new("bench", Total::Unknown);
    let progress = phase.progress();
    measure("Progress (uncontended, saturating)", || {
        progress.advance(black_box(1))
    });
    let mut batch = progress.batched(NonZeroU64::new(64).unwrap());
    measure("worker batch of 64", || batch.advance(black_box(1)));
    batch.flush();
    measure("Instant::now (consumer policy)", || {
        black_box(Instant::now());
    });
}
