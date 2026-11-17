//! Headless tests of the foundation of the interface: the shell, opening, the feed and its
//! cards. The helpers they share with the other test files are in `common`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use common::{app_with, config, count, fixture, harness, profile, settle, settle_idle};
use egui::accesskit::Role;
use egui::{Key, Modifiers};
use egui_kittest::kittest::Queryable as _;
use instagram_core::store::Theme;
use instagram_windows_app::app::App;
use instagram_windows_app::opening::DialogKind;
use instagram_windows_app::state::ScrollRequest;
use instagram_windows_app::theme;

#[test]
fn the_empty_state_shows_both_buttons() {
    let profile = profile("empty");
    let mut harness = harness(App::new(config(&profile)));
    settle_idle(&mut harness);

    assert_eq!(count(&harness, "Open pictures to get started"), 1);
    assert_eq!(count(&harness, "or drop pictures, videos or a folder here"), 1);
    // Each name twice: the sidebar item and the button of the empty state.
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Open pictures").count(), 2);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Open folder").count(), 2);
    for name in ["Home", "Explore", "Auto view", "More", "Instagram"] {
        assert_eq!(harness.query_all_by_role_and_label(Role::Button, name).count(), 1, "sidebar item {name}");
    }

    // The button asks for the native dialog; showing it is the window's job, not the view's.
    let button = harness.get_all_by_role_and_label(Role::Button, "Open pictures").last().expect("the empty-state button");
    button.click();
    harness.run_steps(3);
    assert_eq!(harness.state().services.opener.pending_dialog(), Some(DialogKind::Pictures));
    let button = harness.get_all_by_role_and_label(Role::Button, "Open folder").last().expect("the empty-state button");
    button.click();
    harness.run_steps(3);
    assert_eq!(harness.state().services.opener.pending_dialog(), Some(DialogKind::Folder));
}

#[test]
fn an_opened_export_shows_the_banner_and_the_first_post() {
    let profile = profile("export");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);

    assert_eq!(count(&harness, "Showing 6 posts from export-basic"), 1);
    for name in ["Auto view", "Download all"] {
        assert!(harness.query_all_by_role_and_label(Role::Button, name).count() >= 1, "banner button {name}");
    }
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Open more").count(), 1);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Close").count(), 1);

    // The first card: author, sub-line, likes, the picture by its alt text.
    assert_eq!(count(&harness, "fixture.sunrise.bakery"), 1);
    assert_eq!(count(&harness, "Fixtureville, Testland"), 1);
    assert_eq!(count(&harness, "56.8K likes"), 1);
    assert_eq!(harness.query_all_by_role_and_label(Role::Image, "Orange test card labelled POST 1").count(), 1);
    assert_eq!(count(&harness, "View all 1.2K comments"), 1);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "More options").count(), 1);

    // Nothing was written into the fixture, and the pictures on screen were decoded.
    assert!(harness.state().services.loader.stats().decoded >= 1);
    assert!(!fixture("export-basic").join("descriptions.json").exists());
}

#[test]
fn switch_appearance_flips_the_palette_and_is_remembered() {
    let profile = profile("appearance");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);
    assert_eq!(harness.state().state.theme, Theme::Light);
    assert_eq!(harness.state().state.palette(), &theme::LIGHT);

    harness.get_by_role_and_label(Role::Button, "More").click();
    harness.run_steps(3);
    assert!(harness.state().state.more_menu_open);
    harness.get_by_role_and_label(Role::Button, "Switch appearance").click();
    harness.run_steps(3);

    let app = harness.state();
    assert_eq!(app.state.theme, Theme::Dark);
    assert_eq!(app.state.palette(), &theme::DARK);
    assert!(!app.state.more_menu_open, "the menu closes after the switch");
    // egui paints with the dark style now.
    assert_eq!(harness.ctx.global_style().visuals.panel_fill, theme::DARK.bg);
    // state.json of the profile remembers it for the next start.
    let state_file = app.services.paths.state_file.clone();
    assert!(state_file.starts_with(&profile));
    let saved = std::fs::read_to_string(&state_file).expect("state.json");
    assert!(saved.contains("\"dark\""), "{saved}");
    drop(harness);
    let restarted = App::new(config(&profile));
    assert_eq!(restarted.state.theme, Theme::Dark);
}

#[test]
fn more_expands_the_caption_in_place() {
    let profile = profile("caption");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);

    // Collapsed: the preview ends before the third line of the caption.
    assert_eq!(harness.query_all_by_label_contains("Come early").count(), 0);
    let offset_before = harness.state().feed.scroll_offset();
    harness.get_by_role_and_label(Role::Button, "more").click();
    settle_idle(&mut harness);

    let app = harness.state();
    let key = app.state.posts()[0].key.clone();
    assert!(app.state.post_ui(&key).expanded);
    assert_eq!(app.feed.scroll_offset(), offset_before, "the feed stays where it is");
    assert_eq!(harness.query_all_by_label_contains("Come early, the cinnamon knots never last past nine.").count(), 1);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "more").count(), 0, "there is no way back");
    // The hashtags of the full caption are links now.
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "#sourdough").count(), 1);
}

