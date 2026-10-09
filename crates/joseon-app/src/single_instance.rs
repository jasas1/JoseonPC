//! One Joseon at a time. The close button only hides the window (the program waits in the tray), so a second
//! start would give a second program that captures too. The second start shows the window of the first.

/// The title of the main window. The second start looks for it.
pub const WINDOW_TITLE: &str = "Joseon";

/// True when this is the only Joseon. False when another one runs: its window was asked to show.
#[cfg(windows)]
pub fn claim(after_update: bool) -> bool {
    use windows::core::w;
    use windows::Win32::Foundation::{GetLastError, ERROR_ALREADY_EXISTS};
    use windows::Win32::System::Threading::CreateMutexW;
    use windows::Win32::UI::WindowsAndMessaging::{FindWindowW, SetForegroundWindow, ShowWindow, SW_RESTORE, SW_SHOW};
    unsafe {
        // The mutex lives as long as the process: the handle is never closed.
        // After an update the old program is still on its way out: wait for it, for 10 s at most.
        let tries = if after_update { 50 } else { 1 };
        for attempt in 0..tries {
            let mutex = CreateMutexW(None, false, w!("Local\\Joseon-one-instance"));
            if mutex.is_err() || GetLastError() != ERROR_ALREADY_EXISTS {
                return true;
            }
            if attempt + 1 < tries {
                // Give the name back, or this process itself keeps it alive.
                if let Ok(handle) = mutex {
                    let _ = windows::Win32::Foundation::CloseHandle(handle);
                }
                std::thread::sleep(std::time::Duration::from_millis(200));
            }
        }
        // "Window Class" is the class name winit gives its windows.
        if let Ok(window) = FindWindowW(w!("Window Class"), w!("Joseon")) {
            let _ = ShowWindow(window, SW_SHOW);
            let _ = ShowWindow(window, SW_RESTORE);
            let _ = SetForegroundWindow(window);
        }
        false
    }
}

#[cfg(not(windows))]
pub fn claim(_after_update: bool) -> bool {
    true
}
