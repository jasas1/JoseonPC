//! The stress flags: what this music asks of this headphone and of the chain.
//!
//! Port source: `Sources/JoseonHeadphones/StressFlags.swift`. The texts are the
//! same, character for character.

use joseon_core::contracts::{BandEnergy, LoudnessReading, SpectrumReading, StressFlag, StressSeverity};
use std::collections::HashMap;
use std::ops::{Range, RangeInclusive};

/// Every number the stress detector compares against. Defaults are a starting
/// point for loud listening on a full-size headphone; change them per listener.
#[derive(Debug, Clone, PartialEq)]
pub struct StressThresholds {
    // (a) sub-bass load
    /// `bands.sub_bass` (20–60 Hz RMS) at or above this raises the load flag.
    pub sub_bass_load_dbfs: f32,
    /// `bands.sub_bass` at or above this makes the load flag `High`.
    pub sub_bass_high_load_dbfs: f32,
    /// The load condition must hold this long before the flag appears. The text
    /// of the flag quotes this number, which is why it has its own knob.
    pub sub_bass_sustain_seconds: f64,

    // (b) sub-bass under-delivery
    /// Upper edge of the "deep bass" window the under-delivery check looks at.
    pub deep_bass_top_hz: f32,
    /// A display bin in 20 Hz…`deep_bass_top_hz` must reach this before the check counts as real content.
    pub deep_bass_content_dbfs: f32,
    /// Mean shortfall of response against target in that window, in dB, that raises the flag.
    pub under_delivery_db: f32,

    // (c) treble hot spot
    pub treble_low_hz: f32,
    pub treble_high_hz: f32,
    /// Response peak over target, in dB, that raises the flag.
    pub treble_excess_db: f32,
    /// Response peak over target, in dB, that makes it `High`.
    pub treble_excess_high_db: f32,
    /// A display bin in the treble window must reach this for the music to count as energetic there.
    pub treble_content_dbfs: f32,

    // (d) inter-sample overs
    /// `loudness.true_peak_max_dbtp` above this raises the over flag.
    pub true_peak_ceiling_dbtp: f32,

    // (e) dense master
    /// PLR under this counts as a dense master.
    pub dense_plr_db: f32,
    /// …but only after this much audio has been measured.
    pub dense_min_measured_seconds: f64,

    // Hysteresis. A single threshold plus a short hold cannot survive a value
    // that sits on the threshold. The ladder below is wide enough that a
    // hovering value gives one continuous flag or no flag at all, never a flicker.
    /// A level flag appears only after its condition has held this long.
    pub level_attack_seconds: f64,
    /// …and clears only after its value has stayed this far under its threshold…
    pub release_margin_db: f32,
    /// …for this long. Event flags (clipped samples, inter-sample overs) never
    /// clear on their own: they latch until the flags are reset, because the
    /// event really did happen.
    pub hold_seconds: f64,
}

impl Default for StressThresholds {
    fn default() -> Self {
        Self {
            sub_bass_load_dbfs: -24.0,
            sub_bass_high_load_dbfs: -15.0,
            sub_bass_sustain_seconds: 3.0,
            deep_bass_top_hz: 40.0,
            deep_bass_content_dbfs: -50.0,
            under_delivery_db: 6.0,
            treble_low_hz: 4_000.0,
            treble_high_hz: 10_000.0,
            treble_excess_db: 4.0,
            treble_excess_high_db: 8.0,
            treble_content_dbfs: -45.0,
            true_peak_ceiling_dbtp: 0.0,
            dense_plr_db: 8.0,
            dense_min_measured_seconds: 30.0,
            level_attack_seconds: 3.0,
            release_margin_db: 2.0,
            hold_seconds: 5.0,
        }
    }
}

/// Stable identifiers for the flags this crate raises.
pub mod flag_id {
    pub const SUB_BASS_LOAD: &str = "headphone.subBassLoad";
    pub const SUB_BASS_UNDER_DELIVERY: &str = "headphone.subBassUnderDelivery";
    pub const TREBLE_HOT_SPOT: &str = "headphone.trebleHotSpot";
    pub const INTER_SAMPLE_OVERS: &str = "chain.interSampleOvers";
    pub const DENSE_MASTER: &str = "master.dense";
    pub const CLIPPED_SAMPLES: &str = "master.clipped";

