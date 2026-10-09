//! The capture source: a thread that keeps one loopback stream open, writes
//! its packets to the ring buffer, and follows the changes of the system (a new
//! default device, a stream that ends, a player that takes the device for
//! itself).
//!
//! The system calls are behind `Backend` and `Endpoint`. The Windows ones are
//! in `wasapi.rs`. The tests here run the same thread with a made-up backend,
//! on every system.
//!
//! Threads:
//! - the capture thread owns the backend and the stream (on Windows they are
//!   COM objects that stay on their thread). It calls `on_stream_info_change`.
//! - any thread reads `stream_info`, `health` and the counters.

use crate::convert::Converter;
use crate::format::WireFormat;
use joseon_core::contracts::{AudioSource, SourceError, StreamInfo, StreamInfoCallback};
use joseon_core::ring_buffer::{stereo_ring_buffer, RingReader, RingWriter, DEFAULT_CAPACITY};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// Why no stream could be opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// The system has no output device (or none is switched on).
    NoDevice,
    /// A player holds the device in exclusive mode. The shared mix, which loopback reads, is off.
    Exclusive,
    /// Any other refusal, with the message of the system.
    Platform(String),
}

/// An open stream ended (the device was unplugged, its format changed, a player took it over).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamEnded(pub String);

/// One packet of a stream.
pub enum Packet<'a> {
    /// The bytes of whole frames, in the format of the stream.
    Sound(&'a [u8]),
    /// This many frames that the system marks as silent.
    Silence(usize),
}

/// What takes the packets of `Endpoint::drain`.
pub type PacketSink<'s> = dyn FnMut(Packet<'_>) + 's;

/// One open loopback stream.
pub trait Endpoint {
    /// Device name, rate, channel count and bit depth. `active_sources` is filled in by the source.
    fn info(&self) -> StreamInfo;
    fn format(&self) -> WireFormat;
    /// Gives every waiting packet to `sink`.
    fn drain(&mut self, sink: &mut PacketSink<'_>) -> Result<(), StreamEnded>;
    /// The names of the programs that play sound on this device now.
    fn active_sources(&mut self) -> Vec<String>;
    /// The factor (0...1] that the volume control of the system has put on the captured mix, when that
    /// control stands BEFORE the capture point. `None` when the mix is captured before the volume control (or
    /// the system cannot tell). The source can divide by it: the analysis then sees what the player sent.
    /// `also_hardware` = give the factor also for a device that says it sets the volume itself.
    fn volume_factor(&mut self, _also_hardware: bool) -> Option<f32> {
        None
    }
}

/// The system side of the capture. Made on the capture thread and never leaves it.
pub trait Backend {
    type Stream: Endpoint;
    /// Opens a loopback stream on the default output device.
    fn open(&mut self) -> Result<Self::Stream, OpenError>;
    /// True while the device of `stream` is still the default output device.
    fn is_current(&mut self, stream: &Self::Stream) -> bool;
}

/// What the capture does now, for the status line of the app.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureHealth {
    Stopped,
    /// Sound arrives.
    Signal,
    /// The stream is open and nothing plays. (Windows sends no packets then.)
    Quiet,
    /// A program plays, and the mix has been silent for more than 3 s.
    SilentWhilePlaying,
    /// A player holds the device in exclusive mode.
    ExclusiveMode,
    /// No stream is open. The capture tries again every second.
    NoStream(String),
}

impl CaptureHealth {
    /// One sentence for the listener. `None` when there is nothing to say.
    pub fn advice(&self) -> Option<String> {
        match self {
            Self::Stopped | Self::Signal | Self::Quiet => None,
            Self::SilentWhilePlaying => Some(
                "A player is open but Windows mixes only silence. The player is paused or muted, or it plays \
                 through ASIO or another path that goes around the Windows mixer."
                    .to_string(),
            ),
            Self::ExclusiveMode => Some(
                "A player uses the output device in exclusive mode. Windows then gives no copy of the sound. \
                 Set the player to shared mode (in its audio settings: WASAPI shared, not exclusive)."
                    .to_string(),
            ),
            Self::NoStream(message) => Some(format!("No sound can be read: {message}.")),
        }
    }
}

