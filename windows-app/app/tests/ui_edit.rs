//! Headless tests of the "Post options" sheet, the "Edit description" dialog and the warnings
//! dialog: what the user sees and types, and what ends up in descriptions.json of the test's own
//! profile folder.

mod common;

use std::path::Path;

use common::{app_with, copy_of_loose, count, count_containing, fixture, harness, profile, settle_idle, toasts};
use egui::accesskit::Role;
use egui::{Event, ImeEvent, Key, Modifiers};
use egui_kittest::kittest::{NodeT as _, Queryable as _};
use egui_kittest::{Harness, Node};
use instagram_core::text;
use instagram_windows_app::app::App;
use instagram_windows_app::state::Action;

/// Width of the "Post options" sheet.
const SHEET_WIDTH: f32 = 400.0;
/// Width of the "Edit description" dialog.
const EDITOR_WIDTH: f32 = 520.0;

/// The rows of the dialog of `width` on screen, top to bottom: the buttons that are exactly as
/// wide as the dialog.
fn dialog_rows<'a>(harness: &'a Harness<'_, App>, width: f32) -> Vec<Node<'a>> {
    let mut rows: Vec<Node<'a>> = harness.query_all_by_role(Role::Button).filter(|node| (node.rect().width() - width).abs() < 0.5).collect();
    rows.sort_by(|a, b| a.rect().top().total_cmp(&b.rect().top()));
    rows
}

/// The labels of those rows.
fn row_labels(harness: &Harness<'_, App>, width: f32) -> Vec<String> {
    dialog_rows(harness, width).iter().map(|node| node.accesskit_node().label().unwrap_or_default()).collect()
}

/// The row named `label` of the dialog of `width`.
fn dialog_row<'a>(harness: &'a Harness<'_, App>, width: f32, label: &str) -> Node<'a> {
    dialog_rows(harness, width).into_iter().find(|node| node.accesskit_node().label().as_deref() == Some(label)).unwrap_or_else(|| panic!("the dialog has no row \"{label}\""))
}

/// The field of the editor.
fn field<'a>(harness: &'a Harness<'_, App>) -> Node<'a> {
    harness.get_by_role_and_label(Role::MultilineTextInput, "Description")
}

/// The text of the editor's field as the state holds it.
fn field_text(harness: &Harness<'_, App>) -> String {
    harness.state().state.editor.as_ref().expect("the editor is open").text.clone()
}

/// The "More options" button of the first feed card on screen.
fn first_more_options<'a>(harness: &'a Harness<'_, App>) -> Node<'a> {
    harness.get_all_by_role_and_label(Role::Button, "More options").min_by(|a, b| a.rect().top().total_cmp(&b.rect().top())).expect("a card with \"More options\"")
}

/// Opens the editor of the first post the way the user does: "More options" of its feed card,
/// then "Edit description".
fn open_editor_from_feed(harness: &mut Harness<'_, App>) {
    first_more_options(harness).click();
    harness.run_steps(3);
    dialog_row(harness, SHEET_WIDTH, "Edit description").click();
    harness.run_steps(4);
    assert!(harness.state().state.sheet.is_none(), "choosing a row closes the sheet");
    assert!(harness.state().state.editor.is_some(), "the editor opens");
}

/// What descriptions.json of the profile holds, or "" when it does not exist.
fn descriptions_file(harness: &Harness<'_, App>) -> String {
    std::fs::read_to_string(&harness.state().services.paths.descriptions_file).unwrap_or_default()
}

/// The bytes of every file of a folder (not recursive), to prove that nothing was changed.
fn folder_bytes(folder: &Path) -> Vec<(String, Vec<u8>)> {
    let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(folder)
        .expect("the folder")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| (entry.file_name().to_string_lossy().into_owned(), std::fs::read(entry.path()).expect("a file of the folder")))
        .collect();
    files.sort();
    files
}

