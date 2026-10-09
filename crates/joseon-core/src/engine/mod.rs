//! Pulls audio from a ring buffer, runs the analyzers, publishes `AnalysisFrame`s.
//!
//! Port source: `Sources/JoseonCore/Engine/AnalysisEngine.swift`.
//!
//! The engine runs on its own thread at a fixed tick. The consumer reads
//! `latest_frame` (for example once per displayed frame).
//!
//! Port notes:
//! - Swift shares one ring buffer object, so `start` can be called again with
//!   the same ring. Here the engine owns the reading end while it runs. `stop`
//!   gives it back, and `start` returns the reader it replaced.
//! - Swift runs the tick on a dispatch queue. Here it is a thread that waits on
//!   a condition variable, so `stop` and `set_tick_rate` take effect at once.
//! - The analyzers live behind one lock that the tick holds while it works.
//!   `process_now` takes the same lock, so it is safe while the engine runs.
//! - `latest_frame` gives a shared frame. A clone of it copies no curves.

#[cfg(test)]
mod tests;

use crate::contracts::{
    AnalysisFrame, LoudnessMetering, LoudnessReading, SessionRecording, SharedHeadphoneModel, SharedSplEstimator,
    SpectrumAnalyzing, SpectrumReading, SpectrumSettings, StereoAnalyzing, StereoReading, StreamInfo,
};
use crate::loudness::LoudnessMeter;
use crate::ring_buffer::RingReader;
use crate::session::SessionRecorder;
use crate::spectrum::SpectrumAnalyzer;
use crate::stereo::StereoAnalyzer;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// No samples for longer than this → the input is stale: the engine feeds
/// zeros, so the spectrum releases, the meters fall, and frames say `is_silent`.
pub const STALE_AFTER_SECONDS: f64 = 0.25;
/// Digital silence for this long, and a frame with every level at its floor →
/// the engine sleeps: it still drains the ring and looks for signal, but it
/// does not run the analyzers on zeros. The first non-zero sample wakes it. A
/// silent source then costs almost no CPU.
pub const SLEEP_AFTER_SILENT_SECONDS: f64 = 4.0;

const MAX_READS_PER_TICK: usize = 4;
const SCRATCH_CAPACITY: usize = 1 << 15;

/// Seconds on a monotonic clock. The clock starts at 1 the first time the
/// process asks, so 0 always means "no frame was published yet".
pub fn uptime() -> f64 {
    static ORIGIN: OnceLock<Instant> = OnceLock::new();
    1.0 + ORIGIN.get_or_init(Instant::now).elapsed().as_secs_f64()
}

/// A lock that a panic in another thread does not close: the data is plain numbers.
fn lock<T: ?Sized>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// The analyzers of an engine. `Default` gives the standard set.
pub struct EngineParts {
    pub spectrum: Box<dyn SpectrumAnalyzing>,
    pub loudness: Box<dyn LoudnessMetering>,
    pub stereo: Box<dyn StereoAnalyzing>,
    /// Rolling record of the last minutes of listening, one sample per second of audio time.
    pub session: Arc<dyn SessionRecording>,
}

impl Default for EngineParts {
    fn default() -> Self {
        Self {
            spectrum: Box::new(SpectrumAnalyzer::default()),
            loudness: Box::new(LoudnessMeter::new()),
            stereo: Box::new(StereoAnalyzer::new()),
            session: Arc::new(SessionRecorder::default()),
        }
    }
}

/// What other threads set and read. One short lock.
struct State {
    headphone_model: Option<SharedHeadphoneModel>,
    spl_estimator: Option<SharedSplEstimator>,
    stream_info: Option<StreamInfo>,
    latest: Arc<AnalysisFrame>,
    pending_reset: bool,
    pending_settings: Option<SpectrumSettings>,
    analyzes_stereo: bool,
}

/// The analyzers and everything only the analysis thread touches.
struct Core {
    spectrum: Box<dyn SpectrumAnalyzing>,
    loudness: Box<dyn LoudnessMetering>,
    stereo: Box<dyn StereoAnalyzing>,
    session: Arc<dyn SessionRecording>,
    state: Arc<Mutex<State>>,

