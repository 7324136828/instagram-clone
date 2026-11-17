//! Headless tests of what completes the app: downloads (exports), auto view, "Close pictures",
//! About, what survives a restart, and one whole journey from opening pictures to the archive
//! that carries the descriptions typed on the way.
//!
//! No native dialog is shown: the test answers the Save dialog the app asks for
//! (`Downloader::answer_dialog`). Auto view runs on a clock the test moves by hand
//! (`AppState::auto_view_clock`), so a show of minutes is checked in milliseconds.

mod common;

use std::io::Read as _;
use std::path::{Path, PathBuf};

use common::{app_with, config, copy_of_loose, count, count_containing, fixture, harness, profile, settle, settle_idle, toasts, TestFolder};
use egui::accesskit::Role;
use egui::Key;
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, Node};
use instagram_core::autoview::{AutoViewRequest, Settings};
use instagram_core::store::UiState;
use instagram_windows_app::app::App;
use instagram_windows_app::loader::SizeState;
use instagram_windows_app::state::{Action, ConfirmKind, DownloadPhase, PlayerCommand};

/// Width of the sheets (post options, auto view setup, questions, About).
const SHEET_WIDTH: f32 = 400.0;
/// Width of the "Edit description" dialog.
const EDITOR_WIDTH: f32 = 520.0;

/// The row named `label` of the dialog of `width` on screen: a button exactly as wide as the
/// dialog.
fn dialog_row<'a>(harness: &'a Harness<'_, App>, width: f32, label: &str) -> Node<'a> {
    harness
        .query_all_by_role(Role::Button)
        .find(|node| (node.rect().width() - width).abs() < 0.5 && node.accesskit_node().label().as_deref() == Some(label))
        .unwrap_or_else(|| panic!("the dialog has no row \"{label}\""))
}

/// How many pieces of text on screen read exactly `text` (a dialog carries its title as its own
/// name too; this counts the text only).
fn texts(harness: &Harness<'_, App>, text: &str) -> usize {
    harness.query_all_by_role_and_label(Role::Label, text).count()
}

/// An empty folder of the test's own for the archives.
fn output_folder(profile: &Path) -> PathBuf {
    let folder = profile.join("downloads");
    std::fs::create_dir_all(&folder).expect("the output folder");
    folder
}

/// The names of the files in `folder`, sorted.
fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(folder).expect("the folder").filter_map(|entry| entry.ok()).map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// The text of `post.json` inside the archive `file`, and the names of all its entries.
fn archive_manifest(file: &Path) -> (String, Vec<String>) {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(file).expect("the archive opens")).expect("a zip archive");
    let names: Vec<String> = archive.file_names().map(|name| name.expect("an entry name").into_owned()).collect();
    let mut manifest = String::new();
    archive.by_name("post.json").expect("post.json is in the archive").read_to_string(&mut manifest).expect("post.json is text");
    (manifest, names)
}

/// A folder with `count` copies of a small picture, for a download that takes a moment.
fn many_pictures(profile: &Path, count: usize) -> PathBuf {
    let folder = profile.join("many");
    std::fs::create_dir_all(&folder).expect("the folder of pictures");
    let source = std::fs::read(fixture("loose").join("IMG_2.jpg")).expect("the fixture picture");
    for index in 0..count {
        std::fs::write(folder.join(format!("picture_{index:04}.jpg")), &source).expect("a copy of the picture");
    }
    folder
}

/// Starts "Download all" into `file` and runs the one frame that takes the answer of the Save
/// dialog over: the archive is being written when this returns.
fn start_download_all(harness: &mut Harness<'_, App>, file: &Path) {
    harness.state_mut().dispatch(Action::DownloadAll);
    assert!(harness.state_mut().services.downloader.answer_dialog(Some(file.to_path_buf())), "the Save dialog was asked for");
    harness.step();
}

// --- Download -------------------------------------------------------------------------------------