#[test]
fn the_sheet_of_an_export_post_has_its_rows_in_the_web_projects_order() {
    let profile = profile("sheet-export");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);

    harness.get_by_role_and_label(Role::Button, "More options").click();
    harness.run_steps(3);
    assert!(harness.state().state.sheet.is_some());
    assert_eq!(harness.query_all_by_role_and_label(Role::Dialog, "Post options").count(), 1);
    assert_eq!(
        row_labels(&harness, SHEET_WIDTH),
        ["View on Instagram", "View profile on Instagram", "Copy link", "Go to post", "Edit description", "Download post", "Show in folder", "Auto view from here", "Cancel"]
    );

    // "Cancel" closes it; so does Escape.
    dialog_row(&harness, SHEET_WIDTH, "Cancel").click();
    harness.run_steps(3);
    assert!(harness.state().state.sheet.is_none());
    harness.get_by_role_and_label(Role::Button, "More options").click();
    harness.run_steps(3);
    harness.key_press(Key::Escape);
    harness.run_steps(3);
    assert!(harness.state().state.sheet.is_none());

    // "Copy link" puts the link on the clipboard and says so.
    harness.get_by_role_and_label(Role::Button, "More options").click();
    harness.run_steps(3);
    dialog_row(&harness, SHEET_WIDTH, "Copy link").click();
    harness.run_steps(3);
    assert!(harness.state().state.sheet.is_none());
    assert_eq!(toasts(&harness), ["Link copied to clipboard."]);

    // "Download post" closes the sheet and asks for the Save dialog; cancelling that dialog
    // leaves everything as it was (tests/ui_final.rs follows a download to its archive).
    harness.get_by_role_and_label(Role::Button, "More options").click();
    harness.run_steps(3);
    dialog_row(&harness, SHEET_WIDTH, "Download post").click();
    harness.run_steps(3);
    assert!(harness.state().state.sheet.is_none(), "\"Download post\" closes the sheet");
    assert!(harness.state_mut().services.downloader.answer_dialog(None), "the Save dialog was asked for");
    harness.run_steps(3);
    assert!(harness.state().state.download.is_none());

    // "Auto view from here" closes the sheet and opens the setup dialog for this post.
    harness.get_by_role_and_label(Role::Button, "More options").click();
    harness.run_steps(3);
    dialog_row(&harness, SHEET_WIDTH, "Auto view from here").click();
    harness.run_steps(3);
    assert!(harness.state().state.sheet.is_none(), "\"Auto view from here\" closes the sheet");
    let first = harness.state().state.posts()[0].key.clone();
    assert_eq!(harness.state().state.auto_view_setup.as_ref().and_then(|setup| setup.start.clone()), Some(first));
    dialog_row(&harness, SHEET_WIDTH, "Cancel").click();
    harness.run_steps(3);
    assert!(harness.state().state.auto_view_setup.is_none());

    // "Go to post" opens the viewer on the post; from there the sheet has no "Go to post".
    harness.get_by_role_and_label(Role::Button, "More options").click();
    harness.run_steps(3);
    dialog_row(&harness, SHEET_WIDTH, "Go to post").click();
    settle_idle(&mut harness);
    let first = harness.state().state.posts()[0].key.clone();
    assert_eq!(harness.state().state.viewer.as_ref().map(|viewer| viewer.key.clone()), Some(first.clone()));
    harness.state_mut().dispatch(Action::OpenPostOptions(first));
    harness.run_steps(3);
    assert_eq!(
        row_labels(&harness, SHEET_WIDTH),
        ["View on Instagram", "View profile on Instagram", "Copy link", "Edit description", "Download post", "Show in folder", "Auto view from here", "Cancel"]
    );
    // The sheet lies above the viewer although it was first shown before the viewer existed:
    // its rows take the clicks, and the viewer stays open behind it.
    dialog_row(&harness, SHEET_WIDTH, "Cancel").click();
    harness.run_steps(3);
    assert!(harness.state().state.sheet.is_none());
    assert!(harness.state().state.viewer.is_some());
}