    reader: Option<RingReader>,
    scratch_l: Vec<f32>,
    scratch_r: Vec<f32>,
    /// Input copies with every non-finite sample set to 0. Used only when a block has a NaN or an Inf.
    clean_l: Vec<f32>,
    clean_r: Vec<f32>,
    zeros: Vec<f32>,

    last_sample_time: f64,
    last_tick_time: f64,
    last_sample_rate: f64,
    tick_interval: f64,
    /// `LoudnessReading::measured_seconds` at the last published frame: the
    /// audio clock the SPL estimator and the session recorder share.
    last_published_audio_time: f64,
    /// Seconds of audio in the current run of digital silence.
    silent_run_seconds: f64,
    /// Uptime of the first block of that run. A debug build can analyze slower
    /// than real time: audio seconds then pass slowly, the clock does not.
    silent_run_start: Option<f64>,
    asleep: bool,
    stereo_on: bool,
    sleep_allowed: bool,
    /// True while every block since the last publish was digital silence.
    silent_since_publish: bool,
    analyzed_since_publish: bool,
    last_frame_silent: bool,
    published_model: Option<SharedHeadphoneModel>,
    /// Blocks that had a NaN or an Inf sample. For tests and diagnostics.
    sanitized_block_count: usize,
    /// `process_now` never sleeps by default, so offline runs analyze every block.
    sleeps_in_process_now: bool,
}

impl Core {
    /// Takes the pending settings and the pending reset.
    fn apply_pending(&mut self) {
        let (do_reset, new_settings, model, estimator, want_stereo) = {
            let mut state = lock(&self.state);
            (
                std::mem::take(&mut state.pending_reset),
                state.pending_settings.take(),
                state.headphone_model.clone(),
                state.spl_estimator.clone(),
                state.analyzes_stereo,
            )
        };
        if want_stereo != self.stereo_on {
            self.stereo_on = want_stereo;
            if want_stereo {
                self.stereo.reset(); // its state is from before the pause
            }
        }
        if let Some(settings) = new_settings {
            if settings != *self.spectrum.settings() {
                self.spectrum.set_settings(settings);
                self.asleep = false; // the frame must show the new settings
            }
        }
        if do_reset {
            self.spectrum.reset();
            self.loudness.reset();
            self.stereo.reset();
            if let Some(model) = model {
                lock(&model).reset();
            }
            if let Some(estimator) = estimator {
                lock(&estimator).reset_measurement();
            }
            self.session.note_track_start();
            // The meters start from 0 again: the audio time of the next frame is not a step from this one.
            self.last_published_audio_time = 0.0;
            self.asleep = false;
        }
    }

    fn process_now(&mut self, left: &[f32], right: &[f32], sample_rate: f64) -> Arc<AnalysisFrame> {
        self.apply_pending();
        self.sleep_allowed = self.sleeps_in_process_now;
        if !self.sleep_allowed {
            self.asleep = false;
        }
        self.analyze(left, right, sample_rate);
        self.publish(true)
    }

    fn tick(&mut self) {
        let Some(mut reader) = self.reader.take() else {
            return;
        };
        self.sleep_allowed = true;
        self.apply_pending();
        let now = uptime();
        let since_last_tick = (now - self.last_tick_time).clamp(0.0, 0.25);
        self.last_tick_time = now;
        let (mut scratch_l, mut scratch_r) = (std::mem::take(&mut self.scratch_l), std::mem::take(&mut self.scratch_r));
        let mut n = reader.read(&mut scratch_l, &mut scratch_r);
        if n > 0 {
            self.last_sample_time = now;
            // At most one ring of audio per tick (4 reads of 32768 frames).
            // Analysis that is slower than real time would otherwise never
            // leave this loop: the ring fills again while the block is
            // analyzed, so the tick never ends and no frame is published.
            let mut reads = 0;
            while n > 0 {
                let rate = reader.sample_rate();
                if rate > 0.0 {
                    self.last_sample_rate = rate;
                }
                self.analyze(&scratch_l[..n], &scratch_r[..n], self.last_sample_rate);
                reads += 1;
                if reads >= MAX_READS_PER_TICK {
                    break;
                }
                n = reader.read(&mut scratch_l, &mut scratch_r);
            }
        } else if now - self.last_sample_time > STALE_AFTER_SECONDS {
            // The source stalled (stopped capture, device gone, start pending).
            // Feed the time that passed as digital silence: the analyzers decay
            // with their own ballistics, and the frame says `is_silent`. Never
            // more than 1.5 ticks of zeros per tick: a late tick must not get a
            // longer block, run later again, and so hold the thread at 100 %.
            let seconds = since_last_tick.min(self.tick_interval * 1.5);
            let count = SCRATCH_CAPACITY.min((self.last_sample_rate * seconds) as usize);
            if count > 0 {
                let zeros = std::mem::take(&mut self.zeros);
                self.analyze(&zeros[..count], &zeros[..count], self.last_sample_rate);
                self.zeros = zeros;
            }
        }
        (self.scratch_l, self.scratch_r) = (scratch_l, scratch_r);
        self.reader = Some(reader);
        self.publish(false);
    }

