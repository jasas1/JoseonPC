//! The menus of the window, as plain data: what each menu holds for the
//! settings of now, and what a chosen item does. The window turns the data
//! into a native menu (`main.rs`); the tests need no window.
//!
//! Every card has an options menu (its "..." button, or a right click). The
//! header has a settings menu: on Windows it takes the place of the settings
//! window of the macOS app, because every setting is a choice or a switch.
//!
//! Port source: `configureMenus` and the menu builders of `MainWindowController.swift`,
//! and the sections of `SettingsView.swift`.

use crate::layout::Card;
use crate::model::CurveNames;
use crate::settings::{
    AppSettings, DisplayRefreshMode, LayoutPreset, LoudnessTarget, MiniGraphColor, BIN_CHOICES, DB_RANGE_CHOICES,
    HISTORY_CHOICES, TILT_CHOICES, TIMELINE_WINDOW_CHOICES,
};
use crate::update::Status;

/// What a menu item does.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Command {
    ToggleLeftRight,
    ToggleMid,
    ToggleSide,
    TogglePeakHold,
    ToggleAverage,
    ToggleHeadphoneOverlay,
    Tilt(f64),
    /// 0 = auto, else the fixed range in dB.
    LevelRange(i32),
    HistorySeconds(i32),
    /// The spectrogram card as the flat picture (false) or the 3-D waterfall (true).
    SpectrogramWaterfall(bool),
    /// The waterfall back to its resting view.
    ResetWaterfallView,
    ToggleSpectrumPlacement,
    StereoPlacement(bool),
    TimelineWindow(i64),
    ToggleToneLane,
    ToggleEarLane,
    ClearTimeline,
    ResetMeasurement,
    LoudnessTarget(LoudnessTarget),
    ToggleFreeze,
    // The settings menu.
    Preset(LayoutPreset),
    ToggleTimeline,
    DisplayBins(usize),
    ReleaseSeconds(f64),
    PeakDecay(f64),
    ToggleAutoReset,
    ToggleVolumeCompensation,
    ToggleVolumeAlways,
    CheckForUpdates,
    /// Fetch the release that is there, put it in place, start it.
    InstallUpdate,
    ToggleAutoUpdate,
    ToggleNowPlaying,
    ToggleDemo,
    /// Display → Animation.
    RefreshMode(DisplayRefreshMode),
    /// The headphone at this place of the list. `None` = no headphone.
    Headphone(Option<u16>),
    /// The target at this place of the list.
    Target(u16),
    ImportCurve,
    OpenCurvesFolder,
    // The tray menu.
    OpenWindow,
    ToggleMiniStrip,
    MiniColor(MiniGraphColor),
    Quit,
}

