//! Records the drawing of the text layer in view points with a top-left
//! origin, the same space the GPU code uses. Nothing is painted here:
//! `TextLayer` compares the recorded items with what its bitmap already holds
//! and paints only what changed.
//!
//! Port source: `OverlayItem` and `OverlayContext` in
//! `Sources/JoseonRender/TextOverlay.swift`.

use super::fonts::{self, Font, Line};
use crate::gpu::Color;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex, OnceLock};

/// A rectangle in points, top-left origin.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    pub fn min_x(&self) -> f32 {
        self.x
    }

    pub fn max_x(&self) -> f32 {
        self.x + self.w
    }

    pub fn min_y(&self) -> f32 {
        self.y
    }

    pub fn max_y(&self) -> f32 {
        self.y + self.h
    }

    pub fn mid_x(&self) -> f32 {
        self.x + self.w * 0.5
    }

    pub fn mid_y(&self) -> f32 {
        self.y + self.h * 0.5
    }

    /// Smaller by `dx` on the left and the right, and by `dy` on the top and the bottom. Negative grows.
    pub fn inset(&self, dx: f32, dy: f32) -> Rect {
        Rect::new(self.x + dx, self.y + dy, self.w - 2.0 * dx, self.h - 2.0 * dy)
    }

    /// True when the two have an area in common. Touching edges do not count.
    pub fn intersects(&self, other: &Rect) -> bool {
        self.min_x() < other.max_x()
            && other.min_x() < self.max_x()
            && self.min_y() < other.max_y()
            && other.min_y() < self.max_y()
    }

    pub fn contains(&self, other: &Rect) -> bool {
        other.min_x() >= self.min_x()
            && other.max_x() <= self.max_x()
            && other.min_y() >= self.min_y()
            && other.max_y() <= self.max_y()
    }

    pub fn contains_point(&self, x: f32, y: f32) -> bool {
        x >= self.min_x() && x < self.max_x() && y >= self.min_y() && y < self.max_y()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HAlign {
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VAlign {
    Top,
    Middle,
    Baseline,
    Bottom,
}

/// What an item paints. All positions in points, top-left origin.
#[derive(Debug, Clone, PartialEq)]
pub enum Op {
    Text {
        line: Arc<Line>,
        size: f32,
        x: f32,
        baseline: f32,
        color: Color,
    },
    Fill {
        rect: Rect,
        color: Color,
        radius: f32,
    },
    Stroke {
        rect: Rect,
        color: Color,
        radius: f32,
        width: f32,
    },
    Line {
        from: [f32; 2],
        to: [f32; 2],
        color: Color,
        width: f32,
        dash: Vec<f32>,
    },
    /// Clockwise from `start` to `end`, radians, 0 = 12 o'clock.
    Arc {
        center: [f32; 2],
        radius: f32,
        start: f32,
        end: f32,
        color: Color,
        width: f32,
    },
}

/// One drawing of the text layer: what to paint, the pixel-aligned rectangle it
/// may touch, and a key that is equal exactly when the painted pixels are equal.
#[derive(Debug, Clone, PartialEq)]
pub struct OverlayItem {
    pub op: Op,
    pub rect: Rect,
    pub key: u64,
}

struct Key(std::collections::hash_map::DefaultHasher);

impl Key {
    fn new(kind: u8) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        kind.hash(&mut hasher);
        Self(hasher)
    }

    fn f(mut self, v: f32) -> Self {
        v.to_bits().hash(&mut self.0);
        self
    }

    fn fs(mut self, values: &[f32]) -> Self {
        for v in values {
            v.to_bits().hash(&mut self.0);
        }
        values.len().hash(&mut self.0);
        self
    }

    fn u(mut self, v: u64) -> Self {
        v.hash(&mut self.0);
        self
    }

    fn finish(self) -> u64 {
        self.0.finish()
    }
}

// Text lines are cached: the same labels and a bounded set of numbers come back every redraw.
type LineKey = (String, u32, u32, u32);

fn line_cache() -> &'static Mutex<HashMap<LineKey, Arc<Line>>> {
    static CACHE: OnceLock<Mutex<HashMap<LineKey, Arc<Line>>>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(HashMap::new()))
}

static LINE_CACHE_MISSES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// Lines that had to be laid out (not found in the cache), from the start of the process.
pub fn line_cache_misses() -> u64 {
    LINE_CACHE_MISSES.load(std::sync::atomic::Ordering::Relaxed)
}

