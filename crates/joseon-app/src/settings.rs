//! All user settings, in one JSON file.
//!
//! The keys and the defaults are the ones of the macOS app. A value that is
//! not one of the choices (an edited file, an older build) falls back to its
//! default; the file is never a reason for the app not to start.
//!
//! Port source: `Sources/JoseonApp/AppSettings.swift`.

use joseon_render::contracts::SpectrumViewOptions;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Main window layout presets (Ctrl+1 / Ctrl+2 / Ctrl+3, and Ctrl+6 for the waterfall: Ctrl+4 and Ctrl+5 are the
/// placement and the timeline).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LayoutPreset {
    #[default]
    Essential,
    Spectrum,
    Metering,
    /// The spectrogram card alone, as the 3-D field, over the whole panel area.
    Waterfall,
}

impl LayoutPreset {
    pub const ALL: [LayoutPreset; 4] = [Self::Essential, Self::Spectrum, Self::Metering, Self::Waterfall];

    pub fn title(self) -> &'static str {
        match self {
            Self::Essential => "Essential",
            Self::Spectrum => "Spectrum",
            Self::Metering => "Metering",
            Self::Waterfall => "Waterfall",
        }
    }

    pub fn key(self) -> &'static str {
        match self {
            Self::Essential => "essential",
            Self::Spectrum => "spectrum",
            Self::Metering => "metering",
            Self::Waterfall => "waterfall",
        }
    }

    /// The key that chooses it, as a menu writes it.
    pub fn shortcut(self) -> &'static str {
        match self {
            Self::Essential => "Ctrl+1",
            Self::Spectrum => "Ctrl+2",
            Self::Metering => "Ctrl+3",
            Self::Waterfall => "Ctrl+6",
        }
    }
}

/// Color of the mini spectrum in the tray icon and in the mini strip.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MiniGraphColor {
    /// White: reads on the dark taskbar of Windows. (macOS: the color of the menu bar text.)
    #[default]
    Template,
    Orange,
    Accent,
}

impl MiniGraphColor {
    pub const ALL: [MiniGraphColor; 3] = [Self::Template, Self::Orange, Self::Accent];

    pub fn title(self) -> &'static str {
        match self {
            Self::Template => "White",
            Self::Orange => "Orange",
            Self::Accent => "Accent",
        }
    }

    pub fn rgba(self) -> [f32; 4] {
        match self {
            Self::Template => [1.0, 1.0, 1.0, 1.0],
            Self::Orange => [1.0, 0.58, 0.10, 1.0],
            Self::Accent => joseon_render::Theme::default().accent,
        }
    }
}

/// How smooth the pictures move. Presentation only: the analysis keeps its own rate in both modes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DisplayRefreshMode {
    /// About 60 pictures per second: less power.
    #[default]
    Balanced,
    /// A picture on every refresh of the display: smoother on a fast display, more power.
    HighRefresh,
}

impl DisplayRefreshMode {
    pub const ALL: [DisplayRefreshMode; 2] = [Self::Balanced, Self::HighRefresh];

    pub fn title(self) -> &'static str {
        match self {
            Self::Balanced => "Balanced",
            Self::HighRefresh => "High Refresh (every refresh of the display)",
        }
    }
}

/// Loudness normalization targets the user can compare the integrated loudness with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LoudnessTarget {
    #[default]
    Off,
    Streaming,
    Apple,
    Ebu,
}

impl LoudnessTarget {
    pub const ALL: [LoudnessTarget; 4] = [Self::Off, Self::Streaming, Self::Apple, Self::Ebu];

    pub fn from_raw(raw: i32) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.raw() == raw)
    }

    pub fn raw(self) -> i32 {
        match self {
            Self::Off => 0,
            Self::Streaming => -14,
            Self::Apple => -16,
            Self::Ebu => -23,
        }
    }

    pub fn lufs(self) -> Option<f32> {
        (self != Self::Off).then(|| self.raw() as f32)
    }

    /// Menu and settings title.
    pub fn title(self) -> String {
        let number = format!("\u{2212}{} LUFS", -self.raw());
        match self {
            Self::Off => "Off".to_string(),
            Self::Streaming => format!("{number} \u{00B7} Streaming"),
            Self::Apple => format!("{number} \u{00B7} Apple"),
            Self::Ebu => format!("{number} \u{00B7} EBU R128"),
        }
    }
}

