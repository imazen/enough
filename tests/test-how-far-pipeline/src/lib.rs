//! A pretend batch pipeline: a library that calls another library (the codec)
//! inside its own stages, across crates, through one `&dyn Pulse`.

#![forbid(unsafe_code)]

use how_far::{
    Execution, Outcome, PhaseSpec, PlanError, ProgressExt, Pulse, RunError, StopReason, Total,
    TryStages,
};
use rayon::prelude::*;
use test_how_far_codec::{CodecError, Image};

/// Why the pipeline stopped.
#[derive(Debug, PartialEq, Eq)]
pub enum PipelineError {
    /// An image failed or stopped inside the codec.
    Codec {
        /// The image's position in the batch.
        image: usize,
        /// The codec's error.
        error: CodecError,
    },
    /// A stop request between images.
    Stopped(StopReason),
    /// The progress plan could not be applied.
    Plan(PlanError),
}

impl PipelineError {
    /// Whether the error is a stop request rather than a failure.
    pub fn is_stop(&self) -> bool {
        match self {
            Self::Stopped(_) => true,
            Self::Codec { error, .. } => error.is_stop(),
            Self::Plan(_) => false,
        }
    }
}

impl From<StopReason> for PipelineError {
    fn from(reason: StopReason) -> Self {
        Self::Stopped(reason)
    }
}

impl From<PlanError> for PipelineError {
    fn from(error: PlanError) -> Self {
        Self::Plan(error)
    }
}

impl From<RunError<PipelineError>> for PipelineError {
    fn from(error: RunError<PipelineError>) -> Self {
        match error {
            RunError::Work(error) => error,
            RunError::Plan(error) => Self::Plan(error),
            _ => Self::Plan(PlanError::Unsupported),
        }
    }
}

impl From<RunError<StopReason>> for PipelineError {
    fn from(error: RunError<StopReason>) -> Self {
        match error {
            RunError::Work(reason) => Self::Stopped(reason),
            RunError::Plan(error) => Self::Plan(error),
            _ => Self::Plan(PlanError::Unsupported),
        }
    }
}

/// Validate, encode every image in parallel (one fork-join child per image,
/// on Rayon), then pack. Each child is handed to the codec, which plans its
/// own stages inside it.
pub fn process(images: &[Image], pulse: &dyn Pulse) -> Result<Vec<Vec<u8>>, PipelineError> {
    let pixels: u64 = images.iter().map(|image| image.pixels.len() as u64).sum();
    let mut stages = TryStages::new(
        pulse,
        &[
            PhaseSpec::new("validate", 1, Total::Exact(images.len() as u64)).units("images"),
            PhaseSpec::new("encode", 20, Total::Unknown),
            PhaseSpec::new("pack", 1, Total::Exact(pixels)).units("bytes"),
        ],
    )?;
    stages.run_stoppable(|stage| {
        for _ in images {
            stage.step(1)?;
        }
        Ok(())
    })?;
    let encoded = stages.run_nested(PipelineError::is_stop, |stage| {
        let names: Vec<String> = (0..images.len()).map(|i| format!("image {i}")).collect();
        let parts: Vec<_> = names
            .iter()
            .zip(images)
            .map(|(name, image)| {
                PhaseSpec::new(name, image.pixels.len().max(1) as u64, Total::Unknown)
            })
            .collect();
        let children = stage.split(Execution::ForkJoin, &parts)?;
        let results: Vec<Result<Vec<u8>, PipelineError>> = children
            .into_par_iter()
            .zip(images.par_iter())
            .enumerate()
            .map(|(index, (child, image))| {
                let result = test_how_far_codec::encode(image, &child).map_err(|error| {
                    PipelineError::Codec {
                        image: index,
                        error,
                    }
                });
                child.finish(Outcome::from_result(&result, PipelineError::is_stop))?;
                result
            })
            .collect();
        results
            .into_iter()
            .collect::<Result<Vec<_>, _>>()
            .map_err(RunError::Work)
    })?;
    stages.run_stoppable(|stage| {
        for bytes in &encoded {
            stage.step(bytes.len() as u64)?;
        }
        Ok(())
    })?;
    stages.finish()?;
    Ok(encoded)
}

/// Encode images one after another, each inside its own stage, by calling
/// the codec library with the stage's pulse.
pub fn process_each(images: &[Image], pulse: &dyn Pulse) -> Result<Vec<Vec<u8>>, PipelineError> {
    let names: Vec<String> = (0..images.len()).map(|i| format!("image {i}")).collect();
    let parts: Vec<_> = names
        .iter()
        .zip(images)
        .map(|(name, image)| PhaseSpec::new(name, image.pixels.len().max(1) as u64, Total::Unknown))
        .collect();
    let mut stages = TryStages::new(pulse, &parts)?;
    let mut encoded = Vec::new();
    for (index, image) in images.iter().enumerate() {
        encoded.push(stages.run_classified(PipelineError::is_stop, |stage| {
            test_how_far_codec::encode(image, stage).map_err(|error| PipelineError::Codec {
                image: index,
                error,
            })
        })?);
    }
    stages.finish()?;
    Ok(encoded)
}
