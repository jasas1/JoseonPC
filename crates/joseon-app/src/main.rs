//! The Joseon window.
//!
//! ```text
//! joseon                       the app: the sound the computer plays, in a window
//! joseon --demo                the demo signal, with no capture
//! joseon --snapshot FOLDER     no window: PNG pictures of the three layouts (after --seconds N, default 4)
//! ```

// A release build on Windows is a window program: no console window behind it.
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use joseon_app::menu::{self, Command, Entry};
use joseon_app::mini::{self, StripView, TrayPicture};
use joseon_app::model::AppModel;
use joseon_app::model::HeaderState;
use joseon_app::scene::{Action, Key, PointerShape, Scene};
use joseon_app::settings::{settings_path, AppSettings, LayoutPreset};
use joseon_app::{layout, local_time};
use joseon_core::engine::uptime;
use joseon_render::context::TARGET_FORMAT;
use joseon_render::RenderContext;
use std::path::PathBuf;
use std::sync::Arc;
use winit::application::ApplicationHandler;
use winit::dpi::{LogicalSize, PhysicalSize};
use winit::event::{ElementState, MouseButton, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop};
use winit::keyboard::{Key as WinitKey, ModifiersState, NamedKey};
use winit::window::{CursorIcon, Fullscreen, Window, WindowId, WindowLevel};

struct Arguments {
    demo: bool,
    /// The program was started by an update: the old one may still be on its way out.
    after_update: bool,
    snapshot: Option<PathBuf>,
    seconds: f64,
}

fn arguments() -> Arguments {
    let mut out = Arguments {
        demo: false,
        after_update: false,
        snapshot: None,
        seconds: 4.0,
    };
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--demo" => out.demo = true,
            joseon_app::update::AFTER_UPDATE => out.after_update = true,
            "--snapshot" => out.snapshot = args.next().map(PathBuf::from),
            "--seconds" => out.seconds = args.next().and_then(|v| v.parse().ok()).unwrap_or(4.0),
            _ => {}
        }
    }
    out
}

/// The model and the scene together: one tick brings the picture up to date.
struct Shell {
    model: AppModel,
    scene: Scene,
    path: PathBuf,
    /// False for a run that must not change the settings file (snapshot, `--demo`).
    saves: bool,
    ticks: u64,
    /// The header text of the last slow tick: the tray tooltip and the mini strip use it too.
    header: HeaderState,
    /// A line about an update, over the cards.
    update_banner: Option<String>,
}

impl Shell {
    /// One tick. `visible` = the main window shows: only then the panels take the frame.
    fn tick(&mut self, visible: bool) {
        // A few times per second: the stream facts, the banners, the session record.
        let slow = self.ticks % 10 == 0;
        self.ticks += 1;
        if slow {
            self.model.supervise();
            let placeholder = self.model.placeholder();
            self.header = self.model.header();
            let mut banners = self.model.banners();
            if let Some(text) = &self.update_banner {
                banners.push(joseon_app::model::Banner {
                    text: text.clone(),
                    is_error: false,
                });
            }
            self.scene
                .set_status(self.header.clone(), banners, self.model.is_frozen(), placeholder);
            if self.scene.curve_names() != self.model.curve_names() {
                self.scene.set_curve_names(self.model.curve_names().clone());
            }
            self.scene.set_stress(self.model.stress(uptime()));
        }
        if !visible {
            return;
        }
        let session = slow.then(|| self.model.session());
        self.scene.tick(uptime(), &self.model.frame(), session.as_ref());
    }

    fn act(&mut self, action: Action) -> bool {
        match action {
            Action::None => {}
            Action::SettingsChanged => {
                self.model.settings = self.scene.settings().clone();
                self.model.apply_settings();
                self.save();
            }
            Action::ToggleFreeze => self.model.toggle_freeze(),
            Action::ResetMeasurement => self.model.reset_measurement(),
            Action::ClearTimeline => self.model.clear_session(),
            Action::RestartSource => {
                self.model.settings = self.scene.settings().clone();
                self.model.apply_settings();
                self.save();
                self.model.start();
            }
            // The window program shows the file dialog and the folder.
            Action::ImportCurve | Action::OpenCurvesFolder | Action::CheckUpdates | Action::InstallUpdate => {}
            // The window program shows menus and windows, and ends the program.
            Action::OpenMenu(_) | Action::ToggleFullScreen | Action::OpenWindow | Action::Quit => return true,
        }
        self.ticks = 0; // the status goes to the scene at the next tick
        false
    }

    /// Takes a curve file into the library and chooses it as the headphone.
    fn import(&mut self, path: &std::path::Path) -> Result<(), String> {
        self.model.settings = self.scene.settings().clone();
        let name = self.model.import_curve(path)?;
        self.scene.set_curve_names(self.model.curve_names().clone());
        let mut settings = self.scene.settings().clone();
        settings.headphone_name = Some(name);
        self.scene.set_settings(settings);
        self.save();
        self.ticks = 0;
        Ok(())
    }

