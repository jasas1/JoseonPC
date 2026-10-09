# Decisions

Dated 2026-10-07. Each decision lists the reason and what would change it.

## D1. Language: Rust

Decided. One Cargo workspace, six crates, same layers as the macOS app.

## D2. Graphics: wgpu, Direct3D 12 first, Vulkan as fallback

Do not write to DX12 or Vulkan directly. Use `wgpu` and let it pick the backend.

- wgpu runs on Direct3D 12, Vulkan and Metal from one code base. Shaders are written once in WGSL.
- Default backend on Windows is Direct3D 12. It is part of Windows 10 and 11, so it works on every
  PC with a driver, including laptops with Intel graphics.
- Vulkan is the fallback. A setting or the `WGPU_BACKEND=vulkan` variable selects it. Use it when a
  DX12 driver bug shows up.
- The Metal backend means `joseon-render` builds and runs on a Mac. Panel work and the offscreen
  render tests do not need a Windows PC. Only capture and the tray need one.
- The panels are 2D lines, quads and one scrolling texture. Nothing here needs a feature that only
  raw DX12 or raw Vulkan gives.

Change this if: wgpu cannot hold 120 fps on the spectrogram on a mid-range laptop.

## D3. NVIDIA: no dependency

Nothing from NVIDIA goes into the product.

- CUDA / cuFFT: the FFT sizes Joseon uses take microseconds on a CPU. A GPU FFT adds a copy to and
  from the card and removes every AMD and Intel user.
- Maxine / Broadcast audio effects: these remove noise from voice. Joseon measures; it must not
  change the signal.
- RTX, DLSS, Reflex: for 3D games. The panels are 2D.

Useful as tools only: Nsight Graphics for frame debugging, and a test pass on an NVIDIA driver next
to AMD and Intel.

## D4. Audio capture: WASAPI loopback

- Per-app capture: process loopback (`AUDIOCLIENT_ACTIVATION_TYPE_PROCESS_LOOPBACK`). Microsoft
  documents Windows 10 build 20348 as the minimum.
- Fallback: loopback of the whole output device. Works on every supported Windows.
- Windows shows no permission prompt for loopback capture.

Known limits. These are parity risks and need a decision before the forum release:

1. **Exclusive mode is invisible.** Loopback reads the shared-mode mix. A player in WASAPI
   exclusive mode or ASIO goes around the mixer, and loopback captures silence. Many headphone
   forum readers use exclusive mode. The app must detect silence-while-playing and say why.
2. **Shared mode resamples.** Loopback delivers the device mix format, not the file format. The
   "stream facts" readout shows the device rate. The macOS app has the same property with its tap.
3. **System volume and enhancements.** Check whether the loopback point is before or after the
   endpoint volume and audio enhancements (APO) on real hardware. The SPL estimate depends on it.

What step 8 built (2026-10-08), and why:

- **Device loopback is the capture.** It gives the mix in the format of the mix, with no
  resampling by us. Process loopback makes Windows convert the sound to a format the app names,
  and it needs Windows 10 build 20348. It stays a later option (to leave out our own test tones).
- **The stream is polled** every 5 ms, with a 200 ms buffer. Loopback sends packets only while
  something plays, so an event handle brings nothing here.
- **Exclusive mode is found, not guessed.** Windows refuses a shared stream while a player holds
  the device (`AUDCLNT_E_DEVICE_IN_USE`). The capture then waits and reports `ExclusiveMode`
  with a sentence for the listener. The start does not fail.
- **Silence while a program plays** (3 s) is a second state: a player on ASIO, or paused.
- **The default device is followed.** The capture thread looks once per second. A stream that
  ends is opened again, also every second.
- **Surround is folded down** to stereo (ITU-R BS.775: center and surrounds 3 dB down, and the
  bass channel 3 dB down to both sides). Many gaming headsets show Windows a 7.1 device. The
  analyzer then sees the speech in the center too. The sum is not scaled down: a loud surround
  mix can read over full scale.
- **The Windows volume is taken out of the samples** (2026-10-08). The first run on a real PC
  read −31 LUFS for music that is mastered near −14: loopback is after the volume control. macOS
  taps before it. So the capture reads the volume of the device (every 50 ms) and divides the
  samples by it. The numbers then describe what the player sent, at every slider position, as on
  macOS. Only for a device whose volume Windows applies in software (`QueryHardwareSupport`); a
  device with a hardware volume is captured before it. At most 60 dB. Mute is not undone. The
  setting `compensateSystemVolume` and `capture --raw` switch it off.
- **Still open, needs a PC:** a check that the compensated numbers do not move with the slider;
  enhancements (APO); what the "device format" property says about bit depth (it read 32).

## D5. Crates

Added one at a time, each with a license check, when the code that needs it lands.

