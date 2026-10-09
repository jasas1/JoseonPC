//! Port of the model tests of `HeadphoneModelTests.swift`, and of
//! `StressFlagTests.swift`, `StressFlagPlotTests.swift`,
//! `StressFlagWordingTests.swift`, `RealDataBehaviourTests.swift` and
//! `HeadphoneModelThreadSafetyTests.swift`.

use super::*;
use crate::curve::CurveInterpolator;
use crate::embedded_curves::{self, HEADPHONES};
use crate::library::HeadphoneLibrary;
use crate::model::HeadphoneModel;
use crate::stress::{flag_id, flag_text, StressContext, StressDetector, StressThresholds};
use joseon_core::contracts::{HeadphoneModeling, StressSeverity};
use std::collections::HashSet;
use std::time::{Duration, Instant};

const MINUS: &str = "\u{2212}";

fn model(curve: HeadphoneCurve, target: Option<HeadphoneCurve>, clock: &TestClock) -> HeadphoneModel {
    HeadphoneModel::with(curve, target, StressThresholds::default(), clock.reader())
}

/// Flat headphone against a flat target: nothing about the headphone can fire on its own.
fn flat_model(clock: &TestClock) -> HeadphoneModel {
    model(flat_curve("Flat test"), Some(flat_curve("Flat target")), clock)
}

fn has(flags: &[StressFlag], id: &str) -> bool {
    flags.iter().any(|flag| flag.id == id)
}

#[track_caller]
fn find<'a>(flags: &'a [StressFlag], id: &str) -> &'a StressFlag {
    flags.iter().find(|flag| flag.id == id).unwrap_or_else(|| {
        panic!(
            "{id} did not fire; got {:?}",
            flags.iter().map(|f| &f.id).collect::<Vec<_>>()
        )
    })
}

fn ids(flags: &[StressFlag]) -> Vec<&str> {
    flags.iter().map(|flag| flag.id.as_str()).collect()
}

/// One call of the flags with only a sub-bass level and a loudness.
fn flags_for(model: &mut HeadphoneModel, sub_bass: f32, loudness: Loud) -> Vec<StressFlag> {
    model.stress_flags(&silent_spectrum(256), &bands(sub_bass), &loudness.reading())
}

// MARK: Evaluate

#[test]
fn predicted_at_ear_is_mid_plus_response() {
    let mut model = HeadphoneModel::new(
        embedded_curves::SUSVARA_UNVEILED.curve(),
        Some(embedded_curves::HARMAN_OVER_EAR2018.curve()),
    );
    let spectrum = spectrum(512, |hz| -40.0 + 5.0 * hz.ln().sin());
    let r = model.evaluate(&spectrum, &bands(-120.0), &quiet_loudness());
    assert_eq!(
        (r.response_db.len(), r.target_db.len(), r.predicted_at_ear_db.len()),
        (512, 512, 512)
    );
    for i in 0..512 {
        assert_near(
            r.predicted_at_ear_db[i],
            spectrum.mid[i] + r.response_db[i],
            1e-4,
            &format!("bin {i}"),
        );
    }
    assert_eq!(r.model_name, "HiFiMAN Susvara Unveiled");
    assert_eq!(model.model_name(), "HiFiMAN Susvara Unveiled");
    assert!(r.has_target);
}

#[test]
fn target_is_zeros_without_a_target() {
    let mut model = HeadphoneModel::new(embedded_curves::HD600.curve(), None);
    let r = model.evaluate(&silent_spectrum(64), &bands(-120.0), &quiet_loudness());
    assert_eq!(r.target_db, vec![0.0; 64], "placeholder zeros");
    assert!(!r.has_target);
    assert!(!r.response_db.iter().all(|&level| level == 0.0));
    assert!(model.target().is_none() && model.normalized_target().is_none());
}

#[test]
fn resample_cache_survives_repeat_calls_and_follows_grid_changes() {
    let mut model = HeadphoneModel::new(
        embedded_curves::HD800S.curve(),
        Some(embedded_curves::HARMAN_OVER_EAR2018.curve()),
    );
    let a = silent_spectrum(128);
    let first = model.evaluate(&a, &bands(-120.0), &quiet_loudness());
    let second = model.evaluate(&a, &bands(-120.0), &quiet_loudness());
    assert_eq!(first.response_db, second.response_db);
    let b = silent_spectrum(256);
    let third = model.evaluate(&b, &bands(-120.0), &quiet_loudness());
    assert_eq!(third.response_db.len(), 256);
    assert_eq!(third.response_db, model.response_db(&b.frequencies));
}

#[test]
fn response_on_its_own_grid_is_the_normalized_curve() {
    let model = HeadphoneModel::new(embedded_curves::FOCAL_UTOPIA.curve(), None);
    let own_grid = model.normalized_curve().frequencies_hz.clone();
    for (got, want) in model
        .response_db(&own_grid)
        .iter()
        .zip(&model.normalized_curve().levels_db)
    {
        assert_near(*got, *want, 1e-4, "response");
    }
    assert_eq!(model.curve().name, "Focal Utopia");
}

#[test]
fn model_normalizes_both_curve_and_target() {
    let model = HeadphoneModel::new(curve("c", |_| -12.0), Some(curve("t", |_| 41.0)));
    assert_near(model.normalized_curve().reference_level_db(), 0.0, 1e-4, "curve");
    assert_near(
        model.normalized_target().unwrap().reference_level_db(),
        0.0,
        1e-4,
        "target",
    );
    assert_eq!(
        model.target().unwrap().levels_db[0],
        41.0,
        "the target as it was given is kept"
    );
    let flat = HeadphoneModel::new(curve("flat 3 dB", |_| 3.0), None);
    assert_near(
        CurveInterpolator::new(flat.normalized_curve().clone()).level(1_000.0),
        0.0,
        0.05,
        "flat",
    );
}