    fn save(&self) {
        if self.saves {
            // A settings file that cannot be written is not a reason to stop: the app runs on.
            let _ = self.scene.settings().save(&self.path);
        }
    }
}

fn snapshot(mut shell: Shell, folder: &PathBuf, seconds: f64) -> i32 {
    let end = std::time::Instant::now() + std::time::Duration::from_secs_f64(seconds.max(0.2));
    while std::time::Instant::now() < end {
        shell.tick(true);
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    if std::fs::create_dir_all(folder).is_err() {
        eprintln!("joseon: the folder {} cannot be made", folder.display());
        return 1;
    }
    for preset in LayoutPreset::ALL {
        shell.scene.key(Key::Preset(preset));
        for _ in 0..30 {
            shell.tick(true);
            std::thread::sleep(std::time::Duration::from_millis(16));
        }
        let Some((w, h, rgba)) = shell.scene.snapshot_rgba(uptime()) else {
            eprintln!("joseon: the picture could not be read back");
            return 1;
        };
        let file = folder.join(format!("joseon-{}.png", preset.key()));
        if std::fs::write(&file, joseon_render::target::encode_png(w, h, &rgba)).is_err() {
            eprintln!("joseon: {} cannot be written", file.display());
            return 1;
        }
        println!("{}", file.display());
    }
    0
}

struct App {
    shell: Shell,
    window: Option<Arc<Window>>,
    surface: Option<wgpu::Surface<'static>>,
    config: Option<wgpu::SurfaceConfiguration>,
    modifiers: ModifiersState,
    shape: PointerShape,
    failure: Option<String>,
    /// Where the pointer is, in points.
    pointer: Option<[f32; 2]>,
    /// The commands of the menu that is open (or was open last): the id of an item is "p" and its index here.
    commands: Vec<Command>,
    /// False for `--demo`: no tray icon, and the close button ends the program.
    uses_tray: bool,
    tray: Option<tray_icon::TrayIcon>,
    tray_picture: TrayPicture,
    /// The commands of the tray menu: the id of an item is "t" and its index here.
    tray_commands: Vec<Command>,
    /// What the tray menu was built from. A change builds it again.
    tray_key: String,
    strip: Option<Strip>,
    /// Where the pointer is in the strip, in points.
    strip_pointer: [f32; 2],
    quit: bool,
    /// A menu that waits for the end of the event that asked for it: the window, the items, the place.
    pending_menu: Option<(Arc<Window>, Vec<Entry>, [f32; 2])>,
    updater: joseon_app::update::Updater,
    /// What the updater said last (a change goes to the window).
    update_status: joseon_app::update::Status,
    /// When the program asked for updates by itself last. `None` = not yet.
    last_update_check: Option<std::time::Instant>,
    started: std::time::Instant,
    /// The timer had to draw the main window (written to the log once).
    stalled: bool,
    /// The last picture of the main window.
    last_draw: std::time::Instant,
    last_background: std::time::Instant,
    last_tray: std::time::Instant,
}

/// The mini strip: a small window over the other windows.
struct Strip {
    window: Arc<Window>,
    surface: wgpu::Surface<'static>,
    view: StripView,
}

impl App {
    fn configure(&mut self, size: PhysicalSize<u32>) {
        let (Some(surface), Some(window)) = (&self.surface, &self.window) else {
            return;
        };
        if size.width == 0 || size.height == 0 {
            return; // minimized
        }
        let ctx = RenderContext::shared().expect("the scene has a context");
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format: TARGET_FORMAT,
            color_space: wgpu::SurfaceColorSpace::Auto,
            width: size.width,
            height: size.height,
            present_mode: wgpu::PresentMode::AutoVsync,
            desired_maximum_frame_latency: 2,
            alpha_mode: wgpu::CompositeAlphaMode::Auto,
            view_formats: Vec::new(),
        };
        surface.configure(&ctx.device, &config);
        self.config = Some(config);
        let scale = window.scale_factor() as f32;
        self.shell
            .scene
            .resize(size.width as f32 / scale, size.height as f32 / scale, scale);
    }

    fn draw(&mut self) {
        let (Some(surface), Some(_)) = (&self.surface, &self.config) else {
            return;
        };
        self.shell.tick(true);
        self.last_background = std::time::Instant::now();
        self.last_draw = self.last_background;
        let ctx = RenderContext::shared().expect("the scene has a context");
        let frame = match surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            // Hidden, or the display was late: no picture this time.
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
            // The surface changed under us (a new size, a new display): set it up again and draw the next time.
            _ => {
                if let Some(window) = &self.window {
                    let size = window.inner_size();
                    self.configure(size);
                }
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = ctx.device.create_command_encoder(&Default::default());
        self.shell.scene.render(&mut encoder, &view);
        ctx.queue.submit([encoder.finish()]);
        ctx.queue.present(frame);
    }

    fn key(&mut self, key: &WinitKey) -> Option<Key> {
        let control = self.modifiers.control_key() || self.modifiers.super_key();
        let shift = self.modifiers.shift_key();
        Some(match key {
            WinitKey::Named(NamedKey::Space) if !control => Key::Freeze,
            WinitKey::Named(NamedKey::Escape) => Key::Escape,
            WinitKey::Named(NamedKey::Enter) => Key::Pin,
            WinitKey::Named(NamedKey::ArrowLeft) => Key::CursorDown { octave: shift },
            WinitKey::Named(NamedKey::ArrowRight) => Key::CursorUp { octave: shift },
            WinitKey::Named(NamedKey::F11) => Key::FullScreen,
            WinitKey::Character(text) => match (control, text.to_lowercase().as_str()) {
                (true, "1") => Key::Preset(LayoutPreset::Essential),
                (true, "2") => Key::Preset(LayoutPreset::Spectrum),
                (true, "3") => Key::Preset(LayoutPreset::Metering),
                (true, "6") => Key::Preset(LayoutPreset::Waterfall),
                (true, "4") => Key::TogglePlacement,
                (true, "5") => Key::ToggleTimeline,
                (false, "r") => Key::Reset,
                (false, "f") => Key::FullScreen,
                _ => return None,
            },
            _ => return None,
        })
    }

    fn act(&mut self, action: Action) {
        match action {
            Action::OpenMenu(target) => {
                // The menu opens after this event (`about_to_wait`): a menu runs its own event loop, and that
                // must not start inside the mouse event of the window.
                let entries = self.shell.scene.menu(target);
                if let (Some(window), Some(at)) = (self.window.clone(), self.pointer) {
                    self.pending_menu = Some((window, entries, at));
                }
                return;
            }
            Action::OpenWindow => {
                self.show_window();
                return;
            }
            Action::Quit => {
                self.quit = true;
                return;
            }
            Action::ImportCurve => {
                let mut dialog = rfd::FileDialog::new()
                    .set_title("Import a headphone curve")
                    .add_filter("Frequency response (CSV, text)", &["csv", "txt"]);
                if let Some(window) = &self.window {
                    dialog = dialog.set_parent(window.as_ref());
                }
                if let Some(path) = dialog.pick_file() {
                    self.import(&path);
                }
                return;
            }
            Action::CheckUpdates => {
                self.updater.check(false);
                return;
            }
            Action::InstallUpdate => {
                self.updater.install();
                return;
            }
            Action::OpenCurvesFolder => {
                let folder = self.shell.model.curves_folder().to_path_buf();
                // The folder is made at the first import: make it now, so that there is something to open.
                let _ = std::fs::create_dir_all(&folder);
                let program = if cfg!(windows) { "explorer" } else { "open" };
                let _ = std::process::Command::new(program).arg(&folder).spawn();
                return;
            }
            _ => {}
        }
        if self.shell.act(action) {
            if let Some(window) = &self.window {
                let next = match window.fullscreen() {
                    Some(_) => None,
                    None => Some(Fullscreen::Borderless(None)),
                };
                window.set_fullscreen(next);
            }
        }
    }

    /// Imports a curve file. A file that is no curve gets a message, and nothing changes.
    fn import(&mut self, path: &std::path::Path) {
        if let Err(text) = self.shell.import(path) {
            rfd::MessageDialog::new()
                .set_level(rfd::MessageLevel::Warning)
                .set_title("Joseon")
                .set_description(text)
                .show();
        }
    }

    /// The updater: the check the program makes by itself, the status for the window, and the new start.
    fn run_updater(&mut self) {
        use joseon_app::update::Status;
        // 5 s after the start, and then once a day. Never for a demo run.
        let due = match self.last_update_check {
            None => self.started.elapsed() >= std::time::Duration::from_secs(5),
            Some(last) => last.elapsed() >= std::time::Duration::from_secs(24 * 3600),
        };
        if due && self.uses_tray && self.shell.scene.settings().check_updates_automatically {
            self.last_update_check = Some(std::time::Instant::now());
            self.updater.check(true);
        }
        let status = self.updater.status();
        if status == self.update_status {
            return;
        }
        self.update_status = status.clone();
        self.shell.update_banner = match &status {
            Status::Available(release) => Some(format!(
                "Joseon {} is there. Open the settings menu (the button with three lines) to update.",
                release.version
            )),
            Status::Downloading(version) => Some(format!("Fetching Joseon {version}\u{2026}")),
            Status::Failed(why) => Some(format!("Update failed: {why}")),
            _ => None,
        };
        self.shell.ticks = 0;
        self.shell.scene.set_update(status.clone());
        // The new program is in place: start it, and end this one. The new one waits for this one to go.
        if matches!(status, Status::Ready(_)) {
            if joseon_app::update::relaunch() {
                self.quit = true;
            } else {
                self.shell.update_banner = Some("The update is in place. Start Joseon again to use it.".to_string());
            }
        }
    }

    /// One picture of the main window and of the strip, from the timer that runs while a menu is open.
    #[cfg(windows)]
    fn draw_under_menu(&mut self) {
        if self.main_is_visible() {
            self.draw();
        }
        if let Some(strip) = &mut self.strip {
            strip.draw();
        }
    }

    fn show_window(&mut self) {
        if let Some(window) = &self.window {
            window.set_visible(true);
            window.set_minimized(false);
            window.focus_window();
            window.request_redraw();
        }
    }

    fn main_is_visible(&self) -> bool {
        self.window
            .as_ref()
            .is_some_and(|w| w.is_visible().unwrap_or(true) && !w.is_minimized().unwrap_or(false))
    }

    /// Shows a native menu in `window` at `at` (points). The chosen item arrives as a menu event.
    fn open_menu(&mut self, window: &Window, entries: Vec<Entry>, at: [f32; 2]) {
        self.commands.clear();
        let menu = muda::Menu::new();
        build_menu(&entries, "p", &mut self.commands, &mut |item| {
            let _ = menu.append(item);
        });
        let position = muda::dpi::Position::Logical(muda::dpi::LogicalPosition::new(at[0] as f64, at[1] as f64));
        {
            // A menu runs its own event loop, and the window gets no pictures from winit under it. A timer of
            // Windows calls into this program all the same: it draws the pictures while the menu is open.
            #[cfg(windows)]
            let _draws = menu_draw::start(self);
            show_menu(window, &menu, Some(position));
        }
        // The button that opened the menu went up inside the menu.
        let released = self.shell.scene.pointer_released();
        self.act(released);
        self.run_menu_events();
        // The menu ran its own event loop, and the pictures stood still under it. Start them again: the
        // surface is set up anew, and both windows are asked for a picture.
        if let Some(window) = self.window.clone() {
            self.configure(window.inner_size());
            window.request_redraw();
        }
        if let Some(strip) = &self.strip {
            strip.window.request_redraw();
        }
    }

    fn run_menu_events(&mut self) {
        while let Ok(event) = muda::MenuEvent::receiver().try_recv() {
            let id = event.id.0.as_str();
            let index = |prefix: &str| id.strip_prefix(prefix).and_then(|n| n.parse::<usize>().ok());
            let command = match (index("p"), index("t")) {
                (Some(i), _) => self.commands.get(i).copied(),
                (_, Some(i)) => self.tray_commands.get(i).copied(),
                _ => None,
            };
            if let Some(command) = command {
                let action = self.shell.scene.run(command);
                self.act(action);
            }
        }
    }

    /// The tray icon: made once, its menu built again when what it shows changed, its picture and its tooltip
    /// brought up to date a few times per second.
    fn update_tray(&mut self) {
        if !self.uses_tray {
            return;
        }
        let settings = self.shell.scene.settings().clone();
        let frozen = self.shell.model.is_frozen();
        let key = format!(
            "{} {:?} {} {frozen}",
            settings.show_mini_strip, settings.mini_graph_color, settings.show_now_playing
        );
        if self.tray.is_none() {
            self.tray = tray_icon::TrayIconBuilder::new().with_tooltip("Joseon").build().ok();
            self.tray_key.clear();
        }
        let Some(tray) = &self.tray else { return };
        if key != self.tray_key {
            self.tray_key = key;
            self.tray_commands.clear();
            let menu = muda::Menu::new();
            build_menu(
                &menu::tray_menu(&settings, frozen),
                "t",
                &mut self.tray_commands,
                &mut |item| {
                    let _ = menu.append(item);
                },
            );
            tray.set_menu(Some(Box::new(menu)));
        }
        if self.last_tray.elapsed() < std::time::Duration::from_millis(100) {
            return;
        }
        self.last_tray = std::time::Instant::now();
        let frame = self.shell.model.frame();
        let tilt = settings.tilt_db_per_octave as f32;
        if let Some((w, h, rgba)) = self.tray_picture.icon(&frame, settings.mini_graph_color.rgba(), tilt) {
            if let Ok(icon) = tray_icon::Icon::from_rgba(rgba, w, h) {
                let _ = tray.set_icon(Some(icon));
            }
            let header = &self.shell.header;
            let _ = tray.set_tooltip(Some(mini::tooltip(&header.now_playing, &header.headline())));
        }
    }

    fn run_tray_events(&mut self) {
        while let Ok(event) = tray_icon::TrayIconEvent::receiver().try_recv() {
            use tray_icon::{MouseButton as Button, MouseButtonState as State, TrayIconEvent as Event};
            let open = matches!(
                event,
                Event::Click {
                    button: Button::Left,
                    button_state: State::Up,
                    ..
                } | Event::DoubleClick {
                    button: Button::Left,
                    ..
                }
            );
            if open {
                self.show_window();
            }
        }
    }

    /// Makes the mini strip when the settings ask for it, and takes it away when they do not.
    fn update_strip(&mut self, event_loop: &ActiveEventLoop) {
        let wanted = self.shell.scene.settings().show_mini_strip;
        if !wanted {
            self.strip = None;
            return;
        }
        if self.strip.is_none() {
            self.strip = self.make_strip(event_loop);
            if self.strip.is_none() {
                // No strip on this system: do not try again at every tick.
                let mut settings = self.shell.scene.settings().clone();
                settings.show_mini_strip = false;
                self.shell.scene.set_settings(settings);
                return;
            }
        }
        let settings = self.shell.scene.settings();
        let (color, tilt) = (settings.mini_graph_color.rgba(), settings.tilt_db_per_octave as f32);
        let song = self.shell.header.now_playing.clone();
        let frame = self.shell.model.frame();
        if let Some(strip) = &mut self.strip {
            strip.view.tick(uptime(), &frame, color, tilt, &song);
            strip.window.request_redraw();
        }
    }

    fn make_strip(&self, event_loop: &ActiveEventLoop) -> Option<Strip> {
        let ctx = RenderContext::shared()?;
        #[allow(unused_mut)]
        let mut attributes = Window::default_attributes()
            .with_title("Joseon mini spectrum")
            .with_inner_size(LogicalSize::new(mini::STRIP_SIZE.0 as f64, mini::STRIP_SIZE.1 as f64))
            .with_decorations(false)
            .with_resizable(false)
            .with_window_level(WindowLevel::AlwaysOnTop)
            .with_theme(Some(winit::window::Theme::Dark));
        #[cfg(windows)]
        {
            use winit::platform::windows::WindowAttributesExtWindows;
            attributes = attributes.with_skip_taskbar(true);
        }
        // Where the user left it, or the top right of the main display.
        let saved = self.shell.scene.settings().mini_strip_position;
        let place = saved.map(|p| winit::dpi::Position::Logical(winit::dpi::LogicalPosition::new(p[0], p[1])));
        let corner = || {
            let monitor = event_loop.primary_monitor()?;
            let size = monitor.size().to_logical::<f64>(monitor.scale_factor());
            let x = size.width - mini::STRIP_SIZE.0 as f64 - 24.0;
            Some(winit::dpi::Position::Logical(winit::dpi::LogicalPosition::new(
                x.max(0.0),
                64.0,
            )))
        };
        if let Some(position) = place.or_else(corner) {
            attributes = attributes.with_position(position);
        }
        let window = Arc::new(event_loop.create_window(attributes).ok()?);
        let surface = ctx.instance.create_surface(window.clone()).ok()?;
        let mut strip = Strip {
            view: StripView::new(window.scale_factor() as f32)?,
            window,
            surface,
        };
        strip.configure();
        Some(strip)
    }

    fn update_pointer_shape(&mut self) {
        let shape = self.shell.scene.pointer_shape();
        if shape != self.shape {
            self.shape = shape;
            if let Some(window) = &self.window {
                window.set_cursor(match shape {
                    PointerShape::Arrow => CursorIcon::Default,
                    PointerShape::Crosshair => CursorIcon::Crosshair,
                    PointerShape::Hand => CursorIcon::Pointer,
                    PointerShape::ResizeColumns => CursorIcon::ColResize,
                    PointerShape::ResizeRows => CursorIcon::RowResize,
                });
            }
        }
    }
}

impl ApplicationHandler for App {
    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some((window, entries, at)) = self.pending_menu.take() {
            self.open_menu(&window, entries, at);
        }
        self.run_menu_events();
        self.run_tray_events();
        self.run_updater();
        if self.quit {
            self.shell.save();
            self.shell.model.shutdown();
            self.strip = None;
            self.tray = None;
            event_loop.exit();
            return;
        }
        // The main window ticks with its pictures. While it is hidden, the model ticks here, 30 times per second.
        if !self.main_is_visible() && self.last_background.elapsed() >= std::time::Duration::from_millis(33) {
            self.last_background = std::time::Instant::now();
            self.shell.tick(false);
        }
        // The pictures of the main window follow one another (every picture asks for the next). When that
        // chain breaks, the window would stand still for good: draw here, and start the chain again.
        if self.main_is_visible() && self.last_draw.elapsed() >= std::time::Duration::from_millis(250) {
            if !self.stalled {
                joseon_app::log::line("the main window got no picture for 250 ms: drawn from the timer");
            }
            self.stalled = true;
            self.draw();
            if let Some(window) = &self.window {
                window.request_redraw();
            }
        }
        self.update_tray();
        self.update_strip(event_loop);
        // The tray icon and the strip move with no event from the system: wake up for them.
        let next = std::time::Instant::now() + std::time::Duration::from_millis(33);
        event_loop.set_control_flow(ControlFlow::WaitUntil(next));
    }

    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.window.is_some() {
            return;
        }
        let [width, height] = self.shell.scene.settings().window_size;
        let attributes = Window::default_attributes()
            .with_title(joseon_app::single_instance::WINDOW_TITLE)
            // The window is dark: so is its title bar.
            .with_theme(Some(winit::window::Theme::Dark))
            .with_inner_size(LogicalSize::new(width, height))
            .with_min_inner_size(LogicalSize::new(
                layout::MIN_WINDOW.0 as f64,
                layout::MIN_WINDOW.1 as f64,
            ));
        let window = match event_loop.create_window(attributes) {
            Ok(window) => Arc::new(window),
            Err(error) => {
                self.failure = Some(format!("the window could not be made: {error}"));
                event_loop.exit();
                return;
            }
        };
        let ctx = RenderContext::shared().expect("the scene has a context");
        let surface = match ctx.instance.create_surface(window.clone()) {
            Ok(surface) => surface,
            Err(error) => {
                self.failure = Some(format!("the window has no drawing surface: {error}"));
                event_loop.exit();
                return;
            }
        };
        if !surface.get_capabilities(&ctx.adapter).formats.contains(&TARGET_FORMAT) {
            self.failure = Some("the graphics driver has no BGRA surface format".to_string());
            event_loop.exit();
            return;
        }
        let size = window.inner_size();
        self.window = Some(window.clone());
        self.surface = Some(surface);
        self.configure(size);
        window.request_redraw();
    }

    fn window_event(&mut self, _event_loop: &ActiveEventLoop, id: WindowId, event: WindowEvent) {
        if self.strip.as_ref().is_some_and(|strip| strip.window.id() == id) {
            self.strip_event(event);
            return;
        }
        match event {
            WindowEvent::CloseRequested => {
                self.shell.save();
                // With a tray icon the program lives on there: the window only hides.
                match (&self.window, self.tray.is_some()) {
                    (Some(window), true) => window.set_visible(false),
                    _ => self.quit = true,
                }
            }
            // A curve file dropped on the window is imported.
            WindowEvent::DroppedFile(path) => self.import(&path),
            WindowEvent::Resized(size) => {
                self.configure(size);
                if let Some(window) = self.window.clone() {
                    // The size the user gave the window is a setting. Full screen is not.
                    if window.fullscreen().is_none() && !window.is_maximized() {
                        let logical = size.to_logical::<f64>(window.scale_factor());
                        let mut settings = self.shell.scene.settings().clone();
                        settings.window_size = [logical.width, logical.height];
                        self.shell.scene.set_settings(settings.validated());
                        self.shell.model.settings = self.shell.scene.settings().clone();
                    }
                    window.request_redraw();
                }
            }
            WindowEvent::ScaleFactorChanged { .. } => {
                if let Some(window) = self.window.clone() {
                    self.configure(window.inner_size());
                }
            }
            WindowEvent::RedrawRequested => {
                self.draw();
                // The next picture: the surface waits for the display, so this is one draw per refresh.
                if let Some(window) = &self.window {
                    if window.is_visible().unwrap_or(true) && !window.is_minimized().unwrap_or(false) {
                        window.request_redraw();
                    }
                }
            }
            WindowEvent::Occluded(false) => {
                if let Some(window) = &self.window {
                    window.request_redraw();
                }
            }
            WindowEvent::ModifiersChanged(modifiers) => self.modifiers = modifiers.state(),
            WindowEvent::CursorMoved { position, .. } => {
                let scale = self.window.as_ref().map_or(1.0, |w| w.scale_factor());
                let p = position.to_logical::<f32>(scale);
                self.pointer = Some([p.x, p.y]);
                let action = self.shell.scene.pointer_moved(Some([p.x, p.y]));
                self.act(action);
                self.update_pointer_shape();
            }
            WindowEvent::CursorLeft { .. } => {
                self.pointer = None;
                let action = self.shell.scene.pointer_moved(None);
                self.act(action);
                self.update_pointer_shape();
            }
            WindowEvent::MouseInput {
                state,
                button: MouseButton::Left,
                ..
            } => {
                let action = match state {
                    ElementState::Pressed => self.shell.scene.pointer_pressed(),
                    ElementState::Released => self.shell.scene.pointer_released(),
                };
                self.act(action);
                self.update_pointer_shape();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let action = self.shell.scene.pointer_context();
                self.act(action);
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let points = match delta {
                    winit::event::MouseScrollDelta::LineDelta(_, y) => y * 8.0,
                    winit::event::MouseScrollDelta::PixelDelta(p) => p.y as f32,
                };
                let action = self.shell.scene.scrolled(points);
                self.act(action);
            }
            WindowEvent::PinchGesture { delta, .. } => {
                let action = self.shell.scene.pinched(1.0 + delta as f32);
                self.act(action);
            }
            WindowEvent::KeyboardInput { event, .. } if event.state == ElementState::Pressed => {
                if let Some(key) = self.key(&event.logical_key) {
                    let action = self.shell.scene.key(key);
                    self.act(action);
                }
            }
            _ => {}
        }
    }
}

