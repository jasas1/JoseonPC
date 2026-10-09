//! Parity of the spectrum analyzer with the macOS app.
//!
//! `tests/golden/spectrum.txt` holds what the Swift analyzer of the macOS app
//! gives for three fixed signals (`tools/golden` makes the file). This test
//! feeds the same signals to the Rust analyzer and compares every number.

use joseon_core::contracts::{PeakReading, SpectrumAnalyzing, SpectrumSettings};
use joseon_core::spectrum::SpectrumAnalyzer;
use joseon_core::test_signals::{demo_block, pink_noise, sine};
use std::collections::HashMap;

struct Scenario {
    name: &'static str,
    left: Vec<f32>,
    right: Vec<f32>,
    rate: f64,
    block: usize,
    settings: SpectrumSettings,
    checkpoints: &'static [usize],
    reset_at: Option<usize>,
}

fn scenarios() -> Vec<Scenario> {
    let demo48 = demo_block(0, 48_000 * 8, 48_000.0);

    let (rate, seconds) = (44_100.0, 6.0);
    let n = (rate * seconds) as usize;
    let (mut left, mut right) = (vec![0.0f32; n], vec![0.0f32; n]);
    for (hz, amplitude, gain_left, gain_right) in [
        (31.5, 0.25, 1.0, 1.0),
        (220.0, 0.1, 1.0, 1.0),
        (1_000.0, 0.2, 1.0, 0.5),
        (3_520.0, 0.02, 0.0, 1.0),
        (12_000.0, 0.05, 1.0, -1.0),
    ] {
        let tone = sine(hz, amplitude, rate, seconds, 0.0);
        for i in 0..n {
            left[i] += tone[i] * gain_left;
            right[i] += tone[i] * gain_right;
        }
    }
    let (noise_left, noise_right) = (pink_noise(0.05, n, 7), pink_noise(0.05, n, 8));
    for i in 0..n {
        left[i] += noise_left[i];
        right[i] += noise_right[i];
    }
    let tones_settings = SpectrumSettings {
        tilt_db_per_octave: 4.5,
        display_bins: 600,
        min_hz: 20.0,
        max_hz: 20_000.0,
        ..Default::default()
    };

    let (mut left96, mut right96) = demo_block(0, 96_000 * 5, 96_000.0);
    left96.resize(96_000 * 7, 0.0);
    right96.resize(96_000 * 7, 0.0);

    vec![
        Scenario {
            name: "demo48",
            left: demo48.0,
            right: demo48.1,
            rate: 48_000.0,
            block: 800,
            settings: SpectrumSettings::default(),
            checkpoints: &[30, 120, 300, 480],
            reset_at: None,
        },
        Scenario {
            name: "tones44",
            left,
            right,
            rate,
            block: 441,
            settings: tones_settings,
            checkpoints: &[100, 300, 600],
            reset_at: Some(250),
        },
        Scenario {
            name: "demo96",
            left: left96,
            right: right96,
            rate: 96_000.0,
            block: 4_096,
            settings: SpectrumSettings::default(),
            checkpoints: &[60, 117, 130, 164],
            reset_at: None,
        },
    ]
}

fn golden() -> HashMap<String, String> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/spectrum.txt");
    let text = std::fs::read_to_string(path).expect("the golden file is in the repository");
    text.lines()
        .map(|line| {
            let mut parts = line.splitn(3, ' ');
            let key = format!("{} {}", parts.next().unwrap(), parts.next().unwrap());
            (key, parts.next().unwrap_or("").to_string())
        })
        .collect()
}

fn floats(text: &str) -> Vec<f32> {
    text.split_whitespace().map(|v| v.parse().expect("a float")).collect()
}

/// "hz level cents [name]"
fn peak(text: &str) -> (f32, f32, f32, String) {
    let (numbers, name) = text.split_once('[').expect("a peak");
    let v = floats(numbers);
    (v[0], v[1], v[2], name.trim_end_matches(']').to_string())
}

#[derive(Default)]
struct Report {
    lines: Vec<String>,
    failures: Vec<String>,
}

impl Report {
    /// Compares two curves in dB. `tolerance` is the largest difference allowed
    /// in any one value.
    fn curve(&mut self, key: &str, ours: &[f32], theirs: &[f32], tolerance: f32) {
        if ours.len() != theirs.len() {
            self.failures.push(format!(
                "{key}: {} values, the macOS app has {}",
                ours.len(),
                theirs.len()
            ));
            return;
        }
        let (mut worst, mut at, mut sum) = (0.0f32, 0, 0.0f64);
        for (i, (a, b)) in ours.iter().zip(theirs).enumerate() {
            let d = (a - b).abs();
            sum += d as f64;
            if d > worst || d.is_nan() {
                (worst, at) = (d, i);
            }
        }
        let mean = sum / ours.len().max(1) as f64;
        self.lines
            .push(format!("{key:<28} max {worst:>9.5} dB at {at:>4}   mean {mean:.6}"));
        if !(worst <= tolerance) {
            self.failures.push(format!(
                "{key}: {worst} dB at index {at} ({} vs {})",
                ours[at], theirs[at]
            ));
        }
    }

