//! Parity of the headphone measurement (sweep, analysis, result) with the macOS app.
//!
//! `tests/golden/measurement.txt` holds what the Swift code of the macOS app
//! gives (`tools/golden` makes the file) on fixed recordings: the stimuli, the
//! parts of the analysis, four full sessions, an average of two sides, the
//! leak test, and every sentence the result page shows.

#[path = "../../joseon-core/tests/common/mod.rs"]
mod common;

use common::{doubles, floats, Lines, Score};
use joseon_core::test_signals::pink_noise;
use joseon_headphones::calibration::from_measured_tone;
use joseon_headphones::embedded_curves;
use joseon_headphones::measurement::sweep_analysis::{self as analysis, standard_grid};
use joseon_headphones::measurement::{
    AbsoluteLevel, CouplerCorrection, HeadphoneMeasurement, MeasuredHeadphone, MeasurementRig, MicCalibration,
    OctaveSmoothing, Options, PinkNoiseOptions, Reference, SensitivityContext, SweepOptions, SweepSignal,
};
use std::f64::consts::PI;

/// Largest difference allowed in a level, in dB. The FFT library is not the
/// one the macOS app uses, and the result is stored as `f32`.
const LEVEL_TOLERANCE: f32 = 0.002;
/// Largest relative difference allowed in an `f64` result of the analysis.
const ANALYSIS_TOLERANCE: f64 = 1e-6;
/// Numbers that come from the same formula on the same input.
const EXACT: f64 = 1e-12;

// A fixed "headphone": three biquads, the same as in `tools/golden`.

#[derive(Clone, Copy)]
struct Biquad {
    b0: f64,
    b1: f64,
    b2: f64,
    a1: f64,
    a2: f64,
}

impl Biquad {
    fn high_pass(hz: f64, q: f64, rate: f64) -> Self {
        let w = 2.0 * PI * hz / rate;
        let (alpha, c) = (w.sin() / (2.0 * q), w.cos());
        let a0 = 1.0 + alpha;
        Self {
            b0: (1.0 + c) / 2.0 / a0,
            b1: -(1.0 + c) / a0,
            b2: (1.0 + c) / 2.0 / a0,
            a1: -2.0 * c / a0,
            a2: (1.0 - alpha) / a0,
        }
    }

    fn first_order_high_pass(hz: f64, rate: f64) -> Self {
        let k = (PI * hz / rate).tan();
        Self {
            b0: 1.0 / (1.0 + k),
            b1: -1.0 / (1.0 + k),
            b2: 0.0,
            a1: (k - 1.0) / (1.0 + k),
            a2: 0.0,
        }
    }

    fn peaking(hz: f64, gain_db: f64, q: f64, rate: f64) -> Self {
        let a = 10.0f64.powf(gain_db / 40.0);
        let w = 2.0 * PI * hz / rate;
        let (alpha, c) = (w.sin() / (2.0 * q), w.cos());
        let a0 = 1.0 + alpha / a;
        Self {
            b0: (1.0 + alpha * a) / a0,
            b1: -2.0 * c / a0,
            b2: (1.0 - alpha * a) / a0,
            a1: -2.0 * c / a0,
            a2: (1.0 - alpha / a) / a0,
        }
    }
}

fn filter(x: &[f64], stages: &[Biquad]) -> Vec<f64> {
    let mut y = x.to_vec();
    for s in stages {
        let (mut x1, mut x2, mut y1, mut y2) = (0.0, 0.0, 0.0, 0.0);
        for v in &mut y {
            let input = *v;
            let output = s.b0 * input + s.b1 * x1 + s.b2 * x2 - s.a1 * y1 - s.a2 * y2;
            x2 = x1;
            x1 = input;
            y2 = y1;
            y1 = output;
            *v = output;
        }
    }
    y
}

/// One recording: a delay, the filtered stimulus (with a little second harmonic), a tail, and pink noise.
fn recording(stimulus: &[f64], stages: &[Biquad], gain: f64, noise: f32, seed: u64) -> Vec<f64> {
    let (delay, tail) = (3_733, 6_000);
    let driven: Vec<f64> = stimulus.iter().map(|v| v + 0.02 * v * v).collect();
    let x = filter(&driven, stages);
    let mut out = vec![0.0; delay + x.len() + tail];
    for (i, v) in x.iter().enumerate() {
        out[delay + i] = v * gain;
    }
    let n = pink_noise(noise, out.len(), seed);
    for (v, n) in out.iter_mut().zip(&n) {
        *v += *n as f64;
    }
    out
}

