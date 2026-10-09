//! WASAPI loopback: the shared mix of the default output device, read with no
//! driver and no permission prompt.
//!
//! Every COM object here lives on the capture thread (`source.rs` makes the
//! backend there). The stream is polled: loopback sends packets only while
//! something plays, and a poll every few milliseconds keeps the code free of
//! event handles. The buffer holds 200 ms, so a late poll loses nothing.

use crate::devices::OutputDevice;
use crate::format::{WireFormat, BASE_BYTES};
use crate::source::{Backend, Endpoint, OpenError, Packet, PacketSink, StreamEnded};
use crate::volume::OutputVolumeState;
use joseon_core::contracts::StreamInfo;
use windows::core::{Interface, GUID, HRESULT, PWSTR};
use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
use windows::Win32::Foundation::{CloseHandle, PROPERTYKEY, S_OK};
use windows::Win32::Media::Audio::Endpoints::IAudioEndpointVolume;
use windows::Win32::Media::Audio::{
    eConsole, eRender, AudioSessionStateActive, IAudioCaptureClient, IAudioClient, IAudioSessionControl2,
    IAudioSessionManager2, IMMDevice, IMMDeviceEnumerator, MMDeviceEnumerator, AUDCLNT_BUFFERFLAGS_SILENT,
    AUDCLNT_E_DEVICE_INVALIDATED, AUDCLNT_E_DEVICE_IN_USE, AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
    DEVICE_STATE_ACTIVE, WAVEFORMATEX,
};
use windows::Win32::System::Com::{
    CoCreateInstance, CoInitializeEx, CoTaskMemFree, CoUninitialize, CLSCTX_ALL, COINIT_MULTITHREADED, STGM_READ,
};
use windows::Win32::System::Threading::{
    OpenProcess, QueryFullProcessImageNameW, PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::PropertiesSystem::IPropertyStore;

/// The device has no endpoint of the asked kind (`ERROR_NOT_FOUND` as an HRESULT).
const E_NOT_FOUND: HRESULT = HRESULT(0x8007_0490_u32 as i32);
/// The format the device itself runs at (not the mix): a `WAVEFORMATEX` in a blob.
const PKEY_AUDIOENGINE_DEVICEFORMAT: PROPERTYKEY = PROPERTYKEY {
    fmtid: GUID::from_u128(0xf19f064d_082c_4e27_bc73_6882a1bb8e4c),
    pid: 0,
};
/// 200 ms, in units of 100 ns.
const BUFFER_DURATION: i64 = 2_000_000;

fn message(what: &str, error: &windows::core::Error) -> String {
    format!("{what}: {} ({:#010x})", error.message().trim(), error.code().0 as u32)
}

/// A string the system allocated, copied and freed.
unsafe fn take_string(text: PWSTR) -> String {
    if text.is_null() {
        return String::new();
    }
    let copy = text.to_string().unwrap_or_default();
    CoTaskMemFree(Some(text.as_ptr() as *const _));
    copy
}

unsafe fn device_id(device: &IMMDevice) -> String {
    device.GetId().map(|id| take_string(id)).unwrap_or_default()
}

unsafe fn device_name(store: Option<&IPropertyStore>) -> String {
    store
        .and_then(|store| store.GetValue(&PKEY_Device_FriendlyName).ok())
        .map(|value| value.to_string())
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "Output device".to_string())
}

/// The format the device runs at: bit depth and rate. `None` when the system does not tell it.
unsafe fn device_format(store: Option<&IPropertyStore>) -> Option<WireFormat> {
    let value = store?.GetValue(&PKEY_AUDIOENGINE_DEVICEFORMAT).ok()?;
    // VT_BLOB: a byte count and a pointer. The value owns the bytes until it is dropped.
    let inner = &value.Anonymous.Anonymous;
    if inner.vt.0 != 65 {
        return None;
    }
    let blob = inner.Anonymous.blob;
    if blob.pBlobData.is_null() || (blob.cbSize as usize) < 16 {
        return None;
    }
    let bytes = std::slice::from_raw_parts(blob.pBlobData, blob.cbSize as usize);
    WireFormat::parse(bytes).ok()
}

