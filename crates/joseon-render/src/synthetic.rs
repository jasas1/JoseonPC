//! Music-like `AnalysisFrame` sequences for the demo mode, design review
//! renders and tests. This is a drawing aid: the numbers are plausible,
//! deterministic and synthetic. They are not measurements.
//!
//! Port source: `Sources/JoseonRender/SyntheticFrames.swift`.

// The macOS source writes 1.4142, and the parity test needs the same number.
#![allow(clippy::approx_constant)]

use crate::fmt;
use joseon_core::contracts::{
    AnalysisFrame, BandEnergy, HeadphoneReading, LoudnessReading, PeakReading, SpectrumReading, SplReading,
    StereoReading, StreamInfo, StressFlag, StressSeverity, ThirdOctaveReading,
};
use std::f32::consts::PI;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Options {
    /// Display bins per spectrum.
    pub bin_count: usize,
    pub frames_per_second: f64,
    /// Adds a `HeadphoneReading` with a made-up response and target curve.
    pub include_headphone: bool,
    /// Overall level offset in dB. 0 gives about -13 LUFS.
    pub level_db: f32,
    /// Pushes true peak over 0 dBTP and counts clips, to show the red states.
    pub hot: bool,
    pub seed: u64,
    /// Scope points per frame.
    pub scope_point_count: usize,
    /// Adds `third_octave` and a made-up `spl` reading (see `demo_spl`), as if a calibration were set.
    pub include_spl: bool,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            bin_count: 1024,
            frames_per_second: 60.0,
            include_headphone: false,
            level_db: 0.0,
            hot: false,
            seed: 0x4A6F_7365_6F6E,
            scope_point_count: 1024,
            include_spl: false,
        }
    }
}

struct Partial {
    hz: f32,
    amp: f32,
    pan: f32,
    width_oct: f32,
}

const CHORDS: [[i32; 5]; 4] = [
    [45, 57, 60, 64, 69],
    [41, 53, 57, 60, 65],
    [48, 55, 60, 64, 67],
    [43, 55, 59, 62, 67],
];
const MELODY: [i32; 16] = [76, 79, 81, 79, 76, 72, 74, 76, 81, 84, 83, 79, 76, 74, 72, 74];
const NOTE_NAMES: [&str; 12] = ["C", "C#", "D", "D#", "E", "F", "F#", "G", "G#", "A", "A#", "B"];

fn midi_hz(m: f32) -> f32 {
    440.0 * 2f32.powf((m - 69.0) / 12.0)
}

fn note_name(midi: f32) -> String {
    let idx = midi as i64;
    if idx >= 0 {
        format!("{}{}", NOTE_NAMES[(idx % 12) as usize], idx / 12 - 1)
    } else {
        String::new()
    }
}

/// Running state of the made-up SPL reading: the slow level, the Leq sums, the
/// maximum and the dose. It starts in the middle of a listening session (3.5 h
/// at about 78 dB(A)), so the dose shows a value a listener would see.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DemoSplState {
    slow_power: f64,
    track_power: f64,
    track_seconds: f64,
    session_power: f64,
    session_seconds: f64,
    max_fast: f32,
    dose_niosh: f64,
    dose_who: f64,
}

impl Default for DemoSplState {
    fn default() -> Self {
        Self {
            slow_power: 0.0,
            track_power: 0.0,
            track_seconds: 0.0,
            session_power: 10f64.powf(7.7) * 12_600.0,
            session_seconds: 12_600.0,
            max_fast: 0.0,
            dose_niosh: 0.087,
            dose_who: 0.31,
        }
    }
}

pub struct SyntheticFrames {
    pub options: Options,
    time: f64,
    n: usize,
    freqs: Vec<f32>,
    /// log2(f)
    log_f: Vec<f32>,
    smooth_l: Vec<f32>,
    smooth_r: Vec<f32>,
    smooth_m: Vec<f32>,
    smooth_s: Vec<f32>,
    hold: Vec<f32>,
    avg_power: Vec<f64>,
    frame_index: u64,

    lufs_m: f32,
    lufs_s: f32,
    integrated_power: f64,
    integrated_count: f64,
    max_m: f32,
    max_s: f32,
    tp_max: f32,
    tp_hold_l: f32,
    tp_hold_r: f32,
    short_term_history: Vec<f32>,
    clip_count: u64,
    spl_state: DemoSplState,
}

pub const DEMO_CALIBRATION_NAME: &str = "WA33 at 10 o'clock";
/// Momentary loudness in LUFS + this = the made-up A-weighted level.
const DEMO_SPL_OFFSET_DB: f32 = 93.0;
/// Third-octave level in dBFS RMS + this = the made-up band level at the eardrum in dB SPL.
const DEMO_BAND_OFFSET_DB: f32 = 107.0;

impl SyntheticFrames {
    pub fn new(options: Options) -> Self {
        let n = options.bin_count.max(16);
        let freqs = SpectrumReading::silent(n).frequencies;
        let log_f = freqs.iter().map(|f| f.log2()).collect();
        let floor = vec![SpectrumReading::FLOOR_DB; n];
        Self {
            options,
            time: 0.0,
            n,
            freqs,
            log_f,
            smooth_l: floor.clone(),
            smooth_r: floor.clone(),
            smooth_m: floor.clone(),
            smooth_s: floor.clone(),
            hold: floor,
            avg_power: vec![0.0; n],
            frame_index: 0,
            lufs_m: -70.0,
            lufs_s: -70.0,
            integrated_power: 0.0,
            integrated_count: 0.0,
            max_m: -120.0,
            max_s: -120.0,
            tp_max: -120.0,
            tp_hold_l: -120.0,
            tp_hold_r: -120.0,
            short_term_history: Vec::with_capacity(4096),
            clip_count: 0,
            spl_state: DemoSplState::default(),
        }
    }

