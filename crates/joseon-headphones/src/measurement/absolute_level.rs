//! From the dBFS of a measurement to real dB SPL, and the refusal to do it when
//! nothing connects the two. Also the sensitivity derived from a measurement
//! and its uncertainty budget.
//!
//! Port source: `AbsoluteLevel.swift` and `SensitivityUncertainty.swift` in
//! `Sources/JoseonHeadphones/Measurement`.
//!
//! A deconvolved response is a *ratio*: digital out over digital in. It says
//! what shape the headphone has, and nothing about how loud it was. There are
//! exactly two honest ways to get pascals:
//!
//! - an acoustic calibrator (94 dB SPL at 1 kHz) on the microphone, read in dBFS;
//! - a microphone sensitivity stated in dBFS per pascal, valid only while the
//!   interface gain is known and fixed.
//!
//! With neither, absolute level is **not available**. That is a different
//! answer, not a degraded result with a plausible default.

use super::fft::fixed;
use super::mic_calibration::MicCalibration;
use crate::calibration::{MEASURED_TONE_UNCERTAINTY_DB, SPECS_UNCERTAINTY_DB, SYSTEM_VOLUME_UNCERTAINTY_DB};
use joseon_core::contracts::{CalibrationMethod, HeadphoneSensitivity, PlaybackCalibration};

/// What connects dBFS to dB SPL.
#[derive(Debug, Clone, PartialEq)]
pub enum Reference {
    /// An acoustic calibrator on the microphone. `reading_dbfs` is the RMS level
    /// of its tone in the recording (a full-scale sine reads −3.01 dBFS).
    Calibrator {
        reading_dbfs: f64,
        level_db_spl: f64,
        at_hz: f64,
        uncertainty_db: f64,
    },
    /// A stated microphone sensitivity, for example from a UMIK `Sens Factor`
    /// header. Valid only while the interface gain stays where it was.
    MicSensitivity { dbfs_per_pascal: f64, uncertainty_db: f64 },
    /// Nothing connects them. Carries the sentence the app shows.
    Unknown { reason: String },
}

impl Reference {
    /// A 94 dB SPL, 1 kHz calibrator with 0.5 dB of uncertainty.
    pub fn calibrator(reading_dbfs: f64) -> Self {
        Self::Calibrator {
            reading_dbfs,
            level_db_spl: 94.0,
            at_hz: 1_000.0,
            uncertainty_db: 0.5,
        }
    }

    /// A stated sensitivity with the uncertainty of the `Sens Factor` route.
    pub fn mic_sensitivity(dbfs_per_pascal: f64) -> Self {
        Self::MicSensitivity {
            dbfs_per_pascal,
            uncertainty_db: MicCalibration::SENS_FACTOR_UNCERTAINTY_DB,
        }
    }

    /// The usual case: a UMIK-style file whose `Sens Factor` header is there, or nothing.
    pub fn from_mic_calibration(calibration: Option<&MicCalibration>) -> Self {
        let Some(calibration) = calibration else {
            return Self::Unknown {
                reason: "No microphone calibration file, so nothing states what a sample is worth in pascals."
                    .to_string(),
            };
        };
        match calibration.mic_sensitivity_dbfs_per_pascal() {
            Some(sensitivity) => Self::mic_sensitivity(sensitivity),
            None => Self::Unknown {
                reason: format!(
                    "The calibration file \"{}\" has no sensitivity header, and the gain of the audio interface is unknown, so a level in dBFS can not be turned into dB SPL.",
                    calibration.name
                ),
            },
        }
    }
}

/// Which of the two honest routes an offset came from.
///
/// The two are not worth the same, and the uncertainty budget must say so: a
/// calibrator is a traceable 94 dB SPL on the microphone, and `Sens Factor` is a
/// convention about a digital reference that was never checked.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Route {
    AcousticCalibrator,
    MicrophoneSensFactor,
    #[default]
    Other,
}

impl Route {
    /// The smallest uncertainty this route may claim, in dB, one side.
    pub fn minimum_uncertainty_db(self) -> f64 {
        match self {
            Self::AcousticCalibrator => 0.5,
            Self::MicrophoneSensFactor => MicCalibration::SENS_FACTOR_UNCERTAINTY_DB,
            Self::Other => 0.0,
        }
    }

