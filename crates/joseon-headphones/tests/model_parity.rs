//! Parity of the headphone model and the stress flags with the macOS app.
//!
//! `tests/golden/headphones.txt` holds what the Swift model of the macOS app
//! gives (`tools/golden` makes the file): the static numbers of every embedded
//! curve, and the reading of the model on fixed signals through the engine.
//! This test does the same in Rust and compares numbers and flag texts.

#[path = "../../joseon-core/tests/common/mod.rs"]
mod common;

use common::{floats, golden, loud_signal, Score};
use joseon_core::contracts::StressFlag;
use joseon_core::engine::AnalysisEngine;
use joseon_core::test_signals::demo_block;
use joseon_headphones::embedded_curves::{self, EmbeddedCurve, HEADPHONES, TARGETS};
use joseon_headphones::{CurveInterpolator, HeadphoneModel, StressThresholds};
use std::sync::{Arc, Mutex};

/// Largest difference allowed in a curve value, in dB.
const TOLERANCE: f32 = 0.005;

struct Scenario {
    name: &'static str,
    curve: EmbeddedCurve,
    target: Option<EmbeddedCurve>,
    signal: (Vec<f32>, Vec<f32>),
    rate: f64,
    block: usize,
    checkpoints: &'static [usize],
    reset_at: Option<usize>,
}

fn scenarios() -> Vec<Scenario> {
    let harman = Some(embedded_curves::HARMAN_OVER_EAR2018);
    let short_loud = loud_signal(20.0, 8.0);
    vec![
        Scenario {
            name: "susvara",
            curve: embedded_curves::SUSVARA_UNVEILED,
            target: harman,
            signal: demo_block(0, 48_000 * 20, 48_000.0),
            rate: 48_000.0,
            block: 800,
            checkpoints: &[60, 170, 240, 600, 1_200],
            reset_at: None,
        },
        Scenario {
            name: "loud",
            curve: embedded_curves::AIR_PODS_PRO2,
            target: Some(embedded_curves::HARMAN_IN_EAR2019),
            signal: loud_signal(40.0, 33.0),
            rate: 48_000.0,
            block: 800,
            checkpoints: &[30, 200, 400, 1_900, 2_000, 2_050, 2_390],
            reset_at: Some(2_040),
        },
        Scenario {
            name: "short",
            curve: embedded_curves::HD600,
            target: harman,
            signal: short_loud.clone(),
            rate: 48_000.0,
            block: 800,
            checkpoints: &[120, 240, 470, 700, 900, 1_190],
            reset_at: None,
        },
        Scenario {
            name: "peaky",
            curve: embedded_curves::SONY_WH1000_XM5,
            target: harman,
            signal: short_loud,
            rate: 48_000.0,
            block: 800,
            checkpoints: &[120, 240, 470, 700, 900, 1_190],
            reset_at: None,
        },
        Scenario {
            name: "notarget",
            curve: embedded_curves::HD800S,
            target: None,
            signal: demo_block(0, 44_100 * 8, 44_100.0),
            rate: 44_100.0,
            block: 735,
            checkpoints: &[100, 480],
            reset_at: None,
        },
    ]
}

fn compare_flags(
    score: &mut Score,
    golden: &std::collections::HashMap<String, String>,
    tag: &str,
    flags: &[StressFlag],
) {
    score.same(
        &format!("{tag} flagCount"),
        &flags.len().to_string(),
        &golden[&format!("{tag} flagCount")],
    );
    for (i, flag) in flags.iter().enumerate() {
        let key = format!("{tag} flag{i}");
        let Some(want) = golden.get(&key) else {
            continue; // the count line has already failed
        };
        // "id severity low high" or "id severity none"
        let fields: Vec<&str> = want.split_whitespace().collect();
        score.same(&format!("{key} id"), &flag.id, fields[0]);
        score.same(
            &format!("{key} severity"),
            &(flag.severity as i32).to_string(),
            fields[1],
        );
        match &flag.frequency_range_hz {
            None => score.same(&format!("{key} range"), "none", fields[2]),
            Some(range) if fields.len() == 4 => {
                let theirs = [fields[2].parse().unwrap(), fields[3].parse().unwrap()];
                score.numbers(&format!("{key} range"), &[*range.start(), *range.end()], &theirs, 0.01);
            }
            Some(range) => score
                .failures
                .push(format!("{key}: a range {range:?}, the macOS app has none")),
        }
        // The texts are equal character for character.
        score.same(&format!("{key}.title"), &flag.title, &golden[&format!("{key}.title")]);
        score.same(
            &format!("{key}.detail"),
            &flag.detail,
            &golden[&format!("{key}.detail")],
        );
        score.same(
            &format!("{key}.plotLabel"),
            &flag.plot_label,
            &golden[&format!("{key}.plotLabel")],
        );
    }
}