#[test]
fn evaluate_carries_the_flags_and_reset_drops_them() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    let reading = model.evaluate(
        &silent_spectrum(256),
        &bands(-120.0),
        &Loud {
            clip_count: 2,
            ..Default::default()
        }
        .reading(),
    );
    assert_eq!(ids(&reading.stress_flags), [flag_id::CLIPPED_SAMPLES]);
    // `HeadphoneModeling::reset` is what the engine calls on a measurement reset.
    let as_trait: &mut dyn HeadphoneModeling = &mut model;
    as_trait.reset();
    assert!(flags_for(&mut model, -120.0, Loud::default()).is_empty());
}

// MARK: Quiet baseline

#[test]
fn nothing_fires_on_quiet_flat_input() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    for _ in 0..5 {
        let flags = flags_for(&mut model, -120.0, Loud::default());
        assert!(flags.is_empty(), "quiet input raised {:?}", ids(&flags));
        clock.advance(1.0);
    }
}

#[test]
fn nothing_fires_on_moderate_music_with_a_well_behaved_headphone() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    let spectrum = spectrum(256, |hz| if hz < 40.0 { -95.0 } else { -45.0 });
    let loudness = Loud {
        true_peak_max: -1.5,
        plr: 14.0,
        measured_seconds: 90.0,
        integrated: -15.5,
        ..Default::default()
    };
    let flags = clock.settled(|| model.stress_flags(&spectrum, &bands(-40.0), &loudness.reading()));
    assert!(flags.is_empty(), "{:?}", ids(&flags));
}

// MARK: (a) sub-bass load

#[test]
fn sub_bass_load_needs_sustain_then_holds_then_clears() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    let up =
        |model: &mut HeadphoneModel, level: f32| has(&flags_for(model, level, Loud::default()), flag_id::SUB_BASS_LOAD);

    // Not yet: the condition must hold for 3 s. −20 is above the −24 dBFS default.
    assert!(!up(&mut model, -20.0));
    clock.advance(2.5);
    assert!(!up(&mut model, -20.0), "raised before the 3 s attack");
    clock.advance(0.6);
    let fired = flags_for(&mut model, -20.0, Loud::default());
    let flag = find(&fired, flag_id::SUB_BASS_LOAD);
    assert_eq!(flag.severity, StressSeverity::Watch);
    assert!(flag.detail.contains(&format!("{MINUS}20.0 dBFS")), "{}", flag.detail);
    assert!(flag.detail.contains("Flat test"), "{}", flag.detail);

    // Down to −25, inside the 2 dB release margin: the flag holds, however long it sits there.
    clock.advance(30.0);
    assert!(up(&mut model, -25.0), "the margin has to hold the flag up");
    // Clear of the margin: the 5 s release starts here.
    clock.advance(1.0);
    assert!(up(&mut model, -30.0));
    clock.advance(4.5);
    assert!(up(&mut model, -30.0), "cleared before the 5 s release");
    // Past the release: gone.
    clock.advance(0.6);
    assert!(!up(&mut model, -30.0));
}

/// A value that hovers on the threshold must give one continuous flag or none,
/// never a flag that blinks.
#[test]
fn sub_bass_load_does_not_flicker_when_the_level_hovers_on_the_threshold() {
    for starting_high in [true, false] {
        let clock = TestClock::default();
        let mut model = flat_model(&clock);
        // 60 s of −25 / −23 dBFS around the −24 threshold, a step every 0.5 s.
        let up: Vec<bool> = (0..120)
            .map(|step| {
                let high = (step % 2 == 0) == starting_high;
                let flags = flags_for(&mut model, if high { -23.0 } else { -25.0 }, Loud::default());
                clock.advance(0.5);
                has(&flags, flag_id::SUB_BASS_LOAD)
            })
            .collect();
        let edges = up.windows(2).filter(|pair| pair[0] != pair[1]).count();
        assert!(
            edges <= 1,
            "the flag changed state {edges} times while the level hovered"
        );
    }
}

/// The same hover, but from a level that really did raise the flag: it must
/// stay up for the whole hover.
#[test]
fn sub_bass_load_stays_up_through_a_hover_once_it_is_raised() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    let flags = clock.settled(|| flags_for(&mut model, -18.0, Loud::default()));
    assert!(has(&flags, flag_id::SUB_BASS_LOAD));
    let raised = model.flag_onsets()[flag_id::SUB_BASS_LOAD];
    for step in 0..120 {
        clock.advance(0.5);
        let flags = flags_for(&mut model, if step % 2 == 0 { -23.0 } else { -25.0 }, Loud::default());
        assert!(has(&flags, flag_id::SUB_BASS_LOAD), "dropped at step {step}");
    }
    assert_eq!(
        model.flag_onsets()[flag_id::SUB_BASS_LOAD],
        raised,
        "the onset must not restart"
    );
}

#[test]
fn sub_bass_load_goes_high_at_the_high_threshold() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    let flags = clock.settled(|| flags_for(&mut model, -10.0, Loud::default()));
    assert_eq!(find(&flags, flag_id::SUB_BASS_LOAD).severity, StressSeverity::High);
}

