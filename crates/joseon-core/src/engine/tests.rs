//! Tests of the analysis engine, ported from `AnalysisEngineTests.swift`,
//! `AnalysisEngineHardeningTests.swift`, `SessionEngineTests.swift`,
//! `SessionConcurrencyTests.swift` and the engine test of `TopPeaksTests.swift`.

use super::*;
use crate::contracts::{
    BandEnergy, HeadphoneModeling, HeadphoneReading, SessionEventKind, SessionSnapshot, SplEstimating, SplReading,
    StressFlag, StressSeverity, ThirdOctaveReading,
};
use crate::ring_buffer::{stereo_ring_buffer, DEFAULT_CAPACITY};
use crate::session::tests::{spl_reading, ManualClock, TestFrame};
use crate::test_signals;
use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

const RATE: f64 = 48_000.0;

fn sine(hz: f64, seconds: f64) -> Vec<f32> {
    test_signals::sine(hz, 0.5, RATE, seconds, 0.0)
}

fn wait(seconds: f64) {
    std::thread::sleep(Duration::from_secs_f64(seconds));
}

fn ring() -> (crate::ring_buffer::RingWriter, RingReader) {
    stereo_ring_buffer(DEFAULT_CAPACITY)
}

fn bin_at(frame: &AnalysisFrame, hz: f32) -> usize {
    frame.spectrum.frequencies.iter().position(|&f| f >= hz).unwrap_or(0)
}

/// What a `CountingStereo` saw.
#[derive(Default)]
struct Counters {
    calls: AtomicUsize,
    samples: AtomicUsize,
    saw_non_finite: AtomicBool,
}

impl Counters {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }

    fn samples(&self) -> usize {
        self.samples.load(Ordering::Relaxed)
    }
}

/// Counts `process` calls and samples; passes everything on to a real stereo analyzer.
struct CountingStereo {
    inner: StereoAnalyzer,
    counters: Arc<Counters>,
}

impl StereoAnalyzing for CountingStereo {
    fn scope_point_count(&self) -> usize {
        self.inner.scope_point_count()
    }
    fn set_scope_point_count(&mut self, count: usize) {
        self.inner.set_scope_point_count(count);
    }
    fn process(&mut self, left: &[f32], right: &[f32], sample_rate: f64) {
        self.counters.calls.fetch_add(1, Ordering::Relaxed);
        self.counters.samples.fetch_add(left.len(), Ordering::Relaxed);
        if left.iter().chain(right).any(|sample| !sample.is_finite()) {
            self.counters.saw_non_finite.store(true, Ordering::Relaxed);
        }
        self.inner.process(left, right, sample_rate);
    }
    fn read(&mut self) -> StereoReading {
        self.inner.read()
    }
    fn reset(&mut self) {
        self.inner.reset();
    }
}

/// An engine whose stereo analyzer counts its calls.
fn counting_engine() -> (AnalysisEngine, Arc<Counters>) {
    let counters = Arc::new(Counters::default());
    let stereo = CountingStereo {
        inner: StereoAnalyzer::new(),
        counters: Arc::clone(&counters),
    };
    (
        AnalysisEngine::with_parts(EngineParts {
            stereo: Box::new(stereo),
            ..Default::default()
        }),
        counters,
    )
}

// MARK: Settings, reset, models

#[test]
fn settings_update_applies_before_the_next_analysis() {
    let engine = AnalysisEngine::new();
    let settings = SpectrumSettings {
        display_bins: 512,
        ..Default::default()
    };
    engine.update_spectrum_settings(settings.clone());
    let tone = sine(1_000.0, 0.2);
    let frame = engine.process_now(&tone, &tone, RATE);
    assert_eq!(frame.spectrum.frequencies.len(), 512);
    assert_eq!(engine.spectrum_settings(), settings);
}

struct CountingModel {
    resets: usize,
}

impl HeadphoneModeling for CountingModel {
    fn model_name(&self) -> &str {
        "counting"
    }
    fn evaluate(&mut self, _: &SpectrumReading, _: &BandEnergy, _: &LoudnessReading) -> HeadphoneReading {
        HeadphoneReading {
            model_name: "counting".into(),
            response_db: Vec::new(),
            target_db: Vec::new(),
            has_target: true,
            predicted_at_ear_db: Vec::new(),
            stress_flags: Vec::new(),
        }
    }
    fn reset(&mut self) {
        self.resets += 1;
    }
}

