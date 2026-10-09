//! The two small pictures of Joseon outside its window: the tray icon and the
//! mini strip (a small window that stays over the other windows). Both show
//! the mini spectrum.
//!
//! Port source: `StatusItemController.swift`. The Mac has one place for this,
//! the menu bar. A Windows tray icon is 16 points wide, so the picture there
//! is a sign that sound plays; the strip is where the shape can be read.

use joseon_core::contracts::AnalysisFrame;
use joseon_render::contracts::PanelKind;
use joseon_render::gpu::CurveUniforms;
use joseon_render::mini_spectrum::{self, MiniSpectrum};
use joseon_render::palette::{with_alpha, Theme};
use joseon_render::panel::{Panel, PanelBase, PanelRenderer, Signature};
use joseon_render::target::OffscreenTarget;
use joseon_render::text::fonts::{self, Weight};
use joseon_render::text::{HAlign, OverlayContext, VAlign};
use joseon_render::RenderContext;
use std::any::Any;
use std::sync::Arc;

/// The tray icon: 16 points, drawn at 2 pixels per point.
pub const ICON_POINTS: f32 = 16.0;
pub const ICON_SCALE: f32 = 2.0;
/// The mini strip in points.
pub const STRIP_SIZE: (f32, f32) = (240.0, 52.0);

/// Makes the picture of the tray icon. A picture comes back only when it changed.
pub struct TrayPicture {
    mini: MiniSpectrum,
    last: Vec<u8>,
    last_host_time: f64,
}

impl Default for TrayPicture {
    fn default() -> Self {
        Self::new()
    }
}

impl TrayPicture {
    pub fn new() -> Self {
        let mut mini = MiniSpectrum::new();
        // 16 points hold no fine picture: one column per 2 pixels, and a faster fall so the icon moves with the beat.
        mini.column_pixels = 2;
        mini.release_seconds = 0.12;
        Self {
            mini,
            last: Vec::new(),
            last_host_time: f64::NAN,
        }
    }

    /// The icon for `frame` as (width, height, RGBA). `None` when it is the picture of the last call.
    /// `engine_tilt` = the tilt the engine put on the spectrum already.
    pub fn icon(&mut self, frame: &AnalysisFrame, color: [f32; 4], engine_tilt: f32) -> Option<(u32, u32, Vec<u8>)> {
        if frame.host_time == self.last_host_time && !self.last.is_empty() {
            return None;
        }
        self.last_host_time = frame.host_time;
        self.mini.tilt_db_per_octave = mini_spectrum::tilt(engine_tilt);
        let (w, h, rgba) = self.mini.render(frame, ICON_POINTS, ICON_POINTS, ICON_SCALE, color);
        if rgba == self.last {
            return None;
        }
        self.last.clone_from(&rgba);
        Some((w, h, rgba))
    }
}

/// The tooltip of the tray icon: the song, or the state.
pub fn tooltip(song: &str, headline: &str) -> String {
    let text = if song.is_empty() {
        format!("Joseon \u{2013} {headline}")
    } else {
        format!("Joseon \u{2013} {song}")
    };
    // Windows cuts a tooltip at 127 characters.
    text.chars().take(120).collect()
}

/// The mini strip as a panel: the silhouette, and the song over it.
struct StripPanel {
    mini: MiniSpectrum,
    color: [f32; 4],
    engine_tilt: f32,
    song: String,
    any: bool,
}

