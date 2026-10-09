//! L1. Audio capture into the core ring buffer.
//!
//! Windows: WASAPI loopback of the default output device (`wasapi.rs`). It
//! reads the shared mix, in the format of the mix, with no driver and no
//! permission prompt. The thread, the health states and the conversion of the
//! packets are in `source.rs` and `convert.rs`: they have no system call in
//! them and are tested on every system.
//!
//! Also here: the volume of the output device (`volume.rs`) and what the players
//! of the system play now (`now_playing.rs`).
//!
//! Not here yet: process loopback (the sound of one program) and the
//! measurement microphone input.
//!
//! Port source: `Sources/JoseonCapture/SystemAudioTap.swift` in the macOS
//! repo. This is a rewrite, not a translation: the `AudioSource` contract and
//! the behavior carry over (follow the default device, name the programs that
//! play, say why there is silence).

pub mod convert;
pub mod devices;
pub mod format;
#[cfg(windows)]
pub mod media_windows;
pub mod now_playing;
pub mod source;
pub mod volume;
#[cfg(windows)]
pub mod wasapi;

pub use devices::OutputDevice;
pub use joseon_core::contracts::{AudioSource, SourceError, StreamInfo};
pub use now_playing::NowPlayingReader;
pub use source::{CaptureHealth, LoopbackSource, OpenError};
pub use volume::OutputVolumeState;

#[cfg(windows)]
mod system {
    pub use crate::wasapi::WasapiBackend as SystemBackend;

    pub fn make() -> Result<SystemBackend, crate::OpenError> {
        SystemBackend::new()
    }

    pub fn output_devices() -> Result<Vec<crate::OutputDevice>, String> {
        let backend = SystemBackend::new().map_err(|error| format!("{error:?}"))?;
        backend.output_devices()
    }

    pub fn default_output_volume() -> Option<crate::OutputVolumeState> {
        SystemBackend::new().ok()?.default_volume()
    }
}

#[cfg(not(windows))]
mod system {
    use crate::format::WireFormat;
    use crate::source::{Backend, Endpoint, OpenError, PacketSink, StreamEnded};
    use joseon_core::contracts::StreamInfo;

    pub const WINDOWS_ONLY: &str = "system audio capture needs Windows";

    /// A system with no loopback capture. It cannot be made, so its stream never exists.
    pub enum SystemBackend {}
    pub enum NoStream {}

    impl Endpoint for NoStream {
        fn info(&self) -> StreamInfo {
            match *self {}
        }
        fn format(&self) -> WireFormat {
            match *self {}
        }
        fn drain(&mut self, _sink: &mut PacketSink<'_>) -> Result<(), StreamEnded> {
            match *self {}
        }
        fn active_sources(&mut self) -> Vec<String> {
            match *self {}
        }
    }

    impl Backend for SystemBackend {
        type Stream = NoStream;
        fn open(&mut self) -> Result<NoStream, OpenError> {
            match *self {}
        }
        fn is_current(&mut self, _stream: &NoStream) -> bool {
            match *self {}
        }
    }

    pub fn make() -> Result<SystemBackend, OpenError> {
        Err(OpenError::Platform(WINDOWS_ONLY.to_string()))
    }

    pub fn output_devices() -> Result<Vec<crate::OutputDevice>, String> {
        Err(WINDOWS_ONLY.to_string())
    }

    pub fn default_output_volume() -> Option<crate::OutputVolumeState> {
        None
    }
}

/// The capture of the sound the computer plays.
pub type SystemLoopback = LoopbackSource<system::SystemBackend>;

/// A stopped capture of the default output device. `start` opens the stream.
pub fn system_loopback() -> SystemLoopback {
    LoopbackSource::with_backend(system::make)
}

/// The output devices that are switched on. An error text on a system with no capture.
pub fn output_devices() -> Result<Vec<OutputDevice>, String> {
    system::output_devices()
}

/// A stopped reader of what the players of the system play. On a system with no list of players it runs and
/// knows nothing.
pub fn now_playing_reader() -> NowPlayingReader {
    #[cfg(windows)]
    {
        NowPlayingReader::with_reader(media_windows::session_reader)
    }
    #[cfg(not(windows))]
    {
        NowPlayingReader::with_reader(|| None)
    }
}

/// The volume of the default output device now. `None` when there is no output device (or no capture
/// on this system). Call it from a thread that is not the UI thread: it asks the audio system.
pub fn default_output_volume() -> Option<OutputVolumeState> {
    system::default_output_volume()
}

#[cfg(all(test, not(windows)))]
mod tests {
    use super::*;

    #[test]
    fn a_system_with_no_capture_says_so() {
        let mut source = system_loopback();
        assert_eq!(
            source.start(),
            Err(SourceError::Platform("system audio capture needs Windows".to_string()))
        );
        assert!(!source.is_running());
        assert_eq!(source.health(), CaptureHealth::Stopped);
        assert!(output_devices().unwrap_err().contains("Windows"));
        assert_eq!(default_output_volume(), None);
    }
}
