//! Cost of the analyzers: time budgets and "no allocation in `process`".
//!
//! Port of `SpectrumPerformanceTests.swift`, of the allocation tests in
//! `ThirdOctaveTests.swift` and `TopPeaksTests.swift`, and of the time budget
//! in `LoudnessMeterTests.swift` and `StereoAnalyzerTests.swift`, and of
//! `SessionPerformanceTests.swift`.
//!
//! The time budgets are the budgets of the macOS app, measured on Apple
//! silicon. They are checked in release builds only. Each measurement is the
//! fastest of five runs, and the timing tests run one at a time. On a slower machine set
//! `JOSEON_TIMING_SLACK` to a factor, for example `4`.
//!
//! This file is its own test binary because it replaces the global allocator
//! with one that counts.

use joseon_core::contracts::{
    AnalysisFrame, BandEnergy, HeadphoneReading, LoudnessMetering, SessionRecording, SpectrumAnalyzing,
    SpectrumReading, SplReading, StereoAnalyzing, StressFlag, StressSeverity,
};
use joseon_core::loudness::LoudnessMeter;
use joseon_core::session::{SessionRecorder, MAX_EVENTS};
use joseon_core::spectrum::SpectrumAnalyzer;
use joseon_core::stereo::StereoAnalyzer;
use joseon_core::test_signals::{demo_block, pink_noise, sine, DEFAULT_SEED};
use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Counts the allocations of the calling thread.
struct CountingAllocator;

thread_local! {
    static ALLOCATIONS: Cell<u64> = const { Cell::new(0) };
}

unsafe impl GlobalAlloc for CountingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        // SAFETY: the caller upholds the contract of `GlobalAlloc::alloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn dealloc(&self, pointer: *mut u8, layout: Layout) {
        // SAFETY: `pointer` came from `System.alloc` with this layout.
        unsafe { System.dealloc(pointer, layout) }
    }

    unsafe fn realloc(&self, pointer: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        let _ = ALLOCATIONS.try_with(|count| count.set(count.get() + 1));
        // SAFETY: `pointer` came from `System.alloc` with this layout.
        unsafe { System.realloc(pointer, layout, new_size) }
    }
}

#[global_allocator]
static ALLOCATOR: CountingAllocator = CountingAllocator;

fn allocations() -> u64 {
    ALLOCATIONS.with(Cell::get)
}

const RATE: f64 = 48_000.0;
const BLOCK: usize = 800;

/// Timing tests run one at a time: two at once would time each other.
static TIMING: Mutex<()> = Mutex::new(());

/// Runs `measure` five times and keeps the fastest value of each number. Other
/// work on the machine can only make a run slower, so the fastest run is the
/// one that says what the code costs.
fn fastest<const N: usize>(mut measure: impl FnMut() -> [f64; N]) -> [f64; N] {
    let _one_at_a_time = TIMING.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let mut best = measure();
    // A debug build checks no budget, so one run is enough there.
    let more = if cfg!(debug_assertions) { 0 } else { 4 };
    for _ in 0..more {
        let run = measure();
        for (best, run) in best.iter_mut().zip(run) {
            *best = best.min(run);
        }
    }
    best
}

/// Microseconds allowed, with the slack factor of this machine.
fn budget(microseconds: f64) -> f64 {
    let slack = std::env::var("JOSEON_TIMING_SLACK")
        .ok()
        .and_then(|text| text.parse::<f64>().ok())
        .unwrap_or(1.0);
    microseconds * slack.max(1.0)
}

/// An analyzer that has filled its history, built every FFT and taken every first-use path.
fn warm_analyzer() -> (SpectrumAnalyzer, Vec<f32>) {
    let mut analyzer = SpectrumAnalyzer::default();
    let signal = pink_noise(0.5, 1 << 16, DEFAULT_SEED);
    for block in signal.chunks_exact(BLOCK) {
        analyzer.process(block, block, RATE);
    }
    analyzer.read();
    (analyzer, signal)
}

/// Calls `process` `calls` times on blocks of `signal`, in a loop over the signal.
fn process_loop(analyzer: &mut SpectrumAnalyzer, signal: &[f32], calls: usize) {
    let mut offset = 0;
    for _ in 0..calls {
        let block = &signal[offset..offset + BLOCK];
        analyzer.process(block, block, RATE);
        offset += BLOCK;
        if offset + BLOCK > signal.len() {
            offset = 0;
        }
    }
}

