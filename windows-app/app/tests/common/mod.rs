//! What the headless tests of the interface share (egui_kittest, no window).
//!
//! They build the real `App` with a profile folder of their own under the system's temporary
//! folder, so the user's settings and descriptions are never read or written, and drive it
//! through the accessible names of its widgets, the way a screen reader sees it. The profile
//! folder goes when the test ends (`profile`).

// Every test file uses its own part of these helpers.
#![allow(dead_code)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use instagram_core::catalog::{self, OpenOptions, OpenRequest};
use instagram_core::settings;
use instagram_core::store::Theme;
pub use instagram_core::test_folder::TestFolder;
use instagram_windows_app::app::{App, AppConfig};
use instagram_windows_app::theme;

/// Size of the test window: wide enough for the sidebar with labels.
pub const WINDOW: (f32, f32) = (1400.0, 820.0);

/// The fixtures of the core crate.
pub fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("core").join("tests").join("fixtures").join(name)
}

/// A fresh profile folder for one test, `%TEMP%\instagram-windows-app-tests\<test>-<process id>`.
/// It is removed when the guard is dropped, so the test keeps it for as long as its app runs
/// (`instagram_core::test_folder`).
pub fn profile(test: &str) -> TestFolder {
    TestFolder::new(test).expect("profile folder")
}

/// The configuration of an app whose files live in `profile`.
pub fn config(profile: &Path) -> AppConfig {
    let env: HashMap<String, String> = [("APPDATA".to_owned(), profile.display().to_string()), ("USERNAME".to_owned(), "tester".to_owned())].into();
    let settings = settings::load_or_create(&env).expect("settings in the test profile");
    assert!(settings.paths.settings_file.starts_with(profile), "the test must not touch the real profile");
    assert!(settings.paths.archive_cache_dir.starts_with(profile), "nor the real archive cache");
    let mut config = AppConfig::load(settings);
    // The final state at once: no animation to wait for.
    config.animations = false;
    config
}

/// An app with the collection at `path` on screen, opened synchronously.
pub fn app_with(profile: &Path, path: &Path) -> App {
    let mut app = App::new(config(profile));
    let request = OpenRequest { paths: vec![path.to_path_buf()], options: OpenOptions { default_username: app.services.default_username.clone() } };
    let opened = catalog::open_paths(&request, &app.services.store, &app.services.context).expect("the fixture opens");
    app.show_opened(opened);
    app
}

/// A copy of the loose pictures of the core's fixtures in a folder of the test's own, so that a
/// test can open a single file of it without the other tests' files beside it.
pub fn copy_of_loose(profile: &Path) -> PathBuf {
    let folder = profile.join("pictures");
    std::fs::create_dir_all(&folder).expect("pictures folder");
    for entry in std::fs::read_dir(fixture("loose")).expect("the loose fixture") {
        let entry = entry.expect("a fixture file");
        std::fs::copy(entry.path(), folder.join(entry.file_name())).expect("copy a fixture file");
    }
    folder
}

/// A harness that runs both halves of a frame of `app` in a window of `size`.
pub fn harness_sized(app: App, size: (f32, f32)) -> Harness<'static, App> {
    Harness::builder()
        .with_size(egui::vec2(size.0, size.1))
        .with_pixels_per_point(1.0)
        // Emoji and CJK come from the system font fallback, which a test context does not have.
        .allow_missing_glyphs()
        .with_context_setup(|ctx| {
            theme::setup_context(ctx, Theme::Light);
        })
        .build_ui_state(
            |ui, app: &mut App| {
                app.frame_logic(ui.ctx());
                app.frame_ui(ui);
            },
            app,
        )
}

/// A harness that runs both halves of a frame of `app`.
pub fn harness(app: App) -> Harness<'static, App> {
    harness_sized(app, WINDOW)
}

/// Runs frames until `done` holds, the app is idle and one more frame has shown the result.
pub fn settle(harness: &mut Harness<'_, App>, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        harness.step();
        if harness.state().is_idle() && done(harness.state()) {
            harness.step();
            if harness.state().is_idle() {
                return;
            }
        }
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Runs frames until the app is idle.
pub fn settle_idle(harness: &mut Harness<'_, App>) {
    settle(harness, "the app to become idle", |_| true);
}

/// How many widgets are named exactly `label`.
pub fn count(harness: &Harness<'_, App>, label: &str) -> usize {
    harness.query_all_by_label(label).count()
}

/// How many widgets have a name that contains `text`.
pub fn count_containing(harness: &Harness<'_, App>, text: &str) -> usize {
    harness.query_all_by_label_contains(text).count()
}

/// The texts of the toasts on screen, oldest first.
pub fn toasts(harness: &Harness<'_, App>) -> Vec<String> {
    harness.state().state.toasts.visible().iter().map(|toast| toast.message.clone()).collect()
}
