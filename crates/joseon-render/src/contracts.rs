//! Render contracts: what the app sets on a panel, and the cursor the panels share.
//!
//! Port source: `Sources/JoseonRender/RenderContracts.swift`, and the option
//! enums of `Panels.swift`, `MetersRenderer.swift`, `VectorscopeRenderer.swift`
//! and `Comparison.swift`.

use std::collections::BTreeMap;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PanelKind {
    Spectrum,
    Spectrogram,
    Vectorscope,
    Meters,
    /// The session timeline (last minutes of loudness, peaks and events).
    Timeline,
}

impl PanelKind {
    pub const ALL: [PanelKind; 5] = [
        Self::Spectrum,
        Self::Spectrogram,
        Self::Vectorscope,
        Self::Meters,
        Self::Timeline,
    ];

    /// The name in saved settings and on the probe command line. Same strings as the macOS app.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Spectrum => "spectrum",
            Self::Spectrogram => "spectrogram",
            Self::Vectorscope => "vectorscope",
            Self::Meters => "meters",
            Self::Timeline => "timeline",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SpectrumViewOptions {
    pub show_left_right: bool,
    pub show_mid: bool,
    /// Side = (L−R)/2 as its own trace. Off by default.
    pub show_side: bool,
    pub show_peak_hold: bool,
    pub show_average: bool,
    pub show_headphone_overlay: bool,
    pub min_db: f32,
    pub max_db: f32,
}

impl Default for SpectrumViewOptions {
    fn default() -> Self {
        Self {
            show_left_right: true,
            show_mid: true,
            show_side: false,
            show_peak_hold: true,
            show_average: true,
            show_headphone_overlay: true,
            min_db: -96.0,
            max_db: 0.0,
        }
    }
}

/// What the level axis of the spectrum shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum LevelAxis {
    /// dBFS only.
    #[default]
    DbFs,
    /// dBFS curves as before, plus the third-octave levels at the eardrum on a
    /// right-hand dB SPL axis. Needs `frame.spl` and `frame.third_octave`;
    /// without them the header says "not calibrated".
    DbSpl,
}

/// Which noise dose the meters show.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum DoseStandard {
    /// NIOSH: 85 dB(A) for 8 h is 100 %, 3 dB exchange rate.
    #[default]
    NioshDaily,
    /// WHO / ITU H.870, adults: 80 dB(A) for 40 h per week is 100 %, 3 dB exchange rate.
    WhoWeekly,
}

/// The spectrogram as the scrolling picture or as the 3-D field (a waterfall). A switch keeps the history.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum SpectrogramMode {
    #[default]
    Flat,
    Waterfall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum VectorscopeMode {
    /// Goniometer: the L / R sample cloud, mid up, side across.
    #[default]
    Lissajous,
    /// Stereo placement by frequency: y = log frequency, x = pan position of
    /// each analyzer bin, light = level, hue = the band hue of the frequency.
    PanSpectrum,
}

/// What the difference lane of an A/B comparison shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ComparisonMode {
    /// `B − A` of the two long-term spectra: what changed in the music.
    #[default]
    Signal,
    /// `response_B − response_A` of the two headphone curves.
    Headphone,
}

// MARK: Linked cursor
//
// One `PanelCursorLink` is shared by the panels of one window. A panel that
// the pointer (or the keyboard) moves in writes the cursor; every panel that
// holds the same link draws it and shows its own readout at that frequency.
// UI thread only.

/// The shared cursor. `frequency_hz` is always set; `seconds_ago` only when the
/// cursor comes from a time axis (spectrogram).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PanelCursor {
    pub frequency_hz: f32,
    /// Seconds before "now" on the spectrogram time axis. `None` = the live moment.
    pub seconds_ago: Option<f64>,
    /// The panel the cursor came from.
    pub source: PanelKind,
    /// A click pins the cursor: it stays when the pointer leaves, until Esc or a second click.
    pub is_pinned: bool,
}

impl PanelCursor {
    /// A hover cursor at the live moment.
    pub fn new(frequency_hz: f32, source: PanelKind) -> Self {
        Self {
            frequency_hz,
            seconds_ago: None,
            source,
            is_pinned: false,
        }
    }
}

/// A past spectrum column that the spectrogram publishes while the cursor has a
/// time, so the spectrum panel can draw it as a ghost trace.
#[derive(Debug, Clone, PartialEq)]
pub struct CursorHistorySlice {
    pub seconds_ago: f64,
    /// Same length and meaning as `SpectrumReading::frequencies` / `mid` (dBFS).
    pub frequencies: Vec<f32>,
    pub mid_db: Vec<f32>,
}

#[derive(Default)]
pub struct PanelCursorLink {
    cursor: Option<PanelCursor>,
    history_slice: Option<CursorHistorySlice>,
    observers: BTreeMap<u64, Box<dyn FnMut()>>,
    next_observer: u64,
}

impl PanelCursorLink {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn cursor(&self) -> Option<PanelCursor> {
        self.cursor
    }

    pub fn history_slice(&self) -> Option<&CursorHistorySlice> {
        self.history_slice.as_ref()
    }

    /// Sets or moves the cursor. A pinned cursor is replaced only by another
    /// pinned cursor or by `clear`: hover moves from any panel are ignored
    /// while it is pinned.
    pub fn set(&mut self, new: PanelCursor) {
        if let Some(current) = self.cursor {
            if current.is_pinned && !new.is_pinned {
                return;
            }
        }
        if Some(new) == self.cursor {
            return;
        }
        self.cursor = Some(new);
        if new.seconds_ago.is_none() {
            self.history_slice = None;
        }
        self.notify();
    }

    /// The pointer left the panel: clears a hover cursor, keeps a pinned one.
    pub fn hover_ended(&mut self, source: PanelKind) {
        match self.cursor {
            Some(current) if !current.is_pinned && current.source == source => {}
            _ => return,
        }
        self.cursor = None;
        self.history_slice = None;
        self.notify();
    }

    /// Esc, or a click on a pinned cursor.
    pub fn clear(&mut self) {
        if self.cursor.is_none() && self.history_slice.is_none() {
            return;
        }
        self.cursor = None;
        self.history_slice = None;
        self.notify();
    }

    /// The spectrogram publishes the column under a timed cursor. `None` when the time is outside its history.
    pub fn publish(&mut self, slice: Option<CursorHistorySlice>) {
        if slice == self.history_slice {
            return;
        }
        self.history_slice = slice;
        self.notify();
    }

    /// Gives a token for `remove_observer`. The closure runs after every change.
    pub fn add_observer(&mut self, on_change: impl FnMut() + 'static) -> u64 {
        self.next_observer += 1;
        self.observers.insert(self.next_observer, Box::new(on_change));
        self.next_observer
    }

    pub fn remove_observer(&mut self, id: u64) {
        self.observers.remove(&id);
    }

    fn notify(&mut self) {
        for observer in self.observers.values_mut() {
            observer();
        }
    }
}