/// The times of the capture thread. The defaults are for the app; tests use short ones.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Timing {
    /// The pause between two looks for packets.
    pub poll: Duration,
    /// How often the thread looks at the playing programs and at the default device.
    pub housekeeping: Duration,
    /// The pause before the next try when no stream is open.
    pub retry: Duration,
    /// A program plays and no sample is other than zero for this long: `SilentWhilePlaying`.
    pub silent_after: Duration,
    /// No sample other than zero for this long: back from `Signal` to `Quiet`.
    pub quiet_after: Duration,
    /// How often the thread reads the volume of the system.
    pub volume_poll: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            poll: Duration::from_millis(5),
            housekeeping: Duration::from_secs(1),
            retry: Duration::from_secs(1),
            silent_after: Duration::from_secs(3),
            quiet_after: Duration::from_secs(2),
            volume_poll: Duration::from_millis(50),
        }
    }
}

#[derive(Default)]
struct State {
    info: Option<StreamInfo>,
    health: Option<CaptureHealth>,
    callback: Option<StreamInfoCallback>,
    last_error: Option<String>,
    started: Option<Instant>,
}

struct Shared {
    state: Mutex<State>,
    stop: AtomicBool,
    signal: AtomicBool,
    /// The owner wants the volume of the system taken out of the samples.
    compensate: AtomicBool,
    /// Also for a device that says it sets the volume itself.
    compensate_always: AtomicBool,
    /// The gain on the samples now, as the bits of an `f32` (1 = none).
    gain_bits: std::sync::atomic::AtomicU32,
    packets: AtomicU64,
    frames: AtomicU64,
    rebuilds: AtomicU64,
}

impl Shared {
    fn set_health(&self, health: CaptureHealth) {
        self.state.lock().unwrap().health = Some(health);
    }

    /// Stores the stream facts and tells the owner when they changed.
    fn publish(&self, info: StreamInfo) {
        let mut state = self.state.lock().unwrap();
        if state.info.as_ref() == Some(&info) {
            return;
        }
        state.info = Some(info.clone());
        if let Some(callback) = &state.callback {
            callback(info);
        }
    }
}

/// System audio through a loopback stream of the default output device.
pub struct LoopbackSource<B: Backend> {
    make: fn() -> Result<B, OpenError>,
    timing: Timing,
    shared: Arc<Shared>,
    reader: Option<RingReader>,
    /// `Some` while stopped. The capture thread owns the writer while it runs.
    writer: Option<RingWriter>,
    thread: Option<JoinHandle<RingWriter>>,
}

impl<B: Backend + 'static> LoopbackSource<B> {
    /// `make` runs on the capture thread, at every start.
    pub fn with_backend(make: fn() -> Result<B, OpenError>) -> Self {
        let (writer, reader) = stereo_ring_buffer(DEFAULT_CAPACITY);
        Self {
            make,
            timing: Timing::default(),
            shared: Arc::new(Shared {
                state: Mutex::new(State::default()),
                stop: AtomicBool::new(false),
                signal: AtomicBool::new(false),
                compensate: AtomicBool::new(true),
                compensate_always: AtomicBool::new(false),
                gain_bits: std::sync::atomic::AtomicU32::new(1.0f32.to_bits()),
                packets: AtomicU64::new(0),
                frames: AtomicU64::new(0),
                rebuilds: AtomicU64::new(0),
            }),
            reader: Some(reader),
            writer: Some(writer),
            thread: None,
        }
    }

    pub fn set_timing(&mut self, timing: Timing) {
        self.timing = timing;
    }

    pub fn health(&self) -> CaptureHealth {
        if !self.is_running() {
            return CaptureHealth::Stopped;
        }
        let state = self.shared.state.lock().unwrap();
        state.health.clone().unwrap_or(CaptureHealth::Quiet)
    }

    /// Takes the volume control of the system out of the samples, where it stands before the capture point
    /// (on by default). The numbers then describe what the player sent, at every position of the slider.
    pub fn set_volume_compensation(&self, on: bool) {
        self.shared.compensate.store(on, Ordering::Relaxed);
    }

    /// Takes the volume out also on a device that says it sets the volume itself (off by default). For the
    /// devices where the capture is quieter at a lower volume all the same.
    pub fn set_volume_compensation_always(&self, on: bool) {
        self.shared.compensate_always.store(on, Ordering::Relaxed);
    }

    /// The gain the capture puts on the samples now, in dB: 0 with no compensation, +17 when the volume of
    /// the system stands at -17 dB.
    pub fn compensation_db(&self) -> f32 {
        20.0 * f32::from_bits(self.shared.gain_bits.load(Ordering::Relaxed)).log10()
    }

    /// True when a sample other than zero arrived since `start`.
    pub fn has_received_signal(&self) -> bool {
        self.shared.signal.load(Ordering::Relaxed)
    }

    /// Seconds since `start`. 0 when stopped.
    pub fn seconds_since_start(&self) -> f64 {
        let state = self.shared.state.lock().unwrap();
        state.started.map_or(0.0, |t| t.elapsed().as_secs_f64())
    }

    /// Diagnostics: packets and frames since `start`, and how often a stream was opened.
    pub fn packet_count(&self) -> u64 {
        self.shared.packets.load(Ordering::Relaxed)
    }

    pub fn frames_delivered(&self) -> u64 {
        self.shared.frames.load(Ordering::Relaxed)
    }

    pub fn stream_count(&self) -> u64 {
        self.shared.rebuilds.load(Ordering::Relaxed)
    }

    /// The message of the last stream that ended or did not open. `None` after a clean open.
    pub fn last_error(&self) -> Option<String> {
        self.shared.state.lock().unwrap().last_error.clone()
    }
}

