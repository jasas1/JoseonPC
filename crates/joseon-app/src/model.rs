//! The audio source, the analysis engine and the text of the header.
//!
//! Port source: `AppModel.swift` (the source lifecycle, the header state, freeze). The headphone
//! model, the level at the ear, the comparison and the track history are later steps.

use crate::number_text;
use crate::settings::{AppSettings, LoudnessTarget};
use joseon_capture::NowPlayingReader;
use joseon_capture::{CaptureHealth, SystemLoopback};
use joseon_core::contracts::{
    AnalysisFrame, AudioSource, NowPlayingSource, SessionSnapshot, SharedHeadphoneModel, SpectrumSettings, StreamInfo,
    StressFlag,
};
use joseon_core::engine::{is_at_rest, AnalysisEngine};
use joseon_core::test_signals::DemoAudioSource;
use joseon_headphones::library::HeadphoneLibrary;
use joseon_headphones::model::HeadphoneModel;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;

/// The one phrase the app uses when no audio arrives: header and panel placeholders.
pub const WAITING_FOR_AUDIO: &str = "Waiting for audio";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SignalState {
    Live,
    #[default]
    Silent,
    Demo,
}

impl SignalState {
    pub fn label(self) -> &'static str {
        match self {
            Self::Live => "Live",
            Self::Silent => WAITING_FOR_AUDIO,
            Self::Demo => "Demo signal",
        }
    }
}

/// Text for the header strip.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub struct HeaderState {
    /// Programs that play audio, for example "Qobuz". Empty when none plays.
    pub sources: String,
    pub device: String,
    /// "96 kHz", or empty.
    pub sample_rate: String,
    /// "24 bit", or empty.
    pub bit_depth: String,
    /// "volume −17 dB" while the capture takes the volume of the system out of the samples. Else empty.
    pub volume: String,
    /// "Artist – Title" of the song that plays. Empty when no player tells it.
    pub now_playing: String,
    pub state: SignalState,
}

/// 96000 gives "96 kHz", 44100 gives "44.1 kHz". Empty for no rate.
pub fn format_sample_rate(sample_rate: f64) -> String {
    if !(sample_rate > 0.0) {
        return String::new();
    }
    let khz = sample_rate / 1000.0;
    if (khz - khz.round()).abs() < 0.001 {
        format!("{} kHz", khz.round() as i64)
    } else {
        format!("{khz:.1} kHz")
    }
}

/// "volume −17 dB" for a compensation of 17 dB. Empty under half a dB: there is nothing to tell.
pub fn volume_text(compensation_db: f32) -> String {
    if !(compensation_db >= 0.5) {
        return String::new();
    }
    format!("volume \u{2212}{:.0} dB", compensation_db)
}

impl HeaderState {
    pub fn new(stream: Option<&StreamInfo>, state: SignalState) -> Self {
        let Some(s) = stream else {
            return Self {
                state,
                ..Self::default()
            };
        };
        Self {
            sources: s.active_sources.join(", "),
            device: s.device_name.clone(),
            sample_rate: format_sample_rate(s.sample_rate),
            bit_depth: s.bit_depth.map_or(String::new(), |bits| format!("{bits} bit")),
            volume: String::new(),
            now_playing: String::new(),
            state,
        }
    }

    /// First line of the stream block: the programs that play, or the one no-audio phrase.
    /// Demo mode: "Demo signal", once.
    pub fn headline(&self) -> String {
        if self.state == SignalState::Demo {
            return SignalState::Demo.label().to_string();
        }
        if self.state == SignalState::Silent || self.sources.is_empty() {
            return WAITING_FOR_AUDIO.to_string();
        }
        self.sources.clone()
    }

    /// Second line: device · sample rate · bit depth. At silence the programs lead the line.
    pub fn facts(&self) -> Vec<String> {
        let mut parts = Vec::new();
        if self.state == SignalState::Silent && !self.sources.is_empty() {
            parts.push(self.sources.clone());
        }
        // The demo source names its device "Demo signal": the headline says that already. Say it once.
        let repeats = self.state == SignalState::Demo && self.device.eq_ignore_ascii_case(SignalState::Demo.label());
        if !self.device.is_empty() && !repeats {
            parts.push(self.device.clone());
        }
        for part in [&self.sample_rate, &self.bit_depth, &self.volume] {
            if !part.is_empty() {
                parts.push(part.clone());
            }
        }
        parts
    }
}