/// The mix format of a client, as our own structure.
unsafe fn mix_format(client: &IAudioClient) -> Result<(WireFormat, *mut WAVEFORMATEX), String> {
    let pointer = client
        .GetMixFormat()
        .map_err(|e| message("the mix format is not available", &e))?;
    let head = std::slice::from_raw_parts(pointer as *const u8, BASE_BYTES);
    let bytes = std::slice::from_raw_parts(pointer as *const u8, WireFormat::structure_bytes(head));
    match WireFormat::parse(bytes) {
        Ok(format) => Ok((format, pointer)),
        Err(error) => {
            CoTaskMemFree(Some(pointer as *const _));
            Err(error.to_string())
        }
    }
}

/// "C:\Program Files\Qobuz\Qobuz.exe" gives "Qobuz".
pub(crate) fn program_name(path: &str) -> String {
    let file = path.rsplit(['\\', '/']).next().unwrap_or(path);
    let stem = match file.rsplit_once('.') {
        Some((stem, extension)) if extension.eq_ignore_ascii_case("exe") => stem,
        _ => file,
    };
    stem.to_string()
}

unsafe fn process_name(pid: u32) -> Option<String> {
    let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid).ok()?;
    let mut buffer = [0u16; 1024];
    let mut length = buffer.len() as u32;
    let result = QueryFullProcessImageNameW(process, PROCESS_NAME_WIN32, PWSTR(buffer.as_mut_ptr()), &mut length);
    let _ = CloseHandle(process);
    result.ok()?;
    let path = String::from_utf16_lossy(&buffer[..length as usize]);
    Some(program_name(&path)).filter(|name| !name.is_empty())
}

/// A device with a volume control in hardware: Windows leaves the mix alone (`ENDPOINT_HARDWARE_SUPPORT_VOLUME`).
const HARDWARE_VOLUME: u32 = 0x1;

/// The volume control of a device, and "the device sets the volume itself" (a hardware volume control). With
/// no hardware control Windows scales the mix in software, and loopback capture reads the mix after that
/// scaling. `None` for a device with no control at all.
unsafe fn endpoint_volume(device: &IMMDevice) -> Option<(IAudioEndpointVolume, bool)> {
    let volume = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None).ok()?;
    let support = volume.QueryHardwareSupport().ok()?;
    Some((volume, support & HARDWARE_VOLUME != 0))
}

/// One open loopback stream on one device.
pub struct WasapiStream {
    device_id: String,
    info: StreamInfo,
    format: WireFormat,
    client: IAudioClient,
    capture: IAudioCaptureClient,
    sessions: Option<IAudioSessionManager2>,
    /// The volume control of the device, and "the device sets the volume itself".
    volume: Option<(IAudioEndpointVolume, bool)>,
}

impl WasapiStream {
    unsafe fn open(device: &IMMDevice) -> Result<Self, OpenError> {
        let refused = |what: &str, error: windows::core::Error| {
            if error.code() == AUDCLNT_E_DEVICE_IN_USE {
                OpenError::Exclusive
            } else {
                OpenError::Platform(message(what, &error))
            }
        };
        let store = device.OpenPropertyStore(STGM_READ).ok();
        let client: IAudioClient = device
            .Activate(CLSCTX_ALL, None)
            .map_err(|e| refused("the device gives no audio client", e))?;
        let (format, wave_format) = mix_format(&client).map_err(OpenError::Platform)?;
        let started = client
            .Initialize(
                AUDCLNT_SHAREMODE_SHARED,
                AUDCLNT_STREAMFLAGS_LOOPBACK,
                BUFFER_DURATION,
                0,
                wave_format,
                None,
            )
            .map_err(|e| refused("the loopback stream did not open", e));
        CoTaskMemFree(Some(wave_format as *const _));
        started?;
        let capture: IAudioCaptureClient = client
            .GetService()
            .map_err(|e| refused("the stream gives no capture service", e))?;
        client.Start().map_err(|e| refused("the stream did not start", e))?;
        let on_device = device_format(store.as_ref());
        let info = StreamInfo {
            bit_depth: on_device.and_then(|f| f.integer_bits()).or(format.integer_bits()),
            device_is_default: Some(true),
            ..StreamInfo::new(format.sample_rate, format.channels, device_name(store.as_ref()))
        };
        Ok(Self {
            device_id: device_id(device),
            info,
            format,
            client,
            capture,
            sessions: device.Activate(CLSCTX_ALL, None).ok(),
            volume: endpoint_volume(device),
        })
    }
}

