//! Draws one panel into any color target. The view on screen and the
//! offscreen renderer (PNG) both drive `PanelRenderer`, so the two outputs come
//! from the same drawing code.
//!
//! Port source: `PanelRenderer` in `Sources/JoseonRender/PanelRenderer.swift`.
//!
//! The Swift class has subclasses. Here `PanelBase` holds what every panel has,
//! and the trait `Panel` is the set of hooks a panel fills in.
//!
//! Per frame: `ingest(frame)` updates state (history, smoothing), `encode`
//! records the GPU work, `refresh_text` paints the text layer.

use crate::arena::{FloatSlice, FrameArena, ShapeBatch};
use crate::context::{Lut, RenderContext};
use crate::contracts::{CursorHistorySlice, PanelCursor, PanelKind};
use crate::cursor::{hover_label, CursorReadoutItem};
use crate::gpu::{Color, CurveUniforms, Globals};
use crate::palette::{with_alpha, Palette, Theme};
use crate::text::{OverlayContext, OverlayItem, Rect, TextLayer};
use joseon_core::contracts::AnalysisFrame;
use std::any::Any;
use std::hash::{Hash, Hasher};
use std::sync::Arc;

#[derive(Clone, Copy)]
enum CurvePipeline {
    LineOver,
    LineAdd,
    Fill,
    Band,
}

/// A hasher for the signatures of a panel: equal signatures = no repaint.
#[derive(Default)]
pub struct Signature(std::collections::hash_map::DefaultHasher);

impl Signature {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn add(&mut self, value: impl Hash) -> &mut Self {
        value.hash(&mut self.0);
        self
    }

    pub fn add_f32(&mut self, value: f32) -> &mut Self {
        value.to_bits().hash(&mut self.0);
        self
    }

    pub fn finish(&self) -> u64 {
        self.0.finish()
    }
}

pub struct PanelBase {
    pub ctx: Arc<RenderContext>,
    pub arena: FrameArena,
    pub batch: ShapeBatch,

    theme: Theme,
    high_contrast: bool,
    pub palette: Palette,
    pub palette_version: u32,

    /// Pointer position in points (top-left origin), `None` when the pointer is outside.
    pub hover: Option<[f32; 2]>,
    /// True when the panel shares a cursor.
    pub cursor_linked: bool,
    /// The shared cursor, copied from the link.
    pub cursor: Option<PanelCursor>,
    /// The past spectrum under a timed cursor, copied from the link.
    pub cursor_slice: Option<CursorHistorySlice>,

    /// Set when something other than a new analysis frame changed the picture
    /// (layout, theme, options, cursor). The owner clears it after a draw.
    pub needs_display: bool,
    /// False when the newest frame carries the same picture as the one before it.
    pub frame_changed: bool,
    last_frame_digest: u64,

    /// Text as a GPU texture, drawn at the end of the main pass.
    pub text_layer: TextLayer,
    last_static_signature: u64,
    static_items: Vec<OverlayItem>,
    static_labels: Vec<(String, Rect)>,
    static_items_signature: u64,
    last_dynamic_signature: u64,
    last_hover_signature: u64,
    last_text_time: f64,
    last_hover_time: f64,

    pub frame: Option<Arc<AnalysisFrame>>,
    /// Points.
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub last_host_time: f64,
    pub frame_delta: f64,
    pub ingest_count: u64,
    /// GPU seconds of the last completed command buffer (set by the owner).
    pub last_gpu_time: f64,
    /// True while `encode` runs with a target: a pre-pass that only serves the picture of this frame may skip otherwise.
    pub drawing_this_frame: bool,
}

impl PanelBase {
    /// Readouts repaint at most this often, in seconds.
    pub const TEXT_INTERVAL: f64 = 0.1;

