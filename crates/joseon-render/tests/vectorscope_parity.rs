//! Parity of the vectorscope with the macOS app: the pan field (pan, level,
//! tones, skirts, confidence), the layout of the panel at eight sizes, its
//! readouts, and the splats it sends to the GPU.
//! `tests/golden/vectorscope.txt` holds what the Swift code gives.

#[path = "../../joseon-core/tests/common/mod.rs"]
mod common;

use common::{floats, Lines, Score};
use joseon_core::contracts::AnalysisFrame;
use joseon_render::contracts::{PanelCursor, PanelKind, VectorscopeMode};
use joseon_render::offscreen::{Session, Settings};
use joseon_render::panel::Panel;
use joseon_render::synthetic::{Options, SyntheticFrames};
use joseon_render::vectorscope::pan_field::PanField;
use joseon_render::vectorscope::{balance_text, Arrangement, VectorscopePanel};
use joseon_render::{RenderContext, Theme};

/// Synthetic frames with two one-sided tones: one in the right channel that breathes, one low in the left channel.
fn toned_frames(count: usize) -> Vec<AnalysisFrame> {
    let mut frames = SyntheticFrames::sequence(count, Options::default());
    for (k, frame) in frames.iter_mut().enumerate() {
        let n = frame.spectrum.left.len();
        let (hi, lo) = (n * 7 / 10, n / 5);
        for i in 0..n {
            let a = i as f32 - hi as f32;
            let b = i as f32 - lo as f32;
            frame.spectrum.right[i] += 34.0 * (-a * a / 6.0).exp() * if k % 90 < 60 { 1.0 } else { 0.2 };
            frame.spectrum.left[i] += 28.0 * (-b * b / 10.0).exp();
        }
    }
    frames
}

fn every<T: Copy>(step: usize, v: &[T]) -> Vec<T> {
    v.iter().step_by(step).copied().collect()
}

fn as_floats<T: Copy + Into<f64>>(v: &[T]) -> Vec<f32> {
    v.iter().map(|x| (*x).into() as f32).collect()
}

#[test]
fn the_pan_field_matches_the_macos_app() {
    let golden = Lines::of("vectorscope.txt");
    let mut score = Score::default();
    let mut field = PanField::new();
    for (k, f) in toned_frames(240).iter().enumerate() {
        field.update(&f.spectrum, if k % 7 == 0 { 0.021 } else { 1.0 / 60.0 }, 48_000.0);
        if k % 40 == 39 {
            let n = field.count();
            let key = |name: &str| format!("field {k} {name}");
            // The frames differ by up to 0.001 dB: a pan is a ratio of powers, a level is in dB.
            score.numbers(
                &key("pan"),
                &every(3, field.pan()),
                &floats(golden.get(&key("pan"))),
                2e-3,
            );
            score.numbers(
                &key("level"),
                &every(3, field.level_db()),
                &floats(golden.get(&key("level"))),
                2e-3,
            );
            score.numbers(
                &key("tonal"),
                &as_floats(field.tonal()),
                &floats(golden.get(&key("tonal"))),
                0.0,
            );
            score.numbers(
                &key("dropped"),
                &as_floats(field.dropped()),
                &floats(golden.get(&key("dropped"))),
                0.0,
            );
            score.numbers(
                &key("confidence"),
                &every(2, field.confidence()),
                &floats(golden.get(&key("confidence"))),
                2e-3,
            );
            let peaks: Vec<f32> = field.tonal_peaks().iter().map(|p| *p as f32).collect();
            score.numbers(&key("peaks"), &peaks, &floats(golden.get(&key("peaks"))), 0.0);
            let light: Vec<f32> = (0..n).step_by(5).map(|i| field.light(i, -17.0, -71.0)).collect();
            score.numbers(&key("light"), &light, &floats(golden.get(&key("light"))), 1e-4);
        }
    }
    score.numbers(
        "field spread",
        &as_floats(field.spread()),
        &floats(golden.get("field spread")),
        0.0,
    );
    score.finish();
}