    pub fn time(&self) -> f64 {
        self.time
    }

    /// `count` frames, 1 / frames_per_second apart, starting at host time 1000.
    pub fn sequence(count: usize, options: Options) -> Vec<AnalysisFrame> {
        let mut g = SyntheticFrames::new(options);
        (0..count).map(|_| g.next_frame()).collect()
    }

    /// Digital silence.
    pub fn silence(count: usize, bin_count: usize, frames_per_second: f64) -> Vec<AnalysisFrame> {
        (0..count)
            .map(|i| {
                let mut frame = AnalysisFrame::new(SpectrumReading::silent(bin_count));
                frame.host_time = 1000.0 + i as f64 / frames_per_second;
                frame.stream = Some(stream("Synthetic", None, &[]));
                frame
            })
            .collect()
    }

    // MARK: Noise

    fn hash(&self, a: i64, b: i64, c: i64) -> f32 {
        let mut x = (a as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15);
        x ^= (b as u64).wrapping_mul(0xC2B2_AE3D_27D4_EB4F);
        x ^= (c as u64).wrapping_mul(0x1656_67B1_9E37_79F9);
        x ^= self.options.seed;
        x ^= x >> 29;
        x = x.wrapping_mul(0xBF58_476D_1CE4_E5B9);
        x ^= x >> 32;
        x = x.wrapping_mul(0x94D0_49BB_1331_11EB);
        x ^= x >> 29;
        (x >> 40) as f32 / (1u32 << 24) as f32 * 2.0 - 1.0
    }

    /// Smooth value noise over (u, v), -1...1.
    fn noise(&self, u: f32, v: f32, channel: i64) -> f32 {
        let (iu, iv) = (u.floor() as i64, v.floor() as i64);
        let (mut fu, mut fv) = (u - iu as f32, v - iv as f32);
        fu = fu * fu * (3.0 - 2.0 * fu);
        fv = fv * fv * (3.0 - 2.0 * fv);
        let (a, b) = (self.hash(iu, iv, channel), self.hash(iu + 1, iv, channel));
        let (c, d) = (self.hash(iu, iv + 1, channel), self.hash(iu + 1, iv + 1, channel));
        (a + (b - a) * fu) + ((c + (d - c) * fu) - (a + (b - a) * fu)) * fv
    }

    // MARK: Music model

    fn partials(&self, t: f64) -> Vec<Partial> {
        let mut out = Vec::with_capacity(160);
        let bar = (t / 2.0) as usize;
        let chord = CHORDS[bar % CHORDS.len()];
        let t_in_bar = (t - bar as f64 * 2.0) as f32;
        // Bass: root, plucked every beat.
        let beat = ((t * 2.0) as f32) % 1.0;
        let bass_env = (-beat * 2.2).exp() * 0.9 + 0.1;
        let root = midi_hz(chord[0] as f32 - 12.0);
        for h in 1..=10 {
            let a = 0.13 * bass_env * (h as f32).powf(-1.35);
            out.push(Partial {
                hz: root * h as f32,
                amp: a,
                pan: 0.0,
                width_oct: 0.018,
            });
        }
        // Pad: chord tones with slow swell, spread across the field.
        let swell = 0.55 + 0.45 * (t_in_bar * PI / 2.0).sin();
        for (k, m) in chord[1..].iter().enumerate() {
            let f0 = midi_hz(*m as f32);
            let pan = [-0.55f32, 0.45, -0.25, 0.6][k % 4];
            for h in 1..=9 {
                let a = 0.060 * swell * (h as f32).powf(-1.1) * if h % 2 == 0 { 0.6 } else { 1.0 };
                out.push(Partial {
                    hz: f0 * h as f32 * (1.0 + 0.0006 * k as f32),
                    amp: a,
                    pan,
                    width_oct: 0.012,
                });
            }
        }
        // Melody: a new note every half beat, with vibrato and a decaying envelope.
        let step = (t * 4.0) as usize;
        let t_in_step = (t * 4.0 - step as f64) as f32;
        let note = MELODY[step % MELODY.len()];
        let vib = 0.12 * (t as f32 * 2.0 * PI * 5.2).sin();
        let f0 = midi_hz(note as f32 + vib);
        let env = (-t_in_step * 1.4).exp() * (1.0 - (-t_in_step * 40.0).exp());
        let pan = 0.25 * (t as f32 * 0.7).sin();
        for h in 1..=14 {
            let a = 0.17 * env * (h as f32).powf(-1.25) * (1.0 + 0.5 * (h as f32 * 1.7).sin());
            out.push(Partial {
                hz: f0 * h as f32,
                amp: a.abs(),
                pan,
                width_oct: 0.010,
            });
        }
        out
    }

    // MARK: Frame

