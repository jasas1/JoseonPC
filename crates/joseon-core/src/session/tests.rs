//! Port of `SessionTestSupport.swift` and `SessionRecorderTests.swift`.
//! The engine and thread tests of the recorder are in `engine/tests.rs`. The
//! cost tests are in `tests/performance.rs`.

use super::*;
use crate::contracts::{BandEnergy, HeadphoneReading, StereoReading, StressFlag, StressSeverity};
use std::sync::Arc;

/// A wall clock the tests move by hand. The recorder never uses it for the
/// timeline axis, only for the dates on samples and events.
#[derive(Clone, Default)]
pub(crate) struct ManualClock(Arc<Mutex<f64>>);

impl ManualClock {
    pub fn starting_at(seconds: f64) -> Self {
        Self(Arc::new(Mutex::new(seconds)))
    }

    pub fn advance(&self, seconds: f64) {
        *self.0.lock().unwrap() += seconds;
    }

    pub fn recorder(&self, capacity: usize) -> SessionRecorder {
        let now = Arc::clone(&self.0);
        SessionRecorder::with_clock(capacity, Box::new(move || at(*now.lock().unwrap())))
    }
}

fn at(seconds: f64) -> SystemTime {
    SystemTime::UNIX_EPOCH + Duration::from_secs_f64(seconds)
}

fn seconds_of(date: SystemTime) -> f64 {
    date.duration_since(SystemTime::UNIX_EPOCH).unwrap().as_secs_f64()
}

/// A frame with only the numbers the recorder reads. Everything else is at its floor.
#[derive(Clone)]
pub(crate) struct TestFrame {
    pub momentary: f32,
    pub short_term: f32,
    pub true_peak_left: f32,
    pub true_peak_right: f32,
    pub clip_count: u64,
    pub correlation: f32,
    pub bands_db: f32,
    pub level_a: Option<f32>,
    pub flags: Vec<StressFlag>,
    pub is_silent: bool,
}

impl Default for TestFrame {
    fn default() -> Self {
        Self {
            momentary: LOUDNESS_FLOOR,
            short_term: LOUDNESS_FLOOR,
            true_peak_left: -120.0,
            true_peak_right: -120.0,
            clip_count: 0,
            correlation: 0.0,
            bands_db: SpectrumReading::FLOOR_DB,
            level_a: None,
            flags: Vec::new(),
            is_silent: false,
        }
    }
}

impl TestFrame {
    pub fn frame(&self) -> AnalysisFrame {
        let mut frame = AnalysisFrame::new(SpectrumReading::silent(8));
        frame.loudness.momentary_lufs = self.momentary;
        frame.loudness.short_term_lufs = self.short_term;
        frame.loudness.true_peak_left_dbtp = self.true_peak_left;
        frame.loudness.true_peak_right_dbtp = self.true_peak_right;
        frame.loudness.clip_count = self.clip_count;
        frame.stereo = StereoReading {
            correlation: self.correlation,
            ..Default::default()
        };
        let b = self.bands_db;
        frame.bands = BandEnergy {
            sub_bass: b,
            bass: b,
            low_mid: b,
            mid: b,
            upper_mid: b,
            presence: b,
            brilliance: b,
            air: b,
        };
        frame.is_silent = self.is_silent;
        if !self.flags.is_empty() {
            frame.headphone = Some(HeadphoneReading {
                model_name: "Test".into(),
                response_db: vec![0.0; 8],
                target_db: vec![0.0; 8],
                has_target: true,
                predicted_at_ear_db: vec![0.0; 8],
                stress_flags: self.flags.clone(),
            });
        }
        frame.spl = self.level_a.map(spl_reading);
        frame
    }
}

pub(crate) fn spl_reading(level: f32) -> SplReading {
    SplReading {
        calibration_name: "Test".into(),
        uncertainty_db: 2.0,
        level_a_fast: level,
        level_a_slow: level,
        level_z_eardrum: level,
        leq_a_track: level,
        leq_a_session: level,
        max_a_fast: level,
        band_levels_eardrum: vec![level; 31],
        dose_niosh: 0.0,
        dose_who_weekly: 0.0,
        dose_seconds: 0.0,
        seconds_to_niosh_limit: f64::INFINITY,
    }
}

fn flag(id: &str, title: &str, severity: StressSeverity) -> StressFlag {
    StressFlag {
        id: id.into(),
        severity,
        title: title.into(),
        detail: format!("detail of {id}"),
        frequency_range_hz: None,
        plot_label: String::new(),
    }
}

