//! A believable session record for the demo mode, the tests and the design
//! review. Made-up numbers, never shown as a measurement of real audio: the app
//! uses it only in its demo mode.
//!
//! Port source: `Sources/JoseonRender/SyntheticFrames+Session.swift`.

use joseon_core::contracts::{
    LoudnessReading, SessionEvent, SessionEventKind, SessionSample, SessionSnapshot, SplReading,
};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// End of the demo session when the caller gives none: a fixed moment, so pictures and tests repeat exactly.
/// 45 minutes past a round hour (UTC), in seconds since 1970.
pub const DEMO_SESSION_END_UNIX: f64 = 1_790_019_900.0;
/// The level at the ear of the demo: this many dB over the loudness in LUFS.
const DEMO_SPL_OFFSET_DB: f32 = 93.0;

struct Track {
    start: i64,
    level: f32,
    dynamics: f32,
    crest: f32,
    ceiling: f32,
    correlation: f32,
    /// Band tilt, dB relative to the short-term loudness: sub, bass, low mid, mid, upper mid, presence, brilliance, air.
    tilt: [f32; 8],
}

fn hash(i: i64, channel: i64) -> f32 {
    let mixed = i.wrapping_mul(73_856_093) ^ channel.wrapping_mul(19_349_663);
    let mut x = (mixed as u64).wrapping_add(0x9E37_79B9_7F4A_7C15);
    x = (x ^ (x >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    x = (x ^ (x >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    x ^= x >> 31;
    (x & 0xFFFF) as f32 / 32_767.5 - 1.0
}

/// Smooth value noise, -1...1.
fn noise(t: f64, period: f64, channel: i64) -> f32 {
    let u = t / period;
    let i = u.floor() as i64;
    let f = (u - i as f64) as f32;
    let s = f * f * (3.0 - 2.0 * f);
    hash(i, channel) * (1.0 - s) + hash(i + 1, channel) * s
}

fn date(unix: f64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs_f64(unix.max(0.0))
}

/// A record of `minutes` of listening: four tracks with different loudness and dynamics (a loud pop master, a quiet
/// acoustic piece, a bass-heavy electronic track, a rock track), two clip bursts, one inter-sample over, a sub-bass
/// stress flag (with a treble flag over part of it, and a short return later), one 20 s digital silence between
/// tracks two and three, and one pause of about three minutes before track four (the audio clock stands still, the
/// wall clock jumps). Deterministic. `include_level_a` adds the level at the ear (a calibrated headphone). The
/// newest sample is at `ending_at_unix` (`None` = `DEMO_SESSION_END_UNIX`).
pub fn demo_session(minutes: i64, include_level_a: bool, ending_at_unix: Option<f64>) -> SessionSnapshot {
    let total = minutes.max(1) * 60;
    let end = ending_at_unix.unwrap_or(DEMO_SESSION_END_UNIX);
    let n = total as f64;
    let at = |share: f64| (n * share) as i64;

    let silence_start = at(0.50);
    let silence_length = 20.min(total / 8);
    let tracks = [
        Track {
            start: 0,
            level: -9.5,
            dynamics: 1.2,
            crest: 9.6,
            ceiling: -0.1,
            correlation: 0.55,
            tilt: [-10.0, -4.0, -9.0, -11.0, -15.0, -19.0, -22.0, -30.0],
        },
        Track {
            start: at(0.247),
            level: -20.0,
            dynamics: 5.0,
            crest: 15.0,
            ceiling: -3.0,
            correlation: 0.82,
            tilt: [-30.0, -10.0, -6.0, -5.0, -12.0, -18.0, -24.0, -34.0],
        },
        Track {
            start: silence_start + silence_length,
            level: -12.5,
            dynamics: 2.4,
            crest: 10.5,
            ceiling: -0.3,
            correlation: 0.35,
            tilt: [-2.0, -3.0, -13.0, -14.0, -18.0, -21.0, -22.0, -27.0],
        },
        Track {
            start: at(0.76),
            level: -11.0,
            dynamics: 3.2,
            crest: 10.0,
            ceiling: -0.2,
            correlation: 0.62,
            tilt: [-16.0, -6.0, -7.0, -6.0, -9.0, -13.0, -19.0, -29.0],
        },
    ];
    let pause_at = tracks[3].start; // the pause stands before track four
    let pause_seconds = 190.0;

    // Events by the second they belong to.
    let clip_bursts: [(i64, &[f32]); 2] = [(at(0.147), &[3.0, 11.0, 19.0, 7.0, 2.0]), (at(0.89), &[5.0, 14.0, 4.0])];
    let over_at = at(0.201);
    let sub_bass: Arc<str> = Arc::from("Strong sub-bass in the signal");
    let treble: Arc<str> = Arc::from("Treble peak over the target");
    let flags: [(&str, &Arc<str>, i64, i64); 3] = [
        ("sub-bass", &sub_bass, at(0.573), at(0.71)),
        ("treble-peak", &treble, at(0.668), at(0.735)),
        ("sub-bass", &sub_bass, at(0.843), at(0.873)),
    ];
    let empty: Arc<str> = Arc::from("");

    let mut samples = Vec::with_capacity(total as usize);
    let mut events: Vec<SessionEvent> = Vec::new();
    let wall_length = n + if pause_at > 0 && pause_at < total {
        pause_seconds
    } else {
        0.0
    };
    let start_unix = end - wall_length;
    let mut add = |kind: SessionEventKind, time: f64, unix: f64, label: &Arc<str>, detail: &str, value: f32| {
        events.push(SessionEvent {
            id: events.len() as u64 + 1,
            kind,
            time,
            date: date(unix),
            label: label.clone(),
            detail: Arc::from(detail),
            value,
        });
    };

    for k in 0..total {
        let time = (k + 1) as f64; // the second that ends here
        let wall = time
            + if k >= pause_at && pause_at > 0 {
                pause_seconds
            } else {
                0.0
            };
        let unix = start_unix + wall;
        let (event_time, event_unix) = (time - 0.5, unix - 0.5);
        let silent = k >= silence_start && k < silence_start + silence_length;
        // Starts happen just inside the second they open.
        let (start_time, start_at) = (time - 0.98, unix - 0.98);
        if k == silence_start {
            add(SessionEventKind::SilenceStart, start_time, start_at, &empty, "", 0.0);
        }
        if k == silence_start + silence_length {
            add(SessionEventKind::SilenceEnd, start_time, start_at, &empty, "", 0.0);
        }

        let ti = tracks.iter().rposition(|t| t.start <= k).unwrap_or(0);
        let tr = &tracks[ti];
        if k == tr.start {
            add(SessionEventKind::TrackStart, start_time, start_at, &empty, "", 0.0);
        }
        if silent {
            samples.push(SessionSample {
                time,
                date: date(unix),
                momentary_max_lufs: LoudnessReading::SILENCE_LUFS,
                short_term_lufs: LoudnessReading::SILENCE_LUFS,
                true_peak_dbtp: -120.0,
                correlation: 0.0,
                bands: [-120.0; 8],
                level_a: include_level_a.then_some(SplReading::FLOOR_DB),
                is_silent: true,
            });
            continue;
        }
        let into = (k - tr.start) as f64;
        let track_end = if ti + 1 < tracks.len() {
            if ti == 1 {
                silence_start
            } else {
                tracks[ti + 1].start
            }
        } else {
            total
        };
        let left = (track_end - k) as f64;
        // Song form: a quieter intro and a bridge at two thirds, a short fade at the end.
        let length = (track_end - tr.start) as f64;
        let mut form = 0.0f32;
        if into < 18.0 {
            form -= (18.0 - into) as f32 / 18.0 * (2.0 + tr.dynamics);
        }
        let bridge = (into - length * 0.64).abs() / (length * 0.06).max(1.0);
        if bridge < 1.0 {
            form -= (1.0 - bridge) as f32 * tr.dynamics * 1.6;
        }
        if left < 7.0 {
            form -= (7.0 - left) as f32 * 2.2;
        }
        let track = ti as i64;
        let slow = noise(time, 37.0, track) * tr.dynamics * 0.55 + noise(time, 9.0, track + 10) * tr.dynamics * 0.3;
        let short_term = tr.level + form + slow;
        let momentary = short_term + 1.4 + noise(time, 2.3, track + 20).abs() * (1.2 + tr.dynamics * 0.5);
        let mut peak = (momentary + tr.crest + noise(time, 1.7, track + 30) * 1.3)
            .min(tr.ceiling - noise(time, 1.1, 40).abs() * 0.25);

        for (start, counts) in clip_bursts {
            if k >= start && k < start + counts.len() as i64 {
                peak = 0.0;
                add(
                    SessionEventKind::Clip,
                    event_time,
                    event_unix,
                    &empty,
                    "",
                    counts[(k - start) as usize],
                );
            }
        }
        if k == over_at {
            peak = 0.4;
            add(
                SessionEventKind::InterSampleOver,
                event_time,
                event_unix,
                &empty,
                "",
                peak,
            );
        }
        for (id, title, from, to) in flags {
            if k == from {
                add(
                    SessionEventKind::StressFlagRaised,
                    event_time,
                    event_unix,
                    title,
                    id,
                    0.0,
                );
            }
            if k == to {
                add(
                    SessionEventKind::StressFlagCleared,
                    event_time,
                    event_unix,
                    title,
                    id,
                    0.0,
                );
            }
        }
        let mut bands = [0.0f32; 8];
        for (i, band) in bands.iter_mut().enumerate() {
            *band = short_term + tr.tilt[i] + noise(time, 5.0 + i as f64 * 1.7, 50 + i as i64) * 2.5;
        }
        // The sub-bass swells where its flag is up.
        if flags
            .iter()
            .any(|(id, _, from, to)| *id == "sub-bass" && k >= *from && k < *to)
        {
            bands[0] += 5.0;
        }
        let correlation = (tr.correlation + noise(time, 13.0, 60 + track) * 0.22)
            .max(-1.0)
            .min(1.0);
        let level_a = include_level_a.then(|| short_term + DEMO_SPL_OFFSET_DB + noise(time, 6.0, 70) * 0.8);
        samples.push(SessionSample {
            time,
            date: date(unix),
            momentary_max_lufs: momentary,
            short_term_lufs: short_term,
            true_peak_dbtp: peak,
            correlation,
            bands,
            level_a,
            is_silent: false,
        });
    }
    events.sort_by(|a, b| a.time.total_cmp(&b.time).then(a.id.cmp(&b.id)));
    let revision = (samples.len() + events.len()) as u64;
    SessionSnapshot {
        samples,
        events,
        revision,
    }
}