#[test]
fn download_all_writes_the_archive_shows_its_progress_and_says_so() {
    let profile = profile("download-all");
    let out = output_folder(&profile);
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);

    // The Save dialog is asked for with the web project's archive name.
    harness.state_mut().dispatch(Action::DownloadAll);
    let request = harness.state().services.downloader.pending_dialog().cloned().expect("a Save dialog is asked for");
    assert_eq!(request.file_name, "instagram-export.zip");
    assert_eq!(request.title, "Download all");
    assert_eq!(harness.state().state.download.as_ref().map(|download| download.phase.clone()), Some(DownloadPhase::Choosing));
    harness.step();
    // While it is being chosen, "Download all" cannot be used a second time.
    assert_eq!(count(&harness, "Download all"), 1);

    // The user picks a file: the banner shows how far the download is, and cancels on a click.
    let file = out.join("instagram-export.zip");
    assert!(harness.state_mut().services.downloader.answer_dialog(Some(file.clone())));
    harness.step();
    assert!(harness.state().state.download_running());
    let progress = harness.state().state.download.as_ref().and_then(|download| download.banner_progress()).expect("the banner has figures");
    assert_eq!(progress.total, 6);
    assert_eq!(count_containing(&harness, "of 6 · Cancel"), 1, "the banner button shows the progress");
    assert_eq!(count(&harness, "Download all"), 0);

    settle_idle(&mut harness);
    assert_eq!(toasts(&harness), ["Downloaded 6 posts as instagram-export.zip."]);
    assert!(harness.state().state.download.is_none());
    assert_eq!(count(&harness, "Download all"), 1, "the button is back");
    // Exactly the archive is in the folder: no temporary file is left.
    assert_eq!(names_in(&out), ["instagram-export.zip"]);
    let (manifest, names) = archive_manifest(&file);
    assert!(manifest.contains("\"format\": \"instagram-export\""), "{manifest}");
    assert!(manifest.contains("FXTbasic001") && manifest.contains("FXTbasic006"));
    assert_eq!(names.last().map(String::as_str), Some("post.json"), "post.json is added last");
    // The folder is remembered for the next Save dialog.
    assert_eq!(harness.state().state.remembered.last_export_folder.as_deref(), Some(out.as_path()));
    assert_eq!(UiState::load(&harness.state().services.paths.state_file).last_export_folder.as_deref(), Some(out.as_path()));
}

#[test]
fn download_post_from_the_sheet_takes_the_chosen_name() {
    let profile = profile("download-post");
    let out = output_folder(&profile);
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);

    harness.get_all_by_role_and_label(Role::Button, "More options").min_by(|a, b| a.rect().top().total_cmp(&b.rect().top())).expect("a card").click();
    harness.run_steps(3);
    dialog_row(&harness, SHEET_WIDTH, "Download post").click();
    harness.run_steps(3);
    assert!(harness.state().state.sheet.is_none(), "choosing the row closes the sheet");
    let request = harness.state().services.downloader.pending_dialog().cloned().expect("a Save dialog is asked for");
    assert_eq!(request.file_name, "instagram-export-FXTbasic001.zip");
    assert_eq!(request.title, "Download post");

    // The name typed in the dialog decides the name of the archive.
    let file = out.join("Bakery post.zip");
    assert!(harness.state_mut().services.downloader.answer_dialog(Some(file.clone())));
    harness.step();
    assert_eq!(toasts(&harness), ["Preparing the download…"]);
    // A single post shows no figures in the banner; "Download all" just cannot be used.
    assert_eq!(count_containing(&harness, "· Cancel"), 0);

    settle_idle(&mut harness);
    assert_eq!(toasts(&harness), ["Preparing the download…", "Downloaded 1 post as Bakery post.zip."]);
    assert_eq!(names_in(&out), ["Bakery post.zip"]);
    let (manifest, names) = archive_manifest(&file);
    assert!(manifest.contains("FXTbasic001") && !manifest.contains("FXTbasic002"));
    assert!(names.iter().any(|name| name.starts_with("posts/")), "{names:?}");
}

#[test]
fn only_one_download_runs_and_a_cancelled_save_dialog_is_silent() {
    let profile = profile("download-one");
    let out = output_folder(&profile);
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);
    let key = harness.state().state.posts()[1].key.clone();

    harness.state_mut().dispatch(Action::DownloadAll);
    harness.state_mut().dispatch(Action::DownloadPost(key));
    harness.step();
    assert_eq!(toasts(&harness), ["A download is already being prepared."]);

    // Cancelling the dialog ends the download without a word and without a file.
    assert!(harness.state_mut().services.downloader.answer_dialog(None));
    settle_idle(&mut harness);
    assert!(harness.state().state.download.is_none());
    assert_eq!(toasts(&harness), ["A download is already being prepared."]);
    assert!(names_in(&out).is_empty());

    // A path that cannot name an archive is refused with the web project's sentence.
    harness.state_mut().dispatch(Action::DownloadAll);
    assert!(harness.state_mut().services.downloader.answer_dialog(Some(out.join("missing-folder").join("x.zip"))));
    settle_idle(&mut harness);
    assert!(toasts(&harness).contains(&"The download could not be prepared.".to_owned()));
    assert!(harness.state().state.download.is_none());
}

#[test]
fn cancelling_a_download_leaves_no_file() {
    let profile = profile("download-cancel");
    let out = output_folder(&profile);
    let pictures = many_pictures(&profile, 1500);
    let mut harness = harness(app_with(&profile, &pictures));
    settle_idle(&mut harness);

    start_download_all(&mut harness, &out.join("instagram-export.zip"));
    assert!(harness.state().state.download_running());
    // The banner button is the cancel button now.
    let cancel = harness.query_all_by_label_contains("· Cancel").next().expect("the banner shows the progress");
    cancel.click();
    settle(&mut harness, "the download to stop", |app| app.state.download.is_none());
    assert_eq!(toasts(&harness), ["Download cancelled."]);
    assert!(names_in(&out).is_empty(), "nothing is left behind: {:?}", names_in(&out));
}