    fn new(ctx: Arc<RenderContext>, theme: Theme) -> Self {
        let arena = FrameArena::new(&ctx);
        Self {
            ctx,
            arena,
            batch: ShapeBatch::new(),
            theme,
            high_contrast: false,
            palette: Palette::new(&theme, false),
            palette_version: 0,
            hover: None,
            cursor_linked: false,
            cursor: None,
            cursor_slice: None,
            needs_display: true,
            frame_changed: true,
            last_frame_digest: 0,
            text_layer: TextLayer::new(),
            last_static_signature: 0,
            static_items: Vec::new(),
            static_labels: Vec::new(),
            static_items_signature: 0,
            last_dynamic_signature: 0,
            last_hover_signature: 0,
            last_text_time: -1.0,
            last_hover_time: -1.0,
            frame: None,
            width: 1.0,
            height: 1.0,
            scale: 2.0,
            last_host_time: 0.0,
            frame_delta: 1.0 / 60.0,
            ingest_count: 0,
            last_gpu_time: 0.0,
            drawing_this_frame: false,
        }
    }

    pub fn theme(&self) -> &Theme {
        &self.theme
    }

    pub fn high_contrast(&self) -> bool {
        self.high_contrast
    }

    /// The size of the target in pixels.
    pub fn pixel_size(&self) -> (u32, u32) {
        (
            ((self.width * self.scale).round() as u32).max(1),
            ((self.height * self.scale).round() as u32).max(1),
        )
    }

    /// The part of the signature every panel has.
    pub fn static_signature(&self) -> u64 {
        let mut h = Signature::new();
        h.add((self.width * 4.0) as i64)
            .add((self.height * 4.0) as i64)
            .add(self.palette_version);
        if self.cursor_linked {
            h.add(true);
        }
        h.finish()
    }

    // MARK: Shapes (the batch with its arena)

    pub fn rect(&mut self, rect: [f32; 4], color: Color) {
        self.batch.rect(&mut self.arena, rect, color);
    }

    pub fn rounded_rect(&mut self, rect: [f32; 4], color: Color, radius: f32) {
        self.batch.rounded_rect(&mut self.arena, rect, color, radius);
    }

    /// A rectangle with a vertical gradient.
    pub fn gradient_rect(&mut self, rect: [f32; 4], top: Color, bottom: Color, radius: f32) {
        self.batch
            .gradient_rect(&mut self.arena, rect, top, bottom, radius, 0.0, 0.0);
    }

    pub fn hline(&mut self, x0: f32, x1: f32, y: f32, color: Color) {
        self.batch.hline(&mut self.arena, x0, x1, y, color, 1.0);
    }

    pub fn vline(&mut self, x: f32, y0: f32, y1: f32, color: Color) {
        self.batch.vline(&mut self.arena, x, y0, y1, color, 1.0);
    }

    pub fn circle(&mut self, center: [f32; 2], r: f32, color: Color, stroke: f32, glow: f32) {
        self.batch.circle(&mut self.arena, center, r, color, stroke, glow);
    }

    /// Draws the shapes added after the last flush, "over".
    pub fn flush(&mut self, pass: &mut wgpu::RenderPass<'_>) {
        self.batch.flush(pass, &self.arena, &self.ctx.shape_over, None);
    }

    /// Draws the shapes added after the last flush, additive.
    pub fn flush_add(&mut self, pass: &mut wgpu::RenderPass<'_>) {
        self.batch.flush(pass, &self.arena, &self.ctx.shape_add, None);
    }

    /// Limits drawing to a rectangle in points. The rectangle is cut to the target.
    pub fn set_scissor(&self, pass: &mut wgpu::RenderPass<'_>, x: f32, y: f32, w: f32, h: f32) {
        let (pw, ph) = self.pixel_size();
        let sc = self.scale;
        let x0 = ((x * sc) as i64).clamp(0, pw as i64 - 1) as u32;
        let y0 = ((y * sc) as i64).clamp(0, ph as i64 - 1) as u32;
        let w = (((w * sc) as i64).max(1) as u32).min(pw - x0);
        let h = (((h * sc) as i64).max(1) as u32).min(ph - y0);
        pass.set_scissor_rect(x0, y0, w, h);
    }

    pub fn clear_scissor(&self, pass: &mut wgpu::RenderPass<'_>) {
        let (pw, ph) = self.pixel_size();
        pass.set_scissor_rect(0, 0, pw, ph);
    }

    // MARK: Curves

