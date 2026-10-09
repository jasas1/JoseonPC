//! The result page of "Measure your headphone…": one bass roll-off note on an
//! averaged result, and one gate for "noise or leak".
//!
//! Ported from `Tests/JoseonHeadphonesTests/MeasurementResultPageTests.swift`.

use super::honesty::measure;
use crate::curve::HeadphoneCurve;
use crate::measurement::sweep_analysis::standard_grid;
use crate::measurement::{MeasuredHeadphone, MeasurementQuality};
use std::collections::HashSet;

fn roll_off_notes(measured: &MeasuredHeadphone) -> Vec<&String> {
    measured
        .quality
        .warnings
        .iter()
        .filter(|w| MeasuredHeadphone::is_bass_roll_off_note(w))
        .collect()
}

/// A side made by hand: flat, with a bass shelf of `bass_db` at and under
/// 50 Hz, and one signal-to-noise figure everywhere.
fn side(bass_db: f32, snr_at_40hz: f32) -> MeasuredHeadphone {
    let grid = standard_grid();
    let levels: Vec<f32> = grid.iter().map(|&f| if f <= 50.0 { bass_db } else { 0.0 }).collect();
    let frequencies_hz: Vec<f32> = grid.iter().map(|&f| f as f32).collect();
    let mut measured = MeasuredHeadphone {
        curve: HeadphoneCurve {
            name: "side".to_string(),
            source: "test".to_string(),
            frequencies_hz: frequencies_hz.clone(),
            levels_db: levels.clone(),
        },
        absolute_magnitude_db: levels,
        sensitivity: None,
        derived_sensitivity: None,
        quality: MeasurementQuality {
            frequencies_hz,
            snr_db: vec![snr_at_40hz; grid.len()],
            thd_percent: 0.1,
            runs: 3,
            agreement_db: 0.2,
            ..Default::default()
        },
        method: "test".to_string(),
    };
    let own = if bass_db == -14.0 {
        "Only the left side says this."
    } else {
        "Only the right side says this."
    };
    let mut warnings = vec!["Both sides say this.".to_string(), own.to_string()];
    if let Some(note) = measured.bass_roll_off_note() {
        warnings.insert(1, note);
    }
    measured.quality.warnings = warnings;
    measured
}

fn shelf_levels() -> (Vec<f64>, Vec<f64>) {
    let grid = standard_grid();
    let levels = grid.iter().map(|&f| if f <= 50.0 { -15.0 } else { 0.0 }).collect();
    (levels, grid)
}

fn note_for(snr: Option<f64>) -> Option<String> {
    let (levels, grid) = shelf_levels();
    MeasuredHeadphone::bass_roll_off_note_for(&levels, &grid, snr, 10.0, MeasuredHeadphone::SNR_THRESHOLD_DB)
}

// MARK: One roll-off note on the average

/// Both sides leak, by different amounts. Each side carries its own note. The
/// average must carry exactly one: the note of the averaged curve.
#[test]
fn average_of_two_leaky_sides_carries_exactly_one_roll_off_note() {
    let left = side(-14.0, 40.0);
    let right = side(-20.0, 32.0);
    let left_note = left.bass_roll_off_note().unwrap();
    let right_note = right.bass_roll_off_note().unwrap();
    assert_ne!(left_note, right_note);

    let average = MeasuredHeadphone::average(&left, &right);
    let notes = roll_off_notes(&average);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert_eq!(Some(notes[0].clone()), average.bass_roll_off_note());
    assert!(!average.quality.warnings.contains(&left_note));
    assert!(!average.quality.warnings.contains(&right_note));

    // The mean of a 14 dB and a 20 dB fall, with the worse signal-to-noise of the two sides.
    assert!(notes[0].contains("falls 17 dB"), "{}", notes[0]);
    assert!(notes[0].contains("32 dB of signal-to-noise"), "{}", notes[0]);

    // All other warnings are still the union of both sides, once each, under the average banner.
    let warnings = &average.quality.warnings;
    assert_eq!(warnings[0], MeasuredHeadphone::AVERAGE_NOTE);
    assert_eq!(warnings.iter().filter(|w| *w == "Both sides say this.").count(), 1);
    assert!(warnings.iter().any(|w| w == "Only the left side says this."));
    assert!(warnings.iter().any(|w| w == "Only the right side says this."));
    assert_eq!(average.quality.runs, 6);
    assert_eq!(average.method, "Average of left and right; test");
}