impl Drop for WasapiStream {
    fn drop(&mut self) {
        unsafe {
            let _ = self.client.Stop();
        }
    }
}

impl Endpoint for WasapiStream {
    fn info(&self) -> StreamInfo {
        self.info.clone()
    }

    fn format(&self) -> WireFormat {
        self.format
    }

    fn drain(&mut self, sink: &mut PacketSink<'_>) -> Result<(), StreamEnded> {
        let ended = |what: &str, error: windows::core::Error| {
            let text = if error.code() == AUDCLNT_E_DEVICE_INVALIDATED {
                "the output device changed".to_string()
            } else {
                message(what, &error)
            };
            StreamEnded(text)
        };
        let frame_bytes = self.format.frame_bytes();
        unsafe {
            loop {
                let waiting = self
                    .capture
                    .GetNextPacketSize()
                    .map_err(|e| ended("the stream stopped", e))?;
                if waiting == 0 {
                    return Ok(());
                }
                let mut data = std::ptr::null_mut();
                let (mut frames, mut flags) = (0u32, 0u32);
                self.capture
                    .GetBuffer(&mut data, &mut frames, &mut flags, None, None)
                    .map_err(|e| ended("the stream gave no packet", e))?;
                let silent = flags & AUDCLNT_BUFFERFLAGS_SILENT.0 as u32 != 0;
                if silent || data.is_null() {
                    sink(Packet::Silence(frames as usize));
                } else {
                    sink(Packet::Sound(std::slice::from_raw_parts(
                        data,
                        frames as usize * frame_bytes,
                    )));
                }
                self.capture
                    .ReleaseBuffer(frames)
                    .map_err(|e| ended("the packet was not given back", e))?;
            }
        }
    }

    fn volume_factor(&mut self, also_hardware: bool) -> Option<f32> {
        let (volume, hardware) = self.volume.as_ref()?;
        if *hardware && !also_hardware {
            return None;
        }
        unsafe {
            // Muted: Windows sends silence. There is nothing to bring back.
            if volume.GetMute().map(|m| m.as_bool()).unwrap_or(false) {
                return None;
            }
            let db = volume.GetMasterVolumeLevel().ok()?;
            Some(10f32.powf(db.min(0.0) / 20.0))
        }
    }

    fn active_sources(&mut self) -> Vec<String> {
        let mut names: Vec<String> = Vec::new();
        let Some(manager) = &self.sessions else {
            return names;
        };
        unsafe {
            let Ok(list) = manager.GetSessionEnumerator() else {
                return names;
            };
            for i in 0..list.GetCount().unwrap_or(0) {
                let Ok(session) = list.GetSession(i) else { continue };
                if session.GetState() != Ok(AudioSessionStateActive) {
                    continue;
                }
                let Ok(session) = session.cast::<IAudioSessionControl2>() else {
                    continue;
                };
                // The session of the system sounds (S_OK) is not a player.
                if session.IsSystemSoundsSession() == S_OK {
                    continue;
                }
                let own = std::process::id();
                // A second Joseon (one that waits in the tray) captures too, and Windows counts that as a
                // session. It is not a player.
                let own_name = std::env::current_exe()
                    .ok()
                    .map(|path| program_name(&path.to_string_lossy()).to_lowercase());
                let name = match session.GetProcessId() {
                    Ok(pid) if pid == own || pid == 0 => continue,
                    // A program whose name Windows keeps to itself still counts as playing.
                    Ok(pid) => process_name(pid).or_else(|| Some(format!("process {pid}"))),
                    Err(_) => None,
                };
                if let Some(name) = name {
                    if Some(name.to_lowercase()) == own_name {
                        continue;
                    }
                    if !names.contains(&name) {
                        names.push(name);
                    }
                }
            }
        }
        names.sort();
        names
    }
}