#[test]
fn like_save_and_share_work_on_a_card() {
    let profile = profile("actions");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);
    let key = harness.state().state.posts()[0].key.clone();

    harness.get_by_role_and_label(Role::Button, "Like").click();
    harness.run_steps(3);
    assert!(harness.state().state.post_ui(&key).liked);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Unlike").count(), 1);
    harness.get_by_role_and_label(Role::Button, "Unlike").click();
    harness.run_steps(3);
    assert!(!harness.state().state.post_ui(&key).liked);

    // The keyboard works too: focus the button, press Enter.
    harness.get_by_role_and_label(Role::Button, "Like").focus();
    harness.run_steps(2);
    harness.key_press(Key::Enter);
    harness.run_steps(3);
    assert!(harness.state().state.post_ui(&key).liked, "Enter activates the focused button");

    harness.get_by_role_and_label(Role::Button, "Save").click();
    harness.run_steps(3);
    assert!(harness.state().state.post_ui(&key).saved);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Remove from saved").count(), 1);

    // Share copies the link of an export post and says so.
    harness.get_by_role_and_label(Role::Button, "Share").click();
    harness.run_steps(3);
    assert_eq!(count(&harness, "Link copied."), 1);
    let copied = harness.output().platform_output.commands.iter().any(|command| matches!(command, egui::OutputCommand::CopyText(text) if text == "https://www.instagram.com/p/FXTbasic001/"));
    let state = harness.state();
    assert!(copied || state.state.toasts.visible().iter().any(|toast| toast.message == "Link copied."));
}

#[test]
fn loose_pictures_show_their_file_names_and_the_warning() {
    let profile = profile("loose");
    let mut harness = harness(app_with(&profile, &fixture("loose")));
    settle_idle(&mut harness);

    assert_eq!(count(&harness, "Showing 5 pictures from loose"), 1);
    assert_eq!(count(&harness, "img_1.png"), 1, "the file name is the sub-line of the first card");
    assert_eq!(count(&harness, "tester"), 1, "the author is the default username of the profile");
    // No count is known for a loose picture: no likes line until the user likes it.
    assert_eq!(harness.query_all_by_label_contains("likes").count(), 0);
    harness.get_by_role_and_label(Role::Button, "Like").click();
    harness.run_steps(3);
    assert_eq!(count(&harness, "Liked by you"), 1);

    // The empty file of the fixture is a warning; the banner button opens the dialog that
    // lists it, and Escape closes that again.
    harness.get_by_role_and_label(Role::Button, "1 warning").click();
    settle_idle(&mut harness);
    assert!(harness.state().state.warnings_opened_at.is_some());
    assert_eq!(harness.query_all_by_label_contains("broken.jpg").count(), 1);
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    assert!(harness.state().state.warnings_opened_at.is_none());
    assert_eq!(harness.query_all_by_label_contains("broken.jpg").count(), 0);

    // Share copies the path of a loose picture.
    harness.get_by_role_and_label(Role::Button, "Share").click();
    harness.run_steps(3);
    assert_eq!(count(&harness, "Path copied."), 1);
}

#[test]
fn keyboard_shortcuts_ask_for_the_dialogs_and_scroll_the_feed() {
    let profile = profile("keys");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);

    harness.key_press_modifiers(Modifiers::COMMAND, Key::O);
    harness.run_steps(3);
    assert_eq!(harness.state().services.opener.pending_dialog(), Some(DialogKind::Pictures));
    harness.key_press_modifiers(Modifiers::COMMAND | Modifiers::SHIFT, Key::O);
    harness.run_steps(3);
    assert_eq!(harness.state().services.opener.pending_dialog(), Some(DialogKind::Folder));

    assert_eq!(harness.state().feed.scroll_offset(), 0.0);
    harness.key_press(Key::End);
    settle(&mut harness, "the end of the feed", |app| app.feed.scroll_offset() > 2000.0);
    assert_eq!(count(&harness, "You're all caught up"), 1);
    assert_eq!(count(&harness, "pixel.orchard.demo"), 1, "the last post is in view");
    harness.key_press(Key::Home);
    settle(&mut harness, "the top of the feed", |app| app.feed.scroll_offset() == 0.0);
    assert_eq!(count(&harness, "Showing 6 posts from export-basic"), 1);
    harness.key_press(Key::PageDown);
    settle(&mut harness, "one page down", |app| app.feed.scroll_offset() > 500.0);
}

