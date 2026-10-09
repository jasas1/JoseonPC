//! Parity of the meters panel with the macOS app: the layout at eight sizes
//! (with and without the level at the ear), the plan of the ear column, the
//! latched ear numbers, the loudness history, the readouts.
//! `tests/golden/meters.txt` holds what the Swift code gives.

#[path = "../../joseon-core/tests/common/mod.rs"]
mod common;

use common::{doubles, floats, Lines, Score};
use joseon_render::contracts::{DoseStandard, PanelCursor, PanelKind};
use joseon_render::meters::ear::{uncertainty_text, EarShown};
use joseon_render::meters::{target_delta_color, EarMode, MetersPanel};
use joseon_render::offscreen::{Session, Settings};
use joseon_render::panel::Panel;
use joseon_render::synthetic::{Options, SyntheticFrames};
use joseon_render::{Palette, RenderContext, Theme};

#[test]
fn the_ear_texts_match_the_macos_app() {
    let golden = Lines::of("meters.txt");
    let mut score = Score::default();
    let seconds = [
        -5.0,
        0.0,
        299.0,
        300.0,
        7_199.0,
        7_200.0,
        8_100.0,
        35_999.0,
        36_000.0,
        39_600.0,
        400_000.0,
        f64::INFINITY,
    ];
    let stepped: Vec<f64> = seconds.iter().map(|s| EarShown::stepped_minutes(*s) as f64).collect();
    score.doubles("stepped", &stepped, &doubles(golden.get("stepped")), 0.0);
    let times: Vec<String> = [-2, -1, 0, 5, 65, 600, 5_999, 6_000]
        .into_iter()
        .map(EarShown::time_text)
        .collect();
    score.same("timeText", &times.join("|"), golden.get("timeText"));
    let texts: Vec<String> = [0.0, 0.4, 2.0, 3.5, 3.6, 4.49, -1.0, f64::NAN]
        .into_iter()
        .map(|db| {
            format!(
                "{}/{}",
                uncertainty_text::total(db, true),
                uncertainty_text::term(db, false)
            )
        })
        .collect();
    score.same("uncertainty", &texts.join("|"), golden.get("uncertainty"));
    let p = Palette::new(&Theme::default(), false);
    let colors: Vec<f32> = [-3.0, -1.04, -1.06, 0.0, 1.04, 1.06, 4.0]
        .into_iter()
        .flat_map(|d| target_delta_color(d, &p))
        .collect();
    score.numbers("deltaColor", &colors, &floats(golden.get("deltaColor")), 1e-6);
    score.finish();
}

