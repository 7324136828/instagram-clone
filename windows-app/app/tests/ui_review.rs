//! Headless tests of what the review of the first release found: each test drives the whole app
//! the way the reviewer's probe did and checks what the user should see.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use common::*;
use egui::accesskit::Role;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::Harness;
use instagram_core::autoview::AutoViewRequest;
use instagram_windows_app::app::{self, App, WindowGeometry};
use instagram_windows_app::loader::SizeState;
use instagram_windows_app::state::{Action, ConfirmKind, View};
use instagram_windows_app::{placement, shell, theme};

/// A PNG of one colour.
fn png(path: &Path, width: u32, height: u32, rgb: [u8; 3]) {
    image::RgbImage::from_pixel(width, height, image::Rgb(rgb)).save(path).expect("a picture can be written");
}

/// A folder of pictures named `names` (80 x 60 each) inside the profile.
fn pictures(profile: &Path, folder: &str, names: &[&str]) -> PathBuf {
    let path = profile.join(folder);
    std::fs::create_dir_all(&path).expect("a folder");
    for (index, name) in names.iter().enumerate() {
        png(&path.join(name), 80, 60, [40 * index as u8, 100, 160]);
    }
    path
}

/// A dropped file as the windowing layer reports it.
#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &Path {
        &self.0
    }
    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|error| error.to_string())
    }
}

/// The file names of the posts on screen.
fn names(app: &App) -> Vec<String> {
    app.state.posts().iter().map(|post| post.file_name().unwrap_or_default()).collect()
}

/// The file name of the picture in the viewer.
fn in_viewer(app: &App) -> Option<String> {
    app.state.viewer.as_ref().and_then(|viewer| app.state.post(&viewer.key)).and_then(|post| post.file_name())
}

/// Runs frames until `done` holds (the app need not be idle: an open may wait for a dialog).
fn step_until(harness: &mut Harness<'_, App>, what: &str, done: impl Fn(&App) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(30);
    while !done(harness.state()) {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        harness.step();
        std::thread::sleep(Duration::from_millis(5));
    }
}

// --- LOGIC-1 ---------------------------------------------------------------------------------

#[test]
fn a_picture_replaced_on_disk_is_shown_as_it_is_now_when_it_is_opened_again() {
    let profile = profile("review-replaced");
    let folder = pictures(&profile, "A", &[]);
    let file = folder.join("p.png");
    png(&file, 64, 64, [220, 0, 0]);
    let mut harness = harness(app_with(&profile, &folder));
    settle_idle(&mut harness);
    let key = harness.state().state.posts()[0].key.clone();
    let path = harness.state().state.posts()[0].media[0].path.clone();
    harness.state_mut().dispatch(Action::OpenViewer(key));
    settle_idle(&mut harness);
    assert_eq!(harness.state().services.loader.peek_size(&path), SizeState::Known((64, 64)));

    harness.state_mut().dispatch(Action::ClosePictures);
    settle_idle(&mut harness);
    png(&file, 300, 100, [0, 0, 220]);
    harness.state_mut().dispatch(Action::OpenPaths(vec![folder.clone()]));
    settle(&mut harness, "the folder to open again", |app| app.state.collection.is_some());
    let key = harness.state().state.posts()[0].key.clone();
    harness.state_mut().dispatch(Action::OpenViewer(key));
    settle_idle(&mut harness);
    assert_eq!(harness.state().services.loader.peek_size(&path), SizeState::Known((300, 100)), "the file as it is now");
    let shown = harness.state_mut().services.loader.any_rendition(&path).expect("the picture is on screen");
    assert_eq!(shown.source_size, (300, 100));
}

// --- LOGIC-3 ---------------------------------------------------------------------------------