#[test]
fn reset_measurement_resets_the_headphone_model() {
    let engine = AnalysisEngine::new();
    let model = Arc::new(Mutex::new(CountingModel { resets: 0 }));
    engine.set_headphone_model(Some(model.clone()));
    let tone = sine(1_000.0, 0.1);
    let frame = engine.process_now(&tone, &tone, RATE);
    assert_eq!(
        frame.headphone.as_ref().map(|h| h.model_name.as_str()),
        Some("counting")
    );
    assert_eq!(model.lock().unwrap().resets, 0);
    engine.reset_measurement();
    engine.process_now(&tone, &tone, RATE);
    assert_eq!(model.lock().unwrap().resets, 1);
    engine.process_now(&tone, &tone, RATE);
    assert_eq!(model.lock().unwrap().resets, 1);
    engine.set_headphone_model(None);
    assert!(engine.process_now(&tone, &tone, RATE).headphone.is_none());
}

/// The engine copies the top peaks and the lowest strong frequency into every frame.
#[test]
fn the_engine_copies_top_peaks_into_the_frame() {
    let engine = AnalysisEngine::new();
    let mut signal = vec![0.0f32; (RATE * 5.0) as usize];
    for (hz, amplitude) in [(220.0, 0.5), (440.0, 0.3), (660.0, 0.2)] {
        for (sum, tone) in signal.iter_mut().zip(test_signals::sine(hz, amplitude, RATE, 5.0, 0.0)) {
            *sum += tone;
        }
    }
    let mut frame = engine.latest_frame();
    for block in signal.chunks(800) {
        frame = engine.process_now(block, block, RATE);
    }
    assert!(!frame.top_peaks.is_empty());
    assert_eq!(frame.top_peaks[0].frequency_hz, frame.peak.frequency_hz);
    assert!(frame.lowest_strong_hz > 0.0);
    assert!(frame.third_octave.is_some());
    assert!(frame.host_time > 0.0);
}

// MARK: The tick

/// Waits until `condition` holds for the newest frame, up to `seconds`. Returns that frame.
fn wait_for(engine: &AnalysisEngine, seconds: f64, condition: impl Fn(&AnalysisFrame) -> bool) -> Arc<AnalysisFrame> {
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    loop {
        let frame = engine.latest_frame();
        if condition(&frame) || Instant::now() >= end {
            return frame;
        }
        wait(0.005);
    }
}

/// A source that stops delivering samples must not leave the last spectrum and meters on screen.
#[test]
fn stalled_input_goes_silent_and_falls() {
    let engine = AnalysisEngine::new();
    let (mut writer, reader) = ring();
    let tone = sine(1_000.0, 1.0);
    assert!(engine.start(reader, 60.0).is_none());
    writer.write(&tone, &tone, RATE);
    // The Swift test waits a fixed 0.15 s. A debug build on a busy machine can
    // need longer to analyze the first second, so this waits for the frame.
    let live = wait_for(&engine, 10.0, |frame| {
        !frame.is_silent && frame.loudness.rms_left_db > -12.0
    });
    assert!(!live.is_silent);
    assert!(live.loudness.rms_left_db > -12.0);
    // No more writes: stale after 0.25 s, then zeros flow and everything falls.
    let stale = wait_for(&engine, 20.0, |frame| {
        frame.is_silent && frame.loudness.momentary_lufs < -60.0
    });
    assert!(engine.stop().is_some(), "stop gives the reader back");
    assert!(stale.is_silent);
    assert!(stale.host_time > live.host_time);
    let bin = bin_at(&stale, 1_000.0);
    assert!(live.spectrum.mid[bin] > -12.0);
    assert!(
        stale.spectrum.mid[bin] < live.spectrum.mid[bin] - 20.0,
        "the spectrum must release"
    );
    assert!(stale.loudness.rms_left_db < -60.0, "the RMS meter must fall");
    assert!(stale.loudness.momentary_lufs < -60.0, "momentary loudness must fall");
}

/// How many new frames the engine publishes in `seconds`.
/// A shared runner gives the tick thread less time than the development machine: `JOSEON_TIMING_SLACK` is a
/// factor on the lower bounds of the counts (1 = none).
fn slack() -> f64 {
    std::env::var("JOSEON_TIMING_SLACK")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(1.0)
}

