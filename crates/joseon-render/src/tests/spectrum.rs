//! The spectrum panel, drawn offscreen.

use super::*;
use crate::contracts::{LevelAxis, PanelCursor, PanelKind};
use crate::offscreen::{self, Session, Settings};
use crate::palette::Theme;
use crate::panel::Panel;
use crate::spectrum::SpectrumPanel;
use crate::synthetic::{Options, SyntheticFrames};
use crate::text::fonts;

fn can_draw() -> bool {
    gpu().is_some() && fonts::available()
}

/// Saves a review picture and gives its pixels.
fn picture(name: &str, frames: usize, options: Options, size: (f32, f32), settings: &Settings) -> (Vec<u8>, u32) {
    let frames = SyntheticFrames::sequence(frames, options);
    let mut session = Session::new(PanelKind::Spectrum, size.0, size.1, 2.0, Theme::default(), settings).unwrap();
    session.feed(&frames);
    let rgba = session.snapshot_rgba().unwrap();
    let (w, h) = (session.target.width, session.target.height);
    std::fs::write(review_path(name), crate::target::encode_png(w, h, &rgba)).unwrap();
    (rgba, w)
}

#[test]
fn the_spectrum_panel_draws_the_demo_music() {
    if !can_draw() {
        return;
    }
    let (rgba, width) = picture(
        "spectrum-plain.png",
        240,
        Options::default(),
        (900.0, 420.0),
        &Settings::default(),
    );
    assert_eq!(width, 1800);
    // The corner is the panel background. The plot holds lit pixels of the curve.
    assert_color(pixel(&rgba, width, 2, 2), [9, 12, 22], 1, "the background");
    let mut bright = 0;
    for y in (80..780).step_by(3) {
        for x in (100..1750).step_by(3) {
            let p = pixel(&rgba, width, x, y);
            if p[0] as u32 + p[1] as u32 + p[2] as u32 > 450 {
                bright += 1;
            }
        }
    }
    assert!(bright > 300, "the curve is on the picture: {bright} bright samples");
}

#[test]
fn the_headphone_band_the_spl_layer_and_the_cursor_are_drawn() {
    if !can_draw() {
        return;
    }
    let rich = Options {
        include_headphone: true,
        include_spl: true,
        ..Default::default()
    };
    picture(
        "spectrum-headphone.png",
        240,
        rich,
        (1100.0, 560.0),
        &Settings::default(),
    );
    let spl = Settings {
        level_axis: LevelAxis::DbSpl,
        ..Default::default()
    };
    picture("spectrum-spl.png", 240, rich, (1100.0, 560.0), &spl);
    let cursor = Settings {
        cursor: Some(PanelCursor {
            frequency_hz: 440.0,
            seconds_ago: None,
            source: PanelKind::Spectrogram,
            is_pinned: true,
        }),
        ..Default::default()
    };
    picture("spectrum-cursor.png", 240, rich, (1100.0, 560.0), &cursor);
    let hover = Settings {
        hover: Some([400.0, 200.0]),
        ..Default::default()
    };
    picture("spectrum-hover.png", 240, Options::default(), (700.0, 360.0), &hover);
    picture(
        "spectrum-compact.png",
        240,
        Options::default(),
        (380.0, 190.0),
        &Settings::default(),
    );
    let contrast = Settings {
        increase_contrast: true,
        ..Default::default()
    };
    picture("spectrum-contrast.png", 240, rich, (900.0, 420.0), &contrast);
}

#[test]
fn layout_follows_the_size_and_the_headphone_band() {
    if !can_draw() {
        return;
    }
    let frames = SyntheticFrames::sequence(
        60,
        Options {
            include_headphone: true,
            ..Default::default()
        },
    );
    let mut session = Session::new(
        PanelKind::Spectrum,
        1000.0,
        500.0,
        2.0,
        Theme::default(),
        &Settings::default(),
    )
    .unwrap();
    session.feed(&frames);
    session.snapshot_rgba().unwrap();
    let (panel, base) = session.renderer.panel_as::<SpectrumPanel>().unwrap();
    let plot = panel.plot_rect();
    // 44 pt for the dB labels, 40 pt for the axis of the headphone band, a header row and the legend row of the band.
    assert_eq!((plot.x, plot.max_x()), (44.0, 960.0));
    assert_eq!((plot.y, plot.max_y()), (48.0, 474.0));
    let band = panel.headphone_band().expect("the band is shown");
    assert!((band.h - plot.h * 0.25).abs() < 1e-3);
    assert_eq!(panel.point_count(), 916);
    // The music keeps its range under the band.
    let (music, shown) = (panel.music_range(), panel.shown_range());
    assert_eq!(music.0, shown.0);
    assert!((shown.1 - music.1 - (music.1 - music.0) / 3.0).abs() < 1e-3);
    assert!((panel.frequency_of_point(0) - 20.0).abs() < 1e-3);
    assert!((panel.frequency_of_point(915) - 20_000.0).abs() < 1.0);
    assert!(panel.grid_frequencies().contains(&1000.0));
    assert!(panel.cursor_x(base).is_none());
    assert_eq!(panel.accessibility_label(), "Spectrum");
    let spoken = panel.accessibility_value(base);
    assert!(
        spoken.starts_with("Peak ") && spoken.contains("Headphone overlay: Demo headphone"),
        "{spoken}"
    );
}

#[test]
fn a_frame_costs_little() {
    if !can_draw() {
        return;
    }
    let frames = SyntheticFrames::sequence(120, Options::default());
    let cost = offscreen::measure(
        PanelKind::Spectrum,
        &frames,
        900.0,
        420.0,
        2.0,
        120,
        &Settings::default(),
    )
    .unwrap();
    println!(
        "spectrum frame: cpu {:.3} ms, gpu wait {:.3} ms, text {:.3} ms",
        cost.cpu_encode_ms, cost.gpu_ms, cost.text_ms
    );
    assert!(cost.cpu_encode_ms.is_finite() && cost.text_ms.is_finite());
}