    /// One block: sanitize, look for signal, run the analyzers (not while asleep).
    fn analyze(&mut self, left: &[f32], right: &[f32], sample_rate: f64) {
        let count = left.len().min(right.len());
        if !(count > 0 && sample_rate > 0.0 && sample_rate.is_finite()) {
            return;
        }
        let (left, right) = (&left[..count], &right[..count]);
        // One finite check for all analyzers. A sum is finite only when every
        // sample is finite. (A sum of finite floats that overflows is sanitized
        // for nothing: harmless.)
        let sum: f32 = left.iter().sum::<f32>() + right.iter().sum::<f32>();
        if sum.is_finite() {
            self.analyze_finite(left, right, sample_rate);
            return;
        }
        self.sanitized_block_count += 1;
        let (mut clean_l, mut clean_r) = (std::mem::take(&mut self.clean_l), std::mem::take(&mut self.clean_r));
        for (l, r) in left.chunks(SCRATCH_CAPACITY).zip(right.chunks(SCRATCH_CAPACITY)) {
            let n = l.len();
            for i in 0..n {
                clean_l[i] = if l[i].is_finite() { l[i] } else { 0.0 };
                clean_r[i] = if r[i].is_finite() { r[i] } else { 0.0 };
            }
            self.analyze_finite(&clean_l[..n], &clean_r[..n], sample_rate);
        }
        (self.clean_l, self.clean_r) = (clean_l, clean_r);
    }

    fn analyze_finite(&mut self, left: &[f32], right: &[f32], sample_rate: f64) {
        let has_signal = left.iter().chain(right).any(|&sample| sample != 0.0);
        if has_signal {
            self.silent_run_seconds = 0.0;
            self.silent_run_start = None;
            self.silent_since_publish = false;
            self.asleep = false;
        } else {
            self.silent_run_seconds += left.len() as f64 / sample_rate;
            self.silent_run_start.get_or_insert_with(uptime);
        }
        if self.asleep {
            return;
        }
        self.analyzed_since_publish = true;
        self.spectrum.process(left, right, sample_rate);
        self.loudness.process(left, right, sample_rate);
        if self.stereo_on {
            self.stereo.process(left, right, sample_rate);
        }
    }

