//! Rolling, in-memory record of the last `capacity_seconds` of listening: one
//! `SessionSample` per second of AUDIO time, plus `SessionEvent`s. Nothing is
//! ever written to disk and no audio is kept, only the numbers of the contract.
//!
//! Port source: `Sources/JoseonCore/Session/SessionRecorder.swift`.
//!
//! ## Threading
//!
//! `ingest` and `note_track_start` run on the analysis thread (about 60 calls
//! per second). `snapshot` and `clear` run on any thread. One lock guards the
//! two rings and the revision. The analysis thread takes it at most twice per
//! ingest, for a few stores each time. A reader takes it for one copy of the
//! rings; its output is reserved before the lock.
//!
//! Port note: the analysis-thread state has its own lock. Rust needs one to
//! allow `&self` there, and only the analysis thread ever takes it.
//!
//! ## Storage
//!
//! The rings are allocated in `new`. A warm recorder allocates nothing per
//! ingest, except for the text of an event.
//!
//! ## Clocks
//!
//! `SessionSample::time` is audio time: it advances by `dt` only, so a silence
//! gap where the engine sleeps does not stretch the timeline axis. Wall-clock
//! dates come from the injected `now`.

#[cfg(test)]
pub(crate) mod tests;

use crate::contracts::{
    AnalysisFrame, LoudnessReading, SessionEvent, SessionEventKind, SessionRecording, SessionSample, SessionSnapshot,
    SpectrumReading, SplReading,
};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

/// Silence longer than this makes a `SilenceStart`. The end of it makes a
/// `SilenceEnd`. Measured in audio seconds while frames flow, in wall seconds
/// across a gap where `dt` is 0 (the engine sleeps on silence and stops publishing).
pub const SILENCE_SECONDS: f64 = 2.0;
/// Hard cap on kept events. The oldest go first.
pub const MAX_EVENTS: usize = 2000;
/// The default length of the record: 30 minutes.
pub const DEFAULT_CAPACITY_SECONDS: usize = 1800;

const BAND_COUNT: usize = 8;
const LOUDNESS_FLOOR: f32 = LoudnessReading::SILENCE_LUFS;
const PEAK_FLOOR: f32 = -120.0;

/// The wall clock of a recorder.
pub type Clock = Box<dyn Fn() -> SystemTime + Send + Sync>;

/// The two rings and the revision. Guarded by `SessionRecorder::rings`.
struct Rings {
    samples: Vec<SessionSample>,
    /// Next slot to write.
    sample_head: usize,
    sample_count: usize,
    events: Vec<SessionEvent>,
    event_head: usize,
    event_count: usize,
    revision: u64,
    /// `clear` empties the rings at once and asks the analysis thread to drop its accumulators.
    clear_pending: bool,
}

impl Rings {
    /// The oldest second in the record, when the record is full.
    fn window_start(&self) -> f64 {
        if self.sample_count == self.samples.len() {
            self.samples[self.sample_head].time - 1.0
        } else {
            f64::NEG_INFINITY
        }
    }

    /// Appends, then walks the event back while it is older than its neighbour.
    /// So the ring stays oldest first, also for a `SilenceStart` that is dated
    /// back to the start of the silence.
    fn push(&mut self, event: SessionEvent) {
        let capacity = self.events.len();
        let mut slot = self.event_head;
        self.events[slot] = event;
        self.event_head = (self.event_head + 1) % capacity;
        if self.event_count < capacity {
            self.event_count += 1; // else the oldest was overwritten
        }
        let mut steps = self.event_count - 1;
        while steps > 0 {
            let previous = (slot + capacity - 1) % capacity;
            if self.events[previous].time <= self.events[slot].time {
                break;
            }
            self.events.swap(previous, slot);
            slot = previous;
            steps -= 1;
        }
        self.revision += 1;
    }
}