pub const BIN_CHOICES: [usize; 3] = [512, 1024, 2048];
pub const TILT_CHOICES: [f64; 3] = [0.0, 3.0, 4.5];
pub const DB_RANGE_CHOICES: [i32; 3] = [60, 96, 120];
pub const HISTORY_CHOICES: [i32; 3] = [10, 20, 60];
/// Session timeline: seconds across the strip (5, 15 or 30 minutes).
pub const TIMELINE_WINDOW_CHOICES: [i64; 3] = [300, 900, 1800];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct AppSettings {
    // Analysis
    pub display_bins: usize,
    pub release_seconds: f64,
    #[serde(rename = "peakDecayDBPerSecond")]
    pub peak_decay_db_per_second: f64,
    #[serde(rename = "tiltDBPerOctave")]
    pub tilt_db_per_octave: f64,
    pub db_range: i32,

    // Spectrum panel traces
    pub show_left_right: bool,
    pub show_mid: bool,
    /// Side = (L−R)/2 as its own trace in the spectrum.
    pub show_side: bool,
    pub show_peak_hold: bool,
    /// Spectrum picks its own 72 dB window that follows the music. Off = the fixed "Level range".
    pub spectrum_auto_range: bool,
    /// Vectorscope card shows stereo placement by frequency (pan spectrum), not the Lissajous scope.
    pub stereo_placement_mode: bool,
    /// "Spectrum" layout: the stereo placement card sits beside the spectrogram.
    pub spectrum_shows_placement: bool,
    pub show_average: bool,
    pub show_headphone_overlay: bool,
    pub spectrogram_history_seconds: i32,
    /// The spectrogram card shows the 3-D waterfall instead of the flat picture. The "Waterfall" layout forces it on.
    pub spectrogram_waterfall: bool,

    // Session timeline strip
    /// The strip under the panels of the main window (Ctrl+5).
    pub show_timeline: bool,
    pub timeline_window_seconds: i64,
    /// The tone lane of the timeline. The panel shows it only when it is 260 pt high or more.
    pub timeline_show_bands: bool,
    /// The "At the ear" lane of the timeline. Off by default: the default screen stays calm.
    pub timeline_show_ear_lane: bool,

    // A/B compare
    pub comparison_level_match: bool,
    pub comparison_headphone_mode: bool,

    // App
    /// Display → Animation: Balanced (about 60 pictures per second) or High Refresh.
    pub display_refresh_mode: DisplayRefreshMode,
    pub layout_preset: LayoutPreset,
    pub mini_graph_color: MiniGraphColor,
    /// The mini strip: a small window that stays over the other windows (new in this port).
    pub show_mini_strip: bool,
    /// Where the user put the mini strip, in screen points. `None` = where the system puts it.
    pub mini_strip_position: Option<[f64; 2]>,
    pub demo_mode: bool,
    /// `None` = the user never chose (the app then picks its default headphone). `Some("")` = "None".
    pub headphone_name: Option<String>,
    /// Empty = the first target in the library.
    pub target_name: String,
    /// LUFS of the loudness target, 0 = off.
    pub loudness_target: i32,
    /// Reset the measurement when audio resumes after more than 2 s of silence (a new track or album).
    pub auto_reset_after_silence: bool,
    pub has_seen_welcome: bool,
    pub main_window_was_open: bool,
    /// Show the title and the artist of the song that plays, as the player tells Windows.
    pub show_now_playing: bool,
    /// Ask the release list for a newer Joseon after the start and once a day.
    pub check_updates_automatically: bool,
    /// Take the Windows volume out of the measurement (new in this port: Windows captures after its volume
    /// control, macOS before). Off = the numbers follow the volume slider.
    pub compensate_system_volume: bool,
    /// Do that also for a device that says it sets the volume itself. For the devices where the capture is
    /// quieter at a lower volume all the same.
    pub compensate_volume_always: bool,

    // Window (new in this port: macOS keeps these itself)
    /// Size of the main window in points.
    pub window_size: [f64; 2],
    /// Height the user gave the timeline strip, in points.
    pub timeline_strip_height: f64,
    /// The strip under the "Waterfall" layout: squashed by default (loudness and peak only), with its own height.
    pub waterfall_strip_height: f64,
    /// The shares of the panes of each split the user dragged, by the key of the split.
    pub splits: BTreeMap<String, Vec<f64>>,
}

