//! JoseonPC contracts (L0).
//!
//! Every crate talks through the types in this file. Change it only in an
//! integration round.
//!
//! Data flow:
//!   `AudioSource` (joseon-capture) -> ring buffer -> analysis engine (joseon-core)
//!   -> `AnalysisFrame` -> renderers (joseon-render) + app shell (joseon-app).
//!   The headphone model (joseon-headphones) adds the predicted at-ear curve to a frame.
//!
//! Port source: `Sources/JoseonCore/Contracts.swift`. Field names are the Swift
//! names in snake case, with the same units and the same defaults. Where Rust
//! needs a different shape, the item says so under "Port note".

use crate::ring_buffer::RingReader;
use std::ops::RangeInclusive;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

// MARK: Sources

/// Facts about the stream that the capture layer sees.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamInfo {
    pub sample_rate: f64,
    pub channel_count: usize,
    /// Output device name, for example "Speakers (USB DAC)".
    pub device_name: String,
    /// Device bit depth when the system reports it.
    pub bit_depth: Option<u32>,
    /// Names of the processes that make sound now, for example `["Qobuz"]`.
    pub active_sources: Vec<String>,
    /// True when `device_name` is the default output device. False when the
    /// capture follows the device a player chose itself. `None` when unknown
    /// (demo, synthetic).
    pub device_is_default: Option<bool>,
}

impl StreamInfo {
    pub fn new(sample_rate: f64, channel_count: usize, device_name: impl Into<String>) -> Self {
        Self {
            sample_rate,
            channel_count,
            device_name: device_name.into(),
            bit_depth: None,
            active_sources: Vec::new(),
            device_is_default: None,
        }
    }
}

/// What the source app plays now. Text only, never audio.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NowPlaying {
    pub title: String,
    pub artist: String,
    /// Empty when the player shows none.
    pub album: String,
    /// The app it was read from, for example "Qobuz".
    pub source: String,
    /// The player marks the stream as hi-res.
    pub is_hi_res: bool,
}

impl NowPlaying {
    /// "Artist – Title", or only the one that is known.
    pub fn line(&self) -> String {
        [self.artist.as_str(), self.title.as_str()]
            .into_iter()
            .filter(|part| !part.is_empty())
            .collect::<Vec<_>>()
            .join(" \u{2013} ")
    }
}

/// Called when the now-playing text changes, including to `None`.
pub type NowPlayingCallback = Box<dyn Fn(Option<NowPlaying>) + Send>;

/// A reader of `NowPlaying`. joseon-capture implements it. Tests use a fake.
///
/// Port note: the Swift callback runs on the main queue. Rust has no main
/// queue, so the callback runs on a thread the source owns. The receiver moves
/// the value to its own thread.
pub trait NowPlayingSource {
    /// `None` when nothing is known (no player, no track).
    fn current(&self) -> Option<NowPlaying>;
    fn set_on_change(&mut self, callback: Option<NowPlayingCallback>);
    /// Starts the reader. Cheap when the player is absent.
    fn start(&mut self);
    fn stop(&mut self);
}

/// Called when the stream facts change (device switch, rate change).
pub type StreamInfoCallback = Box<dyn Fn(StreamInfo) + Send>;

/// A source of stereo float audio. joseon-capture implements this with WASAPI
/// loopback. Tests and the probe implement it with synthetic signals.
///
/// Port notes:
/// - Swift shares one ring buffer object. Here the source keeps the writing end
///   and gives the reading end away once, through `take_reader`.
/// - Swift has `startAsync`. Here the caller runs `start` on its own thread.
/// - The callback runs on the thread that calls `start` or on a thread the
///   source owns, not on a main queue.
pub trait AudioSource {
    /// `None` until the source runs.
    fn stream_info(&self) -> Option<StreamInfo>;
    /// The reading end of the buffer the source writes every captured block to.
    /// `Some` on the first call, `None` after that.
    fn take_reader(&mut self) -> Option<RingReader>;
    fn set_on_stream_info_change(&mut self, callback: Option<StreamInfoCallback>);
    /// Starts capture. Can block. Do not call it on the UI thread.
    fn start(&mut self) -> Result<(), SourceError>;
    /// Stops capture. Can block. A stop is final until the next `start`.
    fn stop(&mut self);
    fn is_running(&self) -> bool;
}

/// Why a source did not start.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SourceError {
    /// No output device is available.
    NoDevice,
    /// The platform refused the capture. The string is the platform message.
    Platform(String),
}

impl std::fmt::Display for SourceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SourceError::NoDevice => write!(f, "no output device"),
            SourceError::Platform(message) => write!(f, "capture failed: {message}"),
        }
    }
}

impl std::error::Error for SourceError {}

// MARK: Analysis output