    /// Short label for the uncertainty table.
    pub fn label(self) -> &'static str {
        match self {
            Self::AcousticCalibrator => "Acoustic calibrator",
            Self::MicrophoneSensFactor => "Microphone \"Sens Factor\" convention",
            Self::Other => "Stated level reference",
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::AcousticCalibrator => "acousticCalibrator",
            Self::MicrophoneSensFactor => "microphoneSensFactor",
            Self::Other => "other",
        }
    }
}

/// dB SPL = dBFS + `offset_db`.
#[derive(Debug, Clone, PartialEq)]
pub struct Scale {
    pub offset_db: f64,
    pub uncertainty_db: f64,
    /// One sentence for the report: how this offset was found.
    pub method: String,
    /// Which route made it. `Other` for a scale made by hand.
    pub route: Route,
}

impl Scale {
    /// dB SPL for a level in dBFS RMS.
    pub fn spl_db(&self, dbfs: f64) -> f64 {
        dbfs + self.offset_db
    }
}

/// Available, or not, with the reason.
#[derive(Debug, Clone, PartialEq)]
pub enum AbsoluteLevel {
    Available(Scale),
    Unavailable { reason: String },
}

impl AbsoluteLevel {
    /// The scale, or `None`.
    pub fn scale(&self) -> Option<&Scale> {
        match self {
            Self::Available(scale) => Some(scale),
            Self::Unavailable { .. } => None,
        }
    }

    /// Why there is no scale, or `None` when there is one.
    pub fn unavailable_reason(&self) -> Option<&str> {
        match self {
            Self::Available(_) => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }

    pub fn is_available(&self) -> bool {
        self.scale().is_some()
    }

    /// Finds the scale from a reference.
    pub fn for_reference(reference: &Reference) -> Self {
        match reference {
            Reference::Calibrator {
                reading_dbfs,
                level_db_spl,
                at_hz,
                uncertainty_db,
            } => {
                if !(reading_dbfs.is_finite() && *reading_dbfs < 0.0) {
                    return Self::Unavailable {
                        reason: format!(
                            "The calibrator reading ({} dBFS) is not a usable level. Re-run the level check with the calibrator on the microphone.",
                            fixed(*reading_dbfs, 1)
                        ),
                    };
                }
                Self::Available(Scale {
                    offset_db: level_db_spl - reading_dbfs,
                    uncertainty_db: uncertainty_db.max(Route::AcousticCalibrator.minimum_uncertainty_db()),
                    method: format!(
                        "Acoustic calibrator, {} dB SPL at {} Hz, read as {} dBFS RMS",
                        fixed(*level_db_spl, 0),
                        fixed(*at_hz, 0),
                        fixed(*reading_dbfs, 2)
                    ),
                    route: Route::AcousticCalibrator,
                })
            }
            Reference::MicSensitivity {
                dbfs_per_pascal,
                uncertainty_db,
            } => {
                if !dbfs_per_pascal.is_finite() {
                    return Self::Unavailable {
                        reason: "The stated microphone sensitivity is not a number.".to_string(),
                    };
                }
                Self::Available(Scale {
                    offset_db: 94.0 - dbfs_per_pascal,
                    // The 2 dB of this route is a floor, not a default a caller
                    // can lower: its reference level was never checked.
                    uncertainty_db: uncertainty_db.max(Route::MicrophoneSensFactor.minimum_uncertainty_db()),
                    method: format!(
                        "Stated microphone sensitivity {} dBFS per pascal (94 dB SPL); valid only while the interface gain is untouched",
                        fixed(*dbfs_per_pascal, 2)
                    ),
                    route: Route::MicrophoneSensFactor,
                })
            }
            Reference::Unknown { reason } => Self::Unavailable { reason: reason.clone() },
        }
    }
}

// MARK: The rig

/// What the headphone was measured on.
///
/// The number of each case is how far the reading of this rig at 1 kHz can be
/// from what an IEC 60318-4 ear simulator reads on the same headphone.
///
/// **The three figures are engineering estimates, not standards values.**
/// Nothing here was checked against a real coupler.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MeasurementRig {
    /// A standards-type ear simulator, IEC 60318-4 (the old 711 coupler).
    EarSimulator,
    /// A flat plate: the headphone is on a baffle with the microphone flush in it.
    #[default]
    FlatPlate,
    /// Anything else: a home-made coupler, a silicone pinna, a cup over a measurement microphone.
    Other,
}