impl Default for AppSettings {
    fn default() -> Self {
        Self {
            display_bins: 1024,
            release_seconds: 0.25,
            peak_decay_db_per_second: 12.0,
            tilt_db_per_octave: 0.0,
            db_range: 96,
            show_left_right: true,
            show_mid: true,
            show_side: false,
            show_peak_hold: true,
            spectrum_auto_range: true,
            stereo_placement_mode: false,
            spectrum_shows_placement: true,
            show_average: true,
            show_headphone_overlay: true,
            spectrogram_history_seconds: 20,
            spectrogram_waterfall: false,
            show_timeline: true,
            timeline_window_seconds: 900,
            timeline_show_bands: true,
            timeline_show_ear_lane: false,
            comparison_level_match: true,
            comparison_headphone_mode: false,
            display_refresh_mode: DisplayRefreshMode::Balanced,
            layout_preset: LayoutPreset::Essential,
            mini_graph_color: MiniGraphColor::Template,
            show_mini_strip: false,
            mini_strip_position: None,
            demo_mode: false,
            headphone_name: None,
            target_name: String::new(),
            loudness_target: 0,
            auto_reset_after_silence: true,
            has_seen_welcome: false,
            main_window_was_open: true,
            show_now_playing: true,
            check_updates_automatically: true,
            compensate_system_volume: true,
            compensate_volume_always: false,
            window_size: [1280.0, 800.0],
            timeline_strip_height: 140.0,
            waterfall_strip_height: 100.0,
            splits: BTreeMap::new(),
        }
    }
}

fn pick<T: PartialEq + Copy>(value: T, choices: &[T], fallback: T) -> T {
    if choices.contains(&value) {
        value
    } else {
        fallback
    }
}

impl AppSettings {
    /// The settings with every value inside its choices and limits.
    pub fn validated(mut self) -> Self {
        let d = Self::default();
        let finite = |v: f64, fallback: f64| if v.is_finite() { v } else { fallback };
        self.display_bins = pick(self.display_bins, &BIN_CHOICES, d.display_bins);
        self.release_seconds = finite(self.release_seconds, d.release_seconds).max(0.05).min(2.0);
        self.peak_decay_db_per_second = finite(self.peak_decay_db_per_second, d.peak_decay_db_per_second)
            .max(3.0)
            .min(48.0);
        self.tilt_db_per_octave = pick(self.tilt_db_per_octave, &TILT_CHOICES, 0.0);
        self.db_range = pick(self.db_range, &DB_RANGE_CHOICES, d.db_range);
        self.spectrogram_history_seconds = pick(
            self.spectrogram_history_seconds,
            &HISTORY_CHOICES,
            d.spectrogram_history_seconds,
        );
        self.timeline_window_seconds = pick(
            self.timeline_window_seconds,
            &TIMELINE_WINDOW_CHOICES,
            d.timeline_window_seconds,
        );
        if LoudnessTarget::from_raw(self.loudness_target).is_none() {
            self.loudness_target = 0;
        }
        let [w, h] = self.window_size;
        self.window_size =
            if w.is_finite() && h.is_finite() && w >= 900.0 && h >= 560.0 && w <= 16_384.0 && h <= 16_384.0 {
                [w, h]
            } else {
                d.window_size
            };
        if !(100.0..=300.0).contains(&self.timeline_strip_height) {
            self.timeline_strip_height = d.timeline_strip_height;
        }
        if !(100.0..=300.0).contains(&self.waterfall_strip_height) {
            self.waterfall_strip_height = d.waterfall_strip_height;
        }
        if self
            .mini_strip_position
            .is_some_and(|p| !p[0].is_finite() || !p[1].is_finite())
        {
            self.mini_strip_position = None;
        }
        self.splits
            .retain(|_, shares| shares.len() >= 2 && shares.iter().all(|s| *s > 0.02 && *s < 0.98));
        self
    }

