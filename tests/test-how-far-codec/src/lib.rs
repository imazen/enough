//! A pretend image codec that reports through `how-far` the way a real codec
//! would. It depends only on `how-far` and Rayon, never on a tracker.
//!
//! It exercises every shape a library takes: serial stages, a Rayon stage
//! shared by workers, a fork-join of scoped threads with their own children,
//! `'static` worker threads, and a codec context that owns its stop policy.

#![forbid(unsafe_code)]

use how_far::{
    Execution, Outcome, PhaseSpec, PlanError, ProgressExt, Pulse, Report, RunError, Stop,
    StopReason, Total, TryStages,
};
use rayon::prelude::*;
use std::{num::NonZeroUsize, sync::Arc};

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

/// Why encoding stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum CodecError {
    /// A cancellation request or timeout.
    Stopped(StopReason),
    /// Input the codec cannot handle.
    Corrupt {
        /// The rejected row.
        row: usize,
    },
    /// The progress plan could not be applied.
    Plan(PlanError),
}

impl CodecError {
    /// Whether the error is a stop request rather than a failure.
    pub fn is_stop(&self) -> bool {
        matches!(self, Self::Stopped(_))
    }
}

impl From<StopReason> for CodecError {
    fn from(reason: StopReason) -> Self {
        Self::Stopped(reason)
    }
}

impl From<PlanError> for CodecError {
    fn from(error: PlanError) -> Self {
        Self::Plan(error)
    }
}

impl From<RunError<CodecError>> for CodecError {
    fn from(error: RunError<CodecError>) -> Self {
        match error {
            RunError::Work(error) => error,
            RunError::Plan(error) => Self::Plan(error),
            _ => Self::Plan(PlanError::Unsupported),
        }
    }
}

/// Analyze rows, transform tiles in parallel, then entropy-code them.
///
/// The three stages show the common shapes: a serial loop, a Rayon stage
/// whose workers share one count, and a codec context that owns its stop
/// policy and progress sink and therefore needs `'static` handles.
pub fn encode(image: &Image, pulse: &dyn Pulse) -> Result<Vec<u8>, CodecError> {
    let tiles = image.tiles() as u64;
    let workers = NonZeroUsize::new(rayon::current_num_threads()).unwrap_or(NonZeroUsize::MIN);
    let mut stages = TryStages::new(
        pulse,
        &[
            PhaseSpec::new("analyze", 1, Total::Exact(image.height as u64)).units("rows"),
            PhaseSpec::new("transform", 6, Total::Exact(tiles))
                .units("tiles")
                .execution(Execution::work_pool(workers)),
            PhaseSpec::new("entropy", 3, Total::Exact(tiles)).units("tiles"),
        ],
    )?;
    stages.run_classified(CodecError::is_stop, |stage| analyze(image, stage))?;
    let transformed =
        stages.run_classified(CodecError::is_stop, |stage| transform(image, stage))?;
    let bytes = stages.run_classified(CodecError::is_stop, |stage| {
        // The coder owns its handle, like a codec built with `with_stop`.
        let mut coder = EntropyCoder::new(stage.handle());
        coder.code(&transformed)
    })?;
    stages.finish()?;
    Ok(bytes)
}