impl Panel for StripPanel {
    // The strip is not one of the panels: it never takes part in the shared cursor. Any kind does.
    fn kind(&self) -> PanelKind {
        PanelKind::Spectrum
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn update(&mut self, base: &mut PanelBase, frame: &AnalysisFrame, _dt: f64) {
        self.mini.tilt_db_per_octave = mini_spectrum::tilt(self.engine_tilt);
        let pixels = ((Self::plot_width(base) * base.scale).round() as usize).max(1);
        let n = self.mini.column_count(pixels);
        self.any = self.mini.update(frame, n, Self::plot_width(base) / n as f32);
    }

    fn static_signature(&mut self, base: &PanelBase) -> u64 {
        let mut h = Signature::new();
        h.add(base.static_signature()).add(&self.song);
        h.finish()
    }

    fn dynamic_signature(&self, _base: &PanelBase) -> u64 {
        0
    }

    fn draw(&mut self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        let p = base.palette;
        let (w, h) = (base.width, base.height);
        base.rect([0.0, 0.0, w, h], p.panel);
        base.batch.gradient_rect(
            &mut base.arena,
            [0.5, 0.5, w - 1.0, h - 1.0],
            [1.0, 1.0, 1.0, 0.10],
            [1.0, 1.0, 1.0, 0.10],
            0.0,
            0.0,
            1.0,
        );
        // The hairline: the whole picture at silence.
        let (x0, bottom) = (Self::MARGIN, h - Self::MARGIN);
        base.rect(
            [x0, bottom - 1.0, Self::plot_width(base), 1.0],
            with_alpha(self.color, 0.9),
        );
        base.flush(pass);
        if !self.any {
            return;
        }
        let top = Self::plot_top(self);
        let Some(values) = base.arena.push_floats(self.mini.shown()) else {
            return;
        };
        let uniforms = CurveUniforms {
            rect: [x0, top, Self::plot_width(base), bottom - 1.0 - top],
            color: self.color,
            fill_top: 1.0,
            fill_bottom: 0.55,
            ..CurveUniforms::default()
        };
        base.draw_curve_fill(pass, values, (1.0, 1.0), uniforms, None);
    }

    fn draw_static(&mut self, base: &PanelBase, o: &mut OverlayContext) {
        if self.song.is_empty() {
            return;
        }
        let font = fonts::ui(10.5, Weight::Regular);
        let room = base.width - Self::MARGIN * 2.0;
        let mut text = self.song.clone();
        while o.measure(&text, &font, 0.0) > room && text.chars().count() > 4 {
            let shorter: String = text.chars().take(text.chars().count() - 4).collect();
            text = format!("{}\u{2026}", shorter.trim_end_matches(['\u{2026}', ' ']));
        }
        let color = with_alpha(base.palette.text, 0.75);
        o.text(
            &text,
            Self::MARGIN,
            Self::MARGIN + 6.0,
            &font,
            color,
            HAlign::Left,
            VAlign::Middle,
        );
    }
}

impl StripPanel {
    const MARGIN: f32 = 6.0;

    fn plot_width(base: &PanelBase) -> f32 {
        (base.width - Self::MARGIN * 2.0).max(8.0)
    }

    /// The song takes a row of its own over the picture.
    fn plot_top(&self) -> f32 {
        Self::MARGIN + if self.song.is_empty() { 0.0 } else { 13.0 }
    }
}

/// The mini strip. It draws into any texture: the strip window, or a test.
pub struct StripView {
    ctx: Arc<RenderContext>,
    renderer: PanelRenderer,
    last_host_time: f64,
}

impl StripView {
    /// `None` when the system gives no GPU device.
    pub fn new(scale: f32) -> Option<Self> {
        let ctx = RenderContext::shared()?;
        let mut renderer = PanelRenderer::with_panel(
            ctx.clone(),
            Theme::default(),
            Box::new(StripPanel {
                mini: MiniSpectrum::new(),
                color: [1.0; 4],
                engine_tilt: 0.0,
                song: String::new(),
                any: false,
            }),
        );
        renderer.set_layout(STRIP_SIZE.0, STRIP_SIZE.1, scale);
        Some(Self {
            ctx,
            renderer,
            last_host_time: f64::NAN,
        })
    }

    pub fn set_scale(&mut self, scale: f32) {
        self.renderer.set_layout(STRIP_SIZE.0, STRIP_SIZE.1, scale);
    }

    pub fn pixel_size(&self) -> (u32, u32) {
        let scale = self.renderer.base.scale;
        (
            (STRIP_SIZE.0 * scale).round() as u32,
            (STRIP_SIZE.1 * scale).round() as u32,
        )
    }

    /// One display tick. `song` = "Artist – Title", or empty.
    pub fn tick(&mut self, now: f64, frame: &Arc<AnalysisFrame>, color: [f32; 4], engine_tilt: f32, song: &str) {
        if let Some((panel, base)) = self.renderer.panel_as::<StripPanel>() {
            if panel.color != color || panel.song != song {
                base.needs_display = true;
            }
            panel.color = color;
            panel.engine_tilt = engine_tilt;
            if panel.song != song {
                panel.song = song.to_string();
            }
        }
        if frame.host_time != self.last_host_time {
            self.last_host_time = frame.host_time;
            self.renderer.ingest(frame.clone());
        }
        self.renderer.refresh_text(now, false);
    }

    pub fn render(&mut self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        self.renderer.encode(encoder, Some(target));
    }