/// What the owner has to do after a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Effect {
    /// The settings changed: store them, give them to the panels and the engine.
    Settings,
    /// The settings changed, and the audio source has to start again (demo on or off).
    SettingsAndSource,
    ToggleFreeze,
    ResetMeasurement,
    ClearTimeline,
    OpenWindow,
    Quit,
    /// Ask the user for a curve file and take it in.
    ImportCurve,
    /// Show the folder of the user's curve files.
    OpenCurvesFolder,
    CheckUpdates,
    InstallUpdate,
    /// The waterfall back to its resting view: no setting changes.
    ResetWaterfallView,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Entry {
    /// A grey title over a group of items.
    Header(&'static str),
    Separator,
    /// A line that only says something.
    Note(String),
    Item {
        title: String,
        /// `Some` = the item has a check mark place, and this is its state.
        checked: Option<bool>,
        command: Command,
    },
    /// A sub-menu.
    Group {
        title: &'static str,
        entries: Vec<Entry>,
    },
}

fn item(title: impl Into<String>, command: Command) -> Entry {
    Entry::Item {
        title: title.into(),
        checked: None,
        command,
    }
}

fn check(title: impl Into<String>, on: bool, command: Command) -> Entry {
    Entry::Item {
        title: title.into(),
        checked: Some(on),
        command,
    }
}

pub fn tilt_title(tilt: f64) -> String {
    if tilt == 0.0 {
        "0 dB per octave (raw)".to_string()
    } else if tilt == tilt.round() {
        format!("{} dB per octave", tilt as i64)
    } else {
        format!("{tilt} dB per octave")
    }
}

fn tilt_entries(s: &AppSettings) -> Vec<Entry> {
    TILT_CHOICES
        .into_iter()
        .map(|tilt| check(tilt_title(tilt), s.tilt_db_per_octave == tilt, Command::Tilt(tilt)))
        .collect()
}

fn range_entries(s: &AppSettings) -> Vec<Entry> {
    let mut out = vec![check(
        "Auto (follows the music)",
        s.spectrum_auto_range,
        Command::LevelRange(0),
    )];
    for range in DB_RANGE_CHOICES {
        let on = !s.spectrum_auto_range && s.db_range == range;
        out.push(check(format!("{range} dB"), on, Command::LevelRange(range)));
    }
    out
}

fn loudness_entries(s: &AppSettings) -> Vec<Entry> {
    LoudnessTarget::ALL
        .into_iter()
        .map(|target| {
            check(
                target.title(),
                s.loudness_target() == target,
                Command::LoudnessTarget(target),
            )
        })
        .collect()
}

/// The options menu of a card.
/// It holds only what belongs to this card: "Freeze Display" stops every card, so it stands in the settings
/// menu (and on the space bar).
pub fn card_menu(card: Card, s: &AppSettings) -> Vec<Entry> {
    let mut menu = Vec::new();
    match card {
        Card::Spectrum => {
            menu.push(Entry::Header("Traces"));
            menu.push(check("Left and Right", s.show_left_right, Command::ToggleLeftRight));
            menu.push(check("Mid", s.show_mid, Command::ToggleMid));
            menu.push(check("Side (L\u{2212}R)", s.show_side, Command::ToggleSide));
            menu.push(check("Peak Hold", s.show_peak_hold, Command::TogglePeakHold));
            menu.push(check("Average", s.show_average, Command::ToggleAverage));
            menu.push(check(
                "Headphone Overlay",
                s.show_headphone_overlay,
                Command::ToggleHeadphoneOverlay,
            ));
            menu.push(Entry::Separator);
            menu.push(Entry::Header("Tilt"));
            menu.extend(tilt_entries(s));
            menu.push(Entry::Separator);
            menu.push(Entry::Header("Range"));
            menu.extend(range_entries(s));
            menu.push(Entry::Separator);
        }
        Card::Spectrogram => {
            let forced = s.layout_preset == LayoutPreset::Waterfall;
            menu.push(Entry::Header("View"));
            menu.push(check(
                "Flat",
                !forced && !s.spectrogram_waterfall,
                Command::SpectrogramWaterfall(false),
            ));
            menu.push(check(
                "3-D Waterfall",
                forced || s.spectrogram_waterfall,
                Command::SpectrogramWaterfall(true),
            ));
            menu.push(check(
                "Waterfall Over the Whole Window\tCtrl+6",
                forced,
                Command::Preset(if forced {
                    LayoutPreset::Essential
                } else {
                    LayoutPreset::Waterfall
                }),
            ));
            if forced || s.spectrogram_waterfall {
                menu.push(item("Reset View", Command::ResetWaterfallView));
            }
            menu.push(Entry::Separator);
            menu.push(Entry::Header("History"));
            for seconds in HISTORY_CHOICES {
                let on = s.spectrogram_history_seconds == seconds;
                menu.push(check(
                    format!("{seconds} seconds"),
                    on,
                    Command::HistorySeconds(seconds),
                ));
            }
            menu.push(Entry::Separator);
            if s.layout_preset == LayoutPreset::Spectrum {
                menu.push(check(
                    "Stereo Placement Beside the Spectrogram\tCtrl+4",
                    s.spectrum_shows_placement,
                    Command::ToggleSpectrumPlacement,
                ));
            }
        }
        Card::Placement => {
            menu.push(item("Hide Stereo Placement\tCtrl+4", Command::ToggleSpectrumPlacement));
            menu.push(Entry::Separator);
        }
        Card::Vectorscope => {
            menu.push(Entry::Header("View"));
            menu.push(check(
                "Vectorscope",
                !s.stereo_placement_mode,
                Command::StereoPlacement(false),
            ));
            menu.push(check(
                "Stereo Placement (by frequency)\tCtrl+4",
                s.stereo_placement_mode,
                Command::StereoPlacement(true),
            ));
            menu.push(Entry::Separator);
        }
        Card::Timeline => {
            menu.push(Entry::Header("Window"));
            for seconds in TIMELINE_WINDOW_CHOICES {
                let on = s.timeline_window_seconds == seconds;
                menu.push(check(
                    format!("{} minutes", seconds / 60),
                    on,
                    Command::TimelineWindow(seconds),
                ));
            }
            menu.push(Entry::Separator);
            menu.push(check("Show Tone Lane", s.timeline_show_bands, Command::ToggleToneLane));
            menu.push(check(
                "Show Level at the Ear Lane",
                s.timeline_show_ear_lane,
                Command::ToggleEarLane,
            ));
            menu.push(Entry::Separator);
            menu.push(item("Clear Timeline", Command::ClearTimeline));
            menu.push(Entry::Separator);
        }
        Card::Meters => {
            menu.push(item("Reset Measurement\tR", Command::ResetMeasurement));
            menu.push(Entry::Separator);
            menu.push(Entry::Header("Loudness Target"));
            menu.extend(loudness_entries(s));
            menu.push(Entry::Separator);
        }
    }
    while matches!(menu.last(), Some(Entry::Separator)) {
        menu.pop();
    }
    menu
}

/// The headphones of the library, "None" first, then the ways to add one.
pub fn headphone_menu(s: &AppSettings, names: &CurveNames) -> Vec<Entry> {
    let chosen = s.headphone_name.as_deref().unwrap_or("");
    let known = names.headphones.iter().any(|n| n == chosen);
    let mut menu = vec![check("None", !known, Command::Headphone(None)), Entry::Separator];
    for (i, name) in names.headphones.iter().enumerate() {
        menu.push(check(name.clone(), name == chosen, Command::Headphone(Some(i as u16))));
    }
    menu.push(Entry::Separator);
    menu.push(item("Import Curve\u{2026}", Command::ImportCurve));
    menu.push(item("Open the Folder of Your Curves", Command::OpenCurvesFolder));
    menu
}

/// The targets of the library. The first one is in use while none is chosen.
pub fn target_entries(s: &AppSettings, names: &CurveNames) -> Vec<Entry> {
    let chosen = names.targets.iter().position(|n| *n == s.target_name).unwrap_or(0);
    names
        .targets
        .iter()
        .enumerate()
        .map(|(i, name)| check(name.clone(), i == chosen, Command::Target(i as u16)))
        .collect()
}

/// The update part of the settings menu: where the updater stands, and what the user can do now.
pub fn update_entries(s: &AppSettings, status: &Status) -> Vec<Entry> {
    let mut menu = vec![Entry::Separator, Entry::Header("Updates")];
    if let Some(line) = status.line() {
        menu.push(Entry::Note(line));
    }
    match status {
        Status::Available(release) => menu.push(item(
            format!("Update to {} and Relaunch", release.version),
            Command::InstallUpdate,
        )),
        status if status.is_busy() => {}
        _ => menu.push(item("Check for Updates\u{2026}", Command::CheckForUpdates)),
    }
    menu.push(check(
        "Check for Updates Automatically",
        s.check_updates_automatically,
        Command::ToggleAutoUpdate,
    ));
    menu
}

pub const RELEASE_CHOICES: [f64; 4] = [0.1, 0.25, 0.5, 1.0];
pub const PEAK_DECAY_CHOICES: [f64; 4] = [6.0, 12.0, 24.0, 48.0];

/// The settings menu of the header.
pub fn settings_menu(s: &AppSettings, frozen: bool, names: &CurveNames) -> Vec<Entry> {
    let mut menu = vec![Entry::Header("Layout")];
    for preset in LayoutPreset::ALL {
        let title = format!("{}\t{}", preset.title(), preset.shortcut());
        menu.push(check(title, s.layout_preset == preset, Command::Preset(preset)));
    }
    menu.push(check(
        "Session Timeline\tCtrl+5",
        s.show_timeline,
        Command::ToggleTimeline,
    ));
    menu.push(Entry::Separator);
    let pick = |title: &'static str, entries: Vec<Entry>| Entry::Group { title, entries };
    menu.push(Entry::Header("Headphones"));
    menu.push(pick("Headphone", headphone_menu(s, names)));
    menu.push(pick("Target", target_entries(s, names)));
    menu.push(Entry::Separator);
    menu.push(Entry::Header("Spectrum"));
    menu.push(pick(
        "Resolution",
        BIN_CHOICES
            .into_iter()
            .map(|bins| {
                check(
                    format!("{bins} points"),
                    s.display_bins == bins,
                    Command::DisplayBins(bins),
                )
            })
            .collect(),
    ));
    menu.push(pick(
        "Release Time",
        RELEASE_CHOICES
            .into_iter()
            .map(|t| check(format!("{t} s"), s.release_seconds == t, Command::ReleaseSeconds(t)))
            .collect(),
    ));
    menu.push(pick(
        "Peak Decay",
        PEAK_DECAY_CHOICES
            .into_iter()
            .map(|d| {
                check(
                    format!("{d} dB per second"),
                    s.peak_decay_db_per_second == d,
                    Command::PeakDecay(d),
                )
            })
            .collect(),
    ));
    menu.push(pick("Tilt", tilt_entries(s)));
    menu.push(pick("Level Range", range_entries(s)));
    menu.push(Entry::Separator);
    menu.push(Entry::Header("Loudness"));
    menu.push(pick("Loudness Target", loudness_entries(s)));
    menu.push(check(
        "Reset When Audio Resumes After 2 s of Silence",
        s.auto_reset_after_silence,
        Command::ToggleAutoReset,
    ));
    menu.push(check(
        "Take the Windows Volume Out of the Numbers",
        s.compensate_system_volume,
        Command::ToggleVolumeCompensation,
    ));
    menu.push(check(
        "Also When the Device Has Its Own Volume Control",
        s.compensate_system_volume && s.compensate_volume_always,
        Command::ToggleVolumeAlways,
    ));
    menu.push(Entry::Separator);
    menu.push(Entry::Header("Display"));
    let mut animation: Vec<Entry> = DisplayRefreshMode::ALL
        .into_iter()
        .map(|mode| check(mode.title(), s.display_refresh_mode == mode, Command::RefreshMode(mode)))
        .collect();
    animation.push(Entry::Note(
        "Balanced uses less power. High Refresh moves more smoothly on a fast display and uses more power.".into(),
    ));
    menu.push(pick("Animation", animation));
    menu.push(Entry::Separator);
    menu.push(item("Reset Measurement\tR", Command::ResetMeasurement));
    menu.push(check("Freeze Display\tSpace", frozen, Command::ToggleFreeze));
    menu.push(Entry::Separator);
    menu.push(check(
        "Show the Name of the Song",
        s.show_now_playing,
        Command::ToggleNowPlaying,
    ));
    menu.push(check("Demo Signal", s.demo_mode, Command::ToggleDemo));
    menu
}

/// The menu of the tray icon and of the mini strip.
pub fn tray_menu(s: &AppSettings, frozen: bool) -> Vec<Entry> {
    vec![
        item("Open Joseon", Command::OpenWindow),
        Entry::Separator,
        check("Mini Spectrum Strip", s.show_mini_strip, Command::ToggleMiniStrip),
        Entry::Group {
            title: "Mini Spectrum Color",
            entries: MiniGraphColor::ALL
                .into_iter()
                .map(|color| check(color.title(), s.mini_graph_color == color, Command::MiniColor(color)))
                .collect(),
        },
        check(
            "Show the Name of the Song",
            s.show_now_playing,
            Command::ToggleNowPlaying,
        ),
        Entry::Separator,
        item("Reset Measurement", Command::ResetMeasurement),
        check("Freeze Display", frozen, Command::ToggleFreeze),
        Entry::Separator,
        item("Quit Joseon", Command::Quit),
    ]
}

/// Does what a menu item says. Settings change in place.
pub fn apply(s: &mut AppSettings, command: Command, names: &CurveNames) -> Effect {
    use Command::*;
    match command {
        ToggleLeftRight => s.show_left_right = !s.show_left_right,
        ToggleMid => s.show_mid = !s.show_mid,
        ToggleSide => s.show_side = !s.show_side,
        TogglePeakHold => s.show_peak_hold = !s.show_peak_hold,
        ToggleAverage => s.show_average = !s.show_average,
        ToggleHeadphoneOverlay => s.show_headphone_overlay = !s.show_headphone_overlay,
        Tilt(tilt) => s.tilt_db_per_octave = tilt,
        LevelRange(choice) => s.set_level_range_choice(choice),
        HistorySeconds(seconds) => s.spectrogram_history_seconds = seconds,
        SpectrogramWaterfall(on) => s.spectrogram_waterfall = on,
        ResetWaterfallView => return Effect::ResetWaterfallView,
        ToggleSpectrumPlacement => s.spectrum_shows_placement = !s.spectrum_shows_placement,
        StereoPlacement(on) => s.stereo_placement_mode = on,
        TimelineWindow(seconds) => s.timeline_window_seconds = seconds,
        ToggleToneLane => s.timeline_show_bands = !s.timeline_show_bands,
        ToggleEarLane => s.timeline_show_ear_lane = !s.timeline_show_ear_lane,
        LoudnessTarget(target) => s.loudness_target = target.raw(),
        Preset(preset) => s.layout_preset = preset,
        ToggleTimeline => s.show_timeline = !s.show_timeline,
        DisplayBins(bins) => s.display_bins = bins,
        ReleaseSeconds(t) => s.release_seconds = t,
        PeakDecay(d) => s.peak_decay_db_per_second = d,
        ToggleAutoReset => s.auto_reset_after_silence = !s.auto_reset_after_silence,
        ToggleVolumeCompensation => s.compensate_system_volume = !s.compensate_system_volume,
        ToggleVolumeAlways => {
            s.compensate_volume_always = !(s.compensate_system_volume && s.compensate_volume_always);
            s.compensate_system_volume = true;
        }
        ToggleNowPlaying => s.show_now_playing = !s.show_now_playing,
        RefreshMode(mode) => s.display_refresh_mode = mode,
        ToggleMiniStrip => s.show_mini_strip = !s.show_mini_strip,
        MiniColor(color) => s.mini_graph_color = color,
        // "None" is a choice too: the empty name, not "never chosen".
        Headphone(None) => s.headphone_name = Some(String::new()),
        Headphone(Some(i)) => {
            if let Some(name) = names.headphones.get(i as usize) {
                s.headphone_name = Some(name.clone());
            }
        }
        Target(i) => {
            if let Some(name) = names.targets.get(i as usize) {
                s.target_name = name.clone();
            }
        }
        CheckForUpdates => return Effect::CheckUpdates,
        InstallUpdate => return Effect::InstallUpdate,
        ToggleAutoUpdate => s.check_updates_automatically = !s.check_updates_automatically,
        ImportCurve => return Effect::ImportCurve,
        OpenCurvesFolder => return Effect::OpenCurvesFolder,
        OpenWindow => return Effect::OpenWindow,
        Quit => return Effect::Quit,
        ToggleDemo => {
            s.demo_mode = !s.demo_mode;
            *s = s.clone().validated();
            return Effect::SettingsAndSource;
        }
        ClearTimeline => return Effect::ClearTimeline,
        ResetMeasurement => return Effect::ResetMeasurement,
        ToggleFreeze => return Effect::ToggleFreeze,
    }
    // A value that is not a choice never reaches the file.
    *s = s.clone().validated();
    Effect::Settings
}

/// Every item of a menu with the command it carries, sub-menus included, in order.
pub fn commands(menu: &[Entry]) -> Vec<Command> {
    let mut out = Vec::new();
    for entry in menu {
        match entry {
            Entry::Item { command, .. } => out.push(*command),
            Entry::Group { entries, .. } => out.extend(commands(entries)),
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names() -> CurveNames {
        CurveNames {
            headphones: vec!["Alpha".to_string(), "Beta IEM".to_string()],
            targets: vec!["Harman over-ear 2018".to_string(), "Flat (zero)".to_string()],
        }
    }

    fn run(s: &mut AppSettings, command: Command) -> Effect {
        apply(s, command, &names())
    }

    #[test]
    fn the_headphone_menu_lists_the_library() {
        let mut s = AppSettings::default();
        assert_eq!(
            titles(&headphone_menu(&s, &names())),
            [
                "[x] None",
                "-",
                "[ ] Alpha",
                "[ ] Beta IEM",
                "-",
                "Import Curve\u{2026}",
                "Open the Folder of Your Curves"
            ]
        );
        assert_eq!(run(&mut s, Command::Headphone(Some(1))), Effect::Settings);
        assert_eq!(s.headphone_name.as_deref(), Some("Beta IEM"));
        assert_eq!(
            titles(&headphone_menu(&s, &names()))[..4],
            ["[ ] None", "-", "[ ] Alpha", "[x] Beta IEM"]
        );
        // "None" is stored as a choice.
        run(&mut s, Command::Headphone(None));
        assert_eq!(s.headphone_name.as_deref(), Some(""));
        // A place that is not in the list changes nothing.
        run(&mut s, Command::Headphone(Some(9)));
        assert_eq!(s.headphone_name.as_deref(), Some(""));
        // The first target is in use while none is chosen.
        assert_eq!(
            titles(&target_entries(&s, &names())),
            ["[x] Harman over-ear 2018", "[ ] Flat (zero)"]
        );
        run(&mut s, Command::Target(1));
        assert_eq!(s.target_name, "Flat (zero)");
        assert_eq!(run(&mut s, Command::ImportCurve), Effect::ImportCurve);
        assert_eq!(run(&mut s, Command::OpenCurvesFolder), Effect::OpenCurvesFolder);
    }

    fn titles(menu: &[Entry]) -> Vec<String> {
        menu.iter()
            .map(|entry| match entry {
                Entry::Header(title) => format!("# {title}"),
                Entry::Separator => "-".to_string(),
                Entry::Note(text) => format!("({text})"),
                Entry::Item { title, checked, .. } => match checked {
                    Some(true) => format!("[x] {title}"),
                    Some(false) => format!("[ ] {title}"),
                    None => title.clone(),
                },
                Entry::Group { title, entries } => format!("> {title} ({})", entries.len()),
            })
            .collect()
    }

    #[test]
    fn the_spectrum_menu_shows_the_settings_of_now() {
        let mut s = AppSettings::default();
        let menu = titles(&card_menu(Card::Spectrum, &s));
        assert_eq!(
            menu,
            [
                "# Traces",
                "[x] Left and Right",
                "[x] Mid",
                "[ ] Side (L\u{2212}R)",
                "[x] Peak Hold",
                "[x] Average",
                "[x] Headphone Overlay",
                "-",
                "# Tilt",
                "[x] 0 dB per octave (raw)",
                "[ ] 3 dB per octave",
                "[ ] 4.5 dB per octave",
                "-",
                "# Range",
                "[x] Auto (follows the music)",
                "[ ] 60 dB",
                "[ ] 96 dB",
                "[ ] 120 dB",
            ]
        );
        assert_eq!(run(&mut s, Command::ToggleSide), Effect::Settings);
        assert_eq!(run(&mut s, Command::LevelRange(120)), Effect::Settings);
        assert_eq!(run(&mut s, Command::Tilt(4.5)), Effect::Settings);
        let menu = titles(&card_menu(Card::Spectrum, &s));
        assert!(menu.contains(&"[x] Side (L\u{2212}R)".to_string()));
        assert!(menu.contains(&"[x] 120 dB".to_string()) && menu.contains(&"[ ] Auto (follows the music)".to_string()));
        assert!(menu.contains(&"[x] 4.5 dB per octave".to_string()));
        // "Freeze Display" stops every card: it is not in the menu of one card.
        assert!(!menu.iter().any(|title| title.contains("Freeze")));
    }

    #[test]
    fn every_card_has_a_menu_and_every_item_works() {
        for preset in LayoutPreset::ALL {
            for card in Card::ALL {
                let mut s = AppSettings {
                    layout_preset: preset,
                    ..AppSettings::default()
                };
                let menu = card_menu(card, &s);
                assert!(
                    !menu.is_empty() && !matches!(menu.last(), Some(Entry::Separator)),
                    "{card:?}"
                );
                assert!(!commands(&menu).contains(&Command::ToggleFreeze), "{card:?}");
                for command in commands(&menu) {
                    let before = s.clone();
                    let effect = run(&mut s, command);
                    assert_eq!(s.clone().validated(), s, "{command:?} leaves valid settings");
                    if effect != Effect::Settings {
                        assert_eq!(s, before, "{command:?} is an action, not a setting");
                    }
                }
            }
        }
        let s = AppSettings {
            layout_preset: LayoutPreset::Spectrum,
            ..AppSettings::default()
        };
        assert!(titles(&card_menu(Card::Spectrogram, &s))
            .contains(&"[x] Stereo Placement Beside the Spectrogram\tCtrl+4".to_string()));
        assert_eq!(
            titles(&card_menu(Card::Meters, &s))[..5],
            [
                "Reset Measurement\tR",
                "-",
                "# Loudness Target",
                "[x] Off",
                "[ ] \u{2212}14 LUFS \u{00B7} Streaming"
            ]
        );
        assert_eq!(
            titles(&card_menu(Card::Timeline, &s))[..4],
            ["# Window", "[ ] 5 minutes", "[x] 15 minutes", "[ ] 30 minutes"]
        );
    }

    #[test]
    fn the_tray_menu_opens_the_window_and_quits() {
        let mut s = AppSettings::default();
        assert_eq!(
            titles(&tray_menu(&s, false)),
            [
                "Open Joseon",
                "-",
                "[ ] Mini Spectrum Strip",
                "> Mini Spectrum Color (3)",
                "[x] Show the Name of the Song",
                "-",
                "Reset Measurement",
                "[ ] Freeze Display",
                "-",
                "Quit Joseon",
            ]
        );
        assert_eq!(run(&mut s, Command::OpenWindow), Effect::OpenWindow);
        assert_eq!(run(&mut s, Command::Quit), Effect::Quit);
        assert_eq!(run(&mut s, Command::ToggleMiniStrip), Effect::Settings);
        assert_eq!(
            run(&mut s, Command::MiniColor(MiniGraphColor::Orange)),
            Effect::Settings
        );
        assert!(s.show_mini_strip && s.mini_graph_color == MiniGraphColor::Orange);
        assert_eq!(MiniGraphColor::Orange.rgba(), [1.0, 0.58, 0.10, 1.0]);
        assert!(titles(&tray_menu(&s, true)).contains(&"[x] Mini Spectrum Strip".to_string()));
        let text = s.to_json();
        assert!(text.contains("\"miniGraphColor\": \"orange\"") && text.contains("\"showMiniStrip\": true"));
    }

    #[test]
    fn the_settings_menu_holds_every_setting() {
        let mut s = AppSettings::default();
        let menu = settings_menu(&s, false, &names());
        assert_eq!(
            titles(&menu),
            [
                "# Layout",
                "[x] Essential\tCtrl+1",
                "[ ] Spectrum\tCtrl+2",
                "[ ] Metering\tCtrl+3",
                "[ ] Waterfall\tCtrl+6",
                "[x] Session Timeline\tCtrl+5",
                "-",
                "# Headphones",
                "> Headphone (7)",
                "> Target (2)",
                "-",
                "# Spectrum",
                "> Resolution (3)",
                "> Release Time (4)",
                "> Peak Decay (4)",
                "> Tilt (3)",
                "> Level Range (4)",
                "-",
                "# Loudness",
                "> Loudness Target (4)",
                "[x] Reset When Audio Resumes After 2 s of Silence",
                "[x] Take the Windows Volume Out of the Numbers",
                "[ ] Also When the Device Has Its Own Volume Control",
                "-",
                "# Display",
                "> Animation (3)",
                "-",
                "Reset Measurement\tR",
                "[ ] Freeze Display\tSpace",
                "-",
                "[x] Show the Name of the Song",
                "[ ] Demo Signal",
            ]
        );
        // One item of every group is checked: the defaults are among the choices.
        for entry in &menu {
            if let Entry::Group { title, entries } = entry {
                let checked = entries
                    .iter()
                    .filter(|e| {
                        matches!(
                            e,
                            Entry::Item {
                                checked: Some(true),
                                ..
                            }
                        )
                    })
                    .count();
                assert_eq!(checked, 1, "{title}");
            }
        }
        assert_eq!(run(&mut s, Command::DisplayBins(2048)), Effect::Settings);
        assert_eq!(
            run(&mut s, Command::LoudnessTarget(LoudnessTarget::Ebu)),
            Effect::Settings
        );
        assert_eq!(run(&mut s, Command::ToggleVolumeCompensation), Effect::Settings);
        assert_eq!(
            (s.display_bins, s.loudness_target, s.compensate_system_volume),
            (2048, -23, false)
        );
        assert_eq!(
            run(&mut s, Command::RefreshMode(DisplayRefreshMode::HighRefresh)),
            Effect::Settings
        );
        assert_eq!(s.display_refresh_mode, DisplayRefreshMode::HighRefresh);
        assert!(s.to_json().contains("\"displayRefreshMode\": \"highRefresh\""));
        assert_eq!(run(&mut s, Command::ToggleDemo), Effect::SettingsAndSource);
        assert!(s.demo_mode);
        assert_eq!(run(&mut s, Command::ClearTimeline), Effect::ClearTimeline);
        assert_eq!(run(&mut s, Command::ResetMeasurement), Effect::ResetMeasurement);
        assert_eq!(run(&mut s, Command::ToggleFreeze), Effect::ToggleFreeze);
        // A value that is not a choice does not stay.
        run(&mut s, Command::DisplayBins(999));
        assert_eq!(s.display_bins, 1024);
        assert_eq!(tilt_title(3.0), "3 dB per octave");
    }
}

#[cfg(test)]
mod update_tests {
    use super::*;
    use crate::update::{Release, Version};

    fn titles(entries: &[Entry]) -> Vec<String> {
        entries
            .iter()
            .map(|entry| match entry {
                Entry::Note(text) => format!("({text})"),
                Entry::Item { title, checked, .. } => {
                    format!("{}{title}", if *checked == Some(true) { "[x] " } else { "" })
                }
                Entry::Header(title) => format!("# {title}"),
                _ => "-".to_string(),
            })
            .collect()
    }

    #[test]
    fn the_update_entries_follow_the_updater() {
        let mut s = AppSettings::default();
        assert_eq!(
            titles(&update_entries(&s, &Status::Idle)),
            [
                "-",
                "# Updates",
                "Check for Updates\u{2026}",
                "[x] Check for Updates Automatically"
            ]
        );
        let release = Release {
            version: Version(9, 1, 0),
            asset_name: String::new(),
            asset_url: String::new(),
            sha256: None,
        };
        assert_eq!(
            titles(&update_entries(&s, &Status::Available(release)))[2..4],
            ["(Joseon 9.1.0 is there)", "Update to 9.1.0 and Relaunch"]
        );
        // While the updater works there is nothing to click.
        assert_eq!(titles(&update_entries(&s, &Status::Checking)).len(), 4);
        assert_eq!(
            titles(&update_entries(&s, &Status::Failed("no answer".to_string())))[2..4],
            ["(Update failed: no answer)", "Check for Updates\u{2026}"]
        );
        let names = CurveNames::default();
        assert_eq!(apply(&mut s, Command::ToggleAutoUpdate, &names), Effect::Settings);
        assert!(!s.check_updates_automatically);
        assert_eq!(apply(&mut s, Command::CheckForUpdates, &names), Effect::CheckUpdates);
        assert_eq!(apply(&mut s, Command::InstallUpdate, &names), Effect::InstallUpdate);
    }
}