    pub fn next_frame(&mut self) -> AnalysisFrame {
        let options = self.options;
        let n = self.n;
        let dt = 1.0 / options.frames_per_second;
        self.time += dt;
        self.frame_index += 1;
        let t = self.time;
        let tf = t as f32;
        let gain = 10f32.powf(options.level_db / 20.0) * if options.hot { 1.9 } else { 1.0 };

        let (mut amp_l, mut amp_r) = (vec![0f32; n], vec![0f32; n]);

        // Percussion envelopes (120 bpm).
        let beat_pos = ((t * 2.0) as f32) % 1.0;
        let beat_index = (t * 2.0) as i64;
        let kick = (-beat_pos * 9.0).exp();
        let snare = if beat_index % 2 == 1 {
            (-beat_pos * 7.0).exp()
        } else {
            0.0
        };
        let eighth = ((t * 4.0) as f32) % 1.0;
        let hat = (-eighth * 16.0).exp() * if (t * 4.0) as i64 % 2 == 1 { 1.0 } else { 0.55 };

        // Broadband bed: pink-ish tilt, slow tonal drift, fine texture that moves in time.
        let bell = |lf: f32, hz: f32, width: f32| (-((lf - hz.log2()) / width).powf(2.0)).exp();
        for i in 0..n {
            let (f, lf) = (self.freqs[i], self.log_f[i]);
            let mut db = -40.0 - 3.0 * (lf - 100f32.log2());
            if f < 45.0 {
                db -= 20.0 * (45.0 / f).log2();
            }
            if f > 15_000.0 {
                db -= 34.0 * (f / 15_000.0).log2();
            }
            db += 3.5 * self.noise(lf * 0.9, tf * 0.18, 1);
            let tex_l = 1.6 * self.noise(lf * 3.0, tf * 0.9, 2) + 0.9 * self.noise(lf * 16.0, tf * 5.0, 3);
            let tex_r = 1.6 * self.noise(lf * 3.0, tf * 0.9, 4) + 0.9 * self.noise(lf * 16.0, tf * 5.0, 5);
            let shared = 2.4 * self.noise(lf * 8.0, tf * 3.2, 6) + 1.5 * self.noise(lf * 24.0, tf * 6.0, 10);
            let mut bed = 10f32.powf(db / 20.0);
            // Kick body, snare band, hats.
            let kick_shape = bell(lf, 58.0, 0.55);
            let snare_shape = bell(lf, 1900.0, 1.9) + 0.8 * bell(lf, 190.0, 0.35);
            let hat_shape = bell(lf, 9500.0, 0.9);
            let perc = 0.085 * kick * kick_shape + 0.030 * snare * snare_shape + 0.014 * hat * hat_shape;
            bed += 0.004 * kick * bell(lf, 3000.0, 2.5);
            amp_l[i] = (bed * 10f32.powf((tex_l + shared) / 20.0) + perc * (1.0 + 0.04 * tex_l)) * gain;
            amp_r[i] = (bed * 10f32.powf((tex_r + shared) / 20.0) + perc * (1.0 - 0.04 * tex_r)) * gain;
        }

        // Harmonic partials as narrow peaks.
        let lf0 = self.log_f[0];
        let lf_span = self.log_f[n - 1] - self.log_f[0];
        for p in self.partials(t) {
            if !(p.hz < 21_000.0) {
                continue;
            }
            let center = (p.hz.log2() - lf0) / lf_span * (n - 1) as f32;
            // Peak width grows toward the lows, like an FFT with fixed Hz resolution.
            let width_oct = p.width_oct.max(2.2 / p.hz);
            let width_bins = width_oct / lf_span * (n - 1) as f32;
            let reach = (width_bins * 3.5) as i64 + 1;
            let lo = (center as i64 - reach).max(0);
            let hi = (center as i64 + reach + 1).min(n as i64 - 1);
            if lo > hi {
                continue;
            }
            let angle = (p.pan + 1.0) * PI / 4.0;
            let (gl, gr) = (angle.cos() * 1.4142, angle.sin() * 1.4142);
            for i in lo..=hi {
                let d = (i as f32 - center) / width_bins;
                let a = p.amp * (-0.5 * d * d).exp() * gain;
                amp_l[i as usize] += a * gl;
                amp_r[i as usize] += a * gr;
            }
        }

        // To dB with instant attack and about 0.25 s release, like the real display smoothing.
        let release = dt as f32 * 46.0;
        let hold_decay = dt as f32 * 12.0;
        let (mut best_bin, mut best_db) = (0usize, -200f32);
        let to_db = |v: f32| (20.0 * v.max(1e-7).log10()).max(-120.0);
        for i in 0..n {
            let (l, r) = (amp_l[i], amp_r[i]);
            let m = (l + r) * 0.5 * 0.94;
            let s = (l - r).abs() * 0.5 + m * 0.22;
            self.smooth_l[i] = to_db(l).max(self.smooth_l[i] - release);
            self.smooth_r[i] = to_db(r).max(self.smooth_r[i] - release);
            self.smooth_m[i] = to_db(m).max(self.smooth_m[i] - release);
            self.smooth_s[i] = to_db(s).max(self.smooth_s[i] - release);
            self.hold[i] = self.smooth_m[i].max(self.hold[i] - hold_decay);
            let pw = (m * m) as f64;
            self.avg_power[i] += (pw - self.avg_power[i]) / self.frame_index.min(1800) as f64;
            if self.freqs[i] > 30.0 && self.smooth_m[i] > best_db {
                best_db = self.smooth_m[i];
                best_bin = i;
            }
        }
        let average: Vec<f32> = self
            .avg_power
            .iter()
            .map(|p| (10.0 * p.max(1e-14).log10()).max(-120.0) as f32)
            .collect();
        let spectrum = SpectrumReading {
            frequencies: self.freqs.clone(),
            left: self.smooth_l.clone(),
            right: self.smooth_r.clone(),
            mid: self.smooth_m.clone(),
            side: self.smooth_s.clone(),
            peak_hold: self.hold.clone(),
            average,
            mid_layers: Vec::new(),
        };

        // Peak with parabolic refinement.
        let mut peak = PeakReading::default();
        if best_db > -90.0 && best_bin > 0 && best_bin < n - 1 {
            let (a, b, c) = (
                self.smooth_m[best_bin - 1],
                self.smooth_m[best_bin],
                self.smooth_m[best_bin + 1],
            );
            let denom = a - 2.0 * b + c;
            let off = if denom.abs() > 1e-6 {
                (0.5 * (a - c) / denom).max(-0.5).min(0.5)
            } else {
                0.0
            };
            let hz = 2f32.powf(self.log_f[best_bin] + off * lf_span / (n - 1) as f32);
            let midi = 69.0 + 12.0 * (hz / 440.0).log2();
            let nearest = midi.round();
            peak = PeakReading {
                frequency_hz: hz,
                level_db: b,
                note_name: note_name(nearest),
                cents: (midi - nearest) * 100.0,
            };
        }

        // Band energy from the instantaneous mid power.
        let edges = BandEnergy::EDGES_HZ;
        let mut band_values = [-120f32; 8];
        let mut total = 0f64;
        for b in 0..8 {
            let (mut sum, mut count) = (0f64, 0usize);
            for i in 0..n {
                if self.freqs[i] >= edges[b] && self.freqs[i] < edges[b + 1] {
                    let m = ((amp_l[i] + amp_r[i]) * 0.5) as f64;
                    sum += m * m;
                    count += 1;
                }
            }
            if count == 0 {
                continue;
            }
            let octaves = (edges[b + 1] / edges[b]).log2() as f64;
            let power = sum / count as f64 * octaves * 9.0;
            total += power;
            band_values[b] = (10.0 * power.max(1e-14).log10()).max(-120.0) as f32;
        }
        let bands = BandEnergy {
            sub_bass: band_values[0],
            bass: band_values[1],
            low_mid: band_values[2],
            mid: band_values[3],
            upper_mid: band_values[4],
            presence: band_values[5],
            brilliance: band_values[6],
            air: band_values[7],
        };

        // Loudness numbers that move like a meter.
        let instant = (10.0 * total.max(1e-14).log10()) as f32 + 3.5;
        let k_m = (1.0 - (-dt / 0.4).exp()) as f32;
        let k_s = (1.0 - (-dt / 3.0).exp()) as f32;
        self.lufs_m = if self.lufs_m < -69.0 {
            instant
        } else {
            self.lufs_m + (instant - self.lufs_m) * k_m
        };
        self.lufs_s = if self.lufs_s < -69.0 {
            instant
        } else {
            self.lufs_s + (self.lufs_m - self.lufs_s) * k_s
        };
        self.integrated_power += 10f64.powf(self.lufs_m as f64 / 10.0);
        self.integrated_count += 1.0;
        let integrated = (10.0 * (self.integrated_power / self.integrated_count).log10()) as f32;
        self.max_m = self.max_m.max(self.lufs_m);
        self.max_s = self.max_s.max(self.lufs_s);
        if self.frame_index % 6 == 0 && self.short_term_history.len() < 4096 {
            self.short_term_history.push(self.lufs_s);
        }
        let mut lra = 0f32;
        if self.short_term_history.len() > 30 {
            let mut sorted = self.short_term_history.clone();
            sorted.sort_by(f32::total_cmp);
            let last = (sorted.len() - 1) as f32;
            lra = sorted[(last * 0.95) as usize] - sorted[(last * 0.10) as usize];
        }
        let crest_l = 8.5 + 3.5 * kick + 1.5 * self.noise(tf * 7.0, 0.5, 7);
        let crest_r = 8.5 + 3.2 * kick + 1.5 * self.noise(tf * 7.0, 3.5, 8);
        let balance_db = 0.6 * self.noise(tf * 0.3, 9.5, 9);
        let tp_l = (self.lufs_m + crest_l - balance_db).min(if options.hot { 0.8 } else { -0.3 });
        let tp_r = (self.lufs_m + crest_r + balance_db).min(if options.hot { 0.6 } else { -0.4 });
        self.tp_hold_l = tp_l.max(self.tp_hold_l - dt as f32 * 20.0);
        self.tp_hold_r = tp_r.max(self.tp_hold_r - dt as f32 * 20.0);
        self.tp_max = self.tp_max.max(self.tp_hold_l).max(self.tp_hold_r);
        if options.hot && tp_l > 0.5 && beat_pos < 0.02 {
            self.clip_count += 1;
        }

        let loudness = LoudnessReading {
            momentary_lufs: self.lufs_m,
            short_term_lufs: self.lufs_s,
            integrated_lufs: integrated,
            momentary_max_lufs: self.max_m,
            short_term_max_lufs: self.max_s,
            loudness_range_lu: lra,
            true_peak_left_dbtp: self.tp_hold_l,
            true_peak_right_dbtp: self.tp_hold_r,
            true_peak_max_dbtp: self.tp_max,
            rms_left_db: self.lufs_m + 2.2 - balance_db,
            rms_right_db: self.lufs_m + 2.0 + balance_db,
            plr_db: self.tp_max - integrated,
            psr_db: self.tp_hold_l.max(self.tp_hold_r) - self.lufs_s,
            clip_count: self.clip_count,
            measured_seconds: t,
            is_integrated_valid: true,
        };

        let stereo = self.make_stereo(t, kick, hat, 10f32.powf((self.lufs_m + 14.0) / 20.0));

        let headphone = options.include_headphone.then(|| {
            let response: Vec<f32> = self.log_f.iter().map(|&lf| demo_response(lf)).collect();
            let target: Vec<f32> = self.log_f.iter().map(|&lf| demo_target(lf)).collect();
            let predicted = (0..n).map(|i| (self.smooth_m[i] + response[i]).max(-120.0)).collect();
            HeadphoneReading {
                model_name: "Demo headphone".to_string(),
                response_db: response,
                target_db: target,
                has_target: true,
                predicted_at_ear_db: predicted,
                stress_flags: demo_stress_flags(),
            }
        });

        let mut frame = AnalysisFrame::new(spectrum);
        frame.host_time = 1000.0 + t;
        frame.stream = Some(stream("Synthetic", Some(24), &["Joseon demo"]));
        frame.peak = peak;
        frame.bands = bands;
        frame.loudness = loudness;
        frame.stereo = stereo;
        frame.headphone = headphone;
        frame.is_silent = false;
        frame.top_peaks = demo_top_peaks(&frame.spectrum, Some(&frame.peak));
        frame.lowest_strong_hz = demo_lowest_strong(&frame.spectrum);
        if options.include_spl {
            let bands = demo_third_octave(&frame.spectrum);
            frame.spl = Some(demo_spl(&frame.loudness, Some(&bands), dt, &mut self.spl_state));
            frame.third_octave = Some(bands);
        }
        frame
    }

