//! Port of `SpectrumTests.swift`: accuracy of `SpectrumAnalyzer`.

use super::*;

// MARK: 1. Calibration

#[test]
fn minus_six_sine_reads_minus_six_at_every_rate() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let mut analyzer = SpectrumAnalyzer::default();
        feed_mono(&mut analyzer, &sine(1_000.0, -6.0, rate, 2.0), rate);
        let reading = analyzer.read().spectrum;
        let bin = nearest_bin(&reading.frequencies, 1_000.0);
        assert_near(reading.mid[bin], -6.0, 0.3, &format!("1 kHz -6 dBFS at {rate} Hz"));
        assert_near(reading.left[bin], -6.0, 0.3, &format!("left at {rate} Hz"));
        assert_near(reading.right[bin], -6.0, 0.3, &format!("right at {rate} Hz"));
    }
}

/// A full-scale sine must read 0 dBFS at any frequency and any FFT size, which
/// means all three resolutions and both crossfade regions.
#[test]
fn full_scale_sine_reads_zero_at_every_frequency() {
    let rate = 48_000.0;
    for hz in [
        30.0, 55.0, 120.0, 200.0, 440.0, 1_000.0, 2_000.0, 3_150.0, 6_300.0, 12_000.0, 18_000.0,
    ] {
        let mut analyzer = SpectrumAnalyzer::default();
        feed_mono(&mut analyzer, &sine(hz, 0.0, rate, 2.0), rate);
        let reading = analyzer.read().spectrum;
        assert_near(peak_near(&reading, hz as f32), 0.0, 0.3, &format!("{hz} Hz full scale"));
    }
}

/// Frequencies off the bin centre are the scalloping worst case.
#[test]
fn calibration_holds_off_bin_centre() {
    let rate = 48_000.0;
    let mut hz = 997.0;
    while hz <= 1_100.0 {
        let mut analyzer = SpectrumAnalyzer::default();
        feed_mono(&mut analyzer, &sine(hz, -12.0, rate, 1.5), rate);
        assert_near(
            peak_near(&analyzer.read().spectrum, hz as f32),
            -12.0,
            0.3,
            &format!("{hz} Hz"),
        );
        hz += 7.3;
    }
}

// MARK: 2. Resolution

#[test]
fn thirty_and_forty_hertz_resolve_with_a_dip() {
    let rate = 48_000.0;
    let mixed = add(&sine(30.0, -6.0, rate, 2.0), &sine(40.0, -6.0, rate, 2.0));
    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &mixed, rate);
    let reading = analyzer.read().spectrum;

    let peak30 = peak_near_in(&reading.mid, &reading.frequencies, 30.0, 0.05);
    let peak40 = peak_near_in(&reading.mid, &reading.frequencies, 40.0, 0.05);
    assert_near(peak30, -6.0, 1.0, "30 Hz");
    assert_near(peak40, -6.0, 1.0, "40 Hz");

    // Lowest point strictly between the two tones.
    let dip = reading
        .frequencies
        .iter()
        .zip(&reading.mid)
        .filter(|(f, _)| **f > 31.0 && **f < 39.0)
        .fold(f32::MAX, |low, (_, &v)| low.min(v));
    let peaks = peak30.min(peak40);
    assert!(
        dip <= peaks - 6.0,
        "dip between 30 and 40 Hz was only {} dB",
        peaks - dip
    );
}

#[test]
fn fifteen_kilohertz_located_within_one_percent() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &sine(15_000.0, -3.0, rate, 2.0), rate);
    let out = analyzer.read();
    assert_near(out.peak.frequency_hz, 15_000.0, 150.0, "peak readout");
    // The display curve has to put the maximum there too.
    let top = index_of_max(&out.spectrum.mid);
    assert_near(out.spectrum.frequencies[top], 15_000.0, 150.0, "display maximum");
}

// MARK: 2b. Tone shape

struct PeakShape {
    octaves: f32,
    level: f32,
    hz: f32,
    top_run: usize,
}

/// Width in octaves of the region around the maximum that stays within 3 dB of
/// it, and the longest run of display bins that equal the maximum.
fn peak_shape(values: &[f32], frequencies: &[f32]) -> PeakShape {
    let top = index_of_max(values);
    let (mut lo, mut hi) = (top, top);
    while lo > 0 && values[lo - 1] > values[top] - 3.0 {
        lo -= 1;
    }
    while hi < values.len() - 1 && values[hi + 1] > values[top] - 3.0 {
        hi += 1;
    }
    let (mut run, mut longest) = (0, 0);
    for &value in &values[lo..=hi] {
        if value == values[top] {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 0;
        }
    }
    // Bin edges, not centres: one display bin already has a width.
    let step = (frequencies[1] / frequencies[0]).log2();
    PeakShape {
        octaves: (frequencies[hi] / frequencies[lo]).log2() + step,
        level: values[top],
        hz: frequencies[top],
        top_run: longest,
    }
}

