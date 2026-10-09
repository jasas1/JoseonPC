//! Parity of the spectrogram history with the macOS app: the time aligner, the
//! layered synthetic frames, and the history of the panel read back from its
//! CPU copy. `tests/golden/spectrogram.txt` holds what the Swift code gives.

#[path = "../../joseon-core/tests/common/mod.rs"]
mod common;

use common::{doubles, floats, Lines, Score};
use joseon_core::contracts::AnalysisFrame;
use joseon_render::contracts::PanelKind;
use joseon_render::offscreen::{Session, Settings};
use joseon_render::spectrogram::aligner::{SpectrogramTimeAligner, SpectrumTiming};
use joseon_render::spectrogram::SpectrogramPanel;
use joseon_render::synthetic::{layered_clicks, Options, SyntheticFrames};
use joseon_render::{RenderContext, Theme};

/// A history cell is 16 bits over 120 dB (0.002 dB a step). The frames differ by up to 0.001 dB.
const TOLERANCE: f32 = 0.02;

#[test]
fn the_time_aligner_matches_the_macos_app() {
    let golden = Lines::of("spectrogram.txt");
    let mut score = Score::default();
    let rows = 64;
    let mut aligner = SpectrogramTimeAligner::new(rows, 96);
    aligner.configure(20.0, 20_000.0, 48_000.0);
    let mut out = vec![0f32; rows];
    let mut t = 0.0f64;
    for i in 0..140 {
        t += if i % 5 == 0 { 0.021 } else { 0.0167 };
        let values: Vec<f32> = (0..rows)
            .map(|r| (0.3 + 0.2 * (r as f64 * 0.3 + t * 9.0).sin()) as f32)
            .collect();
        aligner.push(&values, t);
        if i % 20 == 19 {
            let min_box = 1.0 / 60.0;
            aligner.compose(t - aligner.latency(min_box), min_box, &mut out);
            let key = format!("aligner {i}");
            score.numbers(&key, &out, &floats(golden.get(&key)), 1e-6);
        }
    }
    let min_box = 1.0 / 60.0;
    score.doubles(
        "aligner numbers",
        &[
            aligner.latency(min_box),
            aligner.earliest_center(min_box).unwrap_or(-1.0),
            aligner.delay(0, 0.02),
            aligner.delay(40, 0.02),
            aligner.delay(63, 0.02),
            aligner.newest_time(),
            aligner.first_time().unwrap_or(-1.0),
        ],
        &doubles(golden.get("aligner numbers")),
        1e-9,
    );
    let timing = SpectrumTiming::new(44_100.0);
    score.doubles(
        "aligner timing",
        &[
            timing.window[0],
            timing.window[1],
            timing.window[2],
            timing.hop[0],
            timing.hop[1],
            timing.hop[2],
            timing.center_lag(150.0),
            timing.center_lag(1_000.0),
            timing.hop_at(2_500.0),
        ],
        &doubles(golden.get("aligner timing")),
        1e-9,
    );
    let weights: Vec<f32> = [20.0, 150.0, 200.0, 260.0, 1_000.0, 1_900.0, 2_000.0, 3_000.0, 10_000.0]
        .into_iter()
        .flat_map(SpectrumTiming::weights)
        .collect();
    score.numbers(
        "aligner weights",
        &weights,
        &floats(golden.get("aligner weights")),
        1e-6,
    );
    score.finish();
}

#[test]
fn layered_frames_match_the_macos_app() {
    let golden = Lines::of("spectrogram.txt");
    let mut score = Score::default();
    let frames = layered_clicks(2.0, Some(&[0.5, 1.2]), 30.0, 512);
    let f = &frames[40];
    let mid: Vec<f32> = f.spectrum.mid.iter().step_by(5).copied().collect();
    score.numbers("clicks mid", &mid, &floats(golden.get("clicks mid")), 0.001);
    let mut layers = Vec::new();
    for l in &f.spectrum.mid_layers {
        layers.extend(l.levels_db.iter().step_by(9));
        layers.extend(l.weights.iter().step_by(9));
        layers.push(l.latency_seconds);
    }
    score.numbers("clicks layers", &layers, &floats(golden.get("clicks layers")), 0.001);
    score.finish();
}

#[test]
fn the_history_matches_the_macos_app() {
    if RenderContext::shared().is_none() {
        println!("no GPU adapter: skipped");
        return;
    }
    let golden = Lines::of("spectrogram.txt");
    let mut score = Score::default();
    let hz = [25.0, 43.8, 110.0, 220.0, 440.0, 1_000.0, 3_100.0, 8_000.0, 15_000.0];
    let mut history = |tag: &str, frames: Vec<AnalysisFrame>, seconds: f64| {
        let settings = Settings {
            history_seconds: seconds,
            ..Default::default()
        };
        let mut session = Session::new(PanelKind::Spectrogram, 900.0, 360.0, 2.0, Theme::default(), &settings).unwrap();
        session.feed(&frames);
        let (r, _) = session.renderer.panel_as::<SpectrogramPanel>().unwrap();
        score.doubles(
            &format!("{tag} state"),
            &[
                r.top_db() as f64,
                r.latency_seconds(),
                r.newest_column_time(),
                r.uses_layers() as u8 as f64,
                r.column_seconds(),
            ],
            &doubles(golden.get(&format!("{tag} state"))),
            1e-6,
        );
        for back in [0, 1, 5, 20, 60, 200, 600, 1_199] {
            let ours: Vec<f32> = hz.iter().map(|&f| r.history_level(back, f).unwrap_or(-999.0)).collect();
            let key = format!("{tag} back{back}");
            score.numbers(&key, &ours, &floats(golden.get(&key)), TOLERANCE);
        }
        score.numbers(
            &format!("{tag} ticks"),
            &r.legend_ticks(),
            &floats(golden.get(&format!("{tag} ticks"))),
            0.0,
        );
        let theirs = golden.get(&format!("{tag} column"));
        match r.history_column(1.5) {
            Some((id, slice)) => {
                let mut ours = vec![id as f32];
                ours.extend(slice.mid_db.iter().step_by(16));
                score.numbers(&format!("{tag} column"), &ours, &floats(theirs), TOLERANCE);
            }
            None => score.same(&format!("{tag} column"), "none", theirs),
        }
    };
    history("plain", SyntheticFrames::sequence(600, Options::default()), 20.0);
    history("short", SyntheticFrames::sequence(400, Options::default()), 5.0);
    history("layers", layered_clicks(6.0, None, 60.0, 1024), 8.0);
    score.finish();
}
