//! What plays now: title and artist, as the players tell the system. Text only.
//!
//! Windows keeps a list of "media sessions" (System Media Transport Controls):
//! every player that shows up in the volume flyout is one. This module picks
//! the session to show and polls the list on a thread of its own. The call to
//! Windows is in `media_windows.rs`; the choice and the thread are tested on
//! every system.
//!
//! Port source: `NowPlayingReader.swift`. macOS has no such list, so the Mac
//! app reads the title of the player window. Windows is simpler and works for
//! every player that takes part.

use joseon_core::contracts::{NowPlaying, NowPlayingCallback, NowPlayingSource};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// One player, as the system lists it.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct MediaSession {
    /// The id of the program: "Spotify.exe", or a store id such as "SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify".
    pub app_id: String,
    pub title: String,
    pub artist: String,
    pub album: String,
    pub is_playing: bool,
}

/// False for the programs of Windows itself that hold a sound session and play nothing a listener chose
/// (the service host, the audio engine). They do not show beside the players.
pub fn is_player(name: &str) -> bool {
    const SYSTEM: [&str; 6] = ["svchost", "audiodg", "system", "dwm", "sihost", "runtimebroker"];
    !SYSTEM.iter().any(|system| name.eq_ignore_ascii_case(system))
}

/// "Spotify.exe" gives "Spotify"; a store id gives the part after "!"; a dotted id gives its last part.
pub fn program_name(app_id: &str) -> String {
    let id = app_id.rsplit('!').next().unwrap_or(app_id);
    let file = id.rsplit(['\\', '/']).next().unwrap_or(id);
    let stem = match file.rsplit_once('.') {
        Some((stem, extension)) if extension.eq_ignore_ascii_case("exe") => stem,
        _ => file,
    };
    // "com.squirrel.Qobuz.Qobuz": the last part is the name.
    let name = if stem.matches('.').count() >= 2 {
        stem.rsplit('.').next().unwrap_or(stem)
    } else {
        stem
    };
    name.to_string()
}

/// The session to show. One that plays comes before one that is paused; among those that play, one whose program
/// makes sound now (`playing_programs`, from the capture) comes first. A session with no title is no session.
pub fn choose(sessions: &[MediaSession], playing_programs: &[String]) -> Option<NowPlaying> {
    let audible = |s: &MediaSession| {
        let name = program_name(&s.app_id);
        playing_programs.iter().any(|p| p.eq_ignore_ascii_case(&name))
    };
    sessions
        .iter()
        .filter(|s| s.is_playing && !s.title.trim().is_empty())
        .max_by_key(|s| audible(s))
        .map(|s| NowPlaying {
            title: s.title.trim().to_string(),
            artist: s.artist.trim().to_string(),
            album: s.album.trim().to_string(),
            source: program_name(&s.app_id),
            is_hi_res: false,
        })
}

/// Reads the sessions of the system. Made on the reader thread and never leaves it.
pub type SessionReader = Box<dyn FnMut() -> Vec<MediaSession>>;

struct Shared {
    current: Mutex<Option<NowPlaying>>,
    callback: Mutex<Option<NowPlayingCallback>>,
    playing_programs: Mutex<Vec<String>>,
    stop: AtomicBool,
}

/// Polls the players of the system and tells when the text changes.
pub struct NowPlayingReader {
    make: fn() -> Option<SessionReader>,
    interval: Duration,
    shared: Arc<Shared>,
    thread: Option<JoinHandle<()>>,
}

impl NowPlayingReader {
    /// `make` runs on the reader thread. `None` = this system has no list of players.
    pub fn with_reader(make: fn() -> Option<SessionReader>) -> Self {
        Self {
            make,
            interval: Duration::from_millis(1500),
            shared: Arc::new(Shared {
                current: Mutex::new(None),
                callback: Mutex::new(None),
                playing_programs: Mutex::new(Vec::new()),
                stop: AtomicBool::new(false),
            }),
            thread: None,
        }
    }

    pub fn set_interval(&mut self, interval: Duration) {
        self.interval = interval;
    }

    /// The programs that make sound now (the `active_sources` of the capture): their session comes first.
    pub fn set_playing_programs(&self, programs: &[String]) {
        let mut stored = self.shared.playing_programs.lock().unwrap();
        if stored.as_slice() != programs {
            *stored = programs.to_vec();
        }
    }

    pub fn is_running(&self) -> bool {
        self.thread.is_some()
    }
}

fn poll(shared: &Shared, read: &mut SessionReader) {
    let sessions = read();
    let programs = shared.playing_programs.lock().unwrap().clone();
    let now = choose(&sessions, &programs);
    let mut current = shared.current.lock().unwrap();
    if *current != now {
        *current = now.clone();
        drop(current);
        if let Some(callback) = &*shared.callback.lock().unwrap() {
            callback(now);
        }
    }
}

impl NowPlayingSource for NowPlayingReader {
    fn current(&self) -> Option<NowPlaying> {
        self.shared.current.lock().unwrap().clone()
    }

    fn set_on_change(&mut self, callback: Option<NowPlayingCallback>) {
        *self.shared.callback.lock().unwrap() = callback;
    }