    /// `force`: build a frame also when no block was analyzed since the last one (`process_now`).
    fn publish(&mut self, force: bool) -> Arc<AnalysisFrame> {
        let (model, estimator, info, previous) = {
            let state = lock(&self.state);
            (
                state.headphone_model.clone(),
                state.spl_estimator.clone(),
                state.stream_info.clone(),
                Arc::clone(&state.latest),
            )
        };
        let model_changed = match (&model, &self.published_model) {
            (Some(a), Some(b)) => !Arc::ptr_eq(a, b),
            (None, None) => false,
            _ => true,
        };
        if !self.analyzed_since_publish && !force && !model_changed {
            // No new audio went through the analyzers (the ring was empty, or
            // the engine sleeps): the readings did not change. Keep the frame
            // and its `host_time`, so consumers see "no new frame". Only the
            // stream facts can change (a new device, an app that starts to play).
            if previous.stream != info {
                let mut frame = (*previous).clone();
                frame.stream = info;
                let frame = Arc::new(frame);
                lock(&self.state).latest = Arc::clone(&frame);
                return frame;
            }
            return previous;
        }
        // `is_silent` = digital silence in every block since the last frame, not only in the last block.
        if self.analyzed_since_publish || force {
            self.last_frame_silent = self.silent_since_publish;
        }
        self.silent_since_publish = true;
        self.analyzed_since_publish = false;
        self.published_model.clone_from(&model);

        let spectrum = self.spectrum.read();
        let loudness = self.loudness.read();
        let mut frame = AnalysisFrame::new(spectrum.spectrum);
        frame.host_time = uptime();
        frame.stream = info;
        frame.peak = spectrum.peak;
        frame.bands = spectrum.bands;
        frame.stereo = if self.stereo_on {
            self.stereo.read()
        } else {
            StereoReading::default()
        };
        frame.is_silent = self.last_frame_silent;
        frame.top_peaks = self.spectrum.top_peaks();
        frame.lowest_strong_hz = self.spectrum.lowest_strong_hz();
        frame.headphone = model.map(|model| lock(&model).evaluate(&frame.spectrum, &frame.bands, &loudness));
        // Seconds of audio since the previous published frame, from measured
        // audio time: the dose and the session timeline count audio, not wall
        // time. A reset makes `measured_seconds` fall: that is 0, not a step
        // back. One computation, two consumers.
        let audio_now = loudness.measured_seconds;
        let dt = (audio_now - self.last_published_audio_time).clamp(0.0, 1.0);
        self.last_published_audio_time = audio_now;
        frame.loudness = loudness;
        if let Some(bands) = self.spectrum.third_octave() {
            if let Some(estimator) = estimator {
                frame.spl = Some(lock(&estimator).evaluate(&bands, dt, frame.is_silent));
            }
            frame.third_octave = Some(bands);
        }
        let frame = Arc::new(frame);
        lock(&self.state).latest = Arc::clone(&frame);
        self.session.ingest(&frame, dt);
        if self.sleep_allowed && !self.asleep && is_at_rest(&frame) {
            let silent_by_clock = self.silent_run_start.map_or(0.0, |start| frame.host_time - start);
            if self.silent_run_seconds.max(silent_by_clock) >= SLEEP_AFTER_SILENT_SECONDS {
                self.asleep = true;
            }
        }
        frame
    }
}

/// True when nothing in the frame can still move without new signal: the
/// spectrum, the peak hold and the meters are at their floors and no stress
/// flag is up.
pub fn is_at_rest(frame: &AnalysisFrame) -> bool {
    if !frame.is_silent {
        return false;
    }
    if frame
        .headphone
        .as_ref()
        .is_some_and(|headphone| !headphone.stress_flags.is_empty())
    {
        return false;
    }
    let l = &frame.loudness;
    let meter_floor = LoudnessReading::SILENCE_LUFS + 1.0;
    if !(l.momentary_lufs <= meter_floor
        && l.short_term_lufs <= meter_floor
        && l.rms_left_db <= meter_floor
        && l.rms_right_db <= meter_floor)
    {
        return false;
    }
    let floor = SpectrumReading::FLOOR_DB + 0.5;
    let s = &frame.spectrum;
    for trace in [&s.peak_hold, &s.mid, &s.left, &s.right, &s.side] {
        if trace.iter().any(|&level| level > floor) {
            return false;
        }
    }
    !frame.stereo.band_active.contains(&true)
}

/// What the tick thread and its owner share.
struct TickControl {
    /// `(stop, seconds per tick, change count)`.
    signal: Mutex<(bool, f64, u64)>,
    wake: Condvar,
}

struct Running {
    control: Arc<TickControl>,
    thread: JoinHandle<()>,
}

pub struct AnalysisEngine {
    state: Arc<Mutex<State>>,
    core: Arc<Mutex<Core>>,
    session: Arc<dyn SessionRecording>,
    /// Guards the tick thread: `start`, `stop` and `set_tick_rate` are safe from any thread.
    running: Mutex<Option<Running>>,
}

impl Default for AnalysisEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl AnalysisEngine {
    /// An engine with the standard analyzers.
    pub fn new() -> Self {
        Self::with_parts(EngineParts::default())
    }