/// The lowest volume factor the capture undoes: -60 dB.
pub const MIN_VOLUME_FACTOR: f32 = 0.001;

/// Sleeps `total` in short steps. False when the stop flag came up.
fn pause(shared: &Shared, total: Duration) -> bool {
    let end = Instant::now() + total;
    loop {
        if shared.stop.load(Ordering::Relaxed) {
            return false;
        }
        let left = end.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return true;
        }
        std::thread::sleep(left.min(Duration::from_millis(20)));
    }
}

/// The capture thread. `ready` gets the result of the first try to open a stream.
fn run<B: Backend>(
    make: fn() -> Result<B, OpenError>,
    timing: Timing,
    shared: Arc<Shared>,
    mut writer: RingWriter,
    ready: mpsc::Sender<Result<(), SourceError>>,
) -> RingWriter {
    // `Some` until the first try has its answer.
    let mut ready = Some(ready);
    fn answer(ready: &mut Option<mpsc::Sender<Result<(), SourceError>>>, result: Result<(), SourceError>) {
        if let Some(ready) = ready.take() {
            let _ = ready.send(result);
        }
    }
    let mut backend = match make() {
        Ok(backend) => backend,
        Err(error) => {
            answer(
                &mut ready,
                Err(match error {
                    OpenError::NoDevice => SourceError::NoDevice,
                    OpenError::Exclusive => SourceError::Platform("the device is in exclusive mode".to_string()),
                    OpenError::Platform(message) => SourceError::Platform(message),
                }),
            );
            return writer;
        }
    };
    while !shared.stop.load(Ordering::Relaxed) {
        match backend.open() {
            Ok(mut stream) => {
                shared.rebuilds.fetch_add(1, Ordering::Relaxed);
                shared.state.lock().unwrap().last_error = None;
                shared.set_health(CaptureHealth::Quiet);
                // The stream facts are there when `start` returns.
                let mut info = stream.info();
                info.active_sources = stream.active_sources();
                shared.publish(info);
                answer(&mut ready, Ok(()));
                if let Err(StreamEnded(message)) = pump(&mut backend, &mut stream, &timing, &shared, &mut writer) {
                    shared.state.lock().unwrap().last_error = Some(message);
                }
                // A new stream follows at once: the usual reason is a new default device.
                continue;
            }
            // A player has the device for itself. That is a state of the system, not a failure of the start:
            // the capture waits, and the app says why there is no picture.
            Err(OpenError::Exclusive) => {
                shared.set_health(CaptureHealth::ExclusiveMode);
                answer(&mut ready, Ok(()));
            }
            Err(OpenError::NoDevice) => {
                shared.set_health(CaptureHealth::NoStream("no output device".to_string()));
                // The first try decides `start`. Later the device may come back (a headset that is switched on).
                if ready.is_some() {
                    answer(&mut ready, Err(SourceError::NoDevice));
                    return writer;
                }
            }
            Err(OpenError::Platform(message)) => {
                shared.set_health(CaptureHealth::NoStream(message.clone()));
                shared.state.lock().unwrap().last_error = Some(message.clone());
                if ready.is_some() {
                    answer(&mut ready, Err(SourceError::Platform(message)));
                    return writer;
                }
            }
        }
        if !pause(&shared, timing.retry) {
            break;
        }
    }
    writer
}