#[test]
fn sub_bass_load_detail_names_what_the_response_does_down_there() {
    let clock = TestClock::default();
    // A headphone that rolls off hard below 60 Hz.
    let mut model = model(
        curve("Rolled off", |hz| if hz < 60.0 { -10.0 } else { 0.0 }),
        None,
        &clock,
    );
    let flags = clock.settled(|| flags_for(&mut model, -18.0, Loud::default()));
    let detail = &find(&flags, flag_id::SUB_BASS_LOAD).detail;
    assert!(detail.contains("20–60 Hz"), "{detail}");
    assert!(
        detail.contains(&format!("{MINUS}10.0 dB")) || detail.contains(&format!("{MINUS}9.")),
        "{detail}"
    );
}

#[test]
fn a_curve_with_no_bass_points_says_so() {
    let clock = TestClock::default();
    let high_only = HeadphoneCurve {
        name: "Tweeter".into(),
        source: "synthetic".into(),
        frequencies_hz: (0..20).map(|i| 500.0 + 500.0 * i as f32).collect(),
        levels_db: vec![0.0; 20],
    };
    let mut model = model(high_only, None, &clock);
    assert_eq!(model.stress_context().mean_sub_bass_response_db(), None);
    let flags = clock.settled(|| flags_for(&mut model, -18.0, Loud::default()));
    assert!(find(&flags, flag_id::SUB_BASS_LOAD)
        .detail
        .contains("Tweeter is not measured below 60 Hz."));
}

// MARK: (b) deep bass under-delivery

fn deep_target(top_hz: f32, lift: f32) -> HeadphoneCurve {
    curve("Deep target", move |hz| if hz < top_hz { lift } else { 0.0 })
}

#[test]
fn deep_bass_under_delivery_fires_and_clears() {
    let clock = TestClock::default();
    // Headphone flat, target wants +12 dB under 40 Hz: a 12 dB shortfall.
    let mut model = model(flat_curve("Flat test"), Some(deep_target(40.0, 12.0)), &clock);
    let content = spectrum(256, |hz| if hz < 40.0 { -35.0 } else { -90.0 });
    let flags = clock.settled(|| model.stress_flags(&content, &bands(-40.0), &quiet_loudness()));
    let flag = find(&flags, flag_id::SUB_BASS_UNDER_DELIVERY);
    assert_eq!(flag.severity, StressSeverity::Watch);
    assert!(
        flag.detail.contains("Deep target") && flag.detail.contains("40.0 Hz"),
        "{}",
        flag.detail
    );

    let cleared = clock.settled_for(5.01, || flags_for(&mut model, -120.0, Loud::default()));
    assert!(!has(&cleared, flag_id::SUB_BASS_UNDER_DELIVERY));
}

#[test]
fn deep_bass_under_delivery_needs_content_and_a_target() {
    let clock = TestClock::default();
    let mut with_target = model(flat_curve("Flat test"), Some(deep_target(40.0, 12.0)), &clock);
    let no_deep_content = spectrum(256, |hz| if hz < 40.0 { -95.0 } else { -30.0 });
    let flags = clock.settled(|| with_target.stress_flags(&no_deep_content, &bands(-60.0), &quiet_loudness()));
    assert!(!has(&flags, flag_id::SUB_BASS_UNDER_DELIVERY));

    let mut no_target = model(flat_curve("Flat test"), None, &clock);
    let content = spectrum(256, |hz| if hz < 40.0 { -20.0 } else { -90.0 });
    let flags = clock.settled(|| no_target.stress_flags(&content, &bands(-60.0), &quiet_loudness()));
    assert!(!has(&flags, flag_id::SUB_BASS_UNDER_DELIVERY));
}

// MARK: (c) treble hot spot

fn peaky(lift: f32) -> HeadphoneCurve {
    curve("Peaky", move |hz| if hz > 5_000.0 && hz < 7_000.0 { lift } else { 0.0 })
}

fn bright() -> SpectrumReading {
    spectrum(256, |hz| if hz > 4_000.0 && hz < 10_000.0 { -30.0 } else { -90.0 })
}

#[test]
fn treble_hot_spot_fires_and_clears() {
    let clock = TestClock::default();
    // +6 dB between 5 and 7 kHz over a flat target.
    let mut model = model(peaky(6.0), Some(flat_curve("Flat target")), &clock);
    let flags = clock.settled(|| model.stress_flags(&bright(), &bands(-120.0), &quiet_loudness()));
    let flag = find(&flags, flag_id::TREBLE_HOT_SPOT);
    assert_eq!(
        flag.severity,
        StressSeverity::Watch,
        "+6 dB is under the +8 dB high threshold"
    );
    assert!(
        flag.detail.contains("+6.0 dB") && flag.detail.contains("sibilance"),
        "{}",
        flag.detail
    );

    let quiet = clock.settled_for(5.01, || flags_for(&mut model, -120.0, Loud::default()));
    assert!(!has(&quiet, flag_id::TREBLE_HOT_SPOT));
}

#[test]
fn treble_hot_spot_needs_music_energy_up_there_and_goes_high_at_eight_db() {
    let clock = TestClock::default();
    let mut model = model(peaky(6.0), Some(flat_curve("Flat target")), &clock);
    let dark = spectrum(256, |hz| if hz < 2_000.0 { -20.0 } else { -90.0 });
    let flags = clock.settled(|| model.stress_flags(&dark, &bands(-120.0), &quiet_loudness()));
    assert!(!has(&flags, flag_id::TREBLE_HOT_SPOT));

    let mut hot = self::model(peaky(9.0), Some(flat_curve("Flat target")), &clock);
    let flags = clock.settled(|| hot.stress_flags(&bright(), &bands(-120.0), &quiet_loudness()));
    assert_eq!(find(&flags, flag_id::TREBLE_HOT_SPOT).severity, StressSeverity::High);
}

