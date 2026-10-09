//! The scene, drawn with no window.

use crate::layout::Card;
use crate::menu::Command;
use crate::model::{Banner, HeaderState, SignalState, WAITING_FOR_AUDIO};
use crate::scene::{options_button, preset_buttons, settings_button, Action, Key, MenuTarget, PointerShape, Scene};
use crate::settings::{AppSettings, LayoutPreset};
use joseon_core::contracts::{AnalysisFrame, SpectrumReading, StreamInfo};
use joseon_render::contracts::PanelKind;
use joseon_render::synthetic::{Options, SyntheticFrames};
use joseon_render::synthetic_session::demo_session;
use joseon_render::text::fonts;
use std::sync::Arc;

fn review_path(name: &str) -> std::path::PathBuf {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/joseon-app-review");
    std::fs::create_dir_all(&directory).unwrap();
    directory.join(name)
}

fn header() -> HeaderState {
    let stream = StreamInfo {
        bit_depth: Some(24),
        active_sources: vec!["Qobuz".to_string()],
        ..StreamInfo::new(96_000.0, 2, "Speakers (USB DAC)")
    };
    let mut header = HeaderState::new(Some(&stream), SignalState::Live);
    header.now_playing = "Vaundy \u{2013} Saikai".to_string();
    header.volume = crate::model::volume_text(17.0);
    header
}

/// A scene that has seen four seconds of the synthetic music and the demo session.
fn scene(settings: AppSettings, width: f32, height: f32) -> Option<Scene> {
    if !fonts::available() {
        return None;
    }
    let mut scene = Scene::new(settings, width, height, 2.0)?;
    scene.set_status(header(), Vec::new(), false, None);
    scene.set_utc_offset_seconds(7_200);
    let session = demo_session(30, true, None);
    let frames = SyntheticFrames::sequence(240, Options::default());
    for (i, frame) in frames.into_iter().enumerate() {
        scene.tick(1000.0 + i as f64 / 60.0, &Arc::new(frame), Some(&session));
    }
    Some(scene)
}

fn save(scene: &mut Scene, name: &str) -> (u32, Vec<u8>) {
    let (w, h, rgba) = scene.snapshot_rgba(2000.0).unwrap();
    std::fs::write(review_path(name), joseon_render::target::encode_png(w, h, &rgba)).unwrap();
    (w, rgba)
}

fn pixel(rgba: &[u8], width: u32, x: f32, y: f32) -> [u8; 3] {
    let i = (((y * 2.0) as u32 * width + (x * 2.0) as u32) * 4) as usize;
    [rgba[i], rgba[i + 1], rgba[i + 2]]
}

#[test]
fn every_preset_draws_its_cards() {
    for preset in LayoutPreset::ALL {
        let settings = AppSettings {
            layout_preset: preset,
            ..AppSettings::default()
        };
        let Some(mut scene) = scene(settings, 1280.0, 800.0) else {
            return;
        };
        let (width, rgba) = save(&mut scene, &format!("window-{}.png", preset.key()));
        let layout = scene.layout().clone();
        assert!(layout.cards.len() >= 3, "{preset:?}");
        for (card, rect) in &layout.cards {
            // The gap beside a card has the window color; inside the card something lighter is drawn.
            let outside = pixel(&rgba, width, rect.x - 4.0, rect.mid_y());
            assert!(
                outside.iter().all(|c| *c < 30),
                "{preset:?} {card:?}: the gap is dark: {outside:?}"
            );
            let mut lit = 0;
            let mut x = rect.x + 20.0;
            while x < rect.max_x() - 20.0 {
                let p = pixel(&rgba, width, x, rect.mid_y());
                if p.iter().map(|c| *c as u32).sum::<u32>() > 110 {
                    lit += 1;
                }
                x += 2.0;
            }
            assert!(lit > 3, "{preset:?} {card:?} has a picture ({lit} lit points)");
        }
    }
}