    /// Declaration order, used to keep the flag list stable on screen.
    pub const ALL: [&str; 6] = [
        SUB_BASS_LOAD,
        SUB_BASS_UNDER_DELIVERY,
        TREBLE_HOT_SPOT,
        INTER_SAMPLE_OVERS,
        DENSE_MASTER,
        CLIPPED_SAMPLES,
    ];
}

/// What the detector needs to know about the loaded headphone. Built once by `HeadphoneModel`.
#[derive(Debug, Clone, PartialEq)]
pub struct StressContext {
    pub curve_name: String,
    pub target_name: Option<String>,
    /// Curve frequencies, ascending.
    pub curve_frequencies: Vec<f32>,
    /// Curve levels, normalized to 0 dB at 1 kHz.
    pub curve_levels: Vec<f32>,
    /// Target levels on `curve_frequencies`, normalized to 0 dB at 1 kHz. `None` when there is no target.
    pub target_levels: Option<Vec<f32>>,
    /// Mean response over 20–60 Hz: how much bass the driver has to make. Computed once.
    mean_sub_bass_response_db: Option<f32>,
}

fn mean(values: &[f32]) -> f32 {
    let mut sum = 0.0f32;
    for value in values {
        sum += value;
    }
    sum / values.len() as f32
}

impl StressContext {
    pub fn new(
        curve_name: String,
        target_name: Option<String>,
        curve_frequencies: Vec<f32>,
        curve_levels: Vec<f32>,
        target_levels: Option<Vec<f32>>,
    ) -> Self {
        let mut context = Self {
            curve_name,
            target_name,
            curve_frequencies,
            curve_levels,
            target_levels,
            mean_sub_bass_response_db: None,
        };
        context.mean_sub_bass_response_db = context.mean_response_db(20.0, 60.0);
        context
    }

    pub fn mean_sub_bass_response_db(&self) -> Option<f32> {
        self.mean_sub_bass_response_db
    }

    /// Mean normalized response over a frequency window, in dB.
    pub fn mean_response_db(&self, low_hz: f32, high_hz: f32) -> Option<f32> {
        let window = window_indices(&self.curve_frequencies, low_hz, high_hz);
        self.curve_levels
            .get(window)
            .filter(|levels| !levels.is_empty())
            .map(mean)
    }

    /// Mean (target − response) over a window, in dB. Positive means the headphone is short.
    pub fn mean_shortfall_db(&self, low_hz: f32, high_hz: f32) -> Option<f32> {
        self.mean_shortfall_in(window_indices(&self.curve_frequencies, low_hz, high_hz))
    }

    /// Largest (response − target) in a window, with the frequency where it happens: `(excess dB, Hz)`.
    pub fn max_excess_over_target(&self, low_hz: f32, high_hz: f32) -> Option<(f32, f32)> {
        self.max_excess_in(window_indices(&self.curve_frequencies, low_hz, high_hz))
    }

    // Index-range versions: the detector resolves the window once and reuses it every frame.

    fn mean_shortfall_in(&self, window: Range<usize>) -> Option<f32> {
        let target = self.target_levels.as_ref()?.get(window.clone())?;
        let curve = self.curve_levels.get(window)?;
        if curve.is_empty() {
            return None;
        }
        let mut sum = 0.0f32;
        for (t, c) in target.iter().zip(curve) {
            sum += t - c;
        }
        Some(sum / curve.len() as f32)
    }

    fn max_excess_in(&self, window: Range<usize>) -> Option<(f32, f32)> {
        let target = self.target_levels.as_ref()?.get(window.clone())?;
        let curve = self.curve_levels.get(window.clone())?;
        let frequencies = self.curve_frequencies.get(window)?;
        if curve.is_empty() {
            return None;
        }
        let (mut best_excess, mut best_index) = (f32::MIN, 0);
        for (i, (c, t)) in curve.iter().zip(target).enumerate() {
            let excess = c - t;
            if excess > best_excess {
                (best_excess, best_index) = (excess, i);
            }
        }
        Some((best_excess, frequencies[best_index]))
    }
}

/// How a flag writes its numbers. One place, so a number cannot appear as
/// "−9 dBFS" on the plot and "−9.4 dBFS" in the popover: every text of a flag
/// formats a quantity through the same function, with the same rounding. Minus
/// is U+2212, like everywhere else in the app.
pub mod flag_text {
    /// One decimal: "−21.3", "0.0", "4.6".
    pub fn db(value: f32) -> String {
        number(value, 1, false)
    }

    /// One decimal with an explicit sign: "+4.6", "−7.2", "+0.0".
    pub fn signed_db(value: f32) -> String {
        number(value, 1, true)
    }

