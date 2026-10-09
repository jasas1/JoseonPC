//! Port of `SpectrumToneGateTests.swift`: the memory of the tonal gate in
//! `SpectrumResolution::detect_tones`. A quiet steady tone is a tone in every
//! transform, a tone that stops is released within a few transforms, and plain
//! noise holds no tone.

use super::low_end::grid;
use super::*;
use std::collections::{HashMap, HashSet};
use std::ops::RangeInclusive;

/// One resolution, mapped like the analyzer maps it, fed a signal in 800-frame
/// blocks. Calls `each` after every transform with its index.
fn drive(
    size: usize,
    hop: usize,
    served_hz: RangeInclusive<f32>,
    seconds: f64,
    signal: impl Fn(usize) -> (f32, f32),
    mut each: impl FnMut(usize, &SpectrumResolution),
) {
    let (rate, block) = (48_000.0, 800);
    let mut resolution = SpectrumResolution::new(size, hop, rate).unwrap();
    let (frequencies, step) = grid(1024);
    let df = (rate / size as f64) as f32;
    let weight: Vec<f32> = frequencies
        .iter()
        .map(|f| if served_hz.contains(f) { 1.0 } else { 0.0 })
        .collect();
    resolution.update_map(&frequencies, step, &weight, &|_| 3.0 * df, &|_| df);
    let capacity = 1 << 17;
    let (mut history_l, mut history_r) = (vec![0.0f32; capacity], vec![0.0f32; capacity]);
    let (mut written, mut transforms) = (0, 0);
    for k in 0..(seconds * rate) as usize / block {
        for i in 0..block {
            let s = k * block + i;
            let (l, r) = signal(s);
            history_l[s & (capacity - 1)] = l;
            history_r[s & (capacity - 1)] = r;
        }
        written += block;
        if let Some(end) = resolution.due_hop(written, capacity) {
            resolution.transform(&history_l, &history_r, end);
            each(transforms, &resolution);
            transforms += 1;
        }
    }
}

fn is_tonal(resolution: &SpectrumResolution, hz: f32) -> bool {
    let k = (hz / resolution.df).round() as isize;
    resolution
        .detected_tones()
        .iter()
        .any(|tone| (tone.bin as isize - k).abs() <= 1)
}

fn noise_pair() -> (Vec<f32>, Vec<f32>) {
    (
        test_signals::pink_noise(0.04, 1 << 17, 7),
        test_signals::pink_noise(0.04, 1 << 17, 99),
    )
}

/// The spectrogram fixture: 3.1 kHz at −56 dBFS over pink noise at 0.04, in the
/// 2048-point FFT that owns 3.1 kHz. With a one-transform gate it is tonal in
/// 2% of transforms. Then the tone stops and the noise goes on: the tone is
/// released within twelve transforms and does not come back.
#[test]
fn a_quiet_steady_tone_is_a_tone_in_every_transform_and_is_released_when_it_stops() {
    let rate = 48_000.0;
    let (noise_l, noise_r) = noise_pair();
    let amplitude = 10f64.powf(-56.0 / 20.0) as f32;
    let stop_at = (8.0 * rate) as usize;
    let (mut on, mut checked, mut back_on) = (0, 0, 0);
    let (mut last_on, mut stop_transform) = (-1i64, -1i64);
    drive(
        2048,
        768,
        2_828.0..=24_000.0,
        11.0,
        |s| {
            let tone = if s < stop_at {
                amplitude * (2.0 * std::f64::consts::PI * 3_100.0 * s as f64 / rate).sin() as f32
            } else {
                0.0
            };
            (tone + noise_l[s % noise_l.len()], tone + noise_r[s % noise_r.len()])
        },
        |t, resolution| {
            let t = t as i64;
            let tonal = is_tonal(resolution, 3_100.0);
            if resolution.hop_end <= stop_at {
                // The first second is the memory filling; after it the tone is steady.
                if resolution.hop_end >= rate as usize {
                    checked += 1;
                    if tonal {
                        on += 1;
                    }
                }
            } else {
                if stop_transform < 0 {
                    stop_transform = t;
                }
                if tonal {
                    last_on = t;
                    if t - stop_transform > 12 {
                        back_on += 1;
                    }
                }
            }
        },
    );
    println!(
        "tone gate: tonal in {on} of {checked} transforms; last tonal {} transforms after the stop",
        last_on - stop_transform
    );
    assert!(checked > 400);
    assert!(
        on as f64 >= checked as f64 * 0.98,
        "a steady tone is a tone in every transform: {on} of {checked}"
    );
    assert!(
        last_on - stop_transform <= 12,
        "a tone that stopped is released within twelve transforms"
    );
    assert_eq!(back_on, 0, "and does not come back on the noise");
}

/// Plain pink noise at every resolution: no bin is tonal in three transforms
/// running. The one-transform gate lets a rare candidate through, and the
/// memory can hold it for one more transform while the average is lifted. It
/// must not go on from there.
#[test]
fn pink_noise_holds_no_tone() {
    let (noise_l, noise_r) = noise_pair();
    for (size, hop, served) in [
        (2_048, 768, 2_828.0..=24_000.0),
        (8_192, 1_024, 141.0..=2_828.0),
        (32_768, 32_768 / 6, 10.0..=283.0),
    ] {
        let mut runs: HashMap<usize, usize> = HashMap::new(); // bin -> transforms tonal in a row
        let (mut longest, mut tonal_transforms, mut transforms) = (0, 0, 0);
        drive(
            size,
            hop,
            served,
            8.0,
            |s| (noise_l[s % noise_l.len()], noise_r[s % noise_r.len()]),
            |_, resolution| {
                transforms += 1;
                let bins: HashSet<usize> = resolution.detected_tones().iter().map(|tone| tone.bin).collect();
                tonal_transforms += bins.len();
                let mut next = HashMap::new();
                for &b in &bins {
                    let run = [b - 1, b, b + 1]
                        .iter()
                        .map(|k| runs.get(k).copied().unwrap_or(0))
                        .max()
                        .unwrap()
                        + 1;
                    next.insert(b, run);
                    longest = longest.max(run);
                }
                runs = next;
            },
        );
        println!("tone gate on pink noise, {size}-point: {tonal_transforms} tone-transforms in {transforms} transforms, longest run {longest}");
        assert!(longest <= 2, "{size}-point: a false tone held for {longest} transforms");
        assert!(
            (tonal_transforms as f64) < transforms as f64 * 0.05,
            "{size}-point: false tones in {tonal_transforms} of {transforms} transforms"
        );
    }
}