#[test]
fn the_sheet_of_a_loose_picture_has_no_link_rows() {
    let profile = profile("sheet-loose");
    let mut harness = harness(app_with(&profile, &fixture("loose")));
    settle_idle(&mut harness);

    first_more_options(&harness).click();
    harness.run_steps(3);
    assert_eq!(row_labels(&harness, SHEET_WIDTH), ["Go to post", "Edit description", "Download post", "Show in folder", "Auto view from here", "Cancel"]);
}

#[test]
fn the_editor_opens_focused_with_the_description_and_its_count() {
    let profile = profile("editor-open");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);
    let caption = harness.state().state.posts()[0].caption.clone();
    assert!(caption.contains('🥐') && caption.contains('\n'), "the fixture's first description has an emoji and line breaks");

    open_editor_from_feed(&mut harness);
    assert_eq!(harness.query_all_by_role_and_label(Role::Dialog, "Edit description").count(), 1);
    assert_eq!(count(&harness, "Post by fixture.sunrise.bakery"), 1);
    // The field holds the description verbatim and has the focus.
    assert_eq!(field_text(&harness), caption);
    assert!(field(&harness).is_focused(), "the field has the focus on opening");
    assert_eq!(field(&harness).value().as_deref(), Some(caption.as_str()));
    // The counter counts UTF-16 code units of the raw text: the emoji are two each, a line
    // break is one (the web project shows the same 177 for this description).
    assert_eq!(text::utf16_len(&caption), 177);
    assert_eq!(count(&harness, "177 / 10,000"), 1);
    assert_eq!(count(&harness, "Kept on this computer. The export folder is not changed."), 1);
    // Nothing changed yet: Save is there but disabled.
    assert_eq!(row_labels(&harness, EDITOR_WIDTH), ["Save", "Cancel"]);
    assert!(dialog_row(&harness, EDITOR_WIDTH, "Save").accesskit_node().is_disabled());

    // The caret is at the end: what is typed is appended.
    field(&harness).type_text("!");
    harness.run_steps(3);
    assert_eq!(field_text(&harness), format!("{caption}!"));
    assert_eq!(count(&harness, "178 / 10,000"), 1);
    assert!(!dialog_row(&harness, EDITOR_WIDTH, "Save").accesskit_node().is_disabled(), "a changed text enables Save");

    // Only white space at the ends is no change (the saved text is trimmed).
    harness.state_mut().dispatch(Action::EditorInput(format!("  {caption}\n ")));
    harness.run_steps(3);
    assert!(dialog_row(&harness, EDITOR_WIDTH, "Save").accesskit_node().is_disabled());
}

#[test]
fn control_enter_saves_and_the_feed_and_the_profile_file_change() {
    let profile = profile("editor-save");
    let fixture_before = folder_bytes(&fixture("export-basic"));
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);

    open_editor_from_feed(&mut harness);
    // Replace the text: select everything, type the new description with a real Enter in it.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::A);
    harness.run_steps(2);
    field(&harness).type_text("  Croissants 🥐");
    harness.run_steps(2);
    harness.key_press(Key::Enter);
    harness.run_steps(2);
    field(&harness).type_text("Baked at dawn.  ");
    harness.run_steps(3);
    assert_eq!(field_text(&harness), "  Croissants 🥐\nBaked at dawn.  ", "Enter starts a new line and never saves");
    assert!(harness.state().state.editor.is_some());
    // The counter counts the untrimmed text: 2 + 13 + 1 + 14 + 2 units.
    assert_eq!(count(&harness, "32 / 10,000"), 1);

    harness.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
    settle_idle(&mut harness);
    let app = harness.state();
    assert!(app.state.editor.is_none(), "Ctrl+Enter saves and closes the dialog");
    assert_eq!(app.state.posts()[0].caption, "Croissants 🥐\nBaked at dawn.", "the saved text is trimmed, the line break inside stays");
    assert_eq!(toasts(&harness), ["Description updated."]);
    // The feed card shows it at once.
    assert_eq!(count_containing(&harness, "Croissants"), 1);
    assert_eq!(count_containing(&harness, "Fresh out of the oven"), 0);

    // descriptions.json of the profile holds it; the export folder is untouched.
    let saved = descriptions_file(&harness);
    assert!(app.services.paths.descriptions_file.starts_with(&profile));
    assert!(saved.contains("Croissants 🥐\\nBaked at dawn."), "{saved}");
    assert!(saved.contains("FXTbasic001"), "{saved}");
    assert_eq!(folder_bytes(&fixture("export-basic")), fixture_before);
    assert!(!fixture("export-basic").join("descriptions.json").exists());

    // The next start of the app shows the edited description.
    drop(harness);
    let reopened = app_with(&profile, &fixture("export-basic"));
    assert_eq!(reopened.state.posts()[0].caption, "Croissants 🥐\nBaked at dawn.");
    assert!(reopened.state.posts()[0].is_edited());
}

