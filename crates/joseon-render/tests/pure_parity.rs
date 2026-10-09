//! Parity with the macOS app of the render code that has no GPU in it: colors,
//! lookup tables, the log axis, the resampling of a spectrum onto plot points,
//! and the number formats. `tests/golden/render.txt` holds what the Swift code
//! gives (`tools/golden` makes the file).

#[path = "../../joseon-core/tests/common/mod.rs"]
mod common;

use common::{floats, Lines, Score};
use joseon_render::axis::{resample, LogAxis, ResampleTable};
use joseon_render::palette::{color_map, Palette, Theme};
use joseon_render::{fmt, gpu::Color};

/// A color channel may differ by this much: `pow` and `cbrt` come from the system math library.
/// Measured on Windows: 1.1e-5. One step of an 8-bit color is 4e-3.
const COLOR_TOLERANCE: f32 = 3e-5;

fn flat3(colors: &[[f32; 3]]) -> Vec<f32> {
    colors.iter().flatten().copied().collect()
}

#[test]
fn colors_and_lookup_tables_match_the_macos_app() {
    let golden = Lines::of("render.txt");
    let mut score = Score::default();
    let custom = Theme {
        background: [0.10, 0.02, 0.05, 1.0],
        grid: [0.5, 0.5, 0.2, 0.40],
        text: [0.9, 0.9, 0.7, 1.0],
        panel: [0.2, 0.1, 0.1, 1.0],
        warn: [0.9, 0.9, 0.1, 1.0],
        ..Default::default()
    };
    for (tag, theme, contrast) in [
        ("normal", Theme::default(), false),
        ("contrast", Theme::default(), true),
        ("custom", custom, false),
        ("customhc", custom, true),
    ] {
        let p = Palette::new(&theme, contrast);
        let fields: [Color; 28] = [
            p.background,
            p.panel,
            p.plot,
            p.grid,
            p.grid_strong,
            p.grid_major,
            p.grid_minor,
            p.rms,
            p.text,
            p.text_dim,
            p.text_faint,
            p.accent,
            p.warn,
            p.danger,
            p.good,
            p.left,
            p.right,
            p.spectrum_left,
            p.spectrum_right,
            p.peak_hold,
            p.average,
            p.side,
            p.hp_response,
            p.hp_target,
            p.hp_at_ear,
            p.spl_band,
            p.cursor_ghost,
            p.track,
        ];
        let ours: Vec<f32> = fields.iter().flatten().copied().collect();
        let key = format!("palette {tag}");
        score.numbers(&key, &ours, &floats(golden.get(&key)), COLOR_TOLERANCE);
        for (name, lut) in [
            ("spectrum", p.spectrum_lut(256)),
            ("heat", p.heat_lut(256)),
            ("scope", p.scope_lut(64)),
        ] {
            let key = format!("lut {name}{tag}");
            score.numbers(&key, &flat3(&lut), &floats(golden.get(&key)), COLOR_TOLERANCE);
        }
    }
    let hz = [
        1.0, 10.0, 20.0, 55.0, 100.0, 440.0, 1_000.0, 2_500.0, 9_999.0, 16_000.0, 24_000.0, 40_000.0,
    ];
    let colors: Vec<[f32; 3]> = hz.iter().map(|&f| Palette::spectrum_color(f)).collect();
    score.numbers(
        "color spectrum",
        &flat3(&colors),
        &floats(golden.get("color spectrum")),
        COLOR_TOLERANCE,
    );
    let mut ok = Vec::new();
    for c in [[0.0, 0.0, 0.0], [1.0, 1.0, 1.0], [0.2, 0.5, 0.9], [0.96, 0.22, 0.30]] {
        let lab = color_map::oklab_from_srgb(c);
        ok.extend(lab);
        ok.extend(color_map::srgb_from_oklab(lab));
    }
    score.numbers("color oklab", &ok, &floats(golden.get("color oklab")), COLOR_TOLERANCE);
    score.finish();
}