    fn start(&mut self) {
        if self.thread.is_some() {
            return;
        }
        self.shared.stop.store(false, Ordering::Relaxed);
        let (make, interval, shared) = (self.make, self.interval, Arc::clone(&self.shared));
        self.thread = std::thread::Builder::new()
            .name("joseon now playing".to_string())
            .spawn(move || {
                let Some(mut read) = make() else { return };
                while !shared.stop.load(Ordering::Relaxed) {
                    poll(&shared, &mut read);
                    let end = Instant::now() + interval;
                    while Instant::now() < end && !shared.stop.load(Ordering::Relaxed) {
                        std::thread::sleep(Duration::from_millis(20).min(interval));
                    }
                }
            })
            .ok();
    }

    fn stop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
        // Stopped: nothing is known.
        let had = self.shared.current.lock().unwrap().take();
        if had.is_some() {
            if let Some(callback) = &*self.shared.callback.lock().unwrap() {
                callback(None);
            }
        }
    }
}

impl Drop for NowPlayingReader {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn session(app: &str, title: &str, artist: &str, playing: bool) -> MediaSession {
        MediaSession {
            app_id: app.to_string(),
            title: title.to_string(),
            artist: artist.to_string(),
            album: String::new(),
            is_playing: playing,
        }
    }

    #[test]
    fn the_programs_of_windows_are_no_players() {
        for name in ["svchost", "SVCHOST", "audiodg", "System"] {
            assert!(!is_player(name), "{name}");
        }
        for name in ["Spotify", "SomeGame", "chrome", "process 4312"] {
            assert!(is_player(name), "{name}");
        }
    }

    #[test]
    fn program_names_come_from_the_ids_of_the_system() {
        assert_eq!(program_name("Spotify.exe"), "Spotify");
        assert_eq!(program_name("SpotifyAB.SpotifyMusic_zpdnekdrzrea0!Spotify"), "Spotify");
        assert_eq!(program_name("com.squirrel.Qobuz.Qobuz"), "Qobuz");
        assert_eq!(
            program_name("C:\\Program Files\\foobar2000\\foobar2000.exe"),
            "foobar2000"
        );
        assert_eq!(program_name("chrome"), "chrome");
        assert_eq!(
            program_name("Microsoft.ZuneMusic_8wekyb3d8bbwe!Microsoft.ZuneMusic"),
            "Microsoft.ZuneMusic"
        );
    }

    #[test]
    fn the_session_that_plays_and_sounds_is_shown() {
        let sessions = [
            session("chrome.exe", "A video", "", true),
            session("Spotify.exe", " Saikai ", "Vaundy", true),
            session("Qobuz.exe", "Paused song", "Someone", false),
        ];
        // Both play: the one whose program the capture hears.
        let shown = choose(&sessions, &["Spotify".to_string(), "game".to_string()]).unwrap();
        assert_eq!(
            (shown.title.as_str(), shown.artist.as_str(), shown.source.as_str()),
            ("Saikai", "Vaundy", "Spotify")
        );
        assert_eq!(shown.line(), "Vaundy \u{2013} Saikai");
        assert_eq!(choose(&sessions, &["CHROME".to_string()]).unwrap().source, "chrome");
        // Nothing is heard: still one that plays, never the paused one.
        assert!(choose(&sessions, &[]).is_some_and(|n| n.source != "Qobuz"));
        assert_eq!(choose(&sessions[2..], &[]), None);
        // A session with no title is no session.
        assert_eq!(choose(&[session("x.exe", "  ", "Artist", true)], &[]), None);
        assert_eq!(choose(&[], &[]), None);
    }

    static SCRIPT: Mutex<Vec<MediaSession>> = Mutex::new(Vec::new());

    fn fake() -> Option<SessionReader> {
        Some(Box::new(|| SCRIPT.lock().unwrap().clone()))
    }

    fn none() -> Option<SessionReader> {
        None
    }

    #[test]
    fn the_reader_tells_every_change_once() {
        *SCRIPT.lock().unwrap() = vec![session("Spotify.exe", "One", "A", true)];
        let mut reader = NowPlayingReader::with_reader(fake);
        reader.set_interval(Duration::from_millis(2));
        let (seen, changes) = mpsc::channel();
        reader.set_on_change(Some(Box::new(move |now| {
            let _ = seen.send(now.map(|n| n.line()));
        })));
        assert_eq!(reader.current(), None);
        reader.start();
        reader.start();
        assert_eq!(changes.recv().unwrap().as_deref(), Some("A \u{2013} One"));
        assert_eq!(reader.current().unwrap().title, "One");
        // The same text for many polls: no call.
        std::thread::sleep(Duration::from_millis(30));
        assert!(changes.try_recv().is_err());
        *SCRIPT.lock().unwrap() = vec![session("Spotify.exe", "Two", "A", true)];
        assert_eq!(changes.recv().unwrap().as_deref(), Some("A \u{2013} Two"));
        // Paused: nothing plays.
        *SCRIPT.lock().unwrap() = vec![session("Spotify.exe", "Two", "A", false)];
        assert_eq!(changes.recv().unwrap(), None);
        *SCRIPT.lock().unwrap() = vec![session("Spotify.exe", "Three", "B", true)];
        assert_eq!(changes.recv().unwrap().as_deref(), Some("B \u{2013} Three"));
        // A stop forgets the text and says so.
        reader.stop();
        assert_eq!(changes.recv().unwrap(), None);
        assert!(!reader.is_running() && reader.current().is_none());

        // A system with no list of players: the reader runs and knows nothing.
        let mut reader = NowPlayingReader::with_reader(none);
        reader.start();
        std::thread::sleep(Duration::from_millis(20));
        assert_eq!(reader.current(), None);
        reader.stop();
    }
}