pub const DEFAULT_HEADPHONE_NAME: &str = "HiFiMAN Susvara Unveiled";
pub const DEFAULT_TARGET_NAME: &str = "Harman over-ear 2018";
/// A flag that is gone stays on screen this long: flags flicker with the music.
const FLAG_HOLD_SECONDS: f64 = 4.0;

/// Integrated loudness against the loudness target, as text for the strip under the header.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct LoudnessDelta {
    pub text: String,
    pub relation: Relation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Relation {
    /// No value yet.
    None,
    Under,
    OnTarget,
    Over,
}

impl LoudnessDelta {
    /// "I −11.5 · +2.5 LU over target". `None` with the target off.
    pub fn new(integrated_lufs: f32, target: LoudnessTarget) -> Option<Self> {
        let target_lufs = target.lufs()?;
        if !(integrated_lufs.is_finite() && integrated_lufs > -119.0) {
            return Some(Self {
                text: "I \u{2014}".to_string(),
                relation: Relation::None,
            });
        }
        let integrated = format!("I {}", number_text::signed(integrated_lufs as f64, 1, false));
        let delta = ((integrated_lufs - target_lufs) * 10.0).round() / 10.0;
        if delta.abs() < 0.5 {
            return Some(Self {
                text: format!("{integrated} \u{00B7} on target"),
                relation: Relation::OnTarget,
            });
        }
        let side = if delta > 0.0 { "over" } else { "under" };
        Some(Self {
            text: format!(
                "{integrated} \u{00B7} {} LU {side} target",
                number_text::signed(delta as f64, 1, true)
            ),
            relation: if delta > 0.0 { Relation::Over } else { Relation::Under },
        })
    }
}

/// What the strip under the header shows: the headphone, its stress flags, the loudness against the target.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct StressState {
    /// The name of the headphone model. `None` = no headphone is chosen.
    pub headphone: Option<String>,
    /// Highest severity first.
    pub flags: Vec<StressFlag>,
    /// No audio yet: nothing was checked.
    pub waiting: bool,
    pub loudness: Option<LoudnessDelta>,
}

/// The names the menus offer.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CurveNames {
    pub headphones: Vec<String>,
    pub targets: Vec<String>,
}

/// Flags flicker with the music. Holds each one for a few seconds, highest severity first. The detail text of a
/// held flag changes at most once per second.
#[derive(Default)]
pub struct FlagHolder {
    /// The flag, when it was last seen, when its text was last taken.
    held: HashMap<String, (StressFlag, f64, f64)>,
}

impl FlagHolder {
    pub fn clear(&mut self) {
        self.held.clear();
    }

    pub fn update(&mut self, current: &[StressFlag], now: f64) -> Vec<StressFlag> {
        for flag in current {
            let mut flag = flag.clone();
            // One minus sign everywhere: the model's text may carry an ASCII hyphen.
            flag.title = number_text::typographic(&flag.title);
            flag.detail = number_text::typographic(&flag.detail);
            let keep_text = self.held.get(&flag.id).filter(|(held, _, shown)| {
                held.severity == flag.severity && held.title == flag.title && now - shown < 1.0
            });
            let entry = match keep_text {
                Some((held, _, shown)) => (held.clone(), now, *shown),
                None => (flag.clone(), now, now),
            };
            self.held.insert(flag.id.clone(), entry);
        }
        self.held.retain(|_, (_, seen, _)| now - *seen <= FLAG_HOLD_SECONDS);
        let mut flags: Vec<StressFlag> = self.held.values().map(|(flag, _, _)| flag.clone()).collect();
        flags.sort_by(|a, b| {
            (b.severity as i32)
                .cmp(&(a.severity as i32))
                .then_with(|| a.id.cmp(&b.id))
        });
        flags
    }
}

enum Source {
    Loopback(SystemLoopback),
    Demo(DemoAudioSource),
}