    fn peak(&mut self, key: &str, ours: &PeakReading, theirs: &str) {
        let (hz, level, cents, name) = peak(theirs);
        let cents_apart = if hz > 0.0 && ours.frequency_hz > 0.0 {
            1200.0 * (ours.frequency_hz / hz).log2().abs()
        } else {
            0.0
        };
        let same = ours.note_name == name
            && (ours.frequency_hz > 0.0) == (hz > 0.0)
            && cents_apart < 0.5
            && (ours.level_db - level).abs() < 0.05
            && (ours.cents - cents).abs() < 0.5;
        self.lines.push(format!(
            "{key:<28} {cents_apart:.4} cents, {:.5} dB apart",
            (ours.level_db - level).abs()
        ));
        if !same {
            self.failures.push(format!("{key}: {ours:?} vs {theirs}"));
        }
    }
}

#[test]
fn spectrum_matches_the_macos_app() {
    let golden = golden();
    let mut report = Report::default();
    for scenario in scenarios() {
        let mut analyzer = SpectrumAnalyzer::new(scenario.settings.clone());
        let mut marks = scenario.checkpoints.iter().copied().peekable();
        let blocks = scenario
            .left
            .chunks(scenario.block)
            .zip(scenario.right.chunks(scenario.block));
        for (index, (left, right)) in blocks.enumerate() {
            let block_index = index + 1;
            analyzer.process(left, right, scenario.rate);
            let out = analyzer.read();
            if scenario.reset_at == Some(block_index) {
                analyzer.reset();
            }
            if marks.peek() != Some(&block_index) {
                continue;
            }
            marks.next();
            let tag = format!("{}@{}", scenario.name, block_index);
            let want = |field: &str| {
                let key = format!("{tag} {field}");
                golden
                    .get(&key)
                    .unwrap_or_else(|| panic!("no golden line {key}"))
                    .as_str()
            };
            let spectrum = &out.spectrum;
            for (field, curve) in [
                ("left", &spectrum.left),
                ("right", &spectrum.right),
                ("mid", &spectrum.mid),
                ("side", &spectrum.side),
                ("peakHold", &spectrum.peak_hold),
                ("average", &spectrum.average),
            ] {
                report.curve(
                    &format!("{tag} {field}"),
                    curve,
                    &floats(want(field)),
                    CURVE_TOLERANCE_DB,
                );
            }
            for (i, layer) in spectrum.mid_layers.iter().enumerate() {
                report.curve(
                    &format!("{tag} layer{i}"),
                    &layer.levels_db,
                    &floats(want(&format!("layer{i}"))),
                    CURVE_TOLERANCE_DB,
                );
                assert_eq!(
                    layer.latency_seconds,
                    floats(want(&format!("latency{i}")))[0],
                    "{tag} latency{i}"
                );
            }
            report.curve(
                &format!("{tag} bands"),
                &out.bands.values(),
                &floats(want("bands")),
                LEVEL_TOLERANCE_DB,
            );
            report.peak(&format!("{tag} peak"), &out.peak, want("peak"));

            let top = analyzer.top_peaks();
            let want_top: Vec<&str> = want("topPeaks")
                .split(" | ")
                .filter(|entry| !entry.trim().is_empty())
                .collect();
            if top.len() != want_top.len() {
                report.failures.push(format!(
                    "{tag} topPeaks: {} entries, the macOS app has {}",
                    top.len(),
                    want_top.len()
                ));
            } else {
                for (i, (ours, theirs)) in top.iter().zip(want_top).enumerate() {
                    report.peak(&format!("{tag} topPeaks[{i}]"), ours, theirs);
                }
            }
            assert_eq!(
                analyzer.lowest_strong_hz(),
                floats(want("lowestStrongHz"))[0],
                "{tag} lowestStrongHz"
            );

            match (analyzer.third_octave(), want("thirdLeft")) {
                (None, "nil") => {}
                (Some(third), text) if text != "nil" => {
                    report.curve(
                        &format!("{tag} thirdLeft"),
                        &third.left,
                        &floats(text),
                        LEVEL_TOLERANCE_DB,
                    );
                    report.curve(
                        &format!("{tag} thirdRight"),
                        &third.right,
                        &floats(want("thirdRight")),
                        LEVEL_TOLERANCE_DB,
                    );
                }
                (ours, theirs) => report
                    .failures
                    .push(format!("{tag} thirdOctave: {} vs {theirs}", ours.is_some())),
            }
        }
        assert!(
            marks.peek().is_none(),
            "{}: a checkpoint was not reached",
            scenario.name
        );
    }
    println!("{}", report.lines.join("\n"));
    assert!(
        report.failures.is_empty(),
        "not the same as the macOS app:\n{}",
        report.failures.join("\n")
    );
}

/// Largest difference allowed in one display bin.
const CURVE_TOLERANCE_DB: f32 = 0.01;
/// Largest difference allowed in a band level.
const LEVEL_TOLERANCE_DB: f32 = 0.005;