fn cached_line(text: &str, font: &Font, tracking: f32) -> Arc<Line> {
    let key = (
        text.to_string(),
        font.family_id(),
        font.size.to_bits(),
        tracking.to_bits(),
    );
    let mut cache = line_cache().lock().unwrap();
    if let Some(line) = cache.get(&key) {
        return line.clone();
    }
    LINE_CACHE_MISSES.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // Line ids are never used again: a flush of the cache cannot make two different lines look equal.
    if cache.len() > 2048 {
        cache.clear();
    }
    let line = Arc::new(font.layout(text, tracking));
    cache.insert(key, line.clone());
    line
}

pub struct OverlayContext {
    /// Points.
    pub width: f32,
    pub height: f32,
    pub pixel_scale: f32,
    items: Vec<OverlayItem>,
    /// Every text drawn so far with its ink box (cap height plus a little):
    /// what a reader sees as "the label". Optional labels ask `is_free` first.
    labels: Vec<(String, Rect)>,
}

impl OverlayContext {
    pub fn new(width: f32, height: f32, pixel_scale: f32) -> Self {
        Self {
            width,
            height,
            pixel_scale,
            items: Vec::with_capacity(128),
            labels: Vec::new(),
        }
    }

    pub fn items(&self) -> &[OverlayItem] {
        &self.items
    }

    pub fn labels(&self) -> &[(String, Rect)] {
        &self.labels
    }

    /// Starts from items and labels recorded before (the static labels of a
    /// panel, which change only with the layout).
    pub fn preload(&mut self, items: &[OverlayItem], labels: &[(String, Rect)]) {
        self.items = items.to_vec();
        self.labels = labels.to_vec();
    }

    /// The pixel-aligned rectangle that a drawing with these bounds may touch.
    fn aligned(&self, r: Rect, pad: f32) -> Option<Rect> {
        let s = self.pixel_scale;
        let x0 = (((r.min_x() - pad) * s).floor() / s).max(0.0);
        let y0 = (((r.min_y() - pad) * s).floor() / s).max(0.0);
        let x1 = (((r.max_x() + pad) * s).ceil() / s).min(self.width);
        let y1 = (((r.max_y() + pad) * s).ceil() / s).min(self.height);
        (x1 > x0 && y1 > y0).then(|| Rect::new(x0, y0, x1 - x0, y1 - y0))
    }

    fn add(&mut self, op: Op, bounds: Rect, pad: f32, key: u64) {
        if let Some(rect) = self.aligned(bounds, pad) {
            self.items.push(OverlayItem { op, rect, key });
        }
    }

    fn placed(x: f32, y: f32, width: f32, font: &Font, h: HAlign, v: VAlign) -> (f32, f32) {
        let px = match h {
            HAlign::Left => x,
            HAlign::Center => x - width / 2.0,
            HAlign::Right => x - width,
        };
        // Baseline in top-left coordinates.
        let cap_height = font.cap_height();
        let base = match v {
            VAlign::Baseline => y,
            VAlign::Top => y + cap_height,
            VAlign::Middle => y + cap_height / 2.0,
            VAlign::Bottom => y - font.descent(),
        };
        (px, base)
    }

    fn ink_box(px: f32, base: f32, width: f32, font: &Font) -> Rect {
        let cap_height = font.cap_height();
        Rect::new(px, base - cap_height - 1.0, width, cap_height + 2.5)
    }

    /// The ink box `text` would record for these arguments.
    #[allow(clippy::too_many_arguments)]
    pub fn text_bounds(&self, text: &str, x: f32, y: f32, font: &Font, h: HAlign, v: VAlign, tracking: f32) -> Rect {
        let width = self.measure(text, font, tracking);
        let (px, base) = Self::placed(x, y, width, font, h, v);
        Self::ink_box(px, base, width, font)
    }

    /// True when `rect` (grown by `pad`) touches no label drawn so far and none of `others`.
    pub fn is_free(&self, rect: Rect, pad: f32, others: &[Rect]) -> bool {
        let r = rect.inset(-pad, -pad);
        !self.labels.iter().any(|(_, l)| l.intersects(&r)) && !others.iter().any(|o| o.intersects(&r))
    }