    /// Two decimals with an explicit sign, for true peak: "+1.30".
    pub fn signed_db2(value: f32) -> String {
        number(value, 2, true)
    }

    /// One decimal under 1 kHz ("40.0 Hz"), "x.xx kHz" from there up ("6.10 kHz").
    pub fn hz(value: f32) -> String {
        if value < 999.95 {
            format!("{value:.1} Hz")
        } else {
            format!("{:.2} kHz", value / 1_000.0)
        }
    }

    fn number(value: f32, decimals: usize, signed: bool) -> String {
        let scale = 10f32.powi(decimals as i32);
        let mut rounded = (value * scale).round() / scale;
        if rounded == 0.0 {
            rounded = 0.0; // no "−0.0"
        }
        let digits = format!("{:.*}", decimals, rounded.abs());
        if rounded < 0.0 {
            format!("\u{2212}{digits}")
        } else if signed {
            format!("+{digits}")
        } else {
            digits
        }
    }
}

/// Resolves a frequency window to an index range on an ascending frequency array.
pub(crate) fn window_indices(frequencies: &[f32], low_hz: f32, high_hz: f32) -> Range<usize> {
    if frequencies.is_empty() || !(low_hz <= high_hz) {
        return 0..0;
    }
    let start = frequencies.partition_point(|&f| f < low_hz); // first index with f >= low
    let end = frequencies.partition_point(|&f| f <= high_hz); // first index with f > high
    if start < end {
        start..end
    } else {
        0..0
    }
}

/// Top of `BandEnergy::sub_bass`, which is what the sub-bass load level is measured over.
const SUB_BASS_BAND_TOP_HZ: f32 = 60.0;
/// Half-width of the treble hot spot band, in octaves.
const HOT_SPOT_HALF_WIDTH_OCTAVES: f32 = 1.0 / 6.0;

/// `hz` widened by `octaves` either side, kept inside `bounds` and always a valid range.
pub(crate) fn span_around(hz: f32, octaves: f32, bounds: RangeInclusive<f32>) -> Option<RangeInclusive<f32>> {
    let (bottom, top) = (*bounds.start(), *bounds.end());
    if !(hz > 0.0 && hz.is_finite() && top > bottom) {
        return None;
    }
    let factor = octaves.exp2();
    let low = (hz / factor).max(bottom).min(top);
    let high = (hz * factor).max(bottom).min(top);
    (high > low).then_some(low..=high)
}

/// Loudest display bin over an index range of `spectrum.mid`, in dBFS.
pub(crate) fn max_bin_db(spectrum: &SpectrumReading, window: Range<usize>) -> f32 {
    let floor = SpectrumReading::FLOOR_DB;
    match spectrum.mid.get(window) {
        Some(bins) => bins
            .iter()
            .fold(floor, |best, &level| if level > best { level } else { best }),
        None => floor,
    }
}

/// One flag's hysteresis state.
#[derive(Default)]
struct Gate {
    /// When the raise condition last started holding. `None` while it is false.
    true_since: Option<f64>,
    /// When the clear condition last started holding. `None` while it is false.
    clear_since: Option<f64>,
    /// When the flag went up. Survives as long as the flag does.
    raised_at: Option<f64>,
    held: Option<StressFlag>,
}

/// What makes a flag go up and come down.
enum Trigger {
    /// A measured level against a threshold. `raising` is the condition itself.
    /// `clearing` is the value having gone past the release margin. Between the
    /// two the flag keeps the state it has: a value that hovers on the
    /// threshold can neither raise a new flag nor drop a raised one.
    Level {
        raising: bool,
        clearing: bool,
        attack: f64,
        release: f64,
    },
    /// Something that happened. It latches until `reset`.
    Event(bool),
}

/// Index ranges resolved once per (grid, thresholds) pair, so a 60 Hz loop does no searching.
#[derive(Default)]
struct WindowCache {
    valid: bool,
    spectrum_frequencies: Vec<f32>,
    curve_frequencies: Vec<f32>,
    thresholds: Option<StressThresholds>,
    spectrum_deep_bass: Range<usize>,
    spectrum_treble: Range<usize>,
    curve_deep_bass: Range<usize>,
    curve_treble: Range<usize>,
}

/// Monotonic seconds.
pub type MonotonicClock = Box<dyn Fn() -> f64 + Send>;