#[test]
fn the_panel_matches_the_macos_app() {
    if RenderContext::shared().is_none() {
        println!("no GPU: skipped");
        return;
    }
    let golden = Lines::of("meters.txt");
    let mut score = Score::default();
    let (niosh, who) = (DoseStandard::NioshDaily, DoseStandard::WhoWeekly);
    type Case = (&'static str, bool, (f32, f32), Option<f32>, DoseStandard, Option<f32>);
    let cases: [Case; 8] = [
        ("wide", false, (1280.0, 560.0), None, niosh, None),
        ("wide-ear", true, (1280.0, 560.0), Some(-14.0), niosh, Some(1_000.0)),
        ("tall-ear", true, (1100.0, 700.0), Some(-23.0), who, None),
        ("low-ear", true, (900.0, 380.0), None, niosh, None),
        ("strip", true, (700.0, 420.0), Some(-16.0), niosh, Some(100.0)),
        ("row", true, (700.0, 220.0), None, who, None),
        ("compact", false, (380.0, 300.0), Some(-14.0), niosh, Some(5_000.0)),
        ("narrow-ear", true, (330.0, 200.0), None, niosh, None),
    ];
    for (tag, rich, (width, height), target, standard, cursor_hz) in cases {
        let settings = Settings {
            target_lufs: target,
            dose_standard: standard,
            cursor: cursor_hz.map(|hz| PanelCursor {
                is_pinned: true,
                ..PanelCursor::new(hz, PanelKind::Spectrum)
            }),
            ..Settings::default()
        };
        let options = Options {
            include_spl: rich,
            include_headphone: rich,
            ..Options::default()
        };
        let mut session = Session::new(PanelKind::Meters, width, height, 2.0, Theme::default(), &settings).unwrap();
        session.feed(&SyntheticFrames::sequence(420, options));
        session.snapshot_rgba().unwrap();
        let (r, base) = session.renderer.panel_as::<MetersPanel>().unwrap();
        let key = |name: &str| format!("{tag} {name}");
        // A rectangle with a negative size (bars that have no room and are not drawn): Core Graphics gives its
        // standard form, with the origin moved.
        let standard = |c: &joseon_render::text::Rect| [c.x + c.w.min(0.0), c.y + c.h.min(0.0), c.w.abs(), c.h.abs()];
        let mut layout: Vec<f32> = r.layout_rects().iter().flat_map(standard).collect();
        let mode = match r.ear_mode() {
            EarMode::None => 0.0,
            EarMode::Column => 1.0,
            EarMode::Strip => 2.0,
            EarMode::Row => 3.0,
        };
        let lamp = r.clip_lamp_center();
        layout.extend([
            if r.is_compact() { 1.0 } else { 0.0 },
            mode,
            r.big_font_size(),
            lamp[0],
            lamp[1],
        ]);
        let (clip, strip) = (r.clip_indicator_rect(), r.strip_bar(base));
        layout.extend([clip.x, clip.y, clip.w, clip.h, strip.x, strip.y, strip.w, strip.h]);
        // The bar of the strip (the last four numbers) comes from text widths. With the macOS font it is within
        // 1 pt of the macOS app. Another system has another font: there the bar is only checked to be inside the strip.
        let theirs = floats(golden.get(&key("layout")));
        let fixed = layout.len() - 4;
        if cfg!(target_os = "macos") {
            score.numbers(&key("layout"), &layout, &theirs, 1.0);
        } else {
            score.numbers(&key("layout"), &layout[..fixed], &theirs[..fixed], 1e-3);
            let ear = r.layout_rects()[6];
            assert!(
                strip.w == 0.0 || (strip.x >= ear.x && strip.x + strip.w <= ear.x + ear.w),
                "{tag}: {strip:?}"
            );
        }
        let plan = r.ear_plan();
        let mut numbers = vec![
            plan.content_x,
            plan.caption_y,
            plan.numeral_base,
            plan.numeral_size,
            plan.y85,
            if plan.shows_label85 { 1.0 } else { 0.0 },
            plan.dose_top.unwrap_or(-1.0),
        ];
        numbers.extend(&plan.rows);
        // The cap height of the numeral font is in the plan.
        score.numbers(&key("plan"), &numbers, &floats(golden.get(&key("plan"))), 0.5);
        let shown = match r.ear_shown() {
            Some(e) => format!(
                "{} {} {} {} {} {} {} {} {} [{}] [{}]",
                e.slow,
                e.fast,
                e.leq_track,
                e.leq_session,
                e.max_fast,
                e.dose_niosh,
                e.dose_who,
                e.left_niosh,
                e.left_who,
                e.calibration_name,
                e.uncertainty_text
            ),
            None => "none".to_string(),
        };
        score.same(&key("shown"), &shown, golden.get(&key("shown")));
        let (percent, minutes) = r.dose_value();
        let dose = format!("{percent} {minutes} {}", r.dose_caption());
        score.same(&key("dose"), &dose, golden.get(&key("dose")));
        let (momentary, short_term) = r.history();
        let every = |v: &[f32]| v.iter().step_by(7).copied().collect::<Vec<f32>>();
        score.numbers(
            &key("historyM"),
            &every(&momentary),
            &floats(golden.get(&key("historyM"))),
            2e-3,
        );
        score.numbers(
            &key("historyS"),
            &every(&short_term),
            &floats(golden.get(&key("historyS"))),
            2e-3,
        );
        let items: Vec<String> = r
            .cursor_items(base)
            .iter()
            .map(|item| format!("{} {}", item.rank, item.text))
            .collect();
        score.texts(&key("items"), &items, &golden.texts(&key("items")));
        score.same(&key("value"), &r.accessibility_value(base), golden.get(&key("value")));
        score.numbers(&key("cap"), &r.cap_color(), &floats(golden.get(&key("cap"))), 1e-6);
    }
    score.finish();
}
