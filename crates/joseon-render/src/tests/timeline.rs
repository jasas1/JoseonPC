//! The session timeline, drawn offscreen.

use super::*;
use crate::contracts::{PanelCursor, PanelKind};
use crate::offscreen::{Session, Settings};
use crate::palette::Theme;
use crate::panel::{Panel, PanelRenderer};
use crate::synthetic_session::demo_session;
use crate::text::fonts;
use crate::timeline::{LaneKind, TimelinePanel};
use joseon_core::contracts::SessionSnapshot;

fn settings(window: i64, ear: bool, target: Option<f32>) -> Settings {
    Settings {
        session: Some(demo_session(30, true, None)),
        timeline_window_seconds: window,
        timeline_show_level_at_ear: ear,
        target_lufs: target,
        timeline_utc_offset_seconds: Some(7_200),
        ..Settings::default()
    }
}

fn session(name: &str, size: (f32, f32), settings: &Settings) -> (Session, Vec<u8>) {
    let mut session = Session::new(PanelKind::Timeline, size.0, size.1, 2.0, Theme::default(), settings).unwrap();
    let rgba = session.snapshot_rgba().unwrap();
    std::fs::write(
        review_path(name),
        crate::target::encode_png(session.target.width, session.target.height, &rgba),
    )
    .unwrap();
    (session, rgba)
}

#[test]
fn every_layout_draws() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let cursor = |ago: f64| {
        Some(PanelCursor {
            seconds_ago: Some(ago),
            is_pinned: true,
            ..PanelCursor::new(440.0, PanelKind::Spectrogram)
        })
    };
    let cases = [
        ("wide", (1100.0, 360.0), settings(900, false, None), 3),
        ("ear", (1100.0, 440.0), settings(1800, true, Some(-14.0)), 4),
        ("narrow", (600.0, 300.0), settings(900, true, None), 4),
        (
            "strip",
            (900.0, 150.0),
            Settings {
                hover: Some([500.0, 60.0]),
                ..settings(600, false, Some(-16.0))
            },
            2,
        ),
        (
            "cursor",
            (1100.0, 300.0),
            Settings {
                cursor: cursor(330.0),
                increase_contrast: true,
                ..settings(900, false, None)
            },
            3,
        ),
        (
            "hover",
            (1000.0, 400.0),
            Settings {
                hover: Some([420.0, 150.0]),
                ..settings(1800, true, None)
            },
            4,
        ),
        (
            "short",
            (800.0, 240.0),
            Settings {
                cursor: cursor(700.0),
                ..settings(60, false, None)
            },
            2,
        ),
    ];
    for (tag, size, settings, lanes) in cases {
        let (mut session, rgba) = session(&format!("timeline-{tag}.png"), size, &settings);
        let width = session.target.width;
        let (panel, _) = session.renderer.panel_as::<TimelinePanel>().unwrap();
        assert_eq!(panel.lanes().len(), lanes, "{tag}");
        // The loudness band has light near the right end of its lane.
        let lane = panel.lanes()[0];
        assert_eq!(lane.kind, LaneKind::Loudness);
        let x = panel.plot_x().1 - 20.0;
        let lit = (0..lane.rect.h as u32)
            .map(|dy| pixel(&rgba, width, (x * 2.0) as u32, ((lane.rect.y + dy as f32) * 2.0) as u32))
            .filter(|px| px[2] > 110)
            .count();
        assert!(lit >= 2, "{tag}: the loudness trace is drawn ({lit} rows)");
    }
}

#[test]
fn an_equal_revision_is_not_taken_in_again() {
    let Some(ctx) = gpu() else { return };
    let mut renderer = PanelRenderer::make(PanelKind::Timeline, ctx, Theme::default()).unwrap();
    renderer.set_layout(1000.0, 400.0, 2.0);
    let snapshot = demo_session(10, true, None);
    let (panel, base) = renderer.panel_as::<TimelinePanel>().unwrap();
    assert!(panel.model().is_empty());
    assert_eq!(panel.accessibility_value(base), "last 15 minutes: nothing recorded yet");
    panel.set_snapshot(base, snapshot.clone());
    let (count, version) = (panel.rebuild_count(), panel.content_version());
    base.needs_display = false;
    panel.set_snapshot(base, snapshot.clone());
    assert_eq!((panel.rebuild_count(), panel.content_version()), (count, version));
    assert!(!base.needs_display);
    panel.set_snapshot(
        base,
        SessionSnapshot {
            revision: snapshot.revision + 1,
            ..snapshot
        },
    );
    assert_eq!(panel.rebuild_count(), count + 1);
    assert!(base.needs_display);
    assert!(!panel.uses_analysis_frames() && panel.makes_pointer_cursors());
    assert_eq!(panel.accessibility_label(), "Session timeline");
}