    // MARK: Stereo field

    fn make_stereo(&self, t: f64, kick: f32, hat: f32, level: f32) -> StereoReading {
        let count = self.options.scope_point_count.max(16);
        let mut pts = vec![[0f32; 2]; count];
        let sr = 48_000.0f64;
        let bar = (t / 2.0) as usize;
        let chord = CHORDS[bar % CHORDS.len()];
        let bass_hz = midi_hz(chord[0] as f32 - 12.0) as f64;
        let hz = |index: usize| midi_hz(chord[index] as f32) as f64;
        // hz, amp, pan, phase
        let tones: [(f64, f32, f32, f64); 7] = [
            (hz(1), 0.16, -0.75, 0.3),
            (hz(2), 0.15, 0.7, 1.1),
            (hz(3), 0.13, -0.45, 2.0),
            (hz(4) * 2.0, 0.10, 0.85, 0.7),
            (hz(2) * 3.01, 0.07, -0.9, 1.9),
            (hz(3) * 4.02, 0.05, 0.95, 2.6),
            (
                midi_hz(MELODY[(t * 4.0) as usize % MELODY.len()] as f32) as f64,
                0.14,
                0.3 * (t * 0.7).sin() as f32,
                0.0,
            ),
        ];
        let (mut sum_lr, mut sum_ll, mut sum_rr, mut sum_mm, mut sum_ss) = (0f32, 0f32, 0f32, 0f32, 0f32);
        let base = (t * sr) as i64;
        let tau = 2.0 * std::f64::consts::PI;
        for (i, point) in pts.iter_mut().enumerate() {
            let ts = (base + i as i64) as f64 / sr;
            let bass = (tau * bass_hz * ts).sin() as f32 * (0.13 + 0.16 * kick);
            let (mut l, mut r) = (bass, bass);
            for (tone_hz, amp, pan, phase) in tones {
                let angle = (pan + 1.0) * PI / 4.0;
                // A small inter-channel phase offset gives the pad its width.
                l += amp * angle.cos() * 1.4142 * (tau * tone_hz * ts + phase).sin() as f32;
                r += amp * angle.sin() * 1.4142 * (tau * tone_hz * ts + phase + 1.5 * pan as f64).sin() as f32;
            }
            let sample = base + i as i64;
            let (nl, nr, nc) = (
                self.hash(sample, 11, 1),
                self.hash(sample, 12, 2),
                self.hash(sample, 13, 3),
            );
            let air = 0.012 + 0.05 * hat;
            l += air * (0.9 * nl + 0.3 * nc);
            r += air * (0.9 * nr + 0.3 * nc);
            l *= level * 1.15;
            r *= level * 1.15;
            let (m, s) = ((l + r) * 0.5, (l - r) * 0.5);
            // Contract mapping, the same as the stereo analyzer: x = (R - L) / 2, y = (L + R) / 2.
            // The clamp only catches input over full scale.
            *point = [((r - l) * 0.5).max(-1.0).min(1.0), ((l + r) * 0.5).max(-1.0).min(1.0)];
            sum_lr += l * r;
            sum_ll += l * l;
            sum_rr += r * r;
            sum_mm += m * m;
            sum_ss += s * s;
        }
        let corr = sum_lr / (sum_ll * sum_rr).sqrt().max(1e-9);
        let (rl, rr) = (sum_ll.sqrt(), sum_rr.sqrt());
        let balance = (rr - rl) / (rr + rl).max(1e-9);
        let width = (sum_ss / sum_mm.max(1e-9)).sqrt();
        let tf = t as f32;
        let base_corr = [0.99f32, 0.96, 0.84, 0.66, 0.52, 0.38, 0.22, 0.02];
        let mut band_correlation = [0f32; 8];
        let mut band_balance = [0f32; 8];
        for i in 0..8 {
            let fi = i as f32;
            band_correlation[i] = (base_corr[i] + (0.04 + 0.05 * fi) * self.noise(tf * 0.8, fi * 3.1, 20))
                .max(-1.0)
                .min(1.0);
            band_balance[i] = (0.03 + 0.035 * fi) * self.noise(tf * 0.5, fi * 5.3, 21) * 2.2;
        }
        StereoReading {
            correlation: corr,
            balance,
            width,
            band_correlation,
            band_balance,
            band_active: [true; 8],
            scope_points: pts,
        }
    }
}

