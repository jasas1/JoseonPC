//! Parity of the stereo analyzer with the macOS app.
//!
//! `tests/golden/stereo.txt` holds what the Swift analyzer of the macOS app
//! gives for three fixed signals (`tools/golden` makes the file). This test
//! feeds the same signals to the Rust analyzer and compares every field.

mod common;

use common::{floats, fnv, golden, Score};
use joseon_core::contracts::StereoAnalyzing;
use joseon_core::stereo::StereoAnalyzer;
use joseon_core::test_signals::{demo_block, pink_noise, sine, white_noise};
use std::f64::consts::PI;

/// Largest difference allowed in a correlation, balance or width.
const TOLERANCE: f32 = 1e-4;

struct Part {
    left: Vec<f32>,
    right: Vec<f32>,
    rate: f64,
}

struct Scenario {
    name: &'static str,
    parts: Vec<Part>,
    block: usize,
    checkpoints: &'static [usize],
    reset_at: Option<usize>,
    scope_points: usize,
}

fn scenarios() -> Vec<Scenario> {
    let (left, right) = demo_block(0, 48_000 * 12, 48_000.0);
    let demo48 = Scenario {
        name: "demo48",
        parts: vec![Part {
            left,
            right,
            rate: 48_000.0,
        }],
        block: 800,
        checkpoints: &[1, 2, 30, 120, 400, 720],
        reset_at: None,
        scope_points: 2048,
    };

    let (rate, seconds) = (44_100.0, 6.0);
    let n = (rate * seconds) as usize;
    let tone = |hz: f64, amplitude: f32, phase: f64| sine(hz, amplitude, rate, seconds, phase);
    let (bass, anti, right_only) = (tone(90.0, 0.3, 0.0), tone(5_000.0, 0.2, 0.0), tone(1_000.0, 0.25, 0.0));
    let (wide_l, wide_r) = (tone(3_000.0, 0.1, 0.0), tone(3_000.0, 0.1, PI / 3.0));
    let (noise_l, noise_r) = (pink_noise(0.02, n, 21), pink_noise(0.02, n, 22));
    let mut left: Vec<f32> = (0..n).map(|i| bass[i] + anti[i] + wide_l[i] + noise_l[i]).collect();
    let mut right: Vec<f32> = (0..n)
        .map(|i| bass[i] - anti[i] + right_only[i] + wide_r[i] + noise_r[i])
        .collect();
    let quiet = white_noise(0.000001, (rate * 2.0) as usize, 23);
    left.extend(&quiet);
    right.extend(quiet.iter().map(|x| -x));
    let mix44 = Scenario {
        name: "mix44",
        parts: vec![Part { left, right, rate }],
        block: 441,
        checkpoints: &[10, 150, 400, 590, 640, 780],
        reset_at: Some(420),
        scope_points: 500,
    };

    let a = demo_block(0, 48_000 * 3, 48_000.0);
    let b = demo_block(0, 96_000 * 3, 96_000.0);
    let switch = Scenario {
        name: "switch",
        parts: vec![
            Part {
                left: a.0,
                right: a.1,
                rate: 48_000.0,
            },
            Part {
                left: b.0,
                right: b.1,
                rate: 96_000.0,
            },
            Part {
                left: white_noise(1.0, 96_000, 31),
                right: white_noise(1.0, 96_000, 32),
                rate: 96_000.0,
            },
        ],
        block: 4_096,
        checkpoints: &[5, 36, 37, 80, 106, 130],
        reset_at: None,
        scope_points: 8_192,
    };
    vec![demo48, mix44, switch]
}

#[test]
fn stereo_matches_the_macos_app() {
    let golden = golden("stereo.txt");
    let mut score = Score::default();
    for scenario in scenarios() {
        let mut analyzer = StereoAnalyzer::new();
        analyzer.set_scope_point_count(scenario.scope_points);
        let mut marks = scenario.checkpoints.iter().copied().peekable();
        let mut block_index = 0;
        for part in &scenario.parts {
            for (left, right) in part.left.chunks(scenario.block).zip(part.right.chunks(scenario.block)) {
                analyzer.process(left, right, part.rate);
                block_index += 1;
                if scenario.reset_at == Some(block_index) {
                    analyzer.reset();
                }
                if marks.peek() != Some(&block_index) {
                    continue;
                }
                marks.next();
                let tag = format!("{}@{}", scenario.name, block_index);
                let want = |field: &str| golden[&format!("{tag} {field}")].as_str();
                let r = analyzer.read();
                score.numbers(
                    &format!("{tag} broadband"),
                    &[r.correlation, r.balance, r.width],
                    &floats(want("broadband")),
                    TOLERANCE,
                );
                score.numbers(
                    &format!("{tag} bandCorrelation"),
                    &r.band_correlation,
                    &floats(want("bandCorrelation")),
                    TOLERANCE,
                );
                score.numbers(
                    &format!("{tag} bandBalance"),
                    &r.band_balance,
                    &floats(want("bandBalance")),
                    TOLERANCE,
                );
                let active: Vec<&str> = r.band_active.iter().map(|&on| if on { "1" } else { "0" }).collect();
                score.same(&format!("{tag} bandActive"), &active.join(" "), want("bandActive"));
                // The points come straight from the samples, so they are equal bit for bit.
                let scope = format!(
                    "{} {}",
                    r.scope_points.len(),
                    fnv(r.scope_points.iter().flatten().copied())
                );
                score.same(&format!("{tag} scope"), &scope, want("scope"));
            }
        }
        assert!(
            marks.peek().is_none(),
            "{}: a checkpoint was not reached",
            scenario.name
        );
    }
    score.finish();
}