/// Pictures while a menu is open (see `App::open_menu`).
#[cfg(windows)]
mod menu_draw {
    use super::App;
    use std::cell::Cell;
    use windows::Win32::Foundation::HWND;
    use windows::Win32::UI::WindowsAndMessaging::{KillTimer, SetTimer};

    thread_local! {
        /// The program whose menu is open. Null while none is.
        static APP: Cell<*mut App> = const { Cell::new(std::ptr::null_mut()) };
    }

    unsafe extern "system" fn tick(_window: HWND, _message: u32, _id: usize, _time: u32) {
        let app = APP.with(Cell::get);
        if !app.is_null() {
            // SAFETY: the pointer is set only for the time of the menu call in `open_menu`, on this thread.
            // `open_menu` waits inside that call and touches the program again only when it returns.
            unsafe { (*app).draw_under_menu() };
        }
    }

    /// Ends the pictures when it is dropped.
    pub struct Draws(usize);

    pub fn start(app: &mut App) -> Draws {
        APP.with(|cell| cell.set(app));
        // About 60 pictures per second; the display paces them.
        Draws(unsafe { SetTimer(None, 0, 15, Some(tick)) })
    }

    impl Drop for Draws {
        fn drop(&mut self) {
            APP.with(|cell| cell.set(std::ptr::null_mut()));
            unsafe {
                let _ = KillTimer(None, self.0);
            }
        }
    }
}

