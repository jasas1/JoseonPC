//! Parity of the analysis engine and the session record with the macOS app.
//!
//! `tests/golden/engine.txt` holds what the Swift engine of the macOS app
//! leaves in its session record after two fixed signals (`tools/golden` makes
//! the file). This test runs the same signals through the Rust engine, one
//! `process_now` per block, and compares samples, events and counts.

mod common;

use common::{floats, golden, Score};
use joseon_core::engine::AnalysisEngine;
use joseon_core::test_signals::{demo_block, sine};

/// Largest difference allowed in a number of a session sample (dB, or a correlation).
const TOLERANCE: f32 = 0.01;

struct Scenario {
    name: &'static str,
    left: Vec<f32>,
    right: Vec<f32>,
    block: usize,
    reset_at: &'static [usize],
    sleeps: bool,
}

fn scenarios() -> Vec<Scenario> {
    let rate = 48_000.0;
    let (left, right) = demo_block(0, 48_000 * 12, rate);
    let demo = Scenario {
        name: "demo",
        left,
        right,
        block: 800,
        reset_at: &[400],
        sleeps: false,
    };

    let tone = sine(440.0, 0.5, rate, 3.0, 0.0);
    let hot: Vec<f32> = sine(440.0, 2.0, rate, 1.5, 0.0)
        .iter()
        .map(|x| x.clamp(-1.0, 1.0))
        .collect();
    let silence = vec![0.0f32; (rate * 14.0) as usize];
    let left = [tone.clone(), hot.clone(), silence.clone(), tone.clone()].concat();
    let half: Vec<f32> = tone.iter().map(|x| x * 0.5).collect();
    let right = [half, hot, silence, tone].concat();
    let sleep = Scenario {
        name: "sleep",
        left,
        right,
        block: 4_800,
        reset_at: &[],
        sleeps: true,
    };
    vec![demo, sleep]
}

#[test]
fn engine_and_session_record_match_the_macos_app() {
    let golden = golden("engine.txt");
    let mut score = Score::default();
    for scenario in scenarios() {
        let name = scenario.name;
        let engine = AnalysisEngine::new();
        engine.set_sleeps_in_process_now(scenario.sleeps);
        let (mut blocks, mut silent_frames, mut asleep_blocks) = (0, 0, 0);
        let mut last = engine.latest_frame();
        for (left, right) in scenario
            .left
            .chunks(scenario.block)
            .zip(scenario.right.chunks(scenario.block))
        {
            last = engine.process_now(left, right, 48_000.0);
            blocks += 1;
            silent_frames += last.is_silent as usize;
            asleep_blocks += engine.is_asleep() as usize;
            if scenario.reset_at.contains(&blocks) {
                engine.reset_measurement();
            }
        }
        let snapshot = engine.session().snapshot();
        let counts = format!(
            "{blocks} {silent_frames} {asleep_blocks} {} {} {}",
            snapshot.samples.len(),
            snapshot.events.len(),
            last.is_silent
        );
        score.same(&format!("{name} counts"), &counts, &golden[&format!("{name} counts")]);
        score.numbers(
            &format!("{name} lowestStrongHz"),
            &[last.lowest_strong_hz],
            &floats(&golden[&format!("{name} lowestStrongHz")]),
            0.0,
        );
        for (i, sample) in snapshot.samples.iter().enumerate() {
            let Some(want) = golden.get(&format!("{name} sample{i}")) else {
                continue; // the count line has already failed
            };
            let mut ours = vec![
                sample.time as f32,
                sample.momentary_max_lufs,
                sample.short_term_lufs,
                sample.true_peak_dbtp,
                sample.correlation,
            ];
            ours.extend(sample.bands);
            ours.push(sample.is_silent as u8 as f32);
            score.numbers(&format!("{name} sample{i}"), &ours, &floats(want), TOLERANCE);
        }
        for (i, event) in snapshot.events.iter().enumerate() {
            let Some(want) = golden.get(&format!("{name} event{i}")) else {
                continue;
            };
            // "id kind time value [label]"
            let (numbers, label) = want.split_once(" [").expect("an event line");
            let fields: Vec<&str> = numbers.split_whitespace().collect();
            let key = format!("{name} event{i}");
            score.same(&format!("{key} id"), &event.id.to_string(), fields[0]);
            score.same(&format!("{key} kind"), event.kind.as_str(), fields[1]);
            score.numbers(&format!("{key} time"), &[event.time as f32], &floats(fields[2]), 1e-4);
            score.numbers(&format!("{key} value"), &[event.value], &floats(fields[3]), TOLERANCE);
            score.same(&format!("{key} label"), &event.label, label.trim_end_matches(']'));
        }
    }
    score.finish();
}