    /// Draws `text` and gives its width in points.
    #[allow(clippy::too_many_arguments)]
    pub fn text_tracked(
        &mut self,
        text: &str,
        x: f32,
        y: f32,
        font: &Font,
        color: Color,
        h: HAlign,
        v: VAlign,
        tracking: f32,
    ) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        let line = cached_line(text, font, tracking);
        let width = line.width;
        let (px, base) = Self::placed(x, y, width, font, h, v);
        self.labels
            .push((text.to_string(), Self::ink_box(px, base, width, font)));
        let key = Key::new(0).u(line.id).f(px).f(base).fs(&color).finish();
        let (ascent, descent) = (font.ascent(), font.descent());
        self.add(
            Op::Text {
                line,
                size: font.size,
                x: px,
                baseline: base,
                color,
            },
            Rect::new(px, base - ascent, width, ascent + descent),
            2.0,
            key,
        );
        width
    }

    /// Draws `text` with no tracking and gives its width in points.
    #[allow(clippy::too_many_arguments)]
    pub fn text(&mut self, text: &str, x: f32, y: f32, font: &Font, color: Color, h: HAlign, v: VAlign) -> f32 {
        self.text_tracked(text, x, y, font, color, h, v, 0.0)
    }

    pub fn measure(&self, text: &str, font: &Font, tracking: f32) -> f32 {
        if text.is_empty() {
            return 0.0;
        }
        cached_line(text, font, tracking).width
    }

    pub fn fill_rect(&mut self, r: Rect, color: Color, radius: f32) {
        let key = Key::new(1).fs(&[r.x, r.y, r.w, r.h]).fs(&color).f(radius).finish();
        self.add(Op::Fill { rect: r, color, radius }, r, 1.5, key);
    }

    pub fn stroke_rect(&mut self, r: Rect, color: Color, radius: f32, width: f32) {
        let key = Key::new(2)
            .fs(&[r.x, r.y, r.w, r.h])
            .fs(&color)
            .f(radius)
            .f(width)
            .finish();
        self.add(
            Op::Stroke {
                rect: r,
                color,
                radius,
                width,
            },
            r,
            1.5 + width,
            key,
        );
    }

    pub fn line(&mut self, from: [f32; 2], to: [f32; 2], color: Color, width: f32, dash: &[f32]) {
        let key = Key::new(3).fs(&from).fs(&to).fs(&color).f(width).fs(dash).finish();
        let bounds = Rect::new(
            from[0].min(to[0]),
            from[1].min(to[1]),
            (to[0] - from[0]).abs(),
            (to[1] - from[1]).abs(),
        );
        self.add(
            Op::Line {
                from,
                to,
                color,
                width,
                dash: dash.to_vec(),
            },
            bounds,
            1.5 + width,
            key,
        );
    }

    /// A circular arc, clockwise from `start` to `end` (radians, 0 = 12 o'clock). A ring is 0 ... 2 pi.
    pub fn arc(&mut self, center: [f32; 2], radius: f32, start: f32, end: f32, color: Color, width: f32) {
        if !(end > start && radius > 0.0) {
            return;
        }
        let key = Key::new(4)
            .fs(&center)
            .f(radius)
            .f(start)
            .f(end)
            .fs(&color)
            .f(width)
            .finish();
        let bounds = Rect::new(center[0] - radius, center[1] - radius, radius * 2.0, radius * 2.0);
        self.add(
            Op::Arc {
                center,
                radius,
                start,
                end,
                color,
                width,
            },
            bounds,
            1.5 + width,
            key,
        );
    }
}