fn compare_measured(score: &mut Score, golden: &Lines, tag: &str, m: &MeasuredHeadphone) {
    let want = |field: &str| golden.get(&format!("{tag} {field}"));
    let q = &m.quality;
    score.numbers(
        &format!("{tag} levels"),
        &m.curve.levels_db,
        &floats(want("levels")),
        LEVEL_TOLERANCE,
    );
    score.numbers(
        &format!("{tag} absolute"),
        &m.absolute_magnitude_db,
        &floats(want("absolute")),
        LEVEL_TOLERANCE,
    );
    // The signal-to-noise ratio is a difference of two estimates, one of them of noise.
    score.numbers(&format!("{tag} snr"), &q.snr_db, &floats(want("snr")), 0.01);
    score.numbers(
        &format!("{tag} spread"),
        &q.agreement_spread_db,
        &floats(want("spread")),
        LEVEL_TOLERANCE,
    );
    score.numbers(
        &format!("{tag} quality"),
        &[
            q.thd_percent,
            q.agreement_db,
            q.lowest_resolved_hz,
            q.noise_limited_hz,
            q.highest_reliable_hz,
            q.window_milliseconds,
            q.lowest_reliable_hz(),
            q.runs as f32,
        ],
        &floats(want("quality")),
        LEVEL_TOLERANCE,
    );
    let cause = format!("{:?}", q.low_limit_cause()).to_lowercase();
    score.same(&format!("{tag} cause"), &cause, want("cause"));
    score.same(&format!("{tag} method"), &m.method, want("method"));
    score.texts(
        &format!("{tag} warning"),
        &q.warnings,
        &golden.texts(&format!("{tag} warning")),
    );
    score.same(
        &format!("{tag} note"),
        &m.bass_roll_off_note().unwrap_or("none".to_string()),
        want("note"),
    );
    let csv = m.csv();
    let rows: Vec<&str> = csv.split('\n').filter(|line| !line.is_empty()).collect();
    let head: Vec<String> = rows
        .iter()
        .filter(|line| line.starts_with('#') || line.starts_with("frequency"))
        .map(|line| line.to_string())
        .collect();
    score.texts(
        &format!("{tag} csvhead"),
        &head,
        &golden.texts(&format!("{tag} csvhead")),
    );
    // The count of rows is exact. The two sample rows are compared as numbers.
    let theirs = want("csvrows");
    score.same(
        &format!("{tag} csvrows count"),
        &rows.len().to_string(),
        theirs.split(' ').next().unwrap(),
    );
    let row_numbers = |text: &str| -> Vec<f32> {
        text.split(['[', ']', ',', ' '])
            .filter(|part| !part.is_empty())
            .map(|part| part.parse().unwrap())
            .collect()
    };
    let ours = format!("{} [{}] [{}]", rows.len(), rows[rows.len() - 1], rows[rows.len() - 120]);
    score.numbers(
        &format!("{tag} csvrows"),
        &row_numbers(&ours),
        &row_numbers(theirs),
        LEVEL_TOLERANCE,
    );

    match &m.derived_sensitivity {
        Some(d) => {
            score.doubles(
                &format!("{tag} derived"),
                &[
                    d.sensitivity.db_spl_per_volt,
                    d.sensitivity.impedance_ohms,
                    d.uncertainty_db,
                    d.measured_spl_at_1khz,
                    d.drive_volts_rms,
                    d.printed_uncertainty_db(),
                ],
                &doubles(want("derived")),
                1e-5,
            );
            score.same(
                &format!("{tag} headline"),
                &format!(
                    "{} [{}] [{}]",
                    d.uncertainty_headline(),
                    d.dominant_term().name,
                    d.rig.as_str()
                ),
                want("headline"),
            );
            score.same(&format!("{tag} source"), &d.sensitivity.source, want("source"));
            let budget: Vec<String> = d
                .uncertainty_table()
                .into_iter()
                .map(|(term, value, detail)| format!("{term} | {value} | {detail}"))
                .collect();
            score.texts(
                &format!("{tag} budget"),
                &budget,
                &golden.texts(&format!("{tag} budget")),
            );
        }
        None => score.same(&format!("{tag} headline"), "none", want("headline")),
    }
}