fn stream(name: &str, bit_depth: Option<u32>, sources: &[&str]) -> StreamInfo {
    StreamInfo {
        sample_rate: 48_000.0,
        channel_count: 2,
        device_name: name.to_string(),
        bit_depth,
        active_sources: sources.iter().map(|s| s.to_string()).collect(),
        device_is_default: None,
    }
}

// MARK: Level at the ear (made up)

/// Stand-in for the third-octave bands of the analyzer: the display spectrum
/// summed into the 31 nominal bands. Synthetic: plausible shape, no measurement.
pub fn demo_third_octave(s: &SpectrumReading) -> ThirdOctaveReading {
    let centers = ThirdOctaveReading::NOMINAL_CENTERS_HZ;
    let n = s.frequencies.len().min(s.left.len()).min(s.right.len());
    let mut left = vec![ThirdOctaveReading::FLOOR_DB; centers.len()];
    let mut right = left.clone();
    if n <= 1 {
        return ThirdOctaveReading::new(left, right);
    }
    let edge = 2f32.powf(1.0 / 6.0);
    let mut i = 0;
    for (b, fc) in centers.iter().enumerate() {
        let (lo, hi) = (fc / edge, fc * edge);
        while i < n && s.frequencies[i] < lo {
            i += 1;
        }
        let (mut sum_l, mut sum_r, mut count) = (0f64, 0f64, 0usize);
        let mut k = i;
        while k < n && s.frequencies[k] < hi {
            sum_l += 10f64.powf(s.left[k] as f64 / 10.0);
            sum_r += 10f64.powf(s.right[k] as f64 / 10.0);
            count += 1;
            k += 1;
        }
        if count == 0 {
            continue;
        }
        // Mean power of the display bins of the band, plus 5 dB for the width of a third octave, minus 3 dB (RMS of a sine).
        let level = |sum: f64| {
            ((10.0 * (sum / count as f64).max(1e-14).log10()) as f32 + 2.0).max(ThirdOctaveReading::FLOOR_DB)
        };
        left[b] = level(sum_l);
        right[b] = level(sum_r);
    }
    ThirdOctaveReading::new(left, right)
}