    /// The strip as RGBA pixels, for tests.
    pub fn snapshot_rgba(&mut self, now: f64) -> Option<(u32, u32, Vec<u8>)> {
        let (w, h) = self.pixel_size();
        let target = OffscreenTarget::new(&self.ctx, w, h);
        self.renderer.refresh_text(now, true);
        let mut encoder = self.ctx.device.create_command_encoder(&Default::default());
        self.render(&mut encoder, &target.view);
        self.ctx.queue.submit([encoder.finish()]);
        Some((w, h, target.read_rgba(&self.ctx)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joseon_core::contracts::SpectrumReading;
    use joseon_render::synthetic::{Options, SyntheticFrames};

    #[test]
    fn the_tray_icon_changes_only_with_the_music() {
        let mut tray = TrayPicture::new();
        let white = [1.0; 4];
        let silent = AnalysisFrame::new(SpectrumReading::silent(1024));
        let (w, h, rgba) = tray.icon(&silent, white, 0.0).expect("the first picture");
        assert_eq!((w, h, rgba.len()), (32, 32, 32 * 32 * 4));
        // At silence: the hairline at the bottom, nothing else.
        let alpha = |rgba: &[u8], x: usize, y: usize| rgba[(y * 32 + x) * 4 + 3];
        assert_eq!((alpha(&rgba, 16, 31), alpha(&rgba, 16, 20)), (255, 0));
        // The same frame, and the same picture from a new frame: nothing to send to the tray.
        assert!(tray.icon(&silent, white, 0.0).is_none());
        let mut later = silent.clone();
        later.host_time = 0.5;
        assert!(tray.icon(&later, white, 0.0).is_none());
        // Music: a new picture with a filled part, in the color.
        let frames = SyntheticFrames::sequence(30, Options::default());
        let mut last = None;
        for frame in &frames {
            if let Some(picture) = tray.icon(frame, [1.0, 0.58, 0.10, 1.0], 0.0) {
                last = Some(picture);
            }
        }
        let (_, _, rgba) = last.expect("music changes the icon");
        let filled = rgba.chunks_exact(4).filter(|p| p[3] > 128).count();
        assert!(filled > 60 && filled < 900, "{filled} pixels are filled");
        assert!(rgba.chunks_exact(4).any(|p| p == [255, 148, 26, 255]));
    }

    #[test]
    fn the_tooltip_names_the_song_or_the_state() {
        assert_eq!(
            tooltip("Vaundy \u{2013} Saikai", "Spotify"),
            "Joseon \u{2013} Vaundy \u{2013} Saikai"
        );
        assert_eq!(tooltip("", "Waiting for audio"), "Joseon \u{2013} Waiting for audio");
        assert_eq!(tooltip(&"x".repeat(300), "").chars().count(), 120);
    }

    #[test]
    fn the_strip_draws_the_silhouette_and_the_song() {
        if !fonts::available() {
            return;
        }
        let Some(mut strip) = StripView::new(2.0) else { return };
        let orange = [1.0, 0.58, 0.10, 1.0];
        let frames = SyntheticFrames::sequence(120, Options::default());
        for (i, frame) in frames.into_iter().enumerate() {
            strip.tick(
                100.0 + i as f64 / 60.0,
                &Arc::new(frame),
                orange,
                0.0,
                "Vaundy \u{2013} Saikai",
            );
        }
        let (w, h, rgba) = strip.snapshot_rgba(200.0).unwrap();
        assert_eq!((w, h), (480, 104));
        let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/joseon-app-review");
        std::fs::create_dir_all(&directory).unwrap();
        std::fs::write(
            directory.join("strip.png"),
            joseon_render::target::encode_png(w, h, &rgba),
        )
        .unwrap();
        // Orange in the lower half (the silhouette), light text in the top row, the panel color in a corner.
        let px = |x: u32, y: u32| {
            let i = ((y * w + x) * 4) as usize;
            [rgba[i], rgba[i + 1], rgba[i + 2]]
        };
        let orange_pixels = (0..w).filter(|x| {
            let p = px(*x, h - 20);
            p[0] > 90 && p[0] as u32 > p[2] as u32 * 2
        });
        assert!(orange_pixels.count() > 40, "the silhouette is drawn");
        let text_pixels = (12..300).filter(|x| px(*x, 24).iter().all(|c| *c > 120)).count();
        assert!(text_pixels > 10, "the song is written: {text_pixels}");
        assert!(px(w - 6, 8).iter().all(|c| *c < 60));
    }
}
