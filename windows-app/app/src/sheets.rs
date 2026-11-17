//! The dialogs that list things or ask: the "Post options" sheet of a post, a question before
//! something is lost, and the warnings of what is open.
//!
//! The sheet is the reference web project's action sheet (`ActionSheet.tsx:50-90`,
//! `styles.css:423-430`): a column of centred rows with dividers, each shown only when it
//! applies. Of its rows this app leaves out "About this account" (there are no profile pages)
//! and adds "Show in folder". The warnings dialog borrows the look of the web project's import
//! report (`ImportModal.tsx:56-122`): the box with the first five warnings and "Show all N
//! warnings".

use egui::{pos2, vec2, Id, Rect, Sense};
use instagram_core::messages;
use instagram_core::model::Post;
use instagram_core::urls;

use crate::icons::{self, Icon, IconStyle};
use crate::state::{Action, ConfirmKind, ConfirmState, Env, PostSheet};
use crate::strings;
use crate::theme;
use crate::widgets::{self, ButtonKind, DialogLook, SheetItemKind, TextButton, TextHover};

/// Width of the action sheet.
const SHEET_WIDTH: f32 = 400.0;

// --- Post options -----------------------------------------------------------------------------------

/// One row of the "Post options" sheet.
#[derive(Debug, Clone, PartialEq)]
pub struct PostOption {
    /// The label of the row.
    pub label: String,
    /// Accent (bold, coloured) or plain.
    pub kind: SheetItemKind,
    /// What choosing the row does. Every row closes the sheet.
    pub actions: Vec<Action>,
}

/// The rows of the sheet for `post`, in the web project's order, each only when it applies
/// (design brief, "Post options"): the links of a post that has them, "Go to post" unless the
/// sheet was opened from the viewer, and what every post has.
pub fn post_options(post: &Post, from_viewer: bool) -> Vec<PostOption> {
    let key = &post.key;
    let plain = |label: &str, actions: Vec<Action>| PostOption { label: label.to_owned(), kind: SheetItemKind::Plain, actions };
    let mut options = Vec::new();
    if let Some(link) = &post.permalink {
        let label = strings::view_on(&urls::site_label(link));
        options.push(PostOption { label, kind: SheetItemKind::Accent, actions: vec![Action::ClosePostOptions, Action::OpenLink(link.clone())] });
    }
    if let Some(profile) = &post.user.profile_url {
        options.push(plain(&strings::view_profile_on(&urls::site_label(profile)), vec![Action::ClosePostOptions, Action::OpenLink(profile.clone())]));
    }
    if post.permalink.is_some() {
        options.push(plain(strings::SHEET_COPY_LINK, vec![Action::CopyLink(key.clone())]));
    }
    if !from_viewer {
        options.push(plain(strings::SHEET_GO_TO_POST, vec![Action::OpenViewer(key.clone())]));
    }
    options.push(plain(strings::SHEET_EDIT_DESCRIPTION, vec![Action::OpenEditor(key.clone())]));
    options.push(plain(strings::SHEET_DOWNLOAD_POST, vec![Action::DownloadPost(key.clone())]));
    options.push(plain(strings::SHEET_SHOW_IN_FOLDER, vec![Action::ShowInFolder(key.clone())]));
    options.push(plain(strings::SHEET_AUTO_VIEW_FROM_HERE, vec![Action::StartAutoView(Some(key.clone()))]));
    options.push(plain(strings::SHEET_CANCEL, vec![Action::ClosePostOptions]));
    options
}

/// Shows the "Post options" sheet. It is derived from the post as it is now, so it always
/// shows the current data.
pub fn post_options_sheet(ctx: &egui::Context, sheet: &PostSheet, env: &mut Env<'_>) {
    let state = env.state;
    let palette = state.palette();
    let Some(post) = state.post(&sheet.key) else {
        env.push(Action::ClosePostOptions);
        return;
    };
    let options = post_options(post, sheet.from_viewer);
    let output = widgets::sheet(ctx, Id::unique("post-options"), strings::SHEET_LABEL, SHEET_WIDTH, sheet.opened_at, state, |ui| {
        let mut chosen = None;
        let last = options.len() - 1;
        for (index, option) in options.iter().enumerate() {
            // The first row has no divider above it (`styles.css:426-427`).
            if widgets::sheet_item(ui, &option.label, option.kind, true, index > 0, index == 0, index == last, palette).clicked() {
                chosen = Some(index);
            }
        }
        chosen
    });
    if let Some(index) = output.inner {
        for action in &options[index].actions {
            env.push(action.clone());
        }
    } else if output.close_requested {
        env.push(Action::ClosePostOptions);
    }
}