impl OverlayItem {
    /// Paints the item into premultiplied RGBA pixels of a bitmap of `scale` pixels per point.
    pub fn draw(&self, pixmap: &mut tiny_skia::Pixmap, scale: f32) {
        use tiny_skia::{FillRule, Paint, PathBuilder, Stroke, StrokeDash, Transform};
        let transform = Transform::from_scale(scale, scale);
        let paint = |color: &Color| {
            let mut paint = Paint::default();
            paint.set_color(
                tiny_skia::Color::from_rgba(
                    color[0].clamp(0.0, 1.0),
                    color[1].clamp(0.0, 1.0),
                    color[2].clamp(0.0, 1.0),
                    color[3].clamp(0.0, 1.0),
                )
                .unwrap_or(tiny_skia::Color::TRANSPARENT),
            );
            paint.anti_alias = true;
            paint
        };
        match &self.op {
            Op::Text {
                line,
                size,
                x,
                baseline,
                color,
            } => {
                let (width, height) = (pixmap.width() as usize, pixmap.height() as usize);
                fonts::draw_line(
                    pixmap.data_mut(),
                    width,
                    height,
                    line,
                    *size,
                    *x,
                    *baseline,
                    scale,
                    *color,
                );
            }
            Op::Fill { rect, color, radius } => {
                if let Some(path) = rounded_rect_path(*rect, *radius) {
                    pixmap.fill_path(&path, &paint(color), FillRule::Winding, transform, None);
                }
            }
            Op::Stroke {
                rect,
                color,
                radius,
                width,
            } => {
                if let Some(path) = rounded_rect_path(*rect, *radius) {
                    let stroke = Stroke {
                        width: *width,
                        ..Default::default()
                    };
                    pixmap.stroke_path(&path, &paint(color), &stroke, transform, None);
                }
            }
            Op::Line {
                from,
                to,
                color,
                width,
                dash,
            } => {
                let mut builder = PathBuilder::new();
                builder.move_to(from[0], from[1]);
                builder.line_to(to[0], to[1]);
                if let Some(path) = builder.finish() {
                    let stroke = Stroke {
                        width: *width,
                        dash: if dash.len() >= 2 && dash.len() % 2 == 0 {
                            StrokeDash::new(dash.clone(), 0.0)
                        } else {
                            None
                        },
                        ..Default::default()
                    };
                    pixmap.stroke_path(&path, &paint(color), &stroke, transform, None);
                }
            }
            Op::Arc {
                center,
                radius,
                start,
                end,
                color,
                width,
            } => {
                if let Some(path) = arc_path(*center, *radius, *start, *end) {
                    let stroke = Stroke {
                        width: *width,
                        ..Default::default()
                    };
                    pixmap.stroke_path(&path, &paint(color), &stroke, transform, None);
                }
            }
        }
    }
}

/// How far a control point is from the end of a quarter circle of radius 1.
const KAPPA: f32 = 0.552_284_8;

fn rounded_rect_path(r: Rect, radius: f32) -> Option<tiny_skia::Path> {
    let mut b = tiny_skia::PathBuilder::new();
    if !(radius > 0.0) {
        b.push_rect(tiny_skia::Rect::from_xywh(r.x, r.y, r.w, r.h)?);
        return b.finish();
    }
    let (rx, ry) = (radius.min(r.w / 2.0), radius.min(r.h / 2.0));
    let (x0, y0, x1, y1) = (r.min_x(), r.min_y(), r.max_x(), r.max_y());
    let (kx, ky) = (rx * KAPPA, ry * KAPPA);
    b.move_to(x0 + rx, y0);
    b.line_to(x1 - rx, y0);
    b.cubic_to(x1 - rx + kx, y0, x1, y0 + ry - ky, x1, y0 + ry);
    b.line_to(x1, y1 - ry);
    b.cubic_to(x1, y1 - ry + ky, x1 - rx + kx, y1, x1 - rx, y1);
    b.line_to(x0 + rx, y1);
    b.cubic_to(x0 + rx - kx, y1, x0, y1 - ry + ky, x0, y1 - ry);
    b.line_to(x0, y0 + ry);
    b.cubic_to(x0, y0 + ry - ky, x0 + rx - kx, y0, x0 + rx, y0);
    b.close();
    b.finish()
}

/// Clockwise from `start` to `end`, 0 = 12 o'clock, as cubic pieces of a quarter turn or less.
fn arc_path(center: [f32; 2], radius: f32, start: f32, end: f32) -> Option<tiny_skia::Path> {
    let point = |a: f32| (center[0] + radius * a.sin(), center[1] - radius * a.cos());
    // The direction of travel at angle `a`.
    let tangent = |a: f32| (a.cos(), a.sin());
    let pieces = ((end - start) / std::f32::consts::FRAC_PI_2).ceil().max(1.0) as usize;
    let step = (end - start) / pieces as f32;
    let handle = 4.0 / 3.0 * (step / 4.0).tan() * radius;
    let mut b = tiny_skia::PathBuilder::new();
    let (x, y) = point(start);
    b.move_to(x, y);
    for i in 0..pieces {
        let (a0, a1) = (start + step * i as f32, start + step * (i + 1) as f32);
        let (p0, p1) = (point(a0), point(a1));
        let (t0, t1) = (tangent(a0), tangent(a1));
        b.cubic_to(
            p0.0 + t0.0 * handle,
            p0.1 + t0.1 * handle,
            p1.0 - t1.0 * handle,
            p1.1 - t1.1 * handle,
            p1.0,
            p1.1,
        );
    }
    b.finish()
}