#[test]
fn one_picture_dropped_or_opened_while_pictures_are_open_is_the_one_on_show() {
    let profile = profile("review-drop-one");
    let a = pictures(&profile, "A", &["a1.png", "a2.png", "a3.png"]);
    let b = pictures(&profile, "B", &["b1.png", "b2.png"]);
    let mut harness = harness(app_with(&profile, &a));
    settle_idle(&mut harness);
    let first = harness.state().state.posts()[0].key.clone();
    harness.state_mut().dispatch(Action::OpenViewer(first));
    settle_idle(&mut harness);

    // A picture of another folder, dropped on the open viewer: added, and on show.
    harness.input_mut().dropped_files.push(Arc::new(Dropped(b.join("b1.png"))));
    settle(&mut harness, "the dropped picture", |app| app.state.posts().len() == 4);
    assert_eq!(in_viewer(harness.state()).as_deref(), Some("b1.png"));
    assert_eq!(harness.state().state.window_title(), "b1.png - Instagram Windows App");

    // A picture that is already open: the viewer moves to it.
    harness.input_mut().dropped_files.push(Arc::new(Dropped(a.join("a3.png"))));
    settle(&mut harness, "the viewer to move", |app| in_viewer(app).as_deref() == Some("a3.png"));
    assert_eq!(names(harness.state()).len(), 4);

    // With the viewer closed, one picture opened (dialog or drop) opens the viewer on it.
    harness.state_mut().dispatch(Action::CloseViewer);
    settle_idle(&mut harness);
    harness.state_mut().dispatch(Action::OpenPaths(vec![b.join("b2.png")]));
    settle(&mut harness, "the viewer on the opened picture", |app| app.state.posts().len() == 5);
    assert_eq!(in_viewer(harness.state()).as_deref(), Some("b2.png"));

    // Several pictures added while the viewer is closed: the feed, no viewer.
    harness.state_mut().dispatch(Action::CloseViewer);
    settle_idle(&mut harness);
    let c = pictures(&profile, "C", &["c1.png", "c2.png"]);
    harness.state_mut().dispatch(Action::OpenPaths(vec![c.join("c1.png"), c.join("c2.png")]));
    settle(&mut harness, "two more pictures", |app| app.state.posts().len() == 7);
    assert_eq!(harness.state().state.viewer, None);
}

// --- LOGIC-4 ---------------------------------------------------------------------------------

#[test]
fn pictures_closed_while_more_were_being_added_stay_closed() {
    let profile = profile("review-close-during-add");
    let a = pictures(&profile, "A", &["a1.png"]);
    let b = pictures(&profile, "B", &["b1.png"]);
    let mut harness = harness(app_with(&profile, &a));
    settle_idle(&mut harness);
    harness.state_mut().dispatch(Action::OpenPaths(vec![b.join("b1.png")]));
    harness.state_mut().dispatch(Action::ClosePictures);
    std::thread::sleep(Duration::from_millis(200));
    settle_idle(&mut harness);
    assert!(harness.state().state.collection.is_none(), "closed pictures do not come back: {:?}", names(harness.state()));
}

#[test]
fn a_finished_open_waits_for_the_editor_and_its_typed_text() {
    let profile = profile("review-open-waits");
    let a = pictures(&profile, "A", &["a1.png", "a2.png"]);
    let b = pictures(&profile, "B", &["b1.png"]);
    let mut harness = harness(app_with(&profile, &a));
    settle_idle(&mut harness);
    // The folder B is being read when the user starts to describe a picture of A.
    harness.state_mut().dispatch(Action::OpenPaths(vec![b.clone()]));
    let key = harness.state().state.posts()[0].key.clone();
    harness.state_mut().dispatch(Action::OpenEditor(key));
    harness.state_mut().dispatch(Action::EditorInput("A long description I have been typing".to_owned()));
    step_until(&mut harness, "the open to finish", |app| app.state.held_open.is_some());
    for _ in 0..5 {
        harness.step();
    }
    let editor = harness.state().state.editor.as_ref().expect("the editor stays open");
    assert_eq!(editor.text, "A long description I have been typing");
    assert_eq!(names(harness.state()), ["a1.png", "a2.png"]);

    // Once the editor is closed, the new pictures come.
    harness.state_mut().dispatch(Action::CloseEditor);
    settle(&mut harness, "the waiting pictures", |app| app.state.held_open.is_none());
    assert_eq!(names(harness.state()), ["b1.png"]);
    assert_eq!(harness.state().state.opening_since, None);
}

#[test]
fn a_description_saved_while_a_folder_was_read_stays_on_screen_and_in_the_download() {
    let profile = profile("review-saved-during-open");
    let a = pictures(&profile, "A", &["a1.png"]);
    let mut harness = harness(app_with(&profile, &a));
    settle_idle(&mut harness);
    // The same folder is opened again; meanwhile a description is saved.
    harness.state_mut().dispatch(Action::OpenPaths(vec![a.clone()]));
    let key = harness.state().state.posts()[0].key.clone();
    harness.state_mut().dispatch(Action::OpenEditor(key.clone()));
    harness.state_mut().dispatch(Action::EditorInput("Saved while the folder was read".to_owned()));
    harness.state_mut().dispatch(Action::SaveDescription);
    settle(&mut harness, "the folder to arrive", |app| app.state.opening_since.is_none());
    assert_eq!(harness.state().state.post(&key).map(|post| post.caption.as_str()), Some("Saved while the folder was read"));
}