#[test]
fn escape_and_cancel_discard_and_close_only_the_editor() {
    let profile = profile("editor-escape");
    let mut app = app_with(&profile, &fixture("export-basic"));
    let key = app.state.posts()[2].key.clone();
    app.dispatch(Action::OpenViewer(key.clone()));
    let mut harness = harness(app);
    settle_idle(&mut harness);
    let caption = harness.state().state.posts()[2].caption.clone();

    // From the viewer: its "More options" (the only one that takes clicks now), "Edit description".
    harness.state_mut().dispatch(Action::OpenPostOptions(key.clone()));
    harness.run_steps(3);
    dialog_row(&harness, SHEET_WIDTH, "Edit description").click();
    harness.run_steps(4);
    field(&harness).type_text(" and more");
    harness.run_steps(3);
    assert_eq!(field_text(&harness), format!("{caption} and more"));

    // Escape drops the text and closes the editor, not the viewer under it.
    harness.key_press(Key::Escape);
    harness.run_steps(4);
    let app = harness.state();
    assert!(app.state.editor.is_none());
    assert!(app.state.viewer.is_some(), "the viewer stays open");
    assert_eq!(app.state.posts()[2].caption, caption);
    assert!(app.state.toasts.is_empty());
    assert_eq!(descriptions_file(&harness), "", "nothing was written");
    // The next Escape closes the viewer.
    harness.key_press(Key::Escape);
    harness.run_steps(4);
    assert!(harness.state().state.viewer.is_none());

    // "Cancel" does the same as Escape.
    harness.state_mut().dispatch(Action::OpenEditor(key.clone()));
    harness.run_steps(4);
    field(&harness).type_text(" again");
    harness.run_steps(3);
    dialog_row(&harness, EDITOR_WIDTH, "Cancel").click();
    harness.run_steps(4);
    assert!(harness.state().state.editor.is_none());
    assert_eq!(harness.state().state.posts()[2].caption, caption);
    // A fresh editor starts from the description again, not from what was typed before.
    harness.state_mut().dispatch(Action::OpenEditor(key.clone()));
    harness.run_steps(4);
    assert_eq!(field_text(&harness), caption);

    // A click on the scrim beside the dialog discards as well; with the viewer open behind the
    // editor, only the editor closes.
    harness.state_mut().dispatch(Action::CloseEditor);
    harness.state_mut().dispatch(Action::OpenViewer(key.clone()));
    harness.state_mut().dispatch(Action::OpenEditor(key));
    harness.run_steps(4);
    field(&harness).type_text(" once more");
    harness.run_steps(3);
    let beside = egui::pos2(30.0, 30.0);
    harness.event(Event::PointerMoved(beside));
    harness.event(Event::PointerButton { pos: beside, button: egui::PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    harness.event(Event::PointerButton { pos: beside, button: egui::PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    harness.run_steps(4);
    assert!(harness.state().state.editor.is_none(), "a click beside the dialog closes it");
    assert!(harness.state().state.viewer.is_some(), "the click went to the editor's scrim, not to the viewer's");
    assert_eq!(harness.state().state.posts()[2].caption, caption);
}

#[test]
fn a_paste_beyond_the_limit_is_cut_and_windows_line_breaks_are_normalised() {
    let profile = profile("editor-limit");
    let pictures = copy_of_loose(&profile);
    let mut harness = harness(app_with(&profile, &pictures));
    settle_idle(&mut harness);

    open_editor_from_feed(&mut harness);
    assert_eq!(field_text(&harness), "", "a loose picture starts without a description");
    assert_eq!(count(&harness, "0 / 10,000"), 1);
    assert_eq!(count(&harness, "Kept on this computer with the picture."), 1);

    // Text copied in Windows has CR LF line breaks.
    harness.event(Event::Paste("one\r\ntwo\rthree".to_owned()));
    harness.run_steps(3);
    assert_eq!(field_text(&harness), "one\ntwo\nthree");
    assert_eq!(count(&harness, "13 / 10,000"), 1);
    // The caret is behind what was pasted.
    field(&harness).type_text("!");
    harness.run_steps(3);
    assert_eq!(field_text(&harness), "one\ntwo\nthree!");

    // 10,001 more units do not fit: the paste is cut, what was there stays.
    harness.event(Event::Paste("x".repeat(10_001)));
    harness.run_steps(3);
    let text = field_text(&harness);
    assert_eq!(text::utf16_len(&text), 10_000);
    assert!(text.starts_with("one\ntwo\nthree!xxx"));
    assert_eq!(count(&harness, "10,000 / 10,000"), 1);
    // Typing into the full field changes nothing, and an emoji is never cut in half.
    field(&harness).type_text("y");
    harness.run_steps(2);
    field(&harness).type_text("🥐");
    harness.run_steps(3);
    assert_eq!(field_text(&harness), text);

    // The full text can be saved.
    harness.key_press_modifiers(Modifiers::COMMAND, Key::Enter);
    settle_idle(&mut harness);
    assert!(harness.state().state.editor.is_none());
    assert_eq!(text::utf16_len(&harness.state().state.posts()[0].caption), 10_000);
}

#[test]
fn an_input_method_and_the_tab_key_work_in_the_dialog() {
    let profile = profile("editor-ime");
    let pictures = copy_of_loose(&profile);
    let mut harness = harness(app_with(&profile, &pictures));
    settle_idle(&mut harness);
    open_editor_from_feed(&mut harness);

    // An input method composes text in the field: the candidate shows while it is composed
    // and is replaced by what the user accepts.
    field(&harness).type_text("A");
    harness.run_steps(2);
    harness.event(Event::Ime(ImeEvent::Preedit { text: "にほ".to_owned(), active_range_chars: None }));
    harness.run_steps(2);
    harness.event(Event::Ime(ImeEvent::Preedit { text: "にほん".to_owned(), active_range_chars: None }));
    harness.run_steps(2);
    assert_eq!(field_text(&harness), "Aにほん", "the candidate is shown in the field");
    harness.event(Event::Ime(ImeEvent::Commit("日本".to_owned())));
    harness.run_steps(3);
    assert_eq!(field_text(&harness), "A日本");
    // The emoji panel of Windows (Win + .) sends its choice as text.
    field(&harness).type_text("🥐");
    harness.run_steps(3);
    assert_eq!(field_text(&harness), "A日本🥐");
    assert_eq!(count(&harness, "5 / 10,000"), 1, "three BMP characters and one emoji of two units");

    // Tab leaves the field for "Save", then "Cancel", and comes back to the field: the focus
    // stays inside the dialog.
    harness.key_press(Key::Tab);
    harness.run_steps(2);
    assert!(dialog_row(&harness, EDITOR_WIDTH, "Save").is_focused());
    harness.key_press(Key::Tab);
    harness.run_steps(2);
    assert!(dialog_row(&harness, EDITOR_WIDTH, "Cancel").is_focused());
    harness.key_press(Key::Tab);
    harness.run_steps(2);
    assert!(field(&harness).is_focused(), "the focus does not leave the dialog");
    assert_eq!(field_text(&harness), "A日本🥐", "Tab is not typed into the field");

    // Enter on the focused "Save" saves.
    harness.key_press(Key::Tab);
    harness.run_steps(2);
    harness.key_press(Key::Enter);
    settle_idle(&mut harness);
    assert!(harness.state().state.editor.is_none());
    assert_eq!(harness.state().state.posts()[0].caption, "A日本🥐");
}

#[test]
fn a_failed_save_keeps_the_dialog_and_the_text_and_says_why() {
    let profile = profile("editor-failure");
    let pictures = copy_of_loose(&profile);
    let mut harness = harness(app_with(&profile, &pictures));
    settle_idle(&mut harness);
    // The data folder cannot be written to: a file stands where the folder should be.
    let data_dir = harness.state().services.paths.data_dir.clone();
    assert!(data_dir.starts_with(&profile));
    let _ = std::fs::remove_dir_all(&data_dir);
    std::fs::write(&data_dir, b"not a folder").expect("a file in place of the data folder");

    open_editor_from_feed(&mut harness);
    field(&harness).type_text("Low tide again 🌊");
    harness.run_steps(3);
    dialog_row(&harness, EDITOR_WIDTH, "Save").click();
    harness.run_steps(4);

    let app = harness.state();
    let editor = app.state.editor.as_ref().expect("the dialog stays open");
    assert_eq!(editor.text, "Low tide again 🌊", "the typed text is kept");
    let error = editor.error.clone().expect("the reason is shown in the dialog");
    assert!(error.starts_with("The description could not be saved: "), "{error}");
    assert!(error.ends_with('.'), "{error}");
    assert_eq!(harness.query_all_by_role_and_label(Role::Alert, &error).count(), 1);
    assert!(app.state.toasts.is_empty(), "the failure is not a toast");
    assert_eq!(app.state.posts()[0].caption, "", "the post is unchanged");
    assert!(!dialog_row(&harness, EDITOR_WIDTH, "Save").accesskit_node().is_disabled(), "Save can be tried again");

    // The folder becomes writable again: the same text saves and the error is gone.
    std::fs::remove_file(&data_dir).expect("remove the blocking file");
    dialog_row(&harness, EDITOR_WIDTH, "Save").click();
    settle_idle(&mut harness);
    assert!(harness.state().state.editor.is_none());
    assert_eq!(harness.state().state.posts()[0].caption, "Low tide again 🌊");
    assert_eq!(toasts(&harness), ["Description updated."]);
    assert!(descriptions_file(&harness).contains("Low tide again 🌊"));
    // Nothing was written beside the pictures.
    assert!(!pictures.join("descriptions.json").exists());
}

#[test]
fn a_description_saved_in_the_viewer_shows_everywhere() {
    let profile = profile("editor-everywhere");
    let pictures = copy_of_loose(&profile);
    let mut app = app_with(&profile, &pictures);
    let key = app.state.posts()[1].key.clone();
    app.dispatch(Action::OpenViewer(key.clone()));
    let mut harness = harness(app);
    settle_idle(&mut harness);

    harness.state_mut().dispatch(Action::OpenEditor(key.clone()));
    harness.run_steps(4);
    field(&harness).type_text("Second picture #tidepool");
    harness.run_steps(3);
    dialog_row(&harness, EDITOR_WIDTH, "Save").click();
    settle_idle(&mut harness);

    let app = harness.state();
    assert!(app.state.editor.is_none());
    assert!(app.state.viewer.is_some(), "the viewer stays open after saving");
    assert_eq!(app.state.posts()[1].caption, "Second picture #tidepool");
    // The viewer shows the description with its hashtag as a link.
    assert!(count_containing(&harness, "Second picture") >= 1);
    assert!(harness.query_all_by_role_and_label(Role::Button, "#tidepool").count() >= 1);
    // The entry is kept under the path of the picture.
    let saved = descriptions_file(&harness);
    assert!(saved.contains("Second picture #tidepool"), "{saved}");
    assert!(saved.to_lowercase().contains("img_2.jpg"), "{saved}");

    // Saving the empty text again removes the description.
    harness.state_mut().dispatch(Action::OpenEditor(key));
    harness.run_steps(4);
    harness.state_mut().dispatch(Action::EditorInput(String::new()));
    harness.run_steps(3);
    dialog_row(&harness, EDITOR_WIDTH, "Save").click();
    settle_idle(&mut harness);
    assert_eq!(harness.state().state.posts()[1].caption, "");
    assert!(!descriptions_file(&harness).contains("Second picture"));
}

#[test]
fn the_focus_returns_to_the_button_that_opened_the_dialog() {
    let profile = profile("editor-focus");
    let mut harness = harness(app_with(&profile, &fixture("export-basic")));
    settle_idle(&mut harness);

    // With the keyboard: focus "More options", press Enter, then Escape.
    harness.get_by_role_and_label(Role::Button, "More options").focus();
    harness.run_steps(2);
    assert!(harness.get_by_role_and_label(Role::Button, "More options").is_focused());
    harness.key_press(Key::Enter);
    harness.run_steps(3);
    assert!(harness.state().state.sheet.is_some(), "Enter on the focused button opens the sheet");
    harness.key_press(Key::Escape);
    harness.run_steps(4);
    assert!(harness.state().state.sheet.is_none());
    assert!(harness.get_by_role_and_label(Role::Button, "More options").is_focused(), "the focus is back on the button");

    // Through the sheet into the editor and out again: still the same button.
    harness.key_press(Key::Enter);
    harness.run_steps(3);
    dialog_row(&harness, SHEET_WIDTH, "Edit description").click();
    harness.run_steps(4);
    assert!(field(&harness).is_focused());
    harness.key_press(Key::Escape);
    harness.run_steps(4);
    assert!(harness.state().state.editor.is_none());
    assert!(harness.get_by_role_and_label(Role::Button, "More options").is_focused());
}

#[test]
fn the_warnings_dialog_lists_five_and_all_on_request() {
    // One good picture and seven empty files with picture names: seven warnings.
    let profile = profile("warnings");
    let pictures = profile.join("pictures");
    std::fs::create_dir_all(&pictures).expect("pictures folder");
    std::fs::copy(fixture("loose").join("img_1.png"), pictures.join("good.png")).expect("copy a picture");
    for index in 1..=7 {
        std::fs::write(pictures.join(format!("empty_{index}.jpg")), b"").expect("an empty file");
    }
    let mut harness = harness(app_with(&profile, &pictures));
    settle_idle(&mut harness);

    harness.get_by_role_and_label(Role::Button, "7 warnings").click();
    harness.run_steps(4);
    assert!(harness.state().state.warnings_opened_at.is_some());
    assert_eq!(harness.query_all_by_role_and_label(Role::Dialog, "Warnings").count(), 1);
    assert_eq!(count(&harness, "Showing 1 picture from pictures"), 2, "the banner and the dialog say what is shown");
    // The first five, then the button for the rest.
    assert_eq!(count_containing(&harness, "is an empty file"), 5);
    assert_eq!(count_containing(&harness, "empty_5.jpg"), 1);
    assert_eq!(count_containing(&harness, "empty_6.jpg"), 0);
    harness.get_by_role_and_label(Role::Button, "Show all 7 warnings").click();
    harness.run_steps(4);
    assert_eq!(count_containing(&harness, "is an empty file"), 7);
    harness.get_by_role_and_label(Role::Button, "Show fewer").click();
    harness.run_steps(4);
    assert_eq!(count_containing(&harness, "is an empty file"), 5);

    // The button under the list closes the dialog.
    let close = harness.get_all_by_role_and_label(Role::Button, "Close").max_by(|a, b| a.rect().top().total_cmp(&b.rect().top())).expect("the Close button of the dialog");
    close.click();
    harness.run_steps(4);
    assert!(harness.state().state.warnings_opened_at.is_none());

    // An export with exactly five warnings shows them all and no button for more.
    let mut harness = common::harness(app_with(&profile, &fixture("export-problems")));
    settle_idle(&mut harness);
    harness.get_by_role_and_label(Role::Button, "5 warnings").click();
    harness.run_steps(4);
    assert_eq!(count_containing(&harness, "Post 2: the image"), 1);
    assert_eq!(count_containing(&harness, "Show all"), 0);
    assert_eq!(count(&harness, "Showing 4 of 7 posts from export-problems"), 2);
}