fn frames_in(engine: &AnalysisEngine, seconds: f64) -> usize {
    let mut seen = HashSet::new();
    let end = Instant::now() + Duration::from_secs_f64(seconds);
    while Instant::now() < end {
        seen.insert(engine.latest_frame().host_time.to_bits());
        wait(0.001);
    }
    seen.len()
}

#[test]
fn set_tick_rate_changes_the_rate_without_restart() {
    let engine = AnalysisEngine::new();
    let (_writer, reader) = ring();
    engine.start(reader, 100.0);
    // An empty ring gives no new frame until the input counts as stale (0.25 s); then every tick publishes.
    wait(STALE_AFTER_SECONDS + 0.1);
    let fast = frames_in(&engine, 0.5);
    engine.set_tick_rate(10.0);
    wait(0.15);
    let slow = frames_in(&engine, 0.5);
    engine.set_tick_rate(100.0);
    wait(0.15);
    let fast_again = frames_in(&engine, 0.5);
    engine.stop();
    let least = (25.0 / slack()) as usize;
    assert!(fast > least, "{fast} frames at 100 ticks per second");
    assert!(
        slow < 12 && slow > if slack() > 1.0 { 0 } else { 2 },
        "{slow} frames at 10 ticks per second"
    );
    assert!(fast_again > least, "{fast_again} frames at 100 ticks per second again");
}

// MARK: Thread lifetime

#[test]
fn drop_while_running_does_not_hang() {
    for _ in 0..20 {
        let (_writer, reader) = ring();
        let engine = AnalysisEngine::new();
        engine.start(reader, 500.0);
        wait(0.003);
        drop(engine);
    }
}

#[test]
fn stop_start_and_rate_changes_in_any_order() {
    let engine = AnalysisEngine::new();
    let (_writer_a, reader_a) = ring();
    let (_writer_b, reader_b) = ring();
    assert!(engine.stop().is_none());
    engine.set_tick_rate(30.0); // stopped: no effect
    assert!(!engine.is_running());
    assert!(engine.start(reader_a, 200.0).is_none());
    let reader_a = engine
        .start(reader_b, 200.0)
        .expect("a restart gives the old reader back");
    engine.set_tick_rate(10.0);
    assert!(engine.is_running());
    let reader_b = engine.stop().expect("the reader comes back");
    assert!(engine.stop().is_none());
    engine.set_tick_rate(60.0);
    engine.start(reader_a, 100.0);
    assert!(engine.stop().is_some());
    drop(reader_b);
}

/// `start` swaps the ring while ticks run, from three threads at once.
#[test]
fn concurrent_start_stop_while_ticking() {
    let engine = AnalysisEngine::new();
    let tone = sine(440.0, 0.05);
    std::thread::scope(|scope| {
        for worker in 0..3 {
            let (engine, tone) = (&engine, &tone);
            scope.spawn(move || {
                let (mut writer, reader) = ring();
                let mut reader = Some(reader);
                for i in 0..60 {
                    // Each worker passes on the reader it gets back, or makes a new ring.
                    let next = reader.take().unwrap_or_else(|| ring().1);
                    reader = engine.start(next, 1000.0);
                    if worker == 0 {
                        writer.write(tone, tone, RATE);
                    }
                    if i % 4 == worker {
                        reader = engine.stop().or(reader);
                    }
                    engine.set_tick_rate(100.0 + i as f64);
                    engine.latest_frame();
                }
            });
        }
    });
    engine.stop();
    assert!(!engine.is_running());
}

// MARK: is_silent

/// Several chunks in one tick: signal first, a silent chunk last. The frame is not silent.
#[test]
fn is_silent_covers_every_chunk_of_a_tick() {
    let engine = AnalysisEngine::new();
    let (mut writer, reader) = ring();
    // More than one engine scratch block (32768 frames): 0.5 s of tone, then 0.4 s of silence.
    let tone = sine(1_000.0, 0.5);
    writer.write(&tone, &tone, RATE);
    writer.write(&vec![0.0; 19_200], &vec![0.0; 19_200], RATE);
    engine.start(reader, 1.0); // first tick at once, the next after 1 s
    wait(0.2);
    let frame = engine.latest_frame();
    engine.stop();
    assert!(frame.host_time > 0.0);
    assert!(
        !frame.is_silent,
        "signal in an earlier chunk of the tick: the frame is not silent"
    );
}