#[test]
fn the_pointer_and_the_keys_work() {
    let Some(mut scene) = scene(AppSettings::default(), 1280.0, 800.0) else {
        return;
    };
    let spectrum = scene.layout().card(Card::Spectrum).unwrap();
    // Over the spectrum plot: a cursor at the frequency under the pointer, shared by every panel.
    scene.pointer_moved(Some([spectrum.mid_x(), spectrum.mid_y()]));
    let hover = scene.cursor().expect("a cursor under the pointer");
    assert!(!hover.is_pinned && hover.source == PanelKind::Spectrum);
    assert!(
        hover.frequency_hz > 300.0 && hover.frequency_hz < 2_000.0,
        "{} Hz",
        hover.frequency_hz
    );
    assert_eq!(scene.pointer_shape(), PointerShape::Crosshair);
    // A click pins it; it stays when the pointer leaves; Esc clears it.
    assert_eq!(scene.pointer_pressed(), Action::None);
    scene.pointer_released();
    scene.pointer_moved(None);
    assert!(scene.cursor().unwrap().is_pinned);
    assert_eq!(scene.pointer_shape(), PointerShape::Arrow);
    // The arrow keys move it by a semitone, or by an octave.
    let before = scene.cursor().unwrap().frequency_hz;
    scene.key(Key::CursorUp { octave: true });
    assert!((scene.cursor().unwrap().frequency_hz / before - 2.0).abs() < 0.01);
    scene.key(Key::CursorDown { octave: false });
    scene.tick(
        2000.0,
        &Arc::new(SyntheticFrames::sequence(1, Options::default()).remove(0)),
        None,
    );
    save(&mut scene, "window-cursor.png");
    scene.key(Key::Pin);
    assert!(!scene.cursor().unwrap().is_pinned);
    scene.key(Key::Escape);
    assert_eq!(scene.cursor(), None);
    // With no cursor an arrow key puts one at the peak of the music.
    scene.key(Key::CursorUp { octave: false });
    assert!(scene.cursor().unwrap().is_pinned);
    scene.key(Key::Escape);

    // Keys that the owner acts on.
    assert_eq!(scene.key(Key::Freeze), Action::ToggleFreeze);
    assert_eq!(scene.key(Key::Reset), Action::ResetMeasurement);
    assert_eq!(scene.key(Key::FullScreen), Action::ToggleFullScreen);
    // Layout keys change the settings.
    assert_eq!(scene.key(Key::Preset(LayoutPreset::Metering)), Action::SettingsChanged);
    assert_eq!(scene.layout().key, "metering+timeline");
    assert_eq!(scene.key(Key::Preset(LayoutPreset::Metering)), Action::None);
    assert_eq!(scene.key(Key::ToggleTimeline), Action::SettingsChanged);
    assert_eq!(scene.layout().key, "metering");
    assert_eq!(scene.key(Key::TogglePlacement), Action::SettingsChanged);
    assert!(scene.settings().stereo_placement_mode);
    // A click on a layout button.
    let (_, button) = preset_buttons(1280.0)[0];
    scene.pointer_moved(Some([button.mid_x(), button.mid_y()]));
    assert_eq!(scene.pointer_shape(), PointerShape::Hand);
    assert_eq!(scene.pointer_pressed(), Action::SettingsChanged);
    assert_eq!(scene.settings().layout_preset, LayoutPreset::Essential);
    scene.pointer_released();
}

#[test]
fn a_divider_is_dragged_with_the_pointer() {
    let Some(mut scene) = scene(AppSettings::default(), 1280.0, 800.0) else {
        return;
    };
    let spectrum = scene.layout().card(Card::Spectrum).unwrap();
    let grip = [spectrum.mid_x(), spectrum.max_y() + 4.0];
    scene.pointer_moved(Some(grip));
    assert_eq!(scene.pointer_shape(), PointerShape::ResizeRows);
    scene.pointer_pressed();
    scene.pointer_moved(Some([grip[0], grip[1] + 20.0]));
    assert_eq!(scene.pointer_shape(), PointerShape::ResizeRows);
    assert_eq!(scene.pointer_released(), Action::SettingsChanged);
    assert_eq!(scene.layout().card(Card::Spectrum).unwrap().h, spectrum.h + 20.0);
    assert!(scene.settings().splits.contains_key("split.v2.essential.main"));
    // The timeline strip: its height is a setting of its own.
    let strip = scene.layout().card(Card::Timeline).unwrap();
    scene.pointer_moved(Some([strip.mid_x(), strip.y - 4.0]));
    scene.pointer_pressed();
    scene.pointer_moved(Some([strip.mid_x(), strip.y - 4.0 - 60.0]));
    assert_eq!(scene.pointer_released(), Action::SettingsChanged);
    assert_eq!(scene.settings().timeline_strip_height, 200.0);
    assert_eq!(scene.layout().card(Card::Timeline).unwrap().h, 200.0);
    // Between two cards of the row.
    let scope = scene.layout().card(Card::Vectorscope).unwrap();
    scene.pointer_moved(Some([scope.x - 4.0, scope.mid_y()]));
    assert_eq!(scene.pointer_shape(), PointerShape::ResizeColumns);
}