#[test]
fn curve_numbers_match_the_macos_app() {
    let golden = golden("headphones.txt");
    let mut score = Score::default();
    for curve in HEADPHONES.iter().chain(&TARGETS) {
        let model = HeadphoneModel::new(curve.curve(), Some(embedded_curves::HARMAN_OVER_EAR2018.curve()));
        let context = model.stress_context();
        let peak = context.max_excess_over_target(4_000.0, 10_000.0);
        let (low, high) = curve.curve().range_hz();
        let ours = [
            curve.curve().reference_level_db(),
            context.mean_sub_bass_response_db().unwrap_or(-999.0),
            context.mean_shortfall_db(20.0, 40.0).unwrap_or(-999.0),
            peak.map_or(-999.0, |p| p.0),
            peak.map_or(-999.0, |p| p.1),
            low,
            high,
        ];
        let key = format!("curve [{}]", curve.name);
        // The key has spaces: find the line by its start.
        let line = golden_line(&key);
        score.numbers(&key, &ours, &floats(&line), 1e-5);
    }
    let grid = floats(&golden["interpolated grid"]);
    let hd600 = CurveInterpolator::new(embedded_curves::HD600.curve().normalized_to_1khz());
    score.numbers(
        "interpolated hd600",
        &hd600.levels(&grid),
        &floats(&golden["interpolated hd600"]),
        1e-5,
    );
    let lcd_x = CurveInterpolator::new(embedded_curves::LCD_X.curve());
    let single: Vec<f32> = grid.iter().map(|&hz| lcd_x.level(hz)).collect();
    score.numbers(
        "interpolated single",
        &single,
        &floats(&golden["interpolated single"]),
        1e-5,
    );
    score.finish();
}

/// A golden line whose key has spaces in it: the text after `key`.
fn golden_line(key: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/headphones.txt");
    let text = std::fs::read_to_string(path).unwrap();
    let line = text
        .lines()
        .find(|line| line.starts_with(key))
        .unwrap_or_else(|| panic!("no golden line {key}"));
    line[key.len()..].trim().to_string()
}

#[test]
fn model_and_stress_flags_match_the_macos_app() {
    let golden = golden("headphones.txt");
    let mut score = Score::default();
    for scenario in scenarios() {
        // The clock of the model is the audio clock, as in the reference run.
        let audio_seconds = Arc::new(Mutex::new(0.0f64));
        let clock = Arc::clone(&audio_seconds);
        let model = Arc::new(Mutex::new(HeadphoneModel::with(
            scenario.curve.curve(),
            scenario.target.map(|target| target.curve()),
            StressThresholds::default(),
            Box::new(move || *clock.lock().unwrap()),
        )));
        let engine = AnalysisEngine::new();
        engine.set_headphone_model(Some(model.clone()));
        let (left, right) = &scenario.signal;
        let mut marks = scenario.checkpoints.iter().copied().peekable();
        let mut position = 0;
        for (index, (l, r)) in left
            .chunks(scenario.block)
            .zip(right.chunks(scenario.block))
            .enumerate()
        {
            let block_index = index + 1;
            position += l.len();
            *audio_seconds.lock().unwrap() = position as f64 / scenario.rate;
            let frame = engine.process_now(l, r, scenario.rate);
            if scenario.reset_at == Some(block_index) {
                engine.reset_measurement();
            }
            if marks.peek() != Some(&block_index) {
                continue;
            }
            marks.next();
            let tag = format!("{}@{}", scenario.name, block_index);
            let reading = frame.headphone.as_ref().expect("the engine has a model");
            let want = |field: &str| golden[&format!("{tag} {field}")].as_str();
            score.same(
                &format!("{tag} model"),
                &format!("{} [{}]", reading.has_target, reading.model_name),
                want("model"),
            );
            score.numbers(
                &format!("{tag} response"),
                &reading.response_db,
                &floats(want("response")),
                1e-5,
            );
            score.numbers(
                &format!("{tag} target"),
                &reading.target_db,
                &floats(want("target")),
                1e-5,
            );
            score.numbers(
                &format!("{tag} predicted"),
                &reading.predicted_at_ear_db,
                &floats(want("predicted")),
                TOLERANCE,
            );
            compare_flags(&mut score, &golden, &tag, &reading.stress_flags);
            let mut onsets: Vec<(String, f64)> = model.lock().unwrap().flag_onsets().into_iter().collect();
            onsets.sort_by(|a, b| a.0.cmp(&b.0));
            let onsets: Vec<String> = onsets
                .iter()
                .map(|(id, time)| format!("{id}={}", *time as f32))
                .collect();
            score.same(&format!("{tag} onsets"), &onsets.join(" "), want("onsets"));
        }
        assert!(
            marks.peek().is_none(),
            "{}: a checkpoint was not reached",
            scenario.name
        );
    }
    score.finish();
}