/// The devices of the system, on one thread.
pub struct WasapiBackend {
    /// `None` only while the value is dropped.
    enumerator: Option<IMMDeviceEnumerator>,
    /// CoUninitialize is owed when CoInitializeEx gave S_OK or S_FALSE.
    com: bool,
}

impl WasapiBackend {
    /// Call it on the thread that will use it.
    pub fn new() -> Result<Self, OpenError> {
        unsafe {
            let com = CoInitializeEx(None, COINIT_MULTITHREADED).is_ok();
            let enumerator = CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).map_err(|e| {
                if com {
                    CoUninitialize();
                }
                OpenError::Platform(message("the audio system of Windows did not answer", &e))
            })?;
            Ok(Self {
                enumerator: Some(enumerator),
                com,
            })
        }
    }

    fn enumerator(&self) -> &IMMDeviceEnumerator {
        self.enumerator
            .as_ref()
            .expect("the enumerator lives as long as the backend")
    }

    unsafe fn default_device(&self) -> Result<IMMDevice, OpenError> {
        self.enumerator()
            .GetDefaultAudioEndpoint(eRender, eConsole)
            .map_err(|e| {
                if e.code() == E_NOT_FOUND {
                    OpenError::NoDevice
                } else {
                    OpenError::Platform(message("no default output device", &e))
                }
            })
    }

    /// The volume of the default output device now. `None` when there is no output device.
    pub fn default_volume(&self) -> Option<OutputVolumeState> {
        unsafe {
            let device = self.default_device().ok()?;
            let store = device.OpenPropertyStore(STGM_READ).ok();
            let mut state = OutputVolumeState {
                device_id: device_id(&device),
                device_name: device_name(store.as_ref()),
                volume_db: None,
                max_volume_db: None,
                volume_scalar: None,
                is_muted: false,
                hardware_volume: None,
            };
            // A device with no volume control gives no such interface, or gives errors: the fields stay empty.
            if let Ok(volume) = device.Activate::<IAudioEndpointVolume>(CLSCTX_ALL, None) {
                state.hardware_volume = volume.QueryHardwareSupport().ok().map(|s| s & HARDWARE_VOLUME != 0);
                state.volume_db = volume.GetMasterVolumeLevel().ok().map(f64::from);
                state.volume_scalar = volume.GetMasterVolumeLevelScalar().ok().map(f64::from);
                state.is_muted = volume.GetMute().map(|m| m.as_bool()).unwrap_or(false);
                let (mut low, mut high, mut step) = (0.0f32, 0.0f32, 0.0f32);
                if volume.GetVolumeRange(&mut low, &mut high, &mut step).is_ok() {
                    state.max_volume_db = Some(high as f64);
                }
            }
            Some(state)
        }
    }

    /// Every output device that is switched on.
    pub fn output_devices(&self) -> Result<Vec<OutputDevice>, String> {
        unsafe {
            let default_id = self.default_device().map(|d| device_id(&d)).unwrap_or_default();
            let list = self
                .enumerator()
                .EnumAudioEndpoints(eRender, DEVICE_STATE_ACTIVE)
                .map_err(|e| message("the device list is not available", &e))?;
            let mut devices = Vec::new();
            for i in 0..list.GetCount().unwrap_or(0) {
                let Ok(device) = list.Item(i) else { continue };
                let store = device.OpenPropertyStore(STGM_READ).ok();
                let id = device_id(&device);
                let mix = device
                    .Activate::<IAudioClient>(CLSCTX_ALL, None)
                    .ok()
                    .and_then(|client| mix_format(&client).ok())
                    .map(|(format, pointer)| {
                        CoTaskMemFree(Some(pointer as *const _));
                        format
                    });
                let on_device = device_format(store.as_ref());
                devices.push(OutputDevice {
                    is_default: !id.is_empty() && id == default_id,
                    id,
                    name: device_name(store.as_ref()),
                    mix_sample_rate: mix.map_or(0.0, |f| f.sample_rate),
                    mix_channels: mix.map_or(0, |f| f.channels),
                    device_bits: on_device.and_then(|f| f.integer_bits()),
                    device_sample_rate: on_device.map(|f| f.sample_rate),
                });
            }
            Ok(devices)
        }
    }
}

