# The first test on a Windows PC

This test needs 10 minutes and a PC with Windows 10 or 11 that plays sound. It installs nothing.

## Get the probe

1. Open the newest green run at <https://github.com/jasas1/JoseonPC/actions>.
2. Download the file `joseon-probe-windows` at the bottom of the page. Unpack it.
3. Open a terminal in that folder (Shift + right click, "Open in Terminal").

Windows can show "Windows protected your PC" at the first start: the file is not signed. Click
"More info", then "Run anyway".

## Run it

Play music in any player. Then:

```
.\joseon-probe.exe devices
.\joseon-probe.exe capture --seconds 10 > capture.txt
```

Send back the text of the first command and the file `capture.txt`.

## What the answers mean

| Line | Good | Not good |
|---|---|---|
| `devices` | Your headphones or speakers, one with `"isDefault":true` | `"count":0`, or an error |
| `capture`, `"health"` | `"Signal"` | `"SilentWhilePlaying"`, `"ExclusiveMode"`, `"NoStream(...)"` |
| `capture`, `"rawPeakDBFS"` | A number under 0 that moves with the music | `null` |
| last line, `"result"` | `"signal"` | `"silence"` |

## More checks, when the first one works

1. **Volume.** Joseon takes the Windows volume out of its numbers. Check it: play one song,
   set the Windows volume to 100 %, then to 20 %. The loudness numbers (M, S) must stay about the
   same, and the header must show "volume −… dB". With the probe:
   `capture --seconds 10` at both volumes gives the same `integratedLUFS`;
   `capture --seconds 10 --raw` gives numbers that fall with the slider.
2. **Device change.** Start `capture --seconds 30`, then switch the output (speakers to
   headset). The `stream` in the lines must change, and `streamsOpened` must go up.
3. **Exclusive mode.** In a player with WASAPI exclusive output (foobar2000, Qobuz, Tidal),
   switch it on and play. `health` must say `ExclusiveMode`, with advice.
4. **A game.** Start a game with sound and run `capture`. `activeSources` must name the game.
