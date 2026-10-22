//! L3. The Joseon window: settings, panel layout, the cards, the header.
//!
//! - `settings`: the settings file.
//! - `layout`: where the cards stand, pure geometry.
//! - `model`: the audio source, the analysis engine, the header text.
//! - `scene`: the cards and the header drawn with wgpu, and what the pointer and the keys do. It
//!   draws into any texture, so the tests and the snapshot mode need no window.
//! - `main.rs`: the window itself (winit).
//!
//! Not here yet: menus, the settings window, headphone curves, the level at the ear, the tray.
//!
//! Port source: `Sources/JoseonApp` in the macOS repo.

pub mod layout;
pub mod local_time;
pub mod log;
pub mod menu;
pub mod mini;
pub mod model;
pub mod number_text;
pub mod pacing;
pub mod scene;
pub mod settings;
pub mod single_instance;
pub mod stress_strip;
pub mod update;

#[cfg(test)]
mod scene_tests;