/// Moves packets until the stream ends (`Err`), the default device changes or the stop flag comes up (`Ok`).
fn pump<B: Backend>(
    backend: &mut B,
    stream: &mut B::Stream,
    timing: &Timing,
    shared: &Shared,
    writer: &mut RingWriter,
) -> Result<(), StreamEnded> {
    let format = stream.format();
    let mut converter = Converter::new(format);
    let mut info = stream.info();
    info.active_sources = stream.active_sources();
    shared.publish(info.clone());
    let opened = Instant::now();
    let mut last_volume: Option<Instant> = None;
    let mut last_housekeeping = Instant::now();
    let mut last_sound: Option<Instant> = None;
    while !shared.stop.load(Ordering::Relaxed) {
        if last_volume.is_none_or(|t| t.elapsed() >= timing.volume_poll) {
            last_volume = Some(Instant::now());
            let factor = stream
                .volume_factor(shared.compensate_always.load(Ordering::Relaxed))
                .filter(|_| shared.compensate.load(Ordering::Relaxed));
            // At most 60 dB: under that the slider is as good as off, and the gain would only show noise.
            let gain = factor.map_or(1.0, |f| 1.0 / f.max(MIN_VOLUME_FACTOR).min(1.0));
            converter.set_gain(gain);
            shared.gain_bits.store(converter.gain().to_bits(), Ordering::Relaxed);
        }
        let mut sound = false;
        stream.drain(&mut |packet| {
            let (left, right) = match packet {
                Packet::Sound(bytes) => converter.convert(bytes),
                Packet::Silence(frames) => converter.silence(frames),
            };
            if left.is_empty() {
                return;
            }
            sound |= left.iter().chain(right).any(|v| *v != 0.0);
            writer.write(left, right, format.sample_rate);
            shared.packets.fetch_add(1, Ordering::Relaxed);
            shared.frames.fetch_add(left.len() as u64, Ordering::Relaxed);
        })?;
        if sound {
            if last_sound.is_none() {
                shared.signal.store(true, Ordering::Relaxed);
            }
            last_sound = Some(Instant::now());
            shared.set_health(CaptureHealth::Signal);
        }
        if last_housekeeping.elapsed() >= timing.housekeeping {
            last_housekeeping = Instant::now();
            if !backend.is_current(stream) {
                return Ok(());
            }
            let sources = stream.active_sources();
            if sources != info.active_sources {
                info.active_sources = sources;
                shared.publish(info.clone());
            }
            let silent_for = last_sound.unwrap_or(opened).elapsed();
            if silent_for >= timing.silent_after && !info.active_sources.is_empty() {
                shared.set_health(CaptureHealth::SilentWhilePlaying);
            } else if last_sound.is_none() || silent_for >= timing.quiet_after {
                shared.set_health(CaptureHealth::Quiet);
            }
        }
        std::thread::sleep(timing.poll);
    }
    Ok(())
}

impl<B: Backend + 'static> AudioSource for LoopbackSource<B> {
    fn stream_info(&self) -> Option<StreamInfo> {
        self.shared.state.lock().unwrap().info.clone()
    }

    fn take_reader(&mut self) -> Option<RingReader> {
        self.reader.take()
    }

    fn set_on_stream_info_change(&mut self, callback: Option<StreamInfoCallback>) {
        self.shared.state.lock().unwrap().callback = callback;
    }

    fn start(&mut self) -> Result<(), SourceError> {
        let Some(writer) = self.writer.take() else {
            return Ok(()); // already running
        };
        self.shared.stop.store(false, Ordering::Relaxed);
        self.shared.signal.store(false, Ordering::Relaxed);
        self.shared.packets.store(0, Ordering::Relaxed);
        self.shared.frames.store(0, Ordering::Relaxed);
        self.shared.rebuilds.store(0, Ordering::Relaxed);
        {
            let mut state = self.shared.state.lock().unwrap();
            state.info = None;
            state.health = None;
            state.last_error = None;
            state.started = Some(Instant::now());
        }
        let (ready, result) = mpsc::channel();
        let (make, timing, shared) = (self.make, self.timing, Arc::clone(&self.shared));
        let thread = std::thread::Builder::new()
            .name("joseon capture".to_string())
            .spawn(move || run(make, timing, shared, writer, ready));
        let thread = match thread {
            Ok(thread) => thread,
            Err(error) => return Err(SourceError::Platform(format!("no capture thread: {error}"))),
        };
        let outcome = result
            .recv()
            .unwrap_or_else(|_| Err(SourceError::Platform("the capture thread ended".to_string())));
        match outcome {
            Ok(()) => {
                self.thread = Some(thread);
                Ok(())
            }
            Err(error) => {
                // The thread has ended and gives the writer back.
                self.writer = thread.join().ok();
                self.shared.state.lock().unwrap().started = None;
                Err(error)
            }
        }
    }

    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            self.writer = thread.join().ok();
        }
        let mut state = self.shared.state.lock().unwrap();
        state.started = None;
        state.health = None;
    }

    fn is_running(&self) -> bool {
        self.thread.is_some()
    }
}