impl Drop for WasapiBackend {
    fn drop(&mut self) {
        // The interface goes first: nothing of COM may be used after CoUninitialize.
        self.enumerator = None;
        if self.com {
            unsafe { CoUninitialize() };
        }
    }
}

impl Backend for WasapiBackend {
    type Stream = WasapiStream;

    fn open(&mut self) -> Result<WasapiStream, OpenError> {
        unsafe { WasapiStream::open(&self.default_device()?) }
    }

    fn is_current(&mut self, stream: &WasapiStream) -> bool {
        // No default device at all: the stream ends by itself. Do not close it early.
        unsafe {
            self.default_device()
                .map_or(true, |device| device_id(&device) == stream.device_id)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::LoopbackSource;
    use joseon_core::contracts::{AudioSource, SourceError};

    #[test]
    fn program_names_come_from_paths() {
        assert_eq!(program_name("C:\\Program Files\\Qobuz\\Qobuz.exe"), "Qobuz");
        assert_eq!(program_name("D:\\Apps\\foobar2000.EXE"), "foobar2000");
        assert_eq!(
            program_name("\\Device\\HarddiskVolume3\\Windows\\System32\\audiodg"),
            "audiodg"
        );
        assert_eq!(program_name("chrome.exe"), "chrome");
    }

    /// The test machine may have no sound device (a build server has none). The test then says so and
    /// checks that the code tells it cleanly. With a device it reads the device list and runs the
    /// capture for a moment.
    #[test]
    fn the_capture_starts_or_says_why_not() {
        let backend = WasapiBackend::new().expect("the audio system of Windows answers");
        let devices = backend.output_devices().expect("a device list, also an empty one");
        println!("output devices: {devices:#?}");
        println!("volume: {:?}", backend.default_volume());
        drop(backend);
        let mut source = LoopbackSource::with_backend(WasapiBackend::new);
        let mut reader = source.take_reader().unwrap();
        match source.start() {
            Ok(()) => {
                std::thread::sleep(std::time::Duration::from_millis(600));
                println!(
                    "capture runs: {:?}, health {:?}, {} packets, {} frames, {} frames in the ring",
                    source.stream_info(),
                    source.health(),
                    source.packet_count(),
                    source.frames_delivered(),
                    reader.available_frames()
                );
                assert!(source.is_running());
                let (mut left, mut right) = (vec![0.0f32; 4096], vec![0.0f32; 4096]);
                let _ = reader.read(&mut left, &mut right);
                source.stop();
                assert!(!source.is_running());
            }
            Err(SourceError::NoDevice) => {
                println!("no output device on this machine: the capture said so");
                assert!(devices.iter().all(|d| !d.is_default));
            }
            Err(SourceError::Platform(text)) => {
                println!("the capture did not start: {text}");
                assert!(!text.is_empty());
            }
        }
    }
}