/// A lone tone is a pointed peak of the right height, not a flat-topped table
/// as wide as the power sum. 1 kHz sits inside the bandwidth ramp, where the
/// sum is widest.
#[test]
fn lone_tone_is_a_pointed_peak() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &sine(1_000.0, -6.0, rate, 2.0), rate);
    let reading = analyzer.read().spectrum;
    for (name, curve) in [
        ("mid", &reading.mid),
        ("left", &reading.left),
        ("peak hold", &reading.peak_hold),
    ] {
        let shape = peak_shape(curve, &reading.frequencies);
        assert_near(shape.level, -6.0, 0.3, name);
        assert_near(shape.hz, 1_000.0, 8.0, name);
        assert!(
            shape.octaves < 1.0 / 24.0,
            "{name}: -3 dB width was 1/{} octave",
            1.0 / shape.octaves
        );
        assert!(shape.top_run < 3, "{name}: flat top of {} display bins", shape.top_run);
    }
    // The skirt falls fast: 1/6 octave away the curve is far below the tone.
    let away = reading.mid[nearest_bin(&reading.frequencies, 1_000.0 * 2f32.powf(1.0 / 6.0))];
    assert!(away < -6.0 - 40.0, "skirt at {away} dB");
}

/// The same at low resolution, in the low/mid crossfade and in both bandwidth
/// ramps. On-bin and off-bin tones, with a noise floor under them.
#[test]
fn tones_over_noise_stay_pointed_at_every_resolution() {
    let rate = 48_000.0;
    for hz in [55.0, 98.0, 146.83, 220.0, 440.0, 1_000.0, 2_500.0, 6_200.0] {
        let mut analyzer = SpectrumAnalyzer::default();
        let tone = sine(hz, -12.0, rate, 2.0);
        feed_mono(&mut analyzer, &add(&tone, &pink(0.01, tone.len())), rate);
        let reading = analyzer.read().spectrum;
        let shape = peak_shape(&reading.mid, &reading.frequencies);
        assert_near(shape.level, -12.0, 0.4, &format!("{hz} Hz"));
        assert!(
            (shape.hz as f64 - hz).abs() <= hz * 0.02,
            "{hz} Hz: found at {}",
            shape.hz
        );
        let limit = if hz < 80.0 { 1.0 / 12.0 } else { 1.0 / 20.0 };
        assert!(
            shape.octaves < limit,
            "{hz} Hz: -3 dB width was 1/{} octave",
            1.0 / shape.octaves
        );
        assert!(shape.top_run < 3, "{hz} Hz");
    }
}

// MARK: 3. Low-end smoothness

/// Below ~100 Hz a 1024-bin log grid is much finer than the FFT grid. The curve
/// must be interpolated, not a staircase of repeated FFT-bin values.
#[test]
fn low_end_has_no_stair_steps() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &pink(0.5, (rate * 2.0) as usize), rate);
    let reading = analyzer.read().spectrum;

    let (mut run, mut longest, mut previous) = (1, 1, f32::NAN);
    for (f, &v) in reading.frequencies.iter().zip(&reading.mid) {
        if !(*f >= 20.0 && *f <= 120.0) {
            continue;
        }
        if v == previous {
            run += 1;
            longest = longest.max(run);
        } else {
            run = 1;
        }
        previous = v;
    }
    assert!(
        longest <= 2,
        "found a plateau of {longest} identical display bins below 120 Hz"
    );
}

// MARK: 4. Note readout

#[test]
fn note_names() {
    let rate = 48_000.0;
    for (hz, name) in [(415.30, "G#4"), (440.0, "A4"), (261.6256, "C4"), (1_760.0, "A6")] {
        let mut analyzer = SpectrumAnalyzer::default();
        feed_mono(&mut analyzer, &sine(hz, -6.0, rate, 2.0), rate);
        let peak = analyzer.read().peak;
        assert_eq!(peak.note_name, name, "{hz} Hz");
        assert_near(peak.cents, 0.0, 3.0, &format!("{hz} Hz cents"));
    }
}

#[test]
fn note_is_blank_when_the_peak_does_not_stand_out() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &white(0.3, (rate * 2.0) as usize), rate);
    assert_eq!(analyzer.read().peak.note_name, "", "white noise has no note");
}

