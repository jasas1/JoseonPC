//! Published sensitivity and impedance for the embedded headphones.
//!
//! Port source: `Sources/JoseonHeadphones/HeadphoneSensitivityLibrary.swift`.
//! The figures and the texts are copied from that file by a script, not typed.
//!
//! **Every figure here came off the manufacturer's own page or data sheet on
//! 2026-09-21.** Nothing is remembered, inferred, or carried over from a similar
//! model. `docs/third-party-sensitivity.md` lists each URL and the spec line as
//! printed.
//!
//! A headphone whose maker publishes no usable figure is **not in this
//! library**. It is in `UNLISTED` with the reason, so the app can say why it
//! asks the user for a number. Seven of the seventeen embedded curves are in that
//! state.
//!
//! Keys are the same curve names `HeadphoneLibrary` uses.

use joseon_core::contracts::HeadphoneSensitivity;

/// A headphone Joseon has a curve for but no citable sensitivity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Unlisted {
    /// The curve name, as `HeadphoneLibrary` spells it.
    pub name: &'static str,
    /// What the manufacturer publishes, or does not, in one sentence for the UI.
    pub reason: &'static str,
    /// The page that was checked, so the user can look.
    pub checked_url: &'static str,
}

/// Curves with no citable sensitivity. The app must ask the user to measure or type one.
pub const UNLISTED: [Unlisted; 7] = [
    Unlisted {
        name: "HiFiMAN Susvara Unveiled",
        reason: "HiFiMAN publishes \"86dB\" with no reference — not per milliwatt, not per volt, no frequency. An unreferenced dB figure cannot be converted, so Joseon will not guess one.",
        checked_url: "https://www.hifiman.com/products/detail/347",
    },
    Unlisted {
        name: "HiFiMAN Susvara",
        reason: "HiFiMAN publishes \"83dB\" with no reference, the same as the Unveiled. Not convertible.",
        checked_url: "https://www.hifiman.com/products/detail/275",
    },
    Unlisted {
        name: "Meze Empyrean (leather earpads)",
        reason: "Meze has retired the first-generation Empyrean; its product page is gone. Empyrean II is a different driver, so its 90 dB SPL/mW does not carry over.",
        checked_url: "https://mezeaudio.com/products/meze-empyrean",
    },
    Unlisted {
        name: "ZMF Verite",
        reason: "ZMF has retired the Verite. The remaining page has no specification table — only prose about the driver.",
        checked_url: "https://www.zmfheadphones.com/verite/",
    },
    Unlisted {
        name: "Apple AirPods Max",
        reason: "Apple publishes no sensitivity or impedance for any AirPods model.",
        checked_url: "https://www.apple.com/airpods-max/specs/",
    },
    Unlisted {
        name: "Apple AirPods Pro 2",
        reason: "Apple publishes no sensitivity or impedance for any AirPods model.",
        checked_url: "https://support.apple.com/en-us/111851",
    },
    Unlisted {
        name: "Xenns Mangird Tea Pro",
        reason: "The published figure is \"104 ± 1dB\" with no reference — not per milliwatt, not per volt, no frequency. An unreferenced dB figure cannot be converted, so Joseon will not guess one.",
        checked_url: "https://www.linsoul.com/products/xenns-mangird-tea-pro",
    },
];

/// Sennheiser prints "Sound pressure level (SPL) 97 dB (1 V)": already dB SPL per volt, so nothing is converted.
pub fn hd600() -> HeadphoneSensitivity {
    HeadphoneSensitivity {
        db_spl_per_volt: 97.0,
        impedance_ohms: 300.0,
        source: "Sennheiser HD 600 product page, \"Sound pressure level (SPL) 97 dB (1 V)\", 300 Ω — us.sennheiser-hearing.com/products/hd-600, fetched 2026-09-21".to_string(),
    }
}

/// Sennheiser HD 650, dB SPL at 1 V as printed.
pub fn hd650() -> HeadphoneSensitivity {
    HeadphoneSensitivity {
        db_spl_per_volt: 103.0,
        impedance_ohms: 300.0,
        source: "Sennheiser HD 650 product page, \"Sound pressure level (SPL) 103 dB (1 V)\", 300 Ω — us.sennheiser-hearing.com/products/hd-650, fetched 2026-09-21".to_string(),
    }
}

/// Sennheiser HD 800 S, dB SPL at 1 V as printed.
pub fn hd800s() -> HeadphoneSensitivity {
    HeadphoneSensitivity {
        db_spl_per_volt: 102.0,
        impedance_ohms: 300.0,
        source: "Sennheiser HD 800 S product page, \"Sound pressure level (SPL) 102 dB (1 V)\", 300 Ω — us.sennheiser-hearing.com/products/hd-800-s, fetched 2026-09-21".to_string(),
    }
}