fn analyze(image: &Image, stage: &dyn Pulse) -> Result<(), CodecError> {
    stage.check()?;
    for row in 0..image.height {
        if image.damaged_row == Some(row) {
            return Err(CodecError::Corrupt { row });
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

fn transform(image: &Image, stage: &dyn Pulse) -> Result<Vec<Vec<u8>>, CodecError> {
    // Rayon workers share the stage: one logical count, no child per worker.
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

/// An entropy coder that owns its stop policy and progress sink.
///
/// Codec contexts often store `impl Stop + 'static` so they can outlive the
/// call that built them. A borrowed `&dyn Pulse` cannot be stored like that;
/// [`Pulse::handle`] can.
pub struct EntropyCoder<P: Stop + Report + 'static> {
    pulse: P,
    state: u64,
}

impl<P: Stop + Report + 'static> EntropyCoder<P> {
    /// Build a coder that checks and counts through `pulse`.
    pub fn new(pulse: P) -> Self {
        Self {
            pulse,
            state: 0xcbf2_9ce4_8422_2325,
        }
    }

    /// Code each tile, checking for cancellation every 64 bytes and counting
    /// each finished tile.
    pub fn code(&mut self, tiles: &[Vec<u8>]) -> Result<Vec<u8>, CodecError> {
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

/// Encode tile groups as fork-join children on scoped threads.
///
/// Each group is its own child phase with its own total and outcome; the
/// library finishes every child it split, and never the pulse it was given.
pub fn encode_groups(
    image: &Image,
    groups: usize,
    pulse: &dyn Pulse,
) -> Result<Vec<u8>, CodecError> {
    let tiles = image.tiles();
    let per_group = tiles.div_ceil(groups.max(1));
    let names: Vec<String> = (0..groups).map(|group| format!("group {group}")).collect();
    let ranges: Vec<_> = (0..groups)
        .map(|group| (group * per_group).min(tiles)..((group + 1) * per_group).min(tiles))
        .collect();
    let parts: Vec<_> = names
        .iter()
        .zip(&ranges)
        .map(|(name, range)| {
            PhaseSpec::new(
                name,
                range.len().max(1) as u64,
                Total::Exact(range.len() as u64),
            )
            .units("tiles")
        })
        .collect();
    let children = pulse.split(Execution::ForkJoin, &parts)?;
    let results: Vec<Result<Vec<u8>, CodecError>> = std::thread::scope(|scope| {
        let handles: Vec<_> = children
            .into_iter()
            .zip(ranges)
            .map(|(child, range)| {
                scope.spawn(move || {
                    let result = (|| {
                        child.check()?;
                        let mut bytes = Vec::new();
                        for index in range {
                            bytes.extend(transform_tile(image.tile(index)));
                            child.step(1)?;
                        }
                        Ok(bytes)
                    })();
                    child.finish(Outcome::from_result(&result, CodecError::is_stop))?;
                    result
                })
            })
            .collect();
        handles
            .into_iter()
            .map(|handle| handle.join().expect("worker panicked"))
            .collect()
    });
    let mut out = Vec::new();
    for result in results {
        out.extend(result?);
    }
    Ok(out)
}

/// Encode tiles on `'static` threads that cannot borrow the pulse.
///
/// `std::thread::spawn` needs owned data, so each worker gets a
/// [`PulseHandle`](how_far::PulseHandle) and an `Arc` of the pixels; the
/// library joins them before returning.
pub fn encode_detached(image: &Image, pulse: &dyn Pulse) -> Result<Vec<u8>, CodecError> {
    let image = Arc::new(image.clone());
    let workers: Vec<_> = (0..image.tiles())
        .map(|index| {
            let handle = pulse.handle();
            let image = Arc::clone(&image);
            std::thread::spawn(move || -> Result<Vec<u8>, CodecError> {
                handle.check()?;
                let tile = transform_tile(image.tile(index));
                handle.step(1)?;
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
pub fn encode_quadtree(image: &Image, pulse: &dyn Pulse) -> Result<u64, CodecError> {
    fn node(tiles: &[usize], image: &Image, pulse: &dyn Pulse) -> Result<u64, CodecError> {
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
        let [left, right] = pulse.split_array(
            Execution::ForkJoin,
            [
                PhaseSpec::new("left", left_tiles.len() as u64, Total::Unknown),
                PhaseSpec::new("right", right_tiles.len() as u64, Total::Unknown),
            ],
        )?;
        let (a, b) = rayon::join(
            || {
                let result = node(left_tiles, image, &left);
                left.finish(Outcome::from_result(&result, CodecError::is_stop))?;
                result
            },
            || {
                let result = node(right_tiles, image, &right);
                right.finish(Outcome::from_result(&result, CodecError::is_stop))?;
                result
            },
        );
        Ok(a?.wrapping_add(b?))
    }
    let tiles: Vec<usize> = (0..image.tiles()).collect();
    node(&tiles, image, pulse)
}