/// Budget: `process` of 800 frames averages under 1 ms. 800 frames at 48 kHz is
/// one 60 Hz tick, which is how the engine drives the analyzer.
#[test]
fn process_budget() {
    let (mut analyzer, signal) = warm_analyzer();
    let iterations = 2_000;
    let [microseconds] = fastest(|| {
        let start = Instant::now();
        process_loop(&mut analyzer, &signal, iterations);
        [start.elapsed().as_secs_f64() / iterations as f64 * 1e6]
    });
    println!("process(800 frames @ 48 kHz): {microseconds:.1} us average over {iterations} calls");
    if !cfg!(debug_assertions) {
        assert!(
            microseconds < budget(1_000.0),
            "process of 800 frames must average under 1 ms: {microseconds} us"
        );
    }
}

/// The same on the demo signal, which has a dozen tones to find, measure and
/// draw in every transform, with a `read` per block the way the engine drives it.
#[test]
fn process_budget_on_the_demo_signal() {
    let (left, right) = demo_block(0, RATE as usize * 12, RATE);
    let [process_micros, read_micros] = fastest(|| {
        let mut analyzer = SpectrumAnalyzer::default();
        let (mut process_time, mut read_time, mut calls) = (Duration::ZERO, Duration::ZERO, 0u32);
        for (index, (l, r)) in left.chunks_exact(BLOCK).zip(right.chunks_exact(BLOCK)).enumerate() {
            let t0 = Instant::now();
            analyzer.process(l, r, RATE);
            let t1 = Instant::now();
            std::hint::black_box(analyzer.read());
            let t2 = Instant::now();
            // The first two seconds fill the history and take every first-use path.
            if index * BLOCK >= RATE as usize * 2 {
                process_time += t1 - t0;
                read_time += t2 - t1;
                calls += 1;
            }
        }
        [
            process_time.as_secs_f64() * 1e6 / calls as f64,
            read_time.as_secs_f64() * 1e6 / calls as f64,
        ]
    });
    println!("demo signal: process {process_micros:.1} us, read {read_micros:.1} us, average per block of 800 frames");
    if !cfg!(debug_assertions) {
        assert!(
            process_micros < budget(130.0),
            "process of 800 frames must stay under 130 us: {process_micros} us"
        );
        assert!(
            read_micros < budget(30.0),
            "read must stay under 30 us: {read_micros} us"
        );
    }
}

/// `read` runs once per displayed frame, so it has to stay cheap too.
#[test]
fn read_budget() {
    let (mut analyzer, _) = warm_analyzer();
    let iterations = 2_000;
    let [microseconds] = fastest(|| {
        let start = Instant::now();
        for _ in 0..iterations {
            std::hint::black_box(analyzer.read());
        }
        [start.elapsed().as_secs_f64() / iterations as f64 * 1e6]
    });
    println!("read(1024 bins): {microseconds:.1} us average over {iterations} calls");
    if !cfg!(debug_assertions) {
        assert!(microseconds < budget(250.0), "read took {microseconds} us");
    }
}

/// `process` must not touch the heap once it is warm. The third-octave bands
/// are part of `process`, so this covers them too.
#[test]
fn process_does_not_allocate_when_warm() {
    let (mut analyzer, signal) = warm_analyzer();
    assert!(analyzer.third_octave().is_some(), "the bands are running");
    let before = allocations();
    process_loop(&mut analyzer, &signal, 1_000);
    assert_eq!(allocations() - before, 0, "process allocated over 1000 calls");
}

/// The top-peaks list is built in `read`, from storage that `process` fills,
/// so `process` still does not allocate with tones to find.
#[test]
fn process_does_not_allocate_with_tones() {
    let mut signal = vec![0.0f32; RATE as usize * 2];
    for (hz, amplitude) in [(220.0, 0.4), (440.0, 0.3), (660.0, 0.2), (880.0, 0.15), (1_320.0, 0.1)] {
        for (sum, tone) in signal.iter_mut().zip(sine(hz, amplitude, RATE, 2.0, 0.0)) {
            *sum += tone;
        }
    }
    let mut analyzer = SpectrumAnalyzer::default();
    for block in signal.chunks(BLOCK) {
        analyzer.process(block, block, RATE);
    }
    analyzer.read();
    assert!(
        !analyzer.top_peaks().is_empty(),
        "there is something to build a list from"
    );
    let before = allocations();
    process_loop(&mut analyzer, &signal, 500);
    assert_eq!(allocations() - before, 0, "process allocated when warm");
}