#[test]
fn process_now_silence_flag() {
    let engine = AnalysisEngine::new();
    let tone = sine(1_000.0, 0.05);
    let zeros = vec![0.0; 2400];
    assert!(!engine.process_now(&tone, &tone, RATE).is_silent);
    assert!(engine.process_now(&zeros, &zeros, RATE).is_silent);
    assert!(!engine.process_now(&zeros, &tone, RATE).is_silent);
}

/// A tick with no new samples keeps the frame: consumers see the same
/// `host_time` and skip their work.
#[test]
fn no_new_samples_keeps_the_frame() {
    let engine = AnalysisEngine::new();
    let (mut writer, reader) = ring();
    let tone = sine(1_000.0, 0.05);
    writer.write(&tone, &tone, RATE);
    let started = Instant::now();
    engine.start(reader, 200.0);
    wait(0.03);
    let first = engine.latest_frame();
    wait(0.08); // about 16 ticks, no samples, not yet stale
    let second = engine.latest_frame();
    let info = StreamInfo::new(RATE, 2, "Test device");
    engine.set_stream_info(Some(info.clone()));
    wait(0.04);
    let third = engine.latest_frame();
    engine.stop();
    // On a busy machine the waits can run long; after 0.25 s the input counts
    // as stale and zeros flow. Then this run says nothing.
    if started.elapsed().as_secs_f64() > STALE_AFTER_SECONDS - 0.02 {
        println!("too slow to test");
        return;
    }
    assert!(first.host_time > 0.0);
    assert_eq!(first.host_time, second.host_time);
    assert_eq!(third.host_time, first.host_time);
    assert_eq!(
        third.stream,
        Some(info),
        "stream facts reach the frame also without new audio"
    );
}

/// Analysis slower than real time must still end its tick and publish frames.
#[test]
fn slow_analysis_still_publishes() {
    struct SlowStereo;
    impl StereoAnalyzing for SlowStereo {
        fn scope_point_count(&self) -> usize {
            0
        }
        fn set_scope_point_count(&mut self, _: usize) {}
        fn process(&mut self, _: &[f32], _: &[f32], _: f64) {
            wait(0.03);
        }
        fn read(&mut self) -> StereoReading {
            StereoReading::default()
        }
        fn reset(&mut self) {}
    }
    let engine = AnalysisEngine::with_parts(EngineParts {
        stereo: Box::new(SlowStereo),
        ..Default::default()
    });
    let (mut writer, reader) = ring();
    let tone = sine(1_000.0, 0.01);
    let stop = Arc::new(AtomicBool::new(false));
    let writer_stop = Arc::clone(&stop);
    let writer_thread = std::thread::spawn(move || {
        while !writer_stop.load(Ordering::Relaxed) {
            writer.write(&tone, &tone, RATE);
            wait(0.005);
        }
    });
    engine.start(reader, 100.0);
    wait(0.4);
    let first = engine.latest_frame().host_time;
    wait(0.4);
    let second = engine.latest_frame().host_time;
    engine.stop();
    stop.store(true, Ordering::Relaxed);
    writer_thread.join().unwrap();
    assert!(first > 0.0, "the first tick must end and publish");
    assert!(second > first, "later ticks publish too");
}

/// A stalled source feeds zeros, but never more than 1.5 ticks of them per tick.
#[test]
fn stale_feed_is_capped_per_tick() {
    let (engine, counters) = counting_engine();
    let (_writer, reader) = ring();
    engine.start(reader, 50.0);
    wait(1.25);
    engine.stop();
    // About 1 s stale at 48 kHz. Real time is the upper bound (plus scheduling slack).
    let (samples, calls) = (counters.samples(), counters.calls());
    let least = (24_000.0 / slack()) as usize;
    assert!(samples > least && samples < 60_000, "{samples} samples of zeros");
    assert!(
        samples / calls.max(1) <= (RATE * 0.02 * 1.5) as usize + 1,
        "{samples} samples in {calls} calls"
    );
}

// MARK: Non-finite input