// MARK: (d) inter-sample overs, (f) clipped samples

#[test]
fn inter_sample_overs_latch_until_reset() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    let flags = flags_for(
        &mut model,
        -120.0,
        Loud {
            true_peak_max: 1.3,
            ..Default::default()
        },
    );
    let flag = find(&flags, flag_id::INTER_SAMPLE_OVERS);
    assert_eq!(flag.severity, StressSeverity::High);
    assert!(flag.detail.contains("+1.30 dBTP"), "{}", flag.detail);
    assert!(
        flag.detail.ends_with("drop the player volume about 2 dB."),
        "{}",
        flag.detail
    );
    // An over is an event: it stays up however long the signal behaves afterwards.
    clock.advance(600.0);
    let calm = Loud {
        true_peak_max: -3.0,
        ..Default::default()
    };
    assert!(has(&flags_for(&mut model, -120.0, calm), flag_id::INTER_SAMPLE_OVERS));
    model.reset_flags();
    assert!(!has(&flags_for(&mut model, -120.0, calm), flag_id::INTER_SAMPLE_OVERS));
}

#[test]
fn clipped_samples_latch_until_reset() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    let flags = flags_for(
        &mut model,
        -120.0,
        Loud {
            clip_count: 3,
            ..Default::default()
        },
    );
    let flag = find(&flags, flag_id::CLIPPED_SAMPLES);
    assert_eq!(flag.severity, StressSeverity::High);
    assert!(flag.detail.contains("3 runs"), "{}", flag.detail);
    // Clipping happened: the flag latches until the measurement resets.
    clock.advance(600.0);
    assert!(has(
        &flags_for(&mut model, -120.0, Loud::default()),
        flag_id::CLIPPED_SAMPLES
    ));
    model.reset_flags();
    assert!(flags_for(&mut model, -120.0, Loud::default()).is_empty());
}

// MARK: (e) dense master

/// A minute of silence has PLR 0 and no integrated value. That is "no measurement", not a dense master.
#[test]
fn silence_is_not_a_dense_master() {
    let clock = TestClock::default();
    let mut model = model(flat_curve("Flat test"), None, &clock);
    let silence = Loud {
        plr: 0.0,
        measured_seconds: 120.0,
        ..Default::default()
    };
    assert!(!silence.reading().is_integrated_valid);
    let flags = clock.settled(|| flags_for(&mut model, -120.0, silence));
    assert!(!has(&flags, flag_id::DENSE_MASTER));
}

#[test]
fn dense_master_needs_thirty_seconds_then_fires_and_clears() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    let dense = Loud {
        true_peak_max: -0.2,
        plr: 6.1,
        measured_seconds: 12.0,
        integrated: -6.3,
        ..Default::default()
    };
    assert!(!has(
        &clock.settled(|| flags_for(&mut model, -120.0, dense)),
        flag_id::DENSE_MASTER
    ));

    let measured = Loud {
        measured_seconds: 47.0,
        ..dense
    };
    let flags = clock.settled(|| flags_for(&mut model, -120.0, measured));
    let flag = find(&flags, flag_id::DENSE_MASTER);
    assert_eq!(flag.severity, StressSeverity::Info);
    assert!(
        flag.detail.contains("6.1 dB") && flag.detail.contains("47 s"),
        "{}",
        flag.detail
    );

    // PLR back over 8 + 2 dB for the release time. The flag needs a live
    // measurement to clear as well as to fire.
    let open = Loud {
        true_peak_max: -0.2,
        plr: 14.0,
        measured_seconds: 60.0,
        integrated: -18.0,
        ..Default::default()
    };
    let cleared = clock.settled_for(5.01, || flags_for(&mut model, -120.0, open));
    assert!(!has(&cleared, flag_id::DENSE_MASTER));
}

// MARK: Ordering, stability, onsets

#[test]
fn high_severity_comes_first() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    let loudness = Loud {
        true_peak_max: 0.6,
        plr: 5.0,
        clip_count: 2,
        measured_seconds: 60.0,
        integrated: -4.4,
    };
    let flags = clock.settled(|| flags_for(&mut model, -120.0, loudness));
    assert_eq!(
        ids(&flags),
        [
            flag_id::INTER_SAMPLE_OVERS,
            flag_id::CLIPPED_SAMPLES,
            flag_id::DENSE_MASTER
        ]
    );
    let severities: Vec<StressSeverity> = flags.iter().map(|flag| flag.severity).collect();
    assert_eq!(
        severities,
        [StressSeverity::High, StressSeverity::High, StressSeverity::Info]
    );
}

#[test]
fn flags_are_stable_while_held() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    let first = flags_for(
        &mut model,
        -120.0,
        Loud {
            clip_count: 1,
            ..Default::default()
        },
    );
    clock.advance(1.0);
    assert_eq!(first, flags_for(&mut model, -120.0, Loud::default()));
    assert!(first[0].detail.contains("1 run "), "{}", first[0].detail);

    // The texts stay still: nothing in a held flag ticks with the clock.
    model.reset_flags();
    let raised = clock.settled(|| flags_for(&mut model, -12.0, Loud::default()));
    assert!(!raised.is_empty());
    for _ in 0..20 {
        clock.advance(1.0);
        assert_eq!(
            flags_for(&mut model, -12.0, Loud::default()),
            raised,
            "a flag text moved on its own"
        );
    }
}