// --- LOGIC-5 ---------------------------------------------------------------------------------

#[test]
fn auto_view_of_the_command_line_starts_at_the_picked_picture() {
    let profile = profile("review-autoview-picked");
    let a = pictures(&profile, "A", &["p1.png", "p2.png", "p3.png", "p4.png", "p5.png"]);
    let mut config = config(&profile);
    config.paths = vec![a.join("p4.png")];
    // The longest time per picture: the test's clock moves with every frame, and the show must
    // not move on before it is looked at.
    config.auto_view = Some(AutoViewRequest { interval_sec: Some(3600), wait_for_videos: None, loop_posts: Some(false), shuffle: Some(false) });
    let mut harness = harness(App::new(config));
    step_until(&mut harness, "auto view", |app| app.state.auto_view.is_some());
    let shown = harness.state().state.auto_view_post().map(|(post, _)| post.file_name().unwrap_or_default());
    assert_eq!(shown.as_deref(), Some("p4.png"));
    assert_eq!(harness.state().state.window_title(), "p4.png - Instagram Windows App");
}

// --- LOGIC-6 ---------------------------------------------------------------------------------

#[test]
fn a_picture_opened_in_the_viewer_again_starts_fitted() {
    let profile = profile("review-zoom-reset");
    let a = profile.join("A");
    std::fs::create_dir_all(&a).expect("a folder");
    png(&a.join("a1.png"), 1600, 1200, [200, 0, 0]);
    png(&a.join("a2.png"), 1600, 1200, [0, 200, 0]);
    let mut harness = harness(app_with(&profile, &a));
    settle_idle(&mut harness);
    let first = harness.state().state.posts()[0].key.clone();
    harness.state_mut().dispatch(Action::OpenViewer(first.clone()));
    settle_idle(&mut harness);
    assert!(harness.state().viewer.zoom().is_fit());
    harness.state_mut().viewer.request_zoom(Some(3.0));
    harness.state_mut().touch();
    settle_idle(&mut harness);
    assert_eq!(harness.state().viewer.zoom_percent(), Some(300));
    harness.key_press(egui::Key::Escape);
    settle_idle(&mut harness);
    assert!(harness.state().state.viewer.is_none());
    harness.state_mut().dispatch(Action::OpenViewer(first));
    settle_idle(&mut harness);
    assert!(harness.state().viewer.zoom().is_fit(), "{:?}", harness.state().viewer.zoom_percent());
}

// --- LOGIC-8 and ROB-4 -----------------------------------------------------------------------

#[test]
fn files_that_cannot_be_shown_are_explained_in_a_toast() {
    let profile = profile("review-unusable");
    let a = pictures(&profile, "A", &["a1.png", "a2.png"]);
    std::fs::write(a.join("scan.tif"), b"a scan in a format this viewer does not read").expect("a file");
    std::fs::write(a.join("letter.docx"), b"a letter").expect("a file");
    let mut harness = harness(app_with(&profile, &a));
    settle_idle(&mut harness);
    harness.state_mut().dispatch(Action::ShowExplore);
    settle_idle(&mut harness);

    // Added files of which none can be shown: the reason of the first, and how many more.
    harness.state_mut().dispatch(Action::OpenPaths(vec![a.join("scan.tif"), a.join("letter.docx")]));
    settle(&mut harness, "the files to be looked at", |app| !app.state.toasts.is_empty());
    let toast = toasts(&harness).pop().expect("a toast");
    assert!(toast.starts_with("\"scan.tif\" is not a supported image"), "{toast}");
    assert!(toast.ends_with(" 1 more file could not be opened."), "{toast}");
    assert_eq!(harness.state().state.view, View::Explore, "adding leaves the view alone");

    // One picked file that is no picture, with pictures beside it: said at once.
    harness.state_mut().dispatch(Action::ClosePictures);
    settle_idle(&mut harness);
    harness.state_mut().dispatch(Action::OpenPaths(vec![a.join("scan.tif")]));
    settle(&mut harness, "the folder", |app| app.state.collection.is_some() && !app.state.toasts.is_empty());
    let toast = toasts(&harness).pop().expect("a toast");
    assert!(toast.starts_with("\"scan.tif\" is not a supported image"), "{toast}");
}

