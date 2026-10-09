//! The main window as a picture: the header, the banners, and a card for every
//! panel. Also what the pointer and the keys do to it.
//!
//! Every panel draws into its own texture. The chrome (card frames, titles,
//! header text) is one more "panel" as large as the window: it draws the card
//! backgrounds, puts the panel pictures into them, and draws its text on top.
//! The scene draws into any texture view, so the window, the snapshot mode and
//! the tests share all of it.
//!
//! Port source: `MainWindowController.swift`, `PanelCardView.swift`, the pointer and key
//! handling of `Panels.swift`, and the stream block of `HeaderView.swift`.

use crate::layout::{self, Card, Divider, WindowLayout, CARD_RADIUS};
use crate::menu::{self, Command, Effect, Entry};
use crate::model::{Banner, CurveNames, HeaderState, Relation, SignalState, StressState};
use crate::settings::{AppSettings, LayoutPreset};
use crate::stress_strip::{self, StripLayout};
use bytemuck::{Pod, Zeroable};
use joseon_core::contracts::StressSeverity;
use joseon_core::contracts::{AnalysisFrame, SessionSnapshot};
use joseon_render::contracts::{PanelCursor, PanelCursorLink, PanelKind, VectorscopeMode};
use joseon_render::cursor;
use joseon_render::fmt;
use joseon_render::gpu::Color;
use joseon_render::meters::MetersPanel;
use joseon_render::palette::{mix, with_alpha, Theme, WHITE};
use joseon_render::panel::{Panel, PanelBase, PanelRenderer, Signature};
use joseon_render::spectrogram::SpectrogramPanel;
use joseon_render::spectrum::SpectrumPanel;
use joseon_render::target::OffscreenTarget;
use joseon_render::text::fonts::{self, Weight};
use joseon_render::text::{HAlign, OverlayContext, Rect, VAlign};
use joseon_render::timeline::TimelinePanel;
use joseon_render::vectorscope::VectorscopePanel;
use joseon_render::RenderContext;
use std::any::Any;
use std::sync::Arc;

pub const SCOPE_TITLE: &str = "Vectorscope";
pub const PLACEMENT_TITLE: &str = "Stereo placement";
/// A frozen display: the graph goes to 70 % so the card reads as "not live".
const DIMMED_ALPHA: f32 = 0.7;
const LIVE: Color = [0.25, 0.85, 0.45, 1.0];
const DEMO: Color = [0.72, 0.55, 1.0, 1.0];
/// Frequency range of the keyboard cursor: the axis every panel shows.
const CURSOR_RANGE_HZ: (f32, f32) = (20.0, 20_000.0);

#[repr(C)]
#[derive(Clone, Copy, Pod, Zeroable)]
struct BlitUniforms {
    rect: [f32; 4],
    radius: f32,
    alpha: f32,
    pad: [f32; 2],
}

/// The keys the window knows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    /// Space: freeze the display.
    Freeze,
    /// R: reset the measurement.
    Reset,
    /// F: full screen.
    FullScreen,
    Escape,
    /// Ctrl+1, 2, 3.
    Preset(LayoutPreset),
    /// Ctrl+4: the vectorscope card shows stereo placement (in "Spectrum": the placement card).
    TogglePlacement,
    /// Ctrl+5: the timeline strip.
    ToggleTimeline,
    /// Left and right: the cursor moves one semitone (`octave`: one octave).
    CursorDown {
        octave: bool,
    },
    CursorUp {
        octave: bool,
    },
    /// Return: pin the cursor, or let it go.
    Pin,
}

/// What the owner of the scene has to do after an input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    /// The settings changed: store them and give them to the engine.
    SettingsChanged,
    ToggleFreeze,
    ResetMeasurement,
    ToggleFullScreen,
    ClearTimeline,
    /// Ask the user for a curve file and take it in.
    ImportCurve,
    OpenCurvesFolder,
    CheckUpdates,
    InstallUpdate,
    /// The settings changed, and the audio source has to start again.
    RestartSource,
    /// Show this menu at the pointer.
    OpenMenu(MenuTarget),
    /// Show the main window and bring it to the front.
    OpenWindow,
    Quit,
}

/// A menu of the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MenuTarget {
    /// The options of a card.
    Card(Card),
    /// The settings menu of the header.
    Settings,
    /// The headphones of the library.
    Headphone,
}

/// The "..." button in the title row of a card.
pub fn options_button(card: Rect) -> Rect {
    Rect::new(card.max_x() - 8.0 - 22.0, card.y + 3.0, 22.0, 22.0)
}

/// The settings button of the header, left of the layout buttons.
pub fn settings_button(width: f32) -> Rect {
    let first = preset_buttons(width)[0].1;
    Rect::new(first.x - 10.0 - 28.0, first.y, 28.0, first.h)
}

/// The pointer shape the window shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PointerShape {
    Arrow,
    Crosshair,
    Hand,
    ResizeColumns,
    ResizeRows,
}

/// One card: a panel with its picture.
struct CardView {
    card: Card,
    renderer: PanelRenderer,
    target: Option<OffscreenTarget>,
    group: Option<wgpu::BindGroup>,
    /// The frame of the card and of its panel in the window. `None` = not in this layout.
    rects: Option<(Rect, Rect)>,
    last_host_time: f64,
    /// A frame went in and no picture was drawn since. A panel with a history (the spectrogram, the scope)
    /// then draws that frame into its history before it takes the next one.
    undrawn: bool,
}

/// What the chrome draws for one card.
#[derive(Clone)]
struct ChromeCard {
    rect: Rect,
    panel: Rect,
    title: String,
    /// Quiet text beside the title.
    state: Option<String>,
    /// Quiet text on a plate in the middle of the panel.
    plate: Option<String>,
    group: Option<wgpu::BindGroup>,
    dimmed: bool,
}

/// The layer under and over the panel pictures: card frames, titles, the header.
struct ChromePanel {
    cards: Vec<ChromeCard>,
    header: HeaderState,
    banners: Vec<(Rect, Banner)>,
    preset: LayoutPreset,
    frozen: bool,
    /// Changes when the text changes.
    text_version: u64,
    width: f32,
    strip: StripLayout,
}