/// Stand-in for the SPL estimator: a made-up `SplReading` that moves with the
/// loudness numbers (momentary LUFS + 93 dB), with a calibration named "WA33 at
/// 10 o'clock" and 2 dB of uncertainty. Synthetic, deterministic; not a measurement.
pub fn demo_spl(
    l: &LoudnessReading,
    third_octave: Option<&ThirdOctaveReading>,
    dt: f64,
    state: &mut DemoSplState,
) -> SplReading {
    let floor = SplReading::FLOOR_DB;
    let silent = l.momentary_lufs <= -69.0;
    let fast = if silent {
        floor
    } else {
        (l.momentary_lufs + DEMO_SPL_OFFSET_DB).max(floor)
    };
    let power = if silent { 0.0 } else { 10f64.powf(fast as f64 / 10.0) };
    state.slow_power = if state.slow_power <= 0.0 {
        power
    } else {
        state.slow_power + (power - state.slow_power) * (1.0 - (-dt / 1.0).exp())
    };
    let slow = if state.slow_power > 1.0 {
        (10.0 * state.slow_power.log10()) as f32
    } else {
        floor
    };
    if !silent {
        state.track_power += power * dt;
        state.track_seconds += dt;
        state.session_power += power * dt;
        state.session_seconds += dt;
        state.max_fast = state.max_fast.max(fast);
        state.dose_niosh += dt / (8.0 * 3600.0 * 2f64.powf((85.0 - slow as f64) / 3.0));
        state.dose_who += dt / (40.0 * 3600.0 * 2f64.powf((80.0 - slow as f64) / 3.0));
    }
    let leq = |p: f64, t: f64| {
        if t > 0.0 && p > 0.0 {
            (10.0 * (p / t).log10()) as f32
        } else {
            floor
        }
    };
    // Band levels at the eardrum: the louder channel of each band plus one constant, like a real calibration.
    let mut bands = vec![floor; ThirdOctaveReading::NOMINAL_CENTERS_HZ.len()];
    if let (Some(t), false) = (third_octave, silent) {
        let count = t.left.len().min(t.right.len()).min(bands.len());
        for b in 0..count {
            bands[b] = (t.left[b].max(t.right[b]) + DEMO_BAND_OFFSET_DB).max(floor);
        }
    }
    let left = if slow >= 70.0 {
        (1.0 - state.dose_niosh).max(0.0) * 8.0 * 3600.0 * 2f64.powf((85.0 - slow as f64) / 3.0)
    } else {
        f64::INFINITY
    };
    SplReading {
        calibration_name: DEMO_CALIBRATION_NAME.to_string(),
        uncertainty_db: 2.0,
        level_a_fast: fast,
        level_a_slow: slow,
        level_z_eardrum: if silent { floor } else { slow + 4.5 },
        leq_a_track: leq(state.track_power, state.track_seconds),
        leq_a_session: leq(state.session_power, state.session_seconds),
        max_a_fast: state.max_fast,
        band_levels_eardrum: bands,
        dose_niosh: state.dose_niosh as f32,
        dose_who_weekly: state.dose_who as f32,
        dose_seconds: state.session_seconds,
        seconds_to_niosh_limit: left,
    }
}

/// The same frames with a made-up third-octave reading and SPL reading: for
/// frames of the real analyzers, which carry no SPL until a calibration is set.
pub fn adding_demo_spl(frames: &[AnalysisFrame]) -> Vec<AnalysisFrame> {
    let mut state = DemoSplState::default();
    let mut last: Option<f64> = None;
    frames
        .iter()
        .map(|source| {
            let mut f = source.clone();
            let dt = last.map_or(1.0 / 60.0, |l| (f.host_time - l).max(0.0).min(0.5));
            last = Some(f.host_time);
            let bands = f.third_octave.clone().unwrap_or_else(|| demo_third_octave(&f.spectrum));
            let mut l = f.loudness.clone();
            if f.is_silent {
                l.momentary_lufs = -120.0;
            }
            f.spl = Some(demo_spl(&l, Some(&bands), dt, &mut state));
            f.third_octave = Some(bands);
            f
        })
        .collect()
}