/// What the window shows above the cards, besides the header.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Banner {
    pub text: String,
    /// True for a failure (amber); false for advice (quiet).
    pub is_error: bool,
}

/// Owns the audio source, the analysis engine and the settings.
pub struct AppModel {
    pub settings: AppSettings,
    engine: AnalysisEngine,
    source: Option<Source>,
    is_demo: bool,
    /// Set when the capture did not start. The app shows it and runs the demo source.
    capture_error: Option<String>,
    frozen: Option<Arc<AnalysisFrame>>,
    stream: Option<StreamInfo>,
    now_playing: NowPlayingReader,
    library: HeadphoneLibrary,
    names: CurveNames,
    /// The headphone and the target the engine has now.
    applied_model: Option<String>,
    headphone: Option<String>,
    flags: FlagHolder,
}

/// Analysis frames per second while the window shows.
pub const ENGINE_RATE: f64 = 60.0;

impl AppModel {
    /// The curves of the user are in the folder "Curves" beside the settings file.
    pub fn new(settings: AppSettings) -> Self {
        let folder = crate::settings::settings_path()
            .parent()
            .map_or_else(std::env::temp_dir, Path::to_path_buf)
            .join("Curves");
        Self::with_curves_folder(settings, folder)
    }

    pub fn with_curves_folder(settings: AppSettings, curves_folder: PathBuf) -> Self {
        let mut model = Self {
            settings,
            engine: AnalysisEngine::new(),
            source: None,
            is_demo: false,
            capture_error: None,
            frozen: None,
            stream: None,
            now_playing: joseon_capture::now_playing_reader(),
            library: HeadphoneLibrary::new(curves_folder),
            names: CurveNames::default(),
            applied_model: None,
            headphone: None,
            flags: FlagHolder::default(),
        };
        model.reload_library();
        // First run only: the user never chose a headphone (also not "None"), so the default pair is chosen.
        if model.settings.headphone_name.is_none() && model.names.headphones.iter().any(|n| n == DEFAULT_HEADPHONE_NAME)
        {
            model.settings.headphone_name = Some(DEFAULT_HEADPHONE_NAME.to_string());
            if model.settings.target_name.is_empty() {
                model.settings.target_name = DEFAULT_TARGET_NAME.to_string();
            }
        }
        model.apply_settings();
        model
    }

    /// Starts the capture. When it does not start (or the settings ask for it), the demo signal runs.
    pub fn start(&mut self) {
        self.shutdown();
        self.capture_error = None;
        if !self.settings.demo_mode {
            let mut source = joseon_capture::system_loopback();
            source.set_volume_compensation(self.settings.compensate_system_volume);
            source.set_volume_compensation_always(self.settings.compensate_volume_always);
            match source.start() {
                Ok(()) => {
                    self.begin(source.take_reader(), false);
                    self.source = Some(Source::Loopback(source));
                    self.apply_settings();
                    return;
                }
                Err(error) => self.capture_error = Some(error.to_string()),
            }
        }
        let mut source = DemoAudioSource::new();
        let reader = source.take_reader();
        // The demo source has no way to fail.
        let _ = source.start();
        self.begin(reader, true);
        self.source = Some(Source::Demo(source));
        self.apply_settings();
    }

    fn begin(&mut self, reader: Option<joseon_core::ring_buffer::RingReader>, demo: bool) {
        self.is_demo = demo;
        if let Some(reader) = reader {
            self.engine.start(reader, ENGINE_RATE);
        }
    }

    pub fn shutdown(&mut self) {
        self.engine.stop();
        match self.source.take() {
            Some(Source::Loopback(mut source)) => source.stop(),
            Some(Source::Demo(mut source)) => source.stop(),
            None => {}
        }
        self.now_playing.stop();
        self.stream = None;
    }