    /// A polyline over the plot. `values` are 0 (bottom) ... 1 (top), uniformly spaced in x.
    pub fn draw_curve_line(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        values: FloatSlice,
        uniforms: CurveUniforms,
        additive: bool,
        lut: Option<&Lut>,
    ) {
        if values.count < 2 {
            return;
        }
        let mut u = uniforms;
        u.count = values.count as f32;
        // How many segments a pixel must test: the reach of the line and its glow, in point spacings.
        let spacing = (u.rect[2] / (values.count - 1).max(1) as f32).max(0.05);
        let reach = u.half_width + u.soft * 3.0 + 1.5 / self.scale;
        u.pad0 = ((reach / spacing).ceil() + 1.0).min(48.0);
        let which = if additive {
            CurvePipeline::LineAdd
        } else {
            CurvePipeline::LineOver
        };
        self.draw_curve(pass, which, values, values, u, lut);
    }

    /// The area under a curve: one strip, one gradient. `reference_left` /
    /// `reference_right`: the plot height (0...1) where the fill reaches
    /// `fill_top`, as a straight line from the left end to the right end.
    pub fn draw_curve_fill(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        values: FloatSlice,
        reference: (f32, f32),
        uniforms: CurveUniforms,
        lut: Option<&Lut>,
    ) {
        if values.count < 2 {
            return;
        }
        let mut u = uniforms;
        u.count = values.count as f32;
        u.pad0 = reference.0;
        u.pad2 = reference.1;
        self.draw_curve(pass, CurvePipeline::Fill, values, values, u, lut);
    }

    /// The area between two curves.
    pub fn draw_curve_band(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        values: FloatSlice,
        other: FloatSlice,
        uniforms: CurveUniforms,
    ) {
        if values.count < 2 || other.count < values.count {
            return;
        }
        let mut u = uniforms;
        u.count = values.count as f32;
        self.draw_curve(pass, CurvePipeline::Band, values, other, u, None);
    }

    fn draw_curve(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        which: CurvePipeline,
        values: FloatSlice,
        other: FloatSlice,
        uniforms: CurveUniforms,
        lut: Option<&Lut>,
    ) {
        let Some(offset) = self.arena.push_uniform(&uniforms) else {
            return;
        };
        pass.set_pipeline(match which {
            CurvePipeline::LineOver => &self.ctx.curve_line_over,
            CurvePipeline::LineAdd => &self.ctx.curve_line_add,
            CurvePipeline::Fill => &self.ctx.curve_fill,
            CurvePipeline::Band => &self.ctx.curve_band,
        });
        pass.set_bind_group(0, &self.arena.globals_group, &[]);
        pass.set_bind_group(1, &self.arena.curve_group, &[offset, values.offset, other.offset]);
        pass.set_bind_group(2, &lut.unwrap_or(&self.ctx.white_lut).bind_group, &[]);
        pass.draw(0..(values.count * 2) as u32, 0..1);
    }

    // MARK: Cursor lines

    /// Hover cursor: 1 px, 50 % white. Pinned: 1 px, the accent color.
    pub fn cursor_line_color(&self) -> Color {
        match self.cursor {
            Some(c) if c.is_pinned => with_alpha(self.palette.accent, if self.high_contrast { 1.0 } else { 0.95 }),
            _ => [1.0, 1.0, 1.0, 0.5],
        }
    }

    fn cursor_is_pinned(&self) -> bool {
        self.cursor.is_some_and(|c| c.is_pinned)
    }

    /// A vertical hairline over a plot; a pinned cursor carries a small pin head at its top.
    pub fn draw_cursor_vertical(&mut self, x: f32, top: f32, bottom: f32) {
        let color = self.cursor_line_color();
        self.vline(x, top, bottom, color);
        if self.cursor_is_pinned() {
            self.draw_pin_head(x, top + 3.5);
        }
    }

    /// A horizontal hairline (frequency on a vertical axis). The pin head stands at its axis end.
    pub fn draw_cursor_horizontal(&mut self, y: f32, left: f32, right: f32) {
        let color = self.cursor_line_color();
        self.hline(left, right, y, color);
        if self.cursor_is_pinned() {
            self.draw_pin_head(left + 3.5, y);
        }
    }

    pub fn draw_pin_head(&mut self, x: f32, y: f32) {
        let c = with_alpha(self.palette.accent, 1.0);
        self.circle([x, y], 3.5, c, 0.0, 0.0);
        self.circle([x, y], 3.5, [1.0, 1.0, 1.0, 0.9], 1.0, 0.0);
    }
}