#[test]
fn a_split_download_asks_before_it_replaces_earlier_parts() {
    let profile = profile("download-parts");
    let out = output_folder(&profile);
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);
    // More posts than one archive holds: the download may be split into parts.
    {
        let app = harness.state_mut();
        let collection = app.state.collection.as_mut().expect("a collection");
        let template = collection.posts[0].clone();
        for index in 0..instagram_core::export::EXPORT_PART_POSTS {
            let mut post = template.clone();
            post.key = instagram_core::model::PostKey::for_export_post(Path::new("C:\\export\\post.json"), &format!("extra{index}"));
            post.id = format!("extra{index}");
            collection.posts.push(post);
        }
        app.state.collection_serial += 1;
        app.touch();
    }
    for name in ["trip-part1.zip", "trip-part2.zip", "trip.zip", "unrelated.zip"] {
        std::fs::write(out.join(name), b"earlier").expect("a file of an earlier download");
    }

    harness.state_mut().dispatch(Action::DownloadAll);
    assert!(harness.state_mut().services.downloader.answer_dialog(Some(out.join("trip.zip"))));
    harness.run_steps(3);
    let confirm = harness.state().state.confirm.clone().expect("a question is asked");
    assert_eq!(confirm.kind, ConfirmKind::ReplaceParts { count: 2, first: "trip-part1.zip".to_owned() });
    assert_eq!(texts(&harness, "Replace the earlier download?"), 1);
    assert_eq!(count_containing(&harness, "\"trip-part1.zip\" and 1 more part of an earlier download"), 1);
    assert!(!harness.state().services.downloader.is_running(), "nothing is written before the answer");

    // "Cancel" ends the download without touching a file.
    dialog_row(&harness, SHEET_WIDTH, "Cancel").click();
    harness.run_steps(3);
    assert!(harness.state().state.confirm.is_none());
    assert!(harness.state().state.download.is_none());
    assert!(toasts(&harness).is_empty());
    assert_eq!(names_in(&out), ["trip-part1.zip", "trip-part2.zip", "trip.zip", "unrelated.zip"]);
    assert_eq!(std::fs::read(out.join("trip-part1.zip")).expect("the earlier part"), b"earlier");
}

// --- Close pictures -------------------------------------------------------------------------------

#[test]
fn close_pictures_leads_back_to_the_empty_state() {
    let profile = profile("close");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);
    // The More menu has it.
    harness.get_by_role_and_label(Role::Button, "More").click();
    harness.run_steps(3);
    harness.get_by_role_and_label(Role::Button, "Close pictures").click();
    settle_idle(&mut harness);
    assert!(harness.state().state.collection.is_none());
    assert!(harness.state().state.confirm.is_none(), "nothing is asked without a download");
    assert_eq!(count(&harness, "Open pictures to get started"), 1);
    // In the empty state the menu entry cannot be used.
    harness.get_by_role_and_label(Role::Button, "More").click();
    harness.run_steps(3);
    assert!(harness.get_by_role_and_label(Role::Button, "Close pictures").accesskit_node().is_disabled());
    assert!(harness.get_by_role_and_label(Role::Button, "Download all").accesskit_node().is_disabled());
}

#[test]
fn close_pictures_asks_while_a_download_runs_and_cancels_it() {
    let profile = profile("close-download");
    let out = output_folder(&profile);
    let pictures = many_pictures(&profile, 1500);
    let mut harness = harness(app_with(&profile, &pictures));
    settle_idle(&mut harness);

    start_download_all(&mut harness, &out.join("instagram-export.zip"));
    // The banner's "Close".
    harness.get_by_role_and_label(Role::Button, "Close").click();
    harness.run_steps(3);
    assert_eq!(harness.state().state.confirm.as_ref().map(|confirm| confirm.kind.clone()), Some(ConfirmKind::ClosePictures));
    assert_eq!(texts(&harness, "Close pictures?"), 1);
    assert_eq!(count(&harness, "A download is still being prepared. Closing the pictures cancels it."), 1);

    // "Cancel" keeps the pictures and the download.
    dialog_row(&harness, SHEET_WIDTH, "Cancel").click();
    harness.run_steps(3);
    assert!(harness.state().state.confirm.is_none());
    assert!(harness.state().state.collection.is_some());
    assert!(harness.state().state.download_running());

    // The red row closes them; the download stops and leaves nothing behind.
    harness.state_mut().dispatch(Action::ClosePictures);
    harness.run_steps(3);
    dialog_row(&harness, SHEET_WIDTH, "Close pictures").click();
    settle(&mut harness, "the download to stop", |app| app.state.download.is_none());
    assert!(harness.state().state.collection.is_none());
    assert_eq!(count(&harness, "Open pictures to get started"), 1);
    assert_eq!(toasts(&harness), ["Download cancelled."]);
    assert!(names_in(&out).is_empty(), "nothing is left behind: {:?}", names_in(&out));
}

