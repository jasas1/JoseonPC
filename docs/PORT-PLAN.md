# Port plan

Source: the macOS repo, `Sources/` (about 69,000 lines of Swift, 16,000 lines of tests).
Order: lowest layer first. Each step ends with a test that passes on Windows and on macOS, unless
the step is marked Windows-only.

| Step | Work | Swift source | Needs a PC |
|---|---|---|---|
| 1 | Contracts, stereo ring buffer, test signals | `JoseonCore/Contracts.swift`, `StereoRingBuffer.swift`, `TestSignals.swift` | no |
| 2 | Spectrum: multi-resolution FFT, third-octave bands, peaks | `JoseonCore/Spectrum` | no |
| 3 | Loudness: K-weighting, LUFS, range, true peak, peak hold | `JoseonCore/Loudness` | no |
| 4 | Stereo: band split, correlation, balance | `JoseonCore/Stereo` | no |
| 5 | Analysis engine, session recorder | `JoseonCore/Engine`, `Session` | no |
| 6 | Probe: WAV in, JSON out. Parity test against the macOS probe | `JoseonProbe` | no |
| 7 | Headphones: AutoEq parse, model, stress flags, SPL estimate | `JoseonHeadphones` | no |
| 8 | Capture: WASAPI device loopback, then process loopback | `JoseonCapture/SystemAudioTap.swift` | **yes** |
| 9 | Render context, spectrum panel, offscreen PNG | `JoseonRender` | no |
| 10 | Spectrogram, vectorscope, meters, timeline, cursor link | `JoseonRender` | no |
| 11 | App window, panel layout, settings | `JoseonApp` | partly |
| 12 | Tray icon and mini spectrum | `StatusItemController.swift`, `MiniSpectrumRenderer.swift` | **yes** |
| 13 | Now playing (System Media Transport Controls) | `NowPlayingReader.swift` | **yes** |
| 14 | SPL calibration, tone player, comparison snapshots | `JoseonApp/SPL*`, `Comparison*` | partly |
| 15 | Headphone measurement: sweep out, microphone in | `JoseonHeadphones/Measurement`, `MeasurementInput.swift` | **yes** (the math: no) |
| 16 | Installer, start at login, signing | none | **yes** |

Steps 1 to 7 and 9 to 10 are about two thirds of the work and run on a Mac.

## Progress

