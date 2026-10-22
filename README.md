# JoseonPC

Real-time audio analyzer for Windows. JoseonPC is the Windows version of
[Joseon](https://github.com/jasas1/Joseon), written in Rust. It reads the audio that the PC plays
and shows what is in the music and what the headphones must deliver.

**Status: an early test build that runs.** The window shows all five panels from the sound the PC
plays: spectrum, spectrogram, vectorscope, loudness and peak, session timeline. It has menus, a tray
icon with a mini spectrum, the name of the song, the headphone picker with stress flags, and updates
from inside the program. Every part of the analysis is tested against the macOS app: the spectrum
agrees within 0.0002 dB, the loudness and stereo numbers are equal, and the panels look like the
macOS ones. Not in it yet: the level at the ear, the A/B comparison, the headphone measurement with
a microphone, an installer. See [docs/PORT-PLAN.md](docs/PORT-PLAN.md) for the order of work.

## Download

Get the newest zip from [Releases](https://github.com/jasas1/JoseonPC/releases), unpack it, play
some music, and start `Joseon.exe`. Nothing is installed. The file is not signed yet: Windows shows
"Windows protected your PC" at the first start. Click "More info", then "Run anyway". Later versions
come through "Check for Updates…" in the settings menu.

Goal: the same features as Joseon on macOS.

- High-resolution log spectrum, peak hold, long-term average
- Scrolling spectrogram, also as a 3-D waterfall: drag to turn it, scroll to zoom, Ctrl+6 for the whole window
- Loudness: LUFS momentary / short-term / integrated, loudness range, true peak, PLR, clip count
- Stereo field: vectorscope, phase correlation, balance and correlation per band
- Headphone overlay: measured response (AutoEq data) and predicted at-ear spectrum, stress flags
- Level at the ear (SPL estimate), calibration, headphone measurement with a microphone
- Session timeline and summary, comparison snapshots, now playing
- Tray mini spectrum
- Balanced animation by default, with High Refresh (every refresh of the display) in Settings → Display

Capture uses WASAPI loopback. No driver. Audio stays on the PC and is never recorded.

## Build

Install Rust from <https://rustup.rs>, then:

```bash
cargo test --workspace --release
```

Analyze a WAV file and print the measurement as JSON:

```bash
cargo run --release -p joseon-probe -- analyze FILE.wav
```

On Windows, list the output devices and analyze the sound the PC plays:

```bash
cargo run --release -p joseon-probe -- devices
```

```bash
cargo run --release -p joseon-probe -- capture --seconds 10
```

The first test on a PC is in [docs/WINDOWS-TEST.md](docs/WINDOWS-TEST.md).

## Layout

Same layers as the macOS app. A crate depends only on crates in a lower layer.

| Crate | Layer | Job |
|---|---|---|
| `joseon-core` | L0 | Contracts (`contracts.rs`), ring buffer, DSP analyzers, analysis engine |
| `joseon-capture` | L1 | WASAPI loopback to ring buffer, measurement input, now playing |
| `joseon-headphones` | L1 | AutoEq curve parse, library, headphone model, SPL estimate |
| `joseon-render` | L2 | wgpu panels, tray mini renderer, offscreen PNG renderer |
| `joseon-app` | L3 | Windows, tray icon, settings |
| `joseon-probe` | tool | Headless capture + analysis to JSON, offscreen panel renders |

Technical choices and the reasons for them: [docs/DECISIONS.md](docs/DECISIONS.md).

## License

JoseonPC is free software under the [GNU General Public License v3](LICENSE). Third-party data and
trademark notes: see [NOTICE](NOTICE). The name "Joseon" and the app icon are not covered by the GPL.
