//! Entry point: reads the command line and the settings, starts the log and opens the window.
//!
//! A release build is a GUI-subsystem program without a console, so a fatal settings error is
//! shown in a native message box as well as on stderr before the process ends with code 1, and
//! `--help` and `--version` borrow the console they were started from (`platform.rs`).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::ffi::OsString;

use instagram_core::settings::{self, EnvLookup, ProcessEnv};
use instagram_windows_app::app::{App, AppConfig, WindowGeometry};
use instagram_windows_app::cli::{self, CommandLine, ProfileEnv};
use instagram_windows_app::harness::{self, DEFAULT_WATCHDOG_SECONDS, EXIT_USAGE};
use instagram_windows_app::web::WebConfig;
use instagram_windows_app::{logging, placement, platform, rendering, shortcuts, strings, theme, web_assets};

/// Exit code of a fatal settings error.
const EXIT_SETTINGS: i32 = 1;
/// Flat colours must come out exactly as the palette says: no dithering.
const DITHERING: bool = false;

/// Writes `message` to stderr and shows `dialog` (the same, or a layout of it for the box) in a
/// native message box.
fn report_fatal(message: &str, dialog: Option<String>) {
    eprintln!("[{}] ERROR: {message}", settings::APP_NAME);
    if let Some(dialog) = dialog {
        rfd::MessageDialog::new().set_level(rfd::MessageLevel::Error).set_title(strings::APP_TITLE).set_description(dialog).set_buttons(rfd::MessageButtons::Ok).show();
    }
}

