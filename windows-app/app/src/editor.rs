//! The "Edit description" dialog: the reference web project's `EditCaptionDialog.tsx`
//! (`styles.css:556-566`, spec-edit sections 3 to 6 and 13).
//!
//! A sheet of 520 px: the title, "Post by **username**", a field of seven rows, the note about
//! where the text is kept beside the counter, a line for the reason of a failed save, and the
//! rows "Save" and "Cancel". Enter starts a new line, Ctrl+Enter saves, Escape, "Cancel" and a
//! click beside the dialog drop the typed text.
//!
//! The web project gets three things from its `<textarea>` that a native field does not give:
//! the limit of 10,000 UTF-16 code units (enforced while typing and pasting, never by a
//! message), line breaks that are always `\n`, and the caret that stays where the user typed.
//! `apply_input` reproduces them; it is pure and unit tested. The text itself lives in
//! `AppState` like everything else: the field edits a copy and reports the change as an action.

use std::sync::Arc;

use egui::accesskit::Role;
use egui::epaint::text::{LayoutJob, TextWrapping};
use egui::epaint::StrokeKind;
use egui::text::{CCursor, CCursorRange};
use egui::{pos2, vec2, Color32, Galley, Id, Key, Modifiers, Rect, Sense, Stroke, Ui, UiBuilder, WidgetInfo};
use instagram_core::manifest::MAX_CAPTION_CHARS;
use instagram_core::messages;
use instagram_core::text;

use crate::state::{Action, EditorState, Env};
use crate::strings;
use crate::theme::{self, Palette};
use crate::widgets::{self, SheetItemKind};

// --- Geometry (styles.css:556-566) ----------------------------------------------------------------

/// Width of the dialog.
const WIDTH: f32 = 520.0;
/// Padding of the body: `22px 20px 16px`.
const BODY_PADDING_TOP: f32 = 22.0;
const BODY_PADDING_X: f32 = 20.0;
const BODY_PADDING_BOTTOM: f32 = 16.0;
/// Space between the parts of the body (`gap: 10px`).
const BODY_GAP: f32 = 10.0;
/// Font size of the title.
const TITLE_SIZE: f32 = 18.0;
/// Font size of "Post by ...".
const AUTHOR_SIZE: f32 = 13.0;
/// Font size of the field.
const FIELD_SIZE: f32 = 14.0;
/// Line height of the field (`line-height: 1.45`).
const FIELD_LINE_HEIGHT: f32 = 20.3;
/// Rows the field shows (`rows=7`).
const FIELD_ROWS: f32 = 7.0;
/// Padding of the field (`padding: 10px 12px`), inside its 1 px border.
const FIELD_PADDING_X: f32 = 12.0;
const FIELD_PADDING_Y: f32 = 10.0;
/// Width of the field's border.
const FIELD_BORDER: f32 = 1.0;
/// Corner radius of the field.
const FIELD_RADIUS: f32 = 8.0;
/// The field may grow to this fraction of the window height (`max-height: 50vh`).
const FIELD_MAX_WINDOW_FRACTION: f32 = 0.5;
/// Font size of the note and the counter.
const META_SIZE: f32 = 12.0;
/// Space between the note and the counter (`gap: 12px`).
const META_GAP: f32 = 12.0;
/// Font size of the error line.
const ERROR_SIZE: f32 = 13.0;
/// Line height of the error line (`line-height: 1.4`).
const ERROR_LINE_HEIGHT: f32 = 18.2;

/// The height of the field with seven rows: the rows, the padding and the border.
fn field_min_height() -> f32 {
    FIELD_ROWS * FIELD_LINE_HEIGHT + 2.0 * (FIELD_PADDING_Y + FIELD_BORDER)
}

// --- What a text area does by itself ----------------------------------------------------------------

/// The field after a change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldEdit {
    /// What the field holds now.
    pub text: String,
    /// Where the caret belongs, as an index of `char`s, when the text is not what the user's
    /// keystroke or paste produced (line breaks were normalised, or the limit cut it).
    pub caret: Option<usize>,
}

/// The number of bytes `left` and `right` have in common at their start, on a `char` boundary.
fn common_prefix(left: &str, right: &str) -> usize {
    left.char_indices().zip(right.chars()).find(|((_, a), b)| a != b).map_or_else(|| left.len().min(right.len()), |((index, _), _)| index)
}