#[test]
fn the_carousel_arrows_page_through_the_slides() {
    let profile = profile("carousel");
    let mut app = app_with(&profile, &fixture("export-basic"));
    let key = app.state.posts()[4].key.clone();
    // The feed shows the carousel post.
    app.state.scroll_request = Some(ScrollRequest::Post(key.clone()));
    let mut harness = harness(app);
    settle_idle(&mut harness);

    assert_eq!(count(&harness, "1/2"), 1);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Previous photo").count(), 0, "no way back from the first slide");
    harness.get_by_role_and_label(Role::Button, "Next photo").click();
    settle_idle(&mut harness);
    assert_eq!(harness.state().state.post_ui(&key).slide, 1);
    assert_eq!(count(&harness, "2/2"), 1);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Next photo").count(), 0, "no way on from the last slide");
    harness.get_by_role_and_label(Role::Button, "Previous photo").click();
    settle_idle(&mut harness);
    assert_eq!(harness.state().state.post_ui(&key).slide, 0);
}

#[test]
fn dropped_files_open_and_closing_returns_to_the_empty_state() {
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

    let profile = profile("drop");
    let mut harness = harness(App::new(config(&profile)));
    settle_idle(&mut harness);
    harness.input_mut().dropped_files.push(Arc::new(Dropped(fixture("loose"))));
    settle(&mut harness, "the dropped folder to open", |app| app.state.collection.is_some());
    assert_eq!(count(&harness, "Showing 5 pictures from loose"), 1);
    // The folder is remembered as the start folder of the next dialog.
    let remembered = harness.state().state.remembered.last_folder.clone().expect("last folder");
    assert!(remembered.ends_with("loose"));

    harness.get_by_role_and_label(Role::Button, "Close").click();
    settle_idle(&mut harness);
    assert!(harness.state().state.collection.is_none());
    assert_eq!(count(&harness, "Open pictures to get started"), 1);

    // A path that does not exist leaves the empty state and says why.
    harness.input_mut().dropped_files.push(Arc::new(Dropped(profile.join("no-such-folder"))));
    settle(&mut harness, "the failed open to be reported", |app| !app.state.toasts.is_empty());
    assert!(harness.state().state.collection.is_none());
    assert_eq!(harness.query_all_by_label_contains("no-such-folder").count(), 1);
}

#[test]
fn the_about_sheet_opens_from_the_menu_and_closes_with_escape() {
    let profile = profile("about");
    let mut harness = harness(App::new(config(&profile)));
    settle_idle(&mut harness);
    harness.get_by_role_and_label(Role::Button, "More").click();
    harness.run_steps(3);
    // Without pictures there is nothing to download or close.
    harness.get_by_role_and_label(Role::Button, "About").click();
    harness.run_steps(4);
    assert!(harness.state().state.about_opened_at.is_some());
    assert_eq!(count(&harness, "Instagram Windows App"), 1);
    assert_eq!(harness.query_all_by_label_contains("Version ").count(), 1);
    assert_eq!(harness.query_all_by_label_contains("settings.json").count(), 1);
    harness.key_press(Key::Escape);
    harness.run_steps(4);
    assert!(harness.state().state.about_opened_at.is_none());
}

#[test]
fn a_long_feed_lays_out_only_the_cards_near_the_view() {
    // 400 small pictures: far more than fit a window.
    let profile = profile("long");
    let pictures = profile.join("pictures");
    std::fs::create_dir_all(&pictures).expect("pictures folder");
    let png = std::fs::read(fixture("loose").join("img_1.png")).expect("a small png");
    for index in 0..400 {
        std::fs::write(pictures.join(format!("picture_{index:04}.png")), &png).expect("write");
    }
    let mut harness = harness(app_with(&profile, &pictures));
    settle_idle(&mut harness);

    let app = harness.state();
    assert_eq!(app.state.posts().len(), 400);
    assert!(app.feed.painted_cards() <= 3, "{} cards were painted", app.feed.painted_cards());
    let decoded = app.services.loader.stats().decoded;
    assert!((1..=12).contains(&decoded), "{decoded} pictures were decoded for one screen of a 400-picture feed");
    assert_eq!(count(&harness, "picture_0000.png"), 1);
    assert_eq!(count(&harness, "picture_0200.png"), 0);

    // Jumping to the middle shows that card, with the same small number of cards and textures.
    let key = harness.state().state.posts()[200].key.clone();
    harness.state_mut().state.scroll_request = Some(ScrollRequest::Post(key));
    settle_idle(&mut harness);
    assert_eq!(count(&harness, "picture_0200.png"), 1);
    assert!(harness.state().feed.painted_cards() <= 3);
    assert!(harness.state().services.loader.stats().decoded <= 30);
}
