//! The spectrogram as a 3-D field (the macOS repo: `docs/specs/waterfall.md`, `WaterfallTests.swift`).

use super::*;
use crate::contracts::{PanelCursor, PanelKind, SpectrogramMode};
use crate::offscreen::{self, Session, Settings};
use crate::palette::Theme;
use crate::panel::Panel;
use crate::spectrogram::SpectrogramPanel;
use crate::synthetic::{Options, SyntheticFrames};
use crate::text::fonts;
use joseon_core::contracts::AnalysisFrame;

fn waterfall() -> Settings {
    Settings {
        spectrogram_mode: SpectrogramMode::Waterfall,
        ..Default::default()
    }
}

fn session(name: &str, width: f32, height: f32, frames: &[AnalysisFrame], settings: &Settings) -> (Session, Vec<u8>) {
    // A big picture at 2x is slow on the software adapter of a build server: the big sizes draw at 1x.
    let scale = if width > 1500.0 { 1.0 } else { 2.0 };
    let mut session = Session::new(PanelKind::Spectrogram, width, height, scale, Theme::default(), settings).unwrap();
    session.feed(frames);
    let rgba = session.snapshot_rgba().unwrap();
    std::fs::write(
        review_path(name),
        crate::target::encode_png(session.target.width, session.target.height, &rgba),
    )
    .unwrap();
    (session, rgba)
}

fn sum(p: [u8; 4]) -> u32 {
    p[0] as u32 + p[1] as u32 + p[2] as u32
}

/// Frames of one steady tone over a quiet floor, one per 60th of a second.
fn tone_frames(hz: f32, level_db: f32, seconds: f64) -> Vec<AnalysisFrame> {
    let base = SyntheticFrames::sequence(1, Options::default()).remove(0);
    (0..(seconds * 60.0) as usize)
        .map(|k| {
            let mut f = base.clone();
            f.host_time = k as f64 / 60.0;
            let mut mid = vec![-95.0f32; f.spectrum.frequencies.len()];
            if let Some(i) = f.spectrum.frequencies.iter().position(|&x| x >= hz) {
                for j in i.saturating_sub(1)..=(i + 1).min(mid.len() - 1) {
                    mid[j] = level_db;
                }
            }
            f.spectrum.left = mid.clone();
            f.spectrum.right = mid.clone();
            f.spectrum.peak_hold = mid.clone();
            f.spectrum.average = mid.clone();
            f.spectrum.mid = mid;
            f.spectrum.mid_layers = Vec::new();
            f
        })
        .collect()
}

#[test]
fn the_field_renders_at_every_size_and_follows_the_frames() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let frames = SyntheticFrames::sequence(240, Options::default());
    for (w, h) in [(560.0, 360.0), (1200.0, 600.0), (1920.0, 1000.0)] {
        let (session, rgba) = session(
            &format!("waterfall-{}x{}.png", w as i32, h as i32),
            w,
            h,
            &frames,
            &waterfall(),
        );
        let width = session.target.width;
        // Not uniform, and many colors: the hue runs across the field.
        let first = pixel(&rgba, width, 0, 0);
        let mut different = 0;
        let mut colors = std::collections::HashSet::new();
        for y in (0..session.target.height).step_by(7) {
            for x in (0..width).step_by(7) {
                let p = pixel(&rgba, width, x, y);
                if p != first {
                    different += 1;
                }
                colors.insert([p[0] >> 2, p[1] >> 2, p[2] >> 2]);
            }
        }
        assert!(different > 500, "{w}x{h} is uniform");
        assert!(colors.len() > 200, "{w}x{h} looks flat: {} colors", colors.len());
    }
    // Same frames twice: the same picture. Other frames: another picture.
    let (_, a) = session("waterfall-a.png", 900.0, 420.0, &frames, &waterfall());
    let (_, again) = session("waterfall-a.png", 900.0, 420.0, &frames, &waterfall());
    assert_eq!(a, again);
    let other = SyntheticFrames::sequence(
        240,
        Options {
            seed: 99,
            level_db: -18.0,
            ..Options::default()
        },
    );
    let (_, b) = session("waterfall-b.png", 900.0, 420.0, &other, &waterfall());
    let diff = a
        .chunks_exact(4)
        .zip(b.chunks_exact(4))
        .filter(|(p, q)| (p[1] as i32 - q[1] as i32).abs() > 8)
        .count();
    assert!(diff > 2000, "{diff} pixels differ");
}

