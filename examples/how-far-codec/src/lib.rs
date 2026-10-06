//! A pretend image codec that reports through `how-far` the way a real one
//! would. It depends only on `how-far`, never on a tracker, and returns its own
//! errors.
//!
//! It shows every shape a library takes: independently chosen attempts with a
//! fallback (`decode`), sequential stages around a Rayon stage and a coder
//! that owns its stop policy (`encode`), and, with `std`, fork-join children
//! on scoped threads, `'static` workers and recursive `rayon::join`.
#![cfg_attr(not(feature = "std"), no_std)]
#![forbid(unsafe_code)]
extern crate alloc;

use alloc::vec::Vec;
use how_far::{Execution, PhaseSpec, Phases, Stages, StopReason, Total, prelude::*};

/// Rows per tile.
pub const TILE_ROWS: usize = 16;

/// A grayscale image.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Image {
    /// Pixels per row.
    pub width: usize,
    /// Rows.
    pub height: usize,
    /// Row-major pixels.
    pub pixels: Vec<u8>,
    /// A row the analyzer rejects, to model corrupt input.
    pub damaged_row: Option<usize>,
}

impl Image {
    /// A deterministic test pattern.
    pub fn pattern(width: usize, height: usize) -> Self {
        let pixels = (0..width * height)
            .map(|i| ((i.wrapping_mul(73) ^ (i / width).wrapping_mul(131)) & 255) as u8)
            .collect();
        Self {
            width,
            height,
            pixels,
            damaged_row: None,
        }
    }

    /// The same image with one row the analyzer will reject.
    pub fn damaged_at(mut self, row: usize) -> Self {
        self.damaged_row = Some(row);
        self
    }

    /// Number of tiles of [`TILE_ROWS`] rows, counting a short final tile.
    pub fn tiles(&self) -> usize {
        self.height.div_ceil(TILE_ROWS)
    }

    fn tile(&self, index: usize) -> &[u8] {
        let start = index * TILE_ROWS * self.width;
        let end = ((index + 1) * TILE_ROWS * self.width).min(self.pixels.len());
        &self.pixels[start..end]
    }
}

/// The codec's own error. Progress bookkeeping never adds a variant to it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Error {
    /// A cancellation request or timeout.
    Stopped(StopReason),
    /// Input the codec cannot handle.
    Corrupt {
        /// The rejected row.
        row: usize,
    },
    /// The fast path does not support this input; a fallback may.
    Unsupported,
}
impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Stopped(reason) => reason.fmt(f),
            Self::Corrupt { row } => write!(f, "corrupt input at row {row}"),
            Self::Unsupported => f.write_str("fast path does not support this input"),
        }
    }
}
impl core::error::Error for Error {}
impl From<StopReason> for Error {
    fn from(reason: StopReason) -> Self {
        Self::Stopped(reason)
    }
}
impl IsStop for Error {
    fn stop_reason(&self) -> Option<StopReason> {
        match self {
            Self::Stopped(reason) => Some(*reason),
            _ => None,
        }
    }
}

/// How [`decode`]'s attempts behave, to show recovery and failure.
#[derive(Clone, Copy, Debug)]
pub enum Mode {
    /// The fast path succeeds.
    Fast,
    /// The fast path is unsupported; the fallback succeeds.
    Fallback,
    /// The input is corrupt; nothing is retried.
    Fatal,
    /// The fast path is unsupported and the fallback fails too.
    FallbackFails,
}

/// Decode with a fast path and a fallback. Only `Unsupported` is recoverable;
/// a cancellation is never retried.
pub fn decode(input: &[u8], pulse: &dyn Pulse, mode: Mode) -> Result<Vec<u8>, Error> {
    let total = Total::Exact(input.len() as u64);
    let mut attempts = Phases::new(
        pulse,
        Execution::Sequence,
        &[
            PhaseSpec::new("fast", 1, total),
            PhaseSpec::new("fallback", 1, total),
        ],
    );
    let first = attempts.run(0, |attempt| {
        attempt.check()?;
        match mode {
            Mode::Fast => copy(input, attempt),
            Mode::Fatal => Err(Error::Corrupt { row: 0 }),
            _ => Err(Error::Unsupported),
        }
    });
    let result = match first {
        Err(Error::Unsupported) => attempts.run(1, |attempt| match mode {
            Mode::FallbackFails => Err(Error::Corrupt { row: 0 }),
            _ => copy(input, attempt),
        }),
        result => result,
    };
    result.finish_phase(attempts)
}