| Step | State | Date | Notes |
|---|---|---|---|
| 1 | done | 2026-10-07 | `contracts.rs`, `ring_buffer.rs`, `test_signals.rs`. 32 tests. Not yet run on Windows. |
| 2 | done | 2026-10-07 | `spectrum/`: analyzer, per-resolution FFT, third-octave bank, note names. 86 tests more (118 in total). Not yet run on Windows. |
| 3 | done | 2026-10-07 | `loudness/`: K-weighting, gated loudness, range, true peak, peak hold, clip count. 40 tests more (158 in total). Not yet run on Windows. |
| 4 | done | 2026-10-08 | `stereo/`: 8-band Linkwitz-Riley filter bank, correlation, balance, width, vectorscope points. 28 tests more (186 in total). Not yet run on Windows. |
| 5 | done | 2026-10-08 | `engine/` and `session/`: the analysis engine with its tick thread, and the session recorder. 50 tests more (236 in total). Not yet run on Windows. |
| 6 | done | 2026-10-08 | `joseon-probe`: `demo` and `analyze FILE.wav`, JSON lines. 15 tests more (251 in total). Not yet run on Windows. |
| 7 | done | 2026-10-08 | `joseon-headphones`: curves, library, model, stress flags, level at the ear, dose, sensitivity library, calibration. 158 tests more (409 in total). Not yet run on Windows. The sweep measurement is step 15. |
| 15 | math done | 2026-10-08 | `joseon-headphones/src/measurement/`: sweep and pink-noise stimulus, deconvolution, window, average, distortion, signal to noise, microphone and coupler files, absolute level, derived sensitivity with its uncertainty budget, the result with its warnings and CSV. 62 tests more (471 in total). The sound output and the microphone input are not started: they need a PC. |
| 9 | done, without A/B compare | 2026-10-08 | `joseon-render`: wgpu context and WGSL shaders, frame arena, text layer, palette, axis, formats, synthetic frames, the spectrum panel, the offscreen PNG renderer. 16 tests more (487 in total). Drawn on Metal on a Mac. Not yet run on Windows. |
| 10 | done, without A/B compare | 2026-10-08 | `joseon-render`: spectrogram, vectorscope (goniometer and stereo placement by frequency), meters (with the level at the ear), session timeline, the demo session. 27 tests more (514 in total). The cursor link is in `contracts.rs` (step 9); the views that share it come with step 12. |
| 8 | part 1 done, runs on a PC | 2026-10-08 | `joseon-capture`: WASAPI loopback of the default output device, device list, the programs that play, health states. Probe: `devices`, `capture`. 16 tests more (530 in total). The code type-checks and lints for Windows. First run on a real PC on 2026-10-08: the window shows live sound from a player, with the device name, 48 kHz, and the name of the player. Open: the volume check, a device change, exclusive mode. Process loopback: later. |
| 11 | parts 1 and 2 done | 2026-10-08 | `joseon-app`: the settings file, the layout of the three presets, the window (winit) with all cards, header, banners, the shared cursor, keys, dividers that drag. `joseon --demo`, `joseon --snapshot FOLDER`. 20 tests more (550 in total). The window runs on a Mac. Not yet run on Windows. Part 2: the menus of the cards and a settings menu. Open: headphone curves, the flag strip, the level at the ear. |
| 11 | part 3 done | 2026-10-08 | `joseon-app`: the headphone picker. The library with imported curves, the model to the engine, the first-run default pair, menus for headphone and target, the stress strip under the header (`stress_strip.rs`: quiet states, chips, the "+N" chip, the detail plate, the loudness against the target), import with a file dialog (`rfd`) or a drop on the window. 11 tests more (577 in total). Runs on a Mac. Open: the level at the ear. |
| 11 | fixes after the PC run of v0.0.4 | 2026-10-08 | Report: the picture stood still after any menu was opened; the Windows volume still changed the numbers on "Headphones (High Definition Audio Device)"; "Joseon" showed as a player. The cause of the still picture is not proven (it does not happen on a Mac). Changes: a menu opens after the mouse event (`about_to_wait`), the surface is set up again after a menu, a timer draws when no picture came for 250 ms, a log file with panics (`log.rs`). One instance only (`single_instance.rs`); sessions of a process with our name are no players. The setting `compensateVolumeAlways` for devices that report a hardware volume. `joseon-probe devices` shows `hardwareVolume`. |
| 7 | 5 headphones more | 2026-10-08 | From `data/autoeq/` (AutoEq, MIT): DT 900 Pro X, U12t with three modules, Tea Pro. The macOS app has them too (branch `feat/more-headphones`): `tools/embed_autoeq.py --swift` made its blocks, and `embedded_curves.rs` is made from the Swift file as before, so the parity tests cover all 21 curves. Sensitivity: DT 900 Pro X and U12t from the makers' pages; the Tea Pro figure has no reference. |
| 16 | the updater, written, not yet run on a PC | 2026-10-08 | `joseon-app/update.rs`: the release list of the project on GitHub (`curl`), the newest release with a Windows zip, SHA-256 against the digest of the list, unpack (`tar`), the old files get `.old`, the new program starts with `--after-update` and waits for the old one. Settings menu: "Check for Updates…", "Update to … and Relaunch", "Check for Updates Automatically" (5 s after the start, then once a day). The release workflow refuses a tag that is not the version in `Cargo.toml`. 8 tests more. |
| 11 | fixes after the PC run of v0.0.8 | 2026-10-08 | PC report: the picture no longer stays still after a menu, but it stood still while a menu was open; "Freeze Display" in a card menu read as "freeze this card"; a wish for a visible button on cards with two views; `svchost` showed as a player. Changes: a Windows timer draws the windows while a menu is open (`menu_draw` in `main.rs`), "Freeze Display" only in the settings menu, a swap button on the vectorscope card, system processes are no players (`now_playing::is_player`). Confirmed on the PC with v0.0.8: headphone picker, strip, song name, several players named. |
| 13 | written, not yet run on a PC | 2026-10-08 | `joseon-capture/now_playing.rs` and `media_windows.rs`: title and artist from the media sessions of Windows, polled every 1.5 s. The header shows them beside the names of the programs. 7 tests more (560 in total). |
| 12 | written, not yet run on a PC | 2026-10-08 | `joseon-render/mini_spectrum.rs` (parity: 7 600 values), `joseon-app/mini.rs`: the tray icon with a live mini spectrum and a menu, and the mini strip (a small window over the others). The close button hides the window to the tray. 12 tests more (566 in total). Runs on a Mac. |
| 10 | the waterfall | 2026-10-13 | `joseon-render/spectrogram/waterfall.rs` and two WGSL files: the spectrogram as a 3-D field, a second mode of the card (the swap button) and the "Waterfall" layout on Ctrl+6 with the timeline squashed under it. A drag turns the view, the wheel or a pinch zooms, a double click or the reset button goes back. Ported from `SpectrogramRenderer+Waterfall.swift` of the macOS app (same camera, same labels, same cursor). 9 tests more (597 in total). Drawn on Metal on a Mac; not yet run on Windows. |