// --- LOGIC-10 --------------------------------------------------------------------------------

#[test]
fn the_notice_about_damaged_descriptions_stays_until_it_is_closed() {
    let profile = profile("review-store-notice");
    let data = profile.join("instagram-windows-app").join("data");
    std::fs::create_dir_all(&data).expect("the data folder");
    std::fs::write(data.join("descriptions.json"), b"{\"format\": \"instagram-windows-app-descriptions\", \"descriptions\": {").expect("a damaged file");
    let a = pictures(&profile, "A", &["a1.png"]);
    let mut config = config(&profile);
    config.paths = vec![a];
    let mut harness = harness(App::new(config));
    settle(&mut harness, "the pictures", |app| app.state.collection.is_some());
    // After the pictures have opened (which closes the dialogs of the posts), it is there.
    let notice = harness.state().state.confirm.as_ref().map(|confirm| confirm.kind.clone());
    let Some(ConfirmKind::StoreNotice(message)) = notice else {
        panic!("the notice is shown as a dialog, not a toast: {notice:?}");
    };
    assert!(message.contains("could not be read. The file was kept as \"descriptions.damaged-"), "{message}");
    assert_eq!(harness.query_all_by_label_contains("The file was kept as").count(), 1);
    harness.get_by_role_and_label(Role::Button, "OK").click();
    settle_idle(&mut harness);
    assert_eq!(harness.state().state.confirm, None);
}

// --- WIN-1 and WIN-8 -------------------------------------------------------------------------

/// eframe's storage, as far as the window geometry goes.
#[derive(Default)]
struct Storage(std::collections::HashMap<String, String>);

impl eframe::Storage for Storage {
    fn get_string(&self, key: &str) -> Option<String> {
        self.0.get(key).cloned()
    }
    fn set_string(&mut self, key: &str, value: String) {
        self.0.insert(key.to_owned(), value);
    }
    fn remove_string(&mut self, key: &str) {
        self.0.remove(key);
    }
    fn flush(&mut self) {}
}

#[test]
fn a_window_closed_while_minimised_or_in_fullscreen_comes_back_normal() {
    const NORMAL: &str = "(normal_geometry:(outer_position_pixels:Some((x:333.0,y:77.0)),scale_factor:1.5,inner_size_points:(x:1400.0,y:950.0)),fullscreen:false,maximized:false)";
    const MINIMISED: &str = "(normal_geometry:(outer_position_pixels:Some((x:-32000.0,y:-32000.0)),scale_factor:1.5,inner_size_points:(x:0.0,y:0.0)),fullscreen:false,maximized:false)";
    let profile = profile("review-window-geometry");
    let mut app = App::new(config(&profile));
    app.window_geometry = WindowGeometry { keeps: true, last_usable: None };
    let mut storage = Storage::default();
    // A normal close: kept as eframe took it.
    storage.0.insert(placement::STORAGE_WINDOW_KEY.to_owned(), NORMAL.to_owned());
    eframe::App::save(&mut app, &mut storage);
    assert_eq!(storage.get_string_or_empty(), NORMAL);
    // Closed while minimised: the last usable geometry stays.
    storage.0.insert(placement::STORAGE_WINDOW_KEY.to_owned(), MINIMISED.to_owned());
    eframe::App::save(&mut app, &mut storage);
    assert_eq!(storage.get_string_or_empty(), NORMAL);
    // Closed in fullscreen: the geometry, without fullscreen.
    storage.0.insert(placement::STORAGE_WINDOW_KEY.to_owned(), NORMAL.replace("fullscreen:false", "fullscreen:true"));
    eframe::App::save(&mut app, &mut storage);
    assert_eq!(storage.get_string_or_empty(), NORMAL);
    // Minimised without anything usable before: nothing is kept, the next start is a first one.
    let mut fresh = App::new(config(&profile));
    fresh.window_geometry = WindowGeometry { keeps: true, last_usable: None };
    storage.0.insert(placement::STORAGE_WINDOW_KEY.to_owned(), MINIMISED.to_owned());
    eframe::App::save(&mut fresh, &mut storage);
    assert_eq!(eframe::Storage::get_string(&storage, placement::STORAGE_WINDOW_KEY), None);
    // A run that does not keep the window (screenshots, tests) leaves the storage alone.
    let mut quiet = App::new(config(&profile));
    storage.0.insert(placement::STORAGE_WINDOW_KEY.to_owned(), MINIMISED.to_owned());
    eframe::App::save(&mut quiet, &mut storage);
    assert_eq!(storage.get_string_or_empty(), MINIMISED);
}