fn main() {
    instagram_windows_app::app::mark_process_start();
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    // --help and --version neither read the settings nor open a window.
    if let Some(information) = shortcuts::information(&arguments) {
        platform::print_information(&information);
        return;
    }
    let CommandLine { paths, auto_view, harness: harness_options, web: web_overrides } = match cli::parse(&arguments) {
        Ok(command_line) => command_line,
        Err(error) => {
            // An automated run must never wait for someone to close a message box.
            let automated = arguments.iter().any(|argument| argument == "--screenshot" || argument == "--scenario" || argument == "--profile-dir");
            report_fatal(&error.message, (!automated).then(|| error.message.clone()));
            std::process::exit(EXIT_USAGE);
        }
    };
    let takes_screenshot = harness_options.screenshot.is_some();

    // Settings: the user's profile, or the folder of `--profile-dir` in its place.
    let profile_env = harness_options.profile_dir.clone().map(|profile_dir| ProfileEnv { profile_dir });
    let env: &dyn EnvLookup = match &profile_env {
        Some(profile) => profile,
        None => &ProcessEnv,
    };
    let settings = match settings::load_or_create(env) {
        Ok(settings) => settings,
        Err(error) => {
            let file = settings::settings_path(env).ok();
            report_fatal(&error.message, (!takes_screenshot).then(|| strings::fatal_dialog_text(&error.message, file.as_deref())));
            std::process::exit(EXIT_SETTINGS);
        }
    };

    // The settings code never creates the data folder. Without it no description can be saved,
    // and the log, state.json and window.ron are lost: that is a settings error like any other.
    if let Err(message) = instagram_windows_app::app::prepare_data_folder(&settings.paths) {
        report_fatal(&message, (!takes_screenshot).then(|| strings::fatal_dialog_text(&message, Some(&settings.paths.settings_file))));
        std::process::exit(EXIT_SETTINGS);
    }
    if let Err(message) = logging::init(&settings.paths.log_file, cfg!(debug_assertions)) {
        eprintln!("[{}] {message}", settings::APP_NAME);
    }
    log::info!("{} {} started; settings {}; data folder {}.", strings::APP_TITLE, env!("CARGO_PKG_VERSION"), settings.paths.settings_file.display(), settings.paths.data_dir.display());

    let window_file = settings.paths.window_file.clone();
    // A screenshot run decides its own size and writes nothing back.
    let persist_window = !takes_screenshot && harness_options.size.is_none();
    // Where the window opens: the stored geometry when it is usable, cut to the screen, or the
    // default size centred on the screen (placement.rs). window.ron is corrected before eframe
    // reads it.
    let mut plan = placement::plan_start(None, None, theme::DEFAULT_WINDOW_SIZE, theme::MIN_WINDOW_SIZE);
    if persist_window {
        let stored = placement::read_stored(&window_file);
        let near = stored.as_deref().and_then(placement::SavedWindow::parse).and_then(|saved| saved.position_px());
        plan = placement::plan_start(stored.as_deref(), platform::work_area(near), theme::DEFAULT_WINDOW_SIZE, theme::MIN_WINDOW_SIZE);
        placement::apply_to_file(&window_file, stored.as_deref(), &plan);
    }

    let web_settings = settings.web.clone();
    let mut config = AppConfig::load(settings);
    // A screenshot shows the app as it is, without a server, unless the command line names an
    // address or a port for it (screenshots of the Browser section).
    if !takes_screenshot || web_overrides.names_an_address() {
        config.web = Some(WebConfig::new(web_settings, web_overrides, web_assets::WEB_FILES));
    }
    config.paths = paths;
    config.auto_view = auto_view;
    config.theme_override = harness_options.theme;
    config.video_caps = harness_options.video_adapter;
    // A screenshot shows the final state of everything, not a moment of an animation.
    config.animations = !takes_screenshot || harness_options.animate;
    config.harness = harness_options.clone();
    let start_theme = config.theme_override.unwrap_or(config.remembered.theme);
    let window_geometry = WindowGeometry { keeps: persist_window, last_usable: plan.restored.and_then(|saved| saved.to_text()) };

    let icon = match eframe::icon_data::from_png_bytes(include_bytes!("../assets/icon.png")) {
        Ok(icon) => Some(icon),
        Err(error) => {
            log::warn!("The window icon could not be read: {error}");
            None
        }
    };
    let mut viewport = egui::ViewportBuilder::default()
        .with_title(strings::APP_TITLE)
        .with_app_id(settings::APP_NAME)
        .with_inner_size(harness_options.window_size().map(|(width, height)| [width, height]).unwrap_or(plan.size))
        .with_min_inner_size(theme::MIN_WINDOW_SIZE)
        .with_drag_and_drop(true);
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }
    if persist_window {
        if let Some(position) = plan.position {
            viewport = viewport.with_position(position);
        }
        if plan.maximized && plan.restored.is_none() {
            viewport = viewport.with_maximized(true);
        }
    }
    if takes_screenshot {
        // The window stays hidden for the whole run (see harness.rs).
        viewport = viewport.with_visible(false);
        harness::start_watchdog(harness_options.watchdog_seconds.unwrap_or(DEFAULT_WATCHDOG_SECONDS));
    } else if let Some(seconds) = harness_options.watchdog_seconds {
        // A scripted run with a visible window (an end-to-end test) ends at the latest then.
        harness::start_watchdog(seconds);
    }
    let options = eframe::NativeOptions {
        viewport,
        // The window geometry is kept in the data folder, never in eframe's default location.
        persistence_path: Some(window_file),
        persist_window,
        dithering: DITHERING,
        ..Default::default()
    };
    // A renderer made for software rendering must fit the buffers eframe made for these options.
    let multisampling = options.multisampling;

    let result = eframe::run_native(
        settings::APP_NAME,
        options,
        Box::new(move |creation| {
            // Without a graphics card the textures are filtered in the shader (rendering.rs).
            if creation.wgpu_render_state.as_ref().is_some_and(|render_state| rendering::adapt_renderer(render_state, multisampling, DITHERING)) {
                log::info!("Software rendering: textures are filtered in the shader.");
            }
            let fonts = theme::setup_context(&creation.egui_ctx, start_theme);
            log::info!("Fonts: {}.", fonts.loaded.join(", "));
            for missing in &fonts.missing {
                log::warn!("A font is missing, its text uses another face: {missing}");
            }
            let mut app = App::new(config);
            app.window_geometry = window_geometry;
            Ok(Box::new(app))
        }),
    );
    if let Err(error) = result {
        let message = format!("The window could not be opened: {error}");
        log::error!("{message}");
        log::logger().flush();
        report_fatal(&message, (!takes_screenshot).then(|| message.clone()));
        std::process::exit(EXIT_SETTINGS);
    }
    log::logger().flush();
}