#[test]
fn non_finite_samples_never_reach_the_analyzers() {
    let (engine, counters) = counting_engine();
    let mut left = sine(1_000.0, 0.2);
    let mut right = left.clone();
    left[100] = f32::NAN;
    right[2000] = f32::INFINITY;
    left[3000] = f32::NEG_INFINITY;
    let frame = engine.process_now(&left, &right, RATE);
    assert_eq!(engine.sanitized_block_count(), 1);
    assert!(!counters.saw_non_finite.load(Ordering::Relaxed));
    assert!(!frame.is_silent);

    // The next clean blocks read like a clean run: nothing is poisoned.
    let clean = sine(1_000.0, 1.0);
    let frame = engine.process_now(&clean, &clean, RATE);
    assert_eq!(engine.sanitized_block_count(), 1);
    assert!((frame.stereo.correlation - 1.0).abs() <= 0.01);
    assert!(frame.loudness.momentary_lufs.is_finite() && frame.loudness.momentary_lufs > -20.0);
    assert!(frame.loudness.true_peak_max_dbtp.is_finite() && frame.loudness.true_peak_max_dbtp < 1.0);
    assert!(frame.spectrum.mid.iter().all(|level| level.is_finite()));
    assert!(frame.spectrum.mid[bin_at(&frame, 1_000.0)] > -12.0);
}

#[test]
fn a_block_of_only_nan_counts_as_silence() {
    let engine = AnalysisEngine::new();
    let bad = vec![f32::NAN; 4800];
    let frame = engine.process_now(&bad, &bad, RATE);
    assert!(frame.is_silent);
    assert_eq!(frame.stereo.correlation, 0.0);
    assert!(frame.loudness.momentary_lufs <= -100.0);
    // A bad sample rate or an empty block changes nothing and still gives a frame.
    let tone = sine(1_000.0, 0.1);
    for rate in [0.0, f64::NAN, f64::INFINITY, -1.0] {
        assert!(engine.process_now(&tone, &tone, rate).is_silent);
    }
    assert!(engine.process_now(&[], &[], RATE).is_silent);
}

// MARK: Stereo on demand

#[test]
fn stereo_analysis_can_pause_and_restarts_from_a_reset() {
    let (engine, counters) = counting_engine();
    let tone = sine(1_000.0, 0.5);
    let inverted: Vec<f32> = tone.iter().map(|x| -x).collect();
    assert!((engine.process_now(&tone, &inverted, RATE).stereo.correlation + 1.0).abs() <= 0.02);
    engine.set_analyzes_stereo(false);
    assert!(!engine.analyzes_stereo());
    let paused = engine.process_now(&tone, &inverted, RATE);
    assert_eq!(counters.calls(), 1, "paused: the stereo analyzer does not run");
    assert_eq!(paused.stereo, StereoReading::default());
    assert!(
        paused.loudness.momentary_lufs > -20.0,
        "the other analyzers keep running"
    );
    engine.set_analyzes_stereo(true);
    let resumed = engine.process_now(&tone, &tone, RATE);
    assert_eq!(counters.calls(), 2);
    assert!(
        (resumed.stereo.correlation - 1.0).abs() <= 0.02,
        "no anti-phase state left from before the pause"
    );
}

// MARK: Sleep on silence

/// A fast peak-hold decay, so the frame reaches its rest state quickly.
fn fast_decay(engine: &AnalysisEngine) {
    engine.set_sleeps_in_process_now(true);
    engine.update_spectrum_settings(SpectrumSettings {
        peak_decay_db_per_second: 48.0,
        ..Default::default()
    });
}

/// After the decays end, zeros do not go through the analyzers. Signal wakes the engine at once.
#[test]
fn engine_sleeps_on_long_silence_and_wakes_on_signal() {
    let (engine, counters) = counting_engine();
    fast_decay(&engine);
    let tone = sine(1_000.0, 0.5);
    let zeros = vec![0.0; 4800];
    engine.process_now(&tone, &tone, RATE);
    let mut seconds_to_sleep = None;
    for block in 0..600 {
        // up to 60 s of silence
        assert!(engine.process_now(&zeros, &zeros, RATE).is_silent);
        if engine.is_asleep() {
            seconds_to_sleep = Some((block + 1) as f64 * 0.1);
            break;
        }
    }
    let seconds_to_sleep = seconds_to_sleep.expect("the engine never went to sleep");
    assert!(
        (SLEEP_AFTER_SILENT_SECONDS..30.0).contains(&seconds_to_sleep),
        "asleep after {seconds_to_sleep} s"
    );

    let calls_at_sleep = counters.calls();
    let mut asleep_frame = engine.latest_frame();
    for _ in 0..50 {
        asleep_frame = engine.process_now(&zeros, &zeros, RATE);
    }
    assert_eq!(
        counters.calls(),
        calls_at_sleep,
        "asleep: zeros do not reach the analyzers"
    );
    assert!(asleep_frame.is_silent && is_at_rest(&asleep_frame));

    let awake = engine.process_now(&tone, &tone, RATE);
    assert!(!engine.is_asleep() && !awake.is_silent);
    assert_eq!(counters.calls(), calls_at_sleep + 1);
    assert!(awake.spectrum.mid[bin_at(&awake, 1_000.0)] > -12.0);

    // A reset wakes it too (the frame must show the reset), then it sleeps again.
    for _ in 0..600 {
        if engine.is_asleep() {
            break;
        }
        engine.process_now(&zeros, &zeros, RATE);
    }
    assert!(engine.is_asleep());
    engine.reset_measurement();
    let calls_before_reset = counters.calls();
    engine.process_now(&zeros, &zeros, RATE);
    assert!(counters.calls() > calls_before_reset);
}

