Joseon for Windows - test build
===============================

Joseon shows what is in the sound your PC plays: spectrum, spectrogram,
stereo picture, loudness and peak, and a timeline of the session.
It only listens. It changes nothing in the sound, and it records nothing.

This is an early test build.

Start
-----
1. Unpack this folder anywhere (the Desktop is fine). Nothing is installed.
2. Play music or a game.
3. Double-click Joseon.exe.

Windows can show "Windows protected your PC" at the first start, because
the file is not signed. Click "More info", then "Run anyway".

Needs Windows 10 or 11 (64-bit) and a graphics driver with Direct3D 12.

Keys
----
Space           freeze the picture
R               reset the measurement
F or F11        full screen
Ctrl+1 / 2 / 3  layout: Essential / Spectrum / Metering
Ctrl+4          vectorscope or stereo placement
Ctrl+5          show or hide the timeline
Click           pin a cursor on a plot; Esc removes it
Left / Right    move the cursor (Shift: one octave)

The gaps between the cards drag with the mouse.

Tray icon and mini strip
------------------------
Joseon puts a small icon with a live spectrum into the tray (bottom right,
next to the clock; Windows may keep it behind the "^" arrow).
- Left click: open the window.
- Right click: a menu. "Mini Spectrum Strip" shows a small spectrum that
  stays over your other windows. Drag it where you want it.
- The close button of the window only hides it. Joseon runs on in the tray.
  "Quit Joseon" in the tray menu ends it.

Menus
-----
The vectorscope card has a second button with two arrows: it swaps the
view between the vectorscope and the stereo placement by frequency.
"Freeze Display" (settings menu, or the space bar) stops all panels.

The session timeline: a click sets a pin at a moment of the past. The
line over the timeline then shows the numbers of that moment (time,
loudness, peak). A second click on the pin takes it away. The small
mark beside "no clipped samples" is the sign that would show on the
timeline where samples clipped.
Every card has a "..." button at its top right (a right click on the card
opens the same menu). The button with three lines at the top right holds
all settings.

Headphones
----------
The strip under the top bar names your headphone. Click it to choose one
of the list, or "None". The spectrum then shows the response of the
headphone, and the strip shows flags where the music asks much of it.
Click a flag to read why.
Your own headphone: choose "Import Curve..." in that menu, or drop the
file on the window. An AutoEq CSV file works, and so does a text file
with two columns (frequency in Hz, level in dB).

If the numbers follow the Windows volume
---------------------------------------
Joseon takes the Windows volume out of the numbers, so they describe the
music and not the position of the slider. Some sound devices say that
they set the volume themselves, and then Joseon leaves the numbers alone.
If your numbers still go down with the Windows slider, switch on "Also
When the Device Has Its Own Volume Control" in the settings menu.
The volume slider inside a player (Spotify, a game) always changes the
numbers: Joseon cannot see that slider.

Updates
-------
Joseon looks for a newer version after the start and once a day. When
there is one, a line over the panels says so. Open the settings menu
(the button with three lines) and choose "Update to ... and Relaunch".
Joseon fetches the new version from the project page on GitHub, puts it
in this folder and starts again. Your settings stay.
To look by hand: "Check for Updates..." in the same menu. To stop the
automatic look: switch off "Check for Updates Automatically".
The only thing Joseon sends is the request for the list of versions.

If something goes wrong
-----------------------
Joseon writes a small log file: %APPDATA%\Joseon\joseon.log
Send it with your report.

If the panels do not move
-------------------------
A line under the top bar says why. The usual reasons:
- The player uses "exclusive mode" (WASAPI exclusive or ASIO). Set it to
  shared mode.
- No sound device is switched on.

For a closer look, open a terminal in this folder and run:

    .\joseon-probe.exe devices
    .\joseon-probe.exe capture --seconds 10

and send the text it prints.

Settings
--------
Joseon keeps its settings in %APPDATA%\Joseon\settings.json.
Delete that file to go back to the defaults.

License
-------
GNU General Public License, version 3. See LICENSE.txt and NOTICE.txt.