    /// Settings to the engine and to the capture.
    pub fn apply_settings(&mut self) {
        // The reader of the song name runs only while the name shows, and never for the demo signal.
        let wanted = self.settings.show_now_playing && self.source.is_some() && !self.is_demo;
        if wanted && !self.now_playing.is_running() {
            self.now_playing.start();
        } else if !wanted && self.now_playing.is_running() {
            self.now_playing.stop();
        }
        self.apply_headphone();
        let s = &self.settings;
        if let Some(Source::Loopback(source)) = &self.source {
            source.set_volume_compensation(s.compensate_system_volume);
            source.set_volume_compensation_always(s.compensate_volume_always);
        }
        self.engine.update_spectrum_settings(SpectrumSettings {
            display_bins: s.display_bins,
            release_seconds: s.release_seconds as f32,
            peak_decay_db_per_second: s.peak_decay_db_per_second as f32,
            tilt_db_per_octave: s.tilt_db_per_octave as f32,
            ..self.engine.spectrum_settings()
        });
    }

    /// The curves of the library again (after an import, or a file the user put into the folder).
    pub fn reload_library(&mut self) {
        self.names = CurveNames {
            headphones: self.library.all_curves().into_iter().map(|c| c.name).collect(),
            targets: self.library.all_targets().into_iter().map(|c| c.name).collect(),
        };
    }

    pub fn curve_names(&self) -> &CurveNames {
        &self.names
    }

    pub fn curves_folder(&self) -> &Path {
        self.library.user_curves_directory()
    }

    /// The target in use: the chosen one, or the first of the library.
    fn target_name(&self) -> Option<String> {
        let chosen = &self.settings.target_name;
        self.names
            .targets
            .iter()
            .find(|name| *name == chosen)
            .or(self.names.targets.first())
            .cloned()
    }

    /// The headphone model of the settings to the engine. Nothing happens while the choice is the same.
    fn apply_headphone(&mut self) {
        let name = self.settings.headphone_name.clone().unwrap_or_default();
        let target = self.target_name();
        let key = format!("{name}\u{1}{}", target.clone().unwrap_or_default());
        if self.applied_model.as_deref() == Some(key.as_str()) {
            return;
        }
        self.applied_model = Some(key);
        match self.library.curve_named(&name) {
            Some(curve) => {
                let target = target.and_then(|t| self.library.target_named(&t));
                self.headphone = Some(curve.name.clone());
                let model: SharedHeadphoneModel = Arc::new(Mutex::new(HeadphoneModel::new(curve, target)));
                self.engine.set_headphone_model(Some(model));
            }
            None => {
                self.headphone = None;
                self.engine.set_headphone_model(None);
            }
        }
        self.flags.clear();
    }

    /// Takes a curve file into the library and chooses it. An error text for the user when the file is no curve.
    pub fn import_curve(&mut self, path: &Path) -> Result<String, String> {
        match self.library.import_curve(path) {
            Ok(curve) => {
                self.reload_library();
                self.applied_model = None;
                self.settings.headphone_name = Some(curve.name.clone());
                self.apply_headphone();
                Ok(curve.name)
            }
            Err(_) => {
                let file = path
                    .file_name()
                    .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                Err(format!(
                    "Joseon could not read a frequency response from \u{201C}{file}\u{201D}. Use an AutoEq CSV file or a \
                     text file with two columns: frequency in Hz, level in dB."
                ))
            }
        }
    }

    /// The strip under the header, for the frame of now. `now` = a clock in seconds that only goes forward.
    pub fn stress(&mut self, now: f64) -> StressState {
        let frame = self.engine.latest_frame();
        let current: &[StressFlag] = frame.headphone.as_ref().map_or(&[], |h| &h.stress_flags);
        let flags = if self.headphone.is_some() {
            self.flags.update(current, now)
        } else {
            Vec::new()
        };
        let integrated = if frame.loudness.is_integrated_valid {
            frame.loudness.integrated_lufs
        } else {
            f32::NEG_INFINITY
        };
        StressState {
            headphone: self.headphone.clone(),
            flags,
            waiting: !self.is_demo && frame.is_silent,
            loudness: LoudnessDelta::new(integrated, self.settings.loudness_target()),
        }
    }

    /// Call it a few times per second: the stream facts of the source go to the engine.
    pub fn supervise(&mut self) {
        let stream = match &self.source {
            Some(Source::Loopback(source)) => source.stream_info(),
            Some(Source::Demo(source)) => source.stream_info(),
            None => None,
        };
        if stream != self.stream {
            // The song of a program that makes sound comes before the song of one that does not.
            self.now_playing
                .set_playing_programs(stream.as_ref().map_or(&[][..], |s| &s.active_sources));
            self.stream = stream.clone();
            self.engine.set_stream_info(stream);
        }
    }