impl<B: Backend> Drop for LoopbackSource<B> {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::format::SampleKind;
    use std::collections::VecDeque;

    /// What the made-up system does next. One script for the whole process: the tests take turns.
    #[derive(Default)]
    struct Script {
        /// The result of each `open`, in order. Empty = open a stream on the device `device`.
        opens: VecDeque<Result<(), OpenError>>,
        device: String,
        rate: f64,
        sources: Vec<String>,
        /// Packets the next `drain` gives.
        packets: VecDeque<Result<Vec<u8>, usize>>,
        /// The next `drain` ends the stream with this message.
        end: Option<String>,
        volume: Option<f32>,
        make_fails: Option<OpenError>,
    }

    static SCRIPT: Mutex<Option<Script>> = Mutex::new(None);
    static TURN: Mutex<()> = Mutex::new(());

    fn script<T>(body: impl FnOnce(&mut Script) -> T) -> T {
        body(SCRIPT.lock().unwrap().get_or_insert_with(Script::default))
    }

    struct FakeStream {
        device: String,
        rate: f64,
    }

    impl Endpoint for FakeStream {
        fn info(&self) -> StreamInfo {
            StreamInfo {
                bit_depth: Some(24),
                device_is_default: Some(true),
                ..StreamInfo::new(self.rate, 2, self.device.clone())
            }
        }

        fn format(&self) -> WireFormat {
            WireFormat {
                kind: SampleKind::F32,
                channels: 2,
                sample_rate: self.rate,
                valid_bits: 32,
                channel_mask: 3,
            }
        }

        fn drain(&mut self, sink: &mut PacketSink<'_>) -> Result<(), StreamEnded> {
            let (packets, end) = script(|s| (std::mem::take(&mut s.packets), s.end.take()));
            for packet in packets {
                match packet {
                    Ok(bytes) => sink(Packet::Sound(&bytes)),
                    Err(frames) => sink(Packet::Silence(frames)),
                }
            }
            end.map_or(Ok(()), |message| Err(StreamEnded(message)))
        }

        fn active_sources(&mut self) -> Vec<String> {
            script(|s| s.sources.clone())
        }

        fn volume_factor(&mut self, _also_hardware: bool) -> Option<f32> {
            script(|s| s.volume)
        }
    }

    struct FakeBackend;

    impl Backend for FakeBackend {
        type Stream = FakeStream;

        fn open(&mut self) -> Result<FakeStream, OpenError> {
            script(|s| {
                s.opens.pop_front().unwrap_or(Ok(()))?;
                Ok(FakeStream {
                    device: s.device.clone(),
                    rate: s.rate,
                })
            })
        }

        fn is_current(&mut self, stream: &FakeStream) -> bool {
            script(|s| s.device == stream.device)
        }
    }

    fn make() -> Result<FakeBackend, OpenError> {
        script(|s| s.make_fails.clone()).map_or(Ok(FakeBackend), Err)
    }

    fn fast() -> Timing {
        Timing {
            poll: Duration::from_millis(1),
            housekeeping: Duration::from_millis(5),
            retry: Duration::from_millis(5),
            silent_after: Duration::from_millis(40),
            quiet_after: Duration::from_millis(40),
            volume_poll: Duration::from_millis(2),
        }
    }

    /// A source on a fresh script. The guard keeps the other tests out.
    fn source(device: &str) -> (LoopbackSource<FakeBackend>, std::sync::MutexGuard<'static, ()>) {
        let guard = TURN.lock().unwrap_or_else(|e| e.into_inner());
        *SCRIPT.lock().unwrap() = Some(Script {
            device: device.to_string(),
            rate: 48_000.0,
            ..Script::default()
        });
        let mut source = LoopbackSource::with_backend(make);
        source.set_timing(fast());
        (source, guard)
    }

