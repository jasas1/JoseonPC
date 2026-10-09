//! Parity of the level at the ear (SPL estimate and dose) with the macOS app.
//!
//! `tests/golden/spl.txt` holds what the Swift code of the macOS app gives
//! (`tools/golden` makes the file): the weighting and band numbers, the
//! sensitivity library, 1200 day and week stamps, and the reading of the
//! estimator on fixed signals through the engine.

#[path = "../../joseon-core/tests/common/mod.rs"]
mod common;

use common::{doubles, floats, golden, loud_signal, Score};
use joseon_core::contracts::{CalibrationMethod, PlaybackCalibration, SplEstimating, ThirdOctaveReading};
use joseon_core::engine::AnalysisEngine;
use joseon_core::test_signals::{demo_block, pink_noise, sine};
use joseon_headphones::calibration::{expected_spl, expected_spl_at, from_measured_tone, from_specs};
use joseon_headphones::embedded_curves::{self, HEADPHONES, TARGETS};
use joseon_headphones::spl_weighting::{a_weighting, third_octave_bands};
use joseon_headphones::{sensitivity, CurveInterpolator, LocalTime, SplDoseOptions, SplDoseState, SplEstimator};
use std::sync::{Arc, Mutex};

/// Largest difference allowed in a level, in dB. The band levels that come in
/// already differ from the macOS app by up to 0.0012 dB (the spectrum parity).
const LEVEL_TOLERANCE: f32 = 0.01;
/// Largest relative difference allowed in a dose or a time. 0.01 dB is 0.23 % of energy.
const DOSE_TOLERANCE: f64 = 0.003;
/// Numbers that come from the same formula on the same input.
const EXACT: f64 = 1e-12;

/// A golden line whose key has brackets and spaces: the text after `key`.
fn line(key: &str) -> String {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/spl.txt");
    let text = std::fs::read_to_string(path).unwrap();
    let start = format!("{key} ");
    let line = text
        .lines()
        .find(|line| line.starts_with(&start))
        .unwrap_or_else(|| panic!("no golden line {key}"));
    line[start.len()..].to_string()
}

#[test]
fn static_numbers_match_the_macos_app() {
    let golden = golden("spl.txt");
    let mut score = Score::default();
    let centers = ThirdOctaveReading::NOMINAL_CENTERS_HZ;
    let on_centers = |f: &dyn Fn(f64) -> f64| -> Vec<f64> { centers.iter().map(|&hz| f(hz as f64)).collect() };

    score.doubles(
        "aweight",
        &a_weighting::db_on(&centers),
        &doubles(&golden["spl aweight"]),
        EXACT,
    );
    score.doubles(
        "diffuse",
        &on_centers(&SplEstimator::diffuse_field_term_db),
        &doubles(&golden["spl diffuse"]),
        EXACT,
    );
    score.doubles(
        "exact",
        &on_centers(&third_octave_bands::exact_center_hz),
        &doubles(&golden["spl exact"]),
        EXACT,
    );
    score.doubles(
        "samples",
        &third_octave_bands::sample_frequencies(3_150.0),
        &doubles(&golden["spl samples"]),
        EXACT,
    );
    for (i, curve) in HEADPHONES.iter().chain(&TARGETS).enumerate() {
        let interpolator = CurveInterpolator::new(curve.curve().normalized_to_1khz());
        score.doubles(
            &format!("response of {}", curve.name),
            &on_centers(&|hz| third_octave_bands::mean_db(&interpolator, hz)),
            &doubles(&golden[&format!("spl response{i}")]),
            1e-6,
        );
    }
    for (name, s) in sensitivity::by_curve_name() {
        score.doubles(
            name,
            &[s.db_spl_per_volt, s.impedance_ohms],
            &doubles(&line(&format!("sensitivity [{name}]"))),
            EXACT,
        );
    }
    let hd650 = sensitivity::hd650();
    let c = from_specs("c", 2.1, 11.5, 23.7, true);
    score.doubles(
        "calibration",
        &[
            c.full_scale_vrms,
            c.uncertainty_db,
            from_measured_tone("t", 0.123, -18.5).full_scale_vrms,
            expected_spl(-17.3, &hd650, &c),
            expected_spl_at(-17.3, 5_700.0, &hd650, &c, &embedded_curves::HD800S.curve()),
        ],
        &doubles(&golden["spl calibration"]),
        1e-7,
    );
    let allowed: Vec<f64> = [70.0, 79.9, 85.0, 91.3, 104.0, 120.0]
        .iter()
        .map(|&level| SplDoseOptions::niosh_allowed_seconds(level))
        .collect();
    score.doubles("allowed", &allowed, &doubles(&golden["spl allowed"]), EXACT);
    score.finish();
}

