//! Ported from `Tests/JoseonHeadphonesTests/MeasurementSweepTests.swift`.

use super::*;
use crate::measurement::fft::peak_index;
use crate::measurement::sweep_analysis::{smoothed_magnitude_db, OctaveSmoothing};
use crate::measurement::sweep_signal::{PinkNoiseOptions, SweepKind};

fn peak(samples: &[f64]) -> f64 {
    samples.iter().fold(0.0f64, |m, v| m.max(v.abs()))
}

fn pink(seconds: f64, seed: u64) -> SweepSignal {
    SweepSignal::periodic_pink_noise(
        48_000.0,
        PinkNoiseOptions {
            seconds,
            seed,
            ..Default::default()
        },
    )
}

#[test]
fn sweep_shape_and_limits() {
    let sweep = sweep(3.0, -6.0);
    assert_eq!(sweep.samples.len(), 144_000);
    near(sweep.duration_seconds, 3.0, 1e-9);
    near(peak(&sweep.samples), 0.5012, 0.002);
    // Raised-cosine fades: the ends are silent, the middle is not.
    near(sweep.samples[0], 0.0, 1e-9);
    near(sweep.samples[sweep.samples.len() - 1], 0.0, 1e-6);
    assert!(peak(&sweep.samples[71_900..72_100]) > 0.49);
}

#[test]
fn end_frequency_is_clamped_under_nyquist() {
    let sweep = SweepSignal::exponential_sweep(
        44_100.0,
        SweepOptions {
            end_hz: 24_000.0,
            ..Default::default()
        },
    );
    assert!(sweep.end_hz < 22_050.0);
    near(sweep.end_hz, 44_100.0 * 0.5 * 0.95, 1e-6);
}

#[test]
fn duration_is_clamped_to_the_spec_range() {
    near(sweep(0.5, -6.0).duration_seconds, 2.0, 1e-9);
    near(sweep(60.0, -6.0).duration_seconds, 10.0, 1e-9);
}

#[test]
fn harmonic_offsets_follow_the_sweep_rate() {
    let sweep = sweep(5.0, -6.0);
    let l = 5.0 / (sweep.end_hz / sweep.start_hz).ln();
    near(sweep.sweep_rate_seconds(), l, 1e-9);
    near(sweep.harmonic_time_offset_seconds(2), l * 2.0f64.ln(), 1e-9);
    near(sweep.harmonic_time_offset_seconds(3), l * 3.0f64.ln(), 1e-9);
    assert_eq!(sweep.harmonic_time_offset_seconds(1), 0.0);
    near(
        sweep.harmonic_band_coverage(2),
        (sweep.end_hz / 40.0).ln() / (sweep.end_hz / 20.0).ln(),
        1e-9,
    );
    assert!(sweep.harmonic_band_coverage(2) > 0.88);
    assert!(sweep.harmonic_band_coverage(3) < sweep.harmonic_band_coverage(2));
}

/// Farina's inverse filter: convolved with the sweep it must give a flat unit passband.
#[test]
fn inverse_filter_flattens_the_sweep() {
    let sweep = sweep(2.0, 0.0);
    let inverse = sweep.inverse_filter().unwrap();
    assert_eq!(inverse.len(), sweep.samples.len());
    let n = next_power_of_two(2 * sweep.samples.len());
    let fft = RealFft::new(n);
    let x = fft.forward(&sweep.samples);
    let f = fft.forward(&inverse);
    let mut product = HalfSpectrum::new(n);
    for k in 0..product.bin_count() {
        product.real[k] = x.real[k] * f.real[k] - x.imag[k] * f.imag[k];
        product.imag[k] = x.real[k] * f.imag[k] + x.imag[k] * f.real[k];
    }
    let grid = [50.0, 100.0, 300.0, 1_000.0, 3_000.0, 8_000.0, 15_000.0];
    let db = smoothed_magnitude_db(&product, 48_000.0, &grid, OctaveSmoothing::Sixth);
    for (i, f) in grid.iter().enumerate() {
        assert!(db[i].abs() <= 1.0, "inverse filter is not flat at {f} Hz: {}", db[i]);
    }
    // And it really is an impulse: the peak is far above the rest.
    let impulse = fft.inverse(&product);
    let peak = peak_index(&impulse);
    assert!(impulse[peak].abs() / rms(&impulse) > 50.0);
}

#[test]
fn pink_noise_is_periodic_pink_and_repeatable() {
    let noise = pink(2.0, 42);
    assert_eq!(noise.kind, SweepKind::PeriodicPinkNoise);
    assert_eq!(noise.period_samples(), next_power_of_two(96_000));
    near(peak(&noise.samples), 0.5012, 0.002);
    // Same seed, same samples.
    assert_eq!(noise.samples, pink(2.0, 42).samples);
    assert_ne!(noise.samples, pink(2.0, 43).samples);
    // −3 dB per octave.
    let spectrum = RealFft::new(noise.period_samples()).forward(&noise.samples);
    let grid = [100.0, 200.0, 400.0, 800.0, 1_600.0, 3_200.0, 6_400.0];
    let db = smoothed_magnitude_db(&spectrum, 48_000.0, &grid, OctaveSmoothing::Third);
    for i in 1..grid.len() {
        assert!((db[i] - db[i - 1] + 3.01).abs() <= 0.6, "slope wrong at {} Hz", grid[i]);
    }
    assert!(noise.inverse_filter().is_none());
}

/// The crest-factor difference between the two stimuli, which is why the sweep is the default.
#[test]
fn sweep_delivers_more_energy_than_noise_at_the_same_peak() {
    let sweep = sweep(2.0, -6.0);
    let noise = pink(2.0, PinkNoiseOptions::default().seed);
    println!(
        "  sweep RMS {:.2} dBFS, pink noise RMS {:.2} dBFS",
        sweep.rms_level_dbfs(),
        noise.rms_level_dbfs()
    );
    assert!(sweep.rms_level_dbfs() - noise.rms_level_dbfs() > 5.0);
    near(sweep.rms_level_dbfs(), -9.01, 0.5);
}

#[test]
fn real_fft_round_trips() {
    let fft = RealFft::new(1_024);
    let mut rng = SplitMix64::new(5);
    let x: Vec<f64> = (0..1_024).map(|_| rng.next_gaussian()).collect();
    let back = fft.inverse(&fft.forward(&x));
    for i in 0..1_024 {
        near(back[i], x[i], 1e-9);
    }
    // A known single bin: 4 cycles of a cosine put all the energy in bin 4.
    let tone: Vec<f64> = (0..1_024)
        .map(|i| (2.0 * PI * 4.0 * i as f64 / 1_024.0).cos())
        .collect();
    let spectrum = fft.forward(&tone);
    near(spectrum.magnitude(4), 512.0, 1e-6);
    near(spectrum.magnitude(5), 0.0, 1e-6);
    near(spectrum.frequency(4, 48_000.0), 187.5, 1e-9);
}