    fn until(what: &str, condition: impl Fn() -> bool) {
        let end = Instant::now() + Duration::from_secs(5);
        while !condition() {
            assert!(Instant::now() < end, "timed out: {what}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }

    fn floats(values: &[f32]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_le_bytes()).collect()
    }

    #[test]
    fn packets_reach_the_ring_buffer() {
        let (mut source, _turn) = source("Speakers (USB DAC)");
        let mut reader = source.take_reader().unwrap();
        assert!(source.take_reader().is_none());
        assert_eq!(source.health(), CaptureHealth::Stopped);
        source.start().unwrap();
        assert!(source.is_running());
        let info = source.stream_info().unwrap();
        assert_eq!(
            (info.device_name.as_str(), info.sample_rate, info.bit_depth),
            ("Speakers (USB DAC)", 48_000.0, Some(24))
        );
        assert!(!source.has_received_signal());

        // A silent packet, then sound.
        script(|s| s.packets.push_back(Err(3)));
        until("the silent packet", || source.frames_delivered() == 3);
        assert!(!source.has_received_signal());
        assert_eq!(source.health(), CaptureHealth::Quiet);
        script(|s| s.packets.push_back(Ok(floats(&[0.5, -0.5, 0.25, -0.25]))));
        until("the sound", || source.has_received_signal());
        until("the health", || source.health() == CaptureHealth::Signal);
        assert_eq!((source.packet_count(), source.frames_delivered()), (2, 5));
        let (mut left, mut right) = ([0.0f32; 8], [0.0f32; 8]);
        assert_eq!(reader.read(&mut left, &mut right), 5);
        assert_eq!(left[..5], [0.0, 0.0, 0.0, 0.5, 0.25]);
        assert_eq!(right[..5], [0.0, 0.0, 0.0, -0.5, -0.25]);
        assert_eq!(reader.sample_rate(), 48_000.0);
        assert!(source.seconds_since_start() > 0.0);

        source.stop();
        assert!(!source.is_running());
        assert_eq!(
            (source.health(), source.seconds_since_start()),
            (CaptureHealth::Stopped, 0.0)
        );
        // A stop is final until the next start, and the next start works.
        source.start().unwrap();
        assert_eq!(source.frames_delivered(), 0);
        script(|s| s.packets.push_back(Ok(floats(&[0.1, 0.1]))));
        until("sound after the restart", || source.frames_delivered() == 1);
        source.stop();
    }

    #[test]
    fn a_new_default_device_gets_a_new_stream() {
        let (mut source, _turn) = source("Speakers");
        let (seen, changes) = mpsc::channel();
        source.set_on_stream_info_change(Some(Box::new(move |info| {
            let _ = seen.send((info.device_name, info.sample_rate, info.active_sources));
        })));
        source.start().unwrap();
        assert_eq!(changes.recv().unwrap(), ("Speakers".to_string(), 48_000.0, vec![]));
        // A player starts: the stream facts name it.
        script(|s| s.sources = vec!["Qobuz".to_string()]);
        assert_eq!(changes.recv().unwrap().2, ["Qobuz"]);
        // The listener plugs in a DAC at another rate.
        script(|s| {
            s.device = "USB DAC".to_string();
            s.rate = 96_000.0;
        });
        assert_eq!(
            changes.recv().unwrap(),
            ("USB DAC".to_string(), 96_000.0, vec!["Qobuz".to_string()])
        );
        assert_eq!(source.stream_count(), 2);
        assert_eq!(source.last_error(), None);
        source.stop();
    }

    #[test]
    fn a_stream_that_ends_is_opened_again() {
        let (mut source, _turn) = source("Headset");
        source.start().unwrap();
        // The device goes away, and stays away for two tries.
        script(|s| {
            s.opens.extend([
                Err(OpenError::NoDevice),
                Err(OpenError::Platform("device not ready".to_string())),
            ]);
            s.end = Some("the device was removed".to_string());
        });
        until("the third stream", || source.stream_count() == 2);
        assert!(source.is_running());
        assert_eq!(source.last_error(), None, "a clean open clears the message");
        script(|s| s.packets.push_back(Ok(floats(&[0.2, 0.2]))));
        until("sound on the new stream", || source.health() == CaptureHealth::Signal);
        source.stop();
    }

    #[test]
    fn a_player_in_exclusive_mode_is_a_state_not_a_failure() {
        let (mut source, _turn) = source("DAC");
        script(|s| s.opens.extend([Err(OpenError::Exclusive), Err(OpenError::Exclusive)]));
        source.start().unwrap();
        assert_eq!(source.health(), CaptureHealth::ExclusiveMode);
        assert!(source.health().advice().unwrap().contains("shared mode"));
        assert_eq!(source.stream_info(), None);
        // The player lets go: the capture picks the device up with no new start.
        until("the stream", || source.stream_count() == 1);
        until("the health", || source.health() == CaptureHealth::Quiet);
        assert_eq!(CaptureHealth::Quiet.advice(), None);
        source.stop();
    }

    #[test]
    fn silence_while_a_program_plays_is_told() {
        let (mut source, _turn) = source("DAC");
        script(|s| s.sources = vec!["foobar2000".to_string()]);
        source.start().unwrap();
        until("the diagnosis", || source.health() == CaptureHealth::SilentWhilePlaying);
        assert!(source.health().advice().unwrap().contains("ASIO"));
        // Sound: the diagnosis goes away. Then the player stops: quiet.
        script(|s| s.packets.push_back(Ok(floats(&[0.3, 0.3]))));
        until("the signal", || source.health() == CaptureHealth::Signal);
        script(|s| s.sources.clear());
        until("quiet again", || source.health() == CaptureHealth::Quiet);
        source.stop();
    }

    #[test]
    fn a_start_with_no_device_fails_and_can_be_tried_again() {
        let (mut source, _turn) = source("DAC");
        script(|s| s.opens.push_back(Err(OpenError::NoDevice)));
        assert_eq!(source.start(), Err(SourceError::NoDevice));
        assert!(!source.is_running());
        script(|s| s.opens.push_back(Err(OpenError::Platform("access denied".to_string()))));
        assert_eq!(source.start(), Err(SourceError::Platform("access denied".to_string())));
        script(|s| s.make_fails = Some(OpenError::Platform("no COM".to_string())));
        assert_eq!(source.start(), Err(SourceError::Platform("no COM".to_string())));
        script(|s| s.make_fails = None);
        source.start().unwrap();
        assert!(source.is_running());
        assert!(source.start().is_ok(), "a second start changes nothing");
        assert_eq!(source.stream_count(), 1);
        // Dropped while it runs: the thread ends.
    }

    #[test]
    fn the_volume_of_the_system_is_taken_out() {
        let (mut source, _turn) = source("Headphones");
        let mut reader = source.take_reader().unwrap();
        // The slider stands at a quarter (-12 dB), before the capture point.
        script(|s| s.volume = Some(0.25));
        source.start().unwrap();
        until("the gain", || (source.compensation_db() - 12.04).abs() < 0.01);
        script(|s| s.packets.push_back(Ok(floats(&[0.125, -0.125]))));
        until("the packet", || source.frames_delivered() == 1);
        let (mut left, mut right) = ([0.0f32; 4], [0.0f32; 4]);
        assert_eq!(reader.read(&mut left, &mut right), 1);
        assert_eq!((left[0], right[0]), (0.5, -0.5), "what the player sent");
        // The owner can have the samples as captured.
        source.set_volume_compensation(false);
        until("no gain", || source.compensation_db() == 0.0);
        script(|s| s.packets.push_back(Ok(floats(&[0.125, -0.125]))));
        until("the packet", || source.frames_delivered() == 2);
        reader.read(&mut left, &mut right);
        assert_eq!(left[0], 0.125);
        // A slider at the bottom: at most 60 dB come back.
        source.set_volume_compensation(true);
        script(|s| s.volume = Some(0.0));
        until("the limit", || (source.compensation_db() - 60.0).abs() < 0.01);
        // A system that captures before its volume control: no gain.
        script(|s| s.volume = None);
        until("no gain again", || source.compensation_db() == 0.0);
        source.stop();
    }

    #[test]
    fn the_advice_names_the_reason() {
        let text = CaptureHealth::NoStream("no output device".to_string())
            .advice()
            .unwrap();
        assert_eq!(text, "No sound can be read: no output device.");
        assert_eq!(CaptureHealth::Signal.advice(), None);
        assert_eq!(Timing::default().silent_after, Duration::from_secs(3));
    }
}
