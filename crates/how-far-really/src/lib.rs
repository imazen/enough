#![doc = include_str!("../README.md")]
#![forbid(unsafe_code)]
#![warn(missing_docs)]
extern crate alloc;
pub mod diagnostics;
mod json;
pub mod profile;
mod sync;
