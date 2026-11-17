//! Headless tests of export archives in the interface: opening one through the opener thread,
//! the banner, the window title, the editor's note, descriptions that survive closing and
//! reopening, opening more (archives join exports, pictures replace them), "Download all" of
//! archive posts, and the More menu entry of the web app's library. Every file the app writes
//! lies in the test's own profile folder, the archive cache included.

mod common;

use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use common::{count, count_containing, fixture, harness, profile, settle, settle_idle, toasts};
use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, Node};
use instagram_core::model::{ExportKind, PostOrigin};
use instagram_core::settings;
use instagram_windows_app::app::{App, AppConfig};
use instagram_windows_app::state::Action;

/// Width of the "Post options" sheet.
const SHEET_WIDTH: f32 = 400.0;

fn archive(name: &str) -> PathBuf {
    fixture("archives").join(name)
}

/// An app of `profile` with `paths` opened the way the user opens them (in the background).
fn opened(profile: &Path, paths: Vec<PathBuf>) -> Harness<'static, App> {
    let mut harness = harness(App::new(common::config(profile)));
    harness.state_mut().dispatch(Action::OpenPaths(paths));
    settle(&mut harness, "the archive to open", |app| app.state.collection.is_some());
    harness
}

/// The row named `label` of the sheet.
fn sheet_row<'a>(harness: &'a Harness<'_, App>, label: &str) -> Node<'a> {
    harness
        .query_all_by_role(Role::Button)
        .filter(|node| (node.rect().width() - SHEET_WIDTH).abs() < 0.5)
        .find(|node| node.accesskit_node().label().as_deref() == Some(label))
        .unwrap_or_else(|| panic!("the sheet has no row \"{label}\""))
}

/// Opens the editor of the first post on screen through its "More options".
fn open_editor(harness: &mut Harness<'_, App>) {
    let more = harness.get_all_by_role_and_label(Role::Button, "More options").min_by(|a, b| a.rect().top().total_cmp(&b.rect().top())).expect("a card");
    more.click();
    harness.run_steps(3);
    sheet_row(harness, "Edit description").click();
    harness.run_steps(4);
    assert!(harness.state().state.editor.is_some(), "the editor opens");
}

#[test]
fn an_archive_opens_in_the_background_with_its_banner_and_names() {
    let profile = profile("archive-open");
    let mut harness = opened(&profile, vec![archive("export-basic.zip")]);
    settle_idle(&mut harness);
    assert_eq!(count(&harness, "Showing 6 posts from export-basic.zip"), 1);
    let app = harness.state();
    assert_eq!(app.services.leases.len(), 1, "the generation is leased while its posts are shown");
    let generation = app.services.leases[0].generation().to_path_buf();
    assert!(generation.starts_with(&profile), "the archive cache lies in the profile: {}", generation.display());
    assert!(app.services.paths.archive_cache_dir.starts_with(&profile));
    let first = &app.state.posts()[0];
    assert!(matches!(first.origin, PostOrigin::Archive { .. }));
    assert!(first.media[0].path.starts_with(&generation));

    // The viewer names the file as post.json does, not the copy in the cache.
    let key = first.key.clone();
    harness.state_mut().dispatch(Action::OpenViewer(key));
    settle_idle(&mut harness);
    assert_eq!(harness.state().state.window_title(), "post1.jpg - Instagram Windows App");

    // Closing lets go of the generation.
    harness.state_mut().dispatch(Action::CloseViewer);
    harness.state_mut().dispatch(Action::ClosePictures);
    settle_idle(&mut harness);
    assert!(harness.state().services.leases.is_empty());
}