/// Log-spaced display spectrum. All vectors have the same length as `frequencies`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpectrumReading {
    /// Center frequency of each display bin in Hz, log spaced, low to high.
    pub frequencies: Vec<f32>,
    /// Level in dBFS per display bin. Floor is `SpectrumReading::FLOOR_DB`.
    pub left: Vec<f32>,
    pub right: Vec<f32>,
    /// Mid = (L+R)/2.
    pub mid: Vec<f32>,
    /// Side = (L-R)/2.
    pub side: Vec<f32>,
    /// Slow-decay peak hold of `mid`.
    pub peak_hold: Vec<f32>,
    /// Long-term average of `mid` since the last reset (track tonal balance).
    pub average: Vec<f32>,
    /// Optional: the mid curve per FFT resolution, before the analyzer blends
    /// them. The live spectrum uses the blended `mid` (lowest latency). The
    /// spectrogram uses the layers, so it can delay each one to a common time
    /// and blend after that. Empty = not provided.
    pub mid_layers: Vec<SpectrumLayer>,
}

impl SpectrumReading {
    pub const FLOOR_DB: f32 = -120.0;

    /// A reading at the floor on `bin_count` bins (at least 2) from 10 Hz to 24 kHz.
    pub fn silent(bin_count: usize) -> Self {
        Self::silent_in_range(bin_count, 10.0, 24_000.0)
    }

    pub fn silent_in_range(bin_count: usize, min_hz: f32, max_hz: f32) -> Self {
        let n = bin_count.max(2);
        let ratio = max_hz / min_hz;
        let frequencies = (0..n).map(|i| min_hz * ratio.powf(i as f32 / (n - 1) as f32)).collect();
        let floor = vec![Self::FLOOR_DB; n];
        Self {
            frequencies,
            left: floor.clone(),
            right: floor.clone(),
            mid: floor.clone(),
            side: floor.clone(),
            peak_hold: floor.clone(),
            average: floor,
            mid_layers: Vec::new(),
        }
    }
}

/// One FFT resolution of the mid channel on the display bins. See `SpectrumReading::mid_layers`.
#[derive(Debug, Clone, PartialEq)]
pub struct SpectrumLayer {
    /// Level in dBFS per display bin, same calibration and smoothing as `SpectrumReading::mid`.
    pub levels_db: Vec<f32>,
    /// Blend weight per display bin, 0...1. Over all layers the weights sum to 1
    /// in every bin. The analyzer blends in the power domain with these weights.
    pub weights: Vec<f32>,
    /// Time from an event in the audio to the center of this layer's analysis
    /// window, in seconds (half the window length). The longest layer has the
    /// largest value.
    pub latency_seconds: f32,
}

/// The strongest spectral peak, with musical note.
#[derive(Debug, Clone, PartialEq)]
pub struct PeakReading {
    pub frequency_hz: f32,
    pub level_db: f32,
    /// For example "G#4". Empty when there is no clear peak.
    pub note_name: String,
    /// Offset from the equal-tempered note, -50...+50.
    pub cents: f32,
}

impl Default for PeakReading {
    fn default() -> Self {
        Self {
            frequency_hz: 0.0,
            level_db: SpectrumReading::FLOOR_DB,
            note_name: String::new(),
            cents: 0.0,
        }
    }
}

/// Energy per listening band in dBFS (RMS of the band).
///
/// Measured on the mid channel, (L+R)/2, so the numbers agree with
/// `SpectrumReading::mid`: a full-scale mono sine inside a band reads 0 dB.
/// Content that is only in the side channel does not show here.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BandEnergy {
    /// 20–60 Hz
    pub sub_bass: f32,
    /// 60–250 Hz
    pub bass: f32,
    /// 250–500 Hz
    pub low_mid: f32,
    /// 500–2k Hz
    pub mid: f32,
    /// 2k–4k Hz
    pub upper_mid: f32,
    /// 4k–6k Hz
    pub presence: f32,
    /// 6k–12k Hz
    pub brilliance: f32,
    /// 12k–20k+ Hz
    pub air: f32,
}

impl BandEnergy {
    pub const NAMES: [&'static str; 8] = [
        "Sub",
        "Bass",
        "Low mid",
        "Mid",
        "Upper mid",
        "Presence",
        "Brilliance",
        "Air",
    ];
    pub const EDGES_HZ: [f32; 9] = [20.0, 60.0, 250.0, 500.0, 2_000.0, 4_000.0, 6_000.0, 12_000.0, 24_000.0];

    pub fn values(&self) -> [f32; 8] {
        [
            self.sub_bass,
            self.bass,
            self.low_mid,
            self.mid,
            self.upper_mid,
            self.presence,
            self.brilliance,
            self.air,
        ]
    }
}

impl Default for BandEnergy {
    fn default() -> Self {
        let floor = -120.0;
        Self {
            sub_bass: floor,
            bass: floor,
            low_mid: floor,
            mid: floor,
            upper_mid: floor,
            presence: floor,
            brilliance: floor,
            air: floor,
        }
    }
}