/// What the second being built has seen so far.
struct Accumulator {
    frames: usize,
    /// Audio seconds in the second being built.
    elapsed: f64,
    momentary_max: f32,
    short_term: f32,
    true_peak: f32,
    correlation_sum: f64,
    band_power: [f64; BAND_COUNT],
    level_a_energy: f64,
    level_a_frames: usize,
    all_silent: bool,
    /// New clipped-sample runs seen in this second.
    clip_runs: u64,
    /// Highest true peak over 0 dBTP in this second, and whether there was one.
    over_dbtp: f32,
    saw_over: bool,
}

impl Accumulator {
    fn new(elapsed: f64) -> Self {
        Self {
            frames: 0,
            elapsed,
            momentary_max: LOUDNESS_FLOOR,
            short_term: LOUDNESS_FLOOR,
            true_peak: PEAK_FLOOR,
            correlation_sum: 0.0,
            band_power: [0.0; BAND_COUNT],
            level_a_energy: 0.0,
            level_a_frames: 0,
            all_silent: true,
            clip_runs: 0,
            over_dbtp: 0.0,
            saw_over: false,
        }
    }
}

struct FlagState {
    id: String,
    title: String,
    seen: bool,
}

/// State that only the analysis thread touches.
struct Analysis {
    /// Seconds of audio since the start (or the last `clear`).
    audio_time: f64,
    acc: Accumulator,
    last_clip_count: u64,
    last_ingest_wall: Option<SystemTime>,
    silence_run: f64,
    in_silence: bool,
    silence_start_time: f64,
    silence_start_wall: Option<SystemTime>,
    active_flags: Vec<FlagState>,
    next_event_id: u64,
    /// Events made during one ingest, pushed under a single lock at the end of it.
    pending: Vec<SessionEvent>,
}

impl Analysis {
    fn new() -> Self {
        Self {
            audio_time: 0.0,
            acc: Accumulator::new(0.0),
            last_clip_count: 0,
            last_ingest_wall: None,
            silence_run: 0.0,
            in_silence: false,
            silence_start_time: 0.0,
            silence_start_wall: None,
            active_flags: Vec::with_capacity(8),
            next_event_id: 1,
            pending: Vec::with_capacity(16),
        }
    }

    /// Forgets the record. Event ids keep rising.
    fn reset(&mut self) {
        let next_event_id = self.next_event_id;
        *self = Self::new();
        self.next_event_id = next_event_id;
    }

    fn emit(&mut self, kind: SessionEventKind, time: f64, date: SystemTime, label: &str, detail: &str, value: f32) {
        self.pending.push(SessionEvent {
            id: self.next_event_id,
            kind,
            time,
            date,
            label: label.into(),
            detail: detail.into(),
            value,
        });
        self.next_event_id += 1;
    }

    fn accumulate(&mut self, frame: &AnalysisFrame, step: f64) {
        let acc = &mut self.acc;
        acc.frames += 1;
        acc.elapsed += step;

        let l = &frame.loudness;
        acc.momentary_max = acc.momentary_max.max(finite(l.momentary_lufs, LOUDNESS_FLOOR));
        acc.short_term = finite(l.short_term_lufs, LOUDNESS_FLOOR); // last one wins
        let peak = finite(l.true_peak_left_dbtp, PEAK_FLOOR).max(finite(l.true_peak_right_dbtp, PEAK_FLOOR));
        acc.true_peak = acc.true_peak.max(peak);
        if peak > 0.0 {
            acc.saw_over = true;
            acc.over_dbtp = acc.over_dbtp.max(peak);
        }
        // A measurement reset makes the count fall: that is a new baseline, never negative clips.
        if l.clip_count > self.last_clip_count {
            acc.clip_runs += l.clip_count - self.last_clip_count;
        }
        self.last_clip_count = l.clip_count;

        let correlation = frame.stereo.correlation;
        acc.correlation_sum += if correlation.is_finite() {
            correlation.clamp(-1.0, 1.0) as f64
        } else {
            0.0
        };

        // Bands: mean of the POWER, not of the decibels.
        for (sum, db) in acc.band_power.iter_mut().zip(frame.bands.values()) {
            *sum += power(db);
        }
        // A-weighted level at the ear: mean of the energy, when the estimate is there.
        if let Some(spl) = &frame.spl {
            acc.level_a_energy += power(finite(spl.level_a_slow, SplReading::FLOOR_DB));
            acc.level_a_frames += 1;
        }
        if !frame.is_silent {
            acc.all_silent = false;
        }
    }