// --- Auto view ------------------------------------------------------------------------------------

/// The seconds field of the setup dialog.
fn seconds_field<'a>(harness: &'a Harness<'_, App>) -> Node<'a> {
    harness.get_by_role_and_label(Role::TextInput, "Time per post")
}

/// Moves the clock of auto view on by `seconds` and runs the frames that follow.
fn advance(harness: &mut Harness<'_, App>, seconds: f64) {
    let app = harness.state_mut();
    let now = app.state.auto_view_clock.expect("the test moves the clock by hand");
    app.state.auto_view_clock = Some(now + seconds);
    harness.run_steps(3);
}

/// Presses `key` and runs the frames that follow.
fn press(harness: &mut Harness<'_, App>, key: Key) {
    harness.key_press(key);
    harness.run_steps(3);
}

/// An app with the export fixture open and auto view started through its setup dialog with
/// `seconds` per picture; the clock of the show is the test's. The profile folder comes first:
/// bound first, it is dropped (and removed) after the harness.
fn started_show(test: &str, seconds: u32, loop_posts: bool) -> (TestFolder, Harness<'static, App>) {
    let profile = profile(test);
    let mut app = app_with(&profile, &fixture("export-basic"));
    app.state.auto_view_clock = Some(100.0);
    let mut harness = harness(app);
    settle_idle(&mut harness);
    harness.state_mut().dispatch(Action::StartAutoView(None));
    harness.state_mut().dispatch(Action::AutoViewInput(seconds.to_string()));
    harness.state_mut().dispatch(Action::AutoViewLoop(loop_posts));
    harness.state_mut().dispatch(Action::AutoViewBegin);
    settle_idle(&mut harness);
    assert!(harness.state().state.auto_view.is_some(), "the show runs");
    (profile, harness)
}

#[test]
fn the_setup_dialog_checks_the_seconds_and_remembers_the_settings() {
    let profile = profile("autoview-setup");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);

    // The banner button opens the dialog: the remembered 15 seconds, selected, in a focused field.
    harness.get_all_by_role_and_label(Role::Button, "Auto view").max_by(|a, b| a.rect().left().total_cmp(&b.rect().left())).expect("the banner button").click();
    harness.run_steps(4);
    assert_eq!(count(&harness, "Shows the 6 posts one after another and moves on by itself."), 1);
    assert!(seconds_field(&harness).is_focused());
    // The line under "Let videos play to the end" fits one line (no word alone on a second).
    let note = harness.get_by_label("A video longer than the time per post plays to its end.").rect();
    assert!(note.height() < 20.0, "one line: {note:?}");
    assert_eq!(count(&harness, "Start again after the last post"), 1, "the clone's words: a post is a picture or a video");
    assert_eq!(harness.state().state.auto_view_setup.as_ref().map(|setup| setup.text.clone()).as_deref(), Some("15"));

    // Typing replaces the selected text. One second is too short: "Start" says so and the
    // dialog stays; nothing is clamped silently.
    seconds_field(&harness).type_text("1");
    harness.run_steps(2);
    assert_eq!(harness.state().state.auto_view_setup.as_ref().map(|setup| setup.text.clone()).as_deref(), Some("1"));
    dialog_row(&harness, SHEET_WIDTH, "Start").click();
    harness.run_steps(3);
    assert!(harness.state().state.auto_view.is_none());
    assert_eq!(harness.query_all_by_role_and_label(Role::Alert, "Enter a whole number of seconds from 2 to 3600.").count(), 1);
    for wrong in ["", "abc", "3601", "2.5", "15s"] {
        harness.state_mut().dispatch(Action::AutoViewInput(wrong.to_owned()));
        harness.state_mut().dispatch(Action::AutoViewBegin);
        harness.run_steps(2);
        assert!(harness.state().state.auto_view_setup.as_ref().is_some_and(|setup| setup.invalid), "{wrong:?} is refused");
    }

    // A quick choice writes its seconds into the field and clears the error.
    harness.get_by_role_and_label(Role::Button, "30 s").click();
    harness.run_steps(3);
    let setup = harness.state().state.auto_view_setup.clone().expect("the dialog is open");
    assert_eq!((setup.text.as_str(), setup.invalid), ("30", false));
    assert_eq!(harness.query_all_by_role(Role::Alert).count(), 0);

    // The options, then "Start": the show runs and state.json remembers all three settings.
    harness.get_by_role_and_label(Role::CheckBox, "Start again after the last post").click();
    harness.get_by_role_and_label(Role::CheckBox, "Shuffle the order").click();
    harness.run_steps(3);
    dialog_row(&harness, SHEET_WIDTH, "Start").click();
    harness.run_steps(4);
    let expected = Settings { interval_sec: 30, wait_for_videos: true, loop_posts: false, shuffle: true };
    let app = harness.state();
    assert!(app.state.auto_view_setup.is_none());
    assert_eq!(app.state.auto_view.as_ref().map(|run| run.settings), Some(expected));
    assert_eq!(UiState::load(&app.services.paths.state_file).auto_view, expected);
    // Shuffled or not, the show starts at the first post.
    assert_eq!(count(&harness, "1 / 6"), 1);
    drop(harness);

    // The next setup dialog starts from what was remembered; Escape closes it without a show.
    let mut harness = common::harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);
    harness.state_mut().dispatch(Action::StartAutoView(None));
    harness.run_steps(3);
    let setup = harness.state().state.auto_view_setup.clone().expect("the dialog is open");
    assert_eq!((setup.text.as_str(), setup.loop_posts, setup.shuffle), ("30", false, true));
    press(&mut harness, Key::Escape);
    assert!(harness.state().state.auto_view_setup.is_none());
    assert!(harness.state().state.auto_view.is_none());
}