Step 1 notes:

- The ring buffer is two handles, a writer and a reader. Samples are stored as atomic bit patterns,
  so the lock-free read has no data race in Rust. The Swift lock fallback is not ported: it exists
  only for macOS 14.
- The noise signals match the macOS app bit for bit. The sine and the demo signal match bit for bit
  on macOS, and within 1e-6 elsewhere, because `sin` and `exp` come from the system math library.
- `ContractFieldTests.swift` is not ported yet. It tests the stereo analyzer and the loudness meter,
  so it moves with steps 3 and 4.
- The two Swift write-speed tests are not ported. They measure and assert nothing.
- `PlaybackCalibration` and `HeadphoneSensitivity` are `Codable` in Swift. The Rust types get `serde`
  when the settings file or the probe JSON needs it (step 6).

Step 2 notes:

- **Parity.** `tests/spectrum_parity.rs` feeds three fixed signals to the Rust analyzer and compares
  every output with what the Swift analyzer gives (`tests/golden/spectrum.txt`, made by
  `tools/golden`). On macOS arm64 the largest difference in any display bin is 0.0002 dB. Peaks,
  note names, band levels and third-octave levels agree. The test allows 0.01 dB per display bin and
  0.005 dB per band level.
- **Speed.** On the demo signal `process` takes 100 us per 800-frame block and `read` takes 14 us
  (Apple M-series, release). The Swift analyzer on the same machine: 124 us and 9.5 us. The budgets
  of the macOS app are 130 us and 30 us.
- **No allocation.** `process` allocates nothing once it is warm. A counting allocator checks it.
- **Four loops are not a line-for-line port.** The tone gate, the two sliding sums and the display
  smoothing do the same arithmetic in the same order, without a branch per bin. A direct
  translation took 149 us per block.
- **FFT.** `realfft` returns the mathematical DFT. vDSP returns twice that. The scale constant
  carries the factor.
- **Contract change.** `top_peaks`, `lowest_strong_hz` and `third_octave` take `&mut self`: the
  list of peaks is built when it is first read.
- **Not ported.** `TopPeaksTests.testTheEngineCopiesThemIntoTheFrame` moves with the analysis engine
  (step 5). The XCTest `measure` test has no assertion.
- Every ported test passed without a change to a threshold.
- A debug build runs the suite in about 75 s. A release build runs it in 3 s.

Step 3 notes:

- **Parity.** `tests/loudness_parity.rs` compares 273 values on three fixed signals with the Swift
  meter (`tests/golden/loudness.txt`): a 40 s demo signal, a stepped tone with a hard-clipped
  stretch and a reset, and a device switch from 48 kHz to 96 kHz to 192 kHz. On macOS arm64 every
  value is equal, bit for bit. The test allows 0.001 dB.
- **Why exact here and not in step 2.** The meter is biquads, sums and a 16-tap filter, added in a
  fixed order. Only `log10`, `tan` and `pow` come from the system math library. The spectrum goes
  through an FFT library that is not the one the macOS app uses.
- **Speed.** `process` takes about 29 us per 800-frame block (Swift on the same machine: about
  30 us). The budget of the macOS app is 500 us. It allocates nothing, from the first call on.
- **Layout.** One `Channel` struct holds what the Swift class keeps as pairs of left and right fields.
- `ContractFieldTests.testIntegratedIsValidOnlyAfterTheFirstGatedBlock` is ported here. The other
  test in that file waits for step 4.
- The XCTest `measure` test is not ported. It asserts nothing.

