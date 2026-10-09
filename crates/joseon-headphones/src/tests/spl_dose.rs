//! Noise dose: NIOSH daily and WHO / ITU-T H.870 weekly, and the time-to-limit readout.
//!
//! Ported from `Tests/JoseonHeadphonesTests/SPLDoseTests.swift`.

use super::spl_support::*;
use crate::dose::{LocalTime, SplDoseOptions, SplDoseState};
use joseon_core::contracts::{SplEstimating, SplReading};

const HOUR: f64 = 3_600.0;

// MARK: NIOSH

/// The criterion itself: 85 dBA for 8 hours is 100 % of the daily dose.
#[test]
fn eight_hours_at_85_is_one_hundred_percent() {
    let reading = run(&mut estimator(), 85.0, 8.0 * HOUR, 60.0);
    near(reading.dose_niosh as f64, 1.0, 0.01);
    near(reading.dose_seconds, 8.0 * HOUR, 1e-6);
}

/// 3 dB exchange rate: +3 dB halves the allowed time.
#[test]
fn four_hours_at_88_is_one_hundred_percent() {
    let reading = run(&mut estimator(), 88.0, 4.0 * HOUR, 60.0);
    near(reading.dose_niosh as f64, 1.0, 0.01);
}

/// +9 dB is one eighth of the time: 1 hour at 94 dBA is 100 %.
#[test]
fn one_hour_at_94_is_one_hundred_percent() {
    let reading = run(&mut estimator(), 94.0, HOUR, 30.0);
    near(reading.dose_niosh as f64, 1.0, 0.01);
}

/// Half the time at the criterion is half the dose.
#[test]
fn dose_is_linear_in_time() {
    let reading = run(&mut estimator(), 85.0, 4.0 * HOUR, 60.0);
    near(reading.dose_niosh as f64, 0.5, 0.01);
}

/// The NIOSH threshold is a named option, default on: sound under 80 dBA adds no dose.
#[test]
fn threshold_is_on_by_default_and_keeps_quiet_sound_out_of_the_dose() {
    assert!(SplDoseOptions::default().apply_niosh_threshold);
    assert_eq!(SplDoseOptions::NIOSH_THRESHOLD_DBA, 80.0);
    let reading = run(&mut estimator(), 79.0, 8.0 * HOUR, 60.0);
    near(reading.dose_niosh as f64, 0.0, 1e-12);
    // The time still counts as listening, and still shows in the Leq.
    near(reading.dose_seconds, 8.0 * HOUR, 1e-6);
    near(reading.leq_a_track as f64, 79.0, 0.1);
}

/// With the threshold off, the same 79 dBA does accumulate.
#[test]
fn threshold_can_be_turned_off() {
    let mut estimator = Fixture {
        options: SplDoseOptions {
            apply_niosh_threshold: false,
        },
        ..Default::default()
    }
    .estimator();
    let reading = run(&mut estimator, 79.0, 8.0 * HOUR, 60.0);
    // T(79) = 8 h · 2^(6/3) = 32 h, so 8 h is a quarter of it.
    near(reading.dose_niosh as f64, 0.25, 0.01);
}

/// The threshold is a cliff at exactly 80, not a slope.
#[test]
fn threshold_boundary() {
    let a = run(&mut estimator(), 79.5, 600.0, 10.0);
    let b = run(&mut estimator(), 80.5, 600.0, 10.0);
    near(a.dose_niosh as f64, 0.0, 1e-12);
    assert!(b.dose_niosh > 0.0);
}

// MARK: WHO / ITU-T H.870

/// The adult weekly allowance: 80 dBA for 40 hours is the whole week.
#[test]
fn forty_hours_at_80_is_the_whole_weekly_allowance() {
    let reading = run(&mut estimator(), 80.0, 40.0 * HOUR, 120.0);
    near(reading.dose_who_weekly as f64, 1.0, 0.01);
}

/// +10 dB is one tenth of the time.
#[test]
fn four_hours_at_90_is_the_whole_weekly_allowance() {
    let reading = run(&mut estimator(), 90.0, 4.0 * HOUR, 60.0);
    near(reading.dose_who_weekly as f64, 1.0, 0.01);
}

/// H.870 states no threshold, so quiet listening does spend a little of the allowance.
#[test]
fn weekly_allowance_has_no_threshold() {
    let reading = run(&mut estimator(), 70.0, 4.0 * HOUR, 60.0);
    // 4 h at 70 dBA = 14400 · 10^-1 / 144000 = 1 %.
    near(reading.dose_who_weekly as f64, 0.01, 0.001);
}

// MARK: Silence

/// Digital silence does not advance the dose clock, however long it lasts.
#[test]
fn silent_frames_do_not_advance_the_dose_clock() {
    let mut estimator = estimator();
    run(&mut estimator, 90.0, 600.0, 10.0);
    let before = estimator.dose_state();
    // An hour of digital silence, with a loud band in the reading: the
    // `is_silent` flag alone stops the clock.
    let loud_bands = reading(95.0);
    let mut last: Option<SplReading> = None;
    for _ in 0..360 {
        last = Some(estimator.evaluate(&loud_bands, 10.0, true));
    }
    let after = estimator.dose_state();
    near(after.niosh_dose, before.niosh_dose, 1e-12);
    near(after.who_weekly_energy_seconds, before.who_weekly_energy_seconds, 1e-9);
    near(after.dose_seconds, 600.0, 1e-6);
    assert!(last.is_some());
}

