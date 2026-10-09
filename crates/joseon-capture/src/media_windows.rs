//! The media sessions of Windows (System Media Transport Controls), read with WinRT.

use crate::now_playing::{MediaSession, SessionReader};
use windows::Media::Control::{
    GlobalSystemMediaTransportControlsSessionManager as Manager,
    GlobalSystemMediaTransportControlsSessionPlaybackStatus as Status,
};
use windows::Win32::System::Com::{CoInitializeEx, COINIT_MULTITHREADED};

/// A reader of the sessions, for the thread that calls this. `None` when Windows gives no session manager.
pub fn session_reader() -> Option<SessionReader> {
    unsafe {
        // The thread stays in the multithreaded apartment for its whole life.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
    let manager = Manager::RequestAsync().ok()?.join().ok()?;
    Some(Box::new(move || read(&manager)))
}

fn read(manager: &Manager) -> Vec<MediaSession> {
    let Ok(sessions) = manager.GetSessions() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for session in sessions {
        let is_playing = session
            .GetPlaybackInfo()
            .and_then(|info| info.PlaybackStatus())
            .is_ok_and(|status| status == Status::Playing);
        // A player that does not answer is left out: the next poll asks again.
        let Ok(properties) = session.TryGetMediaPropertiesAsync().and_then(|op| op.join()) else {
            continue;
        };
        let text =
            |value: windows::core::Result<windows::core::HSTRING>| value.map(|s| s.to_string()).unwrap_or_default();
        out.push(MediaSession {
            app_id: text(session.SourceAppUserModelId()),
            title: text(properties.Title()),
            artist: text(properties.Artist()),
            album: text(properties.AlbumTitle()),
            is_playing,
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On any Windows machine: the list is read, with or without players.
    #[test]
    fn the_sessions_are_read_or_there_is_no_manager() {
        match session_reader() {
            Some(mut read) => println!("media sessions: {:?}", read()),
            None => println!("this Windows gives no media session manager"),
        }
    }
}