impl MeasurementRig {
    pub const ALL: [MeasurementRig; 3] = [Self::EarSimulator, Self::FlatPlate, Self::Other];

    /// One-sided uncertainty in dB this rig adds to a derived sensitivity. Engineering estimate.
    pub fn uncertainty_db(self) -> f64 {
        match self {
            Self::EarSimulator => 1.0,
            Self::FlatPlate => 2.5,
            Self::Other => 3.0,
        }
    }

    /// Short name for the UI and for the uncertainty table.
    pub fn label(self) -> &'static str {
        match self {
            Self::EarSimulator => "Ear simulator (IEC 60318-4 type)",
            Self::FlatPlate => "Flat plate",
            Self::Other => "Other rig",
        }
    }

    /// One sentence for the report: what the 1 kHz reading of this rig is worth.
    pub fn note(self) -> &'static str {
        match self {
            Self::EarSimulator => "Measured on a standards-type ear simulator, so the 1 kHz reading is close to an eardrum reading; Joseon still allows ± 1 dB for the coupler and the seating.",
            Self::FlatPlate => "Measured on a flat plate, which has no ear canal and no eardrum impedance. Its 1 kHz reading can sit ± 2.5 dB from what an IEC 60318-4 ear simulator would read.",
            Self::Other => "Measured on a rig Joseon knows nothing about, so its 1 kHz reading can sit ± 3 dB from an IEC 60318-4 ear simulator reading.",
        }
    }

    /// The name in saved settings. Same strings as the macOS app.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::EarSimulator => "earSimulator",
            Self::FlatPlate => "flatPlate",
            Self::Other => "other",
        }
    }
}

// MARK: Sensitivity derivation

/// Which piece of the chain a term of the budget belongs to. The UI names the
/// limit from this, not from `name`, which is prose.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ComponentKind {
    /// What connects dBFS to dB SPL.
    AbsoluteLevel,
    /// How well the volts at the headphone are known.
    DriveVoltage,
    /// How far the 1 kHz reading of this rig can be from an eardrum reading.
    Rig,
    /// How well the response of the microphone at 1 kHz is known.
    Microphone,
    /// Run-to-run repeatability of this measurement.
    Repeatability,
    #[default]
    Other,
}

impl ComponentKind {
    pub const ALL: [ComponentKind; 6] = [
        Self::AbsoluteLevel,
        Self::DriveVoltage,
        Self::Rig,
        Self::Microphone,
        Self::Repeatability,
        Self::Other,
    ];

    /// The word in the brackets after the total: `"rig-limited"`.
    pub fn limit_phrase(self) -> Option<&'static str> {
        match self {
            Self::AbsoluteLevel => Some("reference-limited"),
            Self::DriveVoltage => Some("voltage-limited"),
            Self::Rig => Some("rig-limited"),
            Self::Microphone => Some("microphone-limited"),
            Self::Repeatability => Some("repeatability-limited"),
            Self::Other => None,
        }
    }
}

/// One line of the uncertainty budget.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Component {
    /// What the UI prints in the first column, for example "Drive voltage".
    pub name: String,
    /// One-sided uncertainty in dB.
    pub db: f64,
    /// Where this number came from, for the second line or the tooltip.
    pub detail: String,
    pub kind: ComponentKind,
}

/// A headphone sensitivity found from a measurement, with its error bar and its workings.
///
/// The error bar is five terms in quadrature: the absolute level reference, the
/// drive voltage (the figure of the playback calibration), the rig, the
/// microphone response at 1 kHz, and the repeatability of the measurement.
#[derive(Debug, Clone, PartialEq)]
pub struct DerivedSensitivity {
    pub sensitivity: HeadphoneSensitivity,
    /// One-sided uncertainty in dB, the pieces added in quadrature. Not
    /// rounded: the UI prints `printed_uncertainty_db`.
    pub uncertainty_db: f64,
    /// Every piece that went into `uncertainty_db`, named.
    pub uncertainty_components: Vec<Component>,
    /// The SPL the headphone made at 1 kHz in the measurement, for the sanity line.
    pub measured_spl_at_1khz: f64,
    /// The RMS volts at the headphone in the measurement.
    pub drive_volts_rms: f64,
    /// What the headphone was measured on. Sets the rig term.
    pub rig: MeasurementRig,
}