// MARK: 5. Silence

#[test]
fn silence_reads_floor_and_no_note() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &silence(rate, 2.0), rate);
    let out = analyzer.read();
    let s = &out.spectrum;
    for (name, curve) in [
        ("mid", &s.mid),
        ("left", &s.left),
        ("right", &s.right),
        ("side", &s.side),
        ("peak hold", &s.peak_hold),
        ("average", &s.average),
    ] {
        assert!(curve.iter().all(|&v| v == FLOOR), "{name} is not at the floor");
    }
    assert_eq!(out.peak.note_name, "");
    assert_eq!(out.peak.level_db, FLOOR);
    for value in out.bands.values() {
        assert_near(value, FLOOR, 0.001, "band");
    }
}

// MARK: 6. Peak hold

#[test]
fn peak_hold_decays_at_the_set_rate() {
    let rate = 48_000.0;
    // A very short release collapses the live curve, so only the hold is left.
    let settings = SpectrumSettings {
        peak_decay_db_per_second: 12.0,
        release_seconds: 0.0001,
        ..Default::default()
    };
    let mut analyzer = SpectrumAnalyzer::new(settings);

    feed_mono(&mut analyzer, &sine(1_000.0, -6.0, rate, 2.0), rate);
    let bin = nearest_bin(&analyzer.read().spectrum.frequencies, 1_000.0);
    let start = analyzer.read().spectrum.peak_hold[bin];
    assert_near(start, -6.0, 0.3, "hold at the start");

    // 1.5 s of silence: the hold falls 12 dB/s * 1.5 s = 18 dB.
    feed_mono(&mut analyzer, &silence(rate, 1.5), rate);
    let after = analyzer.read().spectrum.peak_hold[bin];
    assert_near(after, start - 18.0, 0.5, "hold after 1.5 s");
}

// MARK: 7. Tilt

#[test]
fn tilt_pivots_at_one_kilohertz() {
    let rate = 48_000.0;
    let noise = pink(0.5, (rate * 2.0) as usize);

    let mut flat = SpectrumAnalyzer::default();
    feed_mono(&mut flat, &noise, rate);
    let flat = flat.read().spectrum;

    let mut sloped = SpectrumAnalyzer::new(SpectrumSettings {
        tilt_db_per_octave: 4.5,
        ..Default::default()
    });
    feed_mono(&mut sloped, &noise, rate);
    let sloped = sloped.read().spectrum;

    let low = nearest_bin(&flat.frequencies, 100.0);
    let high = nearest_bin(&flat.frequencies, 10_000.0);
    let expected_low = 4.5 * (flat.frequencies[low] / 1_000.0).log2();
    let expected_high = 4.5 * (flat.frequencies[high] / 1_000.0).log2();

    assert_near(sloped.mid[low] - flat.mid[low], expected_low, 0.05, "100 Hz");
    assert_near(sloped.mid[high] - flat.mid[high], expected_high, 0.05, "10 kHz");
    // 100 Hz and 10 kHz are three and a third octaves either side of the pivot.
    assert_near(expected_high - expected_low, 29.9, 0.2, "tilt end to end");

    let pivot = nearest_bin(&flat.frequencies, 1_000.0);
    assert_near(sloped.mid[pivot], flat.mid[pivot], 0.05, "pivot");
}

// MARK: 8. Band energy

#[test]
fn band_energy_places_tones_in_the_right_band() {
    let rate = 48_000.0;

    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &sine(100.0, 0.0, rate, 2.0), rate);
    let bands = analyzer.read().bands;
    assert_near(bands.bass, 0.0, 0.3, "bass");
    assert_eq!(index_of_max(&bands.values()), 1, "100 Hz belongs to bass");
    assert!(bands.sub_bass < bands.bass - 40.0);
    assert!(bands.low_mid < bands.bass - 40.0);

    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &sine(5_000.0, 0.0, rate, 2.0), rate);
    let bands = analyzer.read().bands;
    assert_near(bands.presence, 0.0, 0.3, "presence");
    assert_eq!(index_of_max(&bands.values()), 5, "5 kHz belongs to presence");
    assert!(bands.upper_mid < bands.presence - 40.0);
    assert!(bands.brilliance < bands.presence - 40.0);
}

// MARK: 9. Channels