/// ITU-R BS.1770-4 / EBU R128 loudness plus peak and dynamics numbers.
#[derive(Debug, Clone, PartialEq)]
pub struct LoudnessReading {
    /// 400 ms
    pub momentary_lufs: f32,
    /// 3 s
    pub short_term_lufs: f32,
    /// Gated, since reset.
    pub integrated_lufs: f32,
    pub momentary_max_lufs: f32,
    pub short_term_max_lufs: f32,
    /// EBU Tech 3342
    pub loudness_range_lu: f32,
    /// Inter-sample true peak per channel: 4x oversampled up to 96 kHz, 2x from
    /// 176.4 kHz, 1x from 352.8 kHz. This is a display value with meter
    /// ballistics: instant attack, about 1.5 s hold, then a fall of about 20 dB
    /// per second. It is not the peak of the last block. Use
    /// `true_peak_max_dbtp` for the measurement.
    pub true_peak_left_dbtp: f32,
    pub true_peak_right_dbtp: f32,
    /// Highest true peak of both channels since reset. No ballistics.
    pub true_peak_max_dbtp: f32,
    /// 300 ms window
    pub rms_left_db: f32,
    pub rms_right_db: f32,
    /// Peak-to-loudness ratio: true peak max - integrated. A per-track dynamics
    /// number. 0 until `is_integrated_valid`.
    pub plr_db: f32,
    /// Peak-to-short-term loudness ratio, live dynamics.
    pub psr_db: f32,
    /// Count of clip events since reset, both channels together. One event = a
    /// run of 3 or more consecutive samples in one channel with |x| >= 0.9999.
    /// A longer run still counts as one event. Sample values, not true peak.
    pub clip_count: u64,
    /// Seconds of audio measured since reset.
    pub measured_seconds: f64,
    /// True when at least one 400 ms block has passed the BS.1770 gates since
    /// reset, so `integrated_lufs` is a measurement. While false,
    /// `integrated_lufs` is `SILENCE_LUFS`, and `loudness_range_lu` and
    /// `plr_db` are 0: consumers show "—" for I, LRA and PLR.
    pub is_integrated_valid: bool,
}

impl LoudnessReading {
    pub const SILENCE_LUFS: f32 = -120.0;
}

impl Default for LoudnessReading {
    fn default() -> Self {
        Self {
            momentary_lufs: Self::SILENCE_LUFS,
            short_term_lufs: Self::SILENCE_LUFS,
            integrated_lufs: Self::SILENCE_LUFS,
            momentary_max_lufs: Self::SILENCE_LUFS,
            short_term_max_lufs: Self::SILENCE_LUFS,
            loudness_range_lu: 0.0,
            true_peak_left_dbtp: -120.0,
            true_peak_right_dbtp: -120.0,
            true_peak_max_dbtp: -120.0,
            rms_left_db: -120.0,
            rms_right_db: -120.0,
            plr_db: 0.0,
            psr_db: 0.0,
            clip_count: 0,
            measured_seconds: 0.0,
            is_integrated_valid: false,
        }
    }
}

/// Stereo field numbers and the vectorscope point cloud.
#[derive(Debug, Clone, PartialEq)]
pub struct StereoReading {
    /// Phase correlation, -1 (out of phase) ... +1 (mono).
    pub correlation: f32,
    /// Balance, -1 (all left) ... +1 (all right), from RMS.
    pub balance: f32,
    /// Side RMS / mid RMS: 0 mono, 1 hard-panned, max 4.
    pub width: f32,
    /// Correlation per band, same band order as `BandEnergy`. 0 where `band_active` is false.
    pub band_correlation: [f32; 8],
    /// Balance per band, same band order as `BandEnergy`. 0 where `band_active` is false.
    pub band_balance: [f32; 8],
    /// Per band: false = gated empty band (under -100 dBFS, or more than 80 dB
    /// under the broadband level). Its correlation and balance are neutral
    /// zeros, not measurements: consumers grey the band out.
    pub band_active: [bool; 8],
    /// Vectorscope points, newest last, each `[x, y]` in -1...1:
    /// x = (R - L) / 2 (positive = right), y = (L + R) / 2 (mid). So
    /// |x| + |y| = max(|L|, |R|): full scale is the diamond with tips at ±1.
    /// The points are evenly decimated samples of the last 50 ms, at most
    /// `StereoAnalyzing::scope_point_count` of them. At high sample rates they
    /// are NOT consecutive samples. A renderer may join neighbours that are
    /// close, but a line between two far points is a chord across the figure,
    /// not the path of the signal: draw far points as dots.
    pub scope_points: Vec<[f32; 2]>,
}

impl Default for StereoReading {
    fn default() -> Self {
        Self {
            correlation: 0.0,
            balance: 0.0,
            width: 0.0,
            band_correlation: [0.0; 8],
            band_balance: [0.0; 8],
            band_active: [false; 8],
            scope_points: Vec::new(),
        }
    }
}

/// Predicted at-ear result from a headphone model.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadphoneReading {
    pub model_name: String,
    /// Headphone magnitude response in dB on the frame's display bins, normalized to 0 dB at 1 kHz.
    pub response_db: Vec<f32>,
    /// Target curve on the same bins, normalized to 0 dB at 1 kHz. All zeros
    /// when `has_target` is false.
    pub target_db: Vec<f32>,
    /// False when the model has no target curve: `target_db` is then a
    /// placeholder, and consumers hide the target line and its legend entry.
    pub has_target: bool,
    /// Source mid spectrum + `response_db`: what the headphone is predicted to deliver at the ear.
    pub predicted_at_ear_db: Vec<f32>,
    /// Flags that the user can read, for example "Sub-bass load high: -9 dBFS below 40 Hz".
    pub stress_flags: Vec<StressFlag>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(i32)]
