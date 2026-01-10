//! Mock codec tests - simulates real codec usage patterns.
#![allow(unused_imports, dead_code)]

use enough::{CancellationSource, Never, Stop, StopReason};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

// ============================================================================
// Mock Codec Types
// ============================================================================

/// Mock codec error type
#[derive(Debug, PartialEq)]
pub enum CodecError {
    Stopped(StopReason),
    InvalidData(&'static str),
    OutputTooLarge,
}

impl From<StopReason> for CodecError {
    fn from(r: StopReason) -> Self {
        CodecError::Stopped(r)
    }
}

impl CodecError {
    fn is_cancelled(&self) -> bool {
        matches!(self, CodecError::Stopped(StopReason::Cancelled))
    }

    fn is_timed_out(&self) -> bool {
        matches!(self, CodecError::Stopped(StopReason::TimedOut))
    }
}

// ============================================================================
// Mock Decoder
// ============================================================================

/// Mock image decoder that respects cancellation
pub struct MockDecoder {
    block_size: usize,
    check_frequency: usize,
}

impl MockDecoder {
    pub fn new() -> Self {
        Self {
            block_size: 1024,
            check_frequency: 16,
        }
    }

    pub fn with_block_size(mut self, size: usize) -> Self {
        self.block_size = size;
        self
    }

    pub fn with_check_frequency(mut self, freq: usize) -> Self {
        self.check_frequency = freq;
        self
    }

    /// Decode "image" data with cancellation support
    pub fn decode(&self, data: &[u8], stop: impl Stop) -> Result<Vec<u8>, CodecError> {
        if data.is_empty() {
            return Err(CodecError::InvalidData("empty input"));
        }

        let mut output = Vec::with_capacity(data.len());

        for (i, chunk) in data.chunks(self.block_size).enumerate() {
            // Check cancellation periodically
            if i % self.check_frequency == 0 {
                stop.check()?;
            }

            // Simulate decode work
            for &byte in chunk {
                output.push(byte.wrapping_add(1));
            }
        }

        Ok(output)
    }
}

impl Default for MockDecoder {
    fn default() -> Self {
        Self::new()
    }
}

// ============================================================================
// Mock Encoder
// ============================================================================

/// Mock image encoder that respects cancellation
pub struct MockEncoder {
    quality: u8,
    check_frequency: usize,
}

impl MockEncoder {
    pub fn new(quality: u8) -> Self {
        Self {
            quality,
            check_frequency: 16,
        }
    }