/// Ingests one test frame.
fn ingest(recorder: &SessionRecorder, frame: TestFrame, dt: f64) {
    recorder.ingest(&frame.frame(), dt);
}

fn plain() -> TestFrame {
    TestFrame::default()
}

fn silent() -> TestFrame {
    TestFrame {
        is_silent: true,
        ..plain()
    }
}

fn recorder(capacity: usize) -> SessionRecorder {
    ManualClock::default().recorder(capacity)
}

fn times(snapshot: &SessionSnapshot) -> Vec<f64> {
    snapshot.samples.iter().map(|sample| sample.time).collect()
}

fn kinds(snapshot: &SessionSnapshot) -> Vec<SessionEventKind> {
    snapshot.events.iter().map(|event| event.kind).collect()
}

#[track_caller]
fn near(actual: f64, expected: f64, accuracy: f64) {
    assert!(
        (actual - expected).abs() <= accuracy,
        "{actual}, expected {expected} ± {accuracy}"
    );
}

// MARK: Per-second aggregation

/// Two half-second frames make one sample: max, last, and the means in the right domain.
#[test]
fn one_second_of_frames_makes_one_sample() {
    let recorder = recorder(10);
    let first = TestFrame {
        momentary: -20.0,
        short_term: -22.0,
        true_peak_left: -1.0,
        true_peak_right: -3.0,
        correlation: 0.2,
        bands_db: 0.0,
        level_a: Some(80.0),
        ..plain()
    };
    ingest(&recorder, first, 0.5);
    assert!(
        recorder.snapshot().samples.is_empty(),
        "half a second is not a sample yet"
    );
    let second = TestFrame {
        momentary: -14.0,
        short_term: -18.0,
        true_peak_left: -2.0,
        true_peak_right: -0.5,
        correlation: 0.8,
        bands_db: -20.0,
        level_a: Some(90.0),
        ..plain()
    };
    ingest(&recorder, second, 0.5);
    let snapshot = recorder.snapshot();
    assert_eq!(snapshot.samples.len(), 1);
    let s = &snapshot.samples[0];
    near(s.time, 1.0, 1e-9);
    assert_eq!(s.momentary_max_lufs, -14.0, "max of the momentary values");
    assert_eq!(s.short_term_lufs, -18.0, "the last short-term value of the second");
    near(s.true_peak_dbtp as f64, -0.5, 1e-6);
    near(s.correlation as f64, 0.5, 1e-6);
    // Mean POWER of 0 dB and −20 dB = (1 + 0.01) / 2 = 0.505 -> −2.966 dB. A
    // mean of the decibels would be −10 dB.
    for band in s.bands {
        near(band as f64, -2.966, 0.01);
    }
    // Mean ENERGY of 80 and 90 dB SPL = (1e8 + 1e9) / 2 -> 87.40 dB.
    near(s.level_a.unwrap() as f64, 87.404, 0.01);
    assert!(!s.is_silent);
}

#[test]
fn silent_second_needs_every_silent_frame() {
    let recorder = recorder(10);
    for i in 0..4 {
        ingest(
            &recorder,
            TestFrame {
                is_silent: i != 2,
                ..plain()
            },
            0.25,
        );
    }
    assert!(!recorder.snapshot().samples[0].is_silent);
    for _ in 0..4 {
        ingest(&recorder, silent(), 0.25);
    }
    assert!(recorder.snapshot().samples.last().unwrap().is_silent);
}

#[test]
fn level_a_is_none_when_no_estimate_arrives() {
    let recorder = recorder(10);
    ingest(&recorder, plain(), 1.0);
    assert_eq!(recorder.snapshot().samples[0].level_a, None);
}

// MARK: The ring

#[test]
fn ring_wraps_at_capacity_and_keeps_the_newest() {
    let recorder = recorder(4);
    for second in 1..=10 {
        ingest(
            &recorder,
            TestFrame {
                momentary: -60.0 + second as f32,
                ..plain()
            },
            1.0,
        );
    }
    let snapshot = recorder.snapshot();
    assert_eq!(times(&snapshot), [7.0, 8.0, 9.0, 10.0]);
    let loudness: Vec<f32> = snapshot
        .samples
        .iter()
        .map(|sample| sample.momentary_max_lufs)
        .collect();
    assert_eq!(loudness, [-53.0, -52.0, -51.0, -50.0]);
}

