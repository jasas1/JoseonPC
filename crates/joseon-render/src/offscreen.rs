//! Renders a panel offscreen for a sequence of frames and gives PNG data. Used
//! by tests, by design review and by the probe. The drawing goes through the
//! same `PanelRenderer` the views on screen use. No window is necessary.
//!
//! Port source: `Sources/JoseonRender/OffscreenRenderer.swift`.

use crate::context::RenderContext;
use crate::contracts::{
    ComparisonMode, CursorHistorySlice, DoseStandard, LevelAxis, PanelCursor, PanelKind, SpectrogramMode,
    SpectrumViewOptions, VectorscopeMode,
};
use crate::meters::MetersPanel;
use crate::palette::Theme;
use crate::panel::PanelRenderer;
use crate::spectrogram::SpectrogramPanel;
use crate::spectrum::SpectrumPanel;
use crate::target::{encode_png, OffscreenTarget};
use crate::timeline::TimelinePanel;
use crate::vectorscope::VectorscopePanel;
use joseon_core::contracts::AnalysisFrame;
use std::sync::Arc;
use std::time::Instant;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderError {
    /// No GPU adapter gives a device.
    NoDevice,
    EncodeFailed,
    /// The panel kind is not ported yet.
    NotImplemented,
}

impl std::fmt::Display for RenderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::NoDevice => "no GPU device",
            Self::EncodeFailed => "the picture could not be made",
            Self::NotImplemented => "this panel is not ported yet",
        })
    }
}

impl std::error::Error for RenderError {}

/// Per-panel settings that the views expose as properties.
#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub spectrum: SpectrumViewOptions,
    pub history_seconds: f64,
    /// Pointer position in points, top-left origin. Draws the crosshair and the hover label.
    pub hover: Option<[f32; 2]>,
    pub increase_contrast: bool,
    pub spectrum_auto_range: bool,
    pub vectorscope_mode: VectorscopeMode,
    /// The spectrogram as the flat picture or as the 3-D field.
    pub spectrogram_mode: SpectrogramMode,
    /// Waterfall: the camera turned by the user, as (yaw, elevation) in radians. `None` = the resting view.
    pub waterfall_view: Option<(f32, f32)>,
    /// Waterfall: the zoom (1 = the fitted view).
    pub waterfall_zoom: f32,
    pub show_stress_bands: bool,
    pub target_lufs: Option<f32>,
    pub level_axis: LevelAxis,
    pub dose_standard: DoseStandard,
    /// A linked cursor with no window: the panel draws it as a view with a
    /// cursor link does. `None` = a panel with no link.
    pub cursor: Option<PanelCursor>,
    /// The past spectrum of a timed cursor, for the ghost trace of the spectrum.
    pub cursor_history_slice: Option<CursorHistorySlice>,
    pub comparison_mode: ComparisonMode,
    pub spectrum_auto_declutter: bool,
    /// The record the session timeline shows. `None` = an empty record.
    pub session: Option<joseon_core::contracts::SessionSnapshot>,
    pub timeline_window_seconds: i64,
    pub timeline_show_bands: bool,
    pub timeline_show_level_at_ear: bool,
    /// Seconds east of UTC of the labels of the timeline. `None` = UTC.
    pub timeline_utc_offset_seconds: Option<i32>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            spectrum: SpectrumViewOptions::default(),
            history_seconds: 20.0,
            hover: None,
            increase_contrast: false,
            spectrum_auto_range: true,
            vectorscope_mode: VectorscopeMode::Lissajous,
            spectrogram_mode: SpectrogramMode::Flat,
            waterfall_view: None,
            waterfall_zoom: 1.0,
            show_stress_bands: true,
            target_lufs: None,
            level_axis: LevelAxis::DbFs,
            dose_standard: DoseStandard::NioshDaily,
            cursor: None,
            cursor_history_slice: None,
            comparison_mode: ComparisonMode::Signal,
            spectrum_auto_declutter: true,
            session: None,
            timeline_window_seconds: 900,
            timeline_show_bands: true,
            timeline_show_level_at_ear: false,
            timeline_utc_offset_seconds: None,
        }
    }
}