    fn track_silence(&mut self, is_silent: bool, step: f64, wall_gap: f64, wall: SystemTime) {
        if !is_silent {
            if self.in_silence {
                self.in_silence = false;
                self.emit(SessionEventKind::SilenceEnd, self.audio_time, wall, "Sound", "", 0.0);
            }
            self.silence_run = 0.0;
            self.silence_start_wall = None;
            return;
        }
        // Audio time while frames flow; wall time across a gap where the engine slept (`dt` is 0).
        let covered = if step > 0.0 { step } else { wall_gap };
        if self.silence_run == 0.0 {
            self.silence_start_time = self.audio_time - step;
            self.silence_start_wall = Some(wall.checked_sub(Duration::from_secs_f64(covered)).unwrap_or(wall));
        }
        self.silence_run += covered;
        if !self.in_silence && self.silence_run > SILENCE_SECONDS {
            self.in_silence = true;
            let date = self.silence_start_wall.unwrap_or(wall);
            self.emit(
                SessionEventKind::SilenceStart,
                self.silence_start_time,
                date,
                "Silence",
                "",
                0.0,
            );
        }
    }

    fn diff_stress_flags(&mut self, frame: &AnalysisFrame, wall: SystemTime) {
        for flag in &mut self.active_flags {
            flag.seen = false;
        }
        for flag in frame.headphone.iter().flat_map(|headphone| &headphone.stress_flags) {
            if let Some(active) = self.active_flags.iter_mut().find(|active| active.id == flag.id) {
                active.seen = true;
                if active.title != flag.title {
                    active.title.clone_from(&flag.title);
                }
            } else {
                self.active_flags.push(FlagState {
                    id: flag.id.clone(),
                    title: flag.title.clone(),
                    seen: true,
                });
                let severity = flag.severity as i32 as f32;
                self.emit(
                    SessionEventKind::StressFlagRaised,
                    self.audio_time,
                    wall,
                    &flag.title,
                    &flag.id,
                    severity,
                );
            }
        }
        let mut i = self.active_flags.len();
        while i > 0 {
            i -= 1;
            if !self.active_flags[i].seen {
                let gone = self.active_flags.remove(i);
                self.emit(
                    SessionEventKind::StressFlagCleared,
                    self.audio_time,
                    wall,
                    &gone.title,
                    &gone.id,
                    0.0,
                );
            }
        }
    }
}

pub struct SessionRecorder {
    capacity_seconds: usize,
    clock: Clock,
    rings: Mutex<Rings>,
    analysis: Mutex<Analysis>,
}

impl Default for SessionRecorder {
    fn default() -> Self {
        Self::new(DEFAULT_CAPACITY_SECONDS)
    }
}

impl SessionRecorder {
    /// A recorder of `capacity_seconds` (at least 1) on the system clock.
    pub fn new(capacity_seconds: usize) -> Self {
        Self::with_clock(capacity_seconds, Box::new(SystemTime::now))
    }