#[test]
fn the_show_moves_on_by_itself_and_obeys_its_keys() {
    let (_profile, mut harness) = started_show("autoview-keys", 5, true);
    assert_eq!(count(&harness, "1 / 6"), 1);
    assert_eq!(count(&harness, "Post 1 of 6, by fixture.sunrise.bakery."), 1, "the status line");

    // Five seconds per picture: still the first at 4.9 s, the second right after.
    advance(&mut harness, 4.9);
    assert_eq!(count(&harness, "1 / 6"), 1);
    advance(&mut harness, 0.2);
    assert_eq!(count(&harness, "2 / 6"), 1);

    // Space pauses: the pill shows, the time stands still however long, and Space goes on with
    // the time that was left.
    advance(&mut harness, 3.0);
    press(&mut harness, Key::Space);
    assert_eq!(count(&harness, "Paused"), 1);
    assert_eq!(count(&harness, "Auto view paused."), 1);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Resume auto view").count(), 1);
    advance(&mut harness, 60.0);
    assert_eq!(count(&harness, "2 / 6"), 1);
    press(&mut harness, Key::Space);
    assert_eq!(count(&harness, "Paused"), 0);
    advance(&mut harness, 1.9);
    assert_eq!(count(&harness, "2 / 6"), 1);
    advance(&mut harness, 0.2);
    assert_eq!(count(&harness, "3 / 6"), 1);

    // The arrows change the post; each starts with its full time.
    press(&mut harness, Key::ArrowRight);
    assert_eq!(count(&harness, "4 / 6"), 1);
    press(&mut harness, Key::ArrowLeft);
    assert_eq!(count(&harness, "3 / 6"), 1);
    advance(&mut harness, 4.9);
    assert_eq!(count(&harness, "3 / 6"), 1);

    // Plus and minus step through the web project's times; only the time is remembered.
    press(&mut harness, Key::Plus);
    assert_eq!(count(&harness, "8 s"), 1);
    assert_eq!(harness.state().state.remembered.auto_view.interval_sec, 8);
    assert_eq!(UiState::load(&harness.state().services.paths.state_file).auto_view.interval_sec, 8);
    press(&mut harness, Key::Minus);
    press(&mut harness, Key::Minus);
    assert_eq!(count(&harness, "3 s"), 1);
    // The shorter time applies to the time already spent: 4.9 of now 3 seconds are over.
    assert_eq!(count(&harness, "4 / 6"), 1);

    // The fifth post is a carousel of two pictures: they share the time, at least two seconds each.
    press(&mut harness, Key::ArrowRight);
    assert_eq!(count(&harness, "5 / 6"), 1);
    assert_eq!(count(&harness, "1/2"), 1);
    advance(&mut harness, 2.1);
    assert_eq!(count(&harness, "5 / 6"), 1);
    assert_eq!(count(&harness, "2/2"), 1);
    advance(&mut harness, 2.1);
    assert_eq!(count(&harness, "6 / 6"), 1);

    // The loop is on: after the last post the first one again.
    advance(&mut harness, 3.1);
    assert_eq!(count(&harness, "1 / 6"), 1);

    // Escape leaves the show.
    press(&mut harness, Key::Escape);
    assert!(harness.state().state.auto_view.is_none());
    assert_eq!(count(&harness, "1 / 6"), 0);
}