/// The three layout buttons at the right of the header.
pub fn preset_buttons(width: f32) -> [(LayoutPreset, Rect); 3] {
    let (w, h, gap) = (84.0, 24.0, 4.0);
    let right = width - 14.0;
    let y = (layout::HEADER_HEIGHT - h) / 2.0;
    let mut out = [(LayoutPreset::Essential, Rect::default()); 3];
    for (i, preset) in LayoutPreset::ALL.into_iter().enumerate() {
        let x = right - (3 - i) as f32 * w - (2 - i) as f32 * gap;
        out[i] = (preset, Rect::new(x, y, w, h));
    }
    out
}

impl ChromePanel {
    fn secondary(base: &PanelBase) -> Color {
        if base.high_contrast() {
            [1.0, 1.0, 1.0, 0.9]
        } else {
            with_alpha(base.palette.text, 0.62)
        }
    }

    fn border(base: &PanelBase) -> Color {
        [1.0, 1.0, 1.0, if base.high_contrast() { 0.55 } else { 0.08 }]
    }

    fn plate_rect(o: &OverlayContext, card: &ChromeCard, text: &str) -> Rect {
        let font = fonts::ui(12.0, Weight::Medium);
        let w = (o.measure(text, &font, 0.0) + 24.0).min(card.panel.w - 16.0);
        let h = 28.0;
        // The top 22 pt of a panel are its own header row: the plate never stands there.
        let y = (card.panel.mid_y() - h / 2.0).max(card.panel.y + 26.0);
        Rect::new(card.panel.mid_x() - w / 2.0, y, w, h)
    }
}

impl Panel for ChromePanel {
    // The chrome is not one of the panels: it never takes part in the shared cursor. Any kind does.
    fn kind(&self) -> PanelKind {
        PanelKind::Meters
    }

    fn as_any(&self) -> &dyn Any {
        self
    }

    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn uses_analysis_frames(&self) -> bool {
        false
    }

    fn static_signature(&mut self, base: &PanelBase) -> u64 {
        let mut h = Signature::new();
        h.add(base.static_signature()).add(self.text_version);
        h.finish()
    }

    fn dynamic_signature(&self, _base: &PanelBase) -> u64 {
        self.text_version
    }

    fn draw(&mut self, base: &mut PanelBase, pass: &mut wgpu::RenderPass<'_>) {
        let p = base.palette;
        let ctx = base.ctx.clone();
        let border = Self::border(base);
        // Card backgrounds.
        for card in &self.cards {
            let r = card.rect;
            base.rounded_rect([r.x, r.y, r.w, r.h], p.panel, CARD_RADIUS);
        }
        // The strip under the header: a quiet band between two hairlines, the chips, the button.
        let strip = &self.strip;
        if strip.rect.w > 0.0 {
            let r = strip.rect;
            base.rect([r.x, r.y, r.w, r.h], with_alpha(p.panel, 0.6));
            base.hline(r.x, r.max_x(), r.y + 0.5, border);
            base.hline(r.x, r.max_x(), r.max_y() - 0.5, border);
            if let Some(center) = strip.mark {
                base.circle(center, 4.5, LIVE, 1.3, 0.0);
            }
            if let Some((b, _)) = &strip.button {
                base.rounded_rect([b.x, b.y, b.w, b.h], with_alpha(p.text, 0.10), 6.0);
            }
            for chip in &strip.chips {
                let (c, r) = (Self::severity_color(&p, chip.severity), chip.rect);
                base.rounded_rect([r.x, r.y, r.w, r.h], with_alpha(c, 0.15), 8.0);
                base.batch.gradient_rect(
                    &mut base.arena,
                    [r.x, r.y, r.w, r.h],
                    with_alpha(c, 0.6),
                    with_alpha(c, 0.6),
                    8.0,
                    0.0,
                    1.0,
                );
                base.circle([r.x + 8.0 + 5.0, r.mid_y()], 3.5, c, 0.0, 0.0);
            }
        }
        // Banners: a tinted row with a mark at its left.
        for (rect, banner) in &self.banners {
            let tint = if banner.is_error { p.warn } else { p.accent };
            let r = Rect::new(rect.x + 10.0, rect.y + 3.0, rect.w - 20.0, rect.h - 6.0);
            base.rounded_rect([r.x, r.y, r.w, r.h], with_alpha(tint, 0.14), 7.0);
            base.rounded_rect([r.x, r.y + 6.0, 3.0, r.h - 12.0], tint, 1.5);
        }
        // The header: the state mark, and the layout buttons.
        let mark = match self.header.state {
            SignalState::Live => LIVE,
            SignalState::Demo => DEMO,
            SignalState::Silent => with_alpha(p.text, 0.35),
        };
        base.circle([22.0, 20.0], 4.0, mark, 0.0, 0.0);
        for (preset, r) in preset_buttons(self.width) {
            let selected = preset == self.preset;
            let fill = if selected {
                with_alpha(p.accent, 0.28)
            } else {
                with_alpha(p.text, 0.06)
            };
            base.rounded_rect([r.x, r.y, r.w, r.h], fill, 6.0);
        }
        if self.frozen {
            let r = Self::paused_pill(self.width);
            base.rounded_rect([r.x, r.y, r.w, r.h], with_alpha(p.accent, 0.9), 6.0);
        }
        // The settings button: three lines.
        let r = settings_button(self.width);
        base.rounded_rect([r.x, r.y, r.w, r.h], with_alpha(p.text, 0.06), 6.0);
        for dy in [-4.5, 0.0, 4.5] {
            base.rounded_rect(
                [r.mid_x() - 6.0, r.mid_y() + dy - 0.75, 12.0, 1.5],
                Self::secondary(base),
                0.75,
            );
        }
        base.flush(pass);

        // The panel pictures.
        for card in &self.cards {
            let Some(group) = &card.group else { continue };
            let uniforms = BlitUniforms {
                rect: [card.panel.x, card.panel.y, card.panel.w, card.panel.h],
                // The bottom corners of the picture follow the card.
                radius: 0.0,
                alpha: if card.dimmed { DIMMED_ALPHA } else { 1.0 },
                pad: [0.0; 2],
            };
            if let Some(offset) = base.arena.push_uniform(&uniforms) {
                pass.set_pipeline(&ctx.blit);
                pass.set_bind_group(0, &base.arena.globals_group, &[]);
                pass.set_bind_group(1, &base.arena.uniform_group, &[offset]);
                pass.set_bind_group(2, group, &[]);
                pass.draw(0..4, 0..1);
            }
        }

        // Over the pictures: the card frames (they also round the corners of the pictures) and the plates.
        for card in &self.cards {
            let r = card.rect;
            // A ring in the window color hides the square corners of the picture.
            base.batch.gradient_rect(
                &mut base.arena,
                [r.x - 2.0, r.y - 2.0, r.w + 4.0, r.h + 4.0],
                p.background,
                p.background,
                CARD_RADIUS + 2.0,
                0.0,
                2.0,
            );
            base.batch.gradient_rect(
                &mut base.arena,
                [r.x, r.y, r.w, r.h],
                border,
                border,
                CARD_RADIUS,
                0.0,
                1.0,
            );
            // The options button: three dots.
            let b = options_button(r);
            for dx in [-5.0, 0.0, 5.0] {
                base.circle([b.mid_x() + dx, b.mid_y()], 1.5, Self::secondary(base), 0.0, 0.0);
            }
            if let Some(text) = &card.plate {
                let o = OverlayContext::new(base.width, base.height, base.scale);
                let plate = Self::plate_rect(&o, card, text);
                base.rounded_rect([plate.x, plate.y, plate.w, plate.h], with_alpha(p.panel, 0.88), 9.0);
                base.batch.gradient_rect(
                    &mut base.arena,
                    [plate.x, plate.y, plate.w, plate.h],
                    border,
                    border,
                    9.0,
                    0.0,
                    1.0,
                );
            }
        }
        // The detail of an open chip stands over the cards.
        if let Some(detail) = &self.strip.detail {
            let r = detail.rect;
            base.rounded_rect([r.x, r.y, r.w, r.h], mix(p.panel, WHITE, 0.06), 9.0);
            let edge = [1.0, 1.0, 1.0, 0.22];
            base.batch
                .gradient_rect(&mut base.arena, [r.x, r.y, r.w, r.h], edge, edge, 9.0, 0.0, 1.0);
        }
        base.flush(pass);
    }