    pub fn loudness_target(&self) -> LoudnessTarget {
        LoudnessTarget::from_raw(self.loudness_target).unwrap_or_default()
    }

    /// "Level range" as one choice: 0 = Auto (the spectrum follows the music), else the fixed range in dB.
    pub fn level_range_choice(&self) -> i32 {
        if self.spectrum_auto_range {
            0
        } else {
            self.db_range
        }
    }

    pub fn set_level_range_choice(&mut self, choice: i32) {
        if choice == 0 {
            self.spectrum_auto_range = true;
        } else {
            self.db_range = pick(choice, &DB_RANGE_CHOICES, self.db_range);
            self.spectrum_auto_range = false;
        }
    }

    /// Options for the main spectrum panel, from the stored toggles.
    pub fn spectrum_view_options(&self) -> SpectrumViewOptions {
        SpectrumViewOptions {
            show_left_right: self.show_left_right,
            show_mid: self.show_mid,
            show_side: self.show_side,
            show_peak_hold: self.show_peak_hold,
            show_average: self.show_average,
            show_headphone_overlay: self.show_headphone_overlay,
            min_db: -(self.db_range as f32),
            max_db: 0.0,
        }
    }

    /// Reads the text of a settings file. Text that is not a settings file gives the defaults.
    pub fn from_json(text: &str) -> Self {
        serde_json::from_str::<AppSettings>(text)
            .unwrap_or_default()
            .validated()
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_else(|_| "{}".to_string())
    }

    /// The settings of a file. No file, or a file that cannot be read: the defaults.
    pub fn load(path: &Path) -> Self {
        std::fs::read_to_string(path).map_or_else(|_| Self::default(), |text| Self::from_json(&text))
    }

    /// Writes the file through a temporary one, so a crash never leaves half a file.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        if let Some(folder) = path.parent() {
            std::fs::create_dir_all(folder)?;
        }
        let temporary = path.with_extension("json.new");
        std::fs::write(&temporary, self.to_json())?;
        std::fs::rename(&temporary, path)
    }
}