    pub fn with_parts(parts: EngineParts) -> Self {
        let first = AnalysisFrame::new(SpectrumReading::silent(parts.spectrum.settings().display_bins));
        let state = Arc::new(Mutex::new(State {
            headphone_model: None,
            spl_estimator: None,
            stream_info: None,
            latest: Arc::new(first),
            pending_reset: false,
            pending_settings: None,
            analyzes_stereo: true,
        }));
        let core = Core {
            spectrum: parts.spectrum,
            loudness: parts.loudness,
            stereo: parts.stereo,
            session: Arc::clone(&parts.session),
            state: Arc::clone(&state),
            reader: None,
            scratch_l: vec![0.0; SCRATCH_CAPACITY],
            scratch_r: vec![0.0; SCRATCH_CAPACITY],
            clean_l: vec![0.0; SCRATCH_CAPACITY],
            clean_r: vec![0.0; SCRATCH_CAPACITY],
            zeros: vec![0.0; SCRATCH_CAPACITY],
            last_sample_time: 0.0,
            last_tick_time: 0.0,
            last_sample_rate: 48_000.0,
            tick_interval: 1.0 / 60.0,
            last_published_audio_time: 0.0,
            silent_run_seconds: 0.0,
            silent_run_start: None,
            asleep: false,
            stereo_on: true,
            sleep_allowed: true,
            silent_since_publish: true,
            analyzed_since_publish: false,
            last_frame_silent: true,
            published_model: None,
            sanitized_block_count: 0,
            sleeps_in_process_now: false,
        };
        Self {
            state,
            core: Arc::new(Mutex::new(core)),
            session: parts.session,
            running: Mutex::new(None),
        }
    }

    /// The rolling record. The engine feeds it; panels read `snapshot()` from any thread.
    pub fn session(&self) -> &Arc<dyn SessionRecording> {
        &self.session
    }

    /// Newest frame. Cheap to call from the render loop.
    pub fn latest_frame(&self) -> Arc<AnalysisFrame> {
        Arc::clone(&lock(&self.state).latest)
    }

    /// Optional. Set or clear at any time.
    pub fn set_headphone_model(&self, model: Option<SharedHeadphoneModel>) {
        lock(&self.state).headphone_model = model;
    }

    /// Optional. Set or clear at any time. Needs a spectrum analyzer that gives third-octave bands.
    pub fn set_spl_estimator(&self, estimator: Option<SharedSplEstimator>) {
        lock(&self.state).spl_estimator = estimator;
    }

    /// Stream facts to stamp on frames. The app sets this when the source reports a change.
    pub fn set_stream_info(&self, info: Option<StreamInfo>) {
        lock(&self.state).stream_info = info;
    }

    pub fn stream_info(&self) -> Option<StreamInfo> {
        lock(&self.state).stream_info.clone()
    }

    /// Off: the stereo analyzer does not run and frames carry an empty
    /// `StereoReading`. The app turns it off while no vectorscope or stereo
    /// readout is on screen: the stereo analyzer has no long-term state, so
    /// nothing is lost, and its filters are a quarter of the analysis cost. On
    /// again: the analyzer starts from a reset.
    pub fn set_analyzes_stereo(&self, on: bool) {
        lock(&self.state).analyzes_stereo = on;
    }

    pub fn analyzes_stereo(&self) -> bool {
        lock(&self.state).analyzes_stereo
    }

    /// Starts a new measurement (integrated loudness, max values, average spectrum, headphone flag hold).
    pub fn reset_measurement(&self) {
        lock(&self.state).pending_reset = true;
    }

    /// A settings change from any thread. It applies before the analysis of the
    /// next tick. When the engine is stopped, it applies on the next `start`
    /// tick or the next `process_now`.
    pub fn update_spectrum_settings(&self, settings: SpectrumSettings) {
        lock(&self.state).pending_settings = Some(settings);
    }

    /// The settings the spectrum analyzer has now.
    pub fn spectrum_settings(&self) -> SpectrumSettings {
        lock(&self.core).spectrum.settings().clone()
    }