#[test]
fn partial_second_is_carried_over_not_lost() {
    let recorder = recorder(10);
    // 0.75 s steps: the leftover of every second is carried into the next one.
    for _ in 0..5 {
        ingest(&recorder, plain(), 0.75);
    }
    assert_eq!(times(&recorder.snapshot()), [1.0, 2.0, 3.0]);
}

// MARK: Clocks

/// The axis is audio time. Wall time can run far ahead (or stand still): only the dates follow it.
#[test]
fn audio_time_clock_ignores_wall_time() {
    let clock = ManualClock::starting_at(1_000.0);
    let recorder = clock.recorder(10);
    for _ in 0..8 {
        clock.advance(60.0); // a minute of wall time per frame
        ingest(&recorder, plain(), 0.25);
    }
    let samples = recorder.snapshot().samples;
    assert_eq!(samples.len(), 2);
    near(samples[0].time, 1.0, 1e-9);
    near(samples[1].time, 2.0, 1e-9);
    near(seconds_of(samples[0].date), 1_000.0 + 4.0 * 60.0, 1e-6);
    near(seconds_of(samples[1].date), 1_000.0 + 8.0 * 60.0, 1e-6);
}

#[test]
fn frames_without_new_audio_do_not_move_the_clock() {
    let clock = ManualClock::default();
    let recorder = clock.recorder(10);
    for _ in 0..100 {
        clock.advance(1.0 / 60.0);
        ingest(&recorder, plain(), 0.0);
    }
    assert!(recorder.snapshot().samples.is_empty(), "no audio, no samples");
}

// MARK: Events: silence

#[test]
fn silence_events_from_audio_time() {
    let clock = ManualClock::default();
    let recorder = clock.recorder(10);
    for _ in 0..8 {
        // 4 s of silence in 0.5 s steps
        clock.advance(0.5);
        ingest(&recorder, silent(), 0.5);
    }
    let events = recorder.snapshot().events;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, SessionEventKind::SilenceStart);
    near(events[0].time, 0.0, 1e-9); // the start of the silence, not where it was noticed
    clock.advance(0.5);
    ingest(
        &recorder,
        TestFrame {
            momentary: -20.0,
            ..plain()
        },
        0.5,
    );
    let events = recorder.snapshot().events;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].kind, SessionEventKind::SilenceEnd);
    near(events[1].time, 4.5, 1e-9);
    near(seconds_of(events[1].date), 4.5, 1e-6);
    assert!(events[1].id > events[0].id);
}

#[test]
fn short_silence_makes_no_event() {
    let clock = ManualClock::default();
    let recorder = clock.recorder(10);
    for _ in 0..3 {
        clock.advance(0.5);
        ingest(&recorder, silent(), 0.5); // 1.5 s only
    }
    clock.advance(0.5);
    ingest(&recorder, plain(), 0.5);
    assert!(recorder.snapshot().events.is_empty());
}

/// The engine sleeps on silence: it stops publishing, or publishes frames with
/// `dt` 0. The gap in wall time is the silence.
#[test]
fn silence_events_from_a_wall_clock_gap_while_dt_is_zero() {
    let clock = ManualClock::default();
    let recorder = clock.recorder(10);
    clock.advance(0.1);
    ingest(
        &recorder,
        TestFrame {
            momentary: -20.0,
            ..plain()
        },
        0.1,
    );
    clock.advance(3.0); // the engine slept for 3 s
    ingest(&recorder, silent(), 0.0);
    let events = recorder.snapshot().events;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, SessionEventKind::SilenceStart);
    near(events[0].time, 0.1, 1e-9); // audio time did not move during the gap
    near(seconds_of(events[0].date), 0.1, 1e-6); // wall-clock date of the gap start
    clock.advance(10.0);
    ingest(
        &recorder,
        TestFrame {
            momentary: -20.0,
            ..plain()
        },
        0.1,
    );
    let events = recorder.snapshot().events;
    assert_eq!(events.len(), 2);
    assert_eq!(events[1].kind, SessionEventKind::SilenceEnd);
    near(events[1].time, 0.2, 1e-9); // the 13 s gap did not stretch the axis
    near(seconds_of(events[1].date), 13.1, 1e-6);
}

// MARK: Events: clipping

fn clipped(count: u64) -> TestFrame {
    TestFrame {
        clip_count: count,
        ..plain()
    }
}