/// The hard budget of the loudness meter: 800 stereo frames in under 0.5 ms on
/// average. And it allocates nothing, from the first call on.
#[test]
fn loudness_process_budget_and_no_allocation() {
    let signal = pink_noise(0.5, BLOCK, DEFAULT_SEED);
    let mut meter = LoudnessMeter::new();
    let iterations = 2_000;
    // Counted before the timing: the lock in `fastest` allocates on first use.
    let before = allocations();
    for _ in 0..100 {
        meter.process(&signal, &signal, RATE);
    }
    assert_eq!(allocations() - before, 0, "the loudness meter allocated in process");
    let [microseconds] = fastest(|| {
        let start = Instant::now();
        for _ in 0..iterations {
            meter.process(&signal, &signal, RATE);
        }
        [start.elapsed().as_secs_f64() / iterations as f64 * 1e6]
    });
    std::hint::black_box(meter.read());
    println!("loudness process(800 frames @ 48 kHz): {microseconds:.1} us average over {iterations} calls");
    if !cfg!(debug_assertions) {
        assert!(
            microseconds < budget(500.0),
            "loudness process of 800 frames averaged {microseconds} us"
        );
    }
}

/// The budget of the stereo analyzer: 800 stereo frames in under 0.5 ms on
/// average. It allocates nothing once it is warm, and a block longer than its
/// internal chunk allocates nothing too.
#[test]
fn stereo_process_budget_and_no_allocation() {
    let (left, right) = (pink_noise(0.5, BLOCK, 7), pink_noise(0.5, BLOCK, 11));
    let long = pink_noise(0.5, 32_768, 3);
    let mut analyzer = StereoAnalyzer::new();
    for _ in 0..100 {
        analyzer.process(&left, &right, RATE);
    }
    let iterations = 2_000;
    // Counted before the timing: the lock in `fastest` allocates on first use.
    let before = allocations();
    for _ in 0..100 {
        analyzer.process(&left, &right, RATE);
    }
    analyzer.process(&long, &long, RATE);
    assert_eq!(allocations() - before, 0, "the stereo analyzer allocated in process");
    let [microseconds] = fastest(|| {
        let start = Instant::now();
        for _ in 0..iterations {
            analyzer.process(&left, &right, RATE);
        }
        [start.elapsed().as_secs_f64() / iterations as f64 * 1e6]
    });
    std::hint::black_box(analyzer.read());
    println!("stereo process(800 frames @ 48 kHz): {microseconds:.1} us average over {iterations} calls");
    if !cfg!(debug_assertions) {
        assert!(
            microseconds < budget(500.0),
            "stereo process of 800 frames averaged {microseconds} us"
        );
    }
}

// MARK: Session recorder
//
// Budgets: `ingest` under 2 us per call, `snapshot` of a full 30-minute record under 0.2 ms.

/// A frame with the numbers the recorder reads: levels that move with `i`, one
/// stress flag, and an SPL estimate.
fn session_frame(i: usize, clip_count: u64) -> AnalysisFrame {
    let mut frame = AnalysisFrame::new(SpectrumReading::silent(8));
    frame.loudness.momentary_lufs = -30.0 + (i % 7) as f32;
    frame.loudness.short_term_lufs = -24.0;
    frame.loudness.true_peak_left_dbtp = -2.0;
    frame.loudness.true_peak_right_dbtp = -1.5;
    frame.loudness.clip_count = clip_count;
    frame.stereo.correlation = 0.4;
    let b = -40.0 + (i % 8) as f32;
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
    frame.is_silent = false;
    frame.headphone = Some(HeadphoneReading {
        model_name: "Test".into(),
        response_db: vec![0.0; 8],
        target_db: vec![0.0; 8],
        has_target: true,
        predicted_at_ear_db: vec![0.0; 8],
        stress_flags: vec![StressFlag {
            id: "load".into(),
            severity: StressSeverity::High,
            title: "Sub-bass load high".into(),
            detail: String::new(),
            frequency_range_hz: None,
            plot_label: String::new(),
        }],
    });
    let level = 75.0 + (i % 8) as f32;
    frame.spl = Some(SplReading {
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
    });
    frame
}

