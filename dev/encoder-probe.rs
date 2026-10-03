//! Actual zenpng encode paths, deterministic input, byte parity and cancellation.
use enough::Unstoppable;
#[cfg(feature = "progress")]
use how_far_along::{prelude::*, *};
use imgref::ImgRef;
use rgb::Rgb;
use zenpng::{Compression, EncodeConfig};

fn pixels(size: usize) -> Vec<Rgb<u8>> {
    (0..size * size)
        .map(|i| {
            Rgb::new(
                (i * 17) as u8,
                ((i / size) * 13) as u8,
                ((i ^ (i / size)) * 7) as u8,
            )
        })
        .collect()
}
fn old(data: &[Rgb<u8>], size: usize, config: &EncodeConfig) -> Vec<u8> {
    zenpng::encode_rgb8(
        ImgRef::new(data, size, size),
        None,
        config,
        &Unstoppable,
        &Unstoppable,
    )
    .unwrap()
}
#[cfg(feature = "progress")]
fn encode(data: &[Rgb<u8>], size: usize, config: &EncodeConfig, p: &dyn Pulse) -> Vec<u8> {
    zenpng::encode_rgb8_with_pulse(ImgRef::new(data, size, size), None, config, p, &Unstoppable)
        .unwrap()
}
fn main() {
    let args: Vec<_> = std::env::args().collect();
    let mode = args.get(1).map(String::as_str).unwrap_or("old");
    let iterations: usize = args.get(2).and_then(|s| s.parse().ok()).unwrap_or(20);
    let size: usize = args.get(3).and_then(|s| s.parse().ok()).unwrap_or(128);
    let data = pixels(size);
    let config = EncodeConfig::default().with_compression(Compression::Fast);
    let baseline = old(&data, size, &config);
    let start = std::time::Instant::now();
    for _ in 0..iterations {
        let output = match mode {
            "old" => old(&data, size, &config),
            #[cfg(feature = "progress")]
            "nopulse" => encode(&data, size, &config, &NoPulse),
            #[cfg(feature = "progress")]
            "tree" => {
                let tree = PulseTree::new(Phase::new("png", Total::Unknown), Unstoppable);
                let output = encode(&data, size, &config, &tree);
                tree.finish(Outcome::Succeeded).unwrap();
                output
            }
            #[cfg(feature = "progress")]
            "callback" => {
                let pulse = FnPulse::new("png", |_| Ok(()));
                let output = encode(&data, size, &config, &pulse);
                pulse.finish(Outcome::Succeeded).unwrap();
                output
            }
            _ => panic!("unknown mode"),
        };
        assert_eq!(output, baseline);
        std::hint::black_box(output);
    }
    println!(
        "{{\"mode\":\"{mode}\",\"iterations\":{iterations},\"size\":{size},\"elapsed_ns\":{},\"png_bytes\":{}}}",
        start.elapsed().as_nanos(),
        baseline.len()
    );
}
#[cfg(all(test, feature = "progress"))]
mod tests {
    use super::*;
    #[test]
    fn serial_and_parallel_output_and_counts_match() {
        for parallel in [false, true] {
            let size = 64;
            let data = pixels(size);
            let mut config = EncodeConfig::default().with_compression(Compression::Fast);
            config.parallel = parallel;
            let baseline = old(&data, size, &config);
            let tree = PulseTree::new(Phase::new("png", Total::Unknown), Unstoppable);
            let observer = tree.observer();
            assert_eq!(encode(&data, size, &config, &tree), baseline);
            tree.finish(Outcome::Succeeded).unwrap();
            let snapshot = observer.snapshot();
            assert_eq!(snapshot.children.len(), 4);
            assert!(
                snapshot.children[0].completed > 0,
                "real verified strategy count"
            );
            assert_eq!(snapshot.fraction(), Some(1.0));
        }
    }
    #[test]
    fn callback_cancels_after_real_encoder_work() {
        let size = 64;
        let data = pixels(size);
        let config = EncodeConfig::default().with_compression(Compression::Fast);
        let pulse = FnPulse::new("png", |cx| {
            let snapshot = cx.snapshot();
            if snapshot.children.first().is_some_and(|s| s.completed > 0) {
                Err(StopReason::Cancelled)
            } else {
                Ok(())
            }
        });
        let observer = pulse.observer();
        let error = zenpng::encode_rgb8_with_pulse(
            ImgRef::new(&data, size, size),
            None,
            &config,
            &pulse,
            &Unstoppable,
        )
        .unwrap_err();
        assert!(matches!(
            error.error(),
            zenpng::PngError::Stopped(StopReason::Cancelled)
        ));
        pulse.finish(Outcome::Cancelled).unwrap();
        let snapshot = observer.snapshot();
        assert!(snapshot.children[0].completed > 0);
        assert_eq!(
            snapshot.children[0].status,
            Status::Finished(Outcome::Cancelled)
        );
        assert_eq!(
            snapshot.children[1].status,
            Status::Finished(Outcome::NotRun)
        );
    }
    #[test]
    fn enabling_progress_preserves_the_old_cancellation_api() {
        let size = 64;
        let data = pixels(size);
        struct Cancel;
        impl Stop for Cancel {
            fn check(&self) -> Result<(), StopReason> {
                Err(StopReason::Cancelled)
            }
        }
        let result = zenpng::encode_rgb8(
            ImgRef::new(&data, size, size),
            None,
            &EncodeConfig::default(),
            &Cancel,
            &Unstoppable,
        );
        assert!(result.is_err());
    }
}
