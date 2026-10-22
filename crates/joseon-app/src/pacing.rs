//! How often the main window draws: the "Animation" setting against the refresh rate of the display.
//!
//! Port of the frame pacing of the macOS app (`PanelView.frameRateRange`, 0.1.2). macOS asks each panel's display
//! link for a rate. Here one surface holds every card, so the whole window draws on every n-th refresh of its
//! display. The analysis keeps its own rate in both modes.

use crate::settings::DisplayRefreshMode;

/// The display rate when the system does not tell it.
pub const FALLBACK_REFRESH_HZ: f64 = 60.0;

/// The rate Balanced aims at: the analysis makes about 60 frames per second.
pub const BALANCED_FPS: f64 = 60.0;

/// Draw on every n-th refresh of the display. Balanced: the n that comes nearest to 60 pictures per second
/// (2 on a 120 or 144 Hz display, 4 on 240 Hz). High Refresh: every refresh.
pub fn refreshes_per_picture(mode: DisplayRefreshMode, display_hz: Option<f64>) -> u32 {
    let hz = display_hz
        .filter(|hz| hz.is_finite() && *hz >= 1.0)
        .unwrap_or(FALLBACK_REFRESH_HZ);
    match mode {
        DisplayRefreshMode::HighRefresh => 1,
        DisplayRefreshMode::Balanced => (hz / BALANCED_FPS).round().max(1.0) as u32,
    }
}

/// When to ask for the next picture after the one that started at `last`: half a refresh before the n-th refresh,
/// so the surface, which waits for the display, shows it on that refresh. `None` = ask at once.
pub fn next_picture_delay(mode: DisplayRefreshMode, display_hz: Option<f64>) -> Option<std::time::Duration> {
    let n = refreshes_per_picture(mode, display_hz);
    if n <= 1 {
        return None;
    }
    let hz = display_hz
        .filter(|hz| hz.is_finite() && *hz >= 1.0)
        .unwrap_or(FALLBACK_REFRESH_HZ);
    Some(std::time::Duration::from_secs_f64((n as f64 - 0.5) / hz))
}

#[cfg(test)]
mod tests {
    use super::*;
    use DisplayRefreshMode::{Balanced, HighRefresh};

    #[test]
    fn balanced_comes_near_sixty_pictures_per_second() {
        for (hz, n) in [
            (60.0, 1),
            (75.0, 1),
            (100.0, 2),
            (120.0, 2),
            (144.0, 2),
            (165.0, 3),
            (240.0, 4),
            (360.0, 6),
        ] {
            assert_eq!(refreshes_per_picture(Balanced, Some(hz)), n, "{hz} Hz");
        }
    }

    #[test]
    fn high_refresh_draws_on_every_refresh() {
        for hz in [60.0, 144.0, 240.0] {
            assert_eq!(refreshes_per_picture(HighRefresh, Some(hz)), 1);
            assert_eq!(next_picture_delay(HighRefresh, Some(hz)), None);
        }
    }

    #[test]
    fn an_unknown_or_odd_rate_counts_as_sixty() {
        for hz in [None, Some(0.0), Some(f64::NAN), Some(-5.0)] {
            assert_eq!(refreshes_per_picture(Balanced, hz), 1);
            assert_eq!(next_picture_delay(Balanced, hz), None);
        }
    }

    #[test]
    fn the_delay_ends_half_a_refresh_before_the_nth() {
        let delay = next_picture_delay(Balanced, Some(144.0)).unwrap().as_secs_f64();
        assert!((delay - 1.5 / 144.0).abs() < 1e-9);
        let delay = next_picture_delay(Balanced, Some(240.0)).unwrap().as_secs_f64();
        assert!((delay - 3.5 / 240.0).abs() < 1e-9);
    }
}