#[test]
fn stimuli_match_the_macos_app() {
    let golden = Lines::of("measurement.txt");
    let mut score = Score::default();
    let sweep = SweepSignal::exponential_sweep(
        48_000.0,
        SweepOptions {
            seconds: 3.0,
            ..Default::default()
        },
    );
    let pink = SweepSignal::periodic_pink_noise(
        48_000.0,
        PinkNoiseOptions {
            seconds: 2.0,
            seed: 42,
            ..Default::default()
        },
    );
    let odd = SweepSignal::exponential_sweep(
        44_100.0,
        SweepOptions {
            start_hz: 10.0,
            end_hz: 30_000.0,
            seconds: 2.5,
            level_dbfs: -12.0,
            fade_in_seconds: 0.05,
            fade_out_seconds: 0.1,
        },
    );
    for (tag, s) in [("sweep", &sweep), ("pink", &pink), ("sweep441", &odd)] {
        score.doubles(
            &format!("stimulus {tag}"),
            &[
                s.samples.len() as f64,
                s.start_hz,
                s.end_hz,
                s.duration_seconds,
                s.highest_full_energy_hz(),
                s.sweep_rate_seconds(),
                s.rms_level_dbfs(),
                s.harmonic_time_offset_seconds(2),
                s.harmonic_band_coverage(3),
                s.fade_in_seconds,
                s.fade_out_seconds,
            ],
            &doubles(golden.get(&format!("stimulus {tag}"))),
            1e-9,
        );
        let ours: Vec<f64> = s.samples.iter().step_by(997).copied().collect();
        // The sweep is one formula per sample. The pink noise goes through an inverse FFT.
        // The samples come through `sin` and `exp` of the system math library: on Windows the sweep differs
        // from the macOS one by 1e-11 of a sample.
        let tolerance = 1e-9;
        score.doubles(
            &format!("samples {tag}"),
            &ours,
            &doubles(golden.get(&format!("samples {tag}"))),
            tolerance,
        );
    }
    let inverse: Vec<f64> = sweep.inverse_filter().unwrap().into_iter().step_by(1_999).collect();
    score.doubles(
        "samples inverse",
        &inverse,
        &doubles(golden.get("samples inverse")),
        1e-9,
    );
    score.finish();
}