#[test]
fn without_the_loop_the_show_ends_and_restart_begins_again() {
    let (_profile, mut harness) = started_show("autoview-end", 2, false);
    harness.state_mut().dispatch(Action::AutoViewCommand(PlayerCommand::TogglePause));
    for _ in 0..5 {
        press(&mut harness, Key::ArrowRight);
    }
    assert_eq!(count(&harness, "6 / 6"), 1);
    // Paused, the last post stays; resumed, its time runs out and the end screen comes.
    advance(&mut harness, 10.0);
    assert_eq!(count(&harness, "End of posts"), 0);
    press(&mut harness, Key::Space);
    advance(&mut harness, 2.1);
    assert_eq!(count(&harness, "End of posts"), 1);
    assert_eq!(count(&harness, "You have seen all 6 posts."), 1);
    assert_eq!(count(&harness, "End of posts."), 1, "the status line");
    assert_eq!(count(&harness, "6 / 6"), 1);
    // Nothing moves from here, and the controls that make no sense cannot be used.
    advance(&mut harness, 30.0);
    assert_eq!(count(&harness, "End of posts"), 1);
    assert!(harness.get_by_role_and_label(Role::Button, "Pause auto view").accesskit_node().is_disabled());
    assert!(harness.get_by_role_and_label(Role::Button, "Next post").accesskit_node().is_disabled());
    assert!(!harness.get_by_role_and_label(Role::Button, "Previous post").accesskit_node().is_disabled());

    // "Previous" leads back to the last post; its time over, the end screen again.
    harness.get_by_role_and_label(Role::Button, "Previous post").click();
    harness.run_steps(3);
    assert_eq!(count(&harness, "End of posts"), 0);
    assert_eq!(count(&harness, "6 / 6"), 1);
    advance(&mut harness, 2.1);
    assert_eq!(count(&harness, "End of posts"), 1);

    // "Restart": the first post, running.
    harness.get_by_role_and_label(Role::Button, "Restart").click();
    harness.run_steps(3);
    assert_eq!(count(&harness, "1 / 6"), 1);
    assert_eq!(count(&harness, "End of posts"), 0);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Pause auto view").count(), 1);
    advance(&mut harness, 2.1);
    assert_eq!(count(&harness, "2 / 6"), 1);

    // "Exit" of the end screen leaves the show.
    for _ in 0..5 {
        press(&mut harness, Key::ArrowRight);
    }
    assert_eq!(count(&harness, "End of posts"), 1);
    harness.get_by_role_and_label(Role::Button, "Exit").click();
    harness.run_steps(3);
    assert!(harness.state().state.auto_view.is_none());
}

#[test]
fn a_minimized_window_stops_the_time_without_pausing_the_show() {
    let (_profile, mut harness) = started_show("autoview-minimized", 5, true);
    advance(&mut harness, 2.0);
    // What the frames of a minimized window do, without the window: the clock is told that the
    // window is minimized, and then a minute passes.
    for seconds in [0.0, 20.0, 20.0, 20.0] {
        let app = harness.state_mut();
        app.state.window_minimized = true;
        app.state.auto_view_clock = app.state.auto_view_clock.map(|now| now + seconds);
        instagram_windows_app::state::tick_auto_view(&mut app.state);
    }
    let run = harness.state().state.auto_view.clone().expect("the show");
    assert_eq!(run.player.cursor.position, 0);
    assert!(!run.player.paused, "minimizing is not pausing");
    assert!(!run.timer.is_running());
    // Back on screen the picture has the three seconds it had left.
    harness.state_mut().state.window_minimized = false;
    harness.run_steps(2);
    advance(&mut harness, 2.9);
    assert_eq!(count(&harness, "1 / 6"), 1);
    assert_eq!(count(&harness, "Paused"), 0);
    advance(&mut harness, 0.2);
    assert_eq!(count(&harness, "2 / 6"), 1);
}

#[test]
fn auto_view_from_here_starts_at_that_post_and_closes_the_viewer() {
    let profile = profile("autoview-here");
    let mut app = app_with(&profile, &fixture("export-basic"));
    app.state.auto_view_clock = Some(0.0);
    let mut harness = harness(app);
    settle_idle(&mut harness);
    let key = harness.state().state.posts()[2].key.clone();
    harness.state_mut().dispatch(Action::OpenViewer(key.clone()));
    harness.state_mut().dispatch(Action::OpenPostOptions(key));
    harness.run_steps(4);
    dialog_row(&harness, SHEET_WIDTH, "Auto view from here").click();
    harness.run_steps(4);
    assert!(harness.state().state.sheet.is_none());
    assert!(harness.state().state.auto_view_setup.is_some());
    dialog_row(&harness, SHEET_WIDTH, "Start").click();
    settle_idle(&mut harness);
    assert!(harness.state().state.viewer.is_none(), "the show replaces the viewer");
    assert_eq!(count(&harness, "3 / 6"), 1);
    // Not shuffled, the order is not rotated: the show goes on from here to the end.
    advance(&mut harness, 15.1);
    assert_eq!(count(&harness, "4 / 6"), 1);

    // "Open post" leaves the show for the viewer on that post.
    harness.get_by_role_and_label(Role::Button, "Open post").click();
    settle_idle(&mut harness);
    assert!(harness.state().state.auto_view.is_none());
    let shown = harness.state().state.viewer.as_ref().map(|viewer| viewer.key.clone());
    assert_eq!(shown, Some(harness.state().state.posts()[3].key.clone()));
}

