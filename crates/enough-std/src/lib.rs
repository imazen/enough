//! # enough-std
//!
//! Standard library implementations of the [`enough::Stop`] trait.
//!
//! This crate provides concrete cancellation types for use in applications:
//!
//! - [`CancellationSource`] - Owns the cancellation state, creates tokens
//! - [`CancellationToken`] - Lightweight, `Copy` token for passing around
//! - [`ChildCancellationSource`] - Hierarchical cancellation (child + parent)
//!
//! ## Basic Usage
//!
//! ```rust
//! use enough_std::{CancellationSource, CancellationToken};
//! use enough::Stop;
//! use std::time::Duration;
//!
//! // Create a source (owns the state)
//! let source = CancellationSource::new();
//!
//! // Get a token (lightweight, Copy)
//! let token = source.token();
//!
//! // Add a timeout (tightens, never loosens)
//! let token = token.with_timeout(Duration::from_secs(30));
//!
//! // Pass to library functions
//! // my_codec::decode(&data, token);
//!
//! // Cancel from elsewhere
//! source.cancel();
//!
//! // Token now returns Err
//! assert!(token.check().is_err());
//! ```
//!
//! ## Child Cancellation
//!
//! ```rust
//! use enough_std::{CancellationSource, ChildCancellationSource};
//! use enough::Stop;
//!
//! let parent = CancellationSource::new();
//!
//! // Create children that inherit parent's cancellation
//! let child_a = ChildCancellationSource::new(parent.token());
//! let child_b = ChildCancellationSource::new(parent.token());
//!
//! // Cancel just child_a - child_b continues
//! child_a.cancel();
//! assert!(child_a.token().is_stopped());
//! assert!(!child_b.token().is_stopped());
//!
//! // Cancel parent - all children stop
//! parent.cancel();
//! assert!(child_b.token().is_stopped());
//! ```

#![warn(missing_docs)]
#![warn(clippy::all)]

mod callback;
mod child;
mod source;
mod token;

pub use callback::CallbackCancellation;
pub use child::ChildCancellationSource;
pub use source::CancellationSource;
pub use token::CancellationToken;

// Re-export core types for convenience
pub use enough::{Never, Stop, StopReason};
