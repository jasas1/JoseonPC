//! The offset of the local time zone from UTC. The standard library has no call for it.

/// Seconds east of UTC, now. 0 when the system does not tell it.
#[cfg(windows)]
pub fn utc_offset_seconds() -> i32 {
    use windows::Win32::System::Time::{GetTimeZoneInformation, TIME_ZONE_INFORMATION};
    let mut zone = TIME_ZONE_INFORMATION::default();
    // 1 = standard time, 2 = daylight time, 0 = the zone has no daylight time.
    let kind = unsafe { GetTimeZoneInformation(&mut zone) };
    let extra = match kind {
        1 => zone.StandardBias,
        2 => zone.DaylightBias,
        _ => 0,
    };
    // The bias is "UTC = local time + bias", in minutes.
    -(zone.Bias + extra) * 60
}

/// Seconds east of UTC, now. 0 when the system does not tell it.
#[cfg(not(windows))]
pub fn utc_offset_seconds() -> i32 {
    // `date +%z` prints "+0200" or "-0730".
    let Ok(output) = std::process::Command::new("date").arg("+%z").output() else {
        return 0;
    };
    parse_offset(String::from_utf8_lossy(&output.stdout).trim()).unwrap_or(0)
}

/// "+0200" gives 7200, "-0730" gives -27000.
pub fn parse_offset(text: &str) -> Option<i32> {
    let (sign, digits) = match text.split_at_checked(1)? {
        ("+", digits) => (1, digits),
        ("-", digits) => (-1, digits),
        _ => return None,
    };
    if digits.len() != 4 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let (hours, minutes): (i32, i32) = (digits[..2].parse().ok()?, digits[2..].parse().ok()?);
    Some(sign * (hours * 3600 + minutes * 60))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offsets_are_read() {
        assert_eq!(parse_offset("+0200"), Some(7_200));
        assert_eq!(parse_offset("-0730"), Some(-27_000));
        assert_eq!(parse_offset("+0000"), Some(0));
        for bad in ["", "0200", "+02", "+02:00", "UTC"] {
            assert_eq!(parse_offset(bad), None, "{bad}");
        }
        // A real zone is inside a day.
        assert!(utc_offset_seconds().abs() <= 14 * 3600);
    }
}