pub enum StressSeverity {
    Info = 0,
    Watch = 1,
    High = 2,
}

#[derive(Debug, Clone, PartialEq)]
pub struct StressFlag {
    pub id: String,
    pub severity: StressSeverity,
    pub title: String,
    pub detail: String,
    /// The frequency span the flag is about, when it has one (for example
    /// 20..=40 for sub-bass under-delivery). Panels shade this span on the
    /// spectrum. `None` for whole-signal flags (overs, dense master).
    pub frequency_range_hz: Option<RangeInclusive<f32>>,
    /// Short number for the plot label, for example "−7 dB vs target". Empty when there is none.
    pub plot_label: String,
}

/// One analysis result. The engine makes about 60 of these per second.
#[derive(Debug, Clone, PartialEq)]
pub struct AnalysisFrame {
    /// Seconds on a monotonic clock. Port note: Swift uses the system uptime.
    pub host_time: f64,
    pub stream: Option<StreamInfo>,
    pub spectrum: SpectrumReading,
    pub peak: PeakReading,
    pub bands: BandEnergy,
    pub loudness: LoudnessReading,
    pub stereo: StereoReading,
    pub headphone: Option<HeadphoneReading>,
    /// True when the input was digital silence for the whole frame.
    pub is_silent: bool,
    /// The strongest tonal peaks of mid, strongest first, at most 5. `peak` is
    /// the first of them when the analyzer provides the list. Empty when it does not.
    pub top_peaks: Vec<PeakReading>,
    /// Lowest frequency with strong sustained content (within 30 dB of the
    /// long-term average maximum), in Hz. 0 = unknown.
    pub lowest_strong_hz: f32,
    /// Third-octave band levels per channel. `None` when the spectrum analyzer does not provide them.
    pub third_octave: Option<ThirdOctaveReading>,
    /// Estimated sound level at the ear. `None` when there is no SPL estimator
    /// (no calibration or no headphone sensitivity).
    pub spl: Option<SplReading>,
}

impl AnalysisFrame {
    /// A silent frame around `spectrum`, every other field at its default.
    pub fn new(spectrum: SpectrumReading) -> Self {
        Self {
            host_time: 0.0,
            stream: None,
            spectrum,
            peak: PeakReading::default(),
            bands: BandEnergy::default(),
            loudness: LoudnessReading::default(),
            stereo: StereoReading::default(),
            headphone: None,
            is_silent: true,
            top_peaks: Vec::new(),
            lowest_strong_hz: 0.0,
            third_octave: None,
            spl: None,
        }
    }
}

// MARK: Analyzer contracts
//
// Each analyzer takes deinterleaved stereo float blocks at `sample_rate`. The
// two slices have the same length. One analysis thread owns the analyzer and
// calls every method.
//
// Port note: the Swift protocols are class protocols that other threads can
// hold. Here the engine owns each analyzer, so the methods take `&mut self`
// and need no lock.

#[derive(Debug, Clone, PartialEq)]
pub struct SpectrumSettings {
    /// Number of log-spaced display bins.
    pub display_bins: usize,
    pub min_hz: f32,
    pub max_hz: f32,
    /// Release time of the display smoothing in seconds (attack is instant).
    pub release_seconds: f32,
    /// Peak-hold decay in dB per second.
    pub peak_decay_db_per_second: f32,
    /// Spectral slope for display in dB per octave (4.5 makes pink-ish music look flat). 0 = raw.
    pub tilt_db_per_octave: f32,
}

impl Default for SpectrumSettings {
    fn default() -> Self {
        Self {
            display_bins: 1024,
            min_hz: 10.0,
            max_hz: 24_000.0,
            release_seconds: 0.25,
            peak_decay_db_per_second: 12.0,
            tilt_db_per_octave: 0.0,
        }
    }
}

/// What `SpectrumAnalyzing::read` returns.
#[derive(Debug, Clone, PartialEq)]
pub struct SpectrumOutput {
    pub spectrum: SpectrumReading,
    pub peak: PeakReading,
    pub bands: BandEnergy,
}

/// Port note: Swift has two optional protocols next to this one,
/// `TopPeaksProviding` and `ThirdOctaveProviding`, and the engine checks for
/// them at run time. Rust cannot ask a trait object for another trait, so they
/// are the last three methods here, with defaults that mean "not provided".
pub trait SpectrumAnalyzing: Send {
    fn settings(&self) -> &SpectrumSettings;
    fn set_settings(&mut self, settings: SpectrumSettings);
    /// Feed new samples. The analyzer keeps its own history for long FFT windows.
    fn process(&mut self, left: &[f32], right: &[f32], sample_rate: f64);
    /// Current display spectrum, peak and bands.
    fn read(&mut self) -> SpectrumOutput;
    fn reset(&mut self);

    /// Strongest tonal peaks of mid, strongest first, at most 5. Same gate as
    /// `PeakReading` (above −80 dBFS, 10 dB over the local median).
    fn top_peaks(&mut self) -> Vec<PeakReading> {
        Vec::new()
    }
    /// Lowest frequency with strong sustained content, in Hz. 0 = unknown.
    fn lowest_strong_hz(&mut self) -> f32 {
        0.0
    }
    fn third_octave(&mut self) -> Option<ThirdOctaveReading> {
        None
    }
}