Step 4 notes:

- **Parity.** `tests/stereo_parity.rs` compares 378 values on three fixed signals with the Swift
  analyzer (`tests/golden/stereo.txt`). On macOS arm64 every value is equal, and the vectorscope
  points are equal bit for bit. The test allows 1e-4 on a correlation, balance or width.
- **Speed.** `process` takes about 66 us per 800-frame block (Swift on the same machine: about
  62 us). The budget of the macOS app is 500 us. It allocates nothing once it is warm.
- **Filter bank API.** Swift has one call per band and side, and a rule to ask for band 0 first.
  Rust has `run(left, right)`, then `left(band)` and `right(band)`. Both sides go through each
  section in one sample loop, which is where the speed comes from.
- `ContractFieldTests.swift` is now ported in full.

Step 5 notes:

- **Parity.** `tests/engine_parity.rs` runs two fixed signals through the whole engine, one
  `process_now` per block, and compares the session record with the Swift engine
  (`tests/golden/engine.txt`): 436 values. Counts, event ids, kinds, times and labels are equal,
  and so is the block at which the engine goes to sleep. The largest difference in a sample is
  0.0012 dB, in a band level that comes from the spectrum. The test allows 0.01 dB.
- **Speed.** `ingest` takes 0.06 us (Swift: 0.18 us; budget 2 us). A snapshot of a full 30-minute
  record takes 18 us (Swift: 50 us; budget 200 us).
- **The reader moves.** Swift shares one ring buffer object. Here the engine owns the reading end
  while it runs. `stop` gives it back, and `start` returns the reader it replaced.
- **The tick is a thread** that waits on a condition variable, not a dispatch timer. `stop` waits
  for a tick that runs now to end.
- **Models are shared handles.** A headphone model or an SPL estimator is an
  `Arc<Mutex<dyn ...>>`: the app sets it from one thread, the engine uses it on another.
- **`latest_frame` gives an `Arc<AnalysisFrame>`.** Swift arrays are copy-on-write, so a frame copy
  is cheap there. In Rust the shared pointer does that job.
- **Event text is shared** (`Arc<str>`), so a snapshot of 2000 events copies no strings.
- **`host_time` starts at 1**, not at the system uptime. 0 still means "no frame yet".
- One test waits for its condition and not for a fixed 0.15 s: `stalled_input_goes_silent_and_falls`.
  A debug build on a busy machine needed longer.
- Every Swift test of step 1 to 5 is now ported, except the XCTest `measure` tests.

Step 6 notes:

- **The plan was wrong about the macOS probe.** It has no file mode: it has `demo`, live capture,
  device lists and renders. So the parity tests stay with the golden files of `tools/golden`.
- **`demo`** is the macOS `demo` mode: the same `streamInfo`, `reading` and `summary` lines, without
  the fields that describe Core Audio devices and taps. Run side by side on this Mac, the two
  probes print the same frame counts, peaks, loudness and correlation.
- **`analyze FILE.wav`** is new. It runs a file through the engine as fast as it goes and prints
  one summary line. It gives a Windows tester a check that needs no capture. The WAV reader is
  in the probe crate and has no dependency.
- **Dependency added:** `serde_json` (MIT or Apache-2.0), in the probe only.
- `capture`, `devices` and `cycle` arrive with step 8. `render` arrives with step 9.

Step 7 notes:

- **Parity of the model.** `tests/model_parity.rs` runs five scenarios through the engine with a
  headphone model: 80 000 values. Curves agree within 0.0002 dB. Every flag id, severity, span,
  onset and text is equal, character for character.
- **Parity of the level at the ear.** `tests/spl_parity.rs` compares three groups with the Swift
  code (`tests/golden/spl.txt`). The static numbers (A-weighting, diffuse-field term, band means
  of all 16 curves, sensitivity library, calibration, NIOSH times: 621 values) are equal, bit for
  bit. Four scenarios through the engine (948 values): levels within 0.0024 dB, dose and time
  within 2.2e-7 as a part of the value. The test allows 0.01 dB and 0.3 %.
- **Calendar.** The macOS app reads the day and the ISO week from `Calendar`. The port does the
  date math by hand in `dose.rs`, with no date crate. 1200 dates over 60 years in four time zones
  give the same day stamp and week stamp as macOS.