/// Turns menu data into native menu items. `commands` gets the command of every item; the id of an item is
/// `prefix` and its index there.
fn build_menu(
    entries: &[Entry],
    prefix: &str,
    commands: &mut Vec<Command>,
    add: &mut dyn FnMut(&dyn muda::IsMenuItem),
) {
    // Windows shows the text after a tab at the right edge, as the key of the item. Elsewhere it has no place.
    let text = |title: &str| {
        if cfg!(windows) {
            title.to_string()
        } else {
            title.split('\t').next().unwrap_or(title).to_string()
        }
    };
    for entry in entries {
        match entry {
            Entry::Header(title) => add(&muda::MenuItem::new(title.to_uppercase(), false, None)),
            Entry::Separator => add(&muda::PredefinedMenuItem::separator()),
            Entry::Note(text) => add(&muda::MenuItem::new(text, false, None)),
            Entry::Item {
                title,
                checked,
                command,
            } => {
                let id = format!("{prefix}{}", commands.len());
                commands.push(*command);
                match checked {
                    Some(on) => add(&muda::CheckMenuItem::with_id(id, text(title), true, *on, None)),
                    None => add(&muda::MenuItem::with_id(id, text(title), true, None)),
                }
            }
            Entry::Group { title, entries } => {
                let group = muda::Submenu::new(*title, true);
                build_menu(entries, prefix, commands, &mut |item| {
                    let _ = group.append(item);
                });
                add(&group);
            }
        }
    }
}

