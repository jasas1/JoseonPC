//! The software volume of an output device, as the system reports it.
//!
//! The level at the ear depends on it when the capture point is before the
//! volume control. Reading only: nothing here changes the volume.
//!
//! Port source: `OutputVolumeState` in `Sources/JoseonCapture/OutputVolume.swift`.

#[derive(Debug, Clone, PartialEq)]
pub struct OutputVolumeState {
    /// The id the system gives the device.
    pub device_id: String,
    pub device_name: String,
    /// Current volume in dB. `None` when the device has no volume control that the system can set.
    pub volume_db: Option<f64>,
    /// Top of the volume range in dB (often 0). `None` when unknown.
    pub max_volume_db: Option<f64>,
    /// Current volume as the 0...1 position of the volume slider. `None` when the device has none.
    pub volume_scalar: Option<f64>,
    pub is_muted: bool,
    /// True when the device itself sets the volume (after the point where the capture takes the mix).
    /// `None` when the system does not tell.
    pub hardware_volume: Option<bool>,
}

impl OutputVolumeState {
    pub fn has_software_volume(&self) -> bool {
        self.volume_db.is_some()
    }

    /// dB below the maximum volume, 0 or negative. `None` without a software volume.
    /// When the system gives no range, the top of the range counts as 0 dB.
    pub fn attenuation_db(&self) -> Option<f64> {
        self.volume_db
            .map(|db| (db - self.max_volume_db.unwrap_or(0.0)).min(0.0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_attenuation_counts_from_the_top_of_the_range() {
        let mut state = OutputVolumeState {
            device_id: "id".to_string(),
            device_name: "Speakers".to_string(),
            volume_db: Some(-12.5),
            max_volume_db: Some(0.0),
            volume_scalar: Some(0.4),
            is_muted: false,
            hardware_volume: None,
        };
        assert!(state.has_software_volume());
        assert_eq!(state.attenuation_db(), Some(-12.5));
        // A device whose range ends at +6 dB.
        state.max_volume_db = Some(6.0);
        assert_eq!(state.attenuation_db(), Some(-18.5));
        state.max_volume_db = None;
        assert_eq!(state.attenuation_db(), Some(-12.5));
        // A reading over the stated top is not a gain.
        state.volume_db = Some(1.0);
        assert_eq!(state.attenuation_db(), Some(0.0));
        state.volume_db = None;
        assert_eq!((state.has_software_volume(), state.attenuation_db()), (false, None));
    }
}