/// A frame that carried no audio time does not advance it either.
#[test]
fn zero_dt_frames_do_not_advance_the_dose_clock() {
    let mut estimator = estimator();
    let bands = reading(95.0);
    for _ in 0..1_000 {
        estimator.evaluate(&bands, 0.0, false);
    }
    let state = estimator.dose_state();
    near(state.dose_seconds, 0.0, 1e-12);
    near(state.niosh_dose, 0.0, 1e-12);
}

// MARK: Time to the limit

/// With no dose spent, the time left is the whole allowance for that level.
#[test]
fn seconds_to_niosh_limit_on_a_fresh_dose() {
    let estimator = estimator();
    near(estimator.seconds_to_niosh_limit(85.0), 8.0 * HOUR, 1.0);
    near(estimator.seconds_to_niosh_limit(88.0), 4.0 * HOUR, 1.0);
    near(estimator.seconds_to_niosh_limit(94.0), HOUR, 1.0);
}

/// Under the threshold the limit is never reached.
#[test]
fn seconds_to_niosh_limit_is_infinite_under_the_threshold() {
    let estimator = estimator();
    assert_eq!(estimator.seconds_to_niosh_limit(79.9), f64::INFINITY);
    assert!(estimator.seconds_to_niosh_limit(85.0).is_finite());
}

/// Spending half the dose halves the time left.
#[test]
fn seconds_to_niosh_limit_shrinks_with_the_dose() {
    let mut estimator = estimator();
    run(&mut estimator, 85.0, 4.0 * HOUR, 60.0);
    near(estimator.seconds_to_niosh_limit(85.0), 4.0 * HOUR, 60.0);
}

/// Once the dose is spent there is no time left.
#[test]
fn seconds_to_niosh_limit_is_zero_at_full_dose() {
    let mut estimator = estimator();
    run(&mut estimator, 85.0, 8.0 * HOUR, 60.0);
    near(estimator.seconds_to_niosh_limit(85.0), 0.0, 1e-9);
}

/// The field of the reading tracks the slow level, so it settles to the same answer.
#[test]
fn reading_reports_the_time_to_the_limit() {
    let reading = run(&mut estimator(), 94.0, 60.0, 1.0);
    // 60 s of 3600 s spent, so about 3540 s left, less the first second of the slow meter.
    near(reading.seconds_to_niosh_limit, 3_540.0, 30.0);
}

// MARK: Reset

/// `reset_dose` clears the dose, the counted seconds and the session Leq.
#[test]
fn reset_dose_clears_everything_it_owns() {
    let mut estimator = estimator();
    run(&mut estimator, 90.0, HOUR, 60.0);
    estimator.reset_dose();
    let reading = run(&mut estimator, 80.0, 10.0, 1.0);
    near(reading.dose_niosh as f64, 0.0, 0.001);
    near(reading.dose_who_weekly as f64, 0.0, 0.001);
    near(reading.dose_seconds, 10.0, 1e-6);
    near(reading.leq_a_session as f64, 80.0, 0.05);
}

// MARK: Persistence

/// A dose survives a round trip through `dose_state` / `restore`.
#[test]
fn dose_state_round_trip() {
    let mut first = estimator();
    run(&mut first, 90.0, 1_800.0, 30.0);
    let saved = first.dose_state();
    assert!(saved.niosh_dose > 0.0);
    let mut second = estimator();
    second.restore(saved.clone());
    assert_eq!(second.dose_state(), saved);
    // The restored estimator carries on from where the first one stopped.
    let a = run(&mut first, 90.0, 600.0, 30.0);
    let b = run(&mut second, 90.0, 600.0, 30.0);
    near(b.dose_niosh as f64, a.dose_niosh as f64, 1e-6);
    near(b.dose_seconds, a.dose_seconds, 1e-9);
    near(b.dose_who_weekly as f64, a.dose_who_weekly as f64, 1e-6);
}

/// The app can write the state as JSON and read it back exactly.
#[test]
fn dose_state_survives_json() {
    let mut estimator = estimator();
    run(&mut estimator, 92.0, 900.0, 30.0);
    let saved = estimator.dose_state();
    let text = serde_json::to_string(&saved).unwrap();
    let decoded: SplDoseState = serde_json::from_str(&text).unwrap();
    assert_eq!(decoded, saved);
    assert!(!decoded.day_stamp.is_empty());
    assert!(!decoded.week_stamp.is_empty());
}

