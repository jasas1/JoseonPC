//! The spectrogram panel, drawn offscreen.

use super::*;
use crate::contracts::{PanelCursor, PanelKind};
use crate::offscreen::{Session, Settings};
use crate::palette::Theme;
use crate::panel::Panel;
use crate::spectrogram::SpectrogramPanel;
use crate::synthetic::{layered_clicks, Options, SyntheticFrames};
use crate::text::fonts;
use joseon_core::contracts::AnalysisFrame;

fn session(name: &str, frames: &[AnalysisFrame], settings: &Settings) -> (Session, Vec<u8>) {
    let mut session = Session::new(PanelKind::Spectrogram, 900.0, 360.0, 2.0, Theme::default(), settings).unwrap();
    session.feed(frames);
    let rgba = session.snapshot_rgba().unwrap();
    std::fs::write(
        review_path(name),
        crate::target::encode_png(session.target.width, session.target.height, &rgba),
    )
    .unwrap();
    (session, rgba)
}

#[test]
fn the_history_scrolls_in_from_the_right() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let frames = SyntheticFrames::sequence(900, Options::default());
    let (mut session, rgba) = session("spectrogram-plain.png", &frames, &Settings::default());
    let width = session.target.width;
    let (panel, base) = session.renderer.panel_as::<SpectrogramPanel>().unwrap();
    let plot = panel.plot_rect();
    assert_eq!((plot.x, plot.y, plot.max_x(), plot.max_y()), (44.0, 12.0, 842.0, 334.0));
    // 15 s of frames in a 20 s history: the right part has light, the left quarter is still the plot background.
    let at = |x: f32, y: f32| pixel(&rgba, width, (x * 2.0) as u32, (y * 2.0) as u32);
    let lit = at(plot.max_x() - 40.0, panel.y_of(440.0));
    assert!(
        lit[0] as u32 + lit[1] as u32 + lit[2] as u32 > 120,
        "music near now: {lit:?}"
    );
    let empty = at(plot.min_x() + 60.0, plot.mid_y());
    assert_color(empty, [6, 9, 17], 3, "no history yet at the left");
    // The picture and the CPU copy say the same about where the history starts.
    assert!(panel.history_level_at([plot.min_x() + 60.0, plot.mid_y()]).is_none());
    assert!(
        panel
            .history_level_at([plot.max_x() - 40.0, panel.y_of(440.0)])
            .unwrap()
            > -60.0
    );
    assert!(panel.emitted_columns() > 800 && panel.emitted_columns() < 900);
    assert_eq!(panel.accessibility_label(), "Spectrogram");
    assert!(panel
        .accessibility_value(base)
        .starts_with("20 seconds of history. Strongest now: "));
    // The color of a level is the color of the legend.
    assert_eq!(panel.color_for(base, -100.0), base.palette.heat_lut(256)[0]);
    assert_eq!(panel.color_for(base, 0.0), base.palette.heat_lut(256)[255]);
}

#[test]
fn layers_and_the_cursor_are_drawn() {
    if gpu().is_none() || !fonts::available() {
        return;
    }
    let frames = layered_clicks(7.0, None, 60.0, 1024);
    let layered = Settings {
        history_seconds: 8.0,
        ..Default::default()
    };
    let (mut first, _) = session("spectrogram-layers.png", &frames, &layered);
    let (panel, _) = first.renderer.panel_as::<SpectrogramPanel>().unwrap();
    assert!(panel.uses_layers());
    // A click is one vertical line: the lows and the highs of one column have the same time.
    let click_age = 7.0 - 3.0;
    let back = ((click_age - panel.latency_seconds()) / 8.0 * 1199.0).round() as usize;
    // The short window of the highs makes a short hill: find the top of each hill near that column.
    let top_near = |hz: f32| {
        (back - 30..back + 30)
            .map(|b| (panel.history_level(b, hz).unwrap(), b))
            .fold((f32::MIN, 0), |best, v| if v.0 > best.0 { v } else { best })
    };
    let (low, low_at) = top_near(60.0);
    let (high, high_at) = top_near(9_000.0);
    assert!(
        low > panel.history_level(back + 120, 60.0).unwrap() + 6.0,
        "the click in the lows"
    );
    assert!(
        high > panel.history_level(back + 120, 9_000.0).unwrap() + 6.0,
        "the click in the highs"
    );
    println!("click: lows at column {low_at}, highs at column {high_at}, expected {back}");
    assert!(
        low_at.abs_diff(high_at) <= 6,
        "one time for lows and highs: {low_at} and {high_at}"
    );
    assert!(
        high_at.abs_diff(back) <= 12,
        "at the time of the click: {high_at}, expected {back}"
    );

    let cursor = Settings {
        history_seconds: 8.0,
        cursor: Some(PanelCursor {
            frequency_hz: 440.0,
            seconds_ago: Some(3.0),
            source: PanelKind::Spectrogram,
            is_pinned: true,
        }),
        ..Default::default()
    };
    let (mut second, _) = session("spectrogram-cursor.png", &frames, &cursor);
    let items = second.renderer.panel.cursor_items(&second.renderer.base);
    let texts: Vec<&str> = items.iter().map(|i| i.text.as_str()).collect();
    assert_eq!(texts[0], "440.0 Hz");
    assert!(texts.contains(&"\u{2212}3.0 s"), "{texts:?}");
    assert!(texts.last().unwrap().starts_with("Mid "), "{texts:?}");
    let (panel, _) = second.renderer.panel_as::<SpectrogramPanel>().unwrap();
    let (id, slice) = panel.history_column(3.0).unwrap();
    assert_eq!(Some(id), panel.history_column_id(3.0));
    assert_eq!(slice.mid_db.len(), 1024);
    assert!(panel.history_column(30.0).is_none());

    let hover = Settings {
        hover: Some([600.0, 150.0]),
        ..Default::default()
    };
    session(
        "spectrogram-hover.png",
        &SyntheticFrames::sequence(900, Options::default()),
        &hover,
    );
}