- **The clock gives a UTC offset.** `LocalTime` is seconds since 1970 plus the local offset. The
  default clock is UTC. The app must give a clock with the offset of the PC (step 14), or the
  listening day changes at midnight UTC.
- **No lock inside the estimator.** The Swift class locks itself. Here it is a plain value behind
  the `Arc<Mutex<...>>` that the engine takes. `configure(curve:sensitivity:calibration:options:)`
  became four setters; a headphone change under one lock is still one change.
- **Calibration factories are free functions** in `calibration.rs`. `PlaybackCalibration` belongs
  to `joseon-core`, so this crate can not add methods to it.
- **The sensitivity texts are copied by a script** from the Swift file, not typed. Entries are
  functions (`sensitivity::hd650()`), because a `String` can not be a constant.
- **Dose JSON.** `SplDoseState` and `SplDoseOptions` have `serde`, with the field names of the
  macOS app. `PlaybackCalibration` and `HeadphoneSensitivity` still have none: they get it with
  the settings file (step 11).
- **Speed.** `evaluate` takes about 0.5 us (budget of the macOS app: 50 us).
- **Dependency added:** `serde` with `derive` (MIT or Apache-2.0).
- `tools/embed_curves.py` makes `embedded_curves.rs` from the macOS data file.
- Every Swift test of `JoseonHeadphonesTests` outside `Measurement` is ported.

Step 15 notes (the math):

- **Parity.** `tests/measurement_parity.rs` runs the Swift code and the Rust code on the same
  fixed recordings (`tests/golden/measurement.txt`): three stimuli, each part of the analysis,
  four full sessions, an average of two sides, the leak test. 5850 values. Every `f32` result is
  equal, bit for bit. The `f64` values agree within 4e-15 as a part of the value. Every warning,
  method sentence, uncertainty headline and CSV header line is equal, character for character.
- **Why this is exact with another FFT library.** The measurement works in `f64` and stores its
  results as `f32`. The difference between vDSP and `rustfft` is far under one `f32` step.
- **Speed.** One 5 s sweep at 48 kHz, with the noise floor: about 34 ms in release (budget of the
  macOS app: 150 ms).
- **Arguments with defaults became option structs**: `SweepOptions`, `PinkNoiseOptions`,
  `Derivation`. `Default::default()` gives the values of the macOS app.
- **`AbsoluteLevel` is the result type.** Swift has a namespace `AbsoluteLevel` with a `Result`
  inside. Here `AbsoluteLevel::for_reference` gives `Available(Scale)` or `Unavailable { reason }`.
- **Numbers in sentences** are printed with `fixed()`, which gives what `String(format: "%.1f")`
  gives, also for a value that is not a number.
- **One test is replaced.** `testFurtherRunsAreCheaperThanTheFirst` compares two times, and can
  fail on a busy machine. `further_runs_use_the_same_deconvolver` checks the same design in a way
  that cannot.
- **Dependency:** `realfft` is now also a dependency of the headphones crate (already in use in
  the core crate).
- Not ported: `MeasurementInput.swift` and the measure window of the app. They are platform code.

Step 9 notes:

- **Pictures against the macOS app.** `tools/golden ... pictures <dir>` draws the macOS panels on the
  same synthetic frames. Seven spectrum pictures (plain, headphone band, SPL layer, pinned cursor,
  hover label, compact, high contrast): the mean difference is about 1 of 255 per channel, and
  about 1 % of the pixels differ by more than 24. What differs is the text (another rasterizer).
  The pictures are in `review-pictures/` (local, not in git).
- **Parity of the parts with no GPU.** `tests/pure_parity.rs` (colors, lookup tables, axis,
  resampling, formats: 13 500 values, colors bit-equal) and `tests/synthetic_parity.rs` (the demo
  frames: 26 400 values within 0.001 dB).
- **Shaders.** The Metal shaders are in WGSL, one file per family in `src/shaders/`. Only shapes,
  curves and the text overlay are there. Spectrogram, scope and pan shaders come with step 10.
- **Per-draw data.** Metal sets bytes per draw. Here a draw gets a slice of an arena buffer with a
  dynamic offset. A curve can have 16 384 values at most (the panel uses 2047 at most).