#[test]
fn a_mode_switch_keeps_the_history_and_the_automatic_top() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let frames = SyntheticFrames::sequence(240, Options::default());
    let mut session = Session::new(
        PanelKind::Spectrogram,
        900.0,
        420.0,
        2.0,
        Theme::default(),
        &Settings::default(),
    )
    .unwrap();
    session.feed(&frames);
    let before = session.snapshot_rgba().unwrap();
    let (panel, base) = session.renderer.panel_as::<SpectrogramPanel>().unwrap();
    let top = panel.top_db();
    panel.set_mode(base, SpectrogramMode::Waterfall);
    assert!(panel.is_waterfall());
    assert_eq!(panel.accessibility_label(), "Spectrogram, 3-D waterfall");
    let field = session.snapshot_rgba().unwrap();
    let (panel, base) = session.renderer.panel_as::<SpectrogramPanel>().unwrap();
    assert_eq!(panel.top_db(), top, "the automatic top is shared");
    panel.set_mode(base, SpectrogramMode::Flat);
    let after = session.snapshot_rgba().unwrap();
    assert_eq!(
        before, after,
        "the flat picture must come back unchanged after the field"
    );
    assert_ne!(before, field);
}

/// Ten seconds of one tone: one tall bar at that frequency, in every row, and nothing at 300 Hz.
#[test]
fn one_tone_stands_at_its_frequency() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let frames = tone_frames(1000.0, -20.0, 10.0);
    let (mut session, rgba) = session("waterfall-tone-1k.png", 1200.0, 600.0, &frames, &waterfall());
    let width = session.target.width;
    let (panel, _) = session.renderer.panel_as::<SpectrogramPanel>().unwrap();
    // The automatic top sits on the 6 dB step over the tone (the resampler may add half a dB to the peak).
    assert!(
        panel.top_db() == -18.0 || panel.top_db() == -12.0,
        "top {}",
        panel.top_db()
    );
    let brightness = |hz: f32, ago: f64, height: f32| {
        let p = panel.project_cell(hz, ago, height).unwrap();
        sum(pixel(
            &rgba,
            width,
            ((p[0] * 2.0) as u32).min(width - 1),
            ((p[1] * 2.0) as u32).min(session.target.height - 1),
        ))
    };
    let newest = panel.newest_seconds_ago();
    // On the front face of the newest bar, at 60 % of the scale: lit. In the air over 300 Hz at the same height: background.
    assert!(brightness(1000.0, newest, 0.6) > 240, "the tone's bar");
    assert!(brightness(300.0, newest, 0.6) < 120, "nothing at 300 Hz");
    // Half way into the depth the tone is still there.
    assert!(
        brightness(1000.0, newest + panel.history_seconds() * 0.4, 0.55) > 160,
        "the tone in the depth"
    );
}

/// The pointer over the field names the frequency of the column and the time of the row under it, from any angle.
#[test]
fn the_cursor_names_frequency_and_time_from_any_angle() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let frames = SyntheticFrames::sequence(240, Options::default());
    let mut session = Session::new(
        PanelKind::Spectrogram,
        1200.0,
        600.0,
        2.0,
        Theme::default(),
        &waterfall(),
    )
    .unwrap();
    session.feed(&frames);
    let _ = session.snapshot_rgba().unwrap();
    let (panel, base) = session.renderer.panel_as::<SpectrogramPanel>().unwrap();
    for view in [
        None,
        Some((0.9, 0.5)),
        Some((1.6, 0.5)),
        Some((3.0, 0.5)),
        Some((-2.2, 0.5)),
    ] {
        panel.set_view(base, view, 1.0);
        for (hz, ago) in [(1000.0f32, 3.0f64), (80.0, 1.0), (6000.0, 3.5)] {
            let p = panel.project_cell(hz, ago, 0.4).unwrap();
            let c = panel
                .cursor_at(base, p)
                .unwrap_or_else(|| panic!("{view:?}: no cursor at {hz} Hz, {ago} s"));
            assert_eq!(c.source, PanelKind::Spectrogram);
            assert!(
                (c.frequency_hz / hz).log2().abs() < 0.15,
                "{view:?}: {hz} Hz read as {}",
                c.frequency_hz
            );
            assert!(
                (c.seconds_ago.unwrap() - ago).abs() < 0.7,
                "{view:?}: {ago} s read as {:?}",
                c.seconds_ago
            );
        }
    }
    assert!(panel.cursor_at(base, [2.0, 2.0]).is_none(), "outside the plot");
}