/// A recorder with the whole record full of samples, and the event ring full too.
fn full_recorder() -> SessionRecorder {
    let capacity = 1800;
    let recorder = SessionRecorder::new(capacity);
    for second in 0..capacity + 5 {
        let frame = session_frame(second, second as u64);
        for _ in 0..60 {
            recorder.ingest(&frame, 1.0 / 60.0);
        }
    }
    // Fill the event ring at the newest end, so nothing is dropped for age.
    for _ in 0..MAX_EVENTS {
        recorder.note_track_start();
    }
    let snapshot = recorder.snapshot();
    assert_eq!((snapshot.samples.len(), snapshot.events.len()), (capacity, MAX_EVENTS));
    recorder
}

#[test]
fn session_ingest_cost() {
    let recorder = SessionRecorder::new(1800);
    let frames: Vec<AnalysisFrame> = (0..8).map(|i| session_frame(i, i as u64 / 4)).collect();
    // Warm up: every path once (ring slots, flag state, event ring).
    for i in 0..6_000 {
        recorder.ingest(&frames[i % frames.len()], 1.0 / 60.0);
    }
    let iterations = 300_000;
    let [per_call] = fastest(|| {
        let start = Instant::now();
        for i in 0..iterations {
            recorder.ingest(&frames[i % frames.len()], 1.0 / 60.0);
        }
        [start.elapsed().as_secs_f64() * 1e6 / iterations as f64]
    });
    println!("SessionRecorder::ingest: {per_call:.3} us per call over {iterations} calls");
    if !cfg!(debug_assertions) {
        assert!(
            per_call < budget(2.0),
            "ingest must stay under 2 us per call: {per_call}"
        );
    }
}

#[test]
fn session_snapshot_cost_of_a_full_record() {
    let recorder = full_recorder();
    let iterations = 500;
    let [per_call] = fastest(|| {
        let start = Instant::now();
        let mut kept = 0;
        for _ in 0..iterations {
            let snapshot = recorder.snapshot();
            kept += snapshot.samples.len() + snapshot.events.len();
        }
        assert_eq!(kept, iterations * 3_800);
        [start.elapsed().as_secs_f64() * 1e6 / iterations as f64]
    });
    println!("SessionRecorder::snapshot (1800 samples + 2000 events): {per_call:.1} us per call");
    if !cfg!(debug_assertions) {
        assert!(
            per_call < budget(200.0),
            "copy-out of a full record must stay under 0.2 ms: {per_call}"
        );
    }
}

/// What the analysis thread really pays: an ingest that closes a second while
/// readers take snapshots. The lock is held for one copy, so the writer is
/// never blocked for long. Two readers at 1000 snapshots per second are a
/// thousand times the load of a timeline panel.
#[test]
fn session_ingest_stays_fast_while_readers_snapshot() {
    let recorder = full_recorder();
    let frame = session_frame(3, 0);
    let [mean, worst] = fastest(|| {
        let stop = Instant::now() + Duration::from_millis(300);
        let (mut worst, mut total, mut calls) = (0.0f64, 0.0f64, 0u32);
        std::thread::scope(|scope| {
            for _ in 0..2 {
                scope.spawn(|| {
                    while Instant::now() < stop {
                        std::hint::black_box(recorder.snapshot());
                        std::thread::sleep(Duration::from_millis(1));
                    }
                });
            }
            while Instant::now() < stop {
                let start = Instant::now();
                recorder.ingest(&frame, 0.5); // every second call closes a second
                let took = start.elapsed().as_secs_f64() * 1e6;
                worst = worst.max(took);
                total += took;
                calls += 1;
            }
        });
        assert!(calls > 1_000);
        [total / calls as f64, worst]
    });
    println!("ingest under 2 readers: mean {mean:.3} us, worst {worst:.1} us");
    if !cfg!(debug_assertions) {
        assert!(
            mean < budget(2.0),
            "mean ingest stays under budget while readers copy: {mean}"
        );
        // The worst single call is scheduling noise, not the lock.
        assert!(
            worst < budget(5_000.0),
            "no ingest waits for a reader for long: {worst}"
        );
    }
}

/// The counter itself works: an allocation on this thread is seen.
#[test]
fn the_allocation_counter_counts() {
    let before = allocations();
    let boxed = std::hint::black_box(Box::new(7u64));
    assert!(allocations() > before);
    drop(boxed);
}