    /// `now` is the wall clock. It is never used for the timeline axis, only for labels.
    pub fn with_clock(capacity_seconds: usize, now: Clock) -> Self {
        let capacity = capacity_seconds.max(1);
        let blank_sample = SessionSample {
            time: 0.0,
            date: SystemTime::UNIX_EPOCH,
            momentary_max_lufs: LOUDNESS_FLOOR,
            short_term_lufs: LOUDNESS_FLOOR,
            true_peak_dbtp: PEAK_FLOOR,
            correlation: 0.0,
            bands: [SpectrumReading::FLOOR_DB; BAND_COUNT],
            level_a: None,
            is_silent: true,
        };
        let blank_event = SessionEvent {
            id: 0,
            kind: SessionEventKind::TrackStart,
            time: 0.0,
            date: SystemTime::UNIX_EPOCH,
            label: "".into(),
            detail: "".into(),
            value: 0.0,
        };
        Self {
            capacity_seconds: capacity,
            clock: now,
            rings: Mutex::new(Rings {
                samples: vec![blank_sample; capacity],
                sample_head: 0,
                sample_count: 0,
                events: vec![blank_event; MAX_EVENTS],
                event_head: 0,
                event_count: 0,
                revision: 0,
                clear_pending: false,
            }),
            analysis: Mutex::new(Analysis::new()),
        }
    }

    /// A panic in another thread must not stop the record: the data is plain numbers.
    fn rings(&self) -> std::sync::MutexGuard<'_, Rings> {
        self.rings.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    fn analysis(&self) -> std::sync::MutexGuard<'_, Analysis> {
        self.analysis.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Writes the finished second into the ring and makes the events that belong to it.
    fn close_sample(&self, analysis: &mut Analysis, time: f64, date: SystemTime) {
        let acc = &analysis.acc;
        let n = acc.frames.max(1) as f64;
        {
            let mut rings = self.rings();
            // A `clear` landed while this ingest ran: the second belongs to the record that was thrown away.
            if rings.clear_pending {
                return;
            }
            let capacity = rings.samples.len();
            let slot = rings.sample_head;
            let sample = &mut rings.samples[slot];
            sample.time = time;
            sample.date = date;
            sample.momentary_max_lufs = acc.momentary_max;
            sample.short_term_lufs = acc.short_term;
            sample.true_peak_dbtp = acc.true_peak;
            sample.correlation = (acc.correlation_sum / n) as f32;
            for (band, sum) in sample.bands.iter_mut().zip(acc.band_power) {
                *band = decibels(sum / n).max(SpectrumReading::FLOOR_DB);
            }
            sample.level_a = (acc.level_a_frames > 0)
                .then(|| decibels(acc.level_a_energy / acc.level_a_frames as f64).max(SplReading::FLOOR_DB));
            sample.is_silent = acc.all_silent;
            rings.sample_head = (slot + 1) % capacity;
            if rings.sample_count < capacity {
                rings.sample_count += 1;
            }
            rings.revision += 1;
            // Events older than the oldest second in the ring are gone.
            if rings.sample_count == capacity {
                let oldest = rings.samples[rings.sample_head].time - 1.0;
                while rings.event_count > 0 {
                    let events = rings.events.len();
                    let first = (rings.event_head + events - rings.event_count) % events;
                    if rings.events[first].time < oldest {
                        rings.event_count -= 1;
                    } else {
                        break;
                    }
                }
            }
        }
        let (clip_runs, saw_over, over_dbtp) = (acc.clip_runs, acc.saw_over, acc.over_dbtp);
        if clip_runs > 0 {
            analysis.emit(SessionEventKind::Clip, time, date, "Clip", "", clip_runs as f32);
        }
        if saw_over {
            analysis.emit(
                SessionEventKind::InterSampleOver,
                time,
                date,
                "Over 0 dBTP",
                "",
                over_dbtp,
            );
        }
    }

    fn flush_pending(&self, analysis: &mut Analysis) {
        if analysis.pending.is_empty() {
            return;
        }
        let mut rings = self.rings();
        // Same as a closed sample: a `clear` in flight wins.
        if !rings.clear_pending {
            // `SilenceStart` carries the time the silence began, which can be
            // before the event was made. An event that lands outside the record
            // is dropped, like one the record has grown past.
            let window_start = rings.window_start();
            for event in analysis.pending.drain(..) {
                if event.time >= window_start {
                    rings.push(event);
                }
            }
        }
        drop(rings);
        analysis.pending.clear();
    }
}