In use: `realfft` 3.5 with `rustfft` 6.4 (MIT or Apache-2.0), since port step 2.
`serde_json` 1 (MIT or Apache-2.0), in the probe, since port step 6.
`serde` 1 with `derive` (MIT or Apache-2.0), in the headphones crate, since port step 7.
In the render crate, since port step 9: `wgpu` 30 (MIT or Apache-2.0), `bytemuck` 1 (Zlib, MIT or Apache-2.0),
`ab_glyph` 0.2 (Apache-2.0), `tiny-skia` 0.12 (BSD-3-Clause), `png` 0.18 and `pollster` 1 (MIT or Apache-2.0).
`windows` 0.62 (Microsoft, MIT or Apache-2.0), in the capture crate on Windows only, since port step 8.
`winit` 0.30 (Apache-2.0) and `muda` 0.21 (MIT or Apache-2.0, native menus), in the app crate, since port step 11.
`tray-icon` 0.26 (MIT or Apache-2.0), in the app crate, since port step 12.
`sha2` 0.11 (MIT or Apache-2.0), in the app crate, for the checksum of an update.
`rfd` 0.17 (MIT, the file dialog and the message box of the system), in the app crate, for the import of a curve.

Planned:

| Need | Candidate |
|---|---|
| FFT (replaces Accelerate / vDSP) | `realfft` / `rustfft` |
| GPU | `wgpu` |
| Window and input | `winit` |
| Settings and control UI | `egui` with `egui-wgpu` |
| Text in panels | `glyphon` |
| WASAPI, tray, media session | `windows` (Microsoft) |
| Tray icon | `tray-icon` |
| Probe JSON, settings | `serde`, `serde_json` |

## D6. Parity is tested, not claimed

Each analyzer has a golden file: the output of the macOS app's own code on fixed signals. A test
feeds the same signals to the Rust code and compares the numbers within a stated tolerance. A
feature has parity when its test passes.

(The first plan was a probe on each side that reads the same WAV file. The macOS probe has no file
mode, so the golden files do this job.)

The golden files are in `crates/joseon-core/tests/golden/` and `crates/joseon-headphones/tests/golden/`.
`tools/golden` makes it: a small Swift program that links the macOS app's own source and runs it
on fixed signals. The Rust test feeds the same signals and compares. Stated tolerance for the
spectrum: 0.01 dB per display bin, 0.005 dB per band level. Measured on macOS arm64: 0.0002 dB.
Stated tolerance for loudness: 0.001 dB. Measured on macOS arm64: 0, every value equal.
Stated tolerance for stereo: 1e-4 on a correlation, balance or width. Measured: 0, every value equal.
Stated tolerance for the session record: 0.01 dB in a sample. Measured: 0.0012 dB. Events and counts are equal.
Stated tolerance for the headphone model: 0.005 dB in a curve. Measured: 0.0002 dB. Flags and texts are equal.
Stated tolerance for the level at the ear: 0.01 dB in a level, 0.3 % in a dose or a time. Measured: 0.0024 dB and 0.00002 %.
Day and week stamps: equal on 1200 dates in four time zones.
Stated tolerance for the headphone measurement: 0.002 dB in a curve, 1e-6 in an `f64` result. Measured: 0 and 4e-15. Sentences are equal.
A Windows run can differ more, because the FFT and the math library take other code paths there.
If it does, the tolerance changes here first, with the measured number.

Measured on Windows (GitHub Actions runner, x64, 2026-10-08). The parity tests of the spectrum, loudness,
stereo, engine, headphone model, level at the ear, synthetic frames, spectrogram and vectorscope pass
with the macOS tolerances. Two tolerances changed:

- Measurement stimulus: a sweep sample differs by 3e-12 (1e-11 of its value). Tolerance 1e-9 (was 1e-12).
- Colors and lookup tables: a channel differs by 1.1e-5. Tolerance 3e-5 (was 1e-6). One 8-bit step is 4e-3.

Text widths depend on the font of the system. Where a layout comes from a text width (the bar of the
ear strip in the meters), the test compares it with macOS only on macOS.

## Open

- **Menu bar mini spectrum.** Decided: both. A tray icon with a tiny spectrum, and a small
  always-on-top strip. The tray menu shows and hides the strip.
- **Now playing.** Windows has a real API (System Media Transport Controls). This should be better
  than the macOS Accessibility reader. Whether Qobuz for Windows publishes to it is untested.
- **Installer and signing.** First a single `Joseon.exe` in a zip: no install, just run. An `.msi`
  comes for the wider release. An unsigned build shows a SmartScreen warning. A code-signing
  certificate removes it and costs money. Not decided.
- **Repository.** Public on GitHub since the updater needs a release list that every PC can read.
  No private paths, no names of people, no secrets.