#[test]
fn axis_and_resampling_match_the_macos_app() {
    let golden = Lines::of("render.txt");
    let mut score = Score::default();
    let axis = LogAxis {
        min_hz: 10.0,
        max_hz: 24_000.0,
    };
    let mut hz: Vec<f32> = LogAxis::LABELED.to_vec();
    hz.extend(LogAxis::MINOR);
    hz.extend([5.0, 0.0, 30_000.0]);
    let positions: Vec<f32> = hz.iter().map(|&f| axis.position(f)).collect();
    score.numbers("axis position", &positions, &floats(golden.get("axis position")), 1e-6);
    // Swift `stride(from: -0.1, through: 1.1, by: 0.05)` in `Float`: start + i * step.
    let theirs = floats(golden.get("axis frequency"));
    let frequencies: Vec<f32> = (0..theirs.len())
        .map(|i| axis.frequency(-0.1f32 + i as f32 * 0.05f32))
        .collect();
    let relative: Vec<f32> = frequencies.iter().zip(&theirs).map(|(a, b)| a / b).collect();
    score.numbers("axis frequency", &relative, &vec![1.0; theirs.len()], 1e-5);

    for (tag, bins, points, lo, hi) in [
        ("up", 240usize, 900usize, 10.0f32, 24_000.0f32),
        ("down", 1_024, 300, 10.0, 24_000.0),
        ("near", 512, 480, 20.0, 20_000.0),
        ("inside", 512, 700, 100.0, 5_000.0),
        ("outside", 300, 500, 2.0, 60_000.0),
    ] {
        let frequencies: Vec<f32> = (0..bins)
            .map(|i| 10.0 * 2400f32.powf(i as f32 / (bins - 1) as f32))
            .collect();
        let mut src: Vec<f32> = frequencies
            .iter()
            .map(|f| -30.0 - 12.0 * (f / 100.0).log2() / 3.0)
            .collect();
        src[bins / 3] += 28.0;
        src[bins / 3 + 1] += 14.0;
        src[bins * 2 / 3] -= 40.0;
        for i in (0..bins).step_by(7) {
            src[i] += (i % 5) as f32 - 2.0;
        }
        let a = LogAxis { min_hz: lo, max_hz: hi };
        let mut out = vec![0.0; points];
        resample(&src, &frequencies, a, -96.0, 0.0, 3.0, &mut out);
        // A value is a level over a 96 dB range: 1e-5 is 0.001 dB.
        let key = format!("resample {tag}");
        score.numbers(&key, &out, &floats(golden.get(&key)), 1e-5);
        let table = ResampleTable::new(&frequencies, a, points).unwrap();
        assert!(table.matches(&frequencies, a, points));
        assert!(!table.matches(&frequencies, a, points + 1));
        let mut via_table = vec![0.0; points];
        table.apply(&src, -90.0, 6.0, -1.5, &mut via_table);
        let key = format!("table {tag}");
        score.numbers(&key, &via_table, &floats(golden.get(&key)), 1e-5);
    }
    score.finish();
}

#[test]
fn number_formats_match_the_macos_app() {
    let golden = Lines::of("render.txt");
    let mut score = Score::default();
    let values = [
        0.0,
        -0.04,
        0.04,
        -0.05,
        0.05,
        1.0,
        -1.0,
        12.345,
        -12.345,
        99.95,
        -119.0,
        -119.5,
        -120.0,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NAN,
        0.5,
        1.5,
        2.5,
        -0.5,
        1234.5678,
        -0.0004,
    ];
    for (tag, digits, signed) in [("d1", 1, false), ("d1s", 1, true), ("d0", 0, false), ("d2s", 2, true)] {
        let db: Vec<String> = values.iter().map(|&v| fmt::db(v, digits, signed)).collect();
        score.texts(&format!("fmt db{tag}"), &db, &golden.texts(&format!("fmt db{tag}")));
        let number: Vec<String> = values.iter().map(|&v| fmt::number(v, digits, signed)).collect();
        score.texts(
            &format!("fmt number{tag}"),
            &number,
            &golden.texts(&format!("fmt number{tag}")),
        );
    }
    let freqs = [
        0.0,
        -5.0,
        f32::NAN,
        7.9,
        8.0,
        19.96,
        20.0,
        27.5,
        440.0,
        441.37,
        999.95,
        1_000.0,
        1_500.0,
        2_000.0,
        9_999.9,
        9_999.96,
        10_000.0,
        12_345.0,
        20_000.0,
        24_000.0,
        30_000.0,
        30_001.0,
    ];
    let hz: Vec<String> = freqs.iter().map(|&f| fmt::hz(f)).collect();
    score.texts("fmt hz", &hz, &golden.texts("fmt hz"));
    let axis: Vec<String> = freqs
        .iter()
        .filter(|f| f.is_finite() && **f >= 0.0)
        .map(|&f| fmt::axis_hz(f))
        .collect();
    score.texts("fmt axis", &axis, &golden.texts("fmt axis"));

    // The note name is text. The cents are a number.
    let theirs = golden.texts("fmt note");
    for (i, &f) in freqs.iter().enumerate() {
        match fmt::note(f) {
            None => score.same(&format!("fmt note {i}"), "none", theirs[i]),
            Some((name, cents)) => {
                let (their_name, their_cents) = theirs[i].split_once(' ').unwrap();
                score.same(&format!("fmt note {i}"), &name, their_name);
                score.numbers(
                    &format!("fmt note {i} cents"),
                    &[cents],
                    &[their_cents.parse().unwrap()],
                    0.01,
                );
            }
        }
    }
    let cents: Vec<String> = [0.0, 0.4, -0.4, 0.5, -0.5, 12.3, -49.6, 50.0]
        .iter()
        .map(|&c| fmt::cents(c))
        .collect();
    score.texts("fmt cents", &cents, &golden.texts("fmt cents"));
    let pretty: Vec<String> = ["C#4", "A4", "F#-1", "##"]
        .iter()
        .map(|s| fmt::pretty_note(s))
        .collect();
    score.texts("fmt pretty", &pretty, &golden.texts("fmt pretty"));
    let mut bands: Vec<String> = fmt::band_labels::SHORT.iter().map(|s| s.to_string()).collect();
    bands.extend(fmt::band_labels::TWO.iter().map(|s| s.to_string()));
    bands.extend([fmt::MINUS.to_string(), fmt::DASH.to_string()]);
    score.texts("fmt bands", &bands, &golden.texts("fmt bands"));
    score.finish();
}