impl Storage {
    fn get_string_or_empty(&self) -> String {
        eframe::Storage::get_string(self, placement::STORAGE_WINDOW_KEY).unwrap_or_default()
    }
}

// --- WIN-3 -----------------------------------------------------------------------------------

#[test]
fn a_closing_window_leaves_the_screen_at_once() {
    let profile = profile("review-close-hides");
    let mut harness = harness(App::new(config(&profile)));
    settle_idle(&mut harness);
    harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().events.push(egui::ViewportEvent::Close);
    harness.step();
    let commands = &harness.output().viewport_output[&egui::ViewportId::ROOT].commands;
    assert!(commands.contains(&egui::ViewportCommand::Visible(false)), "{commands:?}");
    // What is left after that waits at most half a second for the renderer to let go.
    const { assert!(app::EXIT_GUARD_MS <= 500) };
}

// --- WIN-6 -----------------------------------------------------------------------------------

#[test]
fn the_editor_starts_from_what_another_window_saved() {
    let profile = profile("review-two-windows");
    let a = pictures(&profile, "A", &["a1.png", "a2.png"]);
    let mut first = app_with(&profile, &a);
    let mut second = app_with(&profile, &a);
    let key = first.state.posts()[0].key.clone();
    first.dispatch(Action::OpenEditor(key.clone()));
    first.dispatch(Action::EditorInput("from the first window".to_owned()));
    first.dispatch(Action::SaveDescription);
    assert_eq!(first.state.editor, None);
    // The second window knew nothing of it; its editor shows the stored text.
    second.dispatch(Action::OpenEditor(key.clone()));
    assert_eq!(second.state.editor.as_ref().map(|editor| editor.text.as_str()), Some("from the first window"));
    assert_eq!(second.state.post(&key).map(|post| post.caption.as_str()), Some("from the first window"));
}

// --- VIS-1 and VIS-3 -------------------------------------------------------------------------

/// A picture with a description of `lines` lines, saved through the editor.
fn described_picture(profile: &Path, lines: usize) -> App {
    let a = pictures(profile, "A", &["a1.png"]);
    let mut app = app_with(profile, &a);
    let key = app.state.posts()[0].key.clone();
    app.dispatch(Action::OpenEditor(key));
    let text: Vec<String> = (1..=lines).map(|line| format!("Line {line} of a long description")).collect();
    app.dispatch(Action::EditorInput(text.join("\n")));
    app.dispatch(Action::SaveDescription);
    assert_eq!(app.state.editor, None, "saved");
    app
}

#[test]
fn the_scroll_bar_of_the_viewer_text_stays_inside_the_card() {
    let profile = profile("review-viewer-scrollbar");
    let app = described_picture(&profile, 60);
    let mut harness = harness_sized(app, (1440.0, 900.0));
    settle_idle(&mut harness);
    let key = harness.state().state.posts()[0].key.clone();
    harness.state_mut().dispatch(Action::OpenViewer(key));
    settle_idle(&mut harness);
    let (used, area) = harness.state().viewer.text_column().expect("the text column was shown");
    assert!(area.contains_rect(used), "the text column {used:?} leaves its area {area:?}");
}

#[test]
fn the_edit_dialog_with_a_long_text_fits_a_low_window() {
    let profile = profile("review-edit-low-window");
    let app = described_picture(&profile, 60);
    let mut harness = harness_sized(app, (1600.0, 480.0));
    settle_idle(&mut harness);
    let key = harness.state().state.posts()[0].key.clone();
    harness.state_mut().dispatch(Action::OpenEditor(key));
    settle_idle(&mut harness);
    let cancel = harness.get_all_by_role_and_label(Role::Button, "Cancel").max_by(|a, b| a.rect().bottom().total_cmp(&b.rect().bottom())).expect("Cancel");
    assert!(cancel.rect().bottom() <= 480.0 - 20.0 + 0.5, "Cancel ends at {:?}", cancel.rect());
}

// --- VIS-5 -----------------------------------------------------------------------------------