impl DerivedSensitivity {
    /// The total, rounded **up** to the next half decibel. Up, never to nearest:
    /// 2.6 dB printed as 2.5 would be a claim the measurement can not support.
    pub fn printed_uncertainty_db(&self) -> f64 {
        Self::rounded_up_to_half_db(self.uncertainty_db)
    }

    /// The largest single term. A tie goes to the term listed first.
    pub fn dominant_term(&self) -> Component {
        let mut best = Component {
            name: "Measurement".to_string(),
            db: self.uncertainty_db,
            detail: String::new(),
            kind: ComponentKind::Other,
        };
        let mut best_db = f64::NEG_INFINITY;
        for component in &self.uncertainty_components {
            if component.db > best_db {
                best = component.clone();
                best_db = component.db;
            }
        }
        best
    }

    /// What the big type shows: `"± 3 dB (rig-limited)"`. The bracket names the
    /// one term that dominates, so the user can see what to fix.
    pub fn uncertainty_headline(&self) -> String {
        let value = Self::format(self.printed_uncertainty_db());
        match self.dominant_term().kind.limit_phrase() {
            Some(phrase) => format!("± {value} dB ({phrase})"),
            None => format!("± {value} dB"),
        }
    }

    /// Every term, in the order the UI prints them: term, value, detail.
    pub fn uncertainty_table(&self) -> Vec<(String, String, String)> {
        self.uncertainty_components
            .iter()
            .map(|c| (c.name.clone(), format!("{} dB", fixed(c.db, 1)), c.detail.clone()))
            .collect()
    }

    /// `2.35 → 2.5`, `3.0 → 3.0`. The epsilon stops a total that is a hair over
    /// a half step from a jump of a whole one.
    pub fn rounded_up_to_half_db(value: f64) -> f64 {
        if !(value.is_finite() && value > 0.0) {
            return 0.0;
        }
        (((value - 1e-9) / 0.5).ceil() * 0.5).max(0.5)
    }

    /// `3.0 → "3"`, `2.5 → "2.5"`.
    pub(crate) fn format(value: f64) -> String {
        if value == value.round() {
            fixed(value, 0)
        } else {
            fixed(value, 1)
        }
    }
}

/// Derived, or not, with the reason. No absolute level, no sensitivity.
#[derive(Debug, Clone, PartialEq)]
pub enum DerivedSensitivityResult {
    Derived(DerivedSensitivity),
    Unavailable { reason: String },
}

impl DerivedSensitivityResult {
    pub fn value(&self) -> Option<&DerivedSensitivity> {
        match self {
            Self::Derived(derived) => Some(derived),
            Self::Unavailable { .. } => None,
        }
    }

    pub fn unavailable_reason(&self) -> Option<&str> {
        match self {
            Self::Derived(_) => None,
            Self::Unavailable { reason } => Some(reason),
        }
    }
}

/// Uncertainty Joseon gives to the drive voltage: **the figure of the active
/// calibration**, never a constant of this module.
///
/// The user has one calibration, so it gets one uncertainty: the one the
/// calibration window shows. The method sets only a floor, so a calibration made
/// by hand with an optimistic figure can not lower the budget.
pub fn drive_voltage_uncertainty_db(calibration: &PlaybackCalibration) -> f64 {
    let floor = match calibration.method {
        CalibrationMethod::MeasuredVoltage => MEASURED_TONE_UNCERTAINTY_DB,
        CalibrationMethod::SystemVolume => SYSTEM_VOLUME_UNCERTAINTY_DB,
        CalibrationMethod::EnteredSpecs => SPECS_UNCERTAINTY_DB,
    };
    let stated = calibration.uncertainty_db;
    (if stated.is_finite() { stated } else { 0.0 }).max(floor)
}

