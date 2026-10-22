//! The vectorscope panel, drawn offscreen.

use super::*;
use crate::contracts::{PanelCursor, PanelKind, VectorscopeMode};
use crate::offscreen::{Session, Settings};
use crate::palette::Theme;
use crate::panel::Panel;
use crate::synthetic::{Options, SyntheticFrames};
use crate::text::fonts;
use crate::vectorscope::{no_signal, pan_legend_steps, Arrangement, VectorscopePanel, MAX_GAIN};
use joseon_core::contracts::AnalysisFrame;
use std::sync::Arc;

/// Synthetic frames with two one-sided tones: one in the right channel that breathes, one low in the left channel.
fn toned_frames(count: usize) -> Vec<AnalysisFrame> {
    let mut frames = SyntheticFrames::sequence(count, Options::default());
    for (k, frame) in frames.iter_mut().enumerate() {
        let n = frame.spectrum.left.len();
        let (hi, lo) = (n * 7 / 10, n / 5);
        for i in 0..n {
            let a = i as f32 - hi as f32;
            let b = i as f32 - lo as f32;
            frame.spectrum.right[i] += 34.0 * (-a * a / 6.0).exp() * if k % 90 < 60 { 1.0 } else { 0.2 };
            frame.spectrum.left[i] += 28.0 * (-b * b / 10.0).exp();
        }
    }
    frames
}

fn session(name: &str, size: (f32, f32), settings: &Settings) -> (Session, Vec<u8>) {
    let mut session = Session::new(PanelKind::Vectorscope, size.0, size.1, 2.0, Theme::default(), settings).unwrap();
    session.feed(&toned_frames(200));
    let rgba = session.snapshot_rgba().unwrap();
    std::fs::write(
        review_path(name),
        crate::target::encode_png(session.target.width, session.target.height, &rgba),
    )
    .unwrap();
    (session, rgba)
}

fn pinned(hz: f32) -> Option<PanelCursor> {
    Some(PanelCursor {
        is_pinned: true,
        ..PanelCursor::new(hz, PanelKind::Spectrum)
    })
}

fn light(p: [u8; 4]) -> u32 {
    p[0] as u32 + p[1] as u32 + p[2] as u32
}

#[test]
fn the_goniometer_draws_its_trace_inside_the_diamond() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let (mut session, rgba) = session("scope-wide.png", (900.0, 360.0), &Settings::default());
    let width = session.target.width;
    let (panel, base) = session.renderer.panel_as::<VectorscopePanel>().unwrap();
    assert_eq!(panel.arrangement(), Arrangement::TableBeside);
    let field = panel.field();
    assert_eq!((field.x, field.y, field.w, field.h), (12.0, 12.0, 336.0, 336.0));
    let at = |x: f32, y: f32| pixel(&rgba, width, (x * 2.0) as u32, (y * 2.0) as u32);
    // The brightest pixel of the field is far over the plot background, and it is inside the full-scale diamond.
    let (mut best, mut best_at) = (0, (0.0, 0.0));
    let mut y = field.min_y() + 2.0;
    while y < field.max_y() - 2.0 {
        let mut x = field.min_x() + 2.0;
        while x < field.max_x() - 2.0 {
            if light(at(x, y)) > best {
                best = light(at(x, y));
                best_at = (x, y);
            }
            x += 1.0;
        }
        y += 1.0;
    }
    assert!(best > 300, "the trace has light: {best}");
    let reach = (best_at.0 - field.mid_x()).abs() + (best_at.1 - field.mid_y()).abs();
    assert!(
        reach < field.w / 2.0 * 0.86,
        "the core is inside the diamond: {best_at:?}"
    );
    // A corner of the field is outside the diamond: no light there.
    assert!(light(at(field.min_x() + 14.0, field.max_y() - 14.0)) < 90);
    assert!(panel.is_animating());
    assert_eq!(panel.accessibility_label(), "Stereo vectorscope");
    assert!(panel.accessibility_value(base).starts_with("Correlation +0."));
    assert!(
        panel.cursor_at(base, [100.0, 100.0]).is_none(),
        "the goniometer has no frequency axis"
    );
    assert!(!panel.makes_pointer_cursors());
}

