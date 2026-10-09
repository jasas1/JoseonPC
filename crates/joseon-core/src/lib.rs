//! L0. Contracts, ring buffer, DSP analyzers and the analysis engine.
//!
//! No platform code lives here. This crate must build and pass its tests on
//! Windows, macOS and Linux.
//!
//! Port source: `Sources/JoseonCore` in the macOS repo.

pub mod contracts;
pub mod engine;
pub mod loudness;
pub mod ring_buffer;
pub mod session;
pub mod spectrum;
pub mod stereo;
pub mod test_signals;