/// The macOS app reads the day and the week from `Calendar`. This port does the
/// calendar math by hand, so it is checked on 1200 dates in four time zones.
#[test]
fn day_and_week_stamps_match_the_macos_calendar() {
    let golden = golden("spl.txt");
    let mut score = Score::default();
    for i in 0..1_200 {
        let text = &golden[&format!("stamp {i}")];
        let fields: Vec<&str> = text.split_whitespace().collect();
        let time = LocalTime::new(fields[0].parse().unwrap(), fields[1].parse().unwrap());
        score.same(&format!("day {text}"), &SplDoseState::day_stamp_for(time), fields[2]);
        score.same(&format!("week {text}"), &SplDoseState::week_stamp_for(time), fields[3]);
    }
    score.finish();
}

struct Scenario {
    name: &'static str,
    estimator: SplEstimator,
    signal: (Vec<f32>, Vec<f32>),
    rate: f64,
    block: usize,
    checkpoints: &'static [usize],
    reset_measurement_at: Option<usize>,
    reset_dose_at: Option<usize>,
}

fn estimator(
    curve: embedded_curves::EmbeddedCurve,
    sensitivity: joseon_core::contracts::HeadphoneSensitivity,
    calibration: PlaybackCalibration,
    options: SplDoseOptions,
) -> SplEstimator {
    SplEstimator::with(
        curve.curve(),
        sensitivity,
        calibration,
        options,
        Box::new(|| LocalTime::utc(1_789_992_000.0)),
    )
}

fn scenarios() -> Vec<Scenario> {
    let rate = 48_000.0;
    let n = (rate * 12.0) as usize;
    let mut left = pink_noise(0.5, n, 7);
    let mut right = sine(3_150.0, 0.4, rate, 12.0, 0.0);
    for i in (rate * 7.0) as usize..n {
        left[i] = 0.0;
        right[i] = 0.0;
    }
    vec![
        Scenario {
            name: "hd650",
            estimator: estimator(
                embedded_curves::HD650,
                sensitivity::hd650(),
                from_measured_tone("2 V source", 2.0, 0.0),
                SplDoseOptions::default(),
            ),
            signal: demo_block(0, 48_000 * 20, 48_000.0),
            rate,
            block: 800,
            checkpoints: &[50, 60, 240, 600, 1_200],
            reset_measurement_at: None,
            reset_dose_at: None,
        },
        Scenario {
            name: "loudx",
            estimator: estimator(
                embedded_curves::LCD_X,
                sensitivity::lcd_x(),
                from_specs("DAC + amp", 2.0, 6.0, 30.0, true),
                SplDoseOptions {
                    apply_niosh_threshold: false,
                },
            ),
            signal: loud_signal(40.0, 33.0),
            rate,
            block: 800,
            checkpoints: &[50, 200, 400, 1_001, 1_900, 2_000, 2_050, 2_390],
            reset_measurement_at: Some(2_040),
            reset_dose_at: Some(1_000),
        },
        Scenario {
            name: "quiet",
            estimator: estimator(
                embedded_curves::SONY_WH1000_XM5,
                sensitivity::sony_wh1000_xm5(),
                PlaybackCalibration {
                    name: "phone".to_string(),
                    method: CalibrationMethod::SystemVolume,
                    full_scale_vrms: 0.02,
                    uncertainty_db: 3.0,
                },
                SplDoseOptions::default(),
            ),
            signal: demo_block(0, 44_100 * 8, 44_100.0),
            rate: 44_100.0,
            block: 735,
            checkpoints: &[60, 100, 480],
            reset_measurement_at: None,
            reset_dose_at: None,
        },
        Scenario {
            name: "ears",
            estimator: estimator(
                embedded_curves::FOCAL_UTOPIA,
                sensitivity::focal_utopia(),
                from_measured_tone("bench", 0.05, -20.0),
                SplDoseOptions::default(),
            ),
            signal: (left, right),
            rate,
            block: 800,
            checkpoints: &[50, 200, 400, 425, 480, 700],
            reset_measurement_at: None,
            reset_dose_at: None,
        },
    ]
}

