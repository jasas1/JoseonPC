//! The text of a panel: fonts, the recorder of drawing, and the bitmap that the
//! GPU shows.
//!
//! Port source: `TextOverlay.swift` and `TextLayer.swift` in
//! `Sources/JoseonRender`. Core Text and Core Graphics became `ab_glyph` (glyphs)
//! and `tiny-skia` (boxes, lines, arcs).

pub mod fonts;
pub mod layer;
pub mod overlay;

pub use fonts::{Font, Weight};
pub use layer::TextLayer;
pub use overlay::{HAlign, OverlayContext, OverlayItem, Rect, VAlign};
