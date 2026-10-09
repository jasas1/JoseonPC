//! Port of `SpectrumLayersTests.swift`: `SpectrumReading::mid_layers`, the three
//! mid curves before the blend, for the spectrogram.

use super::*;

fn demo_reading(seconds: f64, rate: f64, tilt: f32) -> SpectrumReading {
    let mut analyzer = SpectrumAnalyzer::new(SpectrumSettings {
        tilt_db_per_octave: tilt,
        ..Default::default()
    });
    let block = (rate / 60.0) as usize;
    let mut reading = analyzer.read().spectrum;
    for k in 0..(seconds * 60.0) as usize {
        let (left, right) = test_signals::demo_block((k * block) as i64, block, rate);
        analyzer.process(&left, &right, rate);
        reading = analyzer.read().spectrum;
    }
    reading
}

#[test]
fn three_layers_with_weights_that_sum_to_one_and_half_window_latency() {
    for rate in [44_100.0, 48_000.0, 96_000.0] {
        let reading = demo_reading(1.5, rate, 0.0);
        assert_eq!(reading.mid_layers.len(), 3);
        let n = reading.frequencies.len();
        for layer in &reading.mid_layers {
            assert_eq!(layer.levels_db.len(), n);
            assert_eq!(layer.weights.len(), n);
        }
        for i in 0..n {
            let sum: f32 = reading.mid_layers.iter().map(|layer| layer.weights[i]).sum();
            assert_near(sum, 1.0, 1e-5, &format!("weights at {} Hz", reading.frequencies[i]));
        }
        // Window lengths scale with the rate: 32768 / 8192 / 2048 at 48 kHz, doubled at 96 kHz.
        let scale = if rate == 96_000.0 { 2.0 } else { 1.0 };
        for (layer, size) in reading.mid_layers.iter().zip([32_768.0, 8_192.0, 2_048.0]) {
            assert_near(
                layer.latency_seconds,
                (size * scale / 2.0 / rate) as f32,
                1e-6,
                "latency",
            );
        }
        assert!(reading.mid_layers[0].latency_seconds > reading.mid_layers[1].latency_seconds);
        assert!(reading.mid_layers[1].latency_seconds > reading.mid_layers[2].latency_seconds);
    }
}

/// The contract: blending the layers in the power domain with their weights
/// gives `mid`. Checked on every display bin, tonal peaks included, with and
/// without a display tilt.
#[test]
fn blending_the_layers_reproduces_mid() {
    for tilt in [0.0, 4.5] {
        let reading = demo_reading(4.0, 48_000.0, tilt);
        let (mut worst, mut worst_hz) = (0.0f32, 0.0f32);
        for (i, &f) in reading.frequencies.iter().enumerate() {
            let mut power = 0.0f32;
            for layer in reading.mid_layers.iter().filter(|layer| layer.weights[i] > 0.0) {
                power += layer.weights[i] * 10f32.powf(layer.levels_db[i] / 10.0);
            }
            let blended = (10.0 * power.max(1e-30).log10()).max(FLOOR);
            let error = (blended - reading.mid[i]).abs();
            if error > worst {
                (worst, worst_hz) = (error, f);
            }
        }
        assert!(
            worst <= 0.5,
            "tilt {tilt}: blend and mid differ by {worst} dB at {worst_hz} Hz"
        );
    }
}

/// Each layer is the mid curve where it has all the weight.
#[test]
fn a_layer_is_mid_where_it_stands_alone() {
    let reading = demo_reading(3.0, 48_000.0, 0.0);
    let mut checked = 0;
    for layer in &reading.mid_layers {
        for (i, &w) in layer.weights.iter().enumerate() {
            if w == 1.0 {
                assert_near(
                    layer.levels_db[i],
                    reading.mid[i],
                    0.001,
                    &format!("{} Hz", reading.frequencies[i]),
                );
                checked += 1;
            }
        }
    }
    assert!(checked > 500, "only {checked} bins stand alone");
}

/// Silence: layers at the floor, not NaN, not −300.
#[test]
fn silent_layers_sit_on_the_floor() {
    let mut analyzer = SpectrumAnalyzer::default();
    let zeros = vec![0.0; 48_000];
    analyzer.process(&zeros, &zeros, 48_000.0);
    let reading = analyzer.read().spectrum;
    for layer in &reading.mid_layers {
        assert!(layer.levels_db.iter().all(|&v| v == FLOOR));
    }
    assert!(reading.mid.iter().all(|&v| v == FLOOR));
}
