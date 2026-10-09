//! A small log file beside the settings file. The Windows program has no console, so a failure would leave
//! no trace: this file holds the start of the program, what the capture found, and a panic with its place.

use std::io::Write;
use std::path::PathBuf;
use std::sync::{Mutex, OnceLock};

static FILE: OnceLock<Mutex<Option<std::fs::File>>> = OnceLock::new();

/// Starts a new log at `path` (the old one goes) and sends panics there.
pub fn start(path: PathBuf) {
    if let Some(folder) = path.parent() {
        let _ = std::fs::create_dir_all(folder);
    }
    let _ = FILE.set(Mutex::new(std::fs::File::create(&path).ok()));
    let before = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        line(&format!("PANIC: {info}"));
        before(info);
    }));
}

/// One line with the seconds since the start of the log. Nothing happens before `start`.
pub fn line(text: &str) {
    static BEGIN: OnceLock<std::time::Instant> = OnceLock::new();
    let seconds = BEGIN.get_or_init(std::time::Instant::now).elapsed().as_secs_f64();
    if let Some(Ok(mut file)) = FILE.get().map(|f| f.lock()) {
        if let Some(file) = file.as_mut() {
            let _ = writeln!(file, "{seconds:9.2}  {text}");
            let _ = file.flush();
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn a_line_before_the_start_is_dropped() {
        super::line("nothing listens");
    }
}