#[test]
fn the_pointer_makes_a_cursor_at_the_second_under_it() {
    let Some(ctx) = gpu() else { return };
    let mut renderer = PanelRenderer::make(PanelKind::Timeline, ctx, Theme::default()).unwrap();
    renderer.set_layout(1000.0, 400.0, 2.0);
    {
        let (panel, base) = renderer.panel_as::<TimelinePanel>().unwrap();
        panel.set_snapshot(base, demo_session(30, true, None));
    }
    let (panel, base) = renderer.panel_as::<TimelinePanel>().unwrap();
    let (x0, x1) = panel.plot_x();
    let y = panel.lanes()[0].rect.mid_y();
    // 15 minutes on the plot: the middle is 450 s ago.
    let c = panel.cursor_at(base, [(x0 + x1) / 2.0, y]).unwrap();
    assert_eq!(c.source, PanelKind::Timeline);
    assert_eq!(c.frequency_hz, 1000.0, "no cursor yet: the fallback frequency");
    let ago = c.seconds_ago.unwrap();
    assert!((ago - 450.0).abs() <= 1.0, "{ago} s ago");
    assert!(panel.cursor_at(base, [x0 - 5.0, y]).is_none());
    assert!(panel.cursor_at(base, [x1 - 5.0, panel.axis_y() + 4.0]).is_none());
    // A cursor that moves along a frequency axis elsewhere repaints nothing here.
    renderer.set_cursor_linked(true);
    renderer.set_cursor(Some(PanelCursor::new(440.0, PanelKind::Spectrum)));
    renderer.base.needs_display = false;
    renderer.set_cursor(Some(PanelCursor::new(880.0, PanelKind::Spectrum)));
    assert!(!renderer.base.needs_display);
    renderer.set_cursor(Some(c));
    assert!(renderer.base.needs_display);
    let (panel, base) = renderer.panel_as::<TimelinePanel>().unwrap();
    let i = panel.focus_index(base).unwrap();
    assert!((panel.model().newest_time - panel.model().samples[i].time - ago).abs() < 1e-9);
    assert!(panel.is_on_cursor(base, [(x0 + x1) / 2.0 + 3.0, y]));
    assert!(!panel.is_on_cursor(base, [(x0 + x1) / 2.0 + 30.0, y]));
    assert!(panel.cursor_shows_in_text(&c));
    assert!(!panel.cursor_shows_in_text(&PanelCursor::new(440.0, PanelKind::Spectrum)));
}

#[test]
fn the_readout_names_what_happened_in_a_second() {
    let mut panel = TimelinePanel::new();
    let Some(ctx) = gpu() else { return };
    let mut renderer = PanelRenderer::make(PanelKind::Timeline, ctx, Theme::default()).unwrap();
    renderer.set_layout(1000.0, 400.0, 2.0);
    let snapshot = demo_session(30, true, None);
    panel.set_window_seconds(&mut renderer.base, 1_800);
    panel.set_snapshot(&mut renderer.base, snapshot.clone());
    let model = panel.model();
    // The first sample after the pause.
    let after_pause = model.gaps[0].sample_after;
    let texts = panel.event_texts(after_pause);
    assert_eq!(texts[0], "after a pause of 3:10 min");
    assert!(texts.contains(&"track start".to_string()), "{texts:?}");
    // A second inside the sub-bass flag: the flag is up, with the hue of its bar.
    let inside = model
        .samples
        .iter()
        .position(|s| s.time > model.spans[0].start + 30.0)
        .unwrap();
    assert_eq!(panel.event_texts(inside), ["flag up: Strong sub-bass in the signal"]);
    let hues = panel.event_hues(&renderer.base, inside);
    assert_eq!(hues, [Some(panel.span_color(&renderer.base, "sub-bass"))]);
    // The silence: one line, no numbers.
    let silent = model.samples.iter().position(|s| s.is_silent).unwrap() + 5;
    assert_eq!(panel.readout_lines(silent)[1], "digital silence");
    // A clip burst.
    let clip = snapshot
        .events
        .iter()
        .find(|e| e.kind == joseon_core::contracts::SessionEventKind::Clip)
        .unwrap();
    let i = model.sample_index(clip.time, 2.0).unwrap();
    assert_eq!(panel.event_texts(i), ["clipped \u{00D7}3"]);
}