#[test]
fn process_now_never_sleeps_by_default() {
    let (engine, counters) = counting_engine();
    let zeros = vec![0.0; 48_000];
    for _ in 0..30 {
        engine.process_now(&zeros, &zeros, RATE);
    }
    assert!(!engine.is_asleep());
    assert_eq!(counters.calls(), 30);
}

#[test]
fn a_frame_with_a_stress_flag_or_a_level_is_not_at_rest() {
    let rest = TestFrame {
        is_silent: true,
        ..Default::default()
    }
    .frame();
    assert!(is_at_rest(&rest));
    let flag = StressFlag {
        id: "x".into(),
        severity: StressSeverity::Info,
        title: "X".into(),
        detail: String::new(),
        frequency_range_hz: None,
        plot_label: String::new(),
    };
    assert!(!is_at_rest(
        &TestFrame {
            is_silent: true,
            flags: vec![flag],
            ..Default::default()
        }
        .frame()
    ));
    assert!(!is_at_rest(
        &TestFrame {
            is_silent: false,
            ..Default::default()
        }
        .frame()
    ));
    assert!(!is_at_rest(
        &TestFrame {
            is_silent: true,
            momentary: -40.0,
            ..Default::default()
        }
        .frame()
    ));
    let mut lit = rest.clone();
    lit.spectrum.peak_hold[3] = -60.0;
    assert!(!is_at_rest(&lit));
    let mut active = rest;
    active.stereo.band_active[2] = true;
    assert!(!is_at_rest(&active));
}

// MARK: The engine end of the session recorder

const BLOCK: usize = 800; // one 60 Hz tick

fn demo(tick: usize) -> (Vec<f32>, Vec<f32>) {
    test_signals::demo_block((tick * BLOCK) as i64, BLOCK, RATE)
}

/// Twelve seconds of the demo signal make about twelve samples, with sane numbers in them.
#[test]
fn twelve_seconds_of_demo_audio() {
    let engine = AnalysisEngine::new();
    for tick in 0..12 * 60 {
        let (left, right) = demo(tick);
        engine.process_now(&left, &right, RATE);
    }
    let snapshot = engine.session().snapshot();
    assert_eq!(engine.session().capacity_seconds(), 1800, "30 minutes by default");
    assert!(
        (11..=13).contains(&snapshot.samples.len()),
        "about one sample per second of audio"
    );
    let last = snapshot.samples.last().unwrap();
    assert!(
        (last.time - snapshot.samples.len() as f64).abs() <= 0.05,
        "audio time, one second per sample"
    );
    // The demo signal is music-like: loud, mostly in phase, never clipped, never silent.
    for sample in &snapshot.samples[1..] {
        assert!(sample.momentary_max_lufs > -30.0 && sample.momentary_max_lufs < 0.0);
        assert!(sample.short_term_lufs > -40.0);
        assert!(sample.true_peak_dbtp < 0.0 && sample.true_peak_dbtp > -20.0);
        assert!(sample.correlation > 0.2 && sample.correlation <= 1.0);
        assert!(sample.bands[1] > -60.0, "the demo signal has bass");
        assert!(sample.bands.iter().all(|band| band.is_finite() && *band < 6.0));
        assert_eq!(sample.level_a, None, "no SPL estimator on this engine");
        assert!(!sample.is_silent);
    }
    assert!(
        !snapshot
            .events
            .iter()
            .any(|e| matches!(e.kind, SessionEventKind::Clip | SessionEventKind::InterSampleOver)),
        "the demo signal has no clips and no overs"
    );

    // A measurement reset is a new track, and the event reaches the record with the next frame.
    engine.reset_measurement();
    let (left, right) = demo(12 * 60);
    engine.process_now(&left, &right, RATE);
    let after = engine.session().snapshot();
    assert_eq!(after.events.len(), snapshot.events.len() + 1);
    let event = after.events.last().unwrap();
    assert_eq!(event.kind, SessionEventKind::TrackStart);
    assert!(
        (event.time - last.time).abs() <= 1.1,
        "at the point of the reset on the audio axis"
    );
    assert!(after.revision > snapshot.revision);
}