#[test]
fn the_command_line_starts_the_show_without_the_dialog_and_saves_nothing() {
    let profile = profile("autoview-cli");
    let mut configuration = config(&profile);
    configuration.auto_view = Some(AutoViewRequest { interval_sec: Some(7), wait_for_videos: None, loop_posts: Some(false), shuffle: None });
    let mut app = App::new(configuration);
    app.state.auto_view_clock = Some(0.0);
    let request = instagram_core::catalog::OpenRequest {
        paths: vec![fixture("export-basic")],
        options: instagram_core::catalog::OpenOptions { default_username: app.services.default_username.clone() },
    };
    app.show_opened(instagram_core::catalog::open_paths(&request, &app.services.store, &app.services.context).expect("the fixture opens"));
    let mut harness = harness(app);
    settle_idle(&mut harness);

    let app = harness.state();
    assert!(app.state.auto_view_setup.is_none());
    assert_eq!(app.state.auto_view.as_ref().map(|run| run.settings), Some(Settings { interval_sec: 7, wait_for_videos: true, loop_posts: false, shuffle: false }));
    assert!(app.state.auto_view_request.is_none(), "the request is used once");
    assert_eq!(count(&harness, "1 / 6"), 1);
    assert_eq!(count(&harness, "7 s"), 1);
    // Nothing of it is remembered (state.json exists: it remembers the folder that was opened).
    assert_eq!(app.state.remembered.auto_view, Settings::default());
    assert_eq!(UiState::load(&app.services.paths.state_file).auto_view, Settings::default());
    // Leaving the show does not start it again.
    press(&mut harness, Key::Escape);
    harness.run_steps(3);
    assert!(harness.state().state.auto_view.is_none());
}

// --- About, robustness ------------------------------------------------------------------------------

#[test]
fn about_shows_where_the_files_of_this_profile_are() {
    let profile = profile("about");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);
    harness.get_by_role_and_label(Role::Button, "More").click();
    harness.run_steps(3);
    harness.get_by_role_and_label(Role::Button, "About").click();
    harness.run_steps(4);

    let paths = harness.state().services.paths.clone();
    assert!(paths.data_dir.starts_with(&profile) && paths.settings_file.starts_with(&profile));
    assert_eq!(count(&harness, "Instagram Windows App"), 1);
    assert_eq!(count(&harness, &format!("Version {}", env!("CARGO_PKG_VERSION"))), 1);
    assert_eq!(count(&harness, &paths.data_dir.display().to_string()), 1);
    assert_eq!(count(&harness, &paths.settings_file.display().to_string()), 1);
    assert_eq!(count_containing(&harness, "Apache License 2.0"), 1);
    // "Open data folder" is there (Explorer is not started by a test), and "Close" closes.
    assert!(!dialog_row(&harness, SHEET_WIDTH, "Open data folder").accesskit_node().is_disabled());
    dialog_row(&harness, SHEET_WIDTH, "Close").click();
    harness.run_steps(3);
    assert!(harness.state().state.about_opened_at.is_none());
}

#[test]
fn damaged_files_of_the_profile_do_not_stop_the_app() {
    let profile = profile("damaged");
    // A first start creates the settings and tells where the data folder is.
    let paths = config(&profile).settings.paths;
    std::fs::create_dir_all(&paths.data_dir).expect("the data folder");
    std::fs::write(&paths.state_file, b"{ \"theme\": \"dark\", ").expect("a cut state.json");
    std::fs::write(&paths.descriptions_file, b"not json at all").expect("a damaged descriptions.json");

    let configuration = config(&profile);
    // state.json: the defaults, silently.
    assert_eq!(configuration.remembered, UiState::default());
    // descriptions.json: set aside, a new one started, and the user is told once.
    let notice = configuration.store_notice.clone().expect("a notice about the descriptions");
    assert!(notice.starts_with("The saved descriptions in \""), "{notice}");
    assert!(notice.contains("could not be read. The file was kept as \"descriptions.damaged-"), "{notice}");
    let mut harness = harness(App::new(configuration));
    harness.run_steps(3);
    // The notice is a dialog that stays until it is closed, not a toast that runs out.
    assert_eq!(toasts(&harness), Vec::<String>::new());
    assert_eq!(harness.state().state.confirm.as_ref().map(|confirm| confirm.kind.clone()), Some(instagram_windows_app::state::ConfirmKind::StoreNotice(notice.clone())));
    assert_eq!(count(&harness, &notice), 1);
    assert!(names_in(&paths.data_dir).iter().any(|name| name.starts_with("descriptions.damaged-")), "{:?}", names_in(&paths.data_dir));
}