/// The arguments of `derive_sensitivity`.
#[derive(Debug, Clone)]
pub struct Derivation<'a> {
    /// `20·log₁₀|H|` averaged over 800–1250 Hz, **before** normalization and
    /// after the microphone correction.
    pub magnitude_at_1khz_db: f64,
    pub absolute_level: &'a AbsoluteLevel,
    pub playback: &'a PlaybackCalibration,
    pub sweep_level_dbfs: f64,
    pub impedance_ohms: f64,
    /// `None` uses `drive_voltage_uncertainty_db(playback)`.
    pub drive_voltage_uncertainty_db: Option<f64>,
    /// Repeatability of the measurement itself. The macOS default is 0.3.
    pub measurement_uncertainty_db: f64,
    /// How well the response of the microphone is known at 1 kHz. The macOS default is 0.5.
    pub mic_calibration_uncertainty_db: f64,
    pub rig: MeasurementRig,
    pub source: String,
}

/// Derives dB SPL per volt at 1 kHz from a measurement.
///
/// ```text
/// SPL(1 kHz) = (sweep level dBFS − 3.01) + 20·log₁₀|H(1 kHz)| + offset
/// V_rms      = full_scale_vrms · 10^(sweep level dBFS / 20)
/// dB SPL/V   = SPL(1 kHz) − 20·log₁₀(V_rms)
/// ```
///
/// The sweep level cancels, because the transfer function is a ratio.
pub fn derive_sensitivity(d: Derivation) -> DerivedSensitivityResult {
    let AbsoluteLevel::Available(scale) = d.absolute_level else {
        return DerivedSensitivityResult::Unavailable {
            reason: d
                .absolute_level
                .unavailable_reason()
                .unwrap_or("No absolute level reference, so the measurement gives a shape but not a sensitivity.")
                .to_string(),
        };
    };
    if !(d.playback.full_scale_vrms > 0.0) {
        return DerivedSensitivityResult::Unavailable {
            reason: format!(
                "The playback calibration \"{}\" has no voltage, so the drive level at the headphone is unknown.",
                d.playback.name
            ),
        };
    }
    if !d.magnitude_at_1khz_db.is_finite() {
        return DerivedSensitivityResult::Unavailable {
            reason: "The measured response at 1 kHz is not a usable number.".to_string(),
        };
    }

    let drive_volts = d.playback.full_scale_vrms * 10.0f64.powf(d.sweep_level_dbfs / 20.0);
    let spl = (d.sweep_level_dbfs - 3.0103) + d.magnitude_at_1khz_db + scale.offset_db;
    let db_spl_per_volt = spl - 20.0 * drive_volts.log10();

    // The drive term is the figure of the calibration unless the app states a
    // better one. The floor of the route keeps the absolute-level term honest.
    let drive_db = d
        .drive_voltage_uncertainty_db
        .unwrap_or_else(|| drive_voltage_uncertainty_db(d.playback));
    let level_db = scale.uncertainty_db.max(scale.route.minimum_uncertainty_db());

    let component = |name: &str, db: f64, detail: String, kind| Component {
        name: name.to_string(),
        db,
        detail,
        kind,
    };
    let components = vec![
        component(
            "Absolute level reference",
            level_db,
            scale.route.label().to_string(),
            ComponentKind::AbsoluteLevel,
        ),
        component(
            "Drive voltage",
            drive_db,
            format!("Playback calibration \"{}\"", d.playback.name),
            ComponentKind::DriveVoltage,
        ),
        component(
            "Rig",
            d.rig.uncertainty_db(),
            format!(
                "{}, against an IEC 60318-4 eardrum reading (engineering estimate)",
                d.rig.label()
            ),
            ComponentKind::Rig,
        ),
        component(
            "Microphone response at 1 kHz",
            d.mic_calibration_uncertainty_db,
            if d.mic_calibration_uncertainty_db <= 0.5 {
                "Calibration file applied"
            } else {
                "No microphone calibration file"
            }
            .to_string(),
            ComponentKind::Microphone,
        ),
        component(
            "Measurement repeatability",
            d.measurement_uncertainty_db,
            "Run-to-run spread of this measurement".to_string(),
            ComponentKind::Repeatability,
        ),
    ];
    let quadrature = components.iter().fold(0.0, |sum, c| sum + c.db * c.db).sqrt();

    DerivedSensitivityResult::Derived(DerivedSensitivity {
        sensitivity: HeadphoneSensitivity {
            db_spl_per_volt,
            impedance_ohms: d.impedance_ohms,
            source: d.source,
        },
        uncertainty_db: quadrature,
        uncertainty_components: components,
        measured_spl_at_1khz: spl,
        drive_volts_rms: drive_volts,
        rig: d.rig,
    })
}