fn seconds_of(date: std::time::SystemTime) -> f64 {
    date.duration_since(std::time::SystemTime::UNIX_EPOCH)
        .unwrap()
        .as_secs_f64()
}

/// The engine stops analyzing after seconds of digital silence. The record must
/// still say "silence here", and the sleep must not stretch the audio axis.
#[test]
fn silence_events_survive_the_sleep_path() {
    let clock = ManualClock::default();
    let recorder = Arc::new(clock.recorder(600));
    let engine = AnalysisEngine::with_parts(EngineParts {
        session: recorder.clone(),
        ..Default::default()
    });
    fast_decay(&engine);
    let tone = sine(1_000.0, 0.1);
    let zeros = vec![0.0; 4_800];
    for _ in 0..10 {
        // 1 s of signal
        clock.advance(0.1);
        engine.process_now(&tone, &tone, RATE);
    }
    let mut silent_blocks = 0;
    while !engine.is_asleep() && silent_blocks < 600 {
        clock.advance(0.1);
        engine.process_now(&zeros, &zeros, RATE);
        silent_blocks += 1;
    }
    assert!(engine.is_asleep(), "the engine must reach the sleep path for this test");
    let audio_time_at_sleep = recorder.snapshot().samples.last().unwrap().time;
    for _ in 0..100 {
        // 10 s of wall time asleep: no analysis, no audio time
        clock.advance(0.1);
        engine.process_now(&zeros, &zeros, RATE);
    }
    let asleep = recorder.snapshot();
    assert_eq!(
        asleep.samples.last().unwrap().time,
        audio_time_at_sleep,
        "the sleep does not stretch the axis"
    );
    let starts: Vec<_> = asleep
        .events
        .iter()
        .filter(|e| e.kind == SessionEventKind::SilenceStart)
        .collect();
    assert_eq!(starts.len(), 1);
    assert!(
        (starts[0].time - 1.0).abs() <= 0.2,
        "silence began when the tone stopped"
    );
    assert!((seconds_of(starts[0].date) - 1.0).abs() <= 0.2);

    clock.advance(0.1);
    engine.process_now(&tone, &tone, RATE);
    let awake = recorder.snapshot();
    let ends: Vec<_> = awake
        .events
        .iter()
        .filter(|e| e.kind == SessionEventKind::SilenceEnd)
        .collect();
    assert_eq!(ends.len(), 1);
    assert!(
        (ends[0].time - audio_time_at_sleep).abs() <= 1.1,
        "the axis carried on where it stopped"
    );
    let wall = seconds_of(ends[0].date);
    assert!(
        (wall - (1.0 + silent_blocks as f64 * 0.1 + 10.1)).abs() <= 0.2,
        "the wall-clock date of the wake-up"
    );
    assert!(
        ends[0].time < wall - 9.0,
        "ten seconds of wall time are not ten seconds of the timeline"
    );
}

/// The recorder and the SPL estimator get the same audio `dt`: the level at the ear reaches the samples.
#[test]
fn level_at_the_ear_reaches_the_samples() {
    struct FixedSpl {
        seconds: f64,
    }
    impl SplEstimating for FixedSpl {
        fn evaluate(&mut self, _: &ThirdOctaveReading, dt: f64, _: bool) -> SplReading {
            self.seconds += dt;
            spl_reading(83.0)
        }
        fn reset_measurement(&mut self) {}
        fn reset_dose(&mut self) {}
    }
    let estimator = Arc::new(Mutex::new(FixedSpl { seconds: 0.0 }));
    let engine = AnalysisEngine::new();
    engine.set_spl_estimator(Some(estimator.clone()));
    for tick in 0..3 * 60 {
        let (left, right) = demo(tick);
        engine.process_now(&left, &right, RATE);
    }
    // The estimator starts when the spectrum analyzer can give third-octave
    // bands (a short warm-up), so it sees a little less than the 3 s of the axis.
    let seconds = estimator.lock().unwrap().seconds;
    assert!(
        seconds > 2.0 && seconds <= 3.001,
        "audio seconds, never more: {seconds}"
    );
    let samples = engine.session().snapshot().samples;
    assert!((2..=3).contains(&samples.len()));
    for sample in samples {
        assert!((sample.level_a.unwrap() - 83.0).abs() <= 0.001);
    }
}

