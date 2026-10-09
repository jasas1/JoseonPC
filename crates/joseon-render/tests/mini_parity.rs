//! Parity of the mini spectrum with the macOS app: the level of every column,
//! what is shown after the ballistics, and the top of the adaptive window.
//! `tests/golden/mini.txt` holds what the Swift code gives.

#[path = "../../joseon-core/tests/common/mod.rs"]
mod common;

use common::{floats, Lines, Score};
use joseon_core::contracts::{AnalysisFrame, SpectrumReading};
use joseon_render::mini_spectrum::{tilt, MiniSpectrum};
use joseon_render::synthetic::{Options, SyntheticFrames};

#[test]
fn the_columns_match_the_macos_app() {
    let golden = Lines::of("mini.txt");
    let mut score = Score::default();
    let frames = SyntheticFrames::sequence(200, Options::default());
    for (tag, width, height, scale) in [
        ("menu", 64.0, 18.0, 2.0),
        ("narrow", 44.0, 18.0, 1.0),
        ("icon", 16.0, 16.0, 2.0),
        ("strip", 150.0, 40.0, 2.0),
    ] {
        let mut mini = MiniSpectrum::new();
        let key = |name: &str| format!("mini {tag} {name}");
        let n = mini.column_count((width * scale) as usize);
        score.numbers(&key("columns"), &[n as f32], &floats(golden.get(&key("columns"))), 0.0);
        let white = [1.0; 4];
        for (k, frame) in frames.iter().enumerate() {
            let (pw, ph, rgba) = mini.render(frame, width, height, scale, white);
            assert_eq!(rgba.len(), (pw * ph * 4) as usize);
            if k == 0 || k % 40 == 39 {
                // The frames differ by up to 0.001 dB.
                score.numbers(
                    &key(&format!("{k} db")),
                    mini.column_db(),
                    &floats(golden.get(&key(&format!("{k} db")))),
                    3e-3,
                );
                score.numbers(
                    &key(&format!("{k} shown")),
                    mini.shown(),
                    &floats(golden.get(&key(&format!("{k} shown")))),
                    2e-4,
                );
                let ceiling = [mini.ceiling_db().unwrap_or(-999.0)];
                score.numbers(
                    &key(&format!("{k} ceiling")),
                    &ceiling,
                    &floats(golden.get(&key(&format!("{k} ceiling")))),
                    3e-3,
                );
            }
        }
        // The same frame again: no time passed. Then silence, and a jump of the clock.
        mini.render(&frames[199], width, height, scale, white);
        score.numbers(&key("again"), mini.shown(), &floats(golden.get(&key("again"))), 2e-4);
        let mut quiet = AnalysisFrame::new(SpectrumReading::silent(1024));
        quiet.host_time = frames[199].host_time + 0.1;
        mini.render(&quiet, width, height, scale, white);
        score.numbers(&key("quiet"), mini.shown(), &floats(golden.get(&key("quiet"))), 2e-4);
        let mut late = frames[10].clone();
        late.host_time = frames[199].host_time + 30.0;
        mini.render(&late, width, height, scale, white);
        let mut shown = mini.shown().to_vec();
        shown.push(mini.ceiling_db().unwrap_or(-999.0));
        score.numbers(&key("late"), &shown, &floats(golden.get(&key("late"))), 3e-3);
    }
    let tilts: Vec<f32> = [0.0, 3.0, 4.5, 6.0].into_iter().map(tilt).collect();
    score.numbers("mini tilt", &tilts, &floats(golden.get("mini tilt")), 0.0);
    score.finish();
}

/// The picture: a hairline at silence, a silhouette with music, in the color that was asked for.
#[test]
fn the_picture_is_a_silhouette_over_a_hairline() {
    let frames = SyntheticFrames::sequence(120, Options::default());
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/joseon-render-review");
    std::fs::create_dir_all(&directory).unwrap();
    let orange = [1.0, 0.58, 0.10, 1.0];
    for (tag, width, height, scale) in [
        ("menu", 64.0, 18.0, 2.0),
        ("icon", 16.0, 16.0, 2.0),
        ("strip", 150.0, 40.0, 2.0),
    ] {
        let mut mini = MiniSpectrum::new();
        let silent = AnalysisFrame::new(SpectrumReading::silent(1024));
        let (pw, ph, rgba) = mini.render(&silent, width, height, scale, orange);
        let alpha = |rgba: &[u8], x: u32, y: u32| rgba[((y * pw + x) * 4 + 3) as usize];
        assert_eq!(alpha(&rgba, pw / 2, ph - 1), 255, "{tag}: the hairline");
        assert_eq!(alpha(&rgba, pw / 2, ph - 4), 0, "{tag}: nothing over it at silence");
        let mut picture = Vec::new();
        for frame in &frames {
            picture = mini.render(frame, width, height, scale, orange).2;
        }
        std::fs::write(
            directory.join(format!("mini-{tag}.png")),
            joseon_render::target::encode_png(pw, ph, &picture),
        )
        .unwrap();
        // The area under the loudest column is filled, in the color; the top row is clear.
        let loudest = mini
            .shown()
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .map(|(i, _)| i as u32)
            .unwrap();
        let x = (loudest * pw / mini.shown().len() as u32).min(pw - 1);
        let i = (((ph - 4) * pw + x) * 4) as usize;
        assert_eq!(&picture[i..i + 4], [255, 148, 26, 255], "{tag}: filled under the peak");
        assert!(
            (0..pw).all(|x| alpha(&picture, x, 0) == 0),
            "{tag}: the top row is clear"
        );
        let filled = picture.chunks_exact(4).filter(|p| p[3] > 128).count() as f32 / (pw * ph) as f32;
        assert!(
            filled > 0.08 && filled < 0.8,
            "{tag}: {filled} of the picture is filled"
        );
    }
}