pub trait LoudnessMetering: Send {
    fn process(&mut self, left: &[f32], right: &[f32], sample_rate: f64);
    fn read(&mut self) -> LoudnessReading;
    fn reset(&mut self);
}

pub trait StereoAnalyzing: Send {
    /// Max points in `StereoReading::scope_points`.
    fn scope_point_count(&self) -> usize;
    fn set_scope_point_count(&mut self, count: usize);
    fn process(&mut self, left: &[f32], right: &[f32], sample_rate: f64);
    fn read(&mut self) -> StereoReading;
    fn reset(&mut self);
}

/// joseon-headphones implements this. The engine calls it once per frame.
pub trait HeadphoneModeling: Send {
    fn model_name(&self) -> &str;
    fn evaluate(
        &mut self,
        spectrum: &SpectrumReading,
        bands: &BandEnergy,
        loudness: &LoudnessReading,
    ) -> HeadphoneReading;
    /// New measurement: drop state that spans frames (flag hysteresis). The
    /// engine calls it when the measurement resets. Default: nothing.
    fn reset(&mut self) {}
}

/// A headphone model that the app sets from one thread and the engine uses on another.
pub type SharedHeadphoneModel = Arc<Mutex<dyn HeadphoneModeling>>;

// MARK: Sound level at the ear (SPL estimate)
//
// Chain: ThirdOctaveReading (signal, dBFS RMS per band and channel, from the spectrum analyzer)
//   + PlaybackCalibration (volts at the headphone for a full-scale sine)
//   + HeadphoneSensitivity (dB SPL per volt at 1 kHz) + the measured headphone response
//   -> SPL at the eardrum per band -> diffuse-field equivalent -> A-weighted level, Leq, dose.
// The app can not know the amplifier gain, so every SPL number is an estimate
// that depends on the calibration.