// MARK: The recorder under threads

/// Everything a panel can rely on in one snapshot.
#[track_caller]
fn assert_consistent(snapshot: &SessionSnapshot, capacity: usize) {
    assert!(snapshot.samples.len() <= capacity);
    let mut previous = f64::NEG_INFINITY;
    for sample in &snapshot.samples {
        assert!(sample.time > previous, "samples are oldest first");
        previous = sample.time;
        assert!(sample.bands.iter().all(|band| band.is_finite()));
        assert!(sample.momentary_max_lufs.is_finite() && sample.correlation.is_finite());
    }
    let mut previous = f64::NEG_INFINITY;
    let mut ids = HashSet::new();
    for event in &snapshot.events {
        assert!(event.time >= previous, "events are oldest first");
        previous = event.time;
        assert!(ids.insert(event.id), "event ids are unique");
        assert!(event.id > 0 && event.value.is_finite());
    }
    if snapshot.samples.len() == capacity {
        let oldest = snapshot.samples[0].time;
        assert!(
            snapshot.events.iter().all(|event| event.time >= oldest - 1.0001),
            "no event older than the record"
        );
    }
}

/// One writer as fast as it can go, two readers, for a second of wall time.
#[test]
fn snapshot_is_consistent_under_a_concurrent_reader() {
    let capacity = 30;
    let recorder = SessionRecorder::new(capacity);
    let stop = Instant::now() + Duration::from_secs(1);
    let reads = AtomicUsize::new(0);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let flag = StressFlag {
                id: "load".into(),
                severity: StressSeverity::Watch,
                title: "Load".into(),
                detail: String::new(),
                frequency_range_hz: None,
                plot_label: String::new(),
            };
            let mut n = 0u64;
            while Instant::now() < stop {
                n += 1;
                let frame = TestFrame {
                    momentary: -40.0 + (n % 20) as f32,
                    short_term: -30.0 + (n % 10) as f32,
                    true_peak_left: if n.is_multiple_of(97) { 1.2 } else { -4.0 },
                    true_peak_right: -5.0,
                    clip_count: n / 50,
                    correlation: (n % 3) as f32 / 2.0 - 0.5,
                    bands_db: -60.0 + (n % 40) as f32,
                    level_a: Some(70.0 + (n % 10) as f32),
                    flags: if (n / 37).is_multiple_of(2) {
                        vec![flag.clone()]
                    } else {
                        Vec::new()
                    },
                    is_silent: (n / 211).is_multiple_of(3),
                };
                recorder.ingest(&frame.frame(), 0.1);
                if n.is_multiple_of(997) {
                    recorder.note_track_start();
                }
            }
        });
        for _ in 0..2 {
            scope.spawn(|| {
                let mut last_revision = 0;
                while Instant::now() < stop {
                    let snapshot = recorder.snapshot();
                    assert_consistent(&snapshot, capacity);
                    assert!(snapshot.revision >= last_revision, "the revision never falls");
                    last_revision = snapshot.revision;
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            });
        }
    });
    let last = recorder.snapshot();
    assert_consistent(&last, capacity);
    assert_eq!(last.samples.len(), capacity, "the ring filled during the run");
    assert!(last.revision > 0);
    assert!(reads.load(Ordering::Relaxed) > 100, "the readers really ran");
}

/// `clear` from another thread while the analysis thread feeds: no torn record.
#[test]
fn clear_racing_with_ingest() {
    let recorder = SessionRecorder::new(8);
    let stop = Instant::now() + Duration::from_millis(400);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let frame = TestFrame {
                momentary: -18.0,
                clip_count: 1,
                ..Default::default()
            }
            .frame();
            while Instant::now() < stop {
                recorder.ingest(&frame, 0.3);
            }
        });
        scope.spawn(|| {
            while Instant::now() < stop {
                recorder.clear();
                assert_consistent(&recorder.snapshot(), 8);
                wait(0.0005);
            }
        });
    });
    assert_consistent(&recorder.snapshot(), 8);
}