/// Measured cost of one panel frame.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct FrameCost {
    /// CPU time to ingest a frame and encode the command buffer, milliseconds (median).
    pub cpu_encode_ms: f64,
    /// Time from the submit to the end of the GPU work, milliseconds (median).
    pub gpu_ms: f64,
    /// CPU time of one full text repaint, milliseconds (median). On screen this runs at most 10 times per second.
    pub text_ms: f64,
}

/// A PNG of `panel` after it saw `frames`, at `width` x `height` points and `scale` pixels per point.
pub fn png(
    panel: PanelKind,
    frames: &[AnalysisFrame],
    width: f32,
    height: f32,
    scale: f32,
    theme: Theme,
    settings: &Settings,
) -> Result<Vec<u8>, RenderError> {
    let mut session = Session::new(panel, width, height, scale, theme, settings)?;
    session.feed(frames);
    session.snapshot_png()
}

/// Steady-state frame cost: feeds `frames` once for history, then times `iterations` more frames.
pub fn measure(
    panel: PanelKind,
    frames: &[AnalysisFrame],
    width: f32,
    height: f32,
    scale: f32,
    iterations: usize,
    settings: &Settings,
) -> Result<FrameCost, RenderError> {
    let mut session = Session::new(panel, width, height, scale, Theme::default(), settings)?;
    session.feed(frames);
    let last = frames.last().ok_or(RenderError::EncodeFailed)?;
    let (mut cpu, mut gpu, mut text) = (Vec::new(), Vec::new(), Vec::new());
    let mut host_time = last.host_time;
    let ctx = session.ctx.clone();
    for i in 0..iterations.max(1) {
        let mut f = frames[i % frames.len()].clone();
        host_time += 1.0 / 60.0;
        f.host_time = host_time;
        let start = Instant::now();
        session.renderer.ingest(Arc::new(f));
        let mut encoder = ctx.device.create_command_encoder(&Default::default());
        session.renderer.encode(&mut encoder, Some(&session.target.view));
        let submitted = ctx.queue.submit([encoder.finish()]);
        cpu.push(start.elapsed().as_secs_f64() * 1000.0);
        let wait = Instant::now();
        let _ = ctx.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submitted),
            timeout: None,
        });
        gpu.push(wait.elapsed().as_secs_f64() * 1000.0);
        if i % 6 == 0 {
            let start = Instant::now();
            session.renderer.refresh_text(host_time, true);
            text.push(start.elapsed().as_secs_f64() * 1000.0);
        }
    }
    let median = |mut v: Vec<f64>| {
        v.sort_by(f64::total_cmp);
        v.get(v.len() / 2).copied().unwrap_or(0.0)
    };
    Ok(FrameCost {
        cpu_encode_ms: median(cpu),
        gpu_ms: median(gpu),
        text_ms: median(text),
    })
}

/// One panel with its offscreen target.
pub struct Session {
    pub ctx: Arc<RenderContext>,
    pub renderer: PanelRenderer,
    pub target: OffscreenTarget,
    pub width: f32,
    pub height: f32,
    pub scale: f32,
}