/// The number of bytes `left` and `right` have in common at their end, on a `char` boundary.
fn common_suffix(left: &str, right: &str) -> usize {
    let mut bytes = 0;
    for (a, b) in left.chars().rev().zip(right.chars().rev()) {
        if a != b {
            break;
        }
        bytes += a.len_utf8();
    }
    bytes
}

/// What the field holds after the user changed `before` into `after` (typing, pasting, an
/// input method), as a `<textarea maxLength=10000>` would have it (spec-edit 13.1):
///
/// * `\r\n` and lone `\r` become `\n`;
/// * the text never exceeds 10,000 UTF-16 code units: of what was typed or pasted only as much
///   as fits is taken, cut at a `char` boundary, and the text around it stays whole.
pub fn apply_input(before: &str, after: &str) -> FieldEdit {
    let normalized = text::normalize_newlines(after);
    let within_limit = text::utf16_len(&normalized) <= MAX_CAPTION_CHARS;
    if within_limit && normalized == after {
        return FieldEdit { text: normalized, caret: None };
    }
    // What was inserted is what lies between the start and the end the two texts share.
    let prefix = common_prefix(before, &normalized);
    let suffix = common_suffix(&before[prefix..], &normalized[prefix..]);
    let inserted = &normalized[prefix..normalized.len() - suffix];
    let (head, tail) = (&normalized[..prefix], &normalized[normalized.len() - suffix..]);
    let room = MAX_CAPTION_CHARS.saturating_sub(text::utf16_len(head) + text::utf16_len(tail));
    let kept = text::truncate_utf16(inserted, room);
    let mut result = String::with_capacity(head.len() + kept.len() + tail.len());
    result.push_str(head);
    result.push_str(kept);
    let caret = result.chars().count();
    result.push_str(tail);
    // The text before the change was within the limit, so this never cuts; it is the guarantee.
    text::enforce_caption_limit(&mut result);
    let caret = caret.min(result.chars().count());
    FieldEdit { text: result, caret: Some(caret) }
}

/// Lays the text of the field out as the field shows it.
fn field_galley(ctx: &egui::Context, field_text: &str, wrap_width: f32, color: Color32) -> Arc<Galley> {
    let mut job = LayoutJob { wrap: TextWrapping { max_width: wrap_width, ..TextWrapping::default() }, ..LayoutJob::default() };
    // Spaces at the end of a line stay visible to the caret.
    job.keep_trailing_whitespace = true;
    widgets::append_text(&mut job, field_text, &widgets::format(theme::regular(FIELD_SIZE), color, FIELD_LINE_HEIGHT));
    widgets::layout_job(ctx, job)
}

/// The id of the field of the editor with this serial number.
pub fn field_id(serial: u64) -> Id {
    Id::unique(("edit-description-field", serial))
}

