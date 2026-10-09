//! Parity of the loudness meter with the macOS app.
//!
//! `tests/golden/loudness.txt` holds what the Swift meter of the macOS app
//! gives for three fixed signals (`tools/golden` makes the file). This test
//! feeds the same signals to the Rust meter and compares every field.

use joseon_core::contracts::LoudnessMetering;
use joseon_core::loudness::LoudnessMeter;
use joseon_core::test_signals::{demo_block, pink_noise, sine};
use std::collections::HashMap;

/// Largest difference allowed in a level, in dB or LU.
const LEVEL_TOLERANCE: f32 = 0.001;

const FIELDS: [&str; 13] = [
    "momentary",
    "short-term",
    "integrated",
    "momentary max",
    "short-term max",
    "range",
    "true peak left",
    "true peak right",
    "true peak max",
    "rms left",
    "rms right",
    "plr",
    "psr",
];

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
}

fn scenarios() -> Vec<Scenario> {
    let (left, right) = demo_block(0, 48_000 * 40, 48_000.0);
    let demo48 = Scenario {
        name: "demo48",
        parts: vec![Part {
            left,
            right,
            rate: 48_000.0,
        }],
        block: 800,
        checkpoints: &[3, 50, 300, 1_200, 1_777, 2_400],
        reset_at: None,
    };

    let rate = 44_100.0;
    let tone = |hz: f64, db: f64, seconds: f64| sine(hz, 10f64.powf(db / 20.0) as f32, rate, seconds, 0.0);
    let mut left = [
        tone(1_000.0, -36.0, 6.0),
        tone(1_000.0, -23.0, 12.0),
        tone(1_000.0, -30.0, 8.0),
    ]
    .concat();
    let mut right = [
        tone(997.0, -30.0, 6.0),
        tone(997.0, -20.0, 12.0),
        tone(997.0, -36.0, 8.0),
    ]
    .concat();
    let noise = pink_noise(0.01, left.len(), 3);
    for i in 0..left.len() {
        left[i] += noise[i];
        right[i] -= noise[i];
    }
    let loud: Vec<f32> = sine(440.0, 2.0, rate, 2.0, 0.0)
        .iter()
        .map(|x| x.clamp(-1.0, 1.0))
        .collect();
    left.extend(&loud);
    right.extend(loud.iter().map(|x| x * 0.5));
    let tail = pink_noise(0.0001, (rate * 6.0) as usize, 4);
    left.extend(&tail);
    right.extend(&tail);
    let steps44 = Scenario {
        name: "steps44",
        parts: vec![Part { left, right, rate }],
        block: 441,
        checkpoints: &[100, 600, 1_800, 2_700, 2_810, 3_050, 3_400],
        reset_at: Some(2_750),
    };

    let a = demo_block(0, 48_000 * 5, 48_000.0);
    let b = demo_block(0, 96_000 * 5, 96_000.0);
    let c = sine(40_000.0, 0.7, 192_000.0, 3.0, 0.4);
    let silence = vec![0.0f32; 192_000 * 3];
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
                right: c.iter().map(|x| -x).collect(),
                left: c,
                rate: 192_000.0,
            },
            Part {
                left: silence.clone(),
                right: silence,
                rate: 192_000.0,
            },
        ],
        block: 4_096,
        checkpoints: &[40, 59, 100, 176, 200, 300, 350, 458],
        reset_at: None,
    };
    vec![demo48, steps44, switch]
}

fn golden() -> HashMap<String, String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/loudness.txt");
    let text = std::fs::read_to_string(path).expect("the golden file is in the repository");
    text.lines()
        .map(|line| {
            let mut parts = line.splitn(3, ' ');
            let key = format!("{} {}", parts.next().unwrap(), parts.next().unwrap());
            (key, parts.next().unwrap_or("").to_string())
        })
        .collect()
}

#[test]
fn loudness_matches_the_macos_app() {
    let golden = golden();
    let mut failures = Vec::new();
    let (mut worst, mut worst_at) = (0.0f32, String::new());
    let mut compared = 0;
    for scenario in scenarios() {
        let mut meter = LoudnessMeter::new();
        let mut marks = scenario.checkpoints.iter().copied().peekable();
        let mut block_index = 0;
        for part in &scenario.parts {
            for (left, right) in part.left.chunks(scenario.block).zip(part.right.chunks(scenario.block)) {
                meter.process(left, right, part.rate);
                block_index += 1;
                if scenario.reset_at == Some(block_index) {
                    meter.reset();
                }
                if marks.peek() != Some(&block_index) {
                    continue;
                }
                marks.next();
                let tag = format!("{}@{}", scenario.name, block_index);
                let r = meter.read();
                let ours = [
                    r.momentary_lufs,
                    r.short_term_lufs,
                    r.integrated_lufs,
                    r.momentary_max_lufs,
                    r.short_term_max_lufs,
                    r.loudness_range_lu,
                    r.true_peak_left_dbtp,
                    r.true_peak_right_dbtp,
                    r.true_peak_max_dbtp,
                    r.rms_left_db,
                    r.rms_right_db,
                    r.plr_db,
                    r.psr_db,
                ];
                let theirs: Vec<f32> = golden[&format!("{tag} levels")]
                    .split_whitespace()
                    .map(|v| v.parse().unwrap())
                    .collect();
                assert_eq!(theirs.len(), ours.len(), "{tag}");
                for ((field, a), b) in FIELDS.iter().zip(ours).zip(theirs) {
                    let difference = (a - b).abs();
                    compared += 1;
                    if difference > worst {
                        (worst, worst_at) = (difference, format!("{tag} {field}"));
                    }
                    if !(difference <= LEVEL_TOLERANCE) {
                        failures.push(format!("{tag} {field}: {a} vs {b}"));
                    }
                }
                let counts: Vec<&str> = golden[&format!("{tag} counts")].split_whitespace().collect();
                if r.clip_count != counts[0].parse::<u64>().unwrap() {
                    failures.push(format!("{tag} clip count: {} vs {}", r.clip_count, counts[0]));
                }
                if (r.measured_seconds - counts[1].parse::<f64>().unwrap()).abs() > 1e-9 {
                    failures.push(format!(
                        "{tag} measured seconds: {} vs {}",
                        r.measured_seconds, counts[1]
                    ));
                }
                if r.is_integrated_valid != (counts[2] == "true") {
                    failures.push(format!(
                        "{tag} integrated valid: {} vs {}",
                        r.is_integrated_valid, counts[2]
                    ));
                }
            }
        }
        assert!(
            marks.peek().is_none(),
            "{}: a checkpoint was not reached",
            scenario.name
        );
    }
    println!("{compared} levels compared. Largest difference from the macOS app: {worst} at {worst_at}");
    assert!(
        failures.is_empty(),
        "not the same as the macOS app:\n{}",
        failures.join("\n")
    );
}