#[test]
fn a_picture_that_disappears_shows_the_placeholder_with_the_reason() {
    let profile = profile("vanished");
    let pictures = copy_of_loose(&profile);
    let app = app_with(&profile, &pictures);
    // The folder was read; before anything is decoded one of the pictures is deleted.
    let gone = pictures.join("IMG_2.jpg");
    std::fs::remove_file(&gone).expect("the picture can be deleted");
    let mut harness = harness(app);
    settle_idle(&mut harness);

    let (key, path) = {
        let post = harness.state().state.posts().iter().find(|post| post.file_name().as_deref() == Some("IMG_2.jpg")).expect("the post of the deleted picture");
        (post.key.clone(), post.media[0].path.clone())
    };
    // Its card comes on screen.
    harness.state_mut().state.scroll_request = Some(instagram_windows_app::state::ScrollRequest::Post(key));
    settle_idle(&mut harness);
    let app = harness.state();
    match app.services.loader.peek_size(&path) {
        SizeState::Failed(reason) => assert!(reason.contains("IMG_2.jpg"), "{reason}"),
        other => panic!("the picture should have failed, not {other:?}"),
    }
    // The card says so, with the reason; the other pictures are there.
    let unavailable: Vec<String> = harness.query_all_by_label_contains("(image unavailable)").map(|node| node.accesskit_node().label().unwrap_or_default()).collect();
    assert_eq!(unavailable.len(), 1, "{unavailable:?}");
    assert!(unavailable[0].contains("IMG_2.jpg"), "{unavailable:?}");

    // A download leaves the missing picture out and names it.
    let out = output_folder(&profile);
    start_download_all(&mut harness, &out.join("rest.zip"));
    settle_idle(&mut harness);
    let message = toasts(&harness).pop().expect("a toast");
    assert!(message.starts_with("Downloaded ") && message.contains(" as rest.zip. Left out: "), "{message}");
    assert!(message.contains("IMG_2.jpg"), "{message}");
}

// --- The whole journey ----------------------------------------------------------------------------

#[test]
fn descriptions_typed_today_are_in_the_archive_downloaded_after_a_restart() {
    let profile = profile("journey");
    let pictures = copy_of_loose(&profile);
    let first_caption = "Harbour at dusk 🌊\nSecond line.";
    let second_caption = "第二张照片 مرحبا";

    // Open the loose pictures and describe two of them: one by typing into the dialog, one
    // through the actions the dialog sends.
    let mut harness = harness(app_with(&profile, &pictures));
    settle_idle(&mut harness);
    harness.get_all_by_role_and_label(Role::Button, "More options").min_by(|a, b| a.rect().top().total_cmp(&b.rect().top())).expect("a card").click();
    harness.run_steps(3);
    dialog_row(&harness, SHEET_WIDTH, "Edit description").click();
    harness.run_steps(4);
    harness.get_by_role_and_label(Role::MultilineTextInput, "Description").type_text("Harbour at dusk 🌊");
    harness.run_steps(2);
    press(&mut harness, Key::Enter);
    harness.get_by_role_and_label(Role::MultilineTextInput, "Description").type_text("Second line.");
    harness.run_steps(2);
    dialog_row(&harness, EDITOR_WIDTH, "Save").click();
    harness.run_steps(4);
    assert!(harness.state().state.editor.is_none(), "the description was saved");
    let third = harness.state().state.posts()[2].key.clone();
    harness.state_mut().dispatch(Action::OpenEditor(third.clone()));
    harness.state_mut().dispatch(Action::EditorInput(second_caption.to_owned()));
    harness.state_mut().dispatch(Action::SaveDescription);
    harness.run_steps(3);
    assert_eq!(toasts(&harness), ["Description updated."]);
    let first = harness.state().state.posts()[0].key.clone();
    let before = std::fs::read_dir(&pictures).expect("the folder").count();
    drop(harness);

    // A new app on the same profile: the descriptions are there.
    let mut harness = common::harness(app_with(&profile, &pictures));
    settle_idle(&mut harness);
    let app = harness.state();
    assert_eq!(app.state.post(&first).map(|post| post.caption.as_str()), Some(first_caption));
    assert_eq!(app.state.post(&third).map(|post| post.caption.as_str()), Some(second_caption));
    assert_eq!(count_containing(&harness, "Harbour at dusk"), 1);

    // "Download all": the archive carries them, and nothing was written beside the pictures.
    let out = output_folder(&profile);
    let file = out.join("instagram-export.zip");
    start_download_all(&mut harness, &file);
    settle_idle(&mut harness);
    let posts = harness.state().state.posts().len();
    assert_eq!(toasts(&harness), [format!("Downloaded {posts} posts as instagram-export.zip.")]);
    let (manifest, names) = archive_manifest(&file);
    assert!(manifest.contains("\"caption\": \"Harbour at dusk 🌊\\nSecond line.\""), "{manifest}");
    assert!(manifest.contains(&format!("\"caption\": \"{second_caption}\"")), "{manifest}");
    assert_eq!(names.iter().filter(|name| name.starts_with("posts/")).count(), posts);
    assert_eq!(std::fs::read_dir(&pictures).expect("the folder").count(), before, "the pictures' folder is untouched");
}