impl SessionRecording for SessionRecorder {
    fn capacity_seconds(&self) -> usize {
        self.capacity_seconds
    }

    fn ingest(&self, frame: &AnalysisFrame, dt: f64) {
        let mut analysis = self.analysis();
        let must_clear = std::mem::take(&mut self.rings().clear_pending);
        if must_clear {
            analysis.reset();
        }

        let wall = (self.clock)();
        let step = if dt.is_finite() && dt > 0.0 {
            dt.min(self.capacity_seconds as f64)
        } else {
            0.0
        };
        let wall_gap = analysis.last_ingest_wall.map_or(0.0, |last| {
            wall.duration_since(last).map_or(0.0, |gap| gap.as_secs_f64())
        });
        analysis.last_ingest_wall = Some(wall);

        analysis.audio_time += step;
        analysis.accumulate(frame, step);
        analysis.track_silence(frame.is_silent, step, wall_gap, wall);
        analysis.diff_stress_flags(frame, wall);

        if analysis.acc.elapsed >= 1.0 {
            let remainder = analysis.acc.elapsed % 1.0;
            let time = analysis.audio_time - remainder;
            self.close_sample(&mut analysis, time, wall);
            analysis.acc = Accumulator::new(remainder);
        }
        self.flush_pending(&mut analysis);
    }

    /// The measurement was reset (a new track). The clip count starts from 0 again.
    fn note_track_start(&self) {
        let mut analysis = self.analysis();
        let wall = (self.clock)();
        analysis.last_clip_count = 0;
        let time = analysis.audio_time;
        analysis.emit(SessionEventKind::TrackStart, time, wall, "Track start", "", 0.0);
        self.flush_pending(&mut analysis);
    }

    /// A consistent copy of both rings, oldest first.
    fn snapshot(&self) -> SessionSnapshot {
        // Reserved outside the lock: no allocation runs while the analysis thread waits.
        let mut samples = Vec::with_capacity(self.capacity_seconds);
        let mut events = Vec::with_capacity(64);
        let rings = self.rings();
        let capacity = rings.samples.len();
        let start = (rings.sample_head + capacity - rings.sample_count) % capacity;
        samples.extend((0..rings.sample_count).map(|i| rings.samples[(start + i) % capacity].clone()));
        let capacity = rings.events.len();
        let start = (rings.event_head + capacity - rings.event_count) % capacity;
        events.extend((0..rings.event_count).map(|i| rings.events[(start + i) % capacity].clone()));
        SessionSnapshot {
            samples,
            events,
            revision: rings.revision,
        }
    }

    /// Forgets everything. The revision rises, so panels redraw. Event ids keep rising.
    fn clear(&self) {
        let mut rings = self.rings();
        rings.sample_head = 0;
        rings.sample_count = 0;
        rings.event_head = 0;
        rings.event_count = 0;
        rings.revision += 1;
        rings.clear_pending = true;
    }
}

fn finite(value: f32, floor: f32) -> f32 {
    if value.is_finite() {
        value.max(floor)
    } else {
        floor
    }
}

/// log2(10) / 10 as the macOS app writes it: 0.33219281.
const LOG2_10_OVER_10: f32 = 0.332_192_8;

/// dB -> linear power. 10^(dB/10) = 2^(dB * log2(10) / 10).
fn power(db: f32) -> f64 {
    let clamped = if db.is_finite() { db.max(-200.0) } else { -200.0 };
    (clamped * LOG2_10_OVER_10).exp2() as f64
}

/// Linear power -> dB.
fn decibels(power: f64) -> f32 {
    if !(power > 0.0 && power.is_finite()) {
        return SpectrumReading::FLOOR_DB;
    }
    (power.log2() as f32) * 3.010_3
}
