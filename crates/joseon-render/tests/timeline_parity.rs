//! Parity of the session timeline with the macOS app: the demo session record,
//! the model of a window (spans, silences, gaps, statistics, round minutes),
//! the time texts, and the panel at seven sizes (lanes, scale, marks, readout).
//! `tests/golden/timeline.txt` holds what the Swift code gives.

#[path = "../../joseon-core/tests/common/mod.rs"]
mod common;

use common::{doubles, floats, Lines, Score};
use joseon_render::contracts::{PanelCursor, PanelKind};
use joseon_render::offscreen::{Session, Settings};
use joseon_render::panel::Panel;
use joseon_render::synthetic_session::demo_session;
use joseon_render::timeline::model::{format, unix_seconds, TimelineModel};
use joseon_render::timeline::{clip_count_text, over_count_text, LaneKind, TimelinePanel};
use joseon_render::{RenderContext, Theme};

#[test]
fn the_demo_session_matches_the_macos_app() {
    let golden = Lines::of("timeline.txt");
    let mut score = Score::default();
    for (tag, minutes, level_a) in [("session", 30, true), ("plain", 7, false)] {
        let s = demo_session(minutes, level_a, None);
        let counts = [s.samples.len() as f64, s.events.len() as f64, s.revision as f64];
        score.doubles(
            &format!("{tag} counts"),
            &counts,
            &doubles(golden.get(&format!("{tag} counts"))),
            0.0,
        );
        for i in (0..s.samples.len()).step_by(13) {
            let x = &s.samples[i];
            let ours = [
                x.time,
                unix_seconds(x.date),
                x.momentary_max_lufs as f64,
                x.short_term_lufs as f64,
                x.true_peak_dbtp as f64,
                x.correlation as f64,
                x.bands[0] as f64,
                x.bands[3] as f64,
                x.bands[7] as f64,
                x.level_a.unwrap_or(-999.0) as f64,
                if x.is_silent { 1.0 } else { 0.0 },
            ];
            let key = format!("{tag} sample {i}");
            // The same arithmetic in `f32`: equal to the last bit but one. The wall clock is whole microseconds here.
            score.doubles(&key, &ours, &doubles(golden.get(&key)), 1e-6);
        }
        let events: Vec<String> = s
            .events
            .iter()
            .map(|e| {
                format!(
                    "{} {} {:?} {:?} {:?} [{}] [{}]",
                    e.id,
                    e.kind.as_str(),
                    e.time,
                    unix_seconds(e.date),
                    e.value,
                    e.label,
                    e.detail
                )
            })
            .collect();
        score.texts(
            &format!("{tag} events"),
            &events,
            &golden.texts(&format!("{tag} events")),
        );
    }
    score.finish();
}

#[test]
fn the_model_matches_the_macos_app() {
    let golden = Lines::of("timeline.txt");
    let mut score = Score::default();
    let snapshot = demo_session(30, true, None);
    for window in [60, 300, 900, 1_800] {
        let m = TimelineModel::new(&snapshot, window);
        let key = |name: &str| format!("model {window} {name}");
        let flag = |b: bool| if b { 1.0 } else { 0.0 };
        let counts = [
            m.samples.len() as f64,
            m.events.len() as f64,
            m.newest_time,
            m.hidden_spans as f64,
            m.span_rows as f64,
            flag(m.has_level_a),
            m.oldest_time(),
            m.left_edge_time(),
        ];
        score.doubles(&key("counts"), &counts, &doubles(golden.get(&key("counts"))), 0.0);
        let spans: Vec<String> = m
            .spans
            .iter()
            .map(|s| {
                format!(
                    "{:?} {:?} {} {} [{}] [{}]",
                    s.start, s.end, s.row, s.is_open, s.title, s.flag_id
                )
            })
            .collect();
        score.texts(&key("spans"), &spans, &golden.texts(&key("spans")));
        let silences: Vec<f64> = m.silences.iter().flat_map(|s| [s.0, s.1]).collect();
        score.doubles(&key("silences"), &silences, &doubles(golden.get(&key("silences"))), 0.0);
        let gaps: Vec<f64> = m
            .gaps
            .iter()
            .flat_map(|g| [g.time, g.skipped_seconds, g.sample_after as f64])
            .collect();
        score.doubles(&key("gaps"), &gaps, &doubles(golden.get(&key("gaps"))), 1e-9);
        let st = &m.stats;
        let or = |v: Option<f32>| v.unwrap_or(-999.0) as f64;
        let stats = [
            or(st.short_term_low),
            or(st.short_term_high),
            or(st.momentary_max),
            or(st.true_peak_max),
            or(st.level_a_max),
            st.clip_events as f64,
            st.clipped_runs as f64,
            st.overs as f64,
            st.flags as f64,
            st.tracks as f64,
        ];
        score.doubles(&key("stats"), &stats, &doubles(golden.get(&key("stats"))), 1e-6);
        score.same(&key("value"), &m.accessibility_value(), golden.get(&key("value")));
        for step in [1, 5] {
            let marks: Vec<f64> = m
                .minute_marks(step, 7_200)
                .iter()
                .flat_map(|m| [m.time, m.unix])
                .collect();
            let key = key(&format!("minutes {step}"));
            score.doubles(&key, &marks, &doubles(golden.get(&key)), 1e-9);
        }
        let index: Vec<f64> = [-50.0, 0.0, 1.0, 900.2, 1_500.0, 1_799.5, 1_800.0, 1_801.9, 1_802.1]
            .into_iter()
            .map(|t| m.sample_index(t, 2.0).map_or(-1.0, |i| i as f64))
            .collect();
        score.doubles(&key("index"), &index, &doubles(golden.get(&key("index"))), 0.0);
    }
    let unix = 1_790_019_900.0 - 3_600.0 * 7.0 + 67.0;
    let texts = [
        format::clock(unix, true, 7_200),
        format::clock(unix, false, -18_000),
        format::ago(0.4),
        format::ago(42.0),
        format::ago(192.0),
        format::duration(20.0),
        format::duration(188.0),
        clip_count_text(0),
        clip_count_text(3),
        over_count_text(1),
        over_count_text(2),
    ];
    score.same("format", &texts.join("|"), golden.get("format"));
    score.finish();
}