#[test]
fn silence_a_banner_and_a_frozen_display_show() {
    let settings = AppSettings {
        show_timeline: false,
        ..AppSettings::default()
    };
    let Some(mut scene) = Scene::new(settings, 1100.0, 700.0, 2.0) else {
        return;
    };
    if !fonts::available() {
        return;
    }
    let banner = Banner {
        text: "A player uses the output device in exclusive mode. Windows then gives no copy of the sound.".to_string(),
        is_error: false,
    };
    let top = scene.layout().card(Card::Spectrum).unwrap().y;
    scene.set_status(HeaderState::default(), vec![banner], false, Some(WAITING_FOR_AUDIO));
    assert_eq!(
        scene.layout().card(Card::Spectrum).unwrap().y,
        top + 38.0,
        "the banner takes its row"
    );
    let silent = Arc::new(AnalysisFrame::new(SpectrumReading::silent(1024)));
    scene.tick(10.0, &silent, None);
    let (width, rgba) = save(&mut scene, "window-waiting.png");
    // The plate stands in the middle of the spectrum card.
    let spectrum = scene.layout().card(Card::Spectrum).unwrap();
    let panel = crate::layout::WindowLayout::panel_rect(spectrum);
    let plate = pixel(&rgba, width, panel.mid_x() - 60.0, panel.mid_y() - 10.0);
    let plot = pixel(&rgba, width, panel.x + 80.0, panel.mid_y() + 60.0);
    assert_ne!(plate, plot, "the plate is not the plot");

    // Frozen: the picture dims, and the header says so.
    let mut scene = scene_with_music();
    let (width, live) = save(&mut scene, "window-live.png");
    scene.set_status(header(), Vec::new(), true, None);
    let (_, frozen) = save(&mut scene, "window-frozen.png");
    let spectrum = scene.layout().card(Card::Spectrum).unwrap();
    let (mut live_sum, mut frozen_sum) = (0u64, 0u64);
    let mut x = spectrum.x + 60.0;
    while x < spectrum.max_x() - 60.0 {
        live_sum += pixel(&live, width, x, spectrum.mid_y())
            .iter()
            .map(|c| *c as u64)
            .sum::<u64>();
        frozen_sum += pixel(&frozen, width, x, spectrum.mid_y())
            .iter()
            .map(|c| *c as u64)
            .sum::<u64>();
        x += 3.0;
    }
    assert!(
        frozen_sum * 10 < live_sum * 9,
        "frozen is dimmer: {frozen_sum} vs {live_sum}"
    );
}

fn scene_with_music() -> Scene {
    scene(AppSettings::default(), 1100.0, 700.0).expect("a GPU and fonts")
}

#[test]
fn the_smallest_window_and_a_resize_draw() {
    let Some(mut scene) = scene(AppSettings::default(), 900.0, 560.0) else {
        return;
    };
    save(&mut scene, "window-small.png");
    assert_eq!(scene.layout().key, "essential");
    scene.resize(1600.0, 1000.0, 1.0);
    assert_eq!(scene.pixel_size(), (1600, 1000));
    let (w, h, _) = scene.snapshot_rgba(3000.0).unwrap();
    assert_eq!((w, h), (1600, 1000));
    assert_eq!(scene.layout().key, "essential+timeline");
}