/// What a panel fills in. Every method has the behavior of the Swift base class as its default.
pub trait Panel: Any {
    /// Which panel this is: a cursor whose `source` is this kind came from here.
    fn kind(&self) -> PanelKind;
    fn as_any(&self) -> &dyn Any;
    fn as_any_mut(&mut self) -> &mut dyn Any;

    /// Make the lookup textures again.
    fn palette_changed(&mut self, _base: &mut PanelBase) {}
    /// Size or scale changed.
    fn layout_changed(&mut self, _base: &mut PanelBase) {}
    /// A new analysis frame arrived. `dt` is the host time after the frame before it.
    fn update(&mut self, _base: &mut PanelBase, _frame: &AnalysisFrame, _dt: f64) {}
    /// Passes that run before the main pass (history and accumulation textures).
    fn encode_pre_pass(&mut self, _base: &mut PanelBase, _encoder: &mut wgpu::CommandEncoder) {}
    /// Draw into the main pass.
    fn draw(&mut self, _base: &mut PanelBase, _pass: &mut wgpu::RenderPass<'_>) {}
    /// Labels that change only with size, theme or options.
    fn draw_static(&mut self, _base: &PanelBase, _o: &mut OverlayContext) {}
    /// Readouts. Painted at most 10 times per second, and only when `dynamic_signature` changes.
    fn draw_dynamic(&mut self, _base: &mut PanelBase, _o: &mut OverlayContext) {}
    /// The box at the pointer stays over this y. `None` = the height of the panel.
    fn hover_label_floor(&self, _base: &PanelBase) -> Option<f32> {
        None
    }
    /// Small label that follows the pointer: its lines and the anchor in points.
    fn hover_label(&mut self, _base: &PanelBase) -> Option<(Vec<String>, [f32; 2])> {
        None
    }
    fn static_signature(&mut self, base: &PanelBase) -> u64 {
        base.static_signature()
    }
    fn dynamic_signature(&self, base: &PanelBase) -> u64 {
        base.ingest_count
    }
    fn accessibility_label(&self) -> String {
        String::new()
    }
    fn accessibility_value(&self, _base: &PanelBase) -> String {
        String::new()
    }

    // The linked cursor.

    /// False when a move of the cursor changes nothing this panel draws.
    fn cursor_change_shows(&self, _base: &PanelBase, _old: Option<PanelCursor>) -> bool {
        true
    }
    fn cursor_slice_changed(&mut self, _base: &mut PanelBase) {}
    /// False when this cursor changes nothing in the text of the panel.
    fn cursor_shows_in_text(&self, _cursor: &PanelCursor) -> bool {
        true
    }
    /// What of the cursor the text of this panel shows. Equal = no text repaint.
    fn combine_cursor_signature(&self, base: &PanelBase, c: &PanelCursor, h: &mut Signature) {
        h.add_f32(c.frequency_hz)
            .add(c.seconds_ago.map(f64::to_bits))
            .add(c.is_pinned)
            .add(c.source)
            .add(self.shows_pointer_readout(base));
    }
    /// True when the pointer makes cursors in this panel (a plot with a frequency axis).
    fn makes_pointer_cursors(&self) -> bool {
        false
    }
    /// The cursor a pointer at `point` stands for, from the axes of this panel.
    fn cursor_at(&self, _base: &PanelBase, _point: [f32; 2]) -> Option<PanelCursor> {
        None
    }
    /// True when `point` is on the drawn cursor (a click there clears a pinned cursor).
    fn is_on_cursor(&self, _base: &PanelBase, _point: [f32; 2]) -> bool {
        false
    }
    /// The answer of this panel at the cursor, in reading order.
    fn cursor_items(&mut self, _base: &PanelBase) -> Vec<CursorReadoutItem> {
        Vec::new()
    }
    /// The readout follows the pointer: a hover cursor that this panel made, with the pointer still over it.
    fn shows_pointer_readout(&self, base: &PanelBase) -> bool {
        match (base.cursor_linked, base.hover, base.cursor) {
            (true, Some(hover), Some(c)) if !c.is_pinned && c.source == self.kind() => {
                // A keyboard move leaves the pointer where it was: the cursor is no longer under it.
                self.cursor_at(base, hover) == Some(c)
            }
            _ => false,
        }
    }
    /// The readout stands in the header row: every other panel, and the source panel of a pinned or keyboard cursor.
    fn shows_header_readout(&self, base: &PanelBase) -> bool {
        base.cursor_linked && base.cursor.is_some() && !self.shows_pointer_readout(base)
    }

    // The run state.

    /// True while the panel moves with no new frames (fading light). The owner keeps drawing.
    fn is_animating(&self) -> bool {
        false
    }
    /// False for a panel whose picture does not come from analysis frames (the session timeline).
    fn uses_analysis_frames(&self) -> bool {
        true
    }
    /// True when the cursor readout carries numbers of the newest frame.
    fn cursor_readout_follows_frames(&self) -> bool {
        true
    }
    /// True when the panel keeps GPU history that needs one pre-pass per analysis frame.
    fn needs_pre_pass_per_frame(&self) -> bool {
        false
    }
    /// Display tick with no new analysis frame (persistence still fades).
    fn idle(&mut self, _base: &mut PanelBase, _dt: f64) {}
}

pub struct PanelRenderer {
    pub base: PanelBase,
    pub panel: Box<dyn Panel>,
}

impl PanelRenderer {
    /// A renderer around `panel`.
    pub fn with_panel(ctx: Arc<RenderContext>, theme: Theme, panel: Box<dyn Panel>) -> Self {
        let mut renderer = Self {
            base: PanelBase::new(ctx, theme),
            panel,
        };
        renderer.panel.palette_changed(&mut renderer.base);
        renderer
    }