/// Raises and holds the stress flags. One instance per `HeadphoneModel`.
///
/// The clock is injected, so tests drive the hysteresis without a wait.
pub struct StressDetector {
    now: MonotonicClock,
    gates: HashMap<&'static str, Gate>,
    windows: WindowCache,
}

impl Default for StressDetector {
    fn default() -> Self {
        Self::new(Box::new(joseon_core::engine::uptime))
    }
}

impl StressDetector {
    pub fn new(now: MonotonicClock) -> Self {
        Self {
            now,
            gates: HashMap::new(),
            windows: WindowCache::default(),
        }
    }

    pub fn reset(&mut self) {
        self.gates.clear();
    }

    fn refresh_windows(&mut self, spectrum: &SpectrumReading, context: &StressContext, t: &StressThresholds) {
        let w = &mut self.windows;
        if w.valid
            && w.thresholds.as_ref() == Some(t)
            && w.spectrum_frequencies == spectrum.frequencies
            && w.curve_frequencies == context.curve_frequencies
        {
            return;
        }
        w.spectrum_frequencies.clone_from(&spectrum.frequencies);
        w.curve_frequencies.clone_from(&context.curve_frequencies);
        w.thresholds = Some(t.clone());
        w.spectrum_deep_bass = window_indices(&spectrum.frequencies, 20.0, t.deep_bass_top_hz);
        w.spectrum_treble = window_indices(&spectrum.frequencies, t.treble_low_hz, t.treble_high_hz);
        w.curve_deep_bass = window_indices(&context.curve_frequencies, 20.0, t.deep_bass_top_hz);
        w.curve_treble = window_indices(&context.curve_frequencies, t.treble_low_hz, t.treble_high_hz);
        w.valid = true;
    }