/// `flag_onsets` carries the "since mm:ss" a popover wants, so no flag text has
/// to rewrite itself every second to show it.
#[test]
fn flag_onsets_report_when_each_flag_went_up() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    let clipped = Loud {
        clip_count: 1,
        ..Default::default()
    };
    assert!(model.flag_onsets().is_empty());
    // A clip at t = 2: an event flag, up at once.
    clock.advance(2.0);
    flags_for(&mut model, -120.0, clipped);
    assert_eq!(model.flag_onsets()[flag_id::CLIPPED_SAMPLES], 2.0);
    // Sub-bass load from t = 5, so it goes up at t = 8, when the attack is served.
    clock.advance(3.0);
    flags_for(&mut model, -12.0, clipped);
    assert!(
        !model.flag_onsets().contains_key(flag_id::SUB_BASS_LOAD),
        "up before its attack"
    );
    clock.advance(3.01);
    flags_for(&mut model, -12.0, clipped);
    assert!((model.flag_onsets()[flag_id::SUB_BASS_LOAD] - 8.01).abs() < 0.001);
    assert_eq!(clock.now(), 8.01);
    // The clip flag keeps its own, older onset.
    assert_eq!(model.flag_onsets()[flag_id::CLIPPED_SAMPLES], 2.0);
    // Once a flag really clears, its onset goes with it.
    clock.settled_for(5.01, || flags_for(&mut model, -40.0, clipped));
    assert!(!model.flag_onsets().contains_key(flag_id::SUB_BASS_LOAD));
    assert_eq!(model.flag_onsets()[flag_id::CLIPPED_SAMPLES], 2.0);
    model.reset_flags();
    assert!(model.flag_onsets().is_empty());
}

#[test]
fn thresholds_are_tunable() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    model.thresholds.sub_bass_load_dbfs = -60.0;
    model.thresholds.sub_bass_sustain_seconds = 0.0;
    assert!(has(
        &flags_for(&mut model, -50.0, Loud::default()),
        flag_id::SUB_BASS_LOAD
    ));
}

// MARK: Where a flag belongs on the plot
//
// The flags that are about a part of the spectrum carry the span they were
// measured over and a short number, so a panel can shade the band and label it
// without inventing anything. Whole-signal flags carry neither.

#[test]
fn sub_bass_load_carries_its_band_and_level() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    let flags = clock.settled(|| flags_for(&mut model, -9.0, Loud::default()));
    let flag = find(&flags, flag_id::SUB_BASS_LOAD);
    assert_eq!(
        flag.frequency_range_hz,
        Some(20.0..=60.0),
        "the sub-bass band the level is measured over"
    );
    assert_eq!(flag.plot_label, format!("{MINUS}9.0 dBFS"));
}

#[test]
fn under_delivery_carries_the_deep_bass_span_and_the_shortfall() {
    for top_hz in [40.0, 50.0] {
        let clock = TestClock::default();
        let mut model = model(flat_curve("Flat test"), Some(deep_target(top_hz, 7.0)), &clock);
        // The span follows the threshold, so the shading can never disagree with the detail text.
        model.thresholds.deep_bass_top_hz = top_hz;
        let content = spectrum(256, move |hz| if hz < top_hz { -35.0 } else { -90.0 });
        let flags = clock.settled(|| model.stress_flags(&content, &bands(-40.0), &quiet_loudness()));
        let flag = find(&flags, flag_id::SUB_BASS_UNDER_DELIVERY);
        assert_eq!(
            flag.frequency_range_hz,
            Some(20.0..=top_hz),
            "the span the shortfall is averaged over"
        );
        assert_eq!(flag.plot_label, format!("{MINUS}7.0 dB vs Deep target"));
    }
}

#[test]
fn treble_hot_spot_carries_the_peak_span_and_the_excess() {
    let clock = TestClock::default();
    let mut model = model(peaky(4.6), Some(flat_curve("Flat target")), &clock);
    let flags = clock.settled(|| model.stress_flags(&bright(), &bands(-120.0), &quiet_loudness()));
    let flag = find(&flags, flag_id::TREBLE_HOT_SPOT);
    let range = flag.frequency_range_hz.clone().unwrap();
    // The detail text names the peak frequency; the span has to be centred on it.
    let centre = (range.start() * range.end()).sqrt();
    assert!(centre > 5_000.0 && centre < 7_000.0);
    let sixth = (1.0f32 / 6.0).exp2();
    assert_near(
        range.end() / range.start(),
        sixth * sixth,
        0.02,
        "a third of an octave wide",
    );
    assert!(
        flag.detail.contains(&flag_text::hz(centre)),
        "{} vs centre {centre}",
        flag.detail
    );
    assert_eq!(flag.plot_label, "+4.6 dB vs Flat target");
}

/// The span never leaves the treble window, even for a peak that sits on its edge.
#[test]
fn treble_hot_spot_span_stays_inside_the_treble_window() {
    let clock = TestClock::default();
    let edge = curve("Peaky", |hz| if hz >= 9_000.0 { 9.0 } else { 0.0 });
    let mut model = model(edge, Some(flat_curve("Flat target")), &clock);
    let flags = clock.settled(|| model.stress_flags(&bright(), &bands(-120.0), &quiet_loudness()));
    let range = find(&flags, flag_id::TREBLE_HOT_SPOT)
        .frequency_range_hz
        .clone()
        .unwrap();
    assert!(*range.start() >= 4_000.0 && *range.end() <= 10_000.0 && range.start() < range.end());
}

