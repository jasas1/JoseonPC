//! The session timeline, CPU side: what of a `SessionSnapshot` a window of N
//! seconds shows. No drawing here, so the numbers (runs, spans, gaps,
//! statistics, the sample under a time) are tested with no GPU.
//!
//! Two clocks. `SessionSample::time` is AUDIO time: it stands still while no
//! audio arrives. `SessionSample::date` is the wall clock. The x axis is audio
//! time (one second of music = the same width everywhere); the labels are
//! wall-clock times looked up from the samples. Where the wall clock jumps
//! between two neighbor samples the axis carries a gap mark.
//!
//! A sample stands for the second of audio that ENDS at its `time` (the
//! recorder writes it when the second is complete), so an event belongs to the
//! first sample whose time is not before the event's time.
//!
//! Port source: `Sources/JoseonRender/TimelineModel.swift`.

use crate::fmt;
use joseon_core::contracts::{SessionEvent, SessionEventKind, SessionSample, SessionSnapshot, SplReading};
use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

/// Seconds since 1970 of a wall-clock time.
pub fn unix_seconds(date: SystemTime) -> f64 {
    match date.duration_since(UNIX_EPOCH) {
        Ok(after) => after.as_secs_f64(),
        Err(before) => -before.duration().as_secs_f64(),
    }
}

/// A stress flag from raise to clear.
#[derive(Debug, Clone, PartialEq)]
pub struct Span {
    pub start: f64,
    pub end: f64,
    pub title: String,
    pub flag_id: String,
    /// Row of the span bar, 0 = top. Overlapping spans stack. Rows from `MAX_SPAN_ROWS` on are not drawn, only counted.
    pub row: usize,
    /// Still raised at the newest sample.
    pub is_open: bool,
}

/// The wall clock jumped between two neighbor samples: no audio arrived for a while.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Gap {
    /// Audio time of the gap (between the two samples).
    pub time: f64,
    /// Wall seconds that the audio clock did not count.
    pub skipped_seconds: f64,
    /// Index (into `samples`) of the first sample after the gap.
    pub sample_after: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Statistics {
    /// 10th and 95th percentile of the short-term loudness of the seconds with signal: "mostly between".
    pub short_term_low: Option<f32>,
    pub short_term_high: Option<f32>,
    pub momentary_max: Option<f32>,
    pub true_peak_max: Option<f32>,
    pub level_a_max: Option<f32>,
    /// Seconds that carry a clip event, and the sum of their counts.
    pub clip_events: usize,
    pub clipped_runs: usize,
    pub overs: usize,
    pub flags: usize,
    pub tracks: usize,
}

/// A round minute on recorded audio.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MinuteMark {
    /// Audio time, for x.
    pub time: f64,
    /// The round minute, seconds since 1970.
    pub unix: f64,
}

pub const MAX_SPAN_ROWS: usize = 3;
/// A wall-clock step between neighbor samples that is this much longer than the audio step is a gap.
pub const GAP_THRESHOLD: f64 = 2.0;

pub struct TimelineModel {
    pub window_seconds: f64,
    /// The samples inside the window, oldest first.
    pub samples: Vec<SessionSample>,
    /// The events inside the window (time > the left edge), oldest first.
    pub events: Vec<SessionEvent>,
    pub newest_time: f64,
    pub spans: Vec<Span>,
    /// Spans that found no row (more than `MAX_SPAN_ROWS` at once).
    pub hidden_spans: usize,
    /// Rows in use, 0...MAX_SPAN_ROWS.
    pub span_rows: usize,
    /// From, to (audio time).
    pub silences: Vec<(f64, f64)>,
    pub gaps: Vec<Gap>,
    pub stats: Statistics,
    pub has_level_a: bool,
    /// Event indices (into `events`) per sample index.
    events_of_sample: HashMap<usize, Vec<usize>>,
}

/// The key of a flag: its id, or its title when it has no id.
pub fn flag_key(e: &SessionEvent) -> &str {
    if e.detail.is_empty() {
        &e.label
    } else {
        &e.detail
    }
}