// --- Questions --------------------------------------------------------------------------------------

/// The title, the message and the label of the red row of a question (of a notice: its only
/// row, "OK").
pub fn confirm_texts(kind: &ConfirmKind) -> (&'static str, String, &'static str) {
    match kind {
        ConfirmKind::ClosePictures => (strings::CONFIRM_CLOSE_TITLE, strings::CONFIRM_CLOSE_MESSAGE.to_owned(), strings::CONFIRM_CLOSE_ACCEPT),
        ConfirmKind::ReplaceParts { count, first } => (strings::CONFIRM_REPLACE_TITLE, strings::confirm_replace_message(*count, first), strings::CONFIRM_REPLACE_ACCEPT),
        ConfirmKind::StoreNotice(message) => (strings::NOTICE_STORE_TITLE, message.clone(), strings::NOTICE_OK),
    }
}

/// Shows a question in the shape of the web project's confirm dialog (`ConfirmDialog.tsx`,
/// `styles.css:547-554`): a centred title and a muted message, then the red row that goes
/// ahead and "Cancel". Escape and a click beside it cancel. A notice has a single plain row,
/// "OK", that closes it like Escape does.
pub fn confirm_dialog(ctx: &egui::Context, confirm: &ConfirmState, env: &mut Env<'_>) {
    let state = env.state;
    let palette = state.palette();
    let (title, message, accept_label) = confirm_texts(&confirm.kind);
    let mut accept = false;
    let output = widgets::sheet(ctx, Id::unique("confirm-dialog"), title, SHEET_WIDTH, confirm.opened_at, state, |ui| {
        let width = ui.available_width();
        let id = Id::unique("confirm");
        // `.confirm__text`: `padding: 24px 24px 18px`, `gap: 8px`, centred.
        let text_width = width - 48.0;
        let title_galley = centered(ctx, title, theme::semibold(18.0), palette.text, theme::normal_line_height(18.0), text_width);
        let message_galley = centered(ctx, &message, theme::regular(14.0), palette.muted, 20.3, text_width);
        let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0 + title_galley.size().y + 8.0 + message_galley.size().y + 18.0), Sense::hover());
        let mut y = rect.top() + 24.0;
        for (galley, text, salt) in [(title_galley, title, "title"), (message_galley, message.as_str(), "message")] {
            let height = galley.size().y;
            widgets::paint_galley(ui.painter(), pos2(rect.center().x, y), galley);
            widgets::text_label(ui, Rect::from_min_size(pos2(rect.left() + 24.0, y), vec2(text_width, height)), id.with(salt), text);
            y += height + 8.0;
        }
        if matches!(confirm.kind, ConfirmKind::StoreNotice(_)) {
            return widgets::sheet_item(ui, accept_label, SheetItemKind::Plain, true, true, false, true, palette).clicked();
        }
        accept = widgets::sheet_item(ui, accept_label, SheetItemKind::Danger, true, true, false, false, palette).clicked();
        widgets::sheet_item(ui, strings::CONFIRM_CANCEL, SheetItemKind::Plain, true, true, false, true, palette).clicked()
    });
    if accept {
        env.push(Action::ConfirmAccept);
    } else if output.inner || output.close_requested {
        env.push(Action::ConfirmCancel);
    }
}

// --- Warnings ---------------------------------------------------------------------------------------