#[test]
fn clip_event_counts_new_runs_once_per_second() {
    let recorder = recorder(10);
    ingest(&recorder, clipped(2), 0.5);
    ingest(&recorder, clipped(5), 0.5); // 5 new runs in second 1
    ingest(&recorder, clipped(5), 1.0); // nothing new in second 2
    let events = recorder.snapshot().events;
    assert_eq!(events.len(), 1, "at most one clip event per second");
    assert_eq!((events[0].kind, events[0].value), (SessionEventKind::Clip, 5.0));
    near(events[0].time, 1.0, 1e-9);
}

#[test]
fn a_clip_count_that_falls_is_a_new_baseline_not_negative_clips() {
    let recorder = recorder(10);
    ingest(&recorder, clipped(7), 1.0);
    // A measurement reset: the meter starts from 0 and clips again.
    ingest(&recorder, clipped(3), 1.0);
    ingest(&recorder, clipped(4), 1.0);
    let snapshot = recorder.snapshot();
    assert_eq!(kinds(&snapshot), [SessionEventKind::Clip, SessionEventKind::Clip]);
    assert_eq!(snapshot.events[0].value, 7.0);
    assert_eq!(
        snapshot.events[1].value, 1.0,
        "3 -> 4 is one new run; the fall from 7 is not counted"
    );
}

#[test]
fn note_track_start_rebases_the_clip_count() {
    let recorder = recorder(10);
    ingest(&recorder, clipped(9), 1.0);
    recorder.note_track_start();
    ingest(&recorder, clipped(6), 1.0); // a fresh meter that clipped 6 times
    let snapshot = recorder.snapshot();
    let clips: Vec<f32> = snapshot
        .events
        .iter()
        .filter(|event| event.kind == SessionEventKind::Clip)
        .map(|event| event.value)
        .collect();
    assert_eq!(clips, [9.0, 6.0], "after a track start the whole new count is new");
}

// MARK: Events: inter-sample overs

#[test]
fn inter_sample_over_is_one_per_second_with_the_maximum() {
    let recorder = recorder(10);
    let peaks = |left: f32, right: f32| TestFrame {
        true_peak_left: left,
        true_peak_right: right,
        ..plain()
    };
    ingest(&recorder, peaks(0.8, -3.0), 0.5);
    ingest(&recorder, peaks(-3.0, 1.5), 0.5);
    ingest(&recorder, peaks(-0.2, -0.3), 1.0);
    let snapshot = recorder.snapshot();
    assert_eq!(
        snapshot.events.len(),
        1,
        "one event for the second, none while under 0 dBTP"
    );
    assert_eq!(snapshot.events[0].kind, SessionEventKind::InterSampleOver);
    near(snapshot.events[0].value as f64, 1.5, 1e-6);
    near(snapshot.samples[0].true_peak_dbtp as f64, 1.5, 1e-6);
}

// MARK: Events: stress flags

#[test]
fn stress_flags_are_diffed_not_repeated() {
    let recorder = recorder(10);
    let sub = flag("subBassLoad", "Sub-bass load high", StressSeverity::High);
    let dense = flag("denseMaster", "Dense master", StressSeverity::Watch);
    // The flag holds for 30 frames (hysteresis keeps it up): one raised event, not 30.
    for flags in [vec![sub.clone()], vec![sub, dense.clone()], vec![dense], vec![]] {
        for _ in 0..30 {
            ingest(
                &recorder,
                TestFrame {
                    flags: flags.clone(),
                    ..plain()
                },
                1.0 / 60.0,
            );
        }
    }
    let snapshot = recorder.snapshot();
    use SessionEventKind::{StressFlagCleared as Cleared, StressFlagRaised as Raised};
    assert_eq!(kinds(&snapshot), [Raised, Raised, Cleared, Cleared]);
    let events = &snapshot.events;
    assert_eq!(
        (&*events[0].label, &*events[0].detail),
        ("Sub-bass load high", "subBassLoad")
    );
    assert_eq!(events[0].value, 2.0, "the severity of the flag");
    let details: Vec<&str> = events.iter().map(|event| &*event.detail).collect();
    assert_eq!(details, ["subBassLoad", "denseMaster", "subBassLoad", "denseMaster"]);
    assert_eq!(
        events.iter().map(|event| event.id).collect::<Vec<_>>(),
        [1, 2, 3, 4],
        "ids rise"
    );
}