#[test]
fn whole_signal_flags_have_no_span_and_no_label() {
    let clock = TestClock::default();
    let mut model = flat_model(&clock);
    let loudness = Loud {
        true_peak_max: 1.2,
        plr: 5.0,
        clip_count: 3,
        measured_seconds: 120.0,
        integrated: -6.0,
    };
    let flags = clock.settled(|| flags_for(&mut model, -120.0, loudness));
    for id in [
        flag_id::INTER_SAMPLE_OVERS,
        flag_id::DENSE_MASTER,
        flag_id::CLIPPED_SAMPLES,
    ] {
        let flag = find(&flags, id);
        assert_eq!(flag.frequency_range_hz, None, "{id} is about the whole signal");
        assert_eq!(flag.plot_label, "", "{id} has nothing to label a band with");
    }
}

/// Every span a flag reports is a real range inside the audible band.
#[test]
fn every_span_is_sane() {
    let clock = TestClock::default();
    let mut model = model(peaky(9.0), Some(deep_target(40.0, 12.0)), &clock);
    let busy = spectrum(256, |hz| {
        if hz < 40.0 || (hz > 4_000.0 && hz < 10_000.0) {
            -25.0
        } else {
            -60.0
        }
    });
    let loudness = Loud {
        true_peak_max: 1.2,
        clip_count: 2,
        ..Default::default()
    }
    .reading();
    let flags = clock.settled(|| model.stress_flags(&busy, &bands(-12.0), &loudness));
    assert!(flags.len() > 2);
    for flag in &flags {
        match &flag.frequency_range_hz {
            None => assert_eq!(flag.plot_label, "", "{} labels a band it does not have", flag.id),
            Some(range) => {
                assert!(
                    *range.start() >= 20.0 && *range.end() <= 20_000.0 && range.start() < range.end(),
                    "{}",
                    flag.id
                );
                assert!(!flag.plot_label.is_empty(), "{} shades a band with no label", flag.id);
            }
        }
    }
}

// MARK: Wording
//
// A flag says what is known, and says each number once. The app sees the
// signal and knows the response of the headphone. It does not know the
// playback level, so it knows nothing about driver excursion, and no flag can
// claim it.

/// A rolled-off, treble-peaked headphone against a flat target, so every flag fires.
fn all_flags() -> Vec<StressFlag> {
    let clock = TestClock::default();
    let mut detector = StressDetector::new(clock.reader());
    let frequencies: Vec<f32> = (0..200).map(|i| 20.0 * 1_000f32.powf(i as f32 / 199.0)).collect();
    let levels: Vec<f32> = frequencies
        .iter()
        .map(|&f| {
            if f < 60.0 {
                -9.37
            } else if f > 5_800.0 && f < 6_500.0 {
                5.13
            } else {
                0.0
            }
        })
        .collect();
    let context = StressContext::new(
        "Test phone".into(),
        Some("Flat target".into()),
        frequencies.clone(),
        levels,
        Some(vec![0.0; frequencies.len()]),
    );
    let spectrum = spectrum(512, |f| {
        if (20.0..=60.0).contains(&f) {
            -21.64
        } else if (4_000.0..=10_000.0).contains(&f) {
            -33.26
        } else {
            SpectrumReading::FLOOR_DB
        }
    });
    let loudness = Loud {
        true_peak_max: 0.37,
        plr: 6.14,
        clip_count: 2,
        measured_seconds: 47.0,
        integrated: -7.92,
    }
    .reading();
    // Level flags need 3 s of a true condition, so hold it 4 s.
    let mut flags = Vec::new();
    for _ in 0..5 {
        flags = detector.evaluate(
            &spectrum,
            &bands(-17.46),
            &loudness,
            &StressThresholds::default(),
            &context,
        );
        clock.advance(1.0);
    }
    flags
}

/// The numbers in a text, each with its sign: "+4.6", "−7.2", "40.0".
fn numbers(text: &str) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    let mut found = Vec::new();
    let mut i = 0;
    while i < chars.len() {
        if !chars[i].is_ascii_digit() {
            i += 1;
            continue;
        }
        let mut number = String::new();
        if i > 0 && (chars[i - 1] == '+' || chars[i - 1] == '\u{2212}') {
            number.push(chars[i - 1]);
        }
        while i < chars.len() && chars[i].is_ascii_digit() {
            number.push(chars[i]);
            i += 1;
        }
        if i + 1 < chars.len() && chars[i] == '.' && chars[i + 1].is_ascii_digit() {
            number.push('.');
            i += 1;
            while i < chars.len() && chars[i].is_ascii_digit() {
                number.push(chars[i]);
                i += 1;
            }
        }
        found.push(number);
    }
    found
}

#[test]
fn every_flag_fires_and_none_claims_anything_about_the_driver() {
    let flags = all_flags();
    assert_eq!(
        ids(&flags).into_iter().collect::<HashSet<_>>(),
        flag_id::ALL.into_iter().collect::<HashSet<_>>()
    );
    for flag in &flags {
        let text = format!("{} {} {}", flag.title, flag.detail, flag.plot_label).to_lowercase();
        for word in [
            "excursion",
            "driver",
            "bottom",
            "damage",
            "overload",
            "you will not hear",
        ] {
            assert!(!text.contains(word), "{} says \"{word}\": {}", flag.id, flag.detail);
        }
    }
}

#[test]
fn sub_bass_flag_says_what_is_known() {
    let flags = all_flags();
    let flag = find(&flags, flag_id::SUB_BASS_LOAD);
    assert!(flag.detail.contains("in the signal"), "{}", flag.detail);
    assert!(
        flag.detail.contains(&format!("{MINUS}17.5 dBFS")),
        "the level of sub-bass in the signal: {}",
        flag.detail
    );
    assert!(
        flag.detail
            .contains(&format!("{MINUS}9.4 dB against its own level at 1.00 kHz")),
        "{}",
        flag.detail
    );
    assert!(flag.detail.contains("does not know"), "{}", flag.detail);
    assert_eq!(flag.plot_label, format!("{MINUS}17.5 dBFS"));
}