#[test]
fn tab_runs_through_a_card_header_left_to_right_and_never_rests_on_the_picture() {
    let profile = profile("review-tab-order");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);
    let mut seen: Vec<(Role, String)> = Vec::new();
    for _ in 0..24 {
        harness.key_press(egui::Key::Tab);
        harness.run_steps(2);
        if let Some(node) = harness.query_by(|node| node.is_focused()) {
            let node = node.accesskit_node();
            seen.push((node.role(), node.label().unwrap_or_default()));
        }
    }
    assert!(seen.iter().all(|(role, _)| *role != Role::Image), "the picture is no tab stop: {seen:?}");
    // The first card: the time link, the link icon, then More options, as in the clone.
    let external = seen.iter().position(|(_, label)| label == "View on Instagram").expect("the link icon is reached");
    let more = seen.iter().position(|(_, label)| label == "More options").expect("More options is reached");
    assert!(external < more, "{seen:?}");
    let taken_at = harness.state().state.posts()[0].taken_at.expect("the fixture has a time");
    let time = instagram_core::text::relative_time_label(instagram_core::text::now_ms(), taken_at);
    assert!(external > 0 && seen[external - 1].1 == time, "the time link {time:?} comes first: {seen:?}");
}

// --- VIS-6 -----------------------------------------------------------------------------------

#[test]
fn paths_in_the_about_sheet_break_between_folder_names() {
    let profile = profile("review-about-paths");
    let mut harness = harness(App::new(config(&profile)));
    settle_idle(&mut harness);
    // The default data folder of a user called "user", in the About sheet's 352-point column.
    let path = r"C:\Users\user\AppData\Roaming\instagram-windows-app\data";
    let wrapped = shell::wrapped_path(&harness.ctx, path, &theme::monospace(12.0), 352.0);
    assert_eq!(wrapped.replace('\n', ""), path, "nothing is lost");
    let lines: Vec<&str> = wrapped.lines().collect();
    assert!(lines.len() >= 2, "the path needs two lines: {wrapped}");
    for line in &lines[..lines.len() - 1] {
        assert!(line.ends_with('\\'), "a line ends after a backslash: {wrapped:?}");
    }
    assert_eq!(lines.last(), Some(&"data"), "{wrapped:?}");
    // A name longer than a line stands on its own line; the layout breaks it there.
    let long = format!(r"C:\{}\b", "x".repeat(80));
    let wrapped = shell::wrapped_path(&harness.ctx, &long, &theme::monospace(12.0), 352.0);
    let name = format!("{}\\", "x".repeat(80));
    assert_eq!(wrapped.lines().collect::<Vec<_>>(), [r"C:\", name.as_str(), "b"]);
}

// --- ROB-8 -----------------------------------------------------------------------------------

#[test]
fn a_data_folder_that_cannot_be_created_is_a_settings_error() {
    let profile = profile("review-data-folder");
    let blocker = profile.join("a-file.txt");
    std::fs::write(&blocker, b"in the way").expect("a file");
    let paths = instagram_core::settings::AppPaths::new(profile.join("settings.json"), blocker.clone());
    let message = app::prepare_data_folder(&paths).expect_err("a file is in the way");
    assert!(message.starts_with(&format!("Invalid data_directory in settings at {}: the folder {} cannot be created: ", profile.join("settings.json").display(), blocker.display())), "{message}");
    let fine = instagram_core::settings::AppPaths::new(profile.join("settings.json"), profile.join("data"));
    assert_eq!(app::prepare_data_folder(&fine), Ok(()));
    assert!(profile.join("data").is_dir());
}

// --- WIN-10 ----------------------------------------------------------------------------------

#[test]
fn the_exe_declares_what_a_windows_desktop_program_declares() {
    let manifest = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("assets").join("app.manifest")).expect("the manifest");
    for needed in [
        "<dpiAwareness xmlns=\"http://schemas.microsoft.com/SMI/2016/WindowsSettings\">PerMonitorV2</dpiAwareness>",
        "name=\"Microsoft.Windows.Common-Controls\" version=\"6.0.0.0\"",
        "<supportedOS Id=\"{8e0f7a12-bfb3-4fe8-b9a5-48fd50a15a9a}\"/>",
        "<longPathAware xmlns=\"http://schemas.microsoft.com/SMI/2016/WindowsSettings\">true</longPathAware>",
        "<requestedExecutionLevel level=\"asInvoker\" uiAccess=\"false\"/>",
    ] {
        assert!(manifest.contains(needed), "{needed}");
    }
    let build_script = std::fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("build.rs")).expect("build.rs");
    assert!(build_script.contains(".set_manifest_file(\"assets/app.manifest\")"));
}
