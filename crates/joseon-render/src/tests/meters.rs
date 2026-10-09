//! The meters panel, drawn offscreen.

use super::*;
use crate::contracts::{DoseStandard, PanelCursor, PanelKind};
use crate::meters::ear::{EarDoseLedger, EarShown};
use crate::meters::{EarMode, MetersPanel, NO_VALUE};
use crate::offscreen::{Session, Settings};
use crate::palette::Theme;
use crate::panel::Panel;
use crate::synthetic::{Options, SyntheticFrames};
use crate::text::fonts;
use joseon_core::contracts::AnalysisFrame;
use std::sync::Arc;

fn frames(rich: bool) -> Vec<AnalysisFrame> {
    let options = Options {
        include_spl: rich,
        include_headphone: rich,
        ..Options::default()
    };
    SyntheticFrames::sequence(420, options)
}

fn session(name: &str, rich: bool, size: (f32, f32), settings: &Settings) -> (Session, Vec<u8>) {
    let mut session = Session::new(PanelKind::Meters, size.0, size.1, 2.0, Theme::default(), settings).unwrap();
    session.feed(&frames(rich));
    let rgba = session.snapshot_rgba().unwrap();
    std::fs::write(
        review_path(name),
        crate::target::encode_png(session.target.width, session.target.height, &rgba),
    )
    .unwrap();
    (session, rgba)
}

fn settings(target: Option<f32>, standard: DoseStandard, cursor_hz: Option<f32>) -> Settings {
    Settings {
        target_lufs: target,
        dose_standard: standard,
        cursor: cursor_hz.map(|hz| PanelCursor {
            is_pinned: true,
            ..PanelCursor::new(hz, PanelKind::Spectrum)
        }),
        ..Settings::default()
    }
}

#[test]
fn every_layout_draws() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let (niosh, who) = (DoseStandard::NioshDaily, DoseStandard::WhoWeekly);
    let cases = [
        (
            "wide",
            false,
            (1280.0, 560.0),
            settings(None, niosh, None),
            EarMode::None,
        ),
        (
            "wide-ear",
            true,
            (1280.0, 560.0),
            settings(Some(-14.0), niosh, Some(1_000.0)),
            EarMode::Column,
        ),
        (
            "tall-ear",
            true,
            (1100.0, 700.0),
            settings(Some(-23.0), who, None),
            EarMode::Column,
        ),
        (
            "low-ear",
            true,
            (900.0, 380.0),
            settings(None, niosh, None),
            EarMode::Column,
        ),
        (
            "strip",
            true,
            (700.0, 420.0),
            settings(Some(-16.0), niosh, Some(100.0)),
            EarMode::Strip,
        ),
        ("row", true, (700.0, 220.0), settings(None, who, None), EarMode::Row),
        (
            "compact",
            false,
            (380.0, 300.0),
            settings(Some(-14.0), niosh, Some(5_000.0)),
            EarMode::None,
        ),
        (
            "narrow-ear",
            true,
            (330.0, 200.0),
            settings(None, niosh, None),
            EarMode::Row,
        ),
    ];
    for (tag, rich, size, settings, mode) in cases {
        let (mut session, rgba) = session(&format!("meters-{tag}.png"), rich, size, &settings);
        let width = session.target.width;
        let (panel, _) = session.renderer.panel_as::<MetersPanel>().unwrap();
        assert_eq!(panel.ear_mode(), mode, "{tag}");
        assert_eq!(panel.is_compact(), size.0 < if rich { 820.0 } else { 760.0 }, "{tag}");
        // The momentary bar has light: the synthetic music is near -14 LUFS.
        let [loud, ..] = panel.layout_rects();
        let x = loud.min_x() + if panel.is_compact() { 26.0 } else { 32.0 } + 8.0;
        let y = loud.max_y() - 22.0 - 12.0;
        let px = pixel(&rgba, width, (x * 2.0) as u32, (y * 2.0) as u32);
        assert!(px[2] > 90, "{tag}: the foot of the momentary bar is blue: {px:?}");
    }
}