/// Shows the "Edit description" dialog.
pub fn editor_dialog(ctx: &egui::Context, editor: &EditorState, env: &mut Env<'_>) {
    let state = env.state;
    let palette = state.palette();
    let Some(post) = state.post(&editor.key) else {
        env.push(Action::CloseEditor);
        return;
    };
    let changed = text::caption_changed(&editor.text, &post.caption);
    let screen_height = ctx.content_rect().height();

    // Ctrl+Enter saves. It is taken before the field sees it, which would add a line break.
    let mut save = ctx.input_mut(|input| input.consume_key(Modifiers::COMMAND, Key::Enter)) && changed;
    let mut cancel = false;
    let mut input: Option<String> = None;

    let output = widgets::sheet(ctx, Id::unique("edit-description"), strings::EDIT_TITLE, WIDTH, editor.opened_at, state, |ui| {
        let width = ui.available_width();
        let content_width = width - 2.0 * BODY_PADDING_X;
        let id = Id::unique("edit-description-body");

        let title = widgets::line(ctx, strings::EDIT_TITLE, theme::semibold(TITLE_SIZE), palette.text, theme::normal_line_height(TITLE_SIZE));
        let author = author_galley(ctx, &post.user.username, palette, content_width);
        let note_text = messages::edit_note_for(post);
        let counter_text = messages::caption_counter(&editor.text);
        let counter = widgets::line(ctx, &counter_text, theme::regular(META_SIZE), palette.muted, theme::normal_line_height(META_SIZE));
        let note = widgets::wrapped(ctx, note_text, theme::regular(META_SIZE), palette.muted, theme::normal_line_height(META_SIZE), content_width - META_GAP - counter.size().x);
        let error = editor.error.as_deref().map(|message| centered(ctx, message, theme::regular(ERROR_SIZE), theme::DANGER_RED, ERROR_LINE_HEIGHT, content_width));

        // The field shows seven rows and grows with a longer text up to half the window; beyond
        // that it scrolls. It never grows so far that the dialog with Save and Cancel would not
        // fit the window: then the field scrolls, not the dialog.
        let text_width = content_width - 2.0 * (FIELD_PADDING_X + FIELD_BORDER);
        let text_height = field_galley(ctx, &editor.text, text_width, palette.text).size().y;
        let meta_height = note.size().y.max(counter.size().y);
        let error_height = error.as_ref().map_or(0.0, |galley| BODY_GAP + galley.size().y);
        let around_field = BODY_PADDING_TOP + title.size().y + BODY_GAP + author.size().y + BODY_GAP + BODY_GAP + meta_height + error_height + BODY_PADDING_BOTTOM;
        let rows_height = 2.0 * widgets::sheet_item_height(true);
        let room = widgets::dialog_max_height(screen_height) - around_field - rows_height;
        let field_max_height = (screen_height * FIELD_MAX_WINDOW_FRACTION).min(room).max(field_min_height());
        let field_height = (text_height + 2.0 * (FIELD_PADDING_Y + FIELD_BORDER)).clamp(field_min_height(), field_max_height);
        let body_height = around_field + field_height;
        let (body, _) = ui.allocate_exact_size(vec2(width, body_height), Sense::hover());
        let left = body.left() + BODY_PADDING_X;
        let painter = ui.painter();

        // Title and author line, centred.
        let mut y = body.top() + BODY_PADDING_TOP;
        let title_height = title.size().y;
        let title_rect = widgets::paint_line(painter, body.center().x - title.size().x / 2.0, y + title_height / 2.0, title);
        widgets::text_label(ui, title_rect, id.with("title"), strings::EDIT_TITLE);
        y += title_height + BODY_GAP;
        let author_text = format!("{}{}", strings::EDIT_POST_BY, post.user.username);
        let author_height = author.size().y;
        let author_rect = widgets::paint_line(painter, body.center().x - author.size().x / 2.0, y + author_height / 2.0, author);
        widgets::text_label(ui, author_rect, id.with("author"), &author_text);
        y += author_height + BODY_GAP;

        // The field: a box with a 1 px border that turns muted with the focus (`:focus`).
        let field_rect = Rect::from_min_size(widgets::snap(ctx, pos2(left, y)), vec2(content_width, field_height));
        let field = field_id(editor.serial);
        let focused = ctx.memory(|memory| memory.has_focus(field));
        painter.rect_filled(field_rect, FIELD_RADIUS, palette.surface);
        painter.rect_stroke(field_rect, FIELD_RADIUS, Stroke::new(FIELD_BORDER, if focused { palette.muted } else { palette.border }), StrokeKind::Inside);
        let mut field_text = editor.text.clone();
        let inner = field_rect.shrink(FIELD_BORDER);
        // A child of its own: the field is placed by rectangle and must not move the place
        // where the rows below the body go.
        let mut field_ui = ui.new_child(UiBuilder::new().max_rect(inner));
        let field_output = egui::ScrollArea::vertical()
            .id_salt(("edit-description-scroll", editor.serial))
            .max_height(inner.height())
            .auto_shrink([false, false])
            .show(&mut field_ui, |ui| {
                let mut layouter = |ui: &Ui, buffer: &dyn egui::TextBuffer, wrap_width: f32| field_galley(ui.ctx(), buffer.as_str(), wrap_width, palette.text);
                // The padding lies inside the border, which `inner` has left out already.
                let margin = egui::Margin { left: FIELD_PADDING_X as i8, right: FIELD_PADDING_X as i8, top: FIELD_PADDING_Y as i8, bottom: FIELD_PADDING_Y as i8 };
                egui::TextEdit::multiline(&mut field_text)
                    .id(field)
                    .frame(egui::Frame::NONE.inner_margin(margin))
                    .font(theme::regular(FIELD_SIZE))
                    .text_color(palette.text)
                    .hint_text(egui::RichText::new(strings::EDIT_PLACEHOLDER).color(palette.muted))
                    .desired_width(f32::INFINITY)
                    // The field fills its box, so that a click anywhere in the box reaches it.
                    // Only the height is forced: the width is what the scroll bar leaves.
                    .min_size(vec2(0.0, inner.height()))
                    .layouter(&mut layouter)
                    .show(ui)
            })
            .inner;
        // The name of the field for screen readers (`aria-label="Description"`).
        field_output.response.widget_info(|| {
            let mut info = WidgetInfo::labeled(Role::MultilineTextInput, true, strings::EDIT_FIELD_LABEL);
            info.current_text_value = Some(field_text.clone());
            info
        });

        // On opening, the field takes the focus with the caret at the end of the text.
        let focus_marker = Id::unique("edit-description-focused");
        if ctx.data(|data| data.get_temp::<u64>(focus_marker)) != Some(editor.serial) {
            ctx.data_mut(|data| data.insert_temp(focus_marker, editor.serial));
            field_output.response.request_focus();
            let mut field_state = field_output.state.clone();
            field_state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(field_text.chars().count()))));
            field_state.store(ctx, field);
        }
        if field_text != editor.text {
            let edit = apply_input(&editor.text, &field_text);
            if let Some(caret) = edit.caret {
                // The text is not what the keystroke produced: put the caret where it belongs.
                let mut field_state = field_output.state.clone();
                field_state.cursor.set_char_range(Some(CCursorRange::one(CCursor::new(caret))));
                field_state.store(ctx, field);
            }
            input = Some(edit.text);
        }
        y += field_height + BODY_GAP;

        // The note on the left, the counter on the right.
        let painter = ui.painter();
        let note_pos = widgets::snap(ctx, pos2(left, y));
        widgets::text_label(ui, Rect::from_min_size(note_pos, note.size()), id.with("note"), note_text);
        widgets::paint_galley(painter, note_pos, note);
        let counter_rect = widgets::paint_line(painter, left + content_width - counter.size().x, y + counter.size().y / 2.0, counter);
        widgets::text_label(ui, counter_rect, id.with("counter"), &counter_text);
        y += meta_height;

        // Why the last save failed (`role="alert"`).
        if let (Some(galley), Some(message)) = (error, &editor.error) {
            y += BODY_GAP;
            let rect = Rect::from_min_size(pos2(left, y), vec2(content_width, galley.size().y));
            widgets::paint_galley(painter, pos2(body.center().x, y), galley);
            ui.interact(rect, id.with("error"), Sense::hover()).widget_info(|| WidgetInfo::labeled(Role::Alert, true, message));
        }

        // "Save" has a divider above it, as every sheet row after a body block.
        if widgets::sheet_item(ui, strings::EDIT_SAVE, SheetItemKind::Accent, changed, true, false, false, palette).clicked() {
            save = true;
        }
        if widgets::sheet_item(ui, strings::EDIT_CANCEL, SheetItemKind::Plain, true, true, false, true, palette).clicked() {
            cancel = true;
        }
    });

    if let Some(field_text) = input {
        env.push(Action::EditorInput(field_text));
    }
    if save {
        env.push(Action::SaveDescription);
    } else if cancel || output.close_requested {
        env.push(Action::CloseEditor);
    }
}