impl Strip {
    fn configure(&mut self) {
        let Some(ctx) = RenderContext::shared() else { return };
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return;
        }
        self.view.set_scale(self.window.scale_factor() as f32);
        self.surface.configure(
            &ctx.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: TARGET_FORMAT,
                color_space: wgpu::SurfaceColorSpace::Auto,
                width: size.width,
                height: size.height,
                present_mode: wgpu::PresentMode::AutoVsync,
                desired_maximum_frame_latency: 2,
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
                view_formats: Vec::new(),
            },
        );
    }

    fn draw(&mut self) {
        let Some(ctx) = RenderContext::shared() else { return };
        let frame = match self.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) | wgpu::CurrentSurfaceTexture::Suboptimal(frame) => frame,
            wgpu::CurrentSurfaceTexture::Timeout | wgpu::CurrentSurfaceTexture::Occluded => return,
            _ => {
                self.configure();
                return;
            }
        };
        let view = frame.texture.create_view(&Default::default());
        let mut encoder = ctx.device.create_command_encoder(&Default::default());
        self.view.render(&mut encoder, &view);
        ctx.queue.submit([encoder.finish()]);
        ctx.queue.present(frame);
    }
}

impl App {
    /// The events of the mini strip: it drags with the main button, and the other button opens the tray menu.
    fn strip_event(&mut self, event: WindowEvent) {
        let Some(strip) = &mut self.strip else { return };
        match event {
            WindowEvent::RedrawRequested => strip.draw(),
            WindowEvent::Resized(_) | WindowEvent::ScaleFactorChanged { .. } => strip.configure(),
            WindowEvent::CursorMoved { position, .. } => {
                let p = position.to_logical::<f32>(strip.window.scale_factor());
                self.strip_pointer = [p.x, p.y];
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                let _ = strip.window.drag_window();
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Right,
                ..
            } => {
                let window = strip.window.clone();
                let entries = menu::tray_menu(self.shell.scene.settings(), self.shell.model.is_frozen());
                self.pending_menu = Some((window, entries, self.strip_pointer));
            }
            // Where the user leaves the strip is a setting.
            WindowEvent::Moved(position) => {
                let p = position.to_logical::<f64>(strip.window.scale_factor());
                let mut settings = self.shell.scene.settings().clone();
                settings.mini_strip_position = Some([p.x, p.y]);
                self.shell.scene.set_settings(settings);
                self.shell.model.settings = self.shell.scene.settings().clone();
            }
            WindowEvent::CloseRequested => {
                let action = self.shell.scene.run(Command::ToggleMiniStrip);
                self.act(action);
            }
            _ => {}
        }
    }
}