/// Height of the header: 8 px padding around the 32 px close button, and the border below.
const HEADER_HEIGHT: f32 = 49.0;
/// Padding of the body at the top (`.import-modal__body--success`).
const BODY_PADDING_TOP: f32 = 36.0;
/// Padding of the body at the sides.
const BODY_PADDING_X: f32 = 24.0;
/// Padding of the body at the bottom.
const BODY_PADDING_BOTTOM: f32 = 32.0;
/// Space between the parts of the body (`gap: 14px`).
const BODY_GAP: f32 = 14.0;
/// Line height of the sentence above the box (16 px, `line-height: 1.4`).
const STATUS_LINE_HEIGHT: f32 = 22.4;
/// Padding of the box of warnings (`padding: 12px 14px`).
const BOX_PADDING_X: f32 = 14.0;
const BOX_PADDING_Y: f32 = 12.0;
/// Font size of the box.
const BOX_SIZE: f32 = 13.0;
/// Line height of the box (`line-height: 1.45`).
const BOX_LINE_HEIGHT: f32 = 18.85;
/// Space under the title of the box.
const BOX_TITLE_MARGIN: f32 = 6.0;
/// Indent of the list (`padding-left: 18px`).
const LIST_INDENT: f32 = 18.0;
/// Space between two warnings (`gap: 4px`).
const LIST_GAP: f32 = 4.0;
/// Space above "Show all N warnings" (`margin-top: 8px`).
const MORE_MARGIN: f32 = 8.0;
/// Line height of that button (12 px, the box's 1.45).
const MORE_LINE_HEIGHT: f32 = 17.4;