    fn draw_static(&mut self, base: &PanelBase, o: &mut OverlayContext) {
        let p = &base.palette;
        let secondary = Self::secondary(base);
        let title_font = fonts::ui(10.5, Weight::Semibold);
        for card in &self.cards {
            let (x, y) = (card.rect.x + 12.0, card.rect.y + 14.0);
            let title = card.title.to_uppercase();
            let w = o.text_tracked(&title, x, y, &title_font, secondary, HAlign::Left, VAlign::Middle, 0.4);
            // The text beside the title shows whole or not at all.
            if let Some(state) = &card.state {
                let font = fonts::ui(11.0, Weight::Regular);
                if x + w + 10.0 + o.measure(state, &font, 0.0) <= options_button(card.rect).x - 6.0 {
                    o.text(state, x + w + 10.0, y, &font, secondary, HAlign::Left, VAlign::Middle);
                }
            }
            if let Some(text) = &card.plate {
                let plate = Self::plate_rect(o, card, text);
                let font = fonts::ui(12.0, Weight::Medium);
                o.text(
                    text,
                    plate.mid_x(),
                    plate.mid_y(),
                    &font,
                    secondary,
                    HAlign::Center,
                    VAlign::Middle,
                );
            }
        }
        for (preset, r) in preset_buttons(self.width) {
            let selected = preset == self.preset;
            let font = fonts::ui(11.5, if selected { Weight::Semibold } else { Weight::Regular });
            let color = if selected { mix(p.accent, WHITE, 0.6) } else { secondary };
            o.text(
                preset.title(),
                r.mid_x(),
                r.mid_y(),
                &font,
                color,
                HAlign::Center,
                VAlign::Middle,
            );
        }
        if self.frozen {
            let r = Self::paused_pill(self.width);
            let font = fonts::ui(10.5, Weight::Bold);
            o.text_tracked(
                "PAUSED",
                r.mid_x(),
                r.mid_y(),
                &font,
                p.background,
                HAlign::Center,
                VAlign::Middle,
                0.6,
            );
        }
        let strip = &self.strip;
        if strip.rect.w > 0.0 {
            let mid = strip.rect.mid_y();
            let x = strip.rect.x + 14.0;
            o.text_tracked(
                stress_strip::CAPTION,
                x,
                mid,
                &stress_strip::caption_font(),
                secondary,
                HAlign::Left,
                VAlign::Middle,
                0.4,
            );
            for (text, x) in &strip.texts {
                o.text(
                    text,
                    *x,
                    mid,
                    &stress_strip::quiet_font(),
                    secondary,
                    HAlign::Left,
                    VAlign::Middle,
                );
            }
            let chip_font = stress_strip::chip_font();
            if let Some((b, title)) = &strip.button {
                o.text(
                    title,
                    b.mid_x(),
                    b.mid_y(),
                    &chip_font,
                    p.text,
                    HAlign::Center,
                    VAlign::Middle,
                );
            }
            for chip in &strip.chips {
                let color = Self::severity_color(p, chip.severity);
                o.text(
                    &chip.title,
                    chip.rect.x + 8.0 + 13.0 + 4.0,
                    chip.rect.mid_y(),
                    &chip_font,
                    color,
                    HAlign::Left,
                    VAlign::Middle,
                );
            }
            if let Some((text, relation, right)) = &strip.loudness {
                let color = match relation {
                    Relation::Over => p.warn,
                    Relation::None => secondary,
                    _ => p.text,
                };
                o.text(
                    text,
                    *right,
                    mid,
                    &fonts::ui(11.5, Weight::Medium),
                    color,
                    HAlign::Right,
                    VAlign::Middle,
                );
            }
            if let Some(detail) = &strip.detail {
                let (x, mut y) = (detail.rect.x + 14.0, detail.rect.y + 14.0);
                for (title, severity, lines) in &detail.blocks {
                    let color = Self::severity_color(p, *severity);
                    o.text(
                        title,
                        x,
                        y,
                        &fonts::ui(12.5, Weight::Semibold),
                        color,
                        HAlign::Left,
                        VAlign::Top,
                    );
                    y += 22.0;
                    for line in lines {
                        o.text(
                            line,
                            x,
                            y,
                            &fonts::ui(11.5, Weight::Regular),
                            p.text,
                            HAlign::Left,
                            VAlign::Top,
                        );
                        y += 16.0;
                    }
                }
                o.text(
                    &detail.footnote,
                    x,
                    y + 4.0,
                    &fonts::ui(10.5, Weight::Regular),
                    secondary,
                    HAlign::Left,
                    VAlign::Top,
                );
            }
        }
        for (rect, banner) in &self.banners {
            let font = fonts::ui(12.0, Weight::Regular);
            let room = rect.w - 48.0;
            // The longest start of the sentence that fits.
            let mut text = banner.text.clone();
            while o.measure(&text, &font, 0.0) > room && text.chars().count() > 8 {
                let cut: String = text.chars().take(text.chars().count() - 6).collect();
                text = format!("{}\u{2026}", cut.trim_end_matches(['\u{2026}', ' ']));
            }
            o.text(
                &text,
                rect.x + 24.0,
                rect.mid_y(),
                &font,
                p.text,
                HAlign::Left,
                VAlign::Middle,
            );
        }
    }