- **Text.** `ab_glyph` reads the system fonts: Segoe UI Variable and Cascadia Mono on Windows 11,
  Segoe UI and Consolas before that, San Francisco on a Mac. No font is in the repository. Digits
  are set on one advance. `tiny-skia` paints boxes, lines and arcs. Text on Windows is NOT yet
  seen by anyone: check it on the first PC run.
- **Classes became a base and a trait.** `PanelBase` holds what every panel has. `Panel` is the
  set of hooks. `PanelRenderer` drives both.
- **Speed on this Mac.** One spectrum frame: 0.16 ms of CPU to encode, 0.14 ms for a full text repaint.
- **Not ported yet:** the A/B comparison of the spectrum (reference trace, difference lane). It
  goes with the comparison snapshots of step 14. The Swift render tests are not ported one by one
  yet: this step has its own tests and the picture comparison.
- A test that needs the GPU or a system font passes with a note when the machine has none.

Step 10 notes:

- **Parity.** Each panel has a test against values from the macOS code (`tests/golden/`):
  spectrogram (time aligner and history), vectorscope (23 000 values: pan field, layout, readouts,
  splats), meters (700 values at eight sizes), timeline (2 800 values: the demo session is equal to
  the last bit, the model, the panel at seven sizes). All texts are equal.
- **Pictures.** `tools/compare_pictures.py` compares each Rust picture with the macOS picture of
  the same frames. 1 to 3 % of the pixels differ. All of them are text: the two text engines place
  glyphs a part of a pixel apart.
- **Text size.** A variable font gets its optical size from the point size, in four steps from 17
  to 28, as macOS does. Large numerals then have the width of the macOS ones. Segoe UI Variable on
  Windows has the same axis: to check with the Windows text (step 12).
- **Not ported:** the A/B comparison (the table of the meters, the traces of the spectrum). It
  comes with the session features (step 14).
- **Time zone.** The timeline takes the offset from UTC as a number of seconds. The app gives the
  local offset (step 12). The default is UTC.
- **Rectangles with a negative size.** Core Graphics moves the origin of such a rectangle. The
  Rust `Rect` does not. The panels only make them for parts that are not drawn.

Step 8 notes:

- **What runs where.** The thread, the health states, the packet conversion and the format
  reader have no system call in them: `source.rs`, `convert.rs`, `format.rs`. Their tests run on
  every system, with a made-up backend. The WASAPI calls are in `wasapi.rs` (300 lines).
- **Not a translation.** The macOS tap builds a process tap and an aggregate device. Windows
  needs neither. What carries over is the contract and the behavior: follow the device, name the
  programs that play, say why there is silence.
- **macOS "permission denied" has no Windows form.** Windows asks no permission. Its place is
  taken by `ExclusiveMode` and `SilentWhilePlaying`.
- **The PC test** is in `docs/WINDOWS-TEST.md`. The Windows build of the probe is a file of every
  CI run (`joseon-probe-windows`).
- **Not done:** process loopback, a chosen device other than the default one, the output volume,
  the measurement input.

Step 11 notes (part 1):

- **One window, one surface.** Every panel draws into its own texture. A chrome layer as large
  as the window draws the card backgrounds, puts the panel pictures into them (`blit.wgsl`), and
  draws titles and the header on top. The chrome is a `Panel` too, so it uses the same shapes and
  text code. macOS has one view per panel instead.
- **`scene.rs` needs no window.** It draws into any texture and takes pointer and key input as
  plain calls. The tests and `joseon --snapshot` use that. `main.rs` only maps winit events.
- **Settings** are one JSON file: `%APPDATA%\Joseon\settings.json`. Keys and defaults are those
  of the macOS app. A bad value falls back to its default. The window size, the split shares and
  the strip height are in the same file (macOS keeps those itself).
- **Layout** numbers (shares, minimums, strip rule, placement rule) are those of the macOS
  window. Dividers drag with the pointer; a drag stops at the minimums.
- **Keys:** Space freeze, R reset, F or F11 full screen, Esc clears the cursor, Ctrl+1/2/3 layout,
  Ctrl+4 stereo placement, Ctrl+5 timeline, left/right move the cursor (Shift: one octave), Return
  pins it.
- **No capture, no stop.** When the capture does not start, a banner says why and the demo
  signal runs, as on macOS.
