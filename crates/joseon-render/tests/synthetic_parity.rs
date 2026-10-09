//! Parity of the synthetic frames with the macOS app. The review pictures and
//! the demo mode draw from these frames, so a picture of this port and a
//! picture of the macOS app show the same data.
//!
//! `tests/golden/synthetic.txt` holds what the Swift code gives (`tools/golden`).

#[path = "../../joseon-core/tests/common/mod.rs"]
mod common;

use common::{floats, Lines, Score};
use joseon_core::contracts::AnalysisFrame;
use joseon_render::synthetic::{adding_demo_spl, Options, SyntheticFrames};

/// A level may differ by this much in dB: `sin`, `exp`, `pow` and `log2` in
/// `f32` come from the system math library, and the frames feed back on
/// themselves (release, averages).
const TOLERANCE: f32 = 0.01;

fn every(step: usize, v: &[f32]) -> Vec<f32> {
    v.iter().step_by(step).copied().collect()
}

fn peak_text(score: &mut Score, key: &str, hz: f32, db: f32, cents: f32, name: &str, theirs: &str) {
    let (numbers, their_name) = theirs.split_once(" [").unwrap();
    score.numbers(&format!("{key} numbers"), &[hz, db], &floats(numbers)[..2], TOLERANCE);
    // Cents are 100 times a difference of two logarithms.
    score.numbers(&format!("{key} cents"), &[cents], &floats(numbers)[2..], 0.05);
    score.same(&format!("{key} name"), name, their_name.trim_end_matches(']'));
}