    fn draw_dynamic(&mut self, base: &mut PanelBase, o: &mut OverlayContext) {
        let p = base.palette;
        let secondary = Self::secondary(base);
        // The stream block: what plays, then device · rate · depth.
        let room = settings_button(self.width).x - 20.0 - if self.frozen { 80.0 } else { 0.0 };
        let headline = self.header.headline();
        let strong = fonts::ui(13.0, Weight::Semibold);
        let color = if self.header.state == SignalState::Silent {
            secondary
        } else {
            p.text
        };
        if 34.0 + o.measure(&headline, &strong, 0.0) <= room {
            let w = o.text(&headline, 34.0, 20.0, &strong, color, HAlign::Left, VAlign::Middle);
            // The song, beside the programs that play: quiet, and cut with "…" where the row ends.
            let font = fonts::ui(12.0, Weight::Regular);
            let x = 34.0 + w + 14.0;
            let mut song = self.header.now_playing.clone();
            while !song.is_empty() && x + o.measure(&song, &font, 0.0) > room {
                let shorter: String = song.chars().take(song.chars().count().saturating_sub(4)).collect();
                song = if shorter.trim().is_empty() {
                    String::new()
                } else {
                    format!("{}\u{2026}", shorter.trim_end_matches(['\u{2026}', ' ']))
                };
            }
            if song.chars().count() > 3 {
                o.text(&song, x, 20.5, &font, secondary, HAlign::Left, VAlign::Middle);
            }
        }
        let mut facts = self.header.facts();
        let font = fonts::ui(11.0, Weight::Regular);
        // The facts that fit, the device name first.
        while !facts.is_empty() && 34.0 + o.measure(&facts.join("  \u{00B7}  "), &font, 0.0) > room {
            facts.pop();
        }
        if !facts.is_empty() {
            o.text(
                &facts.join("  \u{00B7}  "),
                34.0,
                37.0,
                &font,
                secondary,
                HAlign::Left,
                VAlign::Middle,
            );
        }
    }
}

impl ChromePanel {
    fn severity_color(p: &joseon_render::Palette, severity: StressSeverity) -> Color {
        match severity {
            StressSeverity::Info => p.accent,
            StressSeverity::Watch => p.warn,
            StressSeverity::High => p.danger,
        }
    }

    fn paused_pill(width: f32) -> Rect {
        let first = settings_button(width);
        Rect::new(first.x - 12.0 - 64.0, first.y + 2.0, 64.0, first.h - 4.0)
    }
}

/// Everything the main window shows.
pub struct Scene {
    ctx: Arc<RenderContext>,
    settings: AppSettings,
    width: f32,
    height: f32,
    scale: f32,
    layout: WindowLayout,
    cards: Vec<CardView>,
    chrome: PanelRenderer,
    link: PanelCursorLink,
    banners: Vec<Banner>,
    header: HeaderState,
    frozen: bool,
    placeholder: Option<String>,
    pointer: Option<[f32; 2]>,
    /// The divider the pointer drags.
    dragging: Option<Divider>,
    dragging_strip: bool,
    high_contrast: bool,
    names: CurveNames,
    update: crate::update::Status,
    stress: StressState,
    strip: StripLayout,
    /// The chip whose detail shows: its place in the row.
    open_chip: Option<usize>,
    last_slice: Option<(Option<usize>, u64)>,
    last_tick: f64,
}

fn panel_kind(card: Card) -> PanelKind {
    match card {
        Card::Spectrum => PanelKind::Spectrum,
        Card::Spectrogram => PanelKind::Spectrogram,
        Card::Vectorscope | Card::Placement => PanelKind::Vectorscope,
        Card::Meters => PanelKind::Meters,
        Card::Timeline => PanelKind::Timeline,
    }
}

impl Scene {
    /// `None` when the system gives no GPU device.
    pub fn new(settings: AppSettings, width: f32, height: f32, scale: f32) -> Option<Self> {
        let ctx = RenderContext::shared()?;
        let mut cards = Vec::new();
        for card in Card::ALL {
            let mut renderer = PanelRenderer::make(panel_kind(card), ctx.clone(), Theme::default())?;
            renderer.set_cursor_linked(true);
            cards.push(CardView {
                card,
                renderer,
                target: None,
                group: None,
                rects: None,
                last_host_time: f64::NAN,
                undrawn: false,
            });
        }
        let chrome = PanelRenderer::with_panel(
            ctx.clone(),
            Theme::default(),
            Box::new(ChromePanel {
                cards: Vec::new(),
                header: HeaderState::default(),
                banners: Vec::new(),
                preset: settings.layout_preset,
                frozen: false,
                text_version: 0,
                width,
                strip: StripLayout::default(),
            }),
        );
        let mut scene = Self {
            ctx,
            settings,
            width,
            height,
            scale,
            layout: WindowLayout::default(),
            cards,
            chrome,
            link: PanelCursorLink::new(),
            banners: Vec::new(),
            header: HeaderState::default(),
            frozen: false,
            placeholder: None,
            pointer: None,
            dragging: None,
            dragging_strip: false,
            high_contrast: false,
            names: CurveNames::default(),
            update: crate::update::Status::default(),
            stress: StressState::default(),
            strip: StripLayout::default(),
            open_chip: None,
            last_slice: None,
            last_tick: 0.0,
        };
        scene.apply_settings();
        scene.relayout();
        Some(scene)
    }