fn copy(input: &[u8], pulse: &dyn Pulse) -> Result<Vec<u8>, Error> {
    let mut output = Vec::with_capacity(input.len());
    let mut pace = pulse.paced(64);
    for &byte in input {
        output.push(byte);
        pace.step(1)?;
    }
    pace.finish()?;
    Ok(output)
}

/// Analyze rows, transform tiles (in parallel with `std`), then entropy-code
/// them with a coder that owns its stop policy.
pub fn encode(image: &Image, pulse: &dyn Pulse) -> Result<Vec<u8>, Error> {
    let tiles = image.tiles() as u64;
    Stages::new(
        pulse,
        &[
            PhaseSpec::new("analyze", 1, Total::Exact(image.height as u64)).units("rows"),
            PhaseSpec::new("transform", 6, Total::Exact(tiles))
                .units("tiles")
                .execution(transform_execution()),
            PhaseSpec::new("entropy", 3, Total::Exact(tiles)).units("tiles"),
        ],
    )
    .complete_with(|stages| {
        stages.run(|stage| analyze(image, stage))?;
        let transformed = stages.run(|stage| transform(image, stage))?;
        stages.run(|stage| match stage.share() {
            // A coder context outlives the call that builds it, so it owns
            // its view, as codecs built with `with_stop(impl Stop + 'static)` do.
            Ok(owned) => EntropyCoder::new(owned).code(&transformed),
            // A pulse that only works borrowed still drives this call's coder.
            Err(_) => EntropyCoder::new(stage).code(&transformed),
        })
    })
}

fn analyze(image: &Image, stage: &dyn Pulse) -> Result<(), Error> {
    stage.check()?;
    for row in 0..image.height {
        if image.damaged_row == Some(row) {
            return Err(Error::Corrupt { row });
        }
        stage.step(1)?;
    }
    Ok(())
}

fn transform_tile(tile: &[u8]) -> Vec<u8> {
    let mut previous = 0_u8;
    tile.iter()
        .map(|&pixel| {
            let delta = pixel.wrapping_sub(previous);
            previous = pixel;
            delta
        })
        .collect()
}

/// Rayon workers share the stage: one logical count, no child per worker.
#[cfg(feature = "std")]
fn transform(image: &Image, stage: &dyn Pulse) -> Result<Vec<Vec<u8>>, Error> {
    use rayon::prelude::*;
    (0..image.tiles())
        .into_par_iter()
        .map(|index| {
            stage.check()?;
            let tile = transform_tile(image.tile(index));
            stage.step(1)?;
            Ok(tile)
        })
        .collect()
}

#[cfg(not(feature = "std"))]
fn transform(image: &Image, stage: &dyn Pulse) -> Result<Vec<Vec<u8>>, Error> {
    let mut tiles = Vec::with_capacity(image.tiles());
    for index in 0..image.tiles() {
        stage.check()?;
        tiles.push(transform_tile(image.tile(index)));
        stage.step(1)?;
    }
    Ok(tiles)
}

#[cfg(feature = "std")]
fn transform_execution() -> Execution {
    let workers = rayon::current_num_threads();
    Execution::work_pool(
        core::num::NonZeroUsize::new(workers).unwrap_or(core::num::NonZeroUsize::MIN),
    )
}

#[cfg(not(feature = "std"))]
fn transform_execution() -> Execution {
    Execution::Sequence
}

/// An entropy coder that keeps its stop policy and progress sink, as codec
/// contexts often do. Built with a [`SharedPulse`](how_far::SharedPulse) from
/// [`Pulse::share`], it is `'static`.
pub struct EntropyCoder<P: Stop + Report> {
    pulse: P,
    state: u64,
}

impl<P: Stop + Report> EntropyCoder<P> {
    /// Build a coder that checks and counts through `pulse`.
    pub fn new(pulse: P) -> Self {
        Self {
            pulse,
            state: 0xcbf2_9ce4_8422_2325,
        }
    }