/// Shows the dialog with the warnings of the open collection: what is shown, then the first
/// five warnings and, when there are more, "Show all N warnings" / "Show fewer".
pub fn warnings_dialog(ctx: &egui::Context, opened_at: f64, env: &mut Env<'_>) {
    let state = env.state;
    let palette = state.palette();
    let Some(collection) = state.collection.as_ref().filter(|collection| !collection.warnings.is_empty()) else {
        env.push(Action::CloseWarnings);
        return;
    };
    let total = collection.warnings.len();
    let shown = if state.warnings_all { total } else { total.min(messages::MAX_WARNINGS_SHOWN) };
    let status = messages::collection_banner(collection);
    let count_label = messages::warnings_label(total);

    let mut close = false;
    let mut toggle = false;
    let output = widgets::dialog(ctx, Id::unique("warnings-dialog"), strings::WARNINGS_TITLE, DialogLook::report(), opened_at, state, |ui| {
        let width = ui.available_width();
        let id = Id::unique("warnings");

        // --- Header: the title centred, the round close button on the right ---
        let (header, _) = ui.allocate_exact_size(vec2(width, HEADER_HEIGHT), Sense::hover());
        let painter = ui.painter();
        let title = widgets::line(ctx, strings::WARNINGS_TITLE, theme::semibold(16.0), palette.text, theme::normal_line_height(16.0));
        let title_center_y = header.top() + (HEADER_HEIGHT - 1.0) / 2.0;
        let title_rect = widgets::paint_line(painter, header.center().x - title.size().x / 2.0, title_center_y, title);
        widgets::text_label(ui, title_rect, id.with("title"), strings::WARNINGS_TITLE);
        painter.rect_filled(Rect::from_min_size(pos2(header.left(), header.bottom() - 1.0), vec2(width, 1.0)), 0.0, palette.border);
        let close_center = pos2(header.right() - 10.0 - 16.0, title_center_y);
        if widgets::icon_button(ui, close_center, id.with("close-icon"), Icon::Close, IconStyle::outline(20.0), palette.text, strings::WARNINGS_CLOSE).clicked() {
            close = true;
        }

        // --- Body: the sentence, the box, the button ---
        let body_width = width - 2.0 * BODY_PADDING_X;
        let status_galley = centered(ctx, &status, theme::semibold(16.0), palette.text, STATUS_LINE_HEIGHT, body_width);
        let box_width = body_width - 2.0 * BOX_PADDING_X;
        let warning_galleys: Vec<_> = collection
            .warnings
            .iter()
            .take(shown)
            .map(|warning| (widgets::wrapped(ctx, &warning.message, theme::regular(BOX_SIZE), palette.text, BOX_LINE_HEIGHT, box_width - LIST_INDENT), warning.message.as_str()))
            .collect();
        let list_height: f32 = warning_galleys.iter().map(|(galley, _)| galley.size().y).sum::<f32>() + LIST_GAP * warning_galleys.len().saturating_sub(1) as f32;
        let has_more = total > messages::MAX_WARNINGS_SHOWN;
        let box_height = BOX_PADDING_Y + BOX_LINE_HEIGHT + BOX_TITLE_MARGIN + list_height + if has_more { MORE_MARGIN + MORE_LINE_HEIGHT } else { 0.0 } + BOX_PADDING_Y;
        let button_size = widgets::button_size(ctx, strings::WARNINGS_CLOSE);
        let button_height = button_size.y.max(36.0);
        let body_height = BODY_PADDING_TOP + status_galley.size().y + BODY_GAP + box_height + BODY_GAP + button_height + BODY_PADDING_BOTTOM;
        let (body, _) = ui.allocate_exact_size(vec2(width, body_height.ceil()), Sense::hover());
        let painter = ui.painter();

        let mut y = body.top() + BODY_PADDING_TOP;
        let status_rect = Rect::from_min_size(pos2(body.left() + BODY_PADDING_X, y), vec2(body_width, status_galley.size().y));
        widgets::paint_galley(painter, pos2(body.center().x, y), status_galley.clone());
        widgets::text_label(ui, status_rect, id.with("status"), &status);
        y += status_galley.size().y + BODY_GAP;

        // `.import-warnings`: a soft box with the title, the bulleted list and the toggle.
        let box_rect = Rect::from_min_size(widgets::snap(ctx, pos2(body.left() + BODY_PADDING_X, y)), vec2(body_width, box_height));
        painter.rect_filled(box_rect, 8.0, palette.soft);
        let content_left = box_rect.left() + BOX_PADDING_X;
        let mut line_y = box_rect.top() + BOX_PADDING_Y;
        icons::paint(painter, Icon::Warning, pos2(content_left + 8.0, line_y + BOX_LINE_HEIGHT / 2.0), IconStyle::outline(16.0), palette.text);
        let count_galley = widgets::line(ctx, &count_label, theme::semibold(BOX_SIZE), palette.text, BOX_LINE_HEIGHT);
        let count_rect = widgets::paint_line(painter, content_left + 16.0 + 6.0, line_y + BOX_LINE_HEIGHT / 2.0, count_galley);
        widgets::text_label(ui, count_rect, id.with("count"), &count_label);
        line_y += BOX_LINE_HEIGHT + BOX_TITLE_MARGIN;
        for (index, (galley, message)) in warning_galleys.iter().enumerate() {
            let pos = widgets::snap(ctx, pos2(content_left + LIST_INDENT, line_y));
            // The bullet of a list item, centred on the first line.
            painter.circle_filled(pos2(pos.x - 9.5, pos.y + BOX_LINE_HEIGHT / 2.0 + 0.5), 2.5, palette.text);
            widgets::paint_galley(painter, pos, galley.clone());
            widgets::text_label(ui, Rect::from_min_size(pos, galley.size()), id.with(("warning", index)), message);
            line_y += galley.size().y + LIST_GAP;
        }
        if has_more {
            let label = if state.warnings_all { messages::SHOW_FEWER_WARNINGS.to_owned() } else { messages::show_all_warnings(total) };
            let button = TextButton { label: &label, font: theme::semibold(12.0), line_height: MORE_LINE_HEIGHT, color: palette.accent_text, hover: TextHover::Underline, icon: None, tooltip: None, enabled: true, leading_space: 0.0 };
            let pos = widgets::snap(ctx, pos2(content_left, line_y - LIST_GAP + MORE_MARGIN));
            if button.show(ui, pos, id.with("toggle"), palette).clicked() {
                toggle = true;
            }
        }
        y += box_height + BODY_GAP;

        // The button under the box (`.btn-blue.import-btn`: at least 36 px high).
        let button_pos = widgets::snap(ctx, pos2(body.center().x - button_size.x / 2.0, y + (button_height - button_size.y) / 2.0));
        if widgets::button(ui, button_pos, id.with("close"), strings::WARNINGS_CLOSE, ButtonKind::Primary, palette).clicked() {
            close = true;
        }
    });
    if toggle {
        env.push(Action::ToggleAllWarnings);
    }
    if close || output.close_requested {
        env.push(Action::CloseWarnings);
    }
}