/// "Post by **username**": muted, the name in the text colour and semibold; a very long name is
/// cut with an ellipsis.
fn author_galley(ctx: &egui::Context, username: &str, palette: &Palette, max_width: f32) -> Arc<Galley> {
    let line_height = theme::normal_line_height(AUTHOR_SIZE);
    let mut job = LayoutJob { wrap: TextWrapping { max_width, max_rows: 1, break_anywhere: true, ..TextWrapping::default() }, break_on_newline: false, ..LayoutJob::default() };
    job.append(strings::EDIT_POST_BY, 0.0, widgets::format(theme::regular(AUTHOR_SIZE), palette.muted, line_height));
    job.append(username, 0.0, widgets::format(theme::semibold(AUTHOR_SIZE), palette.text, line_height));
    widgets::layout_job(ctx, job)
}

/// Text wrapped at `max_width` with every line centred. Paint it at the horizontal centre.
fn centered(ctx: &egui::Context, message: &str, font: egui::FontId, color: Color32, line_height: f32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(message.to_owned(), widgets::format(font, color, line_height));
    job.wrap = TextWrapping { max_width, ..TextWrapping::default() };
    job.halign = egui::Align::Center;
    widgets::layout_job(ctx, job)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn edit(before: &str, after: &str) -> (String, Option<usize>) {
        let result = apply_input(before, after);
        (result.text, result.caret)
    }

    #[test]
    fn ordinary_typing_is_taken_as_it_is() {
        assert_eq!(edit("", "a"), ("a".to_owned(), None));
        assert_eq!(edit("Croissants", "Croissants 🥐\nBaked at dawn."), ("Croissants 🥐\nBaked at dawn.".to_owned(), None));
        // Deleting everything is fine too.
        assert_eq!(edit("abc", ""), (String::new(), None));
    }

    #[test]
    fn pasted_windows_line_breaks_become_plain_ones_and_the_caret_follows() {
        // "one\r\ntwo\rthree" pasted between "<" and ">".
        let (text, caret) = edit("<>", "<one\r\ntwo\rthree>");
        assert_eq!(text, "<one\ntwo\nthree>");
        assert_eq!(caret, Some("<one\ntwo\nthree".chars().count()));
    }

    #[test]
    fn a_paste_that_does_not_fit_is_cut_and_the_text_around_it_stays() {
        let before = format!("{}|{}", "a".repeat(4_000), "z".repeat(4_000));
        // 3,000 characters pasted at the bar: only 1,999 fit.
        let after = format!("{}{}|{}", "a".repeat(4_000), "p".repeat(3_000), "z".repeat(4_000));
        let (text, caret) = edit(&before, &after);
        assert_eq!(text::utf16_len(&text), 10_000);
        assert_eq!(text, format!("{}{}|{}", "a".repeat(4_000), "p".repeat(1_999), "z".repeat(4_000)));
        assert_eq!(caret, Some(5_999));
    }

    #[test]
    fn ten_thousand_and_one_units_are_cut_to_the_limit() {
        // Into an empty field.
        let (text, caret) = edit("", &"x".repeat(10_001));
        assert_eq!(text.len(), 10_000);
        assert_eq!(caret, Some(10_000));
        // Typing one more character into a full field changes nothing.
        let full = "x".repeat(10_000);
        let (text, caret) = edit(&full, &format!("{full}y"));
        assert_eq!(text, full);
        assert_eq!(caret, Some(10_000));
        // In the middle of a full field as well: the caret stays where it was.
        let after = format!("{}y{}", "x".repeat(5_000), "x".repeat(5_000));
        let (text, caret) = edit(&full, &after);
        assert_eq!(text, full);
        assert_eq!(caret, Some(5_000));
    }

    #[test]
    fn the_limit_counts_utf16_units_and_never_splits_a_character() {
        // An emoji counts two units: 9,999 units of text leave room for one unit, not for it.
        let before = "x".repeat(9_999);
        let (text, caret) = edit(&before, &format!("{before}🥐"));
        assert_eq!(text, before);
        assert_eq!(caret, Some(9_999));
        // 5,000 emoji are exactly 10,000 units; one more does not fit.
        let emoji = "🥐".repeat(5_000);
        assert_eq!(edit("", &emoji), (emoji.clone(), None));
        let (text, caret) = edit(&emoji, &format!("{emoji}🥐"));
        assert_eq!(text::utf16_len(&text), 10_000);
        assert_eq!(caret, Some(5_000));
        // A paste of emoji into room for three units takes one emoji.
        let before = "x".repeat(9_997);
        let (text, _) = edit(&before, &format!("🥐🥐🥐{before}"));
        assert_eq!(text, format!("🥐{before}"));
    }

    #[test]
    fn replacing_a_selection_makes_room_for_the_new_text() {
        // The middle 2,000 of a full field are selected and replaced by 2,500 characters:
        // 2,000 of them fit.
        let before = format!("{}{}{}", "a".repeat(4_000), "s".repeat(2_000), "z".repeat(4_000));
        let after = format!("{}{}{}", "a".repeat(4_000), "n".repeat(2_500), "z".repeat(4_000));
        let (text, caret) = edit(&before, &after);
        assert_eq!(text, format!("{}{}{}", "a".repeat(4_000), "n".repeat(2_000), "z".repeat(4_000)));
        assert_eq!(caret, Some(6_000));
    }

    #[test]
    fn the_shared_start_and_end_are_found_on_character_boundaries() {
        assert_eq!(common_prefix("añb", "añc"), "añ".len());
        assert_eq!(common_prefix("abc", "abc"), 3);
        assert_eq!(common_prefix("", "abc"), 0);
        // "é" and "è" share their first byte; the prefix stops before the character.
        assert_eq!(common_prefix("xé", "xè"), 1);
        assert_eq!(common_suffix("bña", "cña"), "ña".len());
        assert_eq!(common_suffix("", "a"), 0);
    }

    #[test]
    fn seven_rows_make_the_field_of_the_web_project() {
        // 7 x 20.3 + 2 x 10 + 2 x 1 = 164.1 (measured: 164.08).
        assert!((field_min_height() - 164.1).abs() < 0.01);
    }
}