#[test]
fn track_start_event() {
    let clock = ManualClock::default();
    let recorder = clock.recorder(10);
    clock.advance(2.0);
    ingest(&recorder, plain(), 2.0);
    recorder.note_track_start();
    let events = recorder.snapshot().events;
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].kind, SessionEventKind::TrackStart);
    near(events[0].time, 2.0, 1e-9);
    near(seconds_of(events[0].date), 2.0, 1e-6);
}

// MARK: Event housekeeping

#[test]
fn events_older_than_the_oldest_sample_are_dropped() {
    let recorder = recorder(3);
    recorder.note_track_start(); // at time 0
    for _ in 0..2 {
        ingest(&recorder, plain(), 1.0);
    }
    assert_eq!(recorder.snapshot().events.len(), 1, "still inside the record");
    for _ in 0..5 {
        ingest(&recorder, plain(), 1.0);
    }
    let snapshot = recorder.snapshot();
    assert_eq!(times(&snapshot), [5.0, 6.0, 7.0]);
    assert!(snapshot.events.is_empty(), "the event is older than the oldest second");
}

#[test]
fn events_are_capped_at_two_thousand() {
    let recorder = recorder(10);
    for _ in 0..2_500 {
        recorder.note_track_start();
    }
    let ids: Vec<u64> = recorder.snapshot().events.iter().map(|event| event.id).collect();
    assert_eq!(ids.len(), MAX_EVENTS);
    assert_eq!(ids, (501..=2_500).collect::<Vec<u64>>(), "the oldest went first");
}

// MARK: Revision and clear

#[test]
fn revision_rises_on_every_sample_and_event() {
    let recorder = recorder(10);
    assert_eq!(recorder.snapshot().revision, 0);
    ingest(&recorder, plain(), 0.5);
    assert_eq!(recorder.snapshot().revision, 0, "nothing closed yet");
    ingest(&recorder, plain(), 0.5);
    assert_eq!(recorder.snapshot().revision, 1);
    recorder.note_track_start();
    assert_eq!(recorder.snapshot().revision, 2);
}

#[test]
fn clear_forgets_everything_and_restarts_the_clock() {
    let recorder = recorder(10);
    for _ in 0..3 {
        ingest(&recorder, clipped(1), 1.0);
    }
    recorder.note_track_start();
    assert!(!recorder.snapshot().samples.is_empty());
    let before = recorder.snapshot().revision;
    recorder.clear();
    let empty = recorder.snapshot();
    assert!(empty.samples.is_empty() && empty.events.is_empty());
    assert!(empty.revision > before, "panels see that the record changed");

    ingest(
        &recorder,
        TestFrame {
            momentary: -12.0,
            ..plain()
        },
        1.0,
    );
    let after = recorder.snapshot();
    assert_eq!(after.samples.len(), 1);
    near(after.samples[0].time, 1.0, 1e-9); // the audio clock starts again
    assert_eq!(after.samples[0].momentary_max_lufs, -12.0);
    assert!(after.events.is_empty(), "no clip event from the count before the clear");
}

// MARK: Bad input

#[test]
fn non_finite_readings_never_reach_a_sample() {
    let recorder = recorder(10);
    let bad = TestFrame {
        momentary: f32::NAN,
        short_term: f32::INFINITY,
        true_peak_left: f32::NAN,
        true_peak_right: f32::NEG_INFINITY,
        correlation: f32::NAN,
        bands_db: f32::NAN,
        level_a: Some(f32::NAN),
        ..plain()
    };
    ingest(&recorder, bad, 1.0);
    let snapshot = recorder.snapshot();
    let s = &snapshot.samples[0];
    assert!(s.momentary_max_lufs.is_finite() && s.short_term_lufs.is_finite() && s.true_peak_dbtp.is_finite());
    assert!(s.correlation.is_finite());
    assert!(s
        .bands
        .iter()
        .all(|band| band.is_finite() && *band >= SpectrumReading::FLOOR_DB));
    assert!(s.level_a.unwrap().is_finite());
    assert!(snapshot.events.is_empty(), "a NaN peak is not an over");
    // A bad `dt` moves nothing.
    for dt in [f64::NAN, f64::INFINITY, -1.0] {
        recorder.ingest(&plain().frame(), dt);
    }
    assert_eq!(recorder.snapshot().samples.len(), 1);
}

#[test]
fn capacity_is_never_zero() {
    let recorder = SessionRecorder::new(0);
    ingest(&recorder, plain(), 1.0);
    ingest(&recorder, plain(), 1.0);
    assert_eq!(recorder.capacity_seconds(), 1);
    assert_eq!(recorder.snapshot().samples.len(), 1);
}
