# JoseonPC review

Reviewed 2026-10-15 against `41725038f84c8792d5ad47ffee5bb64a88e7db4c` (v0.0.11, “Quit Joseon ends the program again”). No product code was changed for this review.

JoseonPC is a local Windows desktop program. It reads the shared output mix with WASAPI loopback, analyzes it on a second thread, and draws with wgpu. Nothing in the running program opens a socket of its own. The only network path is the updater, which shells out to `curl`. Audio is not written to disk.

This pass is a reading of the source, the CI and release workflows, and the locked dependency tree, plus two release-mode timing runs of the analyzers on the review machine. It is not a run of the window on Windows, and it has no GPU frame time.

## How this was checked

| Check | Result |
|---|---|
| `cargo test --release -p joseon-core --test performance -- --test-threads=1` | 11 passed in 4.96 s. Numbers below. |
| `cargo run --release -p joseon-core --example spectrum_bench -- 5` | process 128.0 µs, read 21.7 µs, mean of 3600 blocks. |
| `cargo clippy -p joseon-core -p joseon-headphones -p joseon-capture -p joseon-render -p joseon-probe --all-targets -- -D warnings` | Clean on Linux (dev profile). |
| `cargo clippy -p joseon-app -p joseon-capture --all-targets --target x86_64-pc-windows-msvc -- -D warnings` | Clean. This compiles `wasapi.rs`, `media_windows.rs`, and `joseon-app`. |
| `cargo clippy --workspace --all-targets -- -D warnings` on Linux | Does not finish. `tray-icon` 0.26 refuses to build on Linux unless `libappindicator` or `ksni` is enabled. CI does not hit this: it runs on `windows-latest`. |
| RustSec advisory-db, cloned 2026-10-15, matched to `Cargo.lock` | No vulnerability whose patched range still includes a locked version. Two unmaintained warnings, below. This was a version-range match, not the `cargo audit` binary, so yanked-crate status was not checked. |
| GitHub Releases API for `jasas1/JoseonPC` | v0.0.8 through v0.0.11 each have a `sha256:` digest on the Windows zip. |
| Sibling repo | Public README of [Joseon](https://github.com/jasas1/Joseon) only, for the updater contrast. |

Timing host: Linux x86_64, 4 vCPU, rustc 1.99.1 (`58cf8f9c3`, 2026-10-13). The release profile in the workspace `Cargo.toml` is thin LTO and `codegen-units = 1`. These numbers are not a Windows laptop and not a capture or draw time.

`crates/*/src` is 58,083 lines, unit tests included. There are 601 `#[test]` functions.

## 1. Quality

### Structure

The workspace matches the layering in the README: `joseon-core` (L0), `joseon-capture` and `joseon-headphones` (L1), `joseon-render` (L2), `joseon-app` (L3), plus `joseon-probe`. `joseon-core` depends only on `realfft`. Windows types stay behind `#[cfg(windows)]` in the capture crate (`lib.rs`), and the capture thread is tested through a fake `Backend` on every OS (`source.rs`).

That split is the right one. The DSP, the session record, the headphone math, and the panel layout can be wrong on a machine with no sound device, and the tests do catch that. The parts that can only be wrong on a PC — the WASAPI calls, the tray, the menu timer, the updater’s real download — are exactly the parts with the least automated proof.

A few module comments are behind the code. `crates/joseon-app/src/lib.rs` still says menus, curves, the ear level, and the tray are absent. `model.rs` still says the ear level and the A/B comparison are later steps. The menus, the curve picker, and the tray are in the program. The ear level and the comparison are not wired, which the README states correctly. The crate comments should be brought back in line so the next port step does not re-do work that already landed.

### Error handling

Library paths generally return `Result` or an `Option` with a sentence (`FormatError`, `OpenError`, `CaptureHealth::advice`, the updater’s `String` errors). The analysis engine treats a poisoned mutex as recoverable (`engine/mod.rs`, `lock`), which is appropriate: a panic on the UI thread should not freeze the analyzers.

The capture thread does not. `Shared::set_health` and `publish` use `Mutex::lock().unwrap()` (`source.rs`). A poison there kills the capture thread. `start` then does `thread.join().ok()`, so a panicked capture thread drops the `RingWriter` instead of returning it. Capture cannot be restarted in that process.

Intentional panics in production code are the invariant checks around fixed FFT buffers:

- `spectrum/resolution.rs`, `expect("FFT buffer lengths are fixed at construction")`
- `loudness/true_peak.rs`, `expect("the window is TAPS long")`
- `headphones/measurement/fft.rs`, the same pattern
- `render/target.rs`, PNG encode into a memory buffer

Those fire only if a constructor and a later call disagree. They take down the thread that hits them. The analysis thread dying does not abort the process (the panic hook in `log.rs` writes `joseon.log`), but ticks stop while `AnalysisEngine::is_running` can still be true, because `running` is cleared only by `stop`.

`WasapiBackend::enumerator` expects the `Option` that `Drop` sets to `None`. That is safe for the current callers. It is a crash if a future caller uses the backend while it is dropping.

### Unsafe and FFI

Unsafe is concentrated, which is what you want. Production unsafe is:

| Site | What it does |
|---|---|
| `joseon-capture/src/wasapi.rs` | COM init, device and session enumeration, `GetBuffer` / `ReleaseBuffer`, volume, `OpenProcess` for the player name |
| `joseon-capture/src/media_windows.rs` | WinRT media-session manager, `CoInitializeEx` on that thread |
| `joseon-app/src/single_instance.rs` | `CreateMutexW`, `FindWindowW`, `ShowWindow` |
| `joseon-app/src/main.rs` | Menu timer callback with a raw `*mut App`; `muda` context menu on the HWND |
| `joseon-app/src/local_time.rs` | `GetTimeZoneInformation` |

`windows` 0.62.2 `HANDLE` is `Copy` and does not close on drop (`Free` runs only for `Owned<HANDLE>`). `CreateMutexW` returns `Result<HANDLE>`. The leaked handle in `claim` is what keeps `Local\Joseon-one-instance` taken until the process exits. That matches the comment in `single_instance.rs`. A failed `CreateMutexW` (`mutex.is_err()`) still returns `true`, so the process continues as if it were the only one. That failure is rare. It is the wrong direction if it happens.

The menu timer is the sharpest unsafe in the app. `open_menu` holds `&mut App`, `menu_draw::start` stores that pointer in a thread-local, and `show_context_menu_for_hwnd` runs a nested message loop that calls `draw_under_menu` on the same `App`. The comment says `open_menu` does not touch the program until the menu returns. That is true of the source as it stands, and the timer is same-thread, so this is not a data race. It is still two live mutable paths to one `App`. A later edit that reads `self` while `show_menu` is on the stack becomes undefined behavior with no compiler warning.

WASAPI buffer handling is careful about the COM lifetime that is easy to get wrong: `GetMixFormat`’s `WAVEFORMATEX` is freed on both the success and the `Initialize` failure path (`wasapi.rs` around the `CoTaskMemFree` after `Initialize`). `GetBuffer` is paired with `ReleaseBuffer` on the normal path. A panic inside the packet sink skips `ReleaseBuffer`. The sink today is the closure in `pump`, which can panic on the mutex unwrap above.

`drain` builds the packet slice as `frames as usize * frame_bytes` and passes it to `from_raw_parts` with no check that the product matches a buffer `GetBuffer` actually returned. On x64 a `u32` frame count times a normal frame size does not wrap `usize`. The remaining assumption is that Windows (or the driver) told the truth about `frames`. A lying endpoint is a local audio-driver problem, which is already a kernel-level attacker. Still worth a saturating check so a bad count cannot become a Rust slice.

`device_format` reads a `PROPVARIANT` blob only after `vt == 65` (`VT_BLOB`) and rejects a null pointer or a size under 16. The union layout it uses is the one `windows` 0.62.2 generates for `PROPVARIANT`. There is no upper bound on `cbSize`. Same trust boundary as the packet slice.

`CoInitializeEx(COINIT_MULTITHREADED)` is called on the capture thread and, separately, on the now-playing thread. Both objects stay on the thread that created them. `WasapiBackend`’s drop releases the enumerator before `CoUninitialize`. That order is right.

### Concurrency

Three threads do the live work, plus a fourth for the song title.

1. **Capture** (`"joseon capture"`) owns the COM objects. It polls every 5 ms, housekeeping every 1 s, volume every 50 ms, retry every 1 s (`Timing::default` in `source.rs`). The WASAPI buffer is 200 ms (`BUFFER_DURATION` in `wasapi.rs`).
2. **Analysis** (`"joseon.analysis"`) wakes at 60 Hz (`ENGINE_RATE` in `model.rs`). `tick_loop` holds the `Core` mutex for the whole tick.
3. **UI / render** reads `latest_frame()`, which clones an `Arc` under the short `State` lock. It does not take `Core` except when settings are applied (`spectrum_settings()`).
4. **Now playing** polls WinRT on its own thread every 1500 ms (`NowPlayingReader`’s default `interval` in `now_playing.rs`).

The ring is the piece that is actually specified. `ring_buffer.rs` is single-writer, single-reader, wait-free on the writer: one atomic exchange, a sample copy, three atomic stores, no lock. Samples are `AtomicU32` bit patterns so a torn read is not a data race. The reader copies, then an acquire fence, then checks `claim_end`, and retries up to 8 times. The comment on `reader_copy_fence` is a real memory-order argument, not a slogan. Capacity is `1 << 17` frames (about 2.7 s at 48 kHz).

The capture thread is not a WASAPI event callback and it does not join MMCSS. A stall loses loopback packets after the 200 ms buffer. It does not glitch playback, because loopback is a tap after the mix. The real-time mistakes on that thread are still worth fixing, because they are what makes the stall:

- `Converter::convert` grows its `Vec`s with `push` until the largest packet so far. The first packets, and any later packet bigger than that, allocate. `silence` uses `resize`.
- Every poll that contains a non-zero sample calls `set_health(CaptureHealth::Signal)`, which takes `state` (`pump` in `source.rs`). During music that is a mutex acquisition every 5 ms, shared with the UI’s `stream_info()` / `health()` reads.
- `publish` invokes the stream-info callback while `state` is locked. The app does not set that callback today (`supervise` polls). A callback that calls back into `stream_info` deadlocks, because `std::sync::Mutex` is not reentrant.
- Once a second, `active_sources` walks the audio sessions and calls `OpenProcess` for each one, on the capture thread.

The analysis side is in better shape. `process` of the spectrum, the loudness meter, and the stereo analyzer is tested to allocate nothing once warm (`tests/performance.rs`, and those tests passed here). `read` and `publish` do allocate: `shaped` returns a new `Vec`, and the frame is a new `Arc`. That happens on the analysis thread at the publish rate, not on the capture thread. After 4 s of digital silence the engine sleeps and stops running the analyzers (`SLEEP_AFTER_SILENT_SECONDS`).

Lock order to keep: the tick holds `Core`, then briefly takes `State`, and may lock the headphone model. `apply_settings` on the UI thread takes `Core` via `spectrum_settings()`. That waits out the current tick. It is not on the draw path.

### Tests and blind spots

The parity tests are the strongest part of the project. Golden files from the macOS sources, with tolerances written down in `docs/DECISIONS.md`, cover spectrum, loudness, stereo, the engine session record, the headphone model, SPL math, and the measurement math. Render has parity tests for the panels and a counting-allocator test that `process` does not hit the heap.

What those tests do not cover:

- The WASAPI calls. `the_capture_starts_or_says_why_not` runs only on Windows, and CI’s probe step is `continue-on-error: true` (`.github/workflows/ci.yml`). A build server with no endpoint still goes green. `docs/DECISIONS.md` still lists the volume-compensation check, APO, and the device-format bit depth as needing a PC.
- The updater’s network. The JSON parser, the prefix filter, the checksum reject, and `install_files` are unit-tested. The test that drives `download_and_install` uses a `file://` URL passed straight in, so it does not prove that `newest_release` would have rejected that URL. Live `curl` against GitHub is not in CI.
- GPU draw time, the waterfall’s 4× MSAA (`WATERFALL_SAMPLES` in `render/context.rs`), and the menu-timer path.
- `joseon-app` as a crate on the default CI host is compiled (the workflow builds `joseon.exe`), but macOS CI runs only on `workflow_dispatch`. A pull request does not compile the Mac tray path.

CI itself is in good shape for a project this size: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -- -D warnings`, then `cargo test --workspace --release --no-fail-fast`, on `windows-latest`, 60-minute timeout, `contents: read`. The workspace clippy allows (`needless_range_loop`, `manual_clamp`, and the other five in the root `Cargo.toml`) are documented as Swift-port choices, and `manual_clamp` is there because `clamp` and `min`/`max` disagree on NaN. That is a real reason. Keep the list that short.

`JOSEON_TIMING_SLACK` is `5` on the runner, so a budget of 130 µs fails CI only past 650 µs. Combined with the “fastest of five” rule in `performance.rs`, CI will not notice the spectrum path getting slower until it is several times over budget.

### Build and release hygiene

`Cargo.lock` is committed. `rust-toolchain.toml` is `stable` with rustfmt and clippy, not a date pin, so the compiler moves under the repo. The release profile (thin LTO, one codegen unit) is set.

`.github/workflows/release.yml` refuses a tag that is not `v` plus the `version` in `Cargo.toml`, builds `joseon.exe` and `joseon-probe.exe`, runs `demo` and a one-second snapshot, and only then zips and calls `gh release create` as a prerelease. That is the right order. There is no signing step, no installer, and no `cargo audit` / `cargo deny`. `packaging/` is a text readme and the release notes. The README and that text both tell the user to click through SmartScreen.

`permissions` on CI are `contents: read`. The release job is `contents: write`, which `gh release create` needs. There is no `pull_request_target`.

## 2. Performance

### What was measured

Release profile, this machine, 48 kHz, blocks of 800 frames (16.7 ms of audio, one 60 Hz tick). The performance-test figures are the fastest of five runs. The example is the mean of every block after a warmup pass.

| Path | This machine | Budget in the test |
|---|---|---|
| Spectrum `process`, pink noise, mean of the fastest run | 114.5 µs | 1000 µs |
| Spectrum `process` / `read`, demo signal, fastest run | 113.8 µs / 19.4 µs | 130 µs / 30 µs |
| Spectrum `read`, 1024 bins, fastest run | 17.8 µs | 250 µs |
| Spectrum `process` / `read`, demo signal, `spectrum_bench` mean of 3600 blocks | 128.0 µs / 21.7 µs | (the 130 µs budget is the test’s, not the example’s) |
| Loudness `process` | 27.6 µs | 500 µs |
| Stereo `process` | 74.9 µs | 500 µs |
| `SessionRecorder::ingest` | 0.092 µs; 0.170 µs mean with two snapshotting readers, worst 43.1 µs | 2 µs |
| Snapshot of 1800 samples and 2000 events | 47.0 µs | 200 µs |
| Warm `process` allocations (spectrum, with and without tones; loudness; stereo) | 0 | 0 |

Loudness, stereo, and the session record are far inside their budgets. The spectrum is not. The mean `process` on the demo signal was 128.0 µs against a 130 µs budget taken from the macOS app on Apple silicon (`docs/PORT-PLAN.md` records about 100 µs `process` and 14 µs `read` on that machine). The test can still pass by keeping the fastest of five runs (113.8 µs here) and, on CI, by multiplying the budget by five. A small regression on x64 will not go red.

Those three `process` times were not measured in one tick. Adding the fastest-run figures (113.8 + 27.6 + 74.9) is 216 µs of analyzer `process` per 800-frame block, before `read`, the headphone model, and the draw. At 48 kHz that block is 16.7 ms, so the analyzers as measured are a small fraction of one core when they all run. The spectrum number already includes whichever of the three FFTs were due in those blocks.

`process` allocated nothing across the 1000-call and 500-call checks. That part of the claim in the port notes holds on this machine.

### Capture and FFT shape

At 48 kHz the three FFT sizes are the constants in `spectrum/mod.rs`: 32768, 8192, and 2048 (`LOW_SIZE_AT_48K` and the two beside it). Hops are `size/6`, `size/8`, and `size*3/8`, so the long window updates at about 9 Hz and the short one a bit above the frame rate. Sizes scale with the sample rate and are clamped (`nearest_pow2`, low capped at `1 << 17`). A 192 kHz device builds a larger low FFT, up to that cap, and `configure` allocates the history then. That allocation is on the analysis thread, on a rate change, not on the capture thread.

The capture conversion is scalar (`Converter::sample` per channel) and the ring store is a relaxed atomic per sample, which the ring’s own comment says cannot become `memcpy`. At 48 kHz stereo that is 96,000 atomic stores per second of audio. This review did not time `RingWriter::write` or `Converter::convert` separately.

Structural sizes, from the constants, not from a heap profile:

- Ring storage: `1 << 17` frames × 2 × 4 bytes = 1,048,576 bytes.
- Spectrum history at 48 kHz: `next_pow2(32768 * 2)` = 65536 frames × 2 × 4 bytes = 524,288 bytes, plus the FFT scratch `realfft` builds at configure time.
- Engine scratch: five `Vec<f32>` of length `1 << 15` = 655,360 bytes (`SCRATCH_CAPACITY` in `engine/mod.rs`).
- Spectrogram CPU mirror: 1024 × 1200 `u16` = 2,457,600 bytes (`ROWS`, `COLUMNS`, `mirror` in `spectrogram/mod.rs`). The GPU ring is the same grid in `R16Float` and is updated a column at a time, not re-uploaded whole (the module comment).

### Render

The main window uses `PresentMode::AutoVsync` and `desired_maximum_frame_latency: 2` (`main.rs`). `RedrawRequested` asks for another frame while the window is visible, and the comment there says the surface waits for the display. This review did not measure a frame. The event loop also wakes on a 33 ms `WaitUntil` for the tray and the strip, and a Windows timer at 15 ms draws while a menu is open.

Text layout is cached (`text/overlay.rs`, cap 2048 lines), which is the right call for labels that repeat. The cache key still builds a `String` on every lookup, including hits, and the lookup takes a process-wide mutex. That cost is on the render thread.

The waterfall is the expensive GPU path: a fold into a field texture, then bars with 4× MSAA and a depth buffer (`context.rs`, `spectrogram/waterfall.rs`). No frame time was taken.

## 3. Security

Ranked for a local desktop program. Remote code execution against a user who only runs the zip is not a realistic outcome of the bugs below. The realistic outcomes are a replaced `Joseon.exe` after the user accepts an update, and local denial of service or a bad measurement.

### 1. The updater replaces the program, and the binary is unsigned

This is the issue to fix before the updater is treated as routine.

`check_updates_automatically` defaults to `true` (`settings.rs`). About 5 seconds after start, and then once a day, `run_updater` in `main.rs` calls `Updater::check` when the tray is in use. The check does not install. The user chooses “Update” (`Action::InstallUpdate` → `Updater::install`). On `Status::Ready`, `relaunch` starts the new process and the old one exits.

What that install trusts:

- The release list is `https://api.github.com/repos/jasas1/JoseonPC/releases?per_page=10`, unless `JOSEON_UPDATE_URL` is set (`update.rs`). The environment variable is a test hook. It also means any local process that can set the variable for Joseon chooses the JSON.
- Asset URLs must start with `https://github.com/jasas1/` or `https://objects.githubusercontent.com/`. Drafts are skipped. Prereleases are not. `v0.1.0` on `example.com` is rejected, and there is a test for that. The prefix `https://github.com/jasas1/` is the whole account, not this repository. `objects.githubusercontent.com` is the shared release CDN.
- `curl` is invoked with `-L` (`fetch_text`, `fetch_file`). The prefix is checked on the URL in the JSON, not on the URL after redirects.
- SHA-256 is checked only when the asset JSON has a `digest` of the form `sha256:…` (`download_and_install`). A release with no digest is installed anyway. The four live releases (v0.0.8 through v0.0.11) do have digests, so a current update does check. The code will not fail closed if a future response omits `digest`.
- The zip is unpacked with `tar -xf` and every file is copied into the program folder (`install_files`). `install_files` uses `file_name()`, so the copy step does not honor `..` in a single path component. The `tar` extraction itself is what would have to contain a path escape, and this review did not run Windows `tar` against a hostile archive.
- There is no Authenticode check. The macOS README says a new version is installed only when macOS has verified the signature and the signing team matches. This port checks a GitHub-supplied digest, when present, and then runs the new exe.

A stolen `jasas1` GitHub account, or a `JOSEON_UPDATE_URL` pointed at a JSON file whose asset URL is on the allowed prefix, ends with the user clicking one menu item and the program on disk being replaced. The digest does not help against the account: the attacker publishes the digest too. SmartScreen may prompt again because the new file is also unsigned. That prompt is the only signature-shaped control, and the readme already teaches the user to click through it.

`tool()` runs `curl` and `tar` by bare name. Pinning both to `%SystemRoot%\System32\curl.exe` and `tar.exe` removes the chance that a `curl.exe` copied in by a previous install (the zip is copied in full) or found earlier on `PATH` is the program that fetches the next one. This review did not execute that search on Windows. An absolute path is the fix either way.

`Version::parse` requires three numeric parts, so a tag like `nightly` is ignored. Good. There is still no separate stable channel: a higher prerelease tag is offered. That matches a project whose releases are all prereleases. It should change when a stable tag exists.

### 2. Loopback and the microphone

Loopback of the default render endpoint needs no consent dialog. Windows is built that way, and `docs/DECISIONS.md` says so. The samples stay in the ring and the analyzers. This review found no write of PCM to disk, no upload, and no telemetry. The log beside the settings file records the version, the output-device name, the volume, update errors, and panics (`log.rs`, and the volume line in `main`). It does not record samples. Song title and artist are shown in the window from the system media sessions (`media_windows.rs`) and are not written by `log::line` on that path.

`OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION)` is used to turn a session pid into an exe name. That is a normal limited query, not a process-injection primitive.

The measurement microphone is not opened. `joseon-capture/src/lib.rs` says the mic input is not in this crate yet. When it is, it will be a new consent and a new device path. It should not be added on the same thread object as the loopback without its own review.

Exclusive mode and ASIO are invisible to loopback except as silence or `AUDCLNT_E_DEVICE_IN_USE`. The health sentences for those states exist (`CaptureHealth`). They are a product limitation, not a security hole.

Volume compensation divides samples by the endpoint gain, clamped so the boost is at most 60 dB (`MIN_VOLUME_FACTOR`). Mute is left as silence. A device that reports a tiny software gain makes the measurement 60 dB hotter. That can mislead an SPL figure. It does not become a memory error.

### 3. Files

Settings are `%APPDATA%\Joseon\settings.json` on Windows, overridable with `JOSEON_SETTINGS` (`settings.rs`). The write is a temp file in the same directory and then `rename`. Unknown JSON fields are dropped by serde; `validated` clamps the numbers that have a choice list. A hostile settings file can change layout and the update-check flag. It cannot name a program to run.

Curve import (`library.rs`) reads the whole file, parses it as CSV, and copies it into the Curves folder beside settings. There is no size cap. A huge file dropped on the window (`DroppedFile` in `main.rs`) or chosen in the dialog is an out-of-memory on a user gesture. The parse rejects text with fewer than 8 usable rows. The copy uses `file_name()`, so the destination stays inside the Curves directory.

`OpenCurvesFolder` spawns `explorer` with the folder as an argument, not through a shell.

The WAV reader in the probe is a local tool. It is not on the updater path.

### 4. Installer, signing, secrets

There is no installer and no certificate. Distribution is an unsigned zip from GitHub Releases. `docs/DECISIONS.md` leaves signing as an open decision. Until that exists, every update is “replace the exe and hope the user still clicks Run anyway.”

A search of the tree for key material, tokens, and private-key headers found nothing but the sentence in `DECISIONS.md` that says the repository has no secrets. `sensitivity.rs` stores public product-page URLs. Nothing fetches them at runtime.

### 5. Dependencies

297 packages in `Cargo.lock`. Direct ones that matter: `realfft` 3.5.0 / `rustfft` 6.4.1, `wgpu` 30.0.1, `winit` 0.30.13, `windows` 0.62.2, `sha2` 0.11.0, `serde` 1.0.229, `ab_glyph` 0.2.32, `muda` 0.21.2, `tray-icon` 0.26.1, `rfd` 0.17.2.

Against the RustSec database on the review date, the locked versions sit in the patched or unaffected range of every advisory that names them, except:

- `ttf-parser` 0.25.1, unmaintained (RUSTSEC-2026-0192). It is in the render path via `ab_glyph`, which reads system fonts for the labels.
- `xml-rs` 0.8.29, unmaintained (RUSTSEC-2022-0048). It is a dependency of `gl_generator`, a build-time crate pulled in through the GL side of the graphics stack, not a parser of user XML at runtime.

Neither is a known exploitable bug in these versions. `ttf-parser` is the one that touches bytes from outside the process (font files). Replacing `ab_glyph` is a large job for an unmaintained warning. Pinning a maintained fork, or accepting the warning in a `cargo deny` config with a comment, is enough until the font stack changes.

No vulnerability advisory matched a locked version under the range check described above. CI never runs that check, so the next one will not be noticed at PR time.

## Parity with the macOS app

Only the gaps that are obvious from this tree and the macOS README. The macOS app is reviewed separately.

Behind, in code rather than only in the README:

- **Updates.** macOS installs a build only after the system has checked the signature and the team id. Windows installs an unsigned zip when the user agrees, with an optional GitHub digest (`update.rs`).
- **Capture model.** macOS uses a process tap. Windows uses device loopback. Process loopback is explicitly later (`joseon-capture/src/lib.rs`, `docs/DECISIONS.md`). Exclusive and ASIO playback are silence here.
- **Level at the ear.** `SplEstimator` and the meter drawing exist, and the engine has `set_spl_estimator`. `joseon-app` never calls it. The ear lane in the timeline defaults off.
- **A/B comparison.** `comparison_level_match` and `comparison_headphone_mode` are stored in settings. `joseon-render/src/spectrum/mod.rs` and `meters/mod.rs` both say the comparison trace is not ported.
- **Headphone measurement.** The sweep math, deconvolution, and result page are in `joseon-headphones` and have parity tests. Playback and the microphone are not started (`docs/PORT-PLAN.md` step 15, capture crate comment).
- **Installer and signing.** Step 16 in the port plan. The updater landed. The installer and the certificate did not.

Ahead or different on purpose: the tray mini spectrum, now-playing via the Windows media sessions, the volume compensation (loopback is after the Windows volume control; the Mac tap is before it), and the waterfall, which the port plan says was taken from the Mac renderer and had not yet been run on a PC as of the 2026-10-13 note.

## Prioritized fixes

1. **Updater, before any wider release.** Refuse a release asset with no `sha256` digest. Invoke `curl` and `tar` by absolute System32 path. Keep the allow-list, and do not follow a redirect onto a host outside it. Plan Authenticode (and the same-publisher check the Mac app already has) before auto-check stays on by default. Skip prereleases once a stable tag exists.
2. **Stop CI from being blind to the spectrum budget.** The mean on this x64 box was 128 µs against 130 µs. The test’s fastest-of-five rule and `JOSEON_TIMING_SLACK=5` will hide the next regression. Publish the mean, and keep a budget that can fail on the Windows runner without a factor of five.
3. **Capture thread.** Take the health mutex on a state change, not on every sounding poll. Give `Converter` a fixed cap or reserve the 200 ms worst case up front so the steady state does not allocate. Use the engine’s poison-tolerant lock, and do not drop the `RingWriter` when the capture thread panics. Run the stream-info callback after releasing `state`.
4. **WASAPI slice bounds.** Before `from_raw_parts`, reject a packet whose byte length does not fit the frame count and the mix format. Cap `cbSize` on the device-format blob the same way.
5. **Menu timer.** The raw `*mut App` during `show_menu` works only while `open_menu` truly does not touch `self`. Move the draw onto a path that does not alias the outer `&mut App`, or the next menu change is undefined behavior.
6. **Wire the ear level or stop advertising the settings.** `set_spl_estimator` is never called from the app. The comparison settings are persisted and unused. Stale comments in `joseon-app/src/lib.rs` and `model.rs` say features are missing that are already present, and present that are still missing.
7. **Bound imported curve files** (and the probe WAV reader) so a dropped multi-gigabyte file cannot exhaust the process. A few megabytes is enough for a frequency-response CSV.
8. **Put the RustSec check in CI** (`cargo audit` or `cargo deny`). Record the `ttf-parser` unmaintained warning explicitly so a new advisory is the thing that fails the build.
9. **Single instance.** If `CreateMutexW` fails, do not report success. The current `is_err()` branch starts a second Joseon.
10. **Signing and an installer** remain the open product decision in `docs/DECISIONS.md`. The zip-and-SmartScreen flow is consistent with a test build. It is not consistent with an updater that replaces the exe in place.