/// A loudness or level number that is a measurement (not the floor, not NaN).
pub fn is_measured(v: f32) -> bool {
    v.is_finite() && v > -119.0
}

impl TimelineModel {
    pub fn new(snapshot: &SessionSnapshot, window_seconds: i64) -> Self {
        let window = window_seconds.max(10) as f64;
        let all = &snapshot.samples;
        let newest = all.last().map_or(0.0, |s| s.time);
        let left = newest - window;
        // First sample inside the window.
        let lo = all.partition_point(|s| s.time <= left);
        let visible: Vec<SessionSample> = all[lo..].to_vec();
        let has_level_a = visible.iter().any(|s| s.level_a.is_some());

        // ---- Stress spans: over the WHOLE record, so a flag raised before the window still shows, then cut to the
        // window.
        let mut open: HashMap<String, (f64, String)> = HashMap::new();
        // start, end, title, id, open
        let mut raw: Vec<(f64, f64, String, String, bool)> = Vec::new();
        let record_start = all.first().map_or(0.0, |s| s.time);
        for e in &snapshot.events {
            let key = flag_key(e).to_string();
            match e.kind {
                SessionEventKind::StressFlagRaised => {
                    open.entry(key).or_insert((e.time, e.label.to_string()));
                }
                SessionEventKind::StressFlagCleared => {
                    // A clear without its raise: the raise is older than the record.
                    let (start, title) = open.remove(&key).unwrap_or((record_start, e.label.to_string()));
                    let title = if title.is_empty() { e.label.to_string() } else { title };
                    raw.push((start, e.time, title, key, false));
                }
                _ => {}
            }
        }
        for (key, (start, title)) in open {
            raw.push((start, newest, title, key, true));
        }
        raw.retain(|s| s.1 > left && s.1 >= s.0);
        raw.sort_by(|a, b| a.0.total_cmp(&b.0).then_with(|| a.3.cmp(&b.3)));
        let mut row_ends: Vec<f64> = Vec::new();
        let mut spans = Vec::new();
        let mut hidden_spans = 0;
        for (start, end, title, id, is_open) in raw {
            let mut row = row_ends.iter().position(|e| *e <= start);
            if row.is_none() && row_ends.len() < MAX_SPAN_ROWS {
                row_ends.push(0.0);
                row = Some(row_ends.len() - 1);
            }
            let Some(r) = row else {
                hidden_spans += 1;
                continue;
            };
            row_ends[r] = end;
            spans.push(Span {
                start: start.max(left),
                end,
                title,
                flag_id: id,
                row: r,
                is_open,
            });
        }

        // ---- Events of the window, and which sample each belongs to.
        let events: Vec<SessionEvent> = snapshot.events.iter().filter(|e| e.time > left).cloned().collect();
        let mut events_of_sample: HashMap<usize, Vec<usize>> = HashMap::new();
        if !visible.is_empty() {
            let mut si = 0;
            for (ei, e) in events.iter().enumerate() {
                while si < visible.len() - 1 && visible[si].time < e.time - 1e-6 {
                    si += 1;
                }
                events_of_sample.entry(si).or_default().push(ei);
            }
        }

        // ---- Silence (the clock ran, the signal was digital silence) and gaps (the clock stood still).
        let mut silences = Vec::new();
        let mut gaps = Vec::new();
        let mut run_start: Option<f64> = None;
        for (i, s) in visible.iter().enumerate() {
            if s.is_silent {
                if run_start.is_none() {
                    run_start = Some(s.time - 1.0);
                }
            } else if let Some(r) = run_start.take() {
                silences.push((r.max(left), visible[i - 1].time));
            }
            if i > 0 {
                let p = &visible[i - 1];
                let skipped = (unix_seconds(s.date) - unix_seconds(p.date)) - (s.time - p.time);
                if skipped > GAP_THRESHOLD {
                    gaps.push(Gap {
                        time: (p.time + s.time) / 2.0,
                        skipped_seconds: skipped,
                        sample_after: i,
                    });
                }
            }
        }
        if let (Some(r), Some(last)) = (run_start, visible.last()) {
            silences.push((r.max(left), last.time));
        }

        // ---- Statistics of the window.
        let mut stats = Statistics::default();
        let mut short_term: Vec<f32> = Vec::with_capacity(visible.len());
        let raise = |slot: &mut Option<f32>, v: f32| *slot = Some(slot.unwrap_or(f32::NEG_INFINITY).max(v));
        for s in visible.iter().filter(|s| !s.is_silent) {
            if is_measured(s.short_term_lufs) {
                short_term.push(s.short_term_lufs);
            }
            if is_measured(s.momentary_max_lufs) {
                raise(&mut stats.momentary_max, s.momentary_max_lufs);
            }
            if is_measured(s.true_peak_dbtp) {
                raise(&mut stats.true_peak_max, s.true_peak_dbtp);
            }
            if let Some(a) = s.level_a.filter(|a| *a > SplReading::FLOOR_DB) {
                raise(&mut stats.level_a_max, a);
            }
        }
        if !short_term.is_empty() {
            short_term.sort_by(f32::total_cmp);
            let at = |share: f32| short_term[((short_term.len() - 1) as f32 * share).round() as usize];
            stats.short_term_low = Some(at(0.10));
            stats.short_term_high = Some(at(0.95));
        }
        for e in &events {
            match e.kind {
                SessionEventKind::Clip => {
                    stats.clip_events += 1;
                    stats.clipped_runs += (e.value.round() as i64).max(1) as usize;
                }
                SessionEventKind::InterSampleOver => stats.overs += 1,
                SessionEventKind::TrackStart => stats.tracks += 1,
                _ => {}
            }
        }
        stats.flags = spans.len() + hidden_spans;

        Self {
            window_seconds: window,
            samples: visible,
            events,
            newest_time: newest,
            spans,
            hidden_spans,
            span_rows: row_ends.len(),
            silences,
            gaps,
            stats,
            has_level_a,
            events_of_sample,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.samples.is_empty()
    }

    pub fn oldest_time(&self) -> f64 {
        self.samples.first().map_or(self.newest_time, |s| s.time)
    }

    pub fn left_edge_time(&self) -> f64 {
        self.newest_time - self.window_seconds
    }

    // MARK: Lookup

    /// Index of the sample nearest to an audio time. `None` with no samples, or when the time is more than
    /// `tolerance` seconds outside the recorded samples.
    pub fn sample_index(&self, t: f64, tolerance: f64) -> Option<usize> {
        let (first, last) = (self.samples.first()?, self.samples.last()?);
        if !(t >= first.time - 1.0 - tolerance && t <= last.time + tolerance) {
            return None;
        }
        // The first sample that ends at or after `t`: the second that contains `t`.
        Some(self.samples.partition_point(|s| s.time < t).min(self.samples.len() - 1))
    }

    pub fn events_of_sample(&self, i: usize) -> Vec<&SessionEvent> {
        self.events_of_sample.get(&i).map_or(Vec::new(), |indices| {
            indices.iter().map(|ei| &self.events[*ei]).collect()
        })
    }

    /// The flags that were up during the second of sample `i`.
    pub fn spans_at_sample(&self, i: usize) -> Vec<&Span> {
        let Some(s) = self.samples.get(i) else {
            return Vec::new();
        };
        let t = s.time;
        self.spans
            .iter()
            .filter(|span| span.start <= t && span.end > t - 1.0)
            .collect()
    }

    pub fn is_in_silence(&self, i: usize) -> bool {
        self.samples.get(i).is_some_and(|s| s.is_silent)
    }

    /// The gap that ends at sample `i` (the first sample after a pause).
    pub fn gap_before(&self, i: usize) -> Option<&Gap> {
        self.gaps.iter().find(|g| g.sample_after == i)
    }

    // MARK: Wall-clock marks

    /// The round minutes (multiples of `step_minutes` on the local clock) that fall on recorded audio, with their
    /// audio time. A round minute inside a gap has no place on the axis and is left out.
    pub fn minute_marks(&self, step_minutes: i64, utc_offset_seconds: i32) -> Vec<MinuteMark> {
        let (Some(first), Some(last)) = (self.samples.first(), self.samples.last()) else {
            return Vec::new();
        };
        if step_minutes <= 0 {
            return Vec::new();
        }
        let step = (step_minutes * 60) as f64;
        let offset = utc_offset_seconds as f64;
        let mut local = ((unix_seconds(first.date) - 1.0 + offset) / step).ceil() * step;
        let end = unix_seconds(last.date) + offset;
        let mut out = Vec::new();
        let mut i = 0;
        while local <= end + 0.001 && out.len() < 64 {
            let target = local - offset;
            while i < self.samples.len() - 1 && unix_seconds(self.samples[i].date) < target {
                i += 1;
            }
            // samples[i] is the first sample that ends at or after the minute. The minute has a place on the axis
            // when it lies inside that sample's own second; a minute inside a pause lies before the second of the
            // sample after the pause.
            let d = unix_seconds(self.samples[i].date) - target;
            if d >= -0.001 && d <= 1.001 {
                out.push(MinuteMark {
                    time: self.samples[i].time - d,
                    unix: target,
                });
            }
            local += step;
        }
        out
    }

    // MARK: Text

    /// "last 15 minutes: short-term mostly −18 to −9 LUFS, 3 clip events, 2 stress flags".
    pub fn accessibility_value(&self) -> String {
        let minutes = (self.window_seconds / 60.0).round() as i64;
        let head = format!("last {minutes} minute{}", if minutes == 1 { "" } else { "s" });
        if self.is_empty() {
            return head + ": nothing recorded yet";
        }
        let st = &self.stats;
        let mut parts = Vec::new();
        match (st.short_term_low, st.short_term_high) {
            (Some(lo), Some(hi)) => parts.push(format!(
                "short-term mostly {} to {} LUFS",
                fmt::number(lo, 0, false),
                fmt::number(hi, 0, false)
            )),
            _ => parts.push("no loudness measured".to_string()),
        }
        if let Some(tp) = st.true_peak_max {
            parts.push(format!("true peak max {} dBTP", fmt::number(tp, 1, true)));
        }
        if let Some(a) = st.level_a_max {
            parts.push(format!("at the ear up to about {} dB(A)", fmt::number(a, 0, false)));
        }
        let count = |n: usize, one: &str, many: &str| format!("{n} {}", if n == 1 { one } else { many });
        parts.push(count(
            st.clip_events,
            "second with clipped samples",
            "seconds with clipped samples",
        ));
        if st.overs > 0 {
            parts.push(count(st.overs, "true-peak over", "true-peak overs"));
        }
        parts.push(count(st.flags, "stress flag", "stress flags"));
        if st.tracks > 0 {
            parts.push(count(st.tracks, "track start", "track starts"));
        }
        if !self.gaps.is_empty() {
            parts.push(count(self.gaps.len(), "pause", "pauses"));
        }
        head + ": " + &parts.join(", ")
    }
}

/// The time texts of the timeline.
pub mod format {
    use crate::fmt;

    /// "21:42" or "21:42:07" on the local 24 h clock.
    pub fn clock(unix: f64, seconds: bool, utc_offset_seconds: i32) -> String {
        let local = (unix + utc_offset_seconds as f64).floor() as i64;
        let day = local.rem_euclid(86_400);
        let (h, m, s) = (day / 3600, day % 3600 / 60, day % 60);
        if seconds {
            format!("{h:02}:{m:02}:{s:02}")
        } else {
            format!("{h:02}:{m:02}")
        }
    }

    /// "now", "−42 s", "−3:12".
    pub fn ago(seconds: f64) -> String {
        let s = seconds.round() as i64;
        if s <= 0 {
            return "now".to_string();
        }
        if s < 60 {
            return format!("{}{s} s", fmt::MINUS);
        }
        format!("{}{}:{:02}", fmt::MINUS, s / 60, s % 60)
    }

    /// "20 s", "3:08 min".
    pub fn duration(seconds: f64) -> String {
        let s = seconds.round() as i64;
        if s < 60 {
            format!("{s} s")
        } else {
            format!("{}:{:02} min", s / 60, s % 60)
        }
    }
}