/// Every number in the plot label is in the detail, written the same way, and
/// so is every number in the title. One quantity, one rounding.
#[test]
fn numbers_agree_across_title_detail_and_plot_label() {
    for flag in all_flags() {
        let in_detail = numbers(&flag.detail);
        for n in numbers(&flag.plot_label).into_iter().chain(numbers(&flag.title)) {
            assert!(
                in_detail.contains(&n),
                "{}: \"{n}\" is on the plot or in the title but not in the detail: {}",
                flag.id,
                flag.detail
            );
        }
        let all: Vec<char> = format!("{} {} {}", flag.title, flag.detail, flag.plot_label)
            .chars()
            .collect();
        let ascii_minus = all.windows(2).any(|pair| pair[0] == '-' && pair[1].is_ascii_digit());
        assert!(!ascii_minus, "{} writes a minus as an ASCII hyphen", flag.id);
    }
}

/// Measured and threshold frequencies: one decimal under 1 kHz, x.xx kHz from
/// there up. (Band names such as "20–60 Hz" are exact edges, not measurements.)
#[test]
fn frequency_and_number_format() {
    assert_eq!(flag_text::hz(40.0), "40.0 Hz");
    assert_eq!(flag_text::hz(999.94), "999.9 Hz");
    assert_eq!(flag_text::hz(999.96), "1.00 kHz");
    assert_eq!(flag_text::hz(6_104.0), "6.10 kHz");
    assert_eq!(flag_text::db(-0.04), "0.0");
    assert_eq!(flag_text::db(-21.3), format!("{MINUS}21.3"));
    assert_eq!(flag_text::signed_db(-7.24), format!("{MINUS}7.2"));
    assert_eq!(flag_text::signed_db(5.13), "+5.1");
    assert_eq!(flag_text::signed_db(0.0), "+0.0");
    assert_eq!(flag_text::signed_db2(1.3), "+1.30");

    let flags = all_flags();
    for flag in &flags {
        let text = flag.detail.replace("20–60 Hz", "");
        let words: Vec<&str> = text.split_whitespace().collect();
        for pair in words.windows(2) {
            let unit = pair[1].trim_end_matches(['.', ',', ':']);
            if unit != "Hz" && unit != "kHz" {
                continue;
            }
            let decimals = pair[0].split('.').nth(1).map_or(0, str::len);
            assert_eq!(
                decimals,
                if unit == "kHz" { 2 } else { 1 },
                "{}: {} {unit} in: {}",
                flag.id,
                pair[0],
                flag.detail
            );
            if unit == "Hz" {
                assert!(pair[0].parse::<f32>().unwrap() < 1_000.0);
            }
        }
    }
    let hot = find(&flags, flag_id::TREBLE_HOT_SPOT);
    assert!(hot.detail.contains("kHz at +5.1 dB vs Flat target"), "{}", hot.detail);
    assert_eq!(hot.plot_label, "+5.1 dB vs Flat target");
}

// MARK: The embedded data and the flag logic
//
// The expected values were read off the measurements, not chosen to make a test pass.

fn library_model(headphone: &str, clock: &TestClock) -> HeadphoneModel {
    let library = HeadphoneLibrary::new("/nonexistent");
    model(
        library.curve_named(headphone).unwrap(),
        library.target_named("Harman over-ear 2018"),
        clock,
    )
}

/// Pink-ish music with real content everywhere, near −20 dBFS in the low mids.
fn music_flags(model: &mut HeadphoneModel, clock: &TestClock) -> Vec<StressFlag> {
    let music = spectrum(1_024, |hz| -20.0 - 4.5 * (hz.max(20.0) / 1_000.0).log2());
    let loudness = Loud {
        true_peak_max: -2.0,
        plr: 12.0,
        measured_seconds: 60.0,
        ..Default::default()
    }
    .reading();
    clock.settled(|| model.stress_flags(&music, &bands(-30.0), &loudness))
}

#[test]
fn closed_back_consumer_cans_push_bass_and_open_reference_cans_do_not() {
    let clock = TestClock::default();
    // Sub-bass shelf relative to the 1 kHz level of each headphone, 20–60 Hz.
    let shelf = |name: &str| {
        library_model(name, &clock)
            .stress_context()
            .mean_sub_bass_response_db()
            .unwrap()
    };
    assert!(shelf("Sony WH-1000XM5") > 5.0, "WH-1000XM5 measures a large bass shelf");
    assert!(shelf("Apple AirPods Max") > 2.0, "AirPods Max measures a bass shelf");
    assert!(
        shelf("Sennheiser HD 800 S") < 0.0,
        "HD 800 S measures below its own 1 kHz level in the sub-bass"
    );
}

#[test]
fn hd800s_raises_deep_bass_under_delivery_against_harman_and_bass_boosted_cans_do_not() {
    let clock = TestClock::default();
    let mut hd800s = library_model("Sennheiser HD 800 S", &clock);
    assert!(
        hd800s.stress_context().mean_shortfall_db(20.0, 40.0).unwrap() > 6.0,
        "measured shortfall under 40 Hz"
    );
    assert!(has(&music_flags(&mut hd800s, &clock), flag_id::SUB_BASS_UNDER_DELIVERY));
    for name in ["Sony WH-1000XM5", "Apple AirPods Max"] {
        let mut boosted = library_model(name, &clock);
        assert!(
            !has(&music_flags(&mut boosted, &clock), flag_id::SUB_BASS_UNDER_DELIVERY),
            "{name} is not short of deep bass"
        );
    }
}