// MARK: Spectrum layers

/// Latencies of the three synthetic FFT resolutions (half their window): long, middle, short.
pub const LAYER_LATENCIES: [f32; 3] = [0.34, 0.085, 0.021];

/// Frames that carry `SpectrumReading::mid_layers`, as the analyzer provides
/// them: three resolutions with the latencies of `LAYER_LATENCIES`, crossfade
/// weights around 200 Hz and 2 kHz, a steady noise floor with three tones, and
/// one broadband click at each of `click_times` (seconds from the start; `None`:
/// every second from 1 s). A click shows in each layer at ITS latency, as a hill
/// as long as the window of that layer. `mid` is the undelayed power blend, like
/// the live spectrum. Synthetic, deterministic.
pub fn layered_clicks(
    seconds: f64,
    click_times: Option<&[f64]>,
    frames_per_second: f64,
    bin_count: usize,
) -> Vec<AnalysisFrame> {
    use crate::spectrogram::aligner::SpectrumTiming;
    use joseon_core::contracts::SpectrumLayer;
    let freqs = SpectrumReading::silent(bin_count).frequencies;
    let n = freqs.len();
    let every_second: Vec<f64> = (1..).map(f64::from).take_while(|t| *t < seconds.max(1.0)).collect();
    let clicks = click_times.unwrap_or(&every_second);
    // Steady part, as power: a pink-ish floor with a fixed fine texture, and three tones.
    let tones = [(110f32, -30f32), (440.0, -36.0), (3_100.0, -46.0)];
    let steady: Vec<f32> = freqs
        .iter()
        .map(|&f| {
            let lf = f.log2();
            let mut db = -62.0 - 3.0 * (lf - 100f32.log2()) + 0.5 * (lf * 23.0).sin() + 0.3 * (lf * 71.0 + 1.0).sin();
            if f < 40.0 {
                db -= 18.0 * (40.0 / f).log2();
            }
            let mut p = 10f32.powf(db / 10.0);
            for (hz, level) in tones {
                let d = (lf - hz.log2()) / 0.006;
                p += 10f32.powf(level / 10.0) * (-0.5 * d * d).exp();
            }
            p
        })
        .collect();
    let click_power = 10f32.powf(-34.0 / 10.0);
    let weights: Vec<Vec<f32>> = (0..3)
        .map(|k| freqs.iter().map(|&f| SpectrumTiming::weights(f)[k]).collect())
        .collect();
    let count = ((seconds * frames_per_second) as usize).max(1);
    (0..count)
        .map(|index| {
            let t = (index + 1) as f64 / frames_per_second;
            let mut layers = Vec::with_capacity(3);
            let mut blend = vec![0f32; n];
            for k in 0..3 {
                let latency = LAYER_LATENCIES[k] as f64;
                // The window of this layer passes over the click: Hann window value at the click, squared (power).
                let mut hill = 0f32;
                for c in clicks {
                    let x = (t - c - latency) / latency;
                    if x.abs() < 1.0 {
                        let w = (0.5 * (1.0 + (std::f64::consts::PI * x).cos())) as f32;
                        hill += w * w;
                    }
                }
                let mut levels = vec![0f32; n];
                for i in 0..n {
                    let p = steady[i] + click_power * hill;
                    levels[i] = (10.0 * p.log10()).max(SpectrumReading::FLOOR_DB);
                    blend[i] += weights[k][i] * p;
                }
                layers.push(SpectrumLayer {
                    levels_db: levels,
                    weights: weights[k].clone(),
                    latency_seconds: LAYER_LATENCIES[k],
                });
            }
            let mid: Vec<f32> = blend
                .iter()
                .map(|p| (10.0 * p.max(1e-12).log10()).max(SpectrumReading::FLOOR_DB))
                .collect();
            let spectrum = SpectrumReading {
                frequencies: freqs.clone(),
                left: mid.clone(),
                right: mid.clone(),
                side: vec![SpectrumReading::FLOOR_DB; n],
                peak_hold: mid.clone(),
                average: steady.iter().map(|p| 10.0 * p.log10()).collect(),
                mid,
                mid_layers: layers,
            };
            let mut frame = AnalysisFrame::new(spectrum);
            frame.host_time = 1000.0 + t;
            frame.stream = Some(stream("Synthetic layers", Some(24), &["Joseon demo"]));
            frame.is_silent = false;
            frame.peak = PeakReading {
                frequency_hz: 110.0,
                level_db: -30.0,
                note_name: "A2".to_string(),
                cents: 0.0,
            };
            frame
        })
        .collect()
}

// MARK: Stand-ins for analyzer extras

