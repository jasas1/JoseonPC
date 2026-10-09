//! Tests of the render crate.
//!
//! A test that needs the GPU calls `gpu()`. On a machine with no adapter the
//! test prints a line and passes: there is nothing to draw with.

mod foundation;
mod meters;
mod spectrogram;
mod spectrum;
mod text;
mod timeline;
mod vectorscope;

use crate::context::RenderContext;
use std::sync::Arc;

pub(crate) fn gpu() -> Option<Arc<RenderContext>> {
    let ctx = RenderContext::shared();
    if ctx.is_none() {
        println!("no GPU adapter: skipped");
    }
    ctx
}

/// Where the review pictures of the tests go: `target/joseon-render-review/`.
pub(crate) fn review_path(name: &str) -> std::path::PathBuf {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/joseon-render-review");
    std::fs::create_dir_all(&directory).unwrap();
    directory.join(name)
}

/// The pixel at `(x, y)` of an RGBA image.
pub(crate) fn pixel(rgba: &[u8], width: u32, x: u32, y: u32) -> [u8; 4] {
    let i = ((y * width + x) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2], rgba[i + 3]]
}

#[track_caller]
pub(crate) fn assert_color(actual: [u8; 4], expected: [u8; 3], tolerance: i32, what: &str) {
    for c in 0..3 {
        assert!(
            (actual[c] as i32 - expected[c] as i32).abs() <= tolerance,
            "{what}: {actual:?}, expected {expected:?} ± {tolerance}"
        );
    }
}