- **Menus (part 2).** `menu.rs` holds every menu as plain data and what each item does; the tests
  need no window. Each card has a "..." button (and a right click) with the items of the macOS
  card menu. The header has a settings button: its menu holds every setting, so Windows needs no
  settings window yet (all settings are choices or switches; the two macOS sliders became four
  choices each). `main.rs` shows the data as a native menu (`muda` 0.21, MIT or Apache-2.0).
  Not yet clicked on a real PC.
- **First runs on a real PC (2026-10-08, two headphones):** capture from a player and from a game
  at once, both named in the header; a device change is followed; the picture is smooth.
- **Not done yet:** the headphone model (curves, flag strip, level at the ear), the loudness target in the strip,
  up/down keys in the spectrogram, pausing hidden panels, the "nothing changed, draw nothing"
  saving, high contrast from the system.
- **Crates added:** `winit` 0.30 (Apache-2.0), and `wgpu`, `serde`, `serde_json`, `bytemuck` in the
  app crate.

Step 13 notes:

- **Not a translation.** macOS has no list of players, so the Mac app reads the title of the
  player window through Accessibility. Windows has the list (System Media Transport Controls).
  Every player in the volume flyout is in it: the music apps, and browsers.
- **Which song.** A session that plays comes before a paused one. Among those that play, the one
  whose program the capture hears comes first. A paused player shows nothing.
- **Polled**, every 1.5 s, on its own thread. The reader runs only while the setting is on and a
  real capture runs.
- **Not done:** the "Hi-Res" mark (Windows does not tell it), the scrolling text of the menu bar
  (it belongs to the tray, step 12).

Step 12 notes:

- **Two places, as decided.** A tray icon is 16 points wide: its picture (16 columns) says that
  sound plays and roughly where the energy is. The mini strip (240 x 52 points, always on top,
  no frame) is where the shape reads; it also shows the song. The Mac has one place, the menu bar.
- **The mini spectrum** is a port (`MiniSpectrumRenderer.swift`): the same column model, ballistics
  and adaptive window, equal to the Mac values. The Mac draws a template image that the menu bar
  tints. Here the color is a setting: white, orange or accent.
- **Tray.** Left click: the main window. Right click: Open, Mini Spectrum Strip, color, song
  name, Reset, Freeze, Quit. The tooltip is the song. The icon picture goes to Windows at most 10
  times per second, and only when it changed.
- **The close button hides the window.** The program lives on in the tray, as the Mac app lives
  on in the menu bar. "Quit Joseon" in the tray menu ends it. With `--demo` there is no tray and
  the close button ends the program.
- **While the window is hidden** the panels take no frames (no GPU work); the engine runs on for
  the tray icon and the strip.
- **The strip** drags with the mouse; its place is a setting. A right click opens the tray menu.
  It is off until the user switches it on.
- **Not done:** the Mac popover (a small panel with a compact spectrum and numbers under the
  menu bar icon), a lower engine rate while only the tray shows, the color of a light taskbar.
- **Crate added:** `tray-icon` 0.26 (MIT or Apache-2.0).

First runs on Windows (GitHub Actions, 2026-10-08):

- The workspace builds with MSVC. The tests ran on a shared runner with no graphics card (the
  software Direct3D 12 device).
- `joseon-core`: all 221 unit tests and the parity tests of the spectrum, loudness, stereo and
  engine pass with the macOS tolerances.
- `joseon-headphones`: the model, the level at the ear and the dose pass. Two tests failed: a test
  made a folder name that Windows refuses, and a sweep sample differs by 3e-12.
- `joseon-render`: the panels draw on Direct3D 12. The spectrogram, vectorscope and synthetic
  frames pass. Two tests failed: colors differ by 1e-5, and a layout that comes from a text width
  differs by 5 pt (another font).
- All four are fixed. The measured numbers are in `DECISIONS.md`.
- An early run failed one wall-clock test of the engine tick thread (14 frames in 0.5 s at 100 ticks
  per second, on a loaded runner). The test now takes `JOSEON_TIMING_SLACK`.
- To check on a real PC (step 8): the tick rate of the engine, and the speed of the panels on a
  real graphics card. The software device needs seconds for a picture.

## Rules

- A crate imports only crates in a lower layer. Types cross a boundary only through `contracts.rs`.
- Keep field names and units the same as the Swift contracts.
- Port the Swift tests with the code they test. Do not port code without its tests.
- Timing tests run in release mode only, as in the macOS repo.
- No text or asset comes from another product.