/// Made-up stress flags with a frequency span and a plot label. The numbers are
/// read from the demo curves, so a label never contradicts the curve under it.
pub fn demo_stress_flags() -> Vec<StressFlag> {
    let error = |hz: f32| demo_response(hz.log2()) - demo_target(hz.log2());
    let mut flags = Vec::new();
    // Sub-bass: mean error from 20 to 45 Hz, in steps of 2.5 Hz.
    let steps: Vec<f32> = (0..=10).map(|i| 20.0 + i as f32 * 2.5).collect();
    let sub = steps.iter().fold(0f32, |sum, &hz| sum + error(hz)) / steps.len() as f32;
    if sub < -3.0 {
        flags.push(StressFlag {
            id: "demo-sub".to_string(),
            severity: StressSeverity::Watch,
            title: "Sub-bass under-delivered".to_string(),
            detail: "Synthetic demo flag".to_string(),
            frequency_range_hz: Some(20.0..=45.0),
            plot_label: format!("{} dB vs target", fmt::number(sub, 0, false)),
        });
    }
    // Treble: the largest excess over the target between 4 and 14 kHz.
    let (mut best_hz, mut best) = (0f32, 0f32);
    let mut hz = 4_000f32;
    while hz <= 14_000.0 {
        let e = error(hz);
        if e > best {
            best = e;
            best_hz = hz;
        }
        hz *= 2f32.powf(1.0 / 24.0);
    }
    if best > 2.0 {
        flags.push(StressFlag {
            id: "demo-treble".to_string(),
            severity: StressSeverity::High,
            title: "Treble peak".to_string(),
            detail: "Synthetic demo flag".to_string(),
            frequency_range_hz: Some((best_hz / 2f32.powf(0.2))..=(best_hz * 2f32.powf(0.2))),
            plot_label: format!(
                "+{} dB at {:.1} kHz",
                fmt::number(best, 0, false),
                (best_hz / 1000.0) as f64
            ),
        });
    }
    flags
}

/// Stand-in for the top peaks of the analyzer: the strongest local maxima of
/// mid that stand 10 dB over the level an octave around them, strongest first,
/// at most 5, at least a third of an octave apart. `first` leads the list.
pub fn demo_top_peaks(s: &SpectrumReading, first: Option<&PeakReading>) -> Vec<PeakReading> {
    let n = s.mid.len().min(s.frequencies.len());
    if n <= 64 {
        return Vec::new();
    }
    let per_octave = (n - 1) as f32 / (s.frequencies[n - 1] / s.frequencies[0]).log2();
    let reach = ((per_octave / 2.0) as usize).max(4);
    let mut found: Vec<(usize, f32)> = Vec::new();
    for i in 2..n - 2 {
        if !(s.frequencies[i] > 25.0 && s.mid[i] > -80.0 && s.mid[i] >= s.mid[i - 1] && s.mid[i] > s.mid[i + 1]) {
            continue;
        }
        let (mut sum, mut c) = (0f32, 0f32);
        let mut k = i.saturating_sub(reach);
        while k <= (i + reach).min(n - 1) {
            sum += s.mid[k];
            c += 1.0;
            k += 2;
        }
        if s.mid[i] - sum / c >= 10.0 {
            found.push((i, s.mid[i]));
        }
    }
    // Strongest first. Equal levels keep their order, as in the macOS app on this data.
    found.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut out: Vec<PeakReading> = Vec::new();
    if let Some(first) = first {
        if first.frequency_hz > 0.0 && first.level_db > -119.0 {
            out.push(first.clone());
        }
    }
    for (i, db) in found {
        if out.len() >= 5 {
            break;
        }
        let hz = s.frequencies[i];
        if out.iter().any(|p| (p.frequency_hz / hz).log2().abs() < 1.0 / 3.0) {
            continue;
        }
        let midi = 69.0 + 12.0 * (hz / 440.0).log2();
        let nearest = midi.round();
        out.push(PeakReading {
            frequency_hz: hz,
            level_db: db,
            note_name: note_name(nearest),
            cents: (midi - nearest) * 100.0,
        });
    }
    out
}

/// Stand-in for the lowest strong content of the analyzer: the lowest bin of
/// the long-term average within 30 dB of its maximum.
pub fn demo_lowest_strong(s: &SpectrumReading) -> f32 {
    let n = s.average.len().min(s.frequencies.len());
    let top = s.average[..n].iter().copied().fold(f32::NEG_INFINITY, f32::max);
    if !(n > 0 && top > -100.0) {
        return 0.0;
    }
    (0..n)
        .find(|&i| s.frequencies[i] >= 20.0 && s.average[i] >= top - 30.0)
        .map_or(0.0, |i| s.frequencies[i])
}

// MARK: Demo headphone curves (made up, smooth, 0 dB at 1 kHz)

fn bump(lf: f32, hz: f32, gain: f32, width_oct: f32) -> f32 {
    let d = (lf - hz.log2()) / width_oct;
    gain * (-0.5 * d * d).exp()
}

pub fn demo_response(lf: f32) -> f32 {
    let mut v = 0f32;
    v += bump(lf, 20.0, -7.0, 1.0) + bump(lf, 110.0, 1.8, 0.9);
    v += bump(lf, 3100.0, 8.5, 0.62) + bump(lf, 5800.0, -3.5, 0.22) + bump(lf, 8600.0, 4.5, 0.2);
    v += bump(lf, 17_000.0, -9.0, 0.5);
    let k = 1000f32.log2();
    v - (bump(k, 3100.0, 8.5, 0.62) + bump(k, 110.0, 1.8, 0.9))
}

pub fn demo_target(lf: f32) -> f32 {
    let shelf = |lf: f32| 5.5 / (1.0 + 2f32.powf((lf - 105f32.log2()) * 2.2));
    let mut v = 0f32;
    v += shelf(lf);
    v += bump(lf, 3000.0, 11.0, 0.75) + bump(lf, 9000.0, 2.0, 0.5) + bump(lf, 19_000.0, -8.0, 0.6);
    let k = 1000f32.log2();
    v - (shelf(k) + bump(k, 3000.0, 11.0, 0.75))
}
