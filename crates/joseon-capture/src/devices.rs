//! The output devices of the system, for the device list of the probe and the
//! settings of the app.

/// One output device (a Windows render endpoint).
#[derive(Debug, Clone, PartialEq)]
pub struct OutputDevice {
    /// The id the system gives the endpoint. It stays the same across restarts.
    pub id: String,
    /// For example "Speakers (USB DAC)".
    pub name: String,
    pub is_default: bool,
    /// The format of the shared mix: what loopback capture delivers.
    pub mix_sample_rate: f64,
    pub mix_channels: usize,
    /// The bit depth the device runs at, when the system tells it.
    pub device_bits: Option<u32>,
    /// The rate the device runs at, when the system tells it.
    pub device_sample_rate: Option<f64>,
}