    /// The renderer of a panel kind. `None` for a kind that is not ported yet.
    pub fn make(kind: PanelKind, ctx: Arc<RenderContext>, theme: Theme) -> Option<Self> {
        let panel: Box<dyn Panel> = match kind {
            PanelKind::Spectrum => Box::new(crate::spectrum::SpectrumPanel::new()),
            PanelKind::Spectrogram => Box::new(crate::spectrogram::SpectrogramPanel::new()),
            PanelKind::Meters => Box::new(crate::meters::MetersPanel::new()),
            PanelKind::Timeline => Box::new(crate::timeline::TimelinePanel::new()),
            PanelKind::Vectorscope => Box::new(crate::vectorscope::VectorscopePanel::new()),
        };
        Some(Self::with_panel(ctx, theme, panel))
    }

    /// The panel as its own type, to set its options.
    pub fn panel_as<T: Panel>(&mut self) -> Option<(&mut T, &mut PanelBase)> {
        let panel = self.panel.as_any_mut().downcast_mut::<T>()?;
        Some((panel, &mut self.base))
    }

    fn rebuild_palette(&mut self) {
        self.base.palette = Palette::new(&self.base.theme, self.base.high_contrast);
        self.base.palette_version = self.base.palette_version.wrapping_add(1);
        self.base.needs_display = true;
        self.panel.palette_changed(&mut self.base);
    }

    pub fn set_theme(&mut self, theme: Theme) {
        self.base.theme = theme;
        self.rebuild_palette();
    }

    pub fn set_high_contrast(&mut self, high_contrast: bool) {
        if high_contrast != self.base.high_contrast {
            self.base.high_contrast = high_contrast;
            self.rebuild_palette();
        }
    }

    pub fn set_hover(&mut self, hover: Option<[f32; 2]>) {
        if hover != self.base.hover {
            self.base.hover = hover;
            self.base.needs_display = true;
        }
    }

    pub fn set_cursor_linked(&mut self, linked: bool) {
        if linked != self.base.cursor_linked {
            self.base.cursor_linked = linked;
            self.base.needs_display = true;
            if self.base.width > 1.0 {
                self.panel.layout_changed(&mut self.base);
            }
        }
    }

    pub fn set_cursor(&mut self, cursor: Option<PanelCursor>) {
        let old = self.base.cursor;
        if cursor != old {
            self.base.cursor = cursor;
            if self.panel.cursor_change_shows(&self.base, old) {
                self.base.needs_display = true;
            }
        }
    }

    pub fn set_cursor_slice(&mut self, slice: Option<CursorHistorySlice>) {
        if slice != self.base.cursor_slice {
            self.base.cursor_slice = slice;
            self.panel.cursor_slice_changed(&mut self.base);
        }
    }