/// Shows `menu` in `window` and waits until it closes (Windows) or returns at once (macOS).
fn show_menu(window: &Window, menu: &muda::Menu, position: Option<muda::dpi::Position>) {
    use muda::ContextMenu;
    use winit::raw_window_handle::{HasWindowHandle, RawWindowHandle};
    let Ok(handle) = window.window_handle() else { return };
    match handle.as_raw() {
        #[cfg(windows)]
        RawWindowHandle::Win32(handle) => unsafe {
            menu.show_context_menu_for_hwnd(handle.hwnd.get(), position);
        },
        #[cfg(target_os = "macos")]
        RawWindowHandle::AppKit(handle) => unsafe {
            menu.show_context_menu_for_nsview(handle.ns_view.as_ptr() as _, position);
        },
        _ => {}
    }
}

fn main() {
    let args = arguments();
    let path = settings_path();
    // A second start only shows the window of the Joseon that runs (it may wait in the tray).
    if args.snapshot.is_none() && !args.demo && !joseon_app::single_instance::claim(args.after_update) {
        return;
    }
    if args.snapshot.is_none() {
        joseon_app::log::start(path.with_file_name("joseon.log"));
        joseon_app::log::line(&format!("Joseon {} starts", env!("CARGO_PKG_VERSION")));
        if let Some(v) = joseon_capture::default_output_volume() {
            joseon_app::log::line(&format!(
                "output device: {}, volume {:?} dB, hardware volume control: {:?}",
                v.device_name, v.volume_db, v.hardware_volume
            ));
        }
    }
    // An update leaves the old files beside the new ones: take them away.
    if let Some(folder) = std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(std::path::Path::to_path_buf))
    {
        if args.snapshot.is_none() {
            joseon_app::update::remove_old_files(&folder);
        }
    }
    let mut settings = AppSettings::load(&path);
    settings.demo_mode |= args.demo;
    let [width, height] = settings.window_size;
    let Some(mut scene) = Scene::new(
        settings.clone(),
        width as f32,
        height as f32,
        if args.snapshot.is_some() { 2.0 } else { 1.0 },
    ) else {
        eprintln!("joseon: no graphics device. Joseon needs Direct3D 12 or Vulkan.");
        std::process::exit(1);
    };
    scene.set_utc_offset_seconds(local_time::utc_offset_seconds());
    let mut model = AppModel::new(settings);
    model.start();
    let shell = Shell {
        model,
        scene,
        path,
        saves: args.snapshot.is_none() && !args.demo,
        ticks: 0,
        header: HeaderState::default(),
        update_banner: None,
    };
    if let Some(folder) = &args.snapshot {
        std::process::exit(snapshot(shell, folder, args.seconds));
    }
    let event_loop = match EventLoop::new() {
        Ok(event_loop) => event_loop,
        Err(error) => {
            eprintln!("joseon: no event loop: {error}");
            std::process::exit(1);
        }
    };
    let mut app = App {
        shell,
        window: None,
        surface: None,
        config: None,
        modifiers: ModifiersState::empty(),
        shape: PointerShape::Arrow,
        failure: None,
        pointer: None,
        commands: Vec::new(),
        uses_tray: !args.demo,
        tray: None,
        tray_picture: TrayPicture::new(),
        tray_commands: Vec::new(),
        tray_key: String::new(),
        strip: None,
        strip_pointer: [0.0; 2],
        quit: false,
        pending_menu: None,
        updater: joseon_app::update::Updater::default(),
        update_status: joseon_app::update::Status::default(),
        last_update_check: None,
        started: std::time::Instant::now(),
        stalled: false,
        last_draw: std::time::Instant::now(),
        last_background: std::time::Instant::now(),
        last_tray: std::time::Instant::now(),
    };
    let result = event_loop.run_app(&mut app);
    if let Some(failure) = &app.failure {
        eprintln!("joseon: {failure}");
        std::process::exit(1);
    }
    if let Err(error) = result {
        eprintln!("joseon: {error}");
        std::process::exit(1);
    }
}