#[test]
fn the_panel_matches_the_macos_app() {
    if RenderContext::shared().is_none() {
        println!("no GPU: skipped");
        return;
    }
    let golden = Lines::of("timeline.txt");
    let mut score = Score::default();
    // tag, size, window seconds, ear lane, target, linked cursor (seconds ago, pinned), hover, high contrast
    type Case = (
        &'static str,
        (f32, f32),
        i64,
        bool,
        Option<f32>,
        Option<f64>,
        Option<[f32; 2]>,
        bool,
    );
    let cases: [Case; 7] = [
        ("wide", (1100.0, 360.0), 900, false, None, None, None, false),
        ("ear", (1100.0, 440.0), 1800, true, Some(-14.0), None, None, false),
        ("narrow", (600.0, 300.0), 900, true, None, None, None, false),
        (
            "strip",
            (900.0, 150.0),
            600,
            false,
            Some(-16.0),
            None,
            Some([500.0, 60.0]),
            false,
        ),
        ("cursor", (1100.0, 300.0), 900, false, None, Some(330.0), None, true),
        (
            "hover",
            (1000.0, 400.0),
            1800,
            true,
            None,
            None,
            Some([420.0, 150.0]),
            false,
        ),
        ("short", (800.0, 240.0), 60, false, None, Some(700.0), None, false),
    ];
    for (tag, (width, height), window, ear, target, cursor_ago, hover, contrast) in cases {
        let settings = Settings {
            session: Some(demo_session(30, true, None)),
            timeline_window_seconds: window,
            timeline_show_level_at_ear: ear,
            target_lufs: target,
            timeline_utc_offset_seconds: Some(7_200),
            cursor: cursor_ago.map(|ago| PanelCursor {
                seconds_ago: Some(ago),
                is_pinned: true,
                ..PanelCursor::new(440.0, PanelKind::Spectrogram)
            }),
            hover,
            increase_contrast: contrast,
            ..Settings::default()
        };
        let mut session = Session::new(PanelKind::Timeline, width, height, 2.0, Theme::default(), &settings).unwrap();
        session.snapshot_rgba().unwrap();
        let (r, base) = session.renderer.panel_as::<TimelinePanel>().unwrap();
        let key = |name: &str| format!("{tag} {name}");
        let lanes: Vec<f32> = r
            .lanes()
            .iter()
            .flat_map(|l| {
                let kind = match l.kind {
                    LaneKind::Loudness => 0.0,
                    LaneKind::Peak => 1.0,
                    LaneKind::Ear => 2.0,
                    LaneKind::Tone => 3.0,
                };
                [kind, l.rect.x, l.rect.y, l.rect.w, l.rect.h, l.caption_y]
            })
            .collect();
        score.numbers(&key("lanes"), &lanes, &floats(golden.get(&key("lanes"))), 1e-3);
        let (x0, x1) = r.plot_x();
        let (top, bottom) = r.loudness_range();
        let layout = [
            x0,
            x1,
            r.axis_y(),
            r.spans_top(),
            top,
            bottom,
            r.focus_index(base).map_or(-1.0, |i| i as f32),
            if r.content_version() > 0 { 1.0 } else { 0.0 },
        ];
        score.numbers(&key("layout"), &layout, &floats(golden.get(&key("layout"))), 1e-3);
        let marks: Vec<f32> = r
            .minute_marks()
            .iter()
            .flat_map(|m| [m.x, if m.labeled { 1.0 } else { 0.0 }])
            .collect();
        score.numbers(&key("marks"), &marks, &floats(golden.get(&key("marks"))), 2e-3);
        let items: Vec<String> = r
            .cursor_items(base)
            .iter()
            .map(|item| {
                let tone = format!("{:?}", item.tone).to_lowercase();
                let swatch = if item.swatch.is_some() { "swatch" } else { "-" };
                format!("{} {tone} {swatch} {}", item.rank, item.text)
            })
            .collect();
        score.texts(&key("items"), &items, &golden.texts(&key("items")));
        let lines = r.focus_index(base).map_or(Vec::new(), |i| r.readout_lines(i));
        score.texts(&key("lines"), &lines, &golden.texts(&key("lines")));
        let label = r.hover_label(base).map_or(Vec::new(), |l| l.0);
        score.texts(&key("label"), &label, &golden.texts(&key("label")));
        score.same(&key("value"), &r.accessibility_value(base), golden.get(&key("value")));
        let colors: Vec<f32> = ["sub-bass", "treble-peak", "other"]
            .into_iter()
            .flat_map(|id| r.span_color(base, id))
            .collect();
        score.numbers(
            &key("spanColors"),
            &colors,
            &floats(golden.get(&key("spanColors"))),
            1e-6,
        );
    }
    score.finish();
}