#[test]
fn analysis_and_results_match_the_macos_app() {
    let golden = Lines::of("measurement.txt");
    let mut score = Score::default();
    let rate = 48_000.0;
    let headphone = [
        Biquad::high_pass(35.0, 0.707, rate),
        Biquad::peaking(3_000.0, 6.0, 2.0, rate),
        Biquad::peaking(8_000.0, -8.0, 3.0, rate),
    ];
    let sweep = SweepSignal::exponential_sweep(
        rate,
        SweepOptions {
            seconds: 3.0,
            ..Default::default()
        },
    );
    let options = Options::default();
    let grid = standard_grid();

    // The parts of the analysis, on one recording.
    let one = recording(&sweep.samples, &headphone, 0.5, 0.004, 11);
    let delay = analysis::delay_samples(&one, &sweep.samples);
    score.doubles(
        "parts delay",
        &[delay.samples as f64, delay.sub_sample, delay.confidence],
        &doubles(golden.get("parts delay")),
        ANALYSIS_TOLERANCE,
    );
    let ir = analysis::impulse_response(&one, &sweep, &options);
    let offset = |order: usize| ir.harmonic_offset_samples.get(&order).copied().unwrap_or(0) as f64;
    score.doubles(
        "parts ir",
        &[
            ir.samples.len() as f64,
            ir.peak_index as f64,
            ir.peak_sub_sample,
            ir.noise_floor_db_relative_peak,
            ir.delay_seconds(),
            offset(2),
            offset(3),
            offset(5),
        ],
        &doubles(golden.get("parts ir")),
        ANALYSIS_TOLERANCE,
    );
    let around: Vec<f64> = (0..240).map(|i| ir.samples[ir.peak_index + i - 40]).collect();
    score.doubles(
        "parts irsamples",
        &around,
        &doubles(golden.get("parts irsamples")),
        1e-9,
    );
    let windowed = analysis::window(&ir, &options);
    score.doubles(
        "parts window",
        &[
            windowed.pre_samples as f64,
            windowed.post_samples as f64,
            windowed.peak_offset as f64,
            windowed.lowest_resolved_hz(),
        ],
        &doubles(golden.get("parts window")),
        EXACT,
    );
    let thd = analysis::harmonic_distortion(&ir, &sweep, &options);
    let or_minus = |value: Option<&f64>| value.copied().unwrap_or(-1.0);
    score.doubles(
        "parts thd",
        &[
            thd.total_percent,
            or_minus(thd.per_harmonic_percent.get(&2)),
            or_minus(thd.per_harmonic_percent.get(&3)),
            or_minus(thd.band_coverage.get(&2)),
            or_minus(thd.band_coverage.get(&3)),
        ],
        &doubles(golden.get("parts thd")),
        ANALYSIS_TOLERANCE,
    );
    for (i, smoothing) in OctaveSmoothing::ALL.into_iter().enumerate() {
        score.doubles(
            &format!("parts magnitude{i}"),
            &analysis::magnitude_response(&windowed, &grid, smoothing, &options),
            &doubles(golden.get(&format!("parts magnitude{i}"))),
            ANALYSIS_TOLERANCE,
        );
    }

    // 1. A full session: three runs, a noise segment, a microphone file, a coupler file, a calibrator.
    let mic_text = "\"Sens Factor =-1.20dB, SERNO: 7012345\"\r\n10\t0\t0\r\n100\t0.3\t0\r\n500\t-0.2\t0\r\n1000\t0\t0\r\n3000\t0.8\t0\r\n10000\t3.0\t0\r\n20000\t6.0\t0";
    let mic = MicCalibration::parse(mic_text, "UMIK-1 7012345 (0°)", "").unwrap();
    let coupler = CouplerCorrection::parse(
        "# plate\n20,3.0\n100,2.0\n1000,0.0\n8000,-4.0\n20000,-6.0",
        "Flat plate",
        "",
    )
    .unwrap();
    score.doubles(
        "mic correction",
        &mic.correction_db(&grid),
        &doubles(golden.get("mic correction")),
        EXACT,
    );
    score.doubles(
        "mic header",
        &[
            mic.sens_factor_db.unwrap_or(-999.0),
            mic.mic_sensitivity_dbfs_per_pascal().unwrap_or(-999.0),
            mic.has_phase_column as u8 as f64,
            mic.frequencies_hz.len() as f64,
        ],
        &doubles(golden.get("mic header")),
        EXACT,
    );
    let playback = from_measured_tone("WA33 at 10 o'clock", 0.1, -6.0);
    let silence = vec![0.0; sweep.samples.len()];
    let mut full = |stages: &[Biquad], noise: f32, seeds: &[u64], rig: MeasurementRig, level: Reference| {
        let mut session = HeadphoneMeasurement::with(
            sweep.clone(),
            Options::default(),
            Some(mic.clone()),
            Some(coupler.clone()),
            grid.clone(),
        );
        session.set_noise_segment(recording(&silence, stages, 0.5, noise, 900));
        for (i, &seed) in seeds.iter().enumerate() {
            let s = session.add_run(&recording(&sweep.samples, stages, 0.5, noise, seed), i == 0);
            let key = format!("run {}x{seed}", seeds.len());
            score.doubles(
                &key,
                &[
                    s.delay_samples as f64,
                    s.delay_seconds,
                    s.cross_correlation_delay_samples.map_or(-1.0, |v| v as f64),
                    s.peak_level_db,
                    s.noise_floor_db,
                    s.thd_percent,
                    s.clipped as u8 as f64,
                ],
                &doubles(golden.get(&key)),
                ANALYSIS_TOLERANCE,
            );
        }
        let mut context = SensitivityContext::new(playback.clone(), AbsoluteLevel::for_reference(&level), 45.0);
        context.rig = rig;
        session
            .result("Golden headphone (measured)", "Joseon measurement", Some(&context))
            .unwrap()
    };
    let left = full(
        &headphone,
        0.004,
        &[11, 12, 13],
        MeasurementRig::FlatPlate,
        Reference::calibrator(-32.04),
    );

    // 2. The same headphone with a seal leak, in a noisy room, on an ear simulator, by the Sens Factor route.
    let mut leaky = headphone.to_vec();
    leaky.push(Biquad::first_order_high_pass(120.0, rate));
    let right = full(
        &leaky,
        0.05,
        &[21, 22],
        MeasurementRig::EarSimulator,
        Reference::from_mic_calibration(Some(&mic)),
    );
    compare_measured(&mut score, &golden, "full", &left);
    compare_measured(&mut score, &golden, "leaky", &right);
    compare_measured(
        &mut score,
        &golden,
        "average",
        &MeasuredHeadphone::average(&left, &right),
    );
    for (tag, reference) in [("hd650", embedded_curves::HD650), ("flat", embedded_curves::FLAT)] {
        let a = left.bass_shortfall(&reference.curve()).unwrap();
        let b = right.bass_shortfall(&reference.curve()).unwrap();
        let numbers = |s: &joseon_headphones::measurement::BassShortfall| {
            [
                s.mean_difference_db,
                s.low_hz,
                s.high_hz,
                s.points_used as f64,
                s.likely_leak as u8 as f64,
            ]
        };
        score.doubles(
            &format!("shortfall {tag}"),
            &[numbers(&a), numbers(&b)].concat(),
            &doubles(golden.get(&format!("shortfall {tag}"))),
            1e-4,
        );
        score.texts(
            &format!("shortfall {tag}text"),
            &[a.summary, b.summary],
            &golden.texts(&format!("shortfall {tag}text")),
        );
    }

    // 3. One bare run: no files, no noise segment, no reference, a clipped sample.
    {
        let mut session = HeadphoneMeasurement::new(sweep.clone());
        let mut loud = recording(&sweep.samples, &leaky, 0.5, 0.004, 31);
        loud[10_000] = 1.0;
        session.add_run(&loud, false);
        let bare = session.result("bare", "Joseon measurement", None).unwrap();
        compare_measured(&mut score, &golden, "bare", &bare);
        let none = SensitivityContext::new(
            playback.clone(),
            AbsoluteLevel::for_reference(&Reference::from_mic_calibration(None)),
            45.0,
        );
        let method = |reference: Reference| AbsoluteLevel::for_reference(&reference).scale().unwrap().method.clone();
        score.texts(
            "bare unavailable",
            &[
                session
                    .derived_sensitivity(&none)
                    .unavailable_reason()
                    .unwrap_or("derived")
                    .to_string(),
                AbsoluteLevel::for_reference(&Reference::calibrator(3.0))
                    .unavailable_reason()
                    .unwrap_or("available")
                    .to_string(),
                method(Reference::calibrator(-31.257)),
                method(Reference::mic_sensitivity(-19.2)),
            ],
            &golden.texts("bare unavailable"),
        );
    }

    // 4. Pink noise as the stimulus, 1/6 octave, four periods.
    {
        let pink = SweepSignal::periodic_pink_noise(
            rate,
            PinkNoiseOptions {
                seconds: 2.0,
                seed: 42,
                ..Default::default()
            },
        );
        let options = Options {
            smoothing: OctaveSmoothing::Sixth,
            ..Default::default()
        };
        let mut session = HeadphoneMeasurement::with(pink.clone(), options, None, None, grid.clone());
        session.set_noise_segment(recording(&vec![0.0; pink.samples.len()], &headphone, 0.5, 0.002, 901));
        for seed in [41, 42, 43, 44] {
            session.add_run(&recording(&pink.samples, &headphone, 0.5, 0.002, seed), false);
        }
        let measured = session.result("pink", "Joseon measurement", None).unwrap();
        compare_measured(&mut score, &golden, "pinkrun", &measured);
    }
    score.finish();
}