#[test]
fn estimator_and_dose_match_the_macos_app() {
    let golden = golden("spl.txt");
    let mut score = Score::default();
    for scenario in scenarios() {
        let name = scenario.name;
        score.doubles(
            &format!("{name} chain"),
            &[scenario.estimator.chain_offset_db()],
            &doubles(&golden[&format!("{name} chain")]),
            EXACT,
        );
        let estimator = Arc::new(Mutex::new(scenario.estimator));
        let engine = AnalysisEngine::new();
        engine.set_spl_estimator(Some(estimator.clone()));
        let (left, right) = &scenario.signal;
        let mut marks = scenario.checkpoints.iter().copied().peekable();
        for (index, (l, r)) in left
            .chunks(scenario.block)
            .zip(right.chunks(scenario.block))
            .enumerate()
        {
            let block_index = index + 1;
            let frame = engine.process_now(l, r, scenario.rate);
            if scenario.reset_measurement_at == Some(block_index) {
                engine.reset_measurement();
            }
            if scenario.reset_dose_at == Some(block_index) {
                estimator.lock().unwrap().reset_dose();
            }
            if marks.peek() != Some(&block_index) {
                continue;
            }
            marks.next();
            let tag = format!("{name}@{block_index}");
            let s = frame.spl.as_ref().expect("the engine has an estimator");
            let want = |field: &str| golden[&format!("{tag} {field}")].as_str();
            score.same(
                &format!("{tag} name"),
                &format!("{:?} [{}]", s.uncertainty_db, s.calibration_name),
                want("name"),
            );
            score.numbers(
                &format!("{tag} levels"),
                &[
                    s.level_a_fast,
                    s.level_a_slow,
                    s.level_z_eardrum,
                    s.leq_a_track,
                    s.leq_a_session,
                    s.max_a_fast,
                ],
                &floats(want("levels")),
                LEVEL_TOLERANCE,
            );
            score.numbers(
                &format!("{tag} bands"),
                &s.band_levels_eardrum,
                &floats(want("bands")),
                LEVEL_TOLERANCE,
            );
            score.doubles(
                &format!("{tag} dose"),
                &[s.dose_niosh as f64, s.dose_who_weekly as f64],
                &doubles(want("dose")),
                DOSE_TOLERANCE,
            );
            score.doubles(
                &format!("{tag} time"),
                &[s.dose_seconds, s.seconds_to_niosh_limit],
                &doubles(want("time")),
                DOSE_TOLERANCE,
            );
        }
        assert!(marks.peek().is_none(), "{name}: a checkpoint was not reached");
        let d = estimator.lock().unwrap().dose_state();
        score.doubles(
            &format!("{name} state"),
            &[
                d.niosh_dose,
                d.who_weekly_energy_seconds,
                d.dose_seconds,
                d.session_energy_seconds,
                d.session_seconds,
            ],
            &doubles(&golden[&format!("{name} state")]),
            DOSE_TOLERANCE,
        );
        // The clock of the scenario is Monday 2026-09-21, noon UTC.
        assert_eq!(d.day_stamp, "2026-09-21");
        assert_eq!(d.week_stamp, "2026-W39");
    }
    score.finish();
}