    /// Starts the tick on `reader`, or restarts it on a new one. Returns the
    /// reader the engine had before, if it had one. Safe from any thread.
    pub fn start(&self, reader: RingReader, ticks_per_second: f64) -> Option<RingReader> {
        let mut running = lock(&self.running);
        Self::halt(running.take());
        let interval = 1.0 / ticks_per_second.max(1.0);
        let previous = {
            let mut core = lock(&self.core);
            let now = uptime();
            core.last_sample_time = now;
            core.last_tick_time = now;
            core.tick_interval = interval;
            core.reader.replace(reader)
        };
        let control = Arc::new(TickControl {
            signal: Mutex::new((false, interval, 0)),
            wake: Condvar::new(),
        });
        let (thread_control, thread_core) = (Arc::clone(&control), Arc::clone(&self.core));
        let spawned = std::thread::Builder::new()
            .name("joseon.analysis".to_string())
            .spawn(move || tick_loop(&thread_control, &thread_core));
        // No thread means no ticks: the engine stays stopped, and the reader
        // comes back with the next `stop`.
        if let Ok(thread) = spawned {
            *running = Some(Running { control, thread });
        }
        previous
    }

    /// Stops the tick and gives the reader back. A tick that runs now still
    /// ends; no tick starts after it. Safe to call twice.
    pub fn stop(&self) -> Option<RingReader> {
        let mut running = lock(&self.running);
        Self::halt(running.take());
        lock(&self.core).reader.take()
    }

    pub fn is_running(&self) -> bool {
        lock(&self.running).is_some()
    }

    /// Changes the tick rate of a running engine. No stop and start: the
    /// analyzers and the ring stay as they are. Does nothing when the engine is stopped.
    pub fn set_tick_rate(&self, ticks_per_second: f64) {
        let running = lock(&self.running);
        let Some(running) = running.as_ref() else {
            return;
        };
        let interval = 1.0 / ticks_per_second.max(1.0);
        {
            let mut signal = lock(&running.control.signal);
            signal.1 = interval;
            signal.2 += 1;
        }
        running.control.wake.notify_all();
    }

    /// Signals the tick thread to end and waits for it.
    fn halt(running: Option<Running>) {
        let Some(running) = running else {
            return;
        };
        lock(&running.control.signal).0 = true;
        running.control.wake.notify_all();
        // A join error means the tick thread panicked. It is gone either way.
        let _ = running.thread.join();
    }

    /// Runs one analysis step now on the thread of the caller. For tests and the probe.
    pub fn process_now(&self, left: &[f32], right: &[f32], sample_rate: f64) -> Arc<AnalysisFrame> {
        lock(&self.core).process_now(left, right, sample_rate)
    }

    /// Blocks that had a NaN or an Inf sample.
    pub fn sanitized_block_count(&self) -> usize {
        lock(&self.core).sanitized_block_count
    }

    /// The analyzers do not run now.
    pub fn is_asleep(&self) -> bool {
        lock(&self.core).asleep
    }

    /// Lets `process_now` sleep on silence, as the tick does. Off by default.
    pub fn set_sleeps_in_process_now(&self, on: bool) {
        lock(&self.core).sleeps_in_process_now = on;
    }
}

impl Drop for AnalysisEngine {
    fn drop(&mut self) {
        self.stop();
    }
}

/// The tick thread: one tick at once, then one per interval, until the stop signal.
fn tick_loop(control: &TickControl, core: &Mutex<Core>) {
    let mut next = Instant::now();
    let mut seen_change = 0;
    loop {
        let interval;
        {
            let mut signal = lock(&control.signal);
            loop {
                if signal.0 {
                    return;
                }
                if signal.2 != seen_change {
                    // A new rate: the next tick is one new interval from now.
                    seen_change = signal.2;
                    next = Instant::now() + Duration::from_secs_f64(signal.1);
                }
                let now = Instant::now();
                if now >= next {
                    break;
                }
                signal = control
                    .wake
                    .wait_timeout(signal, next - now)
                    .unwrap_or_else(|poisoned| poisoned.into_inner())
                    .0;
            }
            interval = signal.1;
            next += Duration::from_secs_f64(interval);
            // A late tick does not run a burst to catch up.
            let now = Instant::now();
            if next < now {
                next = now;
            }
        }
        let mut core = lock(core);
        core.tick_interval = interval;
        core.tick();
    }
}