/// One noisy side makes the averaged bass "limited by noise": the worse
/// signal-to-noise wins, and the one note says so.
#[test]
fn average_with_one_noisy_side_says_noise_once() {
    let average = MeasuredHeadphone::average(&side(-14.0, 40.0), &side(-14.0, 6.0));
    let notes = roll_off_notes(&average);
    assert_eq!(notes.len(), 1, "{notes:?}");
    assert!(notes[0].contains("limited by noise"));
    assert!(!notes[0].contains("seal leak"));
}

/// Sealed sides: no note per side, none on the average. And one leaky side whose
/// fall the average halves under the threshold leaves no old sentence.
#[test]
fn average_without_a_fall_carries_no_roll_off_note() {
    let sealed = MeasuredHeadphone::average(&side(-3.0, 40.0), &side(-4.0, 40.0));
    assert!(roll_off_notes(&sealed).is_empty());
    let halved = MeasuredHeadphone::average(&side(-14.0, 40.0), &side(-2.0, 40.0));
    assert!(halved.bass_roll_off_note().is_none());
    assert!(roll_off_notes(&halved).is_empty(), "{:?}", halved.quality.warnings);
}

/// The same through the real analysis: two simulated leaky sides, 80 Hz and 150 Hz.
#[test]
fn average_of_two_simulated_leaky_sides_carries_exactly_one_roll_off_note() {
    let left = measure(Some(80.0), 45.0, true);
    let right = measure(Some(150.0), 45.0, true);
    assert_eq!(roll_off_notes(&left).len(), 1);
    assert_eq!(roll_off_notes(&right).len(), 1);
    assert_ne!(left.bass_roll_off_note(), right.bass_roll_off_note());

    let average = MeasuredHeadphone::average(&left, &right);
    let notes = roll_off_notes(&average);
    println!("  average of an 80 Hz and a 150 Hz leak: {notes:?}");
    assert_eq!(notes.len(), 1);
    assert_eq!(Some(notes[0].clone()), average.bass_roll_off_note());
    // No other sentence is there twice.
    let unique: HashSet<&String> = average.quality.warnings.iter().collect();
    assert_eq!(unique.len(), average.quality.warnings.len());
}

/// Every verdict of the note is recognised, so the average can drop all three.
#[test]
fn every_roll_off_note_is_recognised() {
    for snr in [None, Some(3.0), Some(40.0)] {
        let note = note_for(snr).unwrap();
        assert!(MeasuredHeadphone::is_bass_roll_off_note(&note), "{note}");
    }
    assert!(!MeasuredHeadphone::is_bass_roll_off_note("No coupler correction."));
}

// MARK: One gate for noise versus leak

/// `bass_is_measured` is what the leak card of the result page asks. It must
/// change at exactly the signal-to-noise ratio where the note changes from
/// "limited by noise" to "seal leak".
#[test]
fn the_leak_gate_and_the_roll_off_note_change_at_the_same_signal_to_noise_ratio() {
    let gate = MeasuredHeadphone::SNR_THRESHOLD_DB;
    for snr in [-10.0, 0.0, gate - 5.0, gate - 0.01, gate, gate + 0.01, gate + 30.0] {
        let note = note_for(Some(snr)).unwrap();
        let measured = MeasuredHeadphone::bass_is_measured(Some(snr));
        assert_eq!(measured, snr >= gate, "at {snr} dB");
        assert_eq!(note.contains("looks like a seal leak"), measured, "at {snr} dB: {note}");
        assert_eq!(note.contains("limited by noise"), !measured, "at {snr} dB: {note}");
    }
}

/// No noise-only segment, or a figure that is not a number: nothing says "leak".
#[test]
fn the_leak_gate_is_closed_without_a_signal_to_noise_figure() {
    assert!(!MeasuredHeadphone::bass_is_measured(None));
    assert!(!MeasuredHeadphone::bass_is_measured(Some(f64::NAN)));
    assert!(!MeasuredHeadphone::bass_is_measured(Some(f64::NEG_INFINITY)));
    assert!(MeasuredHeadphone::bass_is_measured(Some(
        MeasuredHeadphone::SNR_THRESHOLD_DB
    )));
}

/// Through the real analysis: the quiet leak passes the gate, the noisy one
/// does not, the one without a noise segment does not.
#[test]
fn the_leak_gate_on_simulated_measurements() {
    let gate = |m: &MeasuredHeadphone| MeasuredHeadphone::bass_is_measured(m.quality.snr_db_at(40.0).map(f64::from));
    assert!(gate(&measure(Some(80.0), 45.0, true)));
    assert!(!gate(&measure(Some(80.0), 5.0, true)));
    assert!(!gate(&measure(Some(80.0), 45.0, false)));
}