    /// Encode "image" data with cancellation support
    pub fn encode(&self, data: &[u8], stop: impl Stop) -> Result<Vec<u8>, CodecError> {
        if data.len() > 10_000_000 {
            return Err(CodecError::OutputTooLarge);
        }

        let mut output = Vec::new();

        // Write "header"
        output.push(0x89);
        output.push(self.quality);

        for (i, chunk) in data.chunks(64).enumerate() {
            // Check cancellation
            if i % self.check_frequency == 0 {
                stop.check()?;
            }

            // Simulate encode work
            let sum: usize = chunk.iter().map(|&b| b as usize).sum();
            output.push((sum % 256) as u8);
        }

        Ok(output)
    }
}

// ============================================================================
// Tests
// ============================================================================

#[test]
fn decoder_completes_without_cancellation() {
    let decoder = MockDecoder::new();
    let data = vec![0u8; 10000];

    let result = decoder.decode(&data, Never);

    assert!(result.is_ok());
    assert_eq!(result.unwrap().len(), 10000);
}

#[test]
fn decoder_respects_cancellation() {
    let decoder = MockDecoder::new().with_check_frequency(1);
    let source = CancellationSource::new();
    let data = vec![0u8; 10000];

    // Cancel immediately
    source.cancel();

    let result = decoder.decode(&data, source.token());

    assert!(result.is_err());
    assert!(result.unwrap_err().is_cancelled());
}

#[test]
fn decoder_respects_timeout() {
    let decoder = MockDecoder::new()
        .with_check_frequency(1)
        .with_block_size(10);
    let source = CancellationSource::new();
    let token = source.token().with_timeout(Duration::from_millis(1));

    // Large data that will take time
    let data = vec![0u8; 100000];

    // Small delay to let timeout expire
    thread::sleep(Duration::from_millis(10));

    let result = decoder.decode(&data, token);

    assert!(result.is_err());
    assert!(result.unwrap_err().is_timed_out());
}

#[test]
fn encoder_completes_without_cancellation() {
    let encoder = MockEncoder::new(80);
    let data = vec![100u8; 1000];

    let result = encoder.encode(&data, Never);

    assert!(result.is_ok());
}

#[test]
fn encoder_respects_cancellation() {
    let encoder = MockEncoder::new(80);
    let source = CancellationSource::new();
    let data = vec![100u8; 10000];

    source.cancel();

    let result = encoder.encode(&data, source.token());

    assert!(result.is_err());
    assert!(result.unwrap_err().is_cancelled());
}

#[test]
fn concurrent_decode_with_shared_cancel() {
    // Use small block size and frequent checking to ensure cancellation is detected
    let decoder = Arc::new(
        MockDecoder::new()
            .with_block_size(10)
            .with_check_frequency(1),
    );
    let source = Arc::new(CancellationSource::new());
    let data = Arc::new(vec![0u8; 1_000_000]); // 1MB of data

    let handles: Vec<_> = (0..4)
        .map(|_| {
            let decoder = Arc::clone(&decoder);
            let source = Arc::clone(&source);
            let data = Arc::clone(&data);

            thread::spawn(move || decoder.decode(&data, source.token()))
        })
        .collect();

    // Cancel immediately - at least some threads should see it
    source.cancel();

    let results: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();

    // All should be cancelled since we cancel before any real work can happen
    let cancelled = results.iter().filter(|r| r.is_err()).count();
    assert!(
        cancelled > 0,
        "At least some operations should be cancelled"
    );
}

#[test]
fn decode_encode_pipeline() -> Result<(), CodecError> {
    let decoder = MockDecoder::new();
    let encoder = MockEncoder::new(90);
    let source = CancellationSource::new();
    let token = source.token();

    let input = vec![42u8; 1000];

    // Pipeline: decode then encode
    let decoded = decoder.decode(&input, token.clone())?;
    let encoded = encoder.encode(&decoded, token)?;

    assert!(!encoded.is_empty());

    Ok(())
}

#[test]
fn decode_encode_pipeline_cancelled() {
    let decoder = MockDecoder::new();
    let encoder = MockEncoder::new(90);
    let source = CancellationSource::new();
    let token = source.token();

    let input = vec![42u8; 1000];

    // Cancel before encode
    let decoded = decoder.decode(&input, token.clone()).unwrap();
    source.cancel();
    let result = encoder.encode(&decoded, token);

    assert!(result.is_err());
}

#[test]
fn timeout_in_pipeline() {
    let decoder = MockDecoder::new()
        .with_block_size(10)
        .with_check_frequency(1);
    let source = CancellationSource::new();
    let token = source.token().with_timeout(Duration::from_millis(1));

    let input = vec![42u8; 100000];

    // Let timeout expire
    thread::sleep(Duration::from_millis(10));

    let result = decoder.decode(&input, token);

    assert!(result.unwrap_err().is_timed_out());
}

#[test]
fn different_stop_impls_work() {
    let decoder = MockDecoder::new();
    let data = vec![0u8; 100];

    // Never
    assert!(decoder.decode(&data, Never).is_ok());

    // CancellationToken
    let source = CancellationSource::new();
    assert!(decoder.decode(&data, source.token()).is_ok());

    // Reference
    let token = source.token();
    assert!(decoder.decode(&data, &token).is_ok());

    // Trait object
    let stop: &dyn Stop = &token;
    assert!(decoder.decode(&data, stop).is_ok());
}

#[test]
fn error_type_integration() {
    // Test that the error type pattern works as expected

    fn might_fail(stop: impl Stop) -> Result<(), CodecError> {
        stop.check()?; // Uses From<StopReason> for CodecError
        Ok(())
    }

    let source = CancellationSource::new();
    assert!(might_fail(source.token()).is_ok());

    source.cancel();
    let err = might_fail(source.token()).unwrap_err();
    assert_eq!(err, CodecError::Stopped(StopReason::Cancelled));
}