/// Where the settings file is: `%APPDATA%\Joseon\settings.json` on Windows.
/// `JOSEON_SETTINGS` names another file (tests, a second profile).
pub fn settings_path() -> PathBuf {
    if let Some(path) = std::env::var_os("JOSEON_SETTINGS") {
        return PathBuf::from(path);
    }
    let folder = if cfg!(windows) {
        std::env::var_os("APPDATA").map(PathBuf::from)
    } else if cfg!(target_os = "macos") {
        std::env::var_os("HOME").map(|home| PathBuf::from(home).join("Library/Application Support"))
    } else {
        std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))
    };
    folder
        .unwrap_or_else(std::env::temp_dir)
        .join(if cfg!(windows) { "Joseon" } else { "JoseonPC" })
        .join("settings.json")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_the_ones_of_the_macos_app() {
        let s = AppSettings::default();
        assert_eq!(
            (
                s.display_bins,
                s.release_seconds,
                s.peak_decay_db_per_second,
                s.db_range
            ),
            (1024, 0.25, 12.0, 96)
        );
        assert!(s.show_left_right && s.show_mid && !s.show_side && s.show_peak_hold && s.show_average);
        assert!(s.spectrum_auto_range && !s.stereo_placement_mode && s.spectrum_shows_placement);
        assert_eq!((s.spectrogram_history_seconds, s.timeline_window_seconds), (20, 900));
        assert!(s.show_timeline && s.timeline_show_bands && !s.timeline_show_ear_lane);
        assert_eq!(
            (s.layout_preset, s.loudness_target(), s.headphone_name.clone()),
            (LayoutPreset::Essential, LoudnessTarget::Off, None)
        );
        assert!(s.auto_reset_after_silence && s.main_window_was_open && !s.demo_mode);
        assert_eq!(s.clone().validated(), s);
        let o = s.spectrum_view_options();
        assert_eq!((o.min_db, o.max_db, o.show_side), (-96.0, 0.0, false));
    }

    #[test]
    fn the_file_keeps_every_setting() {
        let mut s = AppSettings {
            display_bins: 2048,
            tilt_db_per_octave: 4.5,
            layout_preset: LayoutPreset::Metering,
            loudness_target: -16,
            headphone_name: Some("HD 600".to_string()),
            window_size: [1500.0, 900.0],
            ..AppSettings::default()
        };
        s.splits.insert("essential.main".to_string(), vec![0.6, 0.4]);
        let text = s.to_json();
        // The keys are the keys of the macOS app.
        for key in [
            "\"displayBins\": 2048",
            "\"tiltDBPerOctave\": 4.5",
            "\"layoutPreset\": \"metering\"",
            "\"peakDecayDBPerSecond\"",
        ] {
            assert!(text.contains(key), "{key} in {text}");
        }
        assert_eq!(AppSettings::from_json(&text), s);

        let folder = std::env::temp_dir().join(format!("joseon-settings-test-{}", std::process::id()));
        let path = folder.join("deep").join("settings.json");
        s.save(&path).unwrap();
        assert_eq!(AppSettings::load(&path), s);
        assert!(!path.with_extension("json.new").exists());
        assert_eq!(AppSettings::load(&folder.join("none.json")), AppSettings::default());
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn values_outside_the_choices_fall_back() {
        let text = r#"{ "displayBins": 999, "releaseSeconds": 50, "peakDecayDBPerSecond": 1, "tiltDBPerOctave": 2,
            "dbRange": 70, "spectrogramHistorySeconds": 33, "timelineWindowSeconds": 5, "loudnessTarget": -9,
            "windowSize": [100, 100], "timelineStripHeight": 900, "showSide": true, "futureKey": 1,
            "splits": { "ok": [0.5, 0.5], "bad": [0.99, 0.01], "short": [1.0] } }"#;
        let s = AppSettings::from_json(text);
        assert_eq!(
            (s.display_bins, s.release_seconds, s.peak_decay_db_per_second),
            (1024, 2.0, 3.0)
        );
        assert_eq!(
            (s.tilt_db_per_octave, s.db_range, s.spectrogram_history_seconds),
            (0.0, 96, 20)
        );
        assert_eq!(
            (s.timeline_window_seconds, s.loudness_target, s.window_size),
            (900, 0, [1280.0, 800.0])
        );
        assert_eq!(s.timeline_strip_height, 140.0);
        assert!(s.show_side, "a good value next to bad ones is kept");
        assert_eq!(s.splits.keys().collect::<Vec<_>>(), ["ok"]);
        // Not a settings file at all.
        assert_eq!(AppSettings::from_json("not json"), AppSettings::default());
        assert_eq!(
            AppSettings::from_json("{ \"displayBins\": \"many\" }"),
            AppSettings::default()
        );
    }

    #[test]
    fn the_level_range_is_one_choice() {
        let mut s = AppSettings::default();
        assert_eq!(s.level_range_choice(), 0);
        s.set_level_range_choice(120);
        assert_eq!(
            (s.level_range_choice(), s.spectrum_auto_range, s.db_range),
            (120, false, 120)
        );
        s.set_level_range_choice(77);
        assert_eq!(s.db_range, 120, "not a choice: the range stays");
        s.set_level_range_choice(0);
        assert!(s.spectrum_auto_range);
        assert_eq!(LoudnessTarget::Streaming.title(), "\u{2212}14 LUFS \u{00B7} Streaming");
        assert_eq!(
            (LoudnessTarget::Ebu.lufs(), LoudnessTarget::Off.lufs()),
            (Some(-23.0), None)
        );
        assert_eq!(LoudnessTarget::from_raw(-16), Some(LoudnessTarget::Apple));
        assert_eq!(LayoutPreset::Spectrum.title(), "Spectrum");
    }
}