/// Focal's product data sheet prints "Sensitivity 104dB SPL / 1mW @ 1kHz" with 80 Ω. The web page prints a
/// "Maximum SPL" of 104 dB, a different quantity with the same number. The data sheet is the figure used.
pub fn focal_utopia() -> HeadphoneSensitivity {
    HeadphoneSensitivity::from_db_per_milliwatt(
        104.0,
        80.0,
        "Focal Utopia product data sheet (v1, 07/07/2022), \"Sensitivity 104dB SPL / 1mW @ 1kHz\", 80 Ohms — dam.focal-naim.com/m/60e16a1d4cab5a65/original/FP_Utopia_EN-pdf.pdf, fetched 2026-09-21",
    )
}

/// Audeze states the figure at the Drum Reference Point, the reference the SPL chain wants.
pub fn lcd_x() -> HeadphoneSensitivity {
    HeadphoneSensitivity::from_db_per_milliwatt(
        103.0,
        20.0,
        "Audeze LCD-X product page, \"Sensitivity 103 dB/1mW (at Drum Reference Point)\", 20 ohms — audeze.com/products/lcd-x, fetched 2026-09-21 (the page states no model year; the embedded curve is the 2021 revision)",
    )
}

/// Sony publishes the figure in the Help Guide, for the wired connection only. The "headset turned on" row
/// applies. Passive (off) is 100 dB/mW into 16 Ω.
pub fn sony_wh1000_xm5() -> HeadphoneSensitivity {
    HeadphoneSensitivity::from_db_per_milliwatt(
        102.0,
        48.0,
        "Sony WH-1000XM5 Help Guide specifications, \"Sensitivity: 102 dB/mW (when connecting via the headphone cable with the headset turned on)\", 48 Ω (1 kHz) — helpguide.sony.net/mdr/wh1000xm5/v1/en/contents/TP1000541014.html, fetched 2026-09-21",
    )
}

/// beyerdynamic states the figure at 500 Hz, not at 1 kHz as most makers do.
pub fn dt900_pro_x() -> HeadphoneSensitivity {
    HeadphoneSensitivity::from_db_per_milliwatt(
        100.0,
        48.0,
        "beyerdynamic DT 900 PRO X product page, \"Nominal sound pressure level 100 dB SPL (1mW/500Hz)\", 48 ohms — global.beyerdynamic.com/p/dt-900-pro-x, fetched 2026-10-08",
    )
}

/// 64 Audio prints one figure for the U12t and does not say which Apex module it was taken with. The three
/// module curves share this entry.
pub fn u12t() -> HeadphoneSensitivity {
    HeadphoneSensitivity::from_db_per_milliwatt(
        108.0,
        12.6,
        "64 Audio U12t product page, \"Sensitivity 108 dB/mW @ 1 kHz\", 12.6 Ω — 64audio.com/products/u12t, fetched 2026-10-08",
    )
}

/// Every verified entry, with the curve name `HeadphoneLibrary` uses.
pub fn by_curve_name() -> Vec<(&'static str, HeadphoneSensitivity)> {
    vec![
        ("Sennheiser HD 600", hd600()),
        ("Sennheiser HD 650", hd650()),
        ("Sennheiser HD 800 S", hd800s()),
        ("Focal Utopia", focal_utopia()),
        ("Audeze LCD-X (2021)", lcd_x()),
        ("Sony WH-1000XM5", sony_wh1000_xm5()),
        ("Beyerdynamic DT 900 Pro X", dt900_pro_x()),
        ("64 Audio U12t (m15 module)", u12t()),
        ("64 Audio U12t (m20 module)", u12t()),
        ("64 Audio U12t (mX module)", u12t()),
    ]
}

/// Published sensitivity for a curve name, or `None` when the maker publishes none.
///
/// `None` is a real answer, not a failure: it means "ask the user". Show
/// `reason_for_curve` next to that question.
pub fn sensitivity_for_curve(name: &str) -> Option<HeadphoneSensitivity> {
    by_curve_name()
        .into_iter()
        .find(|entry| entry.0 == name)
        .map(|entry| entry.1)
}

/// Why there is no figure for this curve name, for the UI to show beside the input field.
pub fn reason_for_curve(name: &str) -> Option<&'static str> {
    UNLISTED
        .iter()
        .find(|entry| entry.name == name)
        .map(|entry| entry.reason)
}

/// The `source` of a figure the user typed or measured.
pub const USER_SOURCE: &str = "User";

/// A sensitivity the user typed or measured. `source` is fixed to "User", so a
/// typed number can never be mistaken for a published one.
pub fn user_entered(db_spl_per_volt: f64, impedance_ohms: f64) -> HeadphoneSensitivity {
    HeadphoneSensitivity {
        db_spl_per_volt,
        impedance_ohms,
        source: USER_SOURCE.to_string(),
    }
}

/// The same, from a dB/mW figure off a box or a review the user trusts.
pub fn user_entered_per_milliwatt(db_per_milliwatt: f64, impedance_ohms: f64) -> HeadphoneSensitivity {
    HeadphoneSensitivity::from_db_per_milliwatt(db_per_milliwatt, impedance_ohms, USER_SOURCE)
}

/// True when the sensitivity did not come from a manufacturer. The UI must say so.
pub fn is_user_entered(sensitivity: &HeadphoneSensitivity) -> bool {
    sensitivity.source == USER_SOURCE
}