    pub fn is_demo(&self) -> bool {
        self.is_demo
    }

    pub fn is_frozen(&self) -> bool {
        self.frozen.is_some()
    }

    /// Freezes the display on the frame of now, or lets it run again. The measurement goes on.
    pub fn toggle_freeze(&mut self) {
        self.frozen = match self.frozen {
            Some(_) => None,
            None => Some(self.engine.latest_frame()),
        };
    }

    pub fn reset_measurement(&self) {
        self.engine.reset_measurement();
    }

    /// The frame the panels show: the frozen one, or the newest.
    pub fn frame(&self) -> Arc<AnalysisFrame> {
        self.frozen.clone().unwrap_or_else(|| self.engine.latest_frame())
    }

    pub fn session(&self) -> SessionSnapshot {
        self.engine.session().snapshot()
    }

    pub fn clear_session(&self) {
        self.engine.session().clear();
    }

    fn health(&self) -> Option<CaptureHealth> {
        match &self.source {
            Some(Source::Loopback(source)) => Some(source.health()),
            _ => None,
        }
    }

    pub fn header(&self) -> HeaderState {
        let frame = self.engine.latest_frame();
        let state = if self.is_demo {
            SignalState::Demo
        } else if frame.is_silent {
            SignalState::Silent
        } else {
            SignalState::Live
        };
        let mut header = HeaderState::new(self.stream.as_ref(), state);
        if let Some(Source::Loopback(source)) = &self.source {
            header.volume = volume_text(source.compensation_db());
        }
        if state != SignalState::Demo {
            header.now_playing = self.now_playing.current().map_or(String::new(), |n| n.line());
        }
        header
    }

    /// The rows over the cards: why the capture did not start, or why there is silence.
    pub fn banners(&self) -> Vec<Banner> {
        let mut banners = Vec::new();
        if let Some(error) = &self.capture_error {
            banners.push(Banner {
                text: format!("Audio capture did not start: {error}. Joseon shows a demo signal."),
                is_error: true,
            });
        }
        if let Some(advice) = self.health().and_then(|health| health.advice()) {
            banners.push(Banner {
                text: advice,
                is_error: false,
            });
        }
        banners
    }

    /// The quiet text of the largest card while nothing is to draw. `None` while there is a picture.
    pub fn placeholder(&self) -> Option<&'static str> {
        (!self.is_demo && self.frozen.is_none() && is_at_rest(&self.engine.latest_frame())).then_some(WAITING_FOR_AUDIO)
    }
}