#[test]
fn left_only_signal_gives_equal_mid_and_side_and_silent_right() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    let tone = sine(1_000.0, 0.0, rate, 2.0);
    feed(&mut analyzer, &tone, &vec![0.0; tone.len()], rate);
    let reading = analyzer.read().spectrum;
    let bin = nearest_bin(&reading.frequencies, 1_000.0);

    assert_near(reading.left[bin], 0.0, 0.3, "left");
    assert_eq!(reading.right[bin], FLOOR, "right channel is silent");
    assert_near(
        reading.mid[bin],
        reading.side[bin],
        0.01,
        "mid and side match for a one-sided signal",
    );
    assert_near(reading.mid[bin], -6.0, 0.3, "mid is half the amplitude of left");
}

// MARK: 10. Reconfiguration

#[test]
fn sample_rate_change_reconfigures_cleanly() {
    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &sine(1_000.0, -6.0, 48_000.0, 2.0), 48_000.0);
    let first = analyzer.read().spectrum;
    assert_near(
        first.mid[nearest_bin(&first.frequencies, 1_000.0)],
        -6.0,
        0.3,
        "at 48 kHz",
    );

    feed_mono(&mut analyzer, &sine(1_000.0, -6.0, 96_000.0, 2.0), 96_000.0);
    let after = analyzer.read().spectrum;
    assert_near(
        after.mid[nearest_bin(&after.frequencies, 1_000.0)],
        -6.0,
        0.3,
        "after switching to 96 kHz",
    );

    feed_mono(&mut analyzer, &sine(3_000.0, -10.0, 44_100.0, 2.0), 44_100.0);
    assert_near(
        peak_near(&analyzer.read().spectrum, 3_000.0),
        -10.0,
        0.3,
        "after switching to 44.1 kHz",
    );
}

#[test]
fn display_grid_follows_settings() {
    let settings = SpectrumSettings {
        display_bins: 256,
        min_hz: 20.0,
        max_hz: 20_000.0,
        ..Default::default()
    };
    let mut analyzer = SpectrumAnalyzer::new(settings);
    feed_mono(&mut analyzer, &sine(1_000.0, -6.0, 48_000.0, 1.5), 48_000.0);
    let reading = analyzer.read().spectrum;

    for curve in [
        &reading.frequencies,
        &reading.left,
        &reading.right,
        &reading.mid,
        &reading.side,
        &reading.peak_hold,
        &reading.average,
    ] {
        assert_eq!(curve.len(), 256);
    }
    assert_near(reading.frequencies[0], 20.0, 0.01, "first bin");
    assert_near(reading.frequencies[255], 20_000.0, 0.5, "last bin");
    assert_near(
        reading.mid[nearest_bin(&reading.frequencies, 1_000.0)],
        -6.0,
        0.3,
        "1 kHz",
    );
}

#[test]
fn high_sample_rates_keep_the_same_window_durations() {
    for rate in [88_200.0, 176_400.0, 192_000.0] {
        let mut analyzer = SpectrumAnalyzer::default();
        let tone = sine(1_000.0, -6.0, rate, 2.0);
        feed_blocks(&mut analyzer, &tone, &tone, rate, 3_200);
        let reading = analyzer.read().spectrum;
        assert_near(
            reading.mid[nearest_bin(&reading.frequencies, 1_000.0)],
            -6.0,
            0.3,
            &format!("1 kHz at {rate} Hz"),
        );
        // Above Nyquist there is nothing to show.
        for (f, &level) in reading.frequencies.iter().zip(&reading.mid) {
            if *f > (rate / 2.0) as f32 {
                assert_eq!(level, FLOOR);
            }
        }
    }
}

#[test]
fn settings_can_change_while_running() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &sine(1_000.0, -6.0, rate, 2.0), rate);
    assert_eq!(analyzer.read().spectrum.frequencies.len(), 1_024);

    let flat = analyzer.read().spectrum;
    analyzer.set_settings(SpectrumSettings {
        tilt_db_per_octave: 3.0,
        ..analyzer.settings().clone()
    });
    let tilted = analyzer.read().spectrum;
    let bin = nearest_bin(&tilted.frequencies, 4_000.0);
    assert_near(
        tilted.mid[bin] - flat.mid[bin],
        6.0,
        0.05,
        "tilt takes effect without a restart",
    );
    assert_near(
        tilted.mid[nearest_bin(&tilted.frequencies, 1_000.0)],
        -6.0,
        0.3,
        "the pivot does not move",
    );

    analyzer.set_settings(SpectrumSettings {
        display_bins: 512,
        tilt_db_per_octave: 0.0,
        ..analyzer.settings().clone()
    });
    feed_mono(&mut analyzer, &sine(1_000.0, -6.0, rate, 2.0), rate);
    let regridded = analyzer.read().spectrum;
    assert_eq!(regridded.frequencies.len(), 512);
    assert_near(
        regridded.mid[nearest_bin(&regridded.frequencies, 1_000.0)],
        -6.0,
        0.3,
        "after the new grid",
    );
}