/// The JSON field names are those the macOS app writes.
#[test]
fn dose_state_json_uses_the_macos_field_names() {
    let text = serde_json::to_string(&SplDoseState::default()).unwrap();
    for field in [
        "nioshDose",
        "whoWeeklyEnergySeconds",
        "doseSeconds",
        "sessionEnergySeconds",
        "sessionSeconds",
        "dayStamp",
        "weekStamp",
    ] {
        assert!(text.contains(&format!("\"{field}\"")), "{field} is not in {text}");
    }
    let options = serde_json::to_string(&SplDoseOptions::default()).unwrap();
    assert_eq!(options, "{\"applyNIOSHThreshold\":true}");
}

/// Monday 2026-09-21, noon UTC.
const MONDAY_NOON_UTC: LocalTime = LocalTime {
    unix_seconds: 1_789_992_000.0,
    utc_offset_seconds: 0,
};

/// A state with real accumulators, stamped in UTC.
fn saved_state(time: LocalTime) -> SplDoseState {
    let mut estimator = Fixture {
        now: time,
        ..Default::default()
    }
    .estimator();
    run(&mut estimator, 90.0, HOUR, 60.0);
    estimator.dose_state()
}

/// A new day clears the daily dose; the weekly allowance keeps running.
#[test]
fn rollover_to_a_new_day_clears_the_daily_dose_only() {
    let saved = saved_state(MONDAY_NOON_UTC);
    assert!(saved.niosh_dose > 0.0);
    let rolled = saved.rolled(MONDAY_NOON_UTC.adding(24.0 * HOUR));
    assert_eq!(rolled.niosh_dose, 0.0);
    assert_eq!(rolled.dose_seconds, 0.0);
    assert_eq!(rolled.session_seconds, 0.0);
    assert_eq!(rolled.session_energy_seconds, 0.0);
    near(rolled.who_weekly_energy_seconds, saved.who_weekly_energy_seconds, 1e-9);
    assert_eq!(rolled.day_stamp, "2026-09-22");
    assert_eq!(
        rolled.week_stamp, saved.week_stamp,
        "Monday and Tuesday are the same ISO week"
    );
}

/// A new ISO week clears the weekly allowance too.
#[test]
fn rollover_to_a_new_week_clears_the_weekly_allowance() {
    let saved = saved_state(MONDAY_NOON_UTC);
    assert!(saved.who_weekly_energy_seconds > 0.0);
    let rolled = saved.rolled(MONDAY_NOON_UTC.adding(8.0 * 24.0 * HOUR));
    assert_eq!(rolled.who_weekly_energy_seconds, 0.0);
    assert_eq!(rolled.niosh_dose, 0.0);
    assert_ne!(rolled.week_stamp, saved.week_stamp);
}

/// A restore inside the same day and week changes nothing.
#[test]
fn rollover_inside_the_same_day_is_a_no_op() {
    let saved = saved_state(MONDAY_NOON_UTC);
    assert_eq!(saved.rolled(MONDAY_NOON_UTC.adding(120.0)), saved);
}

#[test]
fn stamp_formats() {
    assert_eq!(SplDoseState::day_stamp_for(MONDAY_NOON_UTC), "2026-09-21");
    assert_eq!(SplDoseState::week_stamp_for(MONDAY_NOON_UTC), "2026-W39");
}

/// A day stamp and a week stamp taken with the same offset never disagree
/// about which midnight they mean.
#[test]
fn stamps_agree_across_time_zones() {
    // Sunday 2026-09-20 16:00 UTC is already Monday 01:00 in Tokyo: a new day AND a new week.
    let tokyo = LocalTime::new(1_789_920_000.0, 9 * 3_600);
    let utc = LocalTime::utc(1_789_920_000.0);
    assert_eq!(SplDoseState::day_stamp_for(tokyo), "2026-09-21");
    assert_eq!(SplDoseState::week_stamp_for(tokyo), "2026-W39");
    assert_eq!(SplDoseState::day_stamp_for(utc), "2026-09-20");
    assert_eq!(SplDoseState::week_stamp_for(utc), "2026-W38");
}

/// Not in the macOS tests: the calendar math is written by hand here, so the
/// hard ISO-8601 cases are checked against known dates.
#[test]
fn iso_weeks_at_the_ends_of_years() {
    let at = |unix: f64| {
        let time = LocalTime::utc(unix);
        (SplDoseState::day_stamp_for(time), SplDoseState::week_stamp_for(time))
    };
    let pair = |day: &str, week: &str| (day.to_string(), week.to_string());
    // Thursday 1970-01-01 is in week 1 of 1970.
    assert_eq!(at(0.0), pair("1970-01-01", "1970-W01"));
    // Friday 2021-01-01 belongs to week 53 of 2020.
    assert_eq!(at(1_609_459_200.0), pair("2021-01-01", "2020-W53"));
    // Monday 2024-12-30 belongs to week 1 of 2025.
    assert_eq!(at(1_735_516_800.0), pair("2024-12-30", "2025-W01"));
    // Leap day.
    assert_eq!(at(1_709_164_800.0), pair("2024-02-29", "2024-W09"));
    // Sunday 2027-01-03 belongs to week 53 of 2026.
    assert_eq!(at(1_798_934_400.0), pair("2027-01-03", "2026-W53"));
    // One second before 1970.
    assert_eq!(at(-1.0), pair("1969-12-31", "1970-W01"));
}