#[test]
fn every_arrangement_draws() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let cursor = Settings {
        cursor: pinned(1_000.0),
        ..Settings::default()
    };
    for (name, size, settings, arrangement) in [
        (
            "scope-tall.png",
            (360.0, 520.0),
            Settings::default(),
            Arrangement::StripUnder,
        ),
        ("scope-keys.png", (420.0, 300.0), cursor, Arrangement::KeysBeside),
        (
            "scope-small.png",
            (260.0, 240.0),
            Settings::default(),
            Arrangement::KeysUnder,
        ),
    ] {
        let (mut session, _) = session(name, size, &settings);
        let (panel, _) = session.renderer.panel_as::<VectorscopePanel>().unwrap();
        assert_eq!(panel.arrangement(), arrangement, "{name}");
        let field = panel.field();
        assert!(
            field.w == field.h && field.max_x() <= size.0 && field.max_y() <= size.1,
            "{name}: {field:?}"
        );
    }
}

#[test]
fn the_pan_spectrum_places_a_one_sided_tone_at_its_side() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let settings = Settings {
        vectorscope_mode: VectorscopeMode::PanSpectrum,
        cursor: pinned(440.0),
        ..Settings::default()
    };
    let (mut session, rgba) = session("pan-wide.png", (900.0, 420.0), &settings);
    let width = session.target.width;
    let frames = toned_frames(200);
    let (panel, base) = session.renderer.panel_as::<VectorscopePanel>().unwrap();
    assert_eq!(panel.mode(), VectorscopeMode::PanSpectrum);
    // The linked cursor keeps a header row over the field.
    assert_eq!(panel.field().y, 28.0);
    let freqs = &frames[0].spectrum.frequencies;
    let (right_tone, left_tone) = (freqs[freqs.len() * 7 / 10], freqs[freqs.len() / 5]);
    let at = |x: f32, y: f32| pixel(&rgba, width, (x * 2.0) as u32, (y * 2.0) as u32);
    let (cx, half) = (panel.pan_center_x(), panel.pan_half());
    let row = |hz: f32, x: f32| light(at(x, panel.pan_y(hz)));
    assert!(
        row(right_tone, cx + half * 0.97) > row(right_tone, cx - half * 0.5) + 150,
        "a tone in the right channel is a dot at the right: {} vs {}",
        row(right_tone, cx + half * 0.97),
        row(right_tone, cx - half * 0.5)
    );
    assert!(
        row(left_tone, cx - half * 0.97) > row(left_tone, cx + half * 0.5) + 100,
        "a tone in the left channel is a dot at the left: {} vs {}",
        row(left_tone, cx - half * 0.97),
        row(left_tone, cx + half * 0.5)
    );
    // The pointer makes cursors on the frequency axis only.
    assert!(panel.makes_pointer_cursors());
    let axis = panel.pan_axis_rect();
    let c = panel.cursor_at(base, [200.0, panel.pan_y(1_000.0)]).unwrap();
    assert!((c.frequency_hz - 1_000.0).abs() < 2.0 && c.source == PanelKind::Vectorscope);
    assert!(panel.cursor_at(base, [200.0, axis.min_y() - 3.0]).is_none());
    assert!(panel.is_on_cursor(base, [200.0, panel.pan_y(440.0) + 3.0]));
    assert!(!panel.is_on_cursor(base, [200.0, panel.pan_y(440.0) + 9.0]));
    assert_eq!(panel.accessibility_label(), "Stereo placement by frequency");

    let plain = Settings {
        vectorscope_mode: VectorscopeMode::PanSpectrum,
        hover: Some([150.0, 200.0]),
        ..Settings::default()
    };
    let (mut session, _) = super::vectorscope::session("pan-tall.png", (380.0, 560.0), &plain);
    let (panel, base) = session.renderer.panel_as::<VectorscopePanel>().unwrap();
    assert_eq!(panel.arrangement(), Arrangement::StripUnder);
    let (lines, anchor) = panel.hover_label(base).unwrap();
    assert!(
        lines[0].starts_with("cursor  ") && lines[1].starts_with("cursor  pan "),
        "{lines:?}"
    );
    assert_eq!(anchor, [150.0, 200.0]);
}