#[test]
fn wh1000xm5_raises_a_treble_hot_spot() {
    let clock = TestClock::default();
    let mut sony = library_model("Sony WH-1000XM5", &clock);
    let (excess, hz) = sony.stress_context().max_excess_over_target(4_000.0, 10_000.0).unwrap();
    assert!(excess > 4.0 && (4_000.0..=10_000.0).contains(&hz));
    assert!(has(&music_flags(&mut sony, &clock), flag_id::TREBLE_HOT_SPOT));
}

/// The listening rig this app is built for: nothing shouts at the user.
#[test]
fn susvara_unveiled_is_calm_against_harman_on_ordinary_music() {
    let clock = TestClock::default();
    let mut susvara = library_model("HiFiMAN Susvara Unveiled", &clock);
    assert_near(
        susvara.stress_context().mean_sub_bass_response_db().unwrap(),
        0.0,
        2.0,
        "sub-bass response",
    );
    let flags = music_flags(&mut susvara, &clock);
    assert!(flags.is_empty(), "unexpected flags: {:?}", ids(&flags));
}

#[test]
fn normalized_response_is_sane_across_every_embedded_headphone() {
    for curve in HEADPHONES {
        let model = HeadphoneModel::new(curve.curve(), Some(embedded_curves::HARMAN_OVER_EAR2018.curve()));
        let on_grid = model.response_db(&grid(1_024));
        assert!(
            on_grid
                .iter()
                .all(|level| level.is_finite() && *level > -60.0 && *level < 40.0),
            "{}",
            curve.name
        );
    }
}

// MARK: Threads and cost

fn loud_sub_bass_input() -> (SpectrumReading, BandEnergy, LoudnessReading) {
    let spectrum = spectrum(1024, |hz| if hz < 60.0 { -10.0 } else { -40.0 });
    let loudness = Loud {
        true_peak_max: 0.5,
        plr: 6.0,
        measured_seconds: 60.0,
        integrated: -9.0,
        ..Default::default()
    };
    (spectrum, bands(-8.0), loudness.reading())
}

fn rolled_model() -> HeadphoneModel {
    HeadphoneModel::new(
        curve("Rolled", |hz| if hz < 60.0 { -12.0 } else { 0.0 }),
        Some(flat_curve("Target")),
    )
}

/// `evaluate` runs on the analysis thread while the app changes thresholds and
/// resets flags from another. The engine shares a model as `Arc<Mutex<...>>`.
#[test]
fn evaluate_while_thresholds_and_flags_change_on_other_threads() {
    let model = Arc::new(Mutex::new(rolled_model()));
    let (spectrum, bands, loudness) = loud_sub_bass_input();
    let other_grid = super::spectrum(512, |_| -30.0);
    let end = Instant::now() + Duration::from_millis(600);
    let mut evaluations = 0;
    std::thread::scope(|scope| {
        // "Main thread": thresholds, resets, a second grid (which empties both caches).
        for worker in 0..2 {
            let (model, other_grid, bands, loudness) = (&model, &other_grid, &bands, &loudness);
            scope.spawn(move || {
                let mut i = 0usize;
                while Instant::now() < end {
                    let mut model = model.lock().unwrap();
                    model.thresholds.hold_seconds = (i % 5) as f64;
                    model.thresholds.sub_bass_load_dbfs = -24.0 - (i % 7) as f32;
                    if i % 3 == worker {
                        model.reset_flags();
                    }
                    if i % 11 == 0 {
                        model.stress_flags(other_grid, bands, loudness);
                    }
                    i += 1;
                }
            });
        }
        // "Analysis thread".
        while Instant::now() < end {
            let reading = model.lock().unwrap().evaluate(&spectrum, &bands, &loudness);
            assert_eq!(
                (reading.predicted_at_ear_db.len(), reading.response_db.len()),
                (1024, 1024)
            );
            evaluations += 1;
        }
    });
    assert!(evaluations > 100, "{evaluations} evaluations");
}

/// Budget: `evaluate` stays under 0.2 ms on a 1024-bin grid. Checked in release
/// builds, as the fastest of five runs. `JOSEON_TIMING_SLACK` is a factor for
/// a slower machine.
#[test]
fn evaluate_stays_under_budget() {
    let mut model = rolled_model();
    let (spectrum, bands, loudness) = loud_sub_bass_input();
    for _ in 0..50 {
        model.evaluate(&spectrum, &bands, &loudness);
    }
    let (runs, repeats) = (2_000, if cfg!(debug_assertions) { 1 } else { 5 });
    let fastest = (0..repeats)
        .map(|_| {
            let start = Instant::now();
            for _ in 0..runs {
                std::hint::black_box(model.evaluate(&spectrum, &bands, &loudness));
            }
            start.elapsed().as_secs_f64() * 1e3 / runs as f64
        })
        .fold(f64::INFINITY, f64::min);
    println!("HeadphoneModel::evaluate: {fastest:.4} ms per call, 1024 bins");
    let slack = std::env::var("JOSEON_TIMING_SLACK")
        .ok()
        .and_then(|text| text.parse::<f64>().ok())
        .unwrap_or(1.0);
    if !cfg!(debug_assertions) {
        assert!(
            fastest < 0.2 * slack.max(1.0),
            "evaluate too slow: {fastest} ms per call"
        );
    }
}