    /// The readout as one plain sentence (screen reader value). Empty with no cursor.
    pub fn cursor_accessibility_text(&mut self) -> String {
        let Some(c) = self.base.cursor.filter(|_| self.base.cursor_linked) else {
            return String::new();
        };
        let items = self.panel.cursor_items(&self.base);
        if items.is_empty() {
            return String::new();
        }
        let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
        format!(
            "{}{}",
            if c.is_pinned { "Pinned cursor: " } else { "Cursor: " },
            texts.join(", ")
        )
    }

    // MARK: Driving

    pub fn ingest(&mut self, f: Arc<AnalysisFrame>) {
        let base = &mut self.base;
        let mut dt = if base.frame.is_none() {
            1.0 / 60.0
        } else {
            f.host_time - base.last_host_time
        };
        if !(dt > 0.0) || dt > 0.5 {
            dt = 1.0 / 60.0;
        }
        base.frame_delta = dt;
        base.last_host_time = f.host_time;
        base.ingest_count = base.ingest_count.wrapping_add(1);
        let digest = digest(&f);
        base.frame_changed = digest != base.last_frame_digest || base.ingest_count <= 2;
        base.last_frame_digest = digest;
        base.frame = Some(f.clone());
        self.panel.update(&mut self.base, &f, dt);
    }

    /// Paints the text bitmap again when a signature changed. Readouts: at most
    /// 10 Hz. Pointer label: at most 30 Hz. `force` ignores the rate limits
    /// (offscreen pictures). True when the bitmap changed.
    pub fn refresh_text(&mut self, now: f64, force: bool) -> bool {
        let (width, height, scale) = (self.base.width, self.base.height, self.base.scale);
        self.base.text_layer.resize(width, height, scale);
        let s = self.panel.static_signature(&self.base);
        let mut dirty = s != self.base.last_static_signature || force;
        // With a cursor the readout carries numbers of the newest frame: they follow every frame, at the 10 Hz of the text.
        let has_cursor =
            self.base.cursor_linked && self.base.cursor.is_some_and(|c| self.panel.cursor_shows_in_text(&c));
        let mut dynamic = self.panel.dynamic_signature(&self.base);
        if has_cursor && self.panel.cursor_readout_follows_frames() {
            dynamic ^= self.base.ingest_count.wrapping_mul(0x9E37_79B1);
        }
        let base = &self.base;
        if (force || now - base.last_text_time >= PanelBase::TEXT_INTERVAL || now < base.last_text_time)
            && dynamic != base.last_dynamic_signature
        {
            dirty = true;
        }
        let label = self.panel.hover_label(&self.base);
        let mut hs = 0u64;
        if label.is_some() || has_cursor {
            let mut h = Signature::new();
            if let Some((lines, anchor)) = &label {
                h.add(lines).add((anchor[0] * 2.0) as i64).add((anchor[1] * 2.0) as i64);
            }
            // A moved cursor repaints the readout of every linked panel, at most 30 times per second.
            if let (Some(c), true) = (self.base.cursor, self.base.cursor_linked) {
                self.panel.combine_cursor_signature(&self.base, &c, &mut h);
            }
            hs = h.finish();
        }
        let base = &self.base;
        if hs != base.last_hover_signature
            && (force || now - base.last_hover_time >= 1.0 / 30.0 || now < base.last_hover_time)
        {
            dirty = true;
        }
        if !dirty {
            return false;
        }
        let base = &mut self.base;
        base.last_static_signature = s;
        base.last_dynamic_signature = dynamic;
        base.last_hover_signature = hs;
        base.last_text_time = now;
        base.last_hover_time = now;
        // The static labels are recorded once per layout / theme and used again: their layout code does not run at 10 Hz.
        if base.static_items_signature != s || force || base.static_items.is_empty() {
            let mut so = OverlayContext::new(width, height, scale);
            self.panel.draw_static(&self.base, &mut so);
            self.base.static_items = so.items().to_vec();
            self.base.static_labels = so.labels().to_vec();
            self.base.static_items_signature = s;
        }
        let mut o = OverlayContext::new(width, height, scale);
        o.preload(&self.base.static_items, &self.base.static_labels);
        self.panel.draw_dynamic(&mut self.base, &mut o);
        if let Some((lines, anchor)) = &label {
            let floor = self.panel.hover_label_floor(&self.base);
            hover_label::draw(&mut o, lines, *anchor, &self.base.palette, floor);
        }
        self.base.text_layer.redraw(&o);
        true
    }

