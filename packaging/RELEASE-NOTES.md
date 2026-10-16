An early test build of Joseon for Windows.

**To try it:** download the zip below, unpack it, play some music, and start `Joseon.exe`.
Nothing is installed. `README.txt` in the zip has the keys and what to do when the panels do not move.

**New in v0.0.12**
- Safer updates. Joseon installs an update only when its SHA-256 matches the one GitHub gives for the release. It downloads only from GitHub, with every redirect checked, and it uses the `curl` and `tar` that come with Windows.

**New in v0.0.11**
- "Quit Joseon" in the tray menu ends the program again. Before, it could hang with no window, and only the Task Manager could end it.

**New in v0.0.10**
- The waterfall: the spectrogram as a 3-D field. The swap button on the spectrogram card shows it in the card; Ctrl+6 shows it over the whole window with the timeline squashed under it. Drag to turn, scroll or pinch to zoom, double click or "Reset view" to come back. The same history, the same dB scale, the same cursor as the flat picture. Ported from the macOS app.

**New in v0.0.9**
- The panels keep moving while a menu is open.
- A swap button (two arrows) on the vectorscope card: one click shows the stereo placement view, the next one the vectorscope again.
- "Freeze Display" is now only in the settings menu (and on the space bar). It stops every panel, so it does not belong in the menu of one panel.
- Background services of Windows (svchost and the like) no longer show beside the players.
- From v0.0.8 this version comes through "Check for Updates…".

**New in v0.0.8**
- Updates from inside the program: the settings menu has "Check for Updates…". When a newer Joseon is there, "Update to … and Relaunch" fetches it, puts it in place and starts it. Joseon also looks by itself after the start and once a day (you can switch that off).

**New in v0.0.7**
- Five more headphones in the list: Beyerdynamic DT 900 Pro X, 64 Audio U12t (m15, m20 and mX modules), Xenns Mangird Tea Pro. For the in-ears, choose the target "Harman in-ear 2019".

**New in v0.0.6**
- Fix for the picture that stood still after a menu was opened. Not yet confirmed on a PC.
- One Joseon at a time: a second start shows the window of the one that waits in the tray. Joseon no longer shows itself as a player.
- Volume: a new item in the settings menu, "Also When the Device Has Its Own Volume Control". Switch it on when the numbers still follow the Windows volume slider.
- A log file, `%APPDATA%\Joseon\joseon.log`. Send it with a problem report.

**New in v0.0.5**
- Headphones: choose your headphone and a target (settings menu, or a click on the strip under the header). 17 headphones and 4 targets come with the program.
- The spectrum shows the response of the headphone, the target, and the music as it arrives at the ear.
- The strip under the header shows stress flags: where this music asks much of this headphone. Click a flag to read why.
- Your own curve: "Import Curve…" in the headphone menu, or drop the file on the window. AutoEq CSV files work.

**New in v0.0.4**
- A tray icon with a live mini spectrum. Left click opens the window, right click opens a menu.
- The mini strip: a small spectrum that stays over your other windows (tray menu, "Mini Spectrum Strip"). It drags with the mouse.
- The close button hides the window to the tray. "Quit Joseon" in the tray menu ends the program.

**New in v0.0.3**
- Menus: a "..." button on every card (or a right click), and a settings button at the top right.
- The name of the song that plays, beside the name of the player (for players that tell Windows).

**New in v0.0.2**
- The Windows volume no longer changes the numbers. Joseon takes it out of the measurement, so loudness and peaks describe the music at every position of the volume slider. The header shows "volume −… dB" while it does that.
- A dark title bar.

**In this build**
- The window with all panels: spectrum, spectrogram, vectorscope and stereo placement, loudness and peak, session timeline.
- Capture of the sound the PC plays (WASAPI loopback of the default output device).
- Three layouts, a shared cursor, dividers that drag.

**Not in this build yet**
- The level at the ear in dB SPL, and the A/B comparison.
- An installer, a signed file. Windows shows "Windows protected your PC" at the first start: click "More info", then "Run anyway".

**Tested on one real PC so far** (capture from a music player, all three layouts). Reports are welcome.