impl Session {
    pub fn new(
        panel: PanelKind,
        width: f32,
        height: f32,
        scale: f32,
        theme: Theme,
        settings: &Settings,
    ) -> Result<Self, RenderError> {
        let ctx = RenderContext::shared().ok_or(RenderError::NoDevice)?;
        let mut renderer = PanelRenderer::make(panel, ctx.clone(), theme).ok_or(RenderError::NotImplemented)?;
        let pixel_width = ((width * scale).round() as u32).max(1);
        let pixel_height = ((height * scale).round() as u32).max(1);
        let target = OffscreenTarget::new(&ctx, pixel_width, pixel_height);

        renderer.set_high_contrast(settings.increase_contrast);
        renderer.set_hover(settings.hover);
        if let Some((spectrum, base)) = renderer.panel_as::<SpectrumPanel>() {
            spectrum.set_options(base, settings.spectrum);
            spectrum.set_auto_range(base, settings.spectrum_auto_range);
            spectrum.set_show_stress_bands(base, settings.show_stress_bands);
            spectrum.set_level_axis(base, settings.level_axis);
            spectrum.set_auto_declutter(base, settings.spectrum_auto_declutter);
        }
        if let Some((spectrogram, base)) = renderer.panel_as::<SpectrogramPanel>() {
            spectrogram.set_history_seconds(base, settings.history_seconds);
            spectrogram.set_mode(base, settings.spectrogram_mode);
            if settings.waterfall_view.is_some() || settings.waterfall_zoom != 1.0 {
                spectrogram.set_view(base, settings.waterfall_view, settings.waterfall_zoom);
            }
        }
        if let Some((meters, base)) = renderer.panel_as::<MetersPanel>() {
            meters.set_target_lufs(base, settings.target_lufs);
            meters.set_dose_standard(base, settings.dose_standard);
        }
        if let Some((timeline, base)) = renderer.panel_as::<TimelinePanel>() {
            timeline.set_window_seconds(base, settings.timeline_window_seconds);
            timeline.set_target_lufs(base, settings.target_lufs);
            timeline.set_show_bands(base, settings.timeline_show_bands);
            timeline.set_show_level_at_ear(base, settings.timeline_show_level_at_ear);
            timeline.set_utc_offset_seconds(base, settings.timeline_utc_offset_seconds.unwrap_or(0));
            timeline.set_snapshot(base, settings.session.clone().unwrap_or_default());
        }
        if let Some((scope, base)) = renderer.panel_as::<VectorscopePanel>() {
            scope.set_mode(base, settings.vectorscope_mode);
        }
        if let Some(c) = settings.cursor {
            renderer.set_cursor_linked(true);
            renderer.set_cursor(Some(c));
            renderer.set_cursor_slice(settings.cursor_history_slice.clone());
        }
        renderer.set_layout(width, height, scale);
        Ok(Self {
            ctx,
            renderer,
            target,
            width,
            height,
            scale,
        })
    }

    /// Feeds frames in order. History panels run their GPU pre-pass for every frame.
    pub fn feed(&mut self, frames: &[AnalysisFrame]) {
        for (i, f) in frames.iter().enumerate() {
            self.renderer.ingest(Arc::new(f.clone()));
            let last = i == frames.len() - 1;
            if self.renderer.panel.needs_pre_pass_per_frame() && !last {
                let mut encoder = self.ctx.device.create_command_encoder(&Default::default());
                self.renderer.encode(&mut encoder, None);
                self.ctx.queue.submit([encoder.finish()]);
            }
        }
    }

    /// The pixels of the panel now: RGBA, row by row from the top.
    pub fn snapshot_rgba(&mut self) -> Result<Vec<u8>, RenderError> {
        // The text is laid out before the GPU pass: the legend row decides which traces the pass draws.
        let now = self.renderer.base.last_host_time;
        self.renderer.refresh_text(now, true);
        let mut encoder = self.ctx.device.create_command_encoder(&Default::default());
        self.renderer.encode(&mut encoder, Some(&self.target.view));
        self.ctx.queue.submit([encoder.finish()]);
        self.target.read_rgba(&self.ctx).ok_or(RenderError::EncodeFailed)
    }

    pub fn snapshot_png(&mut self) -> Result<Vec<u8>, RenderError> {
        let rgba = self.snapshot_rgba()?;
        Ok(encode_png(self.target.width, self.target.height, &rgba))
    }
}