#[test]
fn the_history_keeps_no_value_until_the_windows_have_filled() {
    let Some(ctx) = gpu() else { return };
    let mut renderer = crate::panel::PanelRenderer::make(PanelKind::Meters, ctx, Theme::default()).unwrap();
    renderer.set_layout(1280.0, 560.0, 2.0);
    for f in frames(false) {
        renderer.ingest(Arc::new(f));
    }
    let (panel, _) = renderer.panel_as::<MetersPanel>().unwrap();
    let (momentary, short_term) = panel.history();
    // 7 s of frames, one slot per 100 ms.
    assert!((68..=70).contains(&momentary.len()), "{} slots", momentary.len());
    assert_eq!(momentary.len(), short_term.len());
    // M is a 400 ms window, S a 3 s window: the first slots hold no measurement.
    assert_eq!(momentary[0], NO_VALUE);
    assert!(momentary[6] > -40.0);
    assert_eq!(short_term[20], NO_VALUE);
    assert!(short_term[40] > -40.0);
    let (top, bottom) = panel.spark_range();
    assert_eq!(top - bottom, 30.0);
    assert!(
        top % 6.0 == 0.0 && top <= 0.0 && top >= -18.0,
        "the top is on the 6 LU grid: {top}"
    );
}

#[test]
fn the_dose_ledger_of_the_app_replaces_the_running_dose() {
    let Some(ctx) = gpu() else { return };
    let mut renderer = crate::panel::PanelRenderer::make(PanelKind::Meters, ctx, Theme::default()).unwrap();
    renderer.set_layout(1280.0, 560.0, 2.0);
    for f in frames(true) {
        renderer.ingest(Arc::new(f));
    }
    let (panel, base) = renderer.panel_as::<MetersPanel>().unwrap();
    let running = panel.dose_value();
    panel.set_dose_ledger(
        base,
        Some(EarDoseLedger {
            niosh_today: 0.5,
            who_week: 1.2,
        }),
    );
    assert_eq!(panel.dose_value().0, 50);
    assert_ne!(panel.dose_value(), running);
    assert!(base.needs_display);
    panel.set_dose_standard(base, DoseStandard::WhoWeekly);
    assert_eq!(panel.dose_value().0, 120);
    assert_eq!(panel.dose_caption(), "DOSE \u{00B7} WHO WEEK");
    assert!(panel.accessibility_value(base).ends_with("dose 120 percent"));
    // A silent frame has no level: a dash, and no time left.
    let spl = base.frame.as_ref().unwrap().spl.clone().unwrap();
    let silent = EarShown::new(&spl, true, None);
    assert_eq!(
        (silent.slow, silent.fast, silent.left_niosh, silent.left_who),
        (EarShown::NONE, EarShown::NONE, -2, -2)
    );
    assert_eq!(EarShown::level_text(silent.slow), "\u{2014}");
    // Half of today's allowance is left: half of the time at this level.
    let with = EarShown::new(
        &spl,
        false,
        Some(EarDoseLedger {
            niosh_today: 0.5,
            who_week: 0.0,
        }),
    );
    let allowed = 8.0 * 3600.0 * 2f64.powf((85.0 - spl.level_a_slow as f64) / 3.0);
    assert_eq!(with.left_niosh, EarShown::stepped_minutes(0.5 * allowed));
}

#[test]
fn the_cursor_only_matters_when_its_band_changes() {
    let Some(ctx) = gpu() else { return };
    let mut renderer = crate::panel::PanelRenderer::make(PanelKind::Meters, ctx, Theme::default()).unwrap();
    renderer.set_layout(1280.0, 560.0, 2.0);
    renderer.ingest(Arc::new(frames(false).pop().unwrap()));
    renderer.set_cursor_linked(true);
    renderer.set_cursor(Some(PanelCursor::new(1_000.0, PanelKind::Spectrum)));
    renderer.base.needs_display = false;
    renderer.set_cursor(Some(PanelCursor::new(1_200.0, PanelKind::Spectrum)));
    assert!(!renderer.base.needs_display, "the same band: nothing to draw");
    renderer.set_cursor(Some(PanelCursor::new(3_000.0, PanelKind::Spectrum)));
    assert!(renderer.base.needs_display, "the next band: the highlight moves");
    let (panel, base) = renderer.panel_as::<MetersPanel>().unwrap();
    let items = panel.cursor_items(base);
    assert_eq!(items.len(), 2);
    assert!(items[0].text.starts_with("Upper mid "), "{}", items[0].text);
    assert_eq!(panel.accessibility_label(), "Loudness and level meters");
    let clip = panel.clip_indicator_rect();
    let lamp = panel.clip_lamp_center();
    assert!(clip.contains_point(lamp[0] - 1.0, lamp[1]));
}
