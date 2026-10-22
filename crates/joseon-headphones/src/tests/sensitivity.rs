//! The sensitivity library holds published figures only. These tests defend that rule.
//!
//! Ported from `Tests/JoseonHeadphonesTests/SensitivityLibraryTests.swift`.

use super::spl_support::*;
use crate::calibration::from_measured_tone;
use crate::embedded_curves::{HD650, HEADPHONES};
use crate::library::HeadphoneLibrary;
use crate::sensitivity::{self, UNLISTED};
use crate::spl_estimator::SplEstimator;
use joseon_core::contracts::{HeadphoneSensitivity, SplEstimating, ThirdOctaveReading};
use std::collections::HashSet;

// MARK: The figures, as published

/// Sennheiser prints dB SPL at 1 V, which needs no conversion.
#[test]
fn sennheiser_figures_are_used_as_published() {
    near(sensitivity::hd600().db_spl_per_volt, 97.0, 1e-9);
    near(sensitivity::hd600().impedance_ohms, 300.0, 1e-9);
    near(sensitivity::hd650().db_spl_per_volt, 103.0, 1e-9);
    near(sensitivity::hd800s().db_spl_per_volt, 102.0, 1e-9);
    near(sensitivity::hd800s().impedance_ohms, 300.0, 1e-9);
}

/// dB/mW to dB/V is `+10·log10(1000 / Z)`. Focal: 104 dB/mW into 80 Ω.
#[test]
fn focal_conversion() {
    let s = sensitivity::focal_utopia();
    near(s.impedance_ohms, 80.0, 1e-9);
    near(s.db_spl_per_volt, 104.0 + 10.0 * (1000.0f64 / 80.0).log10(), 1e-9);
    near(s.db_spl_per_volt, 114.97, 0.01);
}

/// Audeze: 103 dB/mW into 20 Ω.
#[test]
fn audeze_conversion() {
    let s = sensitivity::lcd_x();
    near(s.impedance_ohms, 20.0, 1e-9);
    near(s.db_spl_per_volt, 103.0 + 10.0 * (1000.0f64 / 20.0).log10(), 1e-9);
    near(s.db_spl_per_volt, 119.99, 0.01);
}

/// Sony: 102 dB/mW into 48 Ω, the wired figure with the headset powered.
#[test]
fn sony_conversion() {
    let s = sensitivity::sony_wh1000_xm5();
    near(s.impedance_ohms, 48.0, 1e-9);
    near(s.db_spl_per_volt, 102.0 + 10.0 * (1000.0f64 / 48.0).log10(), 1e-9);
    near(s.db_spl_per_volt, 115.19, 0.01);
}

/// The conversion itself, where the answer is exact: 1000 Ω gives +0 dB.
#[test]
fn milliwatt_conversion_identity_at_one_kilo_ohm() {
    let s = HeadphoneSensitivity::from_db_per_milliwatt(100.0, 1_000.0, "synthetic test fixture");
    near(s.db_spl_per_volt, 100.0, 1e-9);
}

// MARK: Provenance

/// Every entry cites a manufacturer URL and the fetch date.
#[test]
fn every_entry_cites_its_source_and_date() {
    for (name, s) in sensitivity::by_curve_name() {
        assert!(
            s.source.contains("2026-09-21") || s.source.contains("2026-10-08") || s.source.contains("2026-10-21"),
            "{name} has no fetch date"
        );
        assert!(s.source.contains("fetched"), "{name} does not say it was fetched");
        assert!(
            [
                "sennheiser",
                "focal",
                "audeze",
                "sony",
                "beyerdynamic",
                "64audio",
                "abyss-headphones"
            ]
            .iter()
            .any(|maker| s.source.contains(maker)),
            "{name} does not cite a manufacturer domain"
        );
    }
}

/// Published figures are physically plausible: nothing here is wrong by 10 dB.
#[test]
fn every_figure_is_plausible() {
    for (name, s) in sensitivity::by_curve_name() {
        assert!(s.db_spl_per_volt > 80.0 && s.db_spl_per_volt < 135.0, "{name}");
        assert!(s.impedance_ohms > 4.0 && s.impedance_ohms < 1_000.0, "{name}");
    }
}

// MARK: What is deliberately missing

/// The library covers exactly the headphones whose makers publish a usable figure.
#[test]
fn the_library_holds_only_verified_headphones() {
    let names: HashSet<&str> = sensitivity::by_curve_name().into_iter().map(|e| e.0).collect();
    let expected: HashSet<&str> = [
        "Sennheiser HD 600",
        "Sennheiser HD 650",
        "Sennheiser HD 800 S",
        "Focal Utopia",
        "Audeze LCD-X (2021)",
        "Sony WH-1000XM5",
        "Beyerdynamic DT 900 Pro X",
        "64 Audio U12t (m15 module)",
        "64 Audio U12t (m20 module)",
        "64 Audio U12t (mX module)",
        "Abyss AB-1266 Phi TC",
    ]
    .into();
    assert_eq!(names, expected);
}