#[test]
fn a_description_of_an_archive_post_is_kept_by_the_app_and_comes_back() {
    let profile = profile("archive-edit");
    let before = std::fs::read(archive("export-basic.zip")).expect("the fixture");
    let mut harness = opened(&profile, vec![archive("export-basic.zip")]);
    settle_idle(&mut harness);
    open_editor(&mut harness);
    assert_eq!(count(&harness, "Kept on this computer. The archive is not changed."), 1);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.run_steps(2);
    harness.get_by_role_and_label(Role::MultilineTextInput, "Description").type_text("Kept beside the archive");
    harness.run_steps(2);
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
    settle_idle(&mut harness);
    assert_eq!(toasts(&harness), ["Description updated."]);
    let saved = std::fs::read_to_string(&harness.state().services.paths.descriptions_file).expect("descriptions.json");
    assert!(saved.contains("\"archive|") && saved.contains("Kept beside the archive"), "{saved}");
    assert_eq!(std::fs::read(archive("export-basic.zip")).expect("the fixture"), before, "the archive is not changed");

    // Closed and opened again: the description is back.
    harness.state_mut().dispatch(Action::ClosePictures);
    settle_idle(&mut harness);
    harness.state_mut().dispatch(Action::OpenPaths(vec![archive("export-basic.zip")]));
    settle(&mut harness, "the archive to open again", |app| app.state.collection.is_some());
    assert_eq!(harness.state().state.posts()[0].caption, "Kept beside the archive");
    assert_eq!(count_containing(&harness, "Kept beside the archive"), 1);
}

#[test]
fn archives_opened_more_join_the_exports_and_pictures_replace_them() {
    let profile = profile("archive-more");
    let mut harness = opened(&profile, vec![archive("export-part1.zip")]);
    settle_idle(&mut harness);
    let serial = harness.state().state.collection_serial;
    harness.state_mut().dispatch(Action::OpenPaths(vec![archive("export-part2.zip")]));
    settle(&mut harness, "the second archive to join", |app| app.state.collection_serial != serial);
    settle_idle(&mut harness);
    assert_eq!(count(&harness, "Showing 3 posts from 2 archives"), 1);
    assert_eq!(count(&harness, "1 warning"), 1, "the replaced post is reported");
    assert_eq!(harness.state().services.leases.len(), 2);
    let ids: Vec<String> = harness.state().state.posts().iter().map(|post| post.id.clone()).collect();
    assert_eq!(ids, ["FXTpart0003", "FXTpart0002", "FXTpart0001"]);

    // A picture is not an export: it replaces them, and the generations are let go.
    let serial = harness.state().state.collection_serial;
    harness.state_mut().dispatch(Action::OpenPaths(vec![fixture("loose").join("IMG_2.jpg")]));
    settle(&mut harness, "the picture to replace the exports", |app| app.state.collection_serial != serial);
    settle_idle(&mut harness);
    assert!(!harness.state().state.collection.as_ref().is_some_and(|collection| collection.is_export()));
    assert!(harness.state().services.leases.is_empty());
}

fn node_is_installed() -> bool {
    Command::new("node").arg("--version").output().map(|output| output.status.success()).unwrap_or(false)
}

