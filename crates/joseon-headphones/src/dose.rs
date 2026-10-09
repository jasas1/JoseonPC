//! Noise dose: the options, the counters the app can save, and the day and week stamps.
//!
//! Port source: the first part of `Sources/JoseonHeadphones/SPLEstimator.swift`.
//!
//! The macOS app reads the day and the week from `Calendar`. This port has no
//! calendar library: `LocalTime` carries the UTC offset, and the date math is
//! done here. The app supplies the local offset of the PC.

use serde::{Deserialize, Serialize};
use std::time::{SystemTime, UNIX_EPOCH};

/// Switches that change how the dose is counted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplDoseOptions {
    /// NIOSH integrates only the time at or above a threshold level (80 dBA,
    /// DHHS 98-126). Default **on**. Turn it off to integrate every level.
    #[serde(rename = "applyNIOSHThreshold")]
    pub apply_niosh_threshold: bool,
}

impl Default for SplDoseOptions {
    fn default() -> Self {
        Self {
            apply_niosh_threshold: true,
        }
    }
}

impl SplDoseOptions {
    /// NIOSH REL: 85 dBA for 8 hours is 100 % of the daily dose.
    pub const NIOSH_CRITERION_DBA: f64 = 85.0;
    pub const NIOSH_CRITERION_SECONDS: f64 = 8.0 * 3_600.0;
    /// NIOSH exchange rate: 3 dB. The allowed time halves per 3 dB.
    pub const NIOSH_EXCHANGE_RATE_DB: f64 = 3.0;
    /// NIOSH threshold level. Levels under this add no dose when the option is on.
    pub const NIOSH_THRESHOLD_DBA: f64 = 80.0;
    /// WHO / ITU-T H.870 adult allowance: 80 dBA for 40 hours is one week of sound.
    pub const WHO_CRITERION_DBA: f64 = 80.0;
    pub const WHO_WEEKLY_SECONDS: f64 = 40.0 * 3_600.0;

    /// Allowed time in seconds at a steady A-weighted level, under the NIOSH criterion.
    pub fn niosh_allowed_seconds(level_dba: f64) -> f64 {
        if !level_dba.is_finite() {
            return if level_dba < 0.0 { f64::INFINITY } else { 0.0 };
        }
        Self::NIOSH_CRITERION_SECONDS
            * 2.0f64.powf((Self::NIOSH_CRITERION_DBA - level_dba) / Self::NIOSH_EXCHANGE_RATE_DB)
    }
}

/// A moment in time with the UTC offset of the place where the listener is.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LocalTime {
    /// Seconds since 1970-01-01 00:00 UTC.
    pub unix_seconds: f64,
    /// Local time minus UTC, in seconds. Tokyo is `9 * 3600`.
    pub utc_offset_seconds: i32,
}

impl LocalTime {
    pub fn new(unix_seconds: f64, utc_offset_seconds: i32) -> Self {
        Self {
            unix_seconds,
            utc_offset_seconds,
        }
    }

    /// A moment in UTC.
    pub fn utc(unix_seconds: f64) -> Self {
        Self::new(unix_seconds, 0)
    }

    /// The system clock, in UTC. The app replaces this with a clock that knows the local offset.
    pub fn now_utc() -> Self {
        let seconds = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0.0, |d| d.as_secs_f64());
        Self::utc(seconds)
    }

    /// The same place, `seconds` later.
    pub fn adding(self, seconds: f64) -> Self {
        Self::new(self.unix_seconds + seconds, self.utc_offset_seconds)
    }

    /// Whole local days since 1970-01-01.
    fn local_days(self) -> i64 {
        ((self.unix_seconds + self.utc_offset_seconds as f64) / 86_400.0).floor() as i64
    }
}