/// RMS level per third-octave band (IEC 61260 nominal centers, 20 Hz ... 20 kHz,
/// 31 bands), in dBFS RMS where a full-scale SINE reads -3.01 dBFS. Integration
/// time about 125 ms ("fast"). Floor -140.
#[derive(Debug, Clone, PartialEq)]
pub struct ThirdOctaveReading {
    pub centers_hz: Vec<f32>,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl ThirdOctaveReading {
    pub const NOMINAL_CENTERS_HZ: [f32; 31] = [
        20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0, 500.0, 630.0, 800.0,
        1_000.0, 1_250.0, 1_600.0, 2_000.0, 2_500.0, 3_150.0, 4_000.0, 5_000.0, 6_300.0, 8_000.0, 10_000.0, 12_500.0,
        16_000.0, 20_000.0,
    ];
    pub const FLOOR_DB: f32 = -140.0;

    /// A reading on the nominal centers.
    pub fn new(left: Vec<f32>, right: Vec<f32>) -> Self {
        Self {
            centers_hz: Self::NOMINAL_CENTERS_HZ.to_vec(),
            left,
            right,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CalibrationMethod {
    /// The user measured a test tone with a meter.
    MeasuredVoltage,
    /// From DAC / amplifier data.
    EnteredSpecs,
    /// The system controls the volume of a known output.
    SystemVolume,
}

impl CalibrationMethod {
    /// The name in saved settings and in the probe JSON. Same strings as the macOS app.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MeasuredVoltage => "measuredVoltage",
            Self::EnteredSpecs => "enteredSpecs",
            Self::SystemVolume => "systemVolume",
        }
    }
}

/// How loud the chain plays: the voltage at the headphone terminals for a full-scale (0 dBFS peak) sine.
#[derive(Debug, Clone, PartialEq)]
pub struct PlaybackCalibration {
    /// For example "WA33 at 10 o'clock".
    pub name: String,
    pub method: CalibrationMethod,
    /// RMS volts at the headphone for a 0 dBFS sine. With `SystemVolume` this
    /// is the value at the current volume setting.
    pub full_scale_vrms: f64,
    /// Estimated uncertainty of the final SPL in dB (one side), shown to the
    /// user. About 2 for a measured voltage, 4 or more for specs.
    pub uncertainty_db: f64,
}

/// Headphone sensitivity at 1 kHz.
#[derive(Debug, Clone, PartialEq)]
pub struct HeadphoneSensitivity {
    /// dB SPL at the eardrum simulator for 1 V RMS at 1 kHz.
    pub db_spl_per_volt: f64,
    pub impedance_ohms: f64,
    /// Where the numbers come from, for example "HiFiMAN product page, 86 dB/mW, 45 Ω". "User" when typed in.
    pub source: String,
}

impl HeadphoneSensitivity {
    /// dB/mW -> dB/V: add 10·log10(1000 / Z).
    pub fn from_db_per_milliwatt(db_per_mw: f64, impedance_ohms: f64, source: impl Into<String>) -> Self {
        Self {
            db_spl_per_volt: db_per_mw + 10.0 * (1000.0 / impedance_ohms).log10(),
            impedance_ohms,
            source: source.into(),
        }
    }
}

/// Estimated level at the ear. All levels are dB SPL re 20 µPa. "DF" =
/// diffuse-field equivalent (ISO 11904 idea: eardrum level minus the
/// diffuse-field ear response), the level that noise-dose limits refer to.
#[derive(Debug, Clone, PartialEq)]
pub struct SplReading {
    /// For example "WA33 at 10 o'clock".
    pub calibration_name: String,
    pub uncertainty_db: f32,
    /// A-weighted, DF equivalent, fast (125 ms), louder ear.
    pub level_a_fast: f32,
    /// A-weighted, DF equivalent, slow (1 s), louder ear.
    pub level_a_slow: f32,
    /// Unweighted (Z) level at the eardrum, slow, louder ear.
    pub level_z_eardrum: f32,
    /// A-weighted equivalent continuous level since the measurement reset (track).
    pub leq_a_track: f32,
    /// The same since the dose reset (session / day).
    pub leq_a_session: f32,
    /// Highest `level_a_fast` since the measurement reset.
    pub max_a_fast: f32,
    /// Level per third-octave band at the eardrum (unweighted, louder ear), same centers as `ThirdOctaveReading`.
    pub band_levels_eardrum: Vec<f32>,
    /// Noise dose since the dose reset, 1.0 = 100%. NIOSH: 85 dBA for 8 h, 3 dB exchange rate.
    pub dose_niosh: f32,
    /// WHO / ITU H.870 weekly allowance for adults: 80 dBA for 40 h, 3 dB
    /// exchange rate. 1.0 = the whole week used.
    pub dose_who_weekly: f32,
    /// Seconds of listening counted in the dose (time with signal).
    pub dose_seconds: f64,
    /// At the current `level_a_slow`, the time until `dose_niosh` reaches 1.0,
    /// in seconds. Infinite when the level is low.
    pub seconds_to_niosh_limit: f64,
}

impl SplReading {
    pub const FLOOR_DB: f32 = 0.0;
}

/// joseon-headphones implements this. The engine calls it once per frame.
pub trait SplEstimating: Send {
    /// `dt` = seconds of audio since the previous call (0 when no new audio arrived).
    /// `is_silent` = digital silence.
    fn evaluate(&mut self, third_octave: &ThirdOctaveReading, dt: f64, is_silent: bool) -> SplReading;
    /// New track: clears `leq_a_track` and `max_a_fast`. The dose keeps running.
    fn reset_measurement(&mut self);
    /// Clears the dose and the session Leq.
    fn reset_dose(&mut self);
}

/// An SPL estimator that the app sets from one thread and the engine uses on another.
pub type SharedSplEstimator = Arc<Mutex<dyn SplEstimating>>;

// MARK: Session timeline
//
// A rolling record of the last minutes of listening: one sample per second plus
// events. Memory only. Nothing here is ever written to disk. No audio is kept,
// only numbers.

/// One second of listening, summarized.
#[derive(Debug, Clone, PartialEq)]
pub struct SessionSample {
    /// Seconds of audio time since the recorder started (monotonic; pauses while there is no audio).
    pub time: f64,
    /// Wall-clock time of the sample, for labels.
    pub date: SystemTime,
    /// Maximum momentary loudness in this second (LUFS). Floor = `LoudnessReading::SILENCE_LUFS`.
    pub momentary_max_lufs: f32,
    /// Short-term loudness in this second (LUFS).
    pub short_term_lufs: f32,
    /// Highest true peak in this second (dBTP, louder channel).
    pub true_peak_dbtp: f32,
    /// Mean phase correlation in this second, −1…+1.
    pub correlation: f32,
    /// Mean band energy in this second, the 8 bands of `BandEnergy` (dBFS).
    pub bands: [f32; 8],
    /// Mean A-weighted level at the ear in this second (dB SPL, DF equivalent). `None` when not calibrated.
    pub level_a: Option<f32>,
    /// True when the whole second was digital silence.
    pub is_silent: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SessionEventKind {
    /// The measurement was reset (auto after silence, or manual).
    TrackStart,
    /// New clipped-sample runs in this second; `value` = count.
    Clip,
    /// True peak went over 0 dBTP; `value` = dBTP.
    InterSampleOver,
    /// `label` = flag title, `detail` = flag id.
    StressFlagRaised,
    StressFlagCleared,
    SilenceStart,
    SilenceEnd,
}

impl SessionEventKind {
    /// The name in the probe JSON. Same strings as the macOS app.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::TrackStart => "trackStart",
            Self::Clip => "clip",
            Self::InterSampleOver => "interSampleOver",
            Self::StressFlagRaised => "stressFlagRaised",
            Self::StressFlagCleared => "stressFlagCleared",
            Self::SilenceStart => "silenceStart",
            Self::SilenceEnd => "silenceEnd",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionEvent {
    pub id: u64,
    pub kind: SessionEventKind,
    /// Same clock as `SessionSample::time`.
    pub time: f64,
    pub date: SystemTime,
    /// Port note: shared text, so a snapshot of 2000 events copies no strings.
    pub label: Arc<str>,
    pub detail: Arc<str>,
    pub value: f32,
}

/// A copy of the record, cheap to take about once per second from the UI thread.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct SessionSnapshot {
    /// Oldest first. At most `capacity_seconds` entries.
    pub samples: Vec<SessionSample>,
    /// Oldest first. Events older than the oldest sample are dropped.
    pub events: Vec<SessionEvent>,
    /// Changes whenever samples or events change; panels compare it to skip redraws.
    pub revision: u64,
}

/// joseon-core implements this (the session recorder). The engine feeds it;
/// panels and the app read snapshots.
///
/// Every method takes `&self`: the analysis thread writes while other threads
/// take snapshots, so the implementation holds its own lock.
pub trait SessionRecording: Send + Sync {
    /// Length of the record in seconds (default 1800).
    fn capacity_seconds(&self) -> usize;
    /// Analysis thread: called once per published frame. `dt` = seconds of audio since the previous call.
    fn ingest(&self, frame: &AnalysisFrame, dt: f64);
    /// Analysis thread: the measurement was reset (a new track).
    fn note_track_start(&self);
    /// Any thread.
    fn snapshot(&self) -> SessionSnapshot;
    /// Any thread: forget everything.
    fn clear(&self);
}

// MARK: A/B compare
//
// A `ComparisonSnapshot` is a frozen summary of what was playing: the long-term
// spectrum and the measurement numbers. The user captures "A", then looks at
// the live signal ("B") against it. Memory only; nothing is written to disk; no
// audio is kept.

#[derive(Debug, Clone, PartialEq)]
pub struct ComparisonSnapshot {
    /// Port note: Swift uses a random UUID. Here the caller gives a number
    /// that is unique in the app run, for example a counter.
    pub id: u64,
    /// Shown in the UI, for example "A · 21:42 · Qobuz".
    pub name: String,
    pub date: SystemTime,
    /// Seconds of audio the long-term values cover.
    pub measured_seconds: f64,
    /// Display bins of `average_db` and `peak_hold_db`.
    pub frequencies: Vec<f32>,
    /// Long-term (since reset) mid spectrum on the display bins, dBFS, WITH the
    /// display tilt that was active at capture (`tilt_db_per_octave`). To
    /// compare with a live frame under another tilt, remove both tilts first.
    pub average_db: Vec<f32>,
    /// Peak hold at the moment of capture, same bins.
    pub peak_hold_db: Vec<f32>,
    pub bands: BandEnergy,
    pub loudness: LoudnessReading,
    /// Correlation, width and balance at capture (300 ms values). Coarse, shown as context only.
    pub correlation: f32,
    pub width: f32,
    pub balance: f32,
    /// Headphone model at capture, when one was set: name and curves on the
    /// same bins (normalized 0 dB at 1 kHz).
    pub headphone_name: Option<String>,
    pub response_db: Option<Vec<f32>>,
    pub target_db: Option<Vec<f32>>,
    /// A-weighted Leq of the track at capture (dB SPL, DF equivalent) when calibrated.
    pub leq_a: Option<f32>,
    pub lowest_strong_hz: f32,
    /// Display tilt (dB per octave, pivot 1 kHz) that `average_db` and
    /// `peak_hold_db` carry. The app sets it after capture.
    pub tilt_db_per_octave: f32,
}

impl ComparisonSnapshot {
    /// Capture from a live frame. The frame's long-term spectrum
    /// (`spectrum.average`) must be valid (a few seconds measured); the caller
    /// checks `loudness.measured_seconds`.
    pub fn capture(frame: &AnalysisFrame, id: u64, name: impl Into<String>, date: SystemTime) -> Self {
        let headphone = frame.headphone.as_ref();
        Self {
            id,
            name: name.into(),
            date,
            measured_seconds: frame.loudness.measured_seconds,
            frequencies: frame.spectrum.frequencies.clone(),
            average_db: frame.spectrum.average.clone(),
            peak_hold_db: frame.spectrum.peak_hold.clone(),
            bands: frame.bands,
            loudness: frame.loudness.clone(),
            correlation: frame.stereo.correlation,
            width: frame.stereo.width,
            balance: frame.stereo.balance,
            headphone_name: headphone.map(|h| h.model_name.clone()),
            response_db: headphone.map(|h| h.response_db.clone()),
            target_db: headphone.filter(|h| h.has_target).map(|h| h.target_db.clone()),
            leq_a: frame.spl.as_ref().map(|spl| spl.leq_a_track),
            lowest_strong_hz: frame.lowest_strong_hz,
            tilt_db_per_octave: 0.0,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_error_shows_the_platform_message() {
        let error = SourceError::Platform("0x88890004".to_string());
        assert_eq!(error.to_string(), "capture failed: 0x88890004");
    }

    #[test]
    fn now_playing_line_joins_only_what_is_known() {
        let mut playing = NowPlaying {
            title: "Blue in Green".into(),
            artist: "Miles Davis".into(),
            album: String::new(),
            source: "Qobuz".into(),
            is_hi_res: false,
        };
        assert_eq!(playing.line(), "Miles Davis \u{2013} Blue in Green");
        playing.artist.clear();
        assert_eq!(playing.line(), "Blue in Green");
        playing.title.clear();
        assert_eq!(playing.line(), "");
    }

    #[test]
    fn silent_spectrum_is_log_spaced_at_the_floor() {
        let reading = SpectrumReading::silent(1024);
        assert_eq!(reading.frequencies.len(), 1024);
        assert_eq!(reading.frequencies[0], 10.0);
        assert!((reading.frequencies[1023] - 24_000.0).abs() < 0.5);
        let ratio = reading.frequencies[1] / reading.frequencies[0];
        for pair in reading.frequencies.windows(2) {
            assert!((pair[1] / pair[0] - ratio).abs() < 1e-4, "bins are not log spaced");
        }
        for curve in [
            &reading.left,
            &reading.right,
            &reading.mid,
            &reading.side,
            &reading.peak_hold,
            &reading.average,
        ] {
            assert_eq!(curve.len(), 1024);
            assert!(curve.iter().all(|&level| level == SpectrumReading::FLOOR_DB));
        }
        assert!(reading.mid_layers.is_empty());
        assert_eq!(SpectrumReading::silent(0).frequencies.len(), 2, "at least 2 bins");
    }

    #[test]
    fn defaults_match_the_swift_contract() {
        assert_eq!(BandEnergy::default().values(), [-120.0; 8]);
        assert_eq!(BandEnergy::NAMES.len() + 1, BandEnergy::EDGES_HZ.len());
        let loudness = LoudnessReading::default();
        assert_eq!(loudness.integrated_lufs, LoudnessReading::SILENCE_LUFS);
        assert_eq!(loudness.true_peak_max_dbtp, -120.0);
        assert!(!loudness.is_integrated_valid);
        let peak = PeakReading::default();
        assert_eq!((peak.level_db, peak.note_name.as_str()), (-120.0, ""));
        let settings = SpectrumSettings::default();
        assert_eq!(
            (settings.display_bins, settings.min_hz, settings.max_hz),
            (1024, 10.0, 24_000.0)
        );
        assert_eq!(
            (settings.release_seconds, settings.peak_decay_db_per_second),
            (0.25, 12.0)
        );
        assert_eq!(ThirdOctaveReading::new(vec![], vec![]).centers_hz.len(), 31);
        assert!(StressSeverity::High > StressSeverity::Watch && StressSeverity::Watch > StressSeverity::Info);
        let frame = AnalysisFrame::new(SpectrumReading::silent(8));
        assert!(frame.is_silent && frame.top_peaks.is_empty() && frame.spl.is_none());
    }

    #[test]
    fn sensitivity_converts_from_db_per_milliwatt() {
        // 1 mW into 1000 Ω is 1 V, so the two numbers are equal there.
        let equal = HeadphoneSensitivity::from_db_per_milliwatt(90.0, 1000.0, "test");
        assert!((equal.db_spl_per_volt - 90.0).abs() < 1e-12);
        // 10 Ω: 1 V is 100 mW, which is 20 dB more.
        let low = HeadphoneSensitivity::from_db_per_milliwatt(90.0, 10.0, "test");
        assert!((low.db_spl_per_volt - 110.0).abs() < 1e-12);
    }

    #[test]
    fn comparison_capture_copies_the_frame() {
        let mut frame = AnalysisFrame::new(SpectrumReading::silent(4));
        frame.spectrum.average = vec![-10.0, -20.0, -30.0, -40.0];
        frame.loudness.measured_seconds = 12.5;
        frame.stereo.width = 0.5;
        frame.lowest_strong_hz = 31.0;
        let date = SystemTime::UNIX_EPOCH;

        let plain = ComparisonSnapshot::capture(&frame, 1, "A", date);
        assert_eq!(plain.average_db, frame.spectrum.average);
        assert_eq!(
            (plain.measured_seconds, plain.width, plain.lowest_strong_hz),
            (12.5, 0.5, 31.0)
        );
        assert_eq!(
            (plain.headphone_name, plain.response_db, plain.target_db, plain.leq_a),
            (None, None, None, None)
        );

        // A model without a target curve gives no target to compare with.
        frame.headphone = Some(HeadphoneReading {
            model_name: "Test".into(),
            response_db: vec![1.0; 4],
            target_db: vec![0.0; 4],
            has_target: false,
            predicted_at_ear_db: vec![0.0; 4],
            stress_flags: Vec::new(),
        });
        let no_target = ComparisonSnapshot::capture(&frame, 2, "B", date);
        assert_eq!(no_target.headphone_name.as_deref(), Some("Test"));
        assert_eq!(no_target.response_db, Some(vec![1.0; 4]));
        assert_eq!(no_target.target_db, None);

        frame.headphone.as_mut().unwrap().has_target = true;
        assert_eq!(
            ComparisonSnapshot::capture(&frame, 3, "C", date).target_db,
            Some(vec![0.0; 4])
        );
    }

    #[test]
    fn traits_are_object_safe() {
        let _: Option<&dyn AudioSource> = None;
        let _: Option<&dyn NowPlayingSource> = None;
        let _: Option<&dyn SpectrumAnalyzing> = None;
        let _: Option<&dyn LoudnessMetering> = None;
        let _: Option<&dyn StereoAnalyzing> = None;
        let _: Option<&dyn HeadphoneModeling> = None;
        let _: Option<&dyn SplEstimating> = None;
        let _: Option<&dyn SessionRecording> = None;
    }
}