#[test]
fn the_gain_goes_up_slowly_and_down_at_once() {
    let Some(ctx) = gpu() else { return };
    let mut renderer = crate::panel::PanelRenderer::make(PanelKind::Vectorscope, ctx, Theme::default()).unwrap();
    renderer.set_layout(400.0, 400.0, 2.0);
    let frames = SyntheticFrames::sequence(120, Options::default());
    let scaled = |f: &AnalysisFrame, k: f32| {
        let mut f = f.clone();
        for p in &mut f.stereo.scope_points {
            *p = [p[0] * k, p[1] * k];
        }
        f
    };
    let gain = |r: &mut crate::panel::PanelRenderer| r.panel_as::<VectorscopePanel>().unwrap().0.gain();
    // Quiet music: the gain goes up at once (first reading), to at most the limit.
    for f in &frames[..30] {
        renderer.ingest(Arc::new(scaled(f, 0.05)));
    }
    let quiet = gain(&mut renderer);
    assert!(quiet > 4.0 && quiet <= MAX_GAIN, "quiet music is zoomed in: {quiet}");
    // A loud frame: down is instant.
    renderer.ingest(Arc::new(scaled(&frames[30], 1.0)));
    let loud = gain(&mut renderer);
    assert!(loud < 1.2, "a loud frame takes the gain down at once: {loud}");
    // Back to quiet: the level is held, the gain does not jump back.
    for f in &frames[31..61] {
        renderer.ingest(Arc::new(scaled(f, 0.05)));
    }
    assert!(gain(&mut renderer) < 1.2, "the level is held for 1.5 s");
    // Fewer than 16 points: no reading, the gain stays.
    let mut few = scaled(&frames[61], 1.0);
    few.stereo.scope_points.truncate(8);
    let held = gain(&mut renderer);
    renderer.ingest(Arc::new(few));
    assert_eq!(gain(&mut renderer), held);
}

#[test]
fn silence_has_no_reading() {
    let silent = AnalysisFrame::new(joseon_core::contracts::SpectrumReading::silent(64));
    assert!(no_signal(&silent));
    let frames = SyntheticFrames::sequence(2, Options::default());
    assert!(!no_signal(&frames[1]));
    assert_eq!(pan_legend_steps(3.0, 2.0), 12);
    assert_eq!(pan_legend_steps(134.64, 2.0), 269);
    assert_eq!(pan_legend_steps(900.0, 2.0), 512);
}

#[test]
fn the_light_fades_when_the_frames_stop() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let mut session = Session::new(
        PanelKind::Vectorscope,
        400.0,
        400.0,
        2.0,
        Theme::default(),
        &Settings::default(),
    )
    .unwrap();
    session.feed(&toned_frames(120));
    let before = session.snapshot_rgba().unwrap();
    // Five seconds with no frames: more than ten time constants of the persistence.
    {
        let renderer = &mut session.renderer;
        renderer.panel.idle(&mut renderer.base, 5.0);
        assert!(!renderer.panel.is_animating());
    }
    let after = session.snapshot_rgba().unwrap();
    let width = session.target.width;
    let (panel, _) = session.renderer.panel_as::<VectorscopePanel>().unwrap();
    let field = panel.field();
    let sum = |rgba: &[u8]| {
        let mut total = 0u64;
        let mut y = field.min_y() + 30.0;
        while y < field.max_y() - 30.0 {
            let mut x = field.min_x() + 30.0;
            while x < field.max_x() - 30.0 {
                total += light(pixel(rgba, width, (x * 2.0) as u32, (y * 2.0) as u32)) as u64;
                x += 2.0;
            }
            y += 2.0;
        }
        total
    };
    assert!(
        sum(&after) * 10 < sum(&before) * 9,
        "the trace is gone: {} vs {}",
        sum(&after),
        sum(&before)
    );
}

/// Port of the macOS 0.1.2 pacing fix: the window gives the display time with each frame, so the fades move by the
/// time between pictures. Offscreen pictures keep the frame time.
#[test]
fn a_live_frame_carries_the_display_time() {
    let Some(ctx) = gpu() else { return };
    let mut renderer = crate::panel::PanelRenderer::make(PanelKind::Vectorscope, ctx, Theme::default()).unwrap();
    renderer.set_layout(400.0, 400.0, 2.0);
    let frames = SyntheticFrames::sequence(3, Options::default());
    renderer.ingest(Arc::new(frames[0].clone()));
    renderer.ingest(Arc::new(frames[1].clone()));
    let frame_time = renderer.base.frame_delta;
    assert_eq!(renderer.base.animation_delta, frame_time);
    renderer.ingest_animated(Arc::new(frames[2].clone()), Some(1.0 / 144.0));
    assert_eq!(renderer.base.frame_delta, frame_time);
    assert_eq!(renderer.base.animation_delta, 1.0 / 144.0);
}