/// HiFiMAN publishes a bare "86dB" with no reference. It cannot be converted to
/// dB/V, so the Susvara Unveiled is absent. Nobody fills the gap from memory.
#[test]
fn hifiman_is_absent_because_the_figure_has_no_reference() {
    assert!(sensitivity::sensitivity_for_curve("HiFiMAN Susvara Unveiled").is_none());
    assert!(sensitivity::sensitivity_for_curve("HiFiMAN Susvara").is_none());
    let reason = sensitivity::reason_for_curve("HiFiMAN Susvara Unveiled").unwrap();
    assert!(reason.contains("86dB"));
}

/// Apple publishes nothing for the two AirPods models.
#[test]
fn airpods_are_absent() {
    assert!(sensitivity::sensitivity_for_curve("Apple AirPods Max").is_none());
    assert!(sensitivity::sensitivity_for_curve("Apple AirPods Pro 2").is_none());
    assert!(sensitivity::reason_for_curve("Apple AirPods Max").is_some());
}

/// The retired models are absent, and no figure of a similar model was used.
#[test]
fn retired_models_are_absent_and_nothing_was_substituted() {
    assert!(sensitivity::sensitivity_for_curve("Meze Empyrean (leather earpads)").is_none());
    assert!(sensitivity::sensitivity_for_curve("ZMF Verite").is_none());
    let meze = sensitivity::reason_for_curve("Meze Empyrean (leather earpads)").unwrap();
    assert!(
        meze.contains("Empyrean II"),
        "the reason must warn against the figure of the II"
    );
}

/// Every embedded curve has a figure or a stated reason, never both, never neither.
#[test]
fn every_embedded_curve_is_either_listed_or_explained() {
    for curve in HEADPHONES {
        let has_figure = sensitivity::sensitivity_for_curve(curve.name).is_some();
        let has_reason = sensitivity::reason_for_curve(curve.name).is_some();
        assert!(
            has_figure != has_reason,
            "{} is in neither list, or in both",
            curve.name
        );
    }
    assert_eq!(sensitivity::by_curve_name().len() + UNLISTED.len(), HEADPHONES.len());
}

/// Every "user must enter" entry names the page that was checked.
#[test]
fn every_unlisted_entry_names_what_was_checked() {
    for entry in UNLISTED {
        assert!(!entry.reason.is_empty(), "{}", entry.name);
        assert!(entry.checked_url.starts_with("https://"), "{}", entry.name);
    }
}

/// Lookup uses the same names as the curve library.
#[test]
fn lookup_uses_curve_library_names() {
    let library = HeadphoneLibrary::new("/nonexistent");
    for (name, _) in sensitivity::by_curve_name() {
        assert!(library.curve_named(name).is_some(), "no embedded curve named {name}");
    }
    for entry in UNLISTED {
        assert!(
            library.curve_named(entry.name).is_some(),
            "no embedded curve named {}",
            entry.name
        );
    }
}

// MARK: User-entered figures

/// A typed figure is marked "User", so it can never be mistaken for a published one.
#[test]
fn user_entered_figures_are_marked() {
    let typed = sensitivity::user_entered(90.0, 45.0);
    assert_eq!(typed.source, "User");
    assert!(sensitivity::is_user_entered(&typed));
    assert!(!sensitivity::is_user_entered(&sensitivity::hd600()));
    let from_mw = sensitivity::user_entered_per_milliwatt(86.0, 45.0);
    assert_eq!(from_mw.source, "User");
    near(from_mw.db_spl_per_volt, 86.0 + 10.0 * (1000.0f64 / 45.0).log10(), 1e-9);
}

/// A real chain end to end: HD 650 on a 2 V source, at a plausible listening
/// level. A coherence check on the whole library, not a measurement.
#[test]
fn a_published_headphone_gives_a_plausible_level() {
    let mut estimator = SplEstimator::new(
        HD650.curve(),
        sensitivity::hd650(),
        from_measured_tone("2 V source", 2.0, 0.0),
    );
    // Music at about −20 dBFS RMS spread over the band, which is a normal master.
    let mut bands = floor_bands();
    for (i, &hz) in ThirdOctaveReading::NOMINAL_CENTERS_HZ.iter().enumerate() {
        if hz >= 40.0 && hz <= 12_500.0 {
            bands[i] = -38.0;
        }
    }
    let reading = estimator.evaluate(&ThirdOctaveReading::new(bands.clone(), bands), 0.125, false);
    assert!(reading.level_a_fast > 60.0 && reading.level_a_fast < 120.0);
    println!("HD 650 on 2 V, −38 dBFS per band: {:.1} dBA", reading.level_a_fast);
}
