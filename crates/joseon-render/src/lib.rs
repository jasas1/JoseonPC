//! L2. Panels drawn with wgpu: spectrum, spectrogram, vectorscope, meters and
//! timeline. Also the tray mini renderer and the offscreen PNG renderer.
//!
//! wgpu picks Direct3D 12 on Windows by default, with Vulkan as the fallback.
//! The same code runs on Metal, so panel work can be done and tested on a Mac.
//!
//! Port source: `Sources/JoseonRender` in the macOS repo. Shaders move from
//! Metal Shading Language to WGSL.

pub mod arena;
pub mod axis;
pub mod context;
pub mod contracts;
pub mod cursor;
pub mod fmt;
pub mod gpu;
pub mod meters;
pub mod mini_spectrum;
pub mod offscreen;
pub mod palette;
pub mod panel;
pub mod spectrogram;
pub mod spectrum;
pub mod synthetic;
pub mod synthetic_session;
pub mod target;
pub mod text;
pub mod timeline;
pub mod vectorscope;

#[cfg(test)]
mod tests;

pub use axis::LogAxis;
pub use context::RenderContext;
pub use palette::{Palette, Theme};