/// Text wrapped at `max_width` with every line centred. Paint it at the horizontal centre.
fn centered(ctx: &egui::Context, text: &str, font: egui::FontId, color: egui::Color32, line_height: f32, max_width: f32) -> std::sync::Arc<egui::Galley> {
    let mut job = egui::epaint::text::LayoutJob::single_section(text.to_owned(), widgets::format(font, color, line_height));
    job.wrap = egui::epaint::text::TextWrapping { max_width, ..Default::default() };
    job.halign = egui::Align::Center;
    widgets::layout_job(ctx, job)
}

#[cfg(test)]
mod tests {
    use super::*;
    use instagram_core::model::{MediaItem, MediaKind, MediaType, PostKey, User};
    use std::path::PathBuf;

    fn post(permalink: Option<&str>, profile_url: Option<&str>) -> Post {
        let path = PathBuf::from("C:\\pictures\\a.jpg");
        Post {
            key: PostKey::for_file(&path),
            id: "a".to_owned(),
            permalink: permalink.map(str::to_owned),
            media: vec![MediaItem { kind: MediaKind::Image, path, manifest_path: "a.jpg".to_owned(), poster: None, alt: None, bytes: 1 }],
            user: User { profile_url: profile_url.map(str::to_owned), ..User::named("tester") },
            caption: String::new(),
            original_caption: String::new(),
            comment_count: 0,
            like_count: None,
            taken_at: None,
            media_type: MediaType::Image,
            alt: None,
            location: None,
            context: Some("a.jpg".to_owned()),
            comments: Vec::new(),
            origin: instagram_core::model::PostOrigin::File,
        }
    }

    fn labels(options: &[PostOption]) -> Vec<&str> {
        options.iter().map(|option| option.label.as_str()).collect()
    }

    #[test]
    fn a_post_with_links_has_every_row_in_the_web_projects_order() {
        let post = post(Some("https://www.instagram.com/p/FXTbasic001/"), Some("https://www.instagram.com/fixture.sunrise.bakery/"));
        let options = post_options(&post, false);
        assert_eq!(
            labels(&options),
            ["View on Instagram", "View profile on Instagram", "Copy link", "Go to post", "Edit description", "Download post", "Show in folder", "Auto view from here", "Cancel"]
        );
        // Only the first row is the accent row.
        assert_eq!(options.iter().filter(|option| option.kind == SheetItemKind::Accent).count(), 1);
        assert_eq!(options[0].kind, SheetItemKind::Accent);
        assert_eq!(options[0].actions, [Action::ClosePostOptions, Action::OpenLink("https://www.instagram.com/p/FXTbasic001/".to_owned())]);
        // From the viewer there is no "Go to post".
        assert_eq!(
            labels(&post_options(&post, true)),
            ["View on Instagram", "View profile on Instagram", "Copy link", "Edit description", "Download post", "Show in folder", "Auto view from here", "Cancel"]
        );
    }

    #[test]
    fn a_loose_picture_has_no_link_rows() {
        let post = post(None, None);
        assert_eq!(labels(&post_options(&post, false)), ["Go to post", "Edit description", "Download post", "Show in folder", "Auto view from here", "Cancel"]);
        assert_eq!(labels(&post_options(&post, true)), ["Edit description", "Download post", "Show in folder", "Auto view from here", "Cancel"]);
    }

    #[test]
    fn a_link_on_another_site_is_named_by_its_host() {
        // Without a permalink there is no "Copy link", but the profile row stays.
        let post = post(None, Some("https://example.com/someone"));
        assert_eq!(labels(&post_options(&post, true))[0], "View profile on example.com");
        let post = self::post(Some("https://photos.example.org/p/1"), None);
        assert_eq!(&labels(&post_options(&post, true))[..2], ["View on photos.example.org", "Copy link"]);
    }
}