    /// The flags that are up now, highest severity first, declaration order inside a severity.
    ///
    /// A flag with a `frequency_range_hz` is drawn as a shaded band on the
    /// spectrum, with its `plot_label` next to it. The span is the span the
    /// number was measured over, never a decorative one. Flags about the whole
    /// signal (inter-sample overs, dense master, clipped samples) leave both
    /// empty: there is no frequency to point at.
    pub fn evaluate(
        &mut self,
        spectrum: &SpectrumReading,
        bands: &BandEnergy,
        loudness: &LoudnessReading,
        t: &StressThresholds,
        context: &StressContext,
    ) -> Vec<StressFlag> {
        let time = (self.now)();
        self.refresh_windows(spectrum, context, t);
        let mut out: Vec<StressFlag> = Vec::new();
        let level_trigger = |raising: bool, clearing: bool, attack: f64| Trigger::Level {
            raising,
            clearing,
            attack,
            release: t.hold_seconds,
        };
        let target_name = context.target_name.as_deref().unwrap_or("target");

        // (a) Sub-bass level: sustained strong 20–60 Hz content *in the signal*,
        // and what the response of the headphone does with it. Nothing about
        // driver excursion: that depends on the playback level, and the app
        // does not know the playback level.
        {
            let level = bands.sub_bass;
            let severity = if level >= t.sub_bass_high_load_dbfs {
                StressSeverity::High
            } else {
                StressSeverity::Watch
            };
            let trigger = level_trigger(
                level >= t.sub_bass_load_dbfs,
                level < t.sub_bass_load_dbfs - t.release_margin_db,
                t.sub_bass_sustain_seconds,
            );
            self.update(flag_id::SUB_BASS_LOAD, trigger, time, &mut out, || {
                let shape = match context.mean_sub_bass_response_db {
                    Some(mean) => format!(
                        "{} plays 20–60 Hz at {} dB against its own level at 1.00 kHz.",
                        context.curve_name,
                        flag_text::signed_db(mean)
                    ),
                    None => format!("{} is not measured below 60 Hz.", context.curve_name),
                };
                let level_text = format!("{} dBFS", flag_text::db(level));
                StressFlag {
                    id: flag_id::SUB_BASS_LOAD.into(),
                    severity,
                    title: "Strong sub-bass in the signal".into(),
                    detail: format!(
                        "20–60 Hz sits at {level_text} in the signal for over {:.1} s. {shape} \
                         How loud that is at the ear depends on the playback level, which Joseon does not know.",
                        t.sub_bass_sustain_seconds
                    ),
                    // The sub-bass band itself: the span the level was measured over.
                    frequency_range_hz: Some(20.0..=SUB_BASS_BAND_TOP_HZ),
                    plot_label: level_text,
                }
            });
        }

        // (b) Sub-bass under-delivery: deep content the headphone will not reproduce.
        {
            let content = max_bin_db(spectrum, self.windows.spectrum_deep_bass.clone());
            let shortfall = context.mean_shortfall_in(self.windows.curve_deep_bass.clone());
            // The shortfall belongs to the headphone and does not move. The
            // signal level in the window is the part that needs hysteresis.
            let headphone_is_short = shortfall.unwrap_or(0.0) > t.under_delivery_db;
            let trigger = level_trigger(
                headphone_is_short && content >= t.deep_bass_content_dbfs,
                !headphone_is_short || content < t.deep_bass_content_dbfs - t.release_margin_db,
                t.level_attack_seconds,
            );
            self.update(flag_id::SUB_BASS_UNDER_DELIVERY, trigger, time, &mut out, || {
                // One number, one rounding, in both texts: response minus target, signed.
                let against = format!(
                    "{} dB vs {target_name}",
                    flag_text::signed_db(-shortfall.unwrap_or(0.0))
                );
                StressFlag {
                    id: flag_id::SUB_BASS_UNDER_DELIVERY.into(),
                    severity: StressSeverity::Watch,
                    title: "Deep bass under target".into(),
                    detail: format!(
                        "The signal reaches {} dBFS below {}. {} plays that range at {against}, \
                         so it comes out quieter than the target intends.",
                        flag_text::db(content),
                        flag_text::hz(t.deep_bass_top_hz),
                        context.curve_name
                    ),
                    // Exactly the span the shortfall was averaged over.
                    frequency_range_hz: Some(20.0..=t.deep_bass_top_hz.max(21.0)),
                    plot_label: against,
                }
            });
        }

        // (c) Treble hot spot: sibilance risk where the response peaks over target.
        {
            let peak = context.max_excess_in(self.windows.curve_treble.clone());
            let content = max_bin_db(spectrum, self.windows.spectrum_treble.clone());
            let (excess, hz) = peak.unwrap_or((0.0, 0.0));
            // Again the peak of the curve is fixed. The energy of the music up there is the level.
            let headphone_is_peaky = peak.is_some() && excess >= t.treble_excess_db;
            let severity = if excess >= t.treble_excess_high_db {
                StressSeverity::High
            } else {
                StressSeverity::Watch
            };
            let trigger = level_trigger(
                headphone_is_peaky && content >= t.treble_content_dbfs,
                !headphone_is_peaky || content < t.treble_content_dbfs - t.release_margin_db,
                t.level_attack_seconds,
            );
            self.update(flag_id::TREBLE_HOT_SPOT, trigger, time, &mut out, || {
                let against = format!("{} dB vs {target_name}", flag_text::signed_db(excess));
                StressFlag {
                    id: flag_id::TREBLE_HOT_SPOT.into(),
                    severity,
                    title: "Treble hot spot".into(),
                    detail: format!(
                        "{} plays {} at {against}, and the signal reaches {} dBFS between {} and {}: sibilance risk.",
                        context.curve_name,
                        flag_text::hz(hz),
                        flag_text::db(content),
                        flag_text::hz(t.treble_low_hz),
                        flag_text::hz(t.treble_high_hz)
                    ),
                    // The peak itself, a sixth of an octave either side: wide
                    // enough to see on a log axis, narrow enough that it still
                    // points at the frequency it names.
                    frequency_range_hz: span_around(
                        hz,
                        HOT_SPOT_HALF_WIDTH_OCTAVES,
                        t.treble_low_hz..=t.treble_high_hz,
                    ),
                    plot_label: against,
                }
            });
        }

        // (d) Inter-sample overs: the reconstructed waveform passes 0 dBFS. An
        // over is an event, and `true_peak_max_dbtp` is the max since the
        // measurement started: it latches until the measurement resets, and so
        // does the flag.
        {
            let true_peak = loudness.true_peak_max_dbtp;
            self.update(
                flag_id::INTER_SAMPLE_OVERS,
                Trigger::Event(true_peak > t.true_peak_ceiling_dbtp),
                time,
                &mut out,
                || StressFlag {
                    id: flag_id::INTER_SAMPLE_OVERS.into(),
                    severity: StressSeverity::High,
                    title: "Inter-sample overs".into(),
                    detail: format!(
                        "True peak reached {} dBTP (ceiling {} dBTP). \
                         A DAC or upsampler can clip: drop the player volume about {:.0} dB.",
                        flag_text::signed_db2(true_peak),
                        flag_text::signed_db2(t.true_peak_ceiling_dbtp),
                        (true_peak - t.true_peak_ceiling_dbtp).ceil().max(1.0)
                    ),
                    frequency_range_hz: None,
                    plot_label: String::new(),
                },
            );
        }

        // (e) Dense master: low dynamics in the recording, not a headphone fault.
        {
            let plr = loudness.plr_db;
            // PLR is 0 until the integrated loudness is a measurement: silence must not read as "dense".
            let measured = loudness.is_integrated_valid && loudness.measured_seconds >= t.dense_min_measured_seconds;
            // This threshold is the other way up: low PLR is the condition, so
            // the release margin sits above it.
            let trigger = level_trigger(
                measured && plr < t.dense_plr_db,
                measured && plr > t.dense_plr_db + t.release_margin_db,
                t.level_attack_seconds,
            );
            self.update(flag_id::DENSE_MASTER, trigger, time, &mut out, || StressFlag {
                id: flag_id::DENSE_MASTER.into(),
                severity: StressSeverity::Info,
                title: "Dense master".into(),
                detail: format!(
                    "PLR {} dB after {:.0} s measured (true peak {} dBTP, integrated {} LUFS). \
                     Low dynamics in the master, not the headphone.",
                    flag_text::db(plr),
                    loudness.measured_seconds,
                    flag_text::signed_db2(loudness.true_peak_max_dbtp),
                    flag_text::db(loudness.integrated_lufs)
                ),
                frequency_range_hz: None,
                plot_label: String::new(),
            });
        }

        // (f) Clipped samples: full-scale runs already in the stream.
        // `clip_count` counts since the measurement reset, so this latches too.
        {
            let count = loudness.clip_count;
            self.update(flag_id::CLIPPED_SAMPLES, Trigger::Event(count > 0), time, &mut out, || StressFlag {
                id: flag_id::CLIPPED_SAMPLES.into(),
                severity: StressSeverity::High,
                title: "Clipped samples".into(),
                detail: format!(
                    "{count} run{} of 3+ samples at or above 0 dBFS since reset. The source file or the player is clipping.",
                    if count == 1 { "" } else { "s" }
                ),
                frequency_range_hz: None,
                plot_label: String::new(),
            });
        }

        // `out` is in declaration order. A stable sort keeps that order inside a severity.
        out.sort_by_key(|flag| std::cmp::Reverse(flag.severity));
        out
    }