    pub fn settings(&self) -> &AppSettings {
        &self.settings
    }

    pub fn layout(&self) -> &WindowLayout {
        &self.layout
    }

    pub fn cursor(&self) -> Option<PanelCursor> {
        self.link.cursor()
    }

    pub fn size(&self) -> (f32, f32, f32) {
        (self.width, self.height, self.scale)
    }

    /// The size of the picture in pixels.
    pub fn pixel_size(&self) -> (u32, u32) {
        (
            ((self.width * self.scale).round() as u32).max(1),
            ((self.height * self.scale).round() as u32).max(1),
        )
    }

    pub fn resize(&mut self, width: f32, height: f32, scale: f32) {
        if (width, height, scale) != (self.width, self.height, self.scale) {
            self.width = width.max(1.0);
            self.height = height.max(1.0);
            self.scale = scale.max(0.5);
            self.relayout();
        }
    }

    pub fn set_settings(&mut self, settings: AppSettings) {
        if settings != self.settings {
            self.settings = settings;
            self.apply_settings();
            self.relayout();
        }
    }

    pub fn set_high_contrast(&mut self, on: bool) {
        self.high_contrast = on;
        self.chrome.set_high_contrast(on);
        for card in &mut self.cards {
            card.renderer.set_high_contrast(on);
        }
    }

    /// What the model says now: the header text, the banners, the frozen state, the no-audio text.
    pub fn set_status(&mut self, header: HeaderState, banners: Vec<Banner>, frozen: bool, placeholder: Option<&str>) {
        let banner_count = self.banners.len();
        self.header = header;
        self.banners = banners;
        self.frozen = frozen;
        self.placeholder = placeholder.map(str::to_string);
        // A banner takes height from the cards.
        if self.banners.len() != banner_count {
            self.relayout();
        }
    }

    /// Where the updater stands: the settings menu shows it.
    pub fn set_update(&mut self, status: crate::update::Status) {
        self.update = status;
    }

    /// The headphones and targets the menus offer.
    pub fn set_curve_names(&mut self, names: CurveNames) {
        self.names = names;
    }

    /// What the strip under the header shows now.
    pub fn set_stress(&mut self, stress: StressState) {
        if stress.flags.len() != self.stress.flags.len() || stress.headphone != self.stress.headphone {
            self.open_chip = None;
        }
        if stress != self.stress {
            self.stress = stress;
            self.update_chrome();
        }
    }

    pub fn curve_names(&self) -> &CurveNames {
        &self.names
    }

    pub fn strip_layout(&self) -> &StripLayout {
        &self.strip
    }

    fn card_mut(&mut self, card: Card) -> &mut CardView {
        self.cards
            .iter_mut()
            .find(|c| c.card == card)
            .expect("every card exists")
    }

    /// Settings to the panels.
    fn apply_settings(&mut self) {
        let s = self.settings.clone();
        let target = s.loudness_target().lufs();
        if let Some((panel, base)) = self.card_mut(Card::Spectrum).renderer.panel_as::<SpectrumPanel>() {
            panel.set_options(base, s.spectrum_view_options());
            panel.set_auto_range(base, s.spectrum_auto_range);
        }
        if let Some((panel, base)) = self.card_mut(Card::Spectrogram).renderer.panel_as::<SpectrogramPanel>() {
            panel.set_history_seconds(base, s.spectrogram_history_seconds as f64);
        }
        let mode = if s.stereo_placement_mode {
            VectorscopeMode::PanSpectrum
        } else {
            VectorscopeMode::Lissajous
        };
        if let Some((panel, base)) = self.card_mut(Card::Vectorscope).renderer.panel_as::<VectorscopePanel>() {
            panel.set_mode(base, mode);
        }
        if let Some((panel, base)) = self.card_mut(Card::Placement).renderer.panel_as::<VectorscopePanel>() {
            panel.set_mode(base, VectorscopeMode::PanSpectrum);
        }
        if let Some((panel, base)) = self.card_mut(Card::Meters).renderer.panel_as::<MetersPanel>() {
            panel.set_target_lufs(base, target);
        }
        if let Some((panel, base)) = self.card_mut(Card::Timeline).renderer.panel_as::<TimelinePanel>() {
            panel.set_target_lufs(base, target);
            panel.set_window_seconds(base, s.timeline_window_seconds);
            panel.set_show_bands(base, s.timeline_show_bands);
            panel.set_show_level_at_ear(base, s.timeline_show_ear_lane);
        }
    }

    /// The offset of the labels of the timeline from UTC, in seconds (the local time zone).
    pub fn set_utc_offset_seconds(&mut self, offset: i32) {
        if let Some((panel, base)) = self.card_mut(Card::Timeline).renderer.panel_as::<TimelinePanel>() {
            panel.set_utc_offset_seconds(base, offset);
        }
    }