/// Days since 1970-01-01 -> (year, month, day) in the Gregorian calendar.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let day_of_era = z.rem_euclid(146_097);
    let year_of_era = (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let mp = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * mp + 2) / 5 + 1) as u32;
    let month = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let year = year_of_era + era * 400 + i64::from(month <= 2);
    (year, month, day)
}

/// (year, month, day) -> days since 1970-01-01.
fn days_from_civil(year: i64, month: u32, day: u32) -> i64 {
    let y = year - i64::from(month <= 2);
    let era = y.div_euclid(400);
    let year_of_era = y.rem_euclid(400);
    let m = i64::from(month);
    let day_of_year = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(day) - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

/// Everything the dose counters hold, in a form the app can write to disk and give back.
///
/// The estimator never touches the file system. The app decides where this
/// lives and when a new day or week starts. `rolled` says what a rollover means.
/// The JSON field names are the same as in the macOS app.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SplDoseState {
    /// NIOSH daily dose so far, 1.0 = 100 %.
    pub niosh_dose: f64,
    /// Σ dt · 10^((L − 80)/10) in seconds. WHO weekly dose = this / (40 h).
    pub who_weekly_energy_seconds: f64,
    /// Seconds of audio counted into the dose (time with signal).
    pub dose_seconds: f64,
    /// Σ dt · 10^(L/10) for the session Leq, in seconds (`L` is the A-weighted level).
    pub session_energy_seconds: f64,
    /// Σ dt for the session Leq.
    pub session_seconds: f64,
    /// Calendar day the NIOSH dose belongs to, `yyyy-MM-dd`.
    pub day_stamp: String,
    /// ISO-8601 week the WHO allowance belongs to, `yyyy-Www`.
    pub week_stamp: String,
}

impl SplDoseState {
    /// WHO / ITU-T H.870 weekly dose, 1.0 = the whole adult weekly allowance.
    pub fn who_weekly_dose(&self) -> f64 {
        self.who_weekly_energy_seconds / SplDoseOptions::WHO_WEEKLY_SECONDS
    }

    /// `yyyy-MM-dd` in the listener's own time zone: a listening day is a local day.
    pub fn day_stamp_for(time: LocalTime) -> String {
        let (year, month, day) = civil_from_days(time.local_days());
        format!("{year:04}-{month:02}-{day:02}")
    }

    /// `yyyy-Www` with ISO-8601 week numbers, so a week starts on Monday.
    ///
    /// The ISO year and week of a day are those of the Thursday of its week.
    pub fn week_stamp_for(time: LocalTime) -> String {
        let days = time.local_days();
        // 1970-01-01 was a Thursday. Monday = 0 ... Sunday = 6.
        let weekday = (days + 3).rem_euclid(7);
        let thursday = days - weekday + 3;
        let (year, _, _) = civil_from_days(thursday);
        let week = (thursday - days_from_civil(year, 1, 1)) / 7 + 1;
        format!("{year:04}-W{week:02}")
    }

    /// A fresh state stamped for `time`.
    pub fn empty(time: LocalTime) -> Self {
        Self {
            day_stamp: Self::day_stamp_for(time),
            week_stamp: Self::week_stamp_for(time),
            ..Default::default()
        }
    }

    /// The same state rolled forward to `time`.
    ///
    /// A new day clears the NIOSH daily dose, the counted seconds and the
    /// session Leq. A new ISO week clears the WHO weekly allowance. Nothing
    /// else changes.
    pub fn rolled(&self, time: LocalTime) -> Self {
        let mut out = self.clone();
        let day = Self::day_stamp_for(time);
        let week = Self::week_stamp_for(time);
        if out.day_stamp != day {
            out.niosh_dose = 0.0;
            out.dose_seconds = 0.0;
            out.session_energy_seconds = 0.0;
            out.session_seconds = 0.0;
            out.day_stamp = day;
        }
        if out.week_stamp != week {
            out.who_weekly_energy_seconds = 0.0;
            out.week_stamp = week;
        }
        out
    }
}