    /// Hysteresis. A level flag needs `attack` seconds of a true condition to
    /// go up, and `release` seconds past the release margin to come down; in
    /// between it holds. An event flag goes up at once and stays up until `reset`.
    ///
    /// The text is rebuilt only while the raise condition is true, so a held
    /// flag keeps the numbers it was raised with.
    fn update(
        &mut self,
        id: &'static str,
        trigger: Trigger,
        time: f64,
        out: &mut Vec<StressFlag>,
        make: impl FnOnce() -> StressFlag,
    ) {
        let gate = self.gates.entry(id).or_default();
        match trigger {
            Trigger::Event(happened) => {
                if happened {
                    gate.held = Some(make());
                    gate.raised_at.get_or_insert(time);
                }
            }
            Trigger::Level {
                raising: true, attack, ..
            } => {
                gate.clear_since = None;
                let since = *gate.true_since.get_or_insert(time);
                if time - since >= attack {
                    gate.held = Some(make());
                    gate.raised_at.get_or_insert(time);
                }
            }
            Trigger::Level {
                raising: false,
                clearing,
                release,
                ..
            } => {
                gate.true_since = None;
                if clearing {
                    let since = *gate.clear_since.get_or_insert(time);
                    if gate.held.is_some() && time - since >= release {
                        gate.held = None;
                        gate.raised_at = None;
                    }
                } else {
                    // Inside the margin: the release timer starts again the next
                    // time the value really does drop clear.
                    gate.clear_since = None;
                }
            }
        }
        if let Some(held) = &gate.held {
            out.push(held.clone());
        }
    }

    /// Clock time each flag that is up now was raised, by flag id.
    ///
    /// The flag texts stay still: a "since 1:23" inside `detail` would rewrite
    /// every flag every second. A panel that wants the elapsed time asks for
    /// the onset and formats it at the rate it likes.
    pub fn flag_onsets(&self) -> HashMap<String, f64> {
        self.gates
            .iter()
            .filter(|(_, gate)| gate.held.is_some())
            .filter_map(|(id, gate)| gate.raised_at.map(|raised| (id.to_string(), raised)))
            .collect()
    }
}