#[test]
fn download_all_of_archive_posts_writes_the_members_bytes() {
    let profile = profile("archive-download");
    let out = profile.join("out");
    std::fs::create_dir_all(&out).expect("the output folder");
    let mut harness = opened(&profile, vec![archive("export-basic.zip")]);
    settle_idle(&mut harness);
    let file = out.join("instagram-export.zip");
    harness.state_mut().dispatch(Action::DownloadAll);
    assert!(harness.state_mut().services.downloader.answer_dialog(Some(file.clone())), "the Save dialog was asked for");
    harness.step();
    settle_idle(&mut harness);
    assert_eq!(toasts(&harness), ["Downloaded 6 posts as instagram-export.zip."]);

    // Every picture of the download is a member of the opened archive, byte for byte.
    let read_all = |path: &Path| -> HashMap<String, Vec<u8>> {
        let mut reader = zip::ZipArchive::new(std::fs::File::open(path).expect("an archive")).expect("a zip");
        (0..reader.len())
            .map(|index| {
                let mut member = reader.by_index(index).expect("a member");
                let mut bytes = Vec::new();
                member.read_to_end(&mut bytes).expect("readable");
                (String::from_utf8_lossy(member.name_raw()).into_owned(), bytes)
            })
            .collect()
    };
    let original = read_all(&archive("export-basic.zip"));
    let downloaded = read_all(&file);
    let pictures: Vec<&Vec<u8>> = downloaded.iter().filter(|(name, _)| name.starts_with("posts/")).map(|(_, bytes)| bytes).collect();
    assert_eq!(pictures.len(), 7);
    for bytes in pictures {
        assert!(original.values().any(|member| member == bytes), "a picture of the download is not a member of the archive");
    }

    if node_is_installed() {
        let extracted = profile.join("extracted");
        for (name, bytes) in &downloaded {
            let path = extracted.join(name);
            std::fs::create_dir_all(path.parent().expect("a folder")).expect("folders");
            std::fs::write(&path, bytes).expect("written");
        }
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("core").join("tests").join("validator").join("validate-export.mjs");
        let output = Command::new("node").arg(&script).arg(&extracted).output().expect("node runs");
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.status.code(), Some(0), "{text}");
        assert!(!text.contains("ERROR"), "{text}");
        println!("validator: {}", text.lines().last().unwrap_or_default());
    } else {
        println!("note: node is not on PATH; the validator of the clone was NOT run");
    }
}

/// The configuration of an app whose profile also stands in for %TEMP% (where the web app keeps
/// its library by default).
fn config_with_temp(profile: &Path) -> AppConfig {
    let env: HashMap<String, String> = [
        ("APPDATA".to_owned(), profile.display().to_string()),
        ("TEMP".to_owned(), profile.join("Temp").display().to_string()),
        ("USERNAME".to_owned(), "tester".to_owned()),
    ]
    .into();
    let settings = settings::load_or_create(&env).expect("settings in the test profile");
    assert_eq!(settings.paths.web_app_library_dir.as_deref(), Some(profile.join("Temp").join("instagram_cache").as_path()));
    let mut config = AppConfig::load(settings);
    config.animations = false;
    config
}

#[test]
fn the_more_menu_offers_the_web_apps_library_only_when_it_holds_a_post_json() {
    let profile = profile("web-app-library");
    let mut harness = harness(App::new(config_with_temp(&profile)));
    settle_idle(&mut harness);
    harness.state_mut().dispatch(Action::ToggleMoreMenu);
    settle_idle(&mut harness);
    assert_eq!(count(&harness, "Open the web app's library"), 0, "no library yet");
    harness.state_mut().dispatch(Action::CloseMoreMenu);

    // The web app has saved imports: a copy of an export stands in for its library.
    let library = profile.join("Temp").join("instagram_cache");
    for entry in walk(&fixture("export-basic")) {
        let target = library.join(entry.strip_prefix(fixture("export-basic")).expect("below the fixture"));
        std::fs::create_dir_all(target.parent().expect("a folder")).expect("folders");
        std::fs::copy(&entry, &target).expect("a copy");
    }
    harness.state_mut().dispatch(Action::ToggleMoreMenu);
    settle_idle(&mut harness);
    assert_eq!(count(&harness, "Open the web app's library"), 1);
    harness.get_by_label("Open the web app's library").click();
    settle(&mut harness, "the library to open", |app| app.state.collection.is_some());
    settle_idle(&mut harness);
    let collection = harness.state().state.collection.clone().expect("open");
    assert_eq!(collection.label, "instagram_cache");
    assert_eq!(collection.sources()[0].kind, ExportKind::Folder);
    assert!(!harness.state().state.more_menu_open);
    assert_eq!(count(&harness, "Showing 6 posts from instagram_cache"), 1);
}

/// Every file below `folder`.
fn walk(folder: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in std::fs::read_dir(&next).expect("a folder").flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}