    fn relayout(&mut self) {
        self.layout = layout::window_layout(self.width, self.height, self.banners.len(), &self.settings);
        let (scale, ctx) = (self.scale, self.ctx.clone());
        let layout = self.layout.clone();
        for view in &mut self.cards {
            let Some(rect) = layout.card(view.card) else {
                view.rects = None;
                view.renderer.set_hover(None);
                continue;
            };
            let panel = WindowLayout::panel_rect(rect);
            view.rects = Some((rect, panel));
            view.renderer.set_layout(panel.w, panel.h, scale);
            let (pw, ph) = (
                ((panel.w * scale).round() as u32).max(1),
                ((panel.h * scale).round() as u32).max(1),
            );
            if !view.target.as_ref().is_some_and(|t| (t.width, t.height) == (pw, ph)) {
                let target = OffscreenTarget::new(&ctx, pw, ph);
                view.group = Some(ctx.device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("card picture"),
                    layout: &ctx.lut_layout,
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 0,
                            resource: wgpu::BindingResource::TextureView(&target.view),
                        },
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: wgpu::BindingResource::Sampler(&ctx.linear_sampler),
                        },
                    ],
                }));
                view.target = Some(target);
            }
        }
        self.chrome.set_layout(self.width, self.height, self.scale);
    }

    fn title(&self, card: Card) -> &'static str {
        match card {
            Card::Spectrum => "Spectrum",
            Card::Spectrogram => "Spectrogram",
            Card::Vectorscope if self.settings.stereo_placement_mode => PLACEMENT_TITLE,
            Card::Vectorscope => SCOPE_TITLE,
            Card::Meters => "Loudness and peak",
            Card::Placement => PLACEMENT_TITLE,
            Card::Timeline => "Session timeline",
        }
    }

    /// "Waiting for audio" shows once in the header. In the body, only the largest card of the layout repeats
    /// it, as one quiet plate.
    fn placeholder_card(&self) -> Card {
        if self.settings.layout_preset == LayoutPreset::Metering {
            Card::Meters
        } else {
            Card::Spectrum
        }
    }

    // MARK: Frames

    /// One display tick: the newest frame and the session record go to the panels, the text is brought up to date.
    /// `now` is a clock in seconds that only goes forward.
    pub fn tick(&mut self, now: f64, frame: &Arc<AnalysisFrame>, session: Option<&SessionSnapshot>) {
        let dt = if self.last_tick > 0.0 {
            (now - self.last_tick).max(0.0).min(0.25)
        } else {
            1.0 / 60.0
        };
        self.last_tick = now;
        let cursor = self.link.cursor();
        let slice = self.link.history_slice().cloned();
        for view in &mut self.cards {
            if view.rects.is_none() {
                continue;
            }
            if view.renderer.panel.uses_analysis_frames() {
                if frame.host_time != view.last_host_time || view.renderer.base.frame.is_none() {
                    if view.undrawn && view.renderer.panel.needs_pre_pass_per_frame() {
                        let mut encoder = self.ctx.device.create_command_encoder(&Default::default());
                        view.renderer.encode(&mut encoder, None);
                        self.ctx.queue.submit([encoder.finish()]);
                    }
                    view.undrawn = true;
                    view.last_host_time = frame.host_time;
                    view.renderer.ingest(frame.clone());
                } else {
                    let renderer = &mut view.renderer;
                    renderer.panel.idle(&mut renderer.base, dt);
                }
            }
            view.renderer.set_cursor(cursor);
            view.renderer.set_cursor_slice(slice.clone());
        }
        if let Some(session) = session {
            if let Some((panel, base)) = self.card_mut(Card::Timeline).renderer.panel_as::<TimelinePanel>() {
                panel.set_snapshot(base, session.clone());
                panel.fallback_frequency_hz = if frame.peak.frequency_hz > 0.0 && !fmt::is_floor(frame.peak.level_db) {
                    frame.peak.frequency_hz
                } else {
                    1000.0
                };
            }
        }
        self.publish_history_slice();
        for view in &mut self.cards {
            if view.rects.is_some() {
                view.renderer.refresh_text(now, false);
            }
        }
        self.update_chrome();
        self.chrome.refresh_text(now, false);
    }

    /// The spectrogram gives the past spectrum under a timed cursor to the other panels.
    fn publish_history_slice(&mut self) {
        let ago = self.link.cursor().and_then(|c| c.seconds_ago);
        let last = self.last_slice;
        let view = self.card_mut(Card::Spectrogram);
        let visible = view.rects.is_some();
        let Some((panel, _)) = view.renderer.panel_as::<SpectrogramPanel>() else {
            return;
        };
        let Some(ago) = ago.filter(|_| visible) else {
            if self.last_slice.take().is_some() {
                self.link.publish(None);
            }
            return;
        };
        let key = (panel.history_column_id(ago), ago.to_bits());
        if last == Some(key) {
            return;
        }
        let slice = panel.history_column(ago).map(|(_, slice)| slice);
        self.last_slice = Some(key);
        self.link.publish(slice);
    }

    fn update_chrome(&mut self) {
        let placeholder_card = self.placeholder_card();
        let mut cards = Vec::new();
        for view in &self.cards {
            let Some((rect, panel)) = view.rects else { continue };
            let text = self.placeholder.clone().filter(|_| view.card == placeholder_card);
            // The meters and the stereo card keep scales and readouts on screen at silence: the text goes beside
            // the title.
            let beside_title = matches!(view.card, Card::Meters | Card::Vectorscope | Card::Placement);
            cards.push(ChromeCard {
                rect,
                panel,
                title: self.title(view.card).to_string(),
                state: text.clone().filter(|_| beside_title),
                plate: text.filter(|_| !beside_title),
                group: view.group.clone(),
                dimmed: self.frozen,
            });
        }
        let banners: Vec<(Rect, Banner)> = self
            .layout
            .banners
            .iter()
            .copied()
            .zip(self.banners.iter().cloned())
            .collect();
        let mut h = Signature::new();
        for card in &cards {
            h.add(&card.title).add(&card.state).add(&card.plate);
            for v in [card.rect.x, card.rect.y, card.rect.w, card.rect.h] {
                h.add_f32(v);
            }
        }
        let measure = OverlayContext::new(self.width, self.height, self.scale);
        self.strip = stress_strip::layout(&measure, self.layout.flag_strip, &self.stress, self.open_chip);
        h.add(format!("{:?}", self.strip));
        h.add(&self.header).add(self.frozen).add(self.settings.layout_preset);
        for (_, banner) in &banners {
            h.add(banner);
        }
        let version = h.finish();
        let (width, header, frozen, preset) = (
            self.width,
            self.header.clone(),
            self.frozen,
            self.settings.layout_preset,
        );
        let self_strip = self.strip.clone();
        if let Some((chrome, base)) = self.chrome.panel_as::<ChromePanel>() {
            if chrome.text_version != version {
                base.needs_display = true;
            }
            chrome.cards = cards;
            chrome.header = header;
            chrome.banners = banners;
            chrome.preset = preset;
            chrome.frozen = frozen;
            chrome.width = width;
            chrome.strip = self_strip;
            chrome.text_version = version;
        }
    }

    /// Draws the window into `target` (as large as `pixel_size`).
    pub fn render(&mut self, encoder: &mut wgpu::CommandEncoder, target: &wgpu::TextureView) {
        for view in &mut self.cards {
            if let (Some(_), Some(picture)) = (view.rects, &view.target) {
                view.renderer.encode(encoder, Some(&picture.view));
                view.undrawn = false;
            }
        }
        self.chrome.encode(encoder, Some(target));
    }

    /// The window as RGBA pixels, row by row from the top. For the snapshot mode and the tests.
    pub fn snapshot_rgba(&mut self, now: f64) -> Option<(u32, u32, Vec<u8>)> {
        let (w, h) = self.pixel_size();
        let target = OffscreenTarget::new(&self.ctx, w, h);
        for view in &mut self.cards {
            if view.rects.is_some() {
                view.renderer.refresh_text(now, true);
            }
        }
        self.update_chrome();
        self.chrome.refresh_text(now, true);
        let mut encoder = self.ctx.device.create_command_encoder(&Default::default());
        self.render(&mut encoder, &target.view);
        self.ctx.queue.submit([encoder.finish()]);
        Some((w, h, target.read_rgba(&self.ctx)?))
    }

    // MARK: Pointer

    fn divider_at(&self, x: f32, y: f32) -> Option<&Divider> {
        self.layout.dividers.iter().find(|d| d.rect.contains_point(x, y))
    }

    fn panel_at(&self, x: f32, y: f32) -> Option<(usize, [f32; 2])> {
        self.cards.iter().enumerate().find_map(|(i, view)| {
            let (_, panel) = view.rects?;
            panel.contains_point(x, y).then_some((i, [x - panel.x, y - panel.y]))
        })
    }

    /// The pointer is at `position` (points, from the top left of the window), or left the window.
    pub fn pointer_moved(&mut self, position: Option<[f32; 2]>) -> Action {
        self.pointer = position;
        if let (Some(divider), Some([x, y])) = (&self.dragging, position) {
            let shares = layout::drag_divider(divider, if divider.side_by_side { x } else { y });
            self.settings.splits.insert(divider.key.clone(), shares);
            self.relayout();
            return Action::None;
        }
        if let (true, Some([_, y])) = (self.dragging_strip, position) {
            let wanted = (self.layout.body.max_y() - y - layout::GAP / 2.0)
                .max(layout::STRIP_HEIGHT_RANGE.0)
                .min(layout::STRIP_HEIGHT_RANGE.1);
            self.settings.timeline_strip_height = wanted.round() as f64;
            self.relayout();
            return Action::None;
        }
        let over = position.and_then(|[x, y]| self.panel_at(x, y));
        for (i, view) in self.cards.iter_mut().enumerate() {
            let hover = over.filter(|(index, _)| *index == i).map(|(_, point)| point);
            view.renderer.set_hover(hover);
            if !view.renderer.panel.makes_pointer_cursors() {
                continue;
            }
            // The local hover is "this panel is the cursor source". The frequency (and the time) come from the
            // panel's own axes; the link ignores the move while a cursor is pinned.
            match hover.and_then(|point| view.renderer.panel.cursor_at(&view.renderer.base, point)) {
                Some(c) => self.link.set(c),
                None => self.link.hover_ended(view.renderer.panel.kind()),
            }
        }
        Action::None
    }

    /// The shape the pointer has where it is.
    pub fn pointer_shape(&self) -> PointerShape {
        if let Some(d) = &self.dragging {
            return if d.side_by_side {
                PointerShape::ResizeColumns
            } else {
                PointerShape::ResizeRows
            };
        }
        let Some([x, y]) = self.pointer else {
            return PointerShape::Arrow;
        };
        if self.dragging_strip || self.layout.strip_divider.is_some_and(|r| r.contains_point(x, y)) {
            return PointerShape::ResizeRows;
        }
        if let Some(d) = self.divider_at(x, y) {
            return if d.side_by_side {
                PointerShape::ResizeColumns
            } else {
                PointerShape::ResizeRows
            };
        }
        if preset_buttons(self.width).iter().any(|(_, r)| r.contains_point(x, y))
            || self.menu_button_at(x, y).is_some()
            || stress_strip::hit(&self.strip, x, y).is_some()
        {
            return PointerShape::Hand;
        }
        match self.panel_at(x, y) {
            Some((i, point)) => {
                let renderer = &self.cards[i].renderer;
                if renderer.panel.makes_pointer_cursors() && renderer.panel.cursor_at(&renderer.base, point).is_some() {
                    PointerShape::Crosshair
                } else {
                    PointerShape::Arrow
                }
            }
            None => PointerShape::Arrow,
        }
    }

    /// The main button went down at the pointer.
    pub fn pointer_pressed(&mut self) -> Action {
        let Some([x, y]) = self.pointer else {
            return Action::None;
        };
        if let Some((preset, _)) = preset_buttons(self.width)
            .into_iter()
            .find(|(_, r)| r.contains_point(x, y))
        {
            return self.key(Key::Preset(preset));
        }
        if let Some(target) = self.menu_button_at(x, y) {
            return Action::OpenMenu(target);
        }
        // The strip: a chip shows its detail (a second click hides it); the rest opens the headphone menu.
        let open = self.open_chip.take();
        match stress_strip::hit(&self.strip, x, y) {
            Some(stress_strip::Hit::Chip(i)) => {
                self.open_chip = (open != Some(i)).then_some(i);
                self.update_chrome();
                return Action::None;
            }
            Some(stress_strip::Hit::Picker) => return Action::OpenMenu(MenuTarget::Headphone),
            // A click anywhere else only closes an open detail.
            None if open.is_some() => {
                self.update_chrome();
                return Action::None;
            }
            None => {}
        }
        if self.layout.strip_divider.is_some_and(|r| r.contains_point(x, y)) {
            self.dragging_strip = true;
            return Action::None;
        }
        if let Some(divider) = self.divider_at(x, y).cloned() {
            self.dragging = Some(divider);
            return Action::None;
        }
        let Some((i, point)) = self.panel_at(x, y) else {
            return Action::None;
        };
        let view = &mut self.cards[i];
        // The clip lamp of the meters resets the measurement.
        if let Some((meters, _)) = view.renderer.panel_as::<MetersPanel>() {
            if meters.clip_indicator_rect().contains_point(point[0], point[1]) {
                return Action::ResetMeasurement;
            }
        }
        // A click over a plot with a frequency axis pins the cursor there. A click on a pinned cursor clears it
        // (the hover cursor of the pointer takes over at once).
        if !view.renderer.panel.makes_pointer_cursors() {
            return Action::None;
        }
        let Some(mut c) = view.renderer.panel.cursor_at(&view.renderer.base, point) else {
            return Action::None;
        };
        let on_pinned = self.link.cursor().is_some_and(|c| c.is_pinned)
            && view.renderer.panel.is_on_cursor(&view.renderer.base, point);
        if on_pinned {
            self.link.clear();
        } else {
            c.is_pinned = true;
        }
        self.link.set(c);
        Action::None
    }

    fn menu_button_at(&self, x: f32, y: f32) -> Option<MenuTarget> {
        if settings_button(self.width).contains_point(x, y) {
            return Some(MenuTarget::Settings);
        }
        self.layout
            .cards
            .iter()
            .find(|(_, rect)| options_button(*rect).contains_point(x, y))
            .map(|(card, _)| MenuTarget::Card(*card))
    }

    /// The other button went down at the pointer: the menu of the card under it, or the settings menu.
    pub fn pointer_context(&mut self) -> Action {
        let Some([x, y]) = self.pointer else {
            return Action::None;
        };
        match self.layout.card_at(x, y) {
            Some((card, _)) => Action::OpenMenu(MenuTarget::Card(card)),
            None if y < self.layout.body.y => Action::OpenMenu(MenuTarget::Settings),
            None => Action::None,
        }
    }

    /// What a menu holds now.
    pub fn menu(&self, target: MenuTarget) -> Vec<Entry> {
        match target {
            MenuTarget::Card(card) => menu::card_menu(card, &self.settings, self.frozen),
            MenuTarget::Settings => {
                let mut entries = menu::settings_menu(&self.settings, self.frozen, &self.names);
                entries.extend(menu::update_entries(&self.settings, &self.update));
                entries
            }
            MenuTarget::Headphone => menu::headphone_menu(&self.settings, &self.names),
        }
    }

    /// Does what a chosen menu item says.
    pub fn run(&mut self, command: Command) -> Action {
        let before = self.settings.clone();
        let effect = menu::apply(&mut self.settings, command, &self.names);
        if self.settings != before {
            self.apply_settings();
            self.relayout();
        }
        match effect {
            Effect::Settings if self.settings == before => Action::None,
            Effect::Settings => Action::SettingsChanged,
            Effect::SettingsAndSource => Action::RestartSource,
            Effect::ToggleFreeze => Action::ToggleFreeze,
            Effect::ResetMeasurement => Action::ResetMeasurement,
            Effect::ClearTimeline => Action::ClearTimeline,
            Effect::ImportCurve => Action::ImportCurve,
            Effect::OpenCurvesFolder => Action::OpenCurvesFolder,
            Effect::CheckUpdates => Action::CheckUpdates,
            Effect::InstallUpdate => Action::InstallUpdate,
            Effect::OpenWindow => Action::OpenWindow,
            Effect::Quit => Action::Quit,
        }
    }

    /// The main button went up. A drag of a divider ends: the new shares are settings.
    pub fn pointer_released(&mut self) -> Action {
        let dragged = self.dragging.take().is_some() | std::mem::take(&mut self.dragging_strip);
        if dragged {
            Action::SettingsChanged
        } else {
            Action::None
        }
    }

    // MARK: Keys

    pub fn key(&mut self, key: Key) -> Action {
        match key {
            Key::Freeze => Action::ToggleFreeze,
            Key::Reset => Action::ResetMeasurement,
            Key::FullScreen => Action::ToggleFullScreen,
            Key::Escape => {
                // Esc clears the linked cursor from anywhere in the window.
                if self.link.cursor().is_some() {
                    self.link.clear();
                }
                Action::None
            }
            Key::Preset(preset) => {
                if self.settings.layout_preset == preset {
                    return Action::None;
                }
                self.settings.layout_preset = preset;
                self.relayout();
                Action::SettingsChanged
            }
            Key::TogglePlacement => {
                if self.settings.layout_preset == LayoutPreset::Spectrum {
                    self.settings.spectrum_shows_placement = !self.settings.spectrum_shows_placement;
                } else {
                    self.settings.stereo_placement_mode = !self.settings.stereo_placement_mode;
                }
                self.apply_settings();
                self.relayout();
                Action::SettingsChanged
            }
            Key::ToggleTimeline => {
                self.settings.show_timeline = !self.settings.show_timeline;
                self.relayout();
                Action::SettingsChanged
            }
            Key::CursorDown { octave } | Key::CursorUp { octave } => {
                let up = matches!(key, Key::CursorUp { .. });
                let semitones = if octave { 12.0 } else { 1.0 } * if up { 1.0 } else { -1.0 };
                let c = match self.link.cursor() {
                    Some(mut c) => {
                        c.frequency_hz = cursor::step(c.frequency_hz, semitones, CURSOR_RANGE_HZ.0, CURSOR_RANGE_HZ.1);
                        c
                    }
                    // With no cursor the first arrow key puts one at the frame's peak frequency.
                    None => {
                        let frame = self.card_mut(Card::Spectrum).renderer.base.frame.clone();
                        let peak = frame
                            .filter(|f| !fmt::is_floor(f.peak.level_db) && f.peak.frequency_hz > 0.0)
                            .map_or(1000.0, |f| f.peak.frequency_hz);
                        PanelCursor {
                            is_pinned: true,
                            ..PanelCursor::new(peak.max(CURSOR_RANGE_HZ.0).min(CURSOR_RANGE_HZ.1), PanelKind::Spectrum)
                        }
                    }
                };
                // The link replaces a pinned cursor only by a pinned one.
                if c.is_pinned {
                    self.link.set(c);
                } else {
                    self.link.clear();
                    self.link.set(PanelCursor { is_pinned: true, ..c });
                }
                Action::None
            }
            Key::Pin => {
                if let Some(mut c) = self.link.cursor() {
                    let pinned = c.is_pinned;
                    self.link.clear();
                    c.is_pinned = !pinned;
                    self.link.set(c);
                }
                Action::None
            }
        }
    }
}