#[test]
fn the_balance_text_matches_the_macos_app() {
    let golden = Lines::of("vectorscope.txt");
    let ours: Vec<String> = [-1.0, -0.504, -0.19, -0.004, 0.0, 0.005, 0.0051, 0.33, 0.995, 1.0]
        .into_iter()
        .map(|b| balance_text(b, false))
        .collect();
    assert_eq!(ours.join("|"), golden.get("balance"));
}

#[test]
fn the_panel_matches_the_macos_app() {
    if RenderContext::shared().is_none() {
        println!("no GPU: skipped");
        return;
    }
    let golden = Lines::of("vectorscope.txt");
    let mut score = Score::default();
    let (scope, pan) = (VectorscopeMode::Lissajous, VectorscopeMode::PanSpectrum);
    type Case = (&'static str, VectorscopeMode, (f32, f32), Option<f32>);
    let cases: [Case; 8] = [
        ("scope-wide", scope, (900.0, 360.0), None),
        ("scope-tall", scope, (360.0, 520.0), None),
        ("scope-keys", scope, (420.0, 300.0), Some(1_000.0)),
        ("scope-small", scope, (260.0, 240.0), None),
        ("scope-table-cursor", scope, (1000.0, 420.0), Some(5_000.0)),
        ("pan-wide", pan, (900.0, 420.0), Some(440.0)),
        ("pan-tall", pan, (380.0, 560.0), None),
        ("pan-small", pan, (300.0, 200.0), None),
    ];
    let frames = toned_frames(200);
    for (tag, mode, (width, height), cursor_hz) in cases {
        let settings = Settings {
            vectorscope_mode: mode,
            cursor: cursor_hz.map(|hz| PanelCursor {
                is_pinned: true,
                ..PanelCursor::new(hz, PanelKind::Spectrum)
            }),
            ..Settings::default()
        };
        let mut session =
            Session::new(PanelKind::Vectorscope, width, height, 2.0, Theme::default(), &settings).unwrap();
        session
            .renderer
            .panel_as::<VectorscopePanel>()
            .unwrap()
            .0
            .keeps_splats_for_testing = true;
        session.feed(&frames);
        session.snapshot_rgba().unwrap();
        let (r, base) = session.renderer.panel_as::<VectorscopePanel>().unwrap();
        let arrangement = match r.arrangement() {
            Arrangement::TableBeside => 0.0,
            Arrangement::StripUnder => 1.0,
            Arrangement::KeysBeside => 2.0,
            Arrangement::KeysUnder => 3.0,
        };
        let mut layout = vec![arrangement];
        let field = r.field();
        layout.extend([field.x, field.y, field.w, field.h, r.pan_center_x(), r.pan_half()]);
        let axis = r.pan_axis_rect();
        layout.extend([axis.x, axis.y, axis.w, axis.h]);
        match r.pan_legend_rect() {
            Some(l) => layout.extend([l.x, l.y, l.w, l.h]),
            None => layout.extend([-1.0; 4]),
        }
        layout.extend([100.0, 1_000.0, 10_000.0].map(|hz| r.pan_y(hz)));
        let key = |name: &str| format!("{tag} {name}");
        score.numbers(&key("layout"), &layout, &floats(golden.get(&key("layout"))), 2e-3);
        score.numbers(&key("gain"), &[r.gain()], &floats(golden.get(&key("gain"))), 1e-4);
        let items: Vec<String> = r
            .cursor_items(base)
            .iter()
            .map(|item| format!("{} {}", item.rank, item.text))
            .collect();
        score.texts(&key("items"), &items, &golden.texts(&key("items")));
        score.same(&key("value"), &r.accessibility_value(base), golden.get(&key("value")));
        score.same(&key("label"), &r.accessibility_label(), golden.get(&key("label")));
        score.numbers(
            &key("splatCount"),
            &[r.last_splats.len() as f32],
            &floats(golden.get(&key("splatCount"))),
            0.0,
        );
        let splats: Vec<f32> = r.last_splats.iter().step_by(9).flatten().copied().collect();
        // Pan, axis position, light, height in pixels.
        score.numbers(&key("splats"), &splats, &floats(golden.get(&key("splats"))), 3e-3);
        let pan = r.cursor_pan(base).unwrap_or(-9.0);
        score.numbers(&key("cursorPan"), &[pan], &floats(golden.get(&key("cursorPan"))), 2e-3);
    }
    score.finish();
}
