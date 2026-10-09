//! `joseon-probe`: headless checks for JoseonPC. Audio is analyzed in memory
//! only and is never written to disk.
//!
//! ```text
//! joseon-probe demo --seconds N       the demo signal through the engine, in real time
//! joseon-probe analyze FILE.wav       a WAV file through the engine, as fast as it goes
//! joseon-probe devices                the output devices of the system
//! joseon-probe capture --seconds N    the sound the computer plays, through the engine (--raw: as captured)
//! ```
//!
//! Every line of output is one JSON object with its keys in order.
//!
//! Exit codes: 0 ok / signal received, 1 usage or start error, 2 only silence.
//!
//! Port source: `Sources/JoseonProbe/main.swift`. `demo` writes the `reading`
//! and `summary` lines of the macOS probe, without the fields that describe
//! Core Audio devices and taps. `analyze` is new: the macOS probe has no file
//! mode. `devices` and `capture` are the Windows forms of the macOS commands:
//! WASAPI loopback of the default output device. `render` is not here yet.

pub mod wav;

use joseon_core::contracts::{AnalysisFrame, AudioSource, PeakReading, StreamInfo};
use joseon_core::engine::AnalysisEngine;
use joseon_core::test_signals::DemoAudioSource;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::io::Write;
use std::time::{Duration, Instant};