// MARK: 11. Average and reset

#[test]
fn average_tracks_the_long_term_level_and_reset_clears_it() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &sine(1_000.0, -6.0, rate, 3.0), rate);
    let reading = analyzer.read().spectrum;
    let bin = nearest_bin(&reading.frequencies, 1_000.0);
    assert!(reading.average[bin] > -12.0);
    assert!(reading.average[bin] <= reading.mid[bin] + 0.1);

    analyzer.reset();
    assert_eq!(analyzer.read().spectrum.average[bin], FLOOR, "reset clears the average");

    // After the reset the tone is already steady, so the average converges on it.
    feed_mono(&mut analyzer, &sine(1_000.0, -6.0, rate, 3.0), rate);
    assert_near(analyzer.read().spectrum.average[bin], -6.0, 0.3, "average after reset");
}

// MARK: 12. Crossfade

fn max_step(reading: &SpectrumReading, hz: f32, octaves: f32) -> f32 {
    let mut worst = 0.0f32;
    for i in 1..reading.frequencies.len() {
        if (reading.frequencies[i] / hz).log2().abs() <= octaves {
            worst = worst.max((reading.mid[i] - reading.mid[i - 1]).abs());
        }
    }
    worst
}

/// A seam is a discontinuity. The crossfade has to make the switch of FFT size
/// invisible: no step between neighbouring display bins that is worse than the
/// normal bin-to-bin variation of the same noise elsewhere.
#[test]
fn no_step_at_the_crossover_frequencies() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &pink(0.5, (rate * 4.0) as usize), rate);
    let reading = analyzer.read().spectrum;

    let control = max_step(&reading, 700.0, 0.5); // pure mid resolution
    for crossover in [200.0, 2_000.0] {
        let step = max_step(&reading, crossover, 0.15);
        assert!(
            step <= control,
            "step at {crossover} Hz was {step} dB, control {control} dB"
        );
    }
}

/// White noise has a flat true spectrum, so the displayed curve shows the
/// analyzer's own bandwidth against frequency: flat inside a resolution, rising
/// where the analysis bandwidth widens. The rise must be spread out, never
/// concentrated into a bump at a crossover.
#[test]
fn white_noise_curve_rises_gently_and_never_dips() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::default();
    feed_mono(&mut analyzer, &white(0.5, (rate * 6.0) as usize), rate);
    let reading = analyzer.read().spectrum;

    // Read the long-term power average, not the live curve: one noise frame per
    // display bin scatters by a couple of dB, which would swamp the slope.
    let probes = [
        25.0f32, 50.0, 100.0, 200.0, 400.0, 800.0, 1_600.0, 3_200.0, 6_400.0, 12_800.0,
    ];
    let levels = probes.map(|hz| {
        let near: Vec<f32> = reading
            .frequencies
            .iter()
            .zip(&reading.average)
            .filter(|(f, _)| (**f / hz).log2().abs() <= 0.25)
            .map(|(_, &level)| level)
            .collect();
        near.iter().sum::<f32>() / near.len() as f32
    });
    for i in 1..levels.len() {
        let slope = levels[i] - levels[i - 1];
        assert!(slope > -1.0, "dip after {} Hz", probes[i - 1]);
        assert!(slope < 3.5, "bandwidth bump at {} Hz", probes[i]);
    }
    // Two 4x bandwidth steps between the three FFT sizes: about 12 dB end to end.
    assert_near(levels[levels.len() - 1] - levels[0], 12.0, 3.0, "rise end to end");
}

// MARK: 13. Release

#[test]
fn attack_is_instant_and_release_is_slower() {
    let rate = 48_000.0;
    let mut analyzer = SpectrumAnalyzer::new(SpectrumSettings {
        release_seconds: 0.25,
        ..Default::default()
    });

    feed_mono(&mut analyzer, &sine(1_000.0, -6.0, rate, 2.0), rate);
    let reading = analyzer.read().spectrum;
    let bin = nearest_bin(&reading.frequencies, 1_000.0);
    assert_near(reading.mid[bin], -6.0, 0.3, "attack reaches the level at once");

    // One time constant of silence closes ~63% of the gap towards the floor.
    feed_mono(&mut analyzer, &silence(rate, 0.25), rate);
    let reading = analyzer.read().spectrum;
    assert!(reading.mid[bin] < -6.0);
    assert!(reading.mid[bin] > FLOOR, "release is not instant");
}