#[test]
fn the_buttons_open_menus_and_the_items_change_the_window() {
    let Some(mut scene) = scene(AppSettings::default(), 1280.0, 800.0) else {
        return;
    };
    // The "..." of the spectrum card.
    let button = options_button(scene.layout().card(Card::Spectrum).unwrap());
    scene.pointer_moved(Some([button.mid_x(), button.mid_y()]));
    assert_eq!(scene.pointer_shape(), PointerShape::Hand);
    assert_eq!(
        scene.pointer_pressed(),
        Action::OpenMenu(MenuTarget::Card(Card::Spectrum))
    );
    scene.pointer_released();
    assert!(scene.cursor().is_none(), "a click on the button pins no cursor");
    let menu = scene.menu(MenuTarget::Card(Card::Spectrum));
    assert_eq!(crate::menu::commands(&menu).len(), 14);
    assert_eq!(scene.run(Command::ToggleSide), Action::SettingsChanged);
    assert!(scene.settings().show_side);
    // The same choice again changes nothing.
    assert_eq!(scene.run(Command::Tilt(0.0)), Action::None);
    // The settings button, and a right click on a card and on the header.
    let gear = settings_button(1280.0);
    scene.pointer_moved(Some([gear.mid_x(), gear.mid_y()]));
    assert_eq!(scene.pointer_pressed(), Action::OpenMenu(MenuTarget::Settings));
    scene.pointer_released();
    let meters = scene.layout().card(Card::Meters).unwrap();
    scene.pointer_moved(Some([meters.mid_x(), meters.mid_y()]));
    assert_eq!(
        scene.pointer_context(),
        Action::OpenMenu(MenuTarget::Card(Card::Meters))
    );
    scene.pointer_moved(Some([300.0, 20.0]));
    assert_eq!(scene.pointer_context(), Action::OpenMenu(MenuTarget::Settings));
    // Items that are actions.
    assert_eq!(scene.run(Command::ToggleFreeze), Action::ToggleFreeze);
    assert_eq!(scene.run(Command::ClearTimeline), Action::ClearTimeline);
    assert_eq!(scene.run(Command::ToggleDemo), Action::RestartSource);
    // A layout item moves the cards.
    assert_eq!(
        scene.run(Command::Preset(LayoutPreset::Spectrum)),
        Action::SettingsChanged
    );
    assert!(scene.layout().key.starts_with("spectrum"));
    assert_eq!(scene.run(Command::StereoPlacement(true)), Action::SettingsChanged);
    save(&mut scene, "window-menu-buttons.png");
}

#[test]
fn the_strip_opens_the_headphone_menu_and_a_chip_shows_its_detail() {
    use crate::model::{CurveNames, StressState};
    use crate::stress_strip::Hit;
    use joseon_core::contracts::{StressFlag, StressSeverity};
    let Some(mut scene) = scene(AppSettings::default(), 1280.0, 800.0) else {
        return;
    };
    scene.set_curve_names(CurveNames {
        headphones: vec!["HD 600".to_string(), "HD 800 S".to_string()],
        targets: vec!["Harman".to_string()],
    });
    // No headphone: the button asks for one, and the menu chooses it.
    let (button, _) = scene.strip_layout().button.clone().expect("the button");
    scene.pointer_moved(Some([button.mid_x(), button.mid_y()]));
    assert_eq!(scene.pointer_shape(), PointerShape::Hand);
    assert_eq!(scene.pointer_pressed(), Action::OpenMenu(MenuTarget::Headphone));
    scene.pointer_released();
    let menu = scene.menu(MenuTarget::Headphone);
    assert!(crate::menu::commands(&menu).contains(&Command::ImportCurve));
    assert_eq!(scene.run(Command::Headphone(Some(1))), Action::SettingsChanged);
    assert_eq!(scene.settings().headphone_name.as_deref(), Some("HD 800 S"));
    assert_eq!(scene.run(Command::ImportCurve), Action::ImportCurve);
    assert_eq!(scene.run(Command::OpenCurvesFolder), Action::OpenCurvesFolder);
    // A flag: its chip shows the detail at a click, and hides it at the next one.
    scene.set_stress(StressState {
        headphone: Some("HD 800 S".to_string()),
        flags: vec![StressFlag {
            id: "sub-bass".to_string(),
            severity: StressSeverity::High,
            title: "Strong sub-bass".to_string(),
            detail: "The music has much energy under 40 Hz.".to_string(),
            frequency_range_hz: None,
            plot_label: String::new(),
        }],
        ..StressState::default()
    });
    let chip = scene.strip_layout().chips[0].rect;
    assert_eq!(
        crate::stress_strip::hit(scene.strip_layout(), chip.mid_x(), chip.mid_y()),
        Some(Hit::Chip(0))
    );
    scene.pointer_moved(Some([chip.mid_x(), chip.mid_y()]));
    assert_eq!(scene.pointer_pressed(), Action::None);
    scene.pointer_released();
    assert!(scene.strip_layout().detail.is_some());
    let (width, rgba) = save(&mut scene, "app-stress-detail");
    let plate = scene.strip_layout().detail.clone().unwrap().rect;
    let [r, g, b] = pixel(&rgba, width, plate.mid_x(), plate.max_y() - 4.0);
    assert!(
        r > 14 && g > 14 && b > 20,
        "the plate is lighter than the window: {r} {g} {b}"
    );
    assert_eq!(scene.pointer_pressed(), Action::None);
    scene.pointer_released();
    assert!(scene.strip_layout().detail.is_none());
}