pub const USAGE: &str = "usage:
  joseon-probe demo --seconds N
      The demo signal through the engine in real time. One \"reading\" line every 0.5 s, then a \"summary\".
  joseon-probe analyze FILE.wav [--block FRAMES]
      A WAV file through the engine, as fast as it goes. One \"summary\" line.
      Integer PCM (8, 16, 24, 32 bit) or float (32, 64 bit). Mono goes to both channels.
      --block FRAMES   frames per analysis step (default: one 60th of a second)
  joseon-probe devices
      One \"devices\" line: every output device that is switched on, with the format of its mix.
  joseon-probe capture --seconds N [--raw]
      The sound the computer plays (loopback of the default output device), in real time.
      The Windows volume is taken out of the samples (\"compensationDB\"). --raw: the samples as captured.
      Lines as for demo, with \"health\" and \"advice\" (why there is silence). Play music while it runs.

Not in this build yet: cycle, render, measure-devices, measure-selftest, now-playing.

exit codes: 0 ok, 1 usage or start error, 2 only silence";

/// Commands of the macOS probe that this port does not have yet.
const NOT_YET: [&str; 5] = ["cycle", "render", "measure-devices", "measure-selftest", "now-playing"];

/// `command [positional...] [--key value] [--flag]`
#[derive(Debug, Default, PartialEq)]
pub struct Arguments {
    pub command: String,
    pub positional: Vec<String>,
    pub options: BTreeMap<String, String>,
    pub flags: BTreeSet<String>,
}

impl Arguments {
    /// `args` are the words after the program name.
    pub fn parse(args: &[String]) -> Self {
        let mut parsed = Self {
            command: args.first().cloned().unwrap_or_default(),
            ..Default::default()
        };
        let mut i = 1;
        while i < args.len() {
            if let Some(key) = args[i].strip_prefix("--") {
                match args.get(i + 1).filter(|next| !next.starts_with("--")) {
                    Some(value) => {
                        parsed.options.insert(key.to_string(), value.clone());
                        i += 1;
                    }
                    None => {
                        parsed.flags.insert(key.to_string());
                    }
                }
            } else {
                parsed.positional.push(args[i].clone());
            }
            i += 1;
        }
        parsed
    }

    pub fn double(&self, key: &str, default: f64) -> f64 {
        self.options
            .get(key)
            .and_then(|text| text.parse().ok())
            .unwrap_or(default)
    }

    pub fn int(&self, key: &str, default: usize) -> usize {
        self.options
            .get(key)
            .and_then(|text| text.parse().ok())
            .unwrap_or(default)
    }
}

// MARK: JSON shapes

/// A number with at most `places` decimals. JSON has no NaN or infinity: those are `null`.
fn num(value: f64, places: i32) -> Value {
    if !value.is_finite() {
        return Value::Null;
    }
    let scale = 10f64.powi(places);
    json!((value * scale).round() / scale)
}

fn db(value: f32) -> Value {
    num(value as f64, 3)
}

/// A linear peak as dBFS. `null` for silence.
fn dbfs(linear: f32) -> Value {
    if linear > 0.0 {
        num(20.0 * (linear as f64).log10(), 3)
    } else {
        Value::Null
    }
}

fn stream_json(stream: Option<&StreamInfo>) -> Value {
    match stream {
        None => Value::Null,
        Some(s) => json!({
            "sampleRate": s.sample_rate,
            "channelCount": s.channel_count,
            "deviceName": s.device_name,
            "bitDepth": s.bit_depth,
            "activeSources": s.active_sources,
            "deviceIsDefault": s.device_is_default,
        }),
    }
}

fn peak_json(peak: &PeakReading) -> Value {
    json!({ "hz": db(peak.frequency_hz), "dB": db(peak.level_db), "note": peak.note_name })
}

fn emit(out: &mut dyn Write, line: Value) {
    // A closed pipe is not an error of the probe: the reader went away.
    let _ = writeln!(out, "{line}");
}

fn peak_of(left: &[f32], right: &[f32]) -> f32 {
    left.iter()
        .chain(right)
        .fold(0.0f32, |peak, sample| peak.max(sample.abs()))
}

fn non_zero_frames(left: &[f32], right: &[f32]) -> usize {
    left.iter().zip(right).filter(|(l, r)| **l != 0.0 || **r != 0.0).count()
}

/// The measurement numbers of a frame, shared by both summaries.
fn measurement_json(frame: &AnalysisFrame) -> serde_json::Map<String, Value> {
    let l = &frame.loudness;
    let integrated = |value: f32| if l.is_integrated_valid { db(value) } else { Value::Null };
    let mut map = serde_json::Map::new();
    map.insert("integratedLUFS".into(), integrated(l.integrated_lufs));
    map.insert("truePeakMaxDBTP".into(), db(l.true_peak_max_dbtp));
    map.insert("momentaryMaxLUFS".into(), db(l.momentary_max_lufs));
    map.insert("shortTermMaxLUFS".into(), db(l.short_term_max_lufs));
    map.insert("loudnessRangeLU".into(), integrated(l.loudness_range_lu));
    map.insert("plrDB".into(), integrated(l.plr_db));
    map.insert("clipCount".into(), json!(l.clip_count));
    map.insert("measuredSeconds".into(), num(l.measured_seconds, 3));
    map
}

// MARK: demo

/// The demo signal through the engine in real time.
///
/// One loop drains the ring buffer: it measures the raw peak on its own copy
/// of the samples, then hands the same block to `process_now`. One reader, so
/// the engine never starves.
fn run_demo(seconds: f64, out: &mut dyn Write) -> i32 {
    run_source("demo", &mut DemoAudioSource::new(), seconds, out, &|_| Vec::new())
}

/// The sound the computer plays, through the engine in real time.
fn run_capture(seconds: f64, raw: bool, out: &mut dyn Write) -> i32 {
    let mut source = joseon_capture::system_loopback();
    source.set_volume_compensation(!raw);
    run_source("capture", &mut source, seconds, out, &|source| {
        let health = source.health();
        let volume = joseon_capture::default_output_volume();
        vec![
            ("health", json!(format!("{health:?}"))),
            ("advice", json!(health.advice())),
            ("packets", json!(source.packet_count())),
            ("framesDelivered", json!(source.frames_delivered())),
            ("streamsOpened", json!(source.stream_count())),
            ("lastError", json!(source.last_error())),
            ("compensationDB", num(source.compensation_db() as f64, 2)),
            (
                "volume",
                json!(volume
                    .as_ref()
                    .map(|v| json!({ "dB": v.volume_db, "scalar": v.volume_scalar, "muted": v.is_muted }))),
            ),
        ]
    })
}

/// One line with the output devices.
fn run_devices(out: &mut dyn Write) -> i32 {
    match joseon_capture::output_devices() {
        Ok(devices) => {
            let list: Vec<Value> = devices
                .iter()
                .map(|d| {
                    json!({
                        "name": d.name, "isDefault": d.is_default, "id": d.id,
                        "mixSampleRate": d.mix_sample_rate, "mixChannels": d.mix_channels,
                        "deviceBits": d.device_bits, "deviceSampleRate": d.device_sample_rate,
                    })
                })
                .collect();
            let volume = joseon_capture::default_output_volume().map(|v| {
                json!({
                    "deviceName": v.device_name, "volumeDB": v.volume_db, "maxVolumeDB": v.max_volume_db,
                    "volumeScalar": v.volume_scalar, "muted": v.is_muted, "attenuationDB": v.attenuation_db(),
                    "hardwareVolume": v.hardware_volume,
                })
            });
            emit(
                out,
                json!({ "event": "devices", "count": list.len(), "devices": list, "defaultVolume": volume }),
            );
            0
        }
        Err(error) => {
            emit(out, json!({ "event": "error", "mode": "devices", "error": error }));
            1
        }
    }
}

/// A source through the engine in real time. `extra` adds fields of the source to every line.
fn run_source<S: AudioSource>(
    mode: &str,
    source: &mut S,
    seconds: f64,
    out: &mut dyn Write,
    extra: &dyn Fn(&S) -> Vec<(&'static str, Value)>,
) -> i32 {
    let with_extra = |line: Value, source: &S| {
        let Value::Object(mut map) = line else { return line };
        map.extend(extra(source).into_iter().map(|(key, value)| (key.to_string(), value)));
        Value::Object(map)
    };
    let Some(mut reader) = source.take_reader() else {
        emit(
            out,
            json!({ "event": "error", "mode": mode, "error": "the source has no reader" }),
        );
        return 1;
    };
    let engine = AnalysisEngine::new();
    if let Err(error) = source.start() {
        emit(
            out,
            json!({ "event": "error", "mode": mode, "error": error.to_string() }),
        );
        return 1;
    }
    let mut stream = source.stream_info();
    engine.set_stream_info(stream.clone());
    emit(
        out,
        with_extra(
            json!({ "event": "streamInfo", "mode": mode, "stream": stream_json(stream.as_ref()) }),
            source,
        ),
    );

    let capacity = 1 << 15;
    let (mut left, mut right) = (vec![0.0f32; capacity], vec![0.0f32; capacity]);
    let started = Instant::now();
    let mut next_report = 0.5f64;
    let (mut window_peak, mut total_peak) = (0.0f32, 0.0f32);
    let (mut window_frames, mut total_frames, mut non_zero) = (0usize, 0usize, 0usize);
    let mut frame = engine.latest_frame();
    loop {
        // Empty the ring, but stop at the next deadline: a debug build can run slower than real time.
        let deadline = next_report.min(seconds);
        loop {
            let n = reader.read(&mut left, &mut right);
            if n == 0 {
                break;
            }
            window_peak = window_peak.max(peak_of(&left[..n], &right[..n]));
            non_zero += non_zero_frames(&left[..n], &right[..n]);
            window_frames += n;
            frame = engine.process_now(&left[..n], &right[..n], reader.sample_rate());
            if started.elapsed().as_secs_f64() >= deadline {
                break;
            }
        }
        let t = started.elapsed().as_secs_f64();
        if t >= next_report {
            total_peak = total_peak.max(window_peak);
            total_frames += window_frames;
            // The stream facts change when the device or the playing programs change.
            let now = source.stream_info();
            if now != stream {
                stream = now;
                engine.set_stream_info(stream.clone());
            }
            let line = with_extra(
                json!({
                    "event": "reading", "mode": mode, "t": num(t, 3),
                    "stream": stream_json(stream.as_ref()),
                    "frames": window_frames,
                    "rawPeak": num(window_peak as f64, 6), "rawPeakDBFS": dbfs(window_peak),
                    "momentaryLUFS": db(frame.loudness.momentary_lufs),
                    "correlation": db(frame.stereo.correlation),
                    "topPeak": peak_json(&frame.peak),
                    "engineSilent": frame.is_silent,
                    "hasReceivedSignal": total_peak > 0.0,
                }),
                source,
            );
            emit(out, line);
            window_peak = 0.0;
            window_frames = 0;
            next_report += 0.5;
        }
        if t >= seconds {
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    total_peak = total_peak.max(window_peak); // the part after the last report line
    total_frames += window_frames;
    let last = extra(source);
    source.stop();
    let got_signal = total_peak > 0.0;
    let mut summary = measurement_json(&frame);
    summary.extend(
        [
            ("event", json!("summary")),
            ("mode", json!(mode)),
            ("seconds", num(started.elapsed().as_secs_f64(), 3)),
            ("stream", stream_json(stream.as_ref())),
            ("totalFrames", json!(total_frames)),
            ("nonZeroSamples", json!(non_zero)),
            ("rawPeak", num(total_peak as f64, 6)),
            ("rawPeakDBFS", dbfs(total_peak)),
            ("signalReceived", json!(got_signal)),
            ("result", json!(if got_signal { "signal" } else { "silence" })),
        ]
        .map(|(key, value)| (key.to_string(), value)),
    );
    summary.extend(last.into_iter().map(|(key, value)| (key.to_string(), value)));
    emit(out, Value::Object(summary));
    if got_signal {
        0
    } else {
        2
    }
}

// MARK: analyze

/// A WAV file through the engine, one `process_now` per block.
fn run_analyze(arguments: &Arguments, out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let Some(path) = arguments.positional.first() else {
        let _ = writeln!(err, "analyze: a WAV file is required\n{USAGE}");
        return 1;
    };
    let audio = match std::fs::read(path)
        .map_err(|error| error.to_string())
        .and_then(|bytes| wav::parse(&bytes).map_err(|e| e.to_string()))
    {
        Ok(audio) => audio,
        Err(error) => {
            emit(
                out,
                json!({ "event": "error", "mode": "analyze", "file": path, "error": error }),
            );
            return 1;
        }
    };
    let rate = audio.sample_rate;
    let block = arguments.int("block", (rate / 60.0).round() as usize).clamp(1, 1 << 15);
    let name = std::path::Path::new(path)
        .file_name()
        .map_or_else(|| path.clone(), |n| n.to_string_lossy().into_owned());
    let stream = StreamInfo {
        bit_depth: Some(audio.bit_depth),
        ..StreamInfo::new(rate, audio.channel_count, name)
    };

    let engine = AnalysisEngine::new();
    engine.set_stream_info(Some(stream.clone()));
    let started = Instant::now();
    let mut frame = engine.latest_frame();
    let mut silent_frames = 0usize;
    for (left, right) in audio.left.chunks(block).zip(audio.right.chunks(block)) {
        frame = engine.process_now(left, right, rate);
        silent_frames += frame.is_silent as usize;
    }
    let peak = peak_of(&audio.left, &audio.right);
    let got_signal = peak > 0.0;
    let session = engine.session().snapshot();
    let mut event_counts: BTreeMap<&str, usize> = BTreeMap::new();
    for event in &session.events {
        *event_counts.entry(event.kind.as_str()).or_default() += 1;
    }
    let mut summary = measurement_json(&frame);
    summary.extend(
        [
            ("event", json!("summary")),
            ("mode", json!("analyze")),
            ("file", json!(path)),
            ("stream", stream_json(Some(&stream))),
            ("float", json!(audio.is_float)),
            ("totalFrames", json!(audio.left.len())),
            ("audioSeconds", num(audio.left.len() as f64 / rate, 3)),
            ("analysisSeconds", num(started.elapsed().as_secs_f64(), 3)),
            ("blockFrames", json!(block)),
            ("silentBlocks", json!(silent_frames)),
            ("nonZeroSamples", json!(non_zero_frames(&audio.left, &audio.right))),
            ("rawPeak", num(peak as f64, 6)),
            ("rawPeakDBFS", dbfs(peak)),
            ("momentaryLUFS", db(frame.loudness.momentary_lufs)),
            ("shortTermLUFS", db(frame.loudness.short_term_lufs)),
            ("correlation", db(frame.stereo.correlation)),
            ("balance", db(frame.stereo.balance)),
            ("width", db(frame.stereo.width)),
            ("bandsDB", json!(frame.bands.values().map(db))),
            ("topPeak", peak_json(&frame.peak)),
            (
                "topPeaks",
                json!(frame.top_peaks.iter().map(peak_json).collect::<Vec<_>>()),
            ),
            ("lowestStrongHz", db(frame.lowest_strong_hz)),
            ("sessionSeconds", json!(session.samples.len())),
            ("sessionEvents", json!(event_counts)),
            ("signalReceived", json!(got_signal)),
            ("result", json!(if got_signal { "signal" } else { "silence" })),
        ]
        .map(|(key, value)| (key.to_string(), value)),
    );
    emit(out, Value::Object(summary));
    if got_signal {
        0
    } else {
        2
    }
}

/// Runs the probe. `args` are the words after the program name. Returns the exit code.
pub fn run(args: &[String], out: &mut dyn Write, err: &mut dyn Write) -> i32 {
    let arguments = Arguments::parse(args);
    match arguments.command.as_str() {
        "demo" => run_demo(arguments.double("seconds", 5.0).max(0.5), out),
        "analyze" => run_analyze(&arguments, out, err),
        "devices" => run_devices(out),
        "capture" => run_capture(
            arguments.double("seconds", 5.0).max(0.5),
            arguments.flags.contains("raw"),
            out,
        ),
        command if NOT_YET.contains(&command) => {
            let _ = writeln!(err, "{command}: not in this build yet\n{USAGE}");
            1
        }
        _ => {
            let _ = writeln!(err, "{USAGE}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use joseon_core::test_signals::{demo_block, sine};

    fn words(text: &str) -> Vec<String> {
        text.split_whitespace().map(str::to_string).collect()
    }

    /// Runs the probe and gives the exit code, the JSON lines, and the error text.
    fn probe(args: &[String]) -> (i32, Vec<Value>, String) {
        let (mut out, mut err) = (Vec::new(), Vec::new());
        let code = run(args, &mut out, &mut err);
        let lines = String::from_utf8(out)
            .unwrap()
            .lines()
            .map(|line| serde_json::from_str(line).expect("every line is JSON"))
            .collect();
        (code, lines, String::from_utf8(err).unwrap())
    }

    /// Writes a WAV file for one test and removes it at the end.
    struct TempWav(std::path::PathBuf);

    impl TempWav {
        fn new(name: &str, channels: &[&[f32]], rate: u32, bits: u32) -> Self {
            let path = std::env::temp_dir().join(format!("joseon-probe-test-{}-{name}.wav", std::process::id()));
            std::fs::write(&path, wav::encode(channels, rate, bits)).unwrap();
            Self(path)
        }

        fn analyze(&self, extra: &str) -> (i32, Vec<Value>, String) {
            let mut args = vec!["analyze".to_string(), self.0.to_string_lossy().into_owned()];
            args.extend(words(extra));
            probe(&args)
        }
    }

    impl Drop for TempWav {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.0);
        }
    }

    /// On a system with no capture the two commands say so, as one JSON line, and exit with 1.
    #[cfg(not(windows))]
    #[test]
    fn devices_and_capture_need_windows() {
        for args in ["devices", "capture --seconds 1"] {
            let (code, lines, err) = probe(&words(args));
            assert_eq!((code, err.as_str()), (1, ""), "{args}");
            assert_eq!(lines.len(), 1);
            assert_eq!(lines[0]["event"], "error");
            assert!(lines[0]["error"].as_str().unwrap().contains("needs Windows"));
        }
    }

    /// On Windows the device list is one line, with or without devices. The capture runs or says why not.
    #[cfg(windows)]
    #[test]
    fn devices_and_capture_answer_on_windows() {
        let (code, lines, _) = probe(&words("devices"));
        assert_eq!(lines.len(), 1, "{lines:?}");
        assert!(code == 0 && lines[0]["event"] == "devices" || code == 1 && lines[0]["event"] == "error");
        let (code, lines, _) = probe(&words("capture --seconds 1"));
        assert!(!lines.is_empty());
        // 0 = sound, 2 = silence (nothing plays on a test machine), 1 = no device.
        assert!((0..=2).contains(&code));
        if code != 1 {
            let summary = lines.last().unwrap();
            assert_eq!(summary["event"], "summary");
            assert!(summary["health"].is_string() && summary["streamsOpened"].as_u64().unwrap() >= 1);
        }
    }

    #[test]
    fn arguments_take_options_flags_and_positionals() {
        let a = Arguments::parse(&words(
            "render --panel spectrum file.png --side --seconds 2.5 --realtime",
        ));
        assert_eq!(a.command, "render");
        assert_eq!(a.positional, ["file.png"]);
        assert_eq!(a.options.get("panel").map(String::as_str), Some("spectrum"));
        assert!(a.flags.contains("side") && a.flags.contains("realtime"));
        assert_eq!(a.double("seconds", 5.0), 2.5);
        assert_eq!(a.double("missing", 5.0), 5.0);
        assert_eq!(a.int("seconds", 7), 7, "2.5 is not a whole number");
        assert_eq!(Arguments::parse(&[]), Arguments::default());
    }

    #[test]
    fn numbers_are_rounded_and_never_nan() {
        assert_eq!(num(1.23456, 3), json!(1.235));
        assert_eq!(num(-18.61, 3), json!(-18.61));
        assert_eq!(num(f64::NAN, 3), Value::Null);
        assert_eq!(num(f64::INFINITY, 3), Value::Null);
        assert_eq!(dbfs(0.0), Value::Null);
        assert_eq!(dbfs(1.0), json!(0.0));
        assert_eq!(dbfs(0.5), json!(-6.021));
    }

    #[test]
    fn usage_and_unknown_commands_exit_with_one() {
        for args in ["", "nonsense", "cycle --seconds 1", "render --panel spectrum"] {
            let (code, lines, err) = probe(&words(args));
            assert_eq!(code, 1, "{args}");
            assert!(lines.is_empty());
            assert!(err.contains("usage:"), "{args}: {err}");
        }
        assert!(probe(&words("cycle")).2.starts_with("cycle: not in this build yet"));
        let (code, _, err) = probe(&words("analyze"));
        assert_eq!(code, 1);
        assert!(err.contains("a WAV file is required"));
    }

    #[test]
    fn a_missing_or_bad_file_is_an_error_line() {
        let (code, lines, _) = probe(&words("analyze /no/such/file.wav"));
        assert_eq!(code, 1);
        assert_eq!(lines[0]["event"], "error");
        let path = std::env::temp_dir().join(format!("joseon-probe-test-{}-bad.wav", std::process::id()));
        std::fs::write(&path, b"this is not audio").unwrap();
        let (code, lines, _) = probe(&["analyze".to_string(), path.to_string_lossy().into_owned()]);
        let _ = std::fs::remove_file(&path);
        assert_eq!(code, 1);
        assert_eq!(lines[0]["error"], "not a RIFF/WAVE file");
    }

    /// A −20 dBFS 1 kHz tone at 48 kHz, 16 bit: the numbers a meter must show.
    #[test]
    fn analyze_reads_a_tone() {
        let tone = sine(1_000.0, 0.1, 48_000.0, 6.0, 0.0);
        let file = TempWav::new("tone", &[&tone, &tone], 48_000, 16);
        let (code, lines, err) = file.analyze("");
        assert_eq!((code, lines.len(), err.as_str()), (0, 1, ""));
        let s = &lines[0];
        assert_eq!(
            (s["event"].as_str(), s["mode"].as_str(), s["result"].as_str()),
            (Some("summary"), Some("analyze"), Some("signal"))
        );
        assert_eq!(s["stream"]["sampleRate"], 48_000.0);
        assert_eq!(s["stream"]["channelCount"], 2);
        assert_eq!(s["stream"]["bitDepth"], 16);
        assert_eq!(s["totalFrames"], 288_000);
        assert_eq!(s["audioSeconds"], 6.0);
        assert_eq!(s["blockFrames"], 800);
        assert_eq!(s["silentBlocks"], 0);
        let near = |key: &str, expected: f64, accuracy: f64| {
            let value = s[key].as_f64().unwrap_or_else(|| panic!("{key} is {}", s[key]));
            assert!(
                (value - expected).abs() <= accuracy,
                "{key}: {value}, expected {expected}"
            );
        };
        near("integratedLUFS", -20.0, 0.1); // two channels of −23 LUFS each
        near("truePeakMaxDBTP", -20.0, 0.1);
        near("rawPeakDBFS", -20.0, 0.01);
        near("correlation", 1.0, 0.001);
        near("width", 0.0, 0.001);
        near("loudnessRangeLU", 0.0, 0.2);
        near("lowestStrongHz", 1_000.0, 50.0);
        near("measuredSeconds", 6.0, 0.001);
        assert_eq!(s["clipCount"], 0);
        assert_eq!(s["topPeak"]["note"], "B5");
        assert!((s["topPeak"]["hz"].as_f64().unwrap() - 1_000.0).abs() < 1.0);
        assert_eq!(s["topPeaks"].as_array().unwrap().len(), 1);
        assert_eq!(s["bandsDB"].as_array().unwrap().len(), 8);
        assert!(
            (s["bandsDB"][3].as_f64().unwrap() + 20.0).abs() < 0.3,
            "1 kHz is in the mid band"
        );
        // 360 steps of a 60th of a second do not add up to 6.0 exactly: the last second can stay open.
        assert!((5..=6).contains(&s["sessionSeconds"].as_u64().unwrap()));
    }

    #[test]
    fn analyze_of_silence_exits_with_two_and_null_levels() {
        let quiet = vec![0.0f32; 48_000];
        let file = TempWav::new("silence", &[&quiet], 48_000, 24);
        let (code, lines, _) = file.analyze("--block 4800");
        assert_eq!(code, 2);
        let s = &lines[0];
        assert_eq!(
            (s["result"].as_str(), s["signalReceived"].as_bool()),
            (Some("silence"), Some(false))
        );
        assert_eq!(
            s["integratedLUFS"],
            Value::Null,
            "nothing passed the gate: not a number"
        );
        assert_eq!(s["rawPeakDBFS"], Value::Null);
        assert_eq!(
            (s["blockFrames"].as_u64(), s["silentBlocks"].as_u64()),
            (Some(4800), Some(10))
        );
        assert_eq!(s["stream"]["channelCount"], 1);
    }

    /// The same audio from a float file and through the engine directly.
    #[test]
    fn analyze_matches_the_engine() {
        let (left, right) = demo_block(0, 44_100 * 4, 44_100.0);
        let file = TempWav::new("demo", &[&left, &right], 44_100, 32);
        let (code, lines, _) = file.analyze("");
        assert_eq!(code, 0);
        let s = &lines[0];
        assert_eq!(
            (s["float"].as_bool(), s["blockFrames"].as_u64()),
            (Some(true), Some(735))
        );

        let engine = AnalysisEngine::new();
        let mut frame = engine.latest_frame();
        for (l, r) in left.chunks(735).zip(right.chunks(735)) {
            frame = engine.process_now(l, r, 44_100.0);
        }
        assert_eq!(s["integratedLUFS"], db(frame.loudness.integrated_lufs));
        assert_eq!(s["truePeakMaxDBTP"], db(frame.loudness.true_peak_max_dbtp));
        assert_eq!(s["correlation"], db(frame.stereo.correlation));
        assert_eq!(s["topPeak"], peak_json(&frame.peak));
        assert_eq!(s["topPeaks"].as_array().unwrap().len(), frame.top_peaks.len());
        assert_eq!(s["sessionEvents"], json!({}));
    }

    /// A hard-clipped file: the clip count and the session events say so.
    #[test]
    fn analyze_reports_clips() {
        let hot: Vec<f32> = sine(440.0, 2.0, 48_000.0, 2.0, 0.0)
            .iter()
            .map(|x| x.clamp(-1.0, 1.0))
            .collect();
        let file = TempWav::new("clip", &[&hot, &hot], 48_000, 32);
        let (_, lines, _) = file.analyze("");
        let s = &lines[0];
        assert!(s["clipCount"].as_u64().unwrap() > 1_000);
        assert!(s["sessionEvents"]["clip"].as_u64().unwrap() >= 1);
        assert!(s["sessionEvents"]["interSampleOver"].as_u64().unwrap() >= 1);
        assert!(s["truePeakMaxDBTP"].as_f64().unwrap() > 0.0);
    }

    /// The demo mode in real time, for a little over half a second.
    #[test]
    fn demo_writes_readings_and_a_summary() {
        let (code, lines, err) = probe(&words("demo --seconds 0.7"));
        assert_eq!((code, err.as_str()), (0, ""));
        assert_eq!(lines[0]["event"], "streamInfo");
        assert_eq!(lines[0]["stream"]["deviceName"], "Demo signal");
        let readings: Vec<&Value> = lines.iter().filter(|line| line["event"] == "reading").collect();
        assert!(!readings.is_empty());
        let r = readings[0];
        assert_eq!(
            (
                r["mode"].as_str(),
                r["engineSilent"].as_bool(),
                r["hasReceivedSignal"].as_bool()
            ),
            (Some("demo"), Some(false), Some(true))
        );
        assert!(
            r["frames"].as_u64().unwrap() >= 4_800,
            "{} frames in half a second",
            r["frames"]
        );
        assert!(r["rawPeakDBFS"].as_f64().unwrap() < 0.0);
        let s = lines.last().unwrap();
        assert_eq!(
            (s["event"].as_str(), s["result"].as_str()),
            (Some("summary"), Some("signal"))
        );
        assert!(s["totalFrames"].as_u64().unwrap() >= 9_600);
        assert!(s["seconds"].as_f64().unwrap() >= 0.7);
        assert!(s["truePeakMaxDBTP"].as_f64().unwrap() < 0.0);
    }
}