#[test]
fn orbit_zoom_and_reset_restore_the_resting_view() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let frames = SyntheticFrames::sequence(240, Options::default());
    let mut session = Session::new(
        PanelKind::Spectrogram,
        1200.0,
        600.0,
        2.0,
        Theme::default(),
        &waterfall(),
    )
    .unwrap();
    session.feed(&frames);
    let rest = session.snapshot_rgba().unwrap();
    let (panel, base) = session.renderer.panel_as::<SpectrogramPanel>().unwrap();
    assert!(!panel.has_custom_view());
    panel.orbit(base, 2.4, -0.2);
    assert!(panel.has_custom_view());
    let turned = session.snapshot_rgba().unwrap();
    std::fs::write(
        review_path("waterfall-turned.png"),
        crate::target::encode_png(session.target.width, session.target.height, &turned),
    )
    .unwrap();
    assert_ne!(rest, turned);
    let (panel, base) = session.renderer.panel_as::<SpectrogramPanel>().unwrap();
    panel.zoom(base, 1.8);
    let zoomed = session.snapshot_rgba().unwrap();
    assert_ne!(turned, zoomed);
    let (panel, base) = session.renderer.panel_as::<SpectrogramPanel>().unwrap();
    // The elevation never goes under the floor.
    panel.orbit(base, 0.0, -5.0);
    assert!(panel.waterfall.elevation > 0.05);
    panel.reset_view(base);
    assert!(!panel.has_custom_view());
    assert_eq!(
        session.snapshot_rgba().unwrap(),
        rest,
        "the resting picture must come back"
    );
}

/// Every label stays inside the panel and off the others: alone, with cursors, and from turned views.
#[test]
fn labels_never_collide_from_any_view() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let frames = SyntheticFrames::sequence(240, Options::default());
    let pinned = Settings {
        cursor: Some(PanelCursor {
            frequency_hz: 392.0,
            seconds_ago: Some(1.2),
            source: PanelKind::Spectrogram,
            is_pinned: true,
        }),
        ..waterfall()
    };
    let other = Settings {
        cursor: Some(PanelCursor {
            frequency_hz: 3_100.0,
            seconds_ago: None,
            source: PanelKind::Spectrum,
            is_pinned: false,
        }),
        ..waterfall()
    };
    let mut cases = vec![
        ("plain", waterfall()),
        ("pinned", pinned.clone()),
        ("from the spectrum", other),
    ];
    for (yaw, elevation, zoom) in [
        (1.5, 0.5, 1.0),
        (std::f32::consts::PI, 0.6, 1.0),
        (-0.8, 1.4, 1.0),
        (0.4, 0.12, 1.0),
        (0.3, 0.6, 2.4),
    ] {
        cases.push((
            "turned",
            Settings {
                waterfall_view: Some((yaw, elevation)),
                waterfall_zoom: zoom,
                ..pinned.clone()
            },
        ));
    }
    // At 1x: the label boxes are in points, and a build server's software adapter draws 40 pictures slowly.
    for (w, h) in [
        (1200.0, 600.0),
        (900.0, 420.0),
        (560.0, 360.0),
        (340.0, 160.0),
        (1500.0, 220.0),
    ] {
        for (name, settings) in &cases {
            let mut session = Session::new(PanelKind::Spectrogram, w, h, 1.0, Theme::default(), settings).unwrap();
            session.feed(&frames);
            let _ = session.snapshot_rgba().unwrap();
            let labels = session.renderer.base.text_layer.last_labels.clone();
            assert!(!labels.is_empty(), "{name} {w}x{h}: no labels");
            let bounds = crate::text::Rect::new(-0.5, -0.5, w + 1.0, h + 1.0);
            for (i, (text, rect)) in labels.iter().enumerate() {
                assert!(
                    bounds.contains(rect),
                    "{name} {w}x{h}: \"{text}\" {rect:?} leaves the panel"
                );
                for (other_text, other_rect) in &labels[i + 1..] {
                    assert!(
                        !rect.intersects(other_rect),
                        "{name} {w}x{h}: \"{text}\" {rect:?} on \"{other_text}\" {other_rect:?}"
                    );
                }
            }
        }
    }
}

#[test]
fn the_gpu_cost_stays_in_budget() {
    let Some(ctx) = gpu() else { return };
    if !fonts::available() || cfg!(debug_assertions) || ctx.software {
        return;
    }
    let frames = SyntheticFrames::sequence(240, Options::default());
    let cost = offscreen::measure(PanelKind::Spectrogram, &frames, 1200.0, 600.0, 2.0, 60, &waterfall()).unwrap();
    println!(
        "WATERFALL 1200x600@2x: gpu {:.2} ms, cpu encode {:.2} ms, text {:.2} ms",
        cost.gpu_ms, cost.cpu_encode_ms, cost.text_ms
    );
    assert!(cost.gpu_ms < 6.0, "gpu {:.2} ms", cost.gpu_ms);
}