fn compare_frame(score: &mut Score, golden: &Lines, tag: &str, f: &AnalysisFrame) {
    let want = |field: &str| golden.get(&format!("{tag} {field}"));
    let s = &f.spectrum;
    for (name, v) in [
        ("mid", &s.mid),
        ("left", &s.left),
        ("right", &s.right),
        ("side", &s.side),
        ("hold", &s.peak_hold),
        ("average", &s.average),
    ] {
        score.numbers(&format!("{tag} {name}"), &every(5, v), &floats(want(name)), TOLERANCE);
    }
    peak_text(
        score,
        &format!("{tag} peak"),
        f.peak.frequency_hz,
        f.peak.level_db,
        f.peak.cents,
        &f.peak.note_name,
        want("peak"),
    );
    let top = golden.texts(&format!("{tag} top"));
    score.same(
        &format!("{tag} top count"),
        &f.top_peaks.len().to_string(),
        &top.len().to_string(),
    );
    for (i, (p, theirs)) in f.top_peaks.iter().zip(&top).enumerate() {
        peak_text(
            score,
            &format!("{tag} top {i}"),
            p.frequency_hz,
            p.level_db,
            p.cents,
            &p.note_name,
            theirs,
        );
    }
    let l = &f.loudness;
    let mut numbers = f.bands.values().to_vec();
    numbers.extend([
        l.momentary_lufs,
        l.short_term_lufs,
        l.integrated_lufs,
        l.momentary_max_lufs,
        l.short_term_max_lufs,
        l.loudness_range_lu,
        l.true_peak_left_dbtp,
        l.true_peak_right_dbtp,
        l.true_peak_max_dbtp,
        l.rms_left_db,
        l.rms_right_db,
        l.plr_db,
        l.psr_db,
        l.clip_count as f32,
        l.measured_seconds as f32,
        f.lowest_strong_hz,
        f.stereo.correlation,
        f.stereo.balance,
        f.stereo.width,
        (f.host_time - 1000.0) as f32,
    ]);
    numbers.extend(f.stereo.band_correlation);
    numbers.extend(f.stereo.band_balance);
    score.numbers(&format!("{tag} numbers"), &numbers, &floats(want("numbers")), TOLERANCE);
    let scope: Vec<f32> = f.stereo.scope_points.iter().step_by(13).flatten().copied().collect();
    score.numbers(&format!("{tag} scope"), &scope, &floats(want("scope")), 1e-4);
    if let Some(h) = &f.headphone {
        let mut hp = every(5, &h.response_db);
        hp.extend(every(5, &h.target_db));
        hp.extend(every(5, &h.predicted_at_ear_db));
        score.numbers(&format!("{tag} hp"), &hp, &floats(want("hp")), TOLERANCE);
        let theirs = golden.texts(&format!("{tag} flags"));
        score.same(
            &format!("{tag} flags count"),
            &h.stress_flags.len().to_string(),
            &theirs.len().to_string(),
        );
        for (flag, their) in h.stress_flags.iter().zip(&theirs) {
            // "id | severity | title | label | low high"
            let (text, range) = their.rsplit_once(" | ").unwrap();
            let ours = format!(
                "{} | {} | {} | {}",
                flag.id, flag.severity as i32, flag.title, flag.plot_label
            );
            score.same(&format!("{tag} flag {}", flag.id), &ours, text);
            let span = flag.frequency_range_hz.as_ref().unwrap();
            score.numbers(
                &format!("{tag} flag {} range", flag.id),
                &[*span.start(), *span.end()],
                &floats(range),
                0.5,
            );
        }
    }
    if let (Some(t), Some(e)) = (&f.third_octave, &f.spl) {
        let mut third = t.left.clone();
        third.extend(&t.right);
        score.numbers(&format!("{tag} third"), &third, &floats(want("third")), TOLERANCE);
        let mut spl = vec![
            e.uncertainty_db,
            e.level_a_fast,
            e.level_a_slow,
            e.level_z_eardrum,
            e.leq_a_track,
            e.leq_a_session,
            e.max_a_fast,
            e.dose_niosh,
            e.dose_who_weekly,
        ];
        let theirs = floats(want("spl"));
        score.numbers(&format!("{tag} spl"), &spl, &theirs[..9], TOLERANCE);
        // Seconds: compared as a part of the value. Both may be infinite.
        let times = [e.dose_seconds as f32, e.seconds_to_niosh_limit as f32];
        for (ours, theirs) in times.iter().zip(&theirs[9..11]) {
            let equal = ours == theirs || ((ours - theirs) / theirs.abs().max(1.0)).abs() < 0.005;
            score.same(&format!("{tag} spl seconds {theirs}"), &equal.to_string(), "true");
        }
        spl.clear();
        score.numbers(
            &format!("{tag} spl bands"),
            &e.band_levels_eardrum,
            &theirs[11..],
            TOLERANCE,
        );
        score.same(&format!("{tag} splname"), &e.calibration_name, want("splname"));
    }
}

#[test]
fn synthetic_frames_match_the_macos_app() {
    let golden = Lines::of("synthetic.txt");
    let mut score = Score::default();
    let rich = Options {
        include_headphone: true,
        include_spl: true,
        hot: true,
        ..Default::default()
    };
    let small = Options {
        bin_count: 512,
        level_db: -9.0,
        seed: 77,
        scope_point_count: 300,
        frames_per_second: 30.0,
        ..Default::default()
    };
    for (tag, options) in [("plain", Options::default()), ("rich", rich), ("small", small)] {
        let frames = SyntheticFrames::sequence(400, options);
        for index in [0, 1, 59, 150, 399] {
            compare_frame(&mut score, &golden, &format!("{tag}{index}"), &frames[index]);
        }
    }

    let silence = SyntheticFrames::silence(3, 1024, 60.0);
    let last = &silence[2];
    score.same(
        "silence info",
        &format!(
            "{} {:?} {} {} [{}]",
            silence.len(),
            last.host_time,
            last.is_silent,
            last.spectrum.mid.len(),
            last.stream.as_ref().unwrap().device_name
        ),
        golden.get("silence info"),
    );
    let mut all = SyntheticFrames::sequence(90, Options::default());
    all.extend(silence);
    let added = adding_demo_spl(&all);
    for index in [0, 89, 92] {
        compare_frame(&mut score, &golden, &format!("added{index}"), &added[index]);
    }
    score.finish();
}