    /// Code each tile, checking for cancellation every 64 bytes and counting
    /// each finished tile.
    pub fn code(&mut self, tiles: &[Vec<u8>]) -> Result<Vec<u8>, Error> {
        let mut out = Vec::new();
        self.pulse.check()?;
        for tile in tiles {
            for chunk in tile.chunks(64) {
                self.pulse.check()?;
                for &byte in chunk {
                    self.state = (self.state ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
                }
                out.push(self.state as u8);
            }
            self.pulse.step(1)?;
        }
        Ok(out)
    }
}

/// Encode tile groups as fork-join children on scoped threads. Each group is a
/// child with its own total and outcome; the library completes every child it
/// planned, and never the pulse it was given.
#[cfg(feature = "std")]
pub fn encode_groups(image: &Image, groups: usize, pulse: &dyn Pulse) -> Result<Vec<u8>, Error> {
    let tiles = image.tiles();
    let per_group = tiles.div_ceil(groups.max(1));
    let ranges: Vec<_> = (0..groups)
        .map(|group| (group * per_group).min(tiles)..((group + 1) * per_group).min(tiles))
        .collect();
    let names: Vec<_> = (0..groups)
        .map(|group| alloc::format!("group {group}"))
        .collect();
    let parts: Vec<_> = names
        .iter()
        .zip(&ranges)
        .map(|(name, range)| {
            let len = range.len() as u64;
            PhaseSpec::new(name, len.max(1), Total::Exact(len)).units("tiles")
        })
        .collect();
    let children = pulse.plan(Execution::ForkJoin, &parts);
    let results: Vec<Result<Vec<u8>, Error>> = std::thread::scope(|scope| {
        let workers: Vec<_> = children
            .into_iter()
            .zip(ranges)
            .map(|(child, range)| {
                scope.spawn(move || {
                    child.complete_with(|child| {
                        child.check()?;
                        let mut bytes = Vec::new();
                        for index in range {
                            bytes.extend(transform_tile(image.tile(index)));
                            child.step(1)?;
                        }
                        Ok(bytes)
                    })
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|worker| worker.join().expect("worker panicked"))
            .collect()
    });
    let mut out = Vec::new();
    for result in results {
        out.extend(result?);
    }
    Ok(out)
}

/// Encode tiles on `'static` threads, which cannot borrow the pulse: each gets
/// a shared view and an `Arc` of the pixels, and the library joins them before
/// returning. A pulse that cannot be shared gets scoped threads instead.
#[cfg(feature = "std")]
pub fn encode_detached(image: &Image, pulse: &dyn Pulse) -> Result<Vec<u8>, Error> {
    let Ok(shared) = pulse.share() else {
        return encode_groups(image, image.tiles(), pulse);
    };
    let image = std::sync::Arc::new(image.clone());
    let workers: Vec<_> = (0..image.tiles())
        .map(|index| {
            let (shared, image) = (shared.clone(), image.clone());
            std::thread::spawn(move || -> Result<Vec<u8>, Error> {
                shared.check()?;
                let tile = transform_tile(image.tile(index));
                shared.step(1)?;
                Ok(tile)
            })
        })
        .collect();
    let mut out = Vec::new();
    let mut first_error = None;
    for worker in workers {
        match worker.join().expect("worker panicked") {
            Ok(tile) => out.extend(tile),
            Err(error) => {
                first_error.get_or_insert(error);
            }
        }
    }
    first_error.map_or(Ok(out), Err)
}

/// Split work recursively with `rayon::join`, planning a fork-join child at
/// every level, as divide-and-conquer codecs (quadtrees, wavelets) do.
#[cfg(feature = "std")]
pub fn encode_quadtree(image: &Image, pulse: &dyn Pulse) -> Result<u64, Error> {
    fn node(tiles: &[usize], image: &Image, pulse: &dyn Pulse) -> Result<u64, Error> {
        if tiles.len() <= 2 {
            pulse.check()?;
            let mut sum = 0_u64;
            for &index in tiles {
                sum = sum.wrapping_add(transform_tile(image.tile(index)).len() as u64);
                pulse.step(1)?;
            }
            return Ok(sum);
        }
        let (left_tiles, right_tiles) = tiles.split_at(tiles.len() / 2);
        let mut halves = pulse
            .plan(
                Execution::ForkJoin,
                &[
                    PhaseSpec::new("left", left_tiles.len() as u64, Total::Unknown),
                    PhaseSpec::new("right", right_tiles.len() as u64, Total::Unknown),
                ],
            )
            .into_iter();
        // `plan` returns one child per part.
        let (left, right) = (halves.next().unwrap(), halves.next().unwrap());
        let (a, b) = rayon::join(
            || left.complete_with(|left| node(left_tiles, image, left)),
            || right.complete_with(|right| node(right_tiles, image, right)),
        );
        Ok(a?.wrapping_add(b?))
    }
    let tiles: Vec<usize> = (0..image.tiles()).collect();
    node(&tiles, image, pulse)
}