    pub fn set_layout(&mut self, width: f32, height: f32, scale: f32) {
        let (w, h) = (width.max(1.0), height.max(1.0));
        if w != self.base.width || h != self.base.height || scale != self.base.scale {
            self.base.width = w;
            self.base.height = h;
            self.base.scale = scale;
            self.base.needs_display = true;
            self.panel.layout_changed(&mut self.base);
        }
    }

    /// Records every pass for this panel. The caller submits the encoder.
    /// With no `target` only the history pre-pass runs (an offscreen render
    /// feeds many frames and draws once).
    pub fn encode(&mut self, encoder: &mut wgpu::CommandEncoder, target: Option<&wgpu::TextureView>) {
        let scale = self.base.scale;
        self.base.arena.begin_frame();
        self.base.batch.begin_frame(&mut self.base.arena, scale);
        self.base.drawing_this_frame = target.is_some();
        self.panel.encode_pre_pass(&mut self.base, encoder);
        let globals = Globals {
            view_size: [self.base.width, self.base.height],
            scale,
            time: (self.base.last_host_time % 3600.0) as f32,
        };
        if let Some(target) = target {
            let bg = self.base.palette.background;
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Joseon panel"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: target,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: bg[0] as f64,
                            g: bg[1] as f64,
                            b: bg[2] as f64,
                            a: 1.0,
                        }),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            self.panel.draw(&mut self.base, &mut pass);
            self.base.clear_scissor(&mut pass);
            let base = &mut self.base;
            base.text_layer.upload(&base.ctx);
            base.text_layer.draw(&mut pass, &base.ctx, &mut base.arena);
        }
        self.base.arena.upload(&self.base.ctx.queue, &globals);
    }
}

/// Cheap fingerprint of what a panel can show: sparse samples, no allocation. Equal digests = same picture.
fn digest(f: &AnalysisFrame) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut add = |v: f32| h = (h ^ v.to_bits() as u64).wrapping_mul(0x0100_0000_01b3);
    let s = &f.spectrum;
    let n = s
        .mid
        .len()
        .min(s.peak_hold.len())
        .min(s.left.len())
        .min(s.right.len())
        .min(s.average.len());
    let mut i = 0;
    while i < n {
        add(s.mid[i]);
        add(s.peak_hold[i]);
        add(s.left[i]);
        add(s.right[i]);
        add(s.average[i]);
        i += 17;
    }
    let l = &f.loudness;
    for v in [
        l.momentary_lufs,
        l.short_term_lufs,
        l.integrated_lufs,
        l.true_peak_left_dbtp,
        l.true_peak_right_dbtp,
        l.rms_left_db,
        l.rms_right_db,
        l.clip_count as f32,
        l.loudness_range_lu,
        f.bands.bass,
        f.bands.mid,
        f.bands.air,
    ] {
        add(v);
    }
    let pts = &f.stereo.scope_points;
    add(pts.len() as f32);
    if let (Some(a), Some(b)) = (pts.first(), pts.last()) {
        add(a[0]);
        add(a[1]);
        add(b[0]);
        add(b[1]);
    }
    add(f.stereo.correlation);
    add(f.headphone.as_ref().map_or(-1.0, |h| h.stress_flags.len() as f32));
    add(f.headphone.as_ref().map_or(-1.0, |h| h.model_name.len() as f32));
    add(f.peak.frequency_hz);
    // Level at the ear: only when a calibration is set.
    if let Some(e) = &f.spl {
        for v in [
            e.level_a_fast,
            e.level_a_slow,
            e.max_a_fast,
            e.dose_niosh,
            e.dose_who_weekly,
            e.calibration_name.len() as f32,
        ] {
            add(v);
        }
        let bands = &e.band_levels_eardrum;
        if let (Some(a), Some(b)) = (bands.first(), bands.last()) {
            add(*a);
            add(*b);
            add(bands[bands.len() / 2]);
        }
    }
    h
}