impl Drop for AppModel {
    fn drop(&mut self) {
        self.shutdown();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flag(id: &str, severity: joseon_core::contracts::StressSeverity, detail: &str) -> StressFlag {
        StressFlag {
            id: id.to_string(),
            severity,
            title: format!("Flag {id}"),
            detail: detail.to_string(),
            frequency_range_hz: None,
            plot_label: String::new(),
        }
    }

    #[test]
    fn flags_are_held_and_sorted() {
        use joseon_core::contracts::StressSeverity::{High, Watch};
        let mut holder = FlagHolder::default();
        let shown = holder.update(&[flag("b", Watch, "at -21.6 dBFS"), flag("a", High, "first")], 10.0);
        assert_eq!(
            shown.iter().map(|f| f.id.as_str()).collect::<Vec<_>>(),
            ["a", "b"],
            "highest severity first"
        );
        assert_eq!(shown[1].detail, "at \u{2212}21.6 dBFS");
        // The text of a held flag changes at most once per second.
        let shown = holder.update(&[flag("a", High, "second")], 10.5);
        assert_eq!(shown[0].detail, "first");
        let shown = holder.update(&[flag("a", High, "third")], 11.2);
        assert_eq!(shown[0].detail, "third");
        // "b" is gone from the frames: it stays 4 s after it was last seen.
        assert_eq!(shown.len(), 2);
        assert_eq!(holder.update(&[flag("a", High, "third")], 14.1).len(), 1);
        assert_eq!(holder.update(&[], 20.0).len(), 0);
    }

    #[test]
    fn the_loudness_is_read_against_the_target() {
        use crate::settings::LoudnessTarget::{Off, Streaming};
        assert_eq!(LoudnessDelta::new(-11.5, Off), None);
        let over = LoudnessDelta::new(-11.5, Streaming).unwrap();
        assert_eq!(
            (over.text.as_str(), over.relation),
            ("I \u{2212}11.5 \u{00B7} +2.5 LU over target", Relation::Over)
        );
        let under = LoudnessDelta::new(-20.0, Streaming).unwrap();
        assert_eq!(
            (under.text.as_str(), under.relation),
            ("I \u{2212}20.0 \u{00B7} \u{2212}6.0 LU under target", Relation::Under)
        );
        assert_eq!(
            LoudnessDelta::new(-14.3, Streaming).unwrap().text,
            "I \u{2212}14.3 \u{00B7} on target"
        );
        assert_eq!(LoudnessDelta::new(-120.0, Streaming).unwrap().relation, Relation::None);
    }

    #[test]
    fn the_headphone_model_follows_the_settings() {
        let folder = std::env::temp_dir().join(format!("joseon-model-test-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        let settings = AppSettings {
            demo_mode: true,
            ..AppSettings::default()
        };
        let mut model = AppModel::with_curves_folder(settings, folder.join("Curves"));
        // First run: the default pair.
        assert_eq!(model.settings.headphone_name.as_deref(), Some(DEFAULT_HEADPHONE_NAME));
        assert_eq!(model.settings.target_name, DEFAULT_TARGET_NAME);
        assert_eq!(model.curve_names().headphones.len(), 17);
        assert_eq!(model.curve_names().targets.len(), 4);
        model.start();
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while model.frame().headphone.is_none() {
            assert!(
                std::time::Instant::now() < end,
                "the frames carry the headphone reading"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert_eq!(
            model.frame().headphone.as_ref().unwrap().model_name,
            DEFAULT_HEADPHONE_NAME
        );
        let stress = model.stress(1.0);
        assert_eq!(stress.headphone.as_deref(), Some(DEFAULT_HEADPHONE_NAME));
        assert!(!stress.waiting && stress.loudness.is_none());
        // "None": no model, no reading.
        model.settings.headphone_name = Some(String::new());
        model.apply_settings();
        std::thread::sleep(std::time::Duration::from_millis(120));
        assert!(model.frame().headphone.is_none() && model.stress(2.0).headphone.is_none());
        // A curve file of the user: imported, chosen, and in the list from then on.
        std::fs::create_dir_all(&folder).unwrap();
        let file = folder.join("My IEM.csv");
        let rows: String = (0..40)
            .map(|i| format!("{},{}\n", 20.0 * 1.2f64.powi(i), (i % 7) as f64 - 3.0))
            .collect();
        std::fs::write(&file, format!("frequency,raw\n{rows}")).unwrap();
        let name = model.import_curve(&file).unwrap();
        assert_eq!(model.settings.headphone_name.as_deref(), Some(name.as_str()));
        assert!(model.curve_names().headphones.contains(&name));
        assert!(model.curves_folder().join("My IEM.csv").exists());
        std::fs::write(folder.join("notes.csv"), "no numbers here").unwrap();
        let error = model.import_curve(&folder.join("notes.csv")).unwrap_err();
        assert!(error.contains("notes.csv") && error.contains("two columns"));
        model.shutdown();
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn sample_rates_read_as_kilohertz() {
        assert_eq!(format_sample_rate(96_000.0), "96 kHz");
        assert_eq!(format_sample_rate(44_100.0), "44.1 kHz");
        assert_eq!(format_sample_rate(176_400.0), "176.4 kHz");
        assert_eq!(format_sample_rate(0.0), "");
    }

    #[test]
    fn the_volume_shows_when_it_is_taken_out() {
        assert_eq!(volume_text(17.2), "volume \u{2212}17 dB");
        assert_eq!(volume_text(0.2), "");
        assert_eq!(volume_text(f32::NAN), "");
        let mut header = HeaderState::new(Some(&StreamInfo::new(48_000.0, 2, "Headphones")), SignalState::Live);
        header.volume = volume_text(6.0);
        assert_eq!(header.facts(), ["Headphones", "48 kHz", "volume \u{2212}6 dB"]);
    }

    #[test]
    fn the_header_says_each_thing_once() {
        let stream = StreamInfo {
            bit_depth: Some(24),
            active_sources: vec!["Qobuz".to_string(), "chrome".to_string()],
            ..StreamInfo::new(96_000.0, 2, "Speakers (USB DAC)")
        };
        let live = HeaderState::new(Some(&stream), SignalState::Live);
        assert_eq!(live.headline(), "Qobuz, chrome");
        assert_eq!(live.facts(), ["Speakers (USB DAC)", "96 kHz", "24 bit"]);
        // At silence the phrase leads, and the programs move to the second line.
        let silent = HeaderState::new(Some(&stream), SignalState::Silent);
        assert_eq!(silent.headline(), WAITING_FOR_AUDIO);
        assert_eq!(
            silent.facts(),
            ["Qobuz, chrome", "Speakers (USB DAC)", "96 kHz", "24 bit"]
        );
        // Live with no program named: the phrase, not an empty line.
        let unnamed = HeaderState::new(Some(&StreamInfo::new(48_000.0, 2, "Headset")), SignalState::Live);
        assert_eq!(
            (unnamed.headline().as_str(), unnamed.facts()),
            (WAITING_FOR_AUDIO, vec!["Headset".to_string(), "48 kHz".to_string()])
        );
        // The demo names itself once.
        let demo_stream = StreamInfo {
            bit_depth: Some(32),
            active_sources: vec!["Joseon demo".to_string()],
            ..StreamInfo::new(48_000.0, 2, "Demo signal")
        };
        let demo = HeaderState::new(Some(&demo_stream), SignalState::Demo);
        assert_eq!(demo.headline(), "Demo signal");
        assert_eq!(demo.facts(), ["48 kHz", "32 bit"]);
        let none = HeaderState::new(None, SignalState::Silent);
        assert_eq!((none.headline().as_str(), none.facts().len()), (WAITING_FOR_AUDIO, 0));
    }

    #[test]
    fn the_demo_runs_through_the_engine_and_freezes() {
        let mut model = AppModel::new(AppSettings {
            demo_mode: true,
            display_bins: 512,
            ..AppSettings::default()
        });
        assert_eq!(model.placeholder(), Some(WAITING_FOR_AUDIO), "nothing runs yet");
        model.start();
        assert!(model.is_demo());
        let end = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while model.frame().is_silent {
            assert!(std::time::Instant::now() < end, "the demo gives frames");
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        model.supervise();
        assert_eq!(model.frame().spectrum.mid.len(), 512, "the settings reached the engine");
        assert_eq!(model.header().headline(), "Demo signal");
        assert_eq!(model.header().facts(), ["48 kHz", "32 bit"]);
        assert!(model.banners().is_empty() && model.placeholder().is_none());
        // Frozen: the same frame again and again.
        model.toggle_freeze();
        let held = model.frame();
        std::thread::sleep(std::time::Duration::from_millis(60));
        assert!(Arc::ptr_eq(&held, &model.frame()) && model.is_frozen());
        model.toggle_freeze();
        assert!(!Arc::ptr_eq(&held, &model.frame()));
        std::thread::sleep(std::time::Duration::from_millis(1200));
        assert!(!model.session().samples.is_empty(), "the session record fills");
        model.shutdown();
    }

    /// With no capture on this system the app says so and shows the demo.
    #[cfg(not(windows))]
    #[test]
    fn a_capture_that_does_not_start_gives_a_banner_and_the_demo() {
        let mut model = AppModel::new(AppSettings::default());
        model.start();
        assert!(model.is_demo());
        let banners = model.banners();
        assert_eq!(banners.len(), 1);
        assert!(banners[0].is_error && banners[0].text.starts_with("Audio capture did not start: "));
        assert!(banners[0].text.ends_with("Joseon shows a demo signal."));
    }
}
