//! Auto view, the slide show: the setup dialog and the running show (the reference web project's
//! `AutoView.tsx` and `styles/autoview.css`, spec-viewer 4, with its video option).
//!
//! The setup dialog is a sheet: the seconds per picture with five quick choices, the three
//! options ("Let videos play to the end", loop, shuffle), "Start" and "Cancel". A video plays
//! while the show runs, looping unless the show waits for its end (`autoview::plan_slide`); its
//! controls bar appears under the pointer, M switches the sound (video brief 5.6). The show covers the whole window and is always dark, in both
//! appearances: a 3 px progress bar, a top bar with the author, the counter and the X, the
//! picture contained on black, two lines about it, and the row of controls. After the last
//! picture, with the loop off, the end screen offers "Restart" and "Exit".
//!
//! The order, the position and the clock live in `state.rs` (`AutoViewRun`, `tick_auto_view`);
//! the rules themselves are `instagram_core::autoview`. This module only paints what the state
//! says and turns buttons and keys into actions. A frame is asked for when the progress bar
//! would have moved by a pixel, never more often, and not at all while the show stands still.

use std::sync::Arc;

use egui::accesskit::Role;
use egui::epaint::text::{LayoutJob, TextWrapping};
use egui::epaint::StrokeKind;
use egui::text::{CCursor, CCursorRange};
use egui::{pos2, vec2, Align2, Color32, Galley, Id, Key, Modifiers, Painter, Pos2, Rect, Sense, Stroke, Ui, UiBuilder, Vec2, WidgetInfo};
use instagram_core::autoview::{self as rules, INTERVAL_PRESETS, MAX_INTERVAL_SEC, MIN_INTERVAL_SEC};
use instagram_core::messages;
use instagram_core::model::Post;
use instagram_core::text;
use instagram_core::urls;

use crate::feed;
use crate::icons::{self, Icon, IconStyle};
use crate::loader::Priority;
use crate::media::{self, CarouselChrome};
use crate::state::{Action, AppState, AutoViewRun, AutoViewSetup, Env, PlayerCommand};
use crate::strings;
use crate::theme::{self, Palette};
use crate::video::stage::{self as video_stage, StageMemory, StageOptions};
use crate::video::Surface;
use crate::viewer::{self, Contained};
use crate::widgets::{self, SheetItemKind, TextButton, TextHover};
use crate::zoom::Zoom;

// --- Setup dialog (autoview.css:4-31) ---------------------------------------------------------------

/// Width of the setup dialog. The style sheet says 420 px, but the sheet rule that follows it
/// wins in the browser: the dialog is 400 px wide there, as every sheet.
const SETUP_WIDTH: f32 = 400.0;
/// Padding of the body: `20px 20px 18px`.
const SETUP_PADDING_TOP: f32 = 20.0;
const SETUP_PADDING_X: f32 = 20.0;
const SETUP_PADDING_BOTTOM: f32 = 18.0;
/// Space between the parts of the body (`gap: 14px`).
const SETUP_GAP: f32 = 14.0;
/// Space between the parts of the field group (`gap: 8px`).
const FIELD_GAP: f32 = 8.0;
/// The seconds field: `width: 8em` of 16 px, `padding: 8px 10px`, a 1 px border, radius 8.
const INPUT_WIDTH: f32 = 128.0;
const INPUT_HEIGHT: f32 = 39.0;
const INPUT_PADDING_X: f32 = 10.0;
const INPUT_PADDING_Y: f32 = 8.0;
const INPUT_FONT_SIZE: f32 = 16.0;
/// The seconds field takes at most this many characters: more can never be a valid time.
const INPUT_MAX_CHARS: usize = 8;
/// A quick choice: `min-height: 32px; padding: 6px 12px; border-radius: 16px`, 13 px semibold.
const CHIP_HEIGHT: f32 = 32.0;
const CHIP_PADDING_X: f32 = 12.0;
const CHIP_GAP: f32 = 6.0;
/// Line height of the lead and of the checkbox labels (14 px, `line-height: 1.4`).
const SETUP_LINE_HEIGHT: f32 = 19.6;
/// Line height of the hint (12 px, `line-height: 1.4`).
const HINT_LINE_HEIGHT: f32 = 16.8;
/// Line height of the error line (13 px, `line-height: 1.4`).
const ERROR_LINE_HEIGHT: f32 = 18.2;
/// Line height of the line under the video option (12 px, `line-height: 1.4`).
const WAIT_NOTE_LINE_HEIGHT: f32 = 16.8;
/// Side of a checkbox, and the space between it and its label.
const CHECKBOX_SIZE: f32 = 18.0;
const CHECKBOX_GAP: f32 = 10.0;
/// The border of an empty checkbox, as the browser draws it.
const CHECKBOX_BORDER: Color32 = Color32::from_rgb(0x76, 0x76, 0x76);
/// The error line: `#c22b38`, and `#ff7a85` in the dark appearance.
const ERROR_LIGHT: Color32 = Color32::from_rgb(0xC2, 0x2B, 0x38);
const ERROR_DARK: Color32 = Color32::from_rgb(0xFF, 0x7A, 0x85);
/// A quick choice of the dark appearance under the pointer.
const CHIP_DARK_HOVER: Color32 = Color32::from_rgb(0x4A, 0x4A, 0x4A);

/// The id of the seconds field of the setup dialog with this serial number.
pub fn setup_field_id(serial: u64) -> Id {
    Id::unique(("auto-view-seconds", serial))
}

/// Text wrapped at `max_width`; with `center` every line is centred around the anchor it is
/// painted at.
fn paragraph(ctx: &egui::Context, text: &str, font: egui::FontId, color: Color32, line_height: f32, max_width: f32, center: bool) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), widgets::format(font, color, line_height));
    job.wrap = TextWrapping { max_width, ..TextWrapping::default() };
    if center {
        job.halign = egui::Align::Center;
    }
    widgets::layout_job(ctx, job)
}

/// One checkbox row of the setup dialog (`.av-setup__check`): the whole row toggles.
pub(crate) fn checkbox_row(ui: &Ui, rect: Rect, id: Id, label: &str, checked: bool, palette: &Palette) -> bool {
    let response = widgets::checkbox_area(ui, rect, id, label, checked);
    let painter = ui.painter();
    // `margin: 1px 0 0`: the box sits one pixel below the top of the row.
    let square = Rect::from_min_size(widgets::snap(ui.ctx(), rect.min + vec2(0.0, 1.0)), Vec2::splat(CHECKBOX_SIZE));
    if checked {
        painter.rect_filled(square, 3.0, palette.blue);
        icons::paint(painter, Icon::Check, square.center(), IconStyle::outline(14.0).with_stroke_width(3.0), Color32::WHITE);
    } else {
        painter.rect_filled(square, 3.0, palette.input);
        painter.rect_stroke(square, 3.0, Stroke::new(1.0, CHECKBOX_BORDER), StrokeKind::Inside);
    }
    let galley = widgets::line(ui.ctx(), label, theme::regular(14.0), palette.text, SETUP_LINE_HEIGHT);
    widgets::paint_line(painter, rect.left() + CHECKBOX_SIZE + CHECKBOX_GAP, rect.top() + SETUP_LINE_HEIGHT / 2.0, galley);
    widgets::focus_ring(ui, &response, square, 3.0, theme::FOCUS_RING_OFFSET);
    response.clicked()
}

/// Shows the setup dialog of auto view.
pub fn setup_dialog(ctx: &egui::Context, setup: &AutoViewSetup, env: &mut Env<'_>) {
    let state = env.state;
    let palette = state.palette();
    let post_count = state.posts().len();
    let field = setup_field_id(setup.serial);

    // Enter in the seconds field is "Start" (the form is submitted). It is taken before the
    // field sees it.
    let field_focused = ctx.memory(|memory| memory.has_focus(field));
    let mut start = field_focused && ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter));
    let mut cancel = false;
    let mut actions: Vec<Action> = Vec::new();

    let output = widgets::sheet(ctx, Id::unique("auto-view-setup"), strings::AUTO_VIEW_TITLE, SETUP_WIDTH, setup.opened_at, state, |ui| {
        let width = ui.available_width();
        let content_width = width - 2.0 * SETUP_PADDING_X;
        let id = Id::unique("auto-view-setup-body");

        let title = widgets::line(ctx, strings::AUTO_VIEW_TITLE, theme::semibold(18.0), palette.text, theme::normal_line_height(18.0));
        let lead_text = messages::auto_view_lead(post_count);
        let lead = paragraph(ctx, &lead_text, theme::regular(14.0), palette.muted, SETUP_LINE_HEIGHT, content_width, true);
        let label = widgets::line(ctx, strings::AUTO_VIEW_TIME_LABEL, theme::semibold(14.0), palette.text, theme::normal_line_height(14.0));
        let hint = paragraph(ctx, messages::AUTO_VIEW_HINT, theme::regular(12.0), palette.muted, HINT_LINE_HEIGHT, content_width, false);
        let error_color = if palette.dark { ERROR_DARK } else { ERROR_LIGHT };
        let error = setup.invalid.then(|| paragraph(ctx, messages::AUTO_VIEW_INTERVAL_ERROR, theme::regular(13.0), error_color, ERROR_LINE_HEIGHT, content_width, false));

        let field_group_height = label.size().y + FIELD_GAP + INPUT_HEIGHT + FIELD_GAP + CHIP_HEIGHT + FIELD_GAP + hint.size().y + error.as_ref().map_or(0.0, |galley| FIELD_GAP + galley.size().y);
        // `.av-setup__sub`: the line under the video option, muted, 12 px.
        let wait_note = paragraph(ctx, strings::AUTO_VIEW_WAIT_FOR_VIDEOS_NOTE, theme::regular(12.0), palette.muted, WAIT_NOTE_LINE_HEIGHT, content_width - CHECKBOX_SIZE - CHECKBOX_GAP, false);
        let wait_height = SETUP_LINE_HEIGHT + wait_note.size().y;
        let checks_height = wait_height + 2.0 * SETUP_LINE_HEIGHT + 2.0 * SETUP_GAP;
        let body_height = SETUP_PADDING_TOP + title.size().y + SETUP_GAP + lead.size().y + SETUP_GAP + field_group_height + SETUP_GAP + checks_height + SETUP_PADDING_BOTTOM;
        let (body, _) = ui.allocate_exact_size(vec2(width, body_height.ceil()), Sense::hover());
        let left = body.left() + SETUP_PADDING_X;
        let painter = ui.painter();

        // Title and lead, centred.
        let mut y = body.top() + SETUP_PADDING_TOP;
        let title_height = title.size().y;
        let title_rect = widgets::paint_line(painter, body.center().x - title.size().x / 2.0, y + title_height / 2.0, title);
        widgets::text_label(ui, title_rect, id.with("title"), strings::AUTO_VIEW_TITLE);
        y += title_height + SETUP_GAP;
        let lead_height = lead.size().y;
        widgets::paint_galley(painter, pos2(body.center().x, y), lead);
        widgets::text_label(ui, Rect::from_min_size(pos2(left, y), vec2(content_width, lead_height)), id.with("lead"), &lead_text);
        y += lead_height + SETUP_GAP;

        // The label and the seconds field with "seconds" after it.
        let label_height = label.size().y;
        widgets::paint_line(painter, left, y + label_height / 2.0, label);
        y += label_height + FIELD_GAP;
        let input_rect = Rect::from_min_size(widgets::snap(ctx, pos2(left, y)), vec2(INPUT_WIDTH, INPUT_HEIGHT));
        painter.rect_filled(input_rect, 8.0, palette.input);
        painter.rect_stroke(input_rect, 8.0, Stroke::new(1.0, if setup.invalid { theme::DANGER_RED } else { palette.border }), StrokeKind::Inside);
        if field_focused {
            // `outline: 2px solid var(--ig-blue); outline-offset: 1px`.
            painter.rect_stroke(input_rect.expand(1.0), 9.0, Stroke::new(2.0, palette.blue), StrokeKind::Outside);
        }
        let mut field_text = setup.text.clone();
        let mut field_ui = ui.new_child(UiBuilder::new().max_rect(input_rect.shrink(1.0)));
        let margin = egui::Margin { left: INPUT_PADDING_X as i8, right: INPUT_PADDING_X as i8, top: INPUT_PADDING_Y as i8 - 1, bottom: INPUT_PADDING_Y as i8 - 1 };
        let field_output = egui::TextEdit::singleline(&mut field_text)
            .id(field)
            .frame(egui::Frame::NONE.inner_margin(margin))
            .font(theme::regular(INPUT_FONT_SIZE))
            .text_color(palette.text)
            .char_limit(INPUT_MAX_CHARS)
            .desired_width(f32::INFINITY)
            .show(&mut field_ui);
        field_output.response.widget_info(|| {
            let mut info = WidgetInfo::labeled(Role::TextInput, true, strings::AUTO_VIEW_TIME_LABEL);
            info.current_text_value = Some(field_text.clone());
            info
        });
        // On opening, the field takes the focus with its text selected.
        let focus_marker = Id::unique("auto-view-setup-focused");
        if ctx.data(|data| data.get_temp::<u64>(focus_marker)) != Some(setup.serial) {
            ctx.data_mut(|data| data.insert_temp(focus_marker, setup.serial));
            field_output.response.request_focus();
            let mut field_state = field_output.state.clone();
            field_state.cursor.set_char_range(Some(CCursorRange::two(CCursor::new(0), CCursor::new(field_text.chars().count()))));
            field_state.store(ctx, field);
        }
        if field_text != setup.text {
            actions.push(Action::AutoViewInput(field_text));
        }
        let painter = ui.painter();
        let seconds = widgets::line(ctx, strings::AUTO_VIEW_SECONDS, theme::regular(14.0), palette.muted, theme::normal_line_height(14.0));
        widgets::paint_line(painter, input_rect.right() + FIELD_GAP, input_rect.center().y, seconds);
        y += INPUT_HEIGHT + FIELD_GAP;

        // The quick choices; the one that equals the field is pressed.
        let typed = text::js_trim(&setup.text);
        let mut chip_left = left;
        for preset in INTERVAL_PRESETS {
            let chip_label = rules::format_interval(preset);
            let pressed = typed == preset.to_string();
            let chip_id = id.with(("preset", preset));
            let measure = widgets::line(ctx, &chip_label, theme::semibold(13.0), palette.text, theme::normal_line_height(13.0));
            let rect = Rect::from_min_size(widgets::snap(ctx, pos2(chip_left, y)), vec2(measure.size().x + 2.0 * CHIP_PADDING_X, CHIP_HEIGHT));
            let response = widgets::hit(ui, rect, chip_id, &chip_label);
            let (fill, color) = match (pressed, response.hovered(), palette.dark) {
                (true, _, _) => (palette.text, palette.elevated),
                // In the dark appearance the soft colour is the colour of the sheet itself.
                (false, false, true) => (palette.soft_hover, palette.text),
                (false, true, true) => (CHIP_DARK_HOVER, palette.text),
                (false, false, false) => (palette.soft, palette.text),
                (false, true, false) => (palette.soft_hover, palette.text),
            };
            let painter = ui.painter();
            painter.rect_filled(rect, CHIP_HEIGHT / 2.0, fill);
            let galley = widgets::line(ctx, &chip_label, theme::semibold(13.0), color, theme::normal_line_height(13.0));
            widgets::paint_line(painter, rect.left() + CHIP_PADDING_X, rect.center().y, galley);
            widgets::focus_ring(ui, &response, rect, CHIP_HEIGHT / 2.0, theme::FOCUS_RING_OFFSET);
            if response.clicked() {
                actions.push(Action::AutoViewPreset(preset));
            }
            chip_left = rect.right() + CHIP_GAP;
        }
        y += CHIP_HEIGHT + FIELD_GAP;

        // The hint, and under it the error of a "Start" with unusable seconds.
        let painter = ui.painter();
        let hint_height = hint.size().y;
        widgets::paint_galley(painter, pos2(left, y), hint);
        widgets::text_label(ui, Rect::from_min_size(pos2(left, y), vec2(content_width, hint_height)), id.with("hint"), messages::AUTO_VIEW_HINT);
        y += hint_height;
        if let Some(galley) = error {
            y += FIELD_GAP;
            let rect = Rect::from_min_size(pos2(left, y), vec2(content_width, galley.size().y));
            y += galley.size().y;
            widgets::paint_galley(painter, rect.min, galley);
            ui.interact(rect, id.with("error"), Sense::hover()).widget_info(|| WidgetInfo::labeled(Role::Alert, true, messages::AUTO_VIEW_INTERVAL_ERROR));
        }
        y += SETUP_GAP;

        // The three options, the video option first (`AutoView.tsx:154-166`). The whole row,
        // its line under it included, toggles.
        let wait_rect = Rect::from_min_size(pos2(left, y), vec2(content_width, wait_height));
        if checkbox_row(ui, wait_rect, id.with("wait"), strings::AUTO_VIEW_WAIT_FOR_VIDEOS, setup.wait_for_videos, palette) {
            actions.push(Action::AutoViewWaitForVideos(!setup.wait_for_videos));
        }
        let note_top = y + SETUP_LINE_HEIGHT;
        let note_rect = Rect::from_min_size(pos2(left + CHECKBOX_SIZE + CHECKBOX_GAP, note_top), wait_note.size());
        widgets::paint_galley(ui.painter(), note_rect.min, wait_note);
        widgets::text_label(ui, note_rect, id.with("wait-note"), strings::AUTO_VIEW_WAIT_FOR_VIDEOS_NOTE);
        y += wait_height + SETUP_GAP;
        let loop_rect = Rect::from_min_size(pos2(left, y), vec2(content_width, SETUP_LINE_HEIGHT));
        if checkbox_row(ui, loop_rect, id.with("loop"), strings::AUTO_VIEW_LOOP, setup.loop_posts, palette) {
            actions.push(Action::AutoViewLoop(!setup.loop_posts));
        }
        y += SETUP_LINE_HEIGHT + SETUP_GAP;
        let shuffle_rect = Rect::from_min_size(pos2(left, y), vec2(content_width, SETUP_LINE_HEIGHT));
        if checkbox_row(ui, shuffle_rect, id.with("shuffle"), strings::AUTO_VIEW_SHUFFLE, setup.shuffle, palette) {
            actions.push(Action::AutoViewShuffle(!setup.shuffle));
        }

        // "Start" cannot be used while there is nothing to show.
        if widgets::sheet_item(ui, strings::AUTO_VIEW_START, SheetItemKind::Accent, post_count > 0, true, false, false, palette).clicked() {
            start = true;
        }
        if widgets::sheet_item(ui, strings::AUTO_VIEW_CANCEL, SheetItemKind::Plain, true, true, false, true, palette).clicked() {
            cancel = true;
        }
    });

    for action in actions {
        env.push(action);
    }
    if start {
        env.push(Action::AutoViewBegin);
    } else if cancel || output.close_requested {
        env.push(Action::CloseAutoViewSetup);
    }
}

// --- The running show (autoview.css:33-94) ----------------------------------------------------------

/// Height of the progress bar.
const PROGRESS_HEIGHT: f32 = 3.0;
/// Height of the top bar (`min-height: 56px`).
const TOP_HEIGHT: f32 = 56.0;
/// Padding of the top bar: `6px 8px 6px 16px`.
const TOP_PADDING_LEFT: f32 = 16.0;
const TOP_PADDING_RIGHT: f32 = 8.0;
/// Space between the parts of the top bar (`gap: 10px`).
const TOP_GAP: f32 = 10.0;
/// Size of the avatar in the top bar.
const TOP_AVATAR_SIZE: f32 = 32.0;
/// Diameter of the round icon buttons (`.av__icon-btn`).
const ICON_BUTTON_SIZE: f32 = 44.0;
/// Diameter of the play / pause button (`.av__play`).
const PLAY_BUTTON_SIZE: f32 = 52.0;
/// Diameter of the two buttons of the stepper (`.av__step`).
const STEP_BUTTON_SIZE: f32 = 40.0;
/// Width of the time between them (`min-width: 5.5em` of 13 px).
const INTERVAL_VALUE_WIDTH: f32 = 71.5;
/// Padding of the stepper pill and the space between its parts.
const INTERVAL_PADDING: f32 = 2.0;
/// Space between the controls (`gap: 6px 12px`).
const CONTROLS_GAP: f32 = 12.0;
/// Padding of the row of controls: `10px 16px 14px`.
const CONTROLS_PADDING_TOP: f32 = 10.0;
const CONTROLS_PADDING_BOTTOM: f32 = 14.0;
/// The widest the two lines under the picture get (`max-width: 760px`), padding included.
const INFO_MAX_WIDTH: f32 = 760.0;
/// Padding of those lines: `10px 16px 0`.
const INFO_PADDING_TOP: f32 = 10.0;
const INFO_PADDING_X: f32 = 16.0;
/// Line height of the description (14 px, `line-height: 1.4`).
const CAPTION_LINE_HEIGHT: f32 = 19.6;
/// The description is cut after this many lines (`-webkit-line-clamp: 2`).
const CAPTION_MAX_ROWS: usize = 2;
/// Space above the line of counts and links (`margin-top: 6px`).
const META_MARGIN_TOP: f32 = 6.0;
/// Line height of that line (13 px, `line-height: 1.4`).
const META_LINE_HEIGHT: f32 = 18.2;
/// Space between its parts (`gap: 4px 16px`).
const META_GAP: f32 = 16.0;
/// A post fades in for this long (`av-in .3s ease`).
const FADE_SECONDS: f64 = 0.3;
/// The focus ring inside the show: `2px solid #4cb5f9`.
const FOCUS_COLOR: Color32 = Color32::from_rgb(0x4C, 0xB5, 0xF9);
/// A control that cannot be used right now (`opacity: .35`).
const DISABLED_OPACITY: f32 = 0.35;
/// The play button under the pointer.
const PLAY_HOVER: Color32 = Color32::from_rgb(0xDB, 0xDB, 0xDB);
/// Height of the buttons of the end screen (`min-height: 40px; padding: 8px 22px`).
const END_BUTTON_HEIGHT: f32 = 40.0;
const END_BUTTON_PADDING_X: f32 = 22.0;
/// The progress bar is repainted when it would have moved by a pixel, but not more often than
/// this many seconds apart, and at least every `MAX_FRAME_SECONDS`.
const MIN_FRAME_SECONDS: f64 = 1.0 / 60.0;
/// The same on a machine that draws the window in software, where every frame of a show costs
/// a noticeable share of the processor (a tenth of a second and more): the bar moves twice a
/// second there.
const MIN_FRAME_SECONDS_SOFTWARE: f64 = 0.5;
const MAX_FRAME_SECONDS: f64 = 0.5;
/// The spinner appears when the picture on show has not arrived after this long.
const SPINNER_DELAY_SECONDS: f64 = 0.25;

/// White at `opacity`.
fn white(opacity: f32) -> Color32 {
    theme::rgba(255, 255, 255, opacity)
}

/// The focus ring of the show around `rect`.
fn focus_ring(ui: &Ui, response: &egui::Response, rect: Rect, radius: f32) {
    if response.has_focus() {
        ui.painter().rect_stroke(rect.expand(theme::FOCUS_RING_OFFSET), radius + theme::FOCUS_RING_OFFSET, Stroke::new(theme::FOCUS_RING_WIDTH, FOCUS_COLOR), StrokeKind::Outside);
    }
}

/// A round button of the show (`.av__icon-btn`): a faint disc under the pointer, or the white
/// disc of the play button. `paint` draws the glyph in the colour it is given. Returns true
/// when it was clicked.
#[allow(clippy::too_many_arguments)]
fn round_button(ui: &Ui, center: Pos2, diameter: f32, id: Id, label: &str, tooltip: &str, enabled: bool, solid: bool, paint: impl FnOnce(&Painter, Color32)) -> bool {
    let rect = Rect::from_center_size(center, Vec2::splat(diameter));
    let response = if enabled { widgets::hit(ui, rect, id, label) } else { widgets::hit_disabled(ui, rect, id, label) }.on_hover_text(tooltip);
    let hover = if enabled { widgets::transition(ui, id.with("hover"), response.hovered(), 0.15) } else { 0.0 };
    let opacity = if enabled { 1.0 } else { DISABLED_OPACITY };
    let painter = ui.painter();
    if solid {
        let fill = Color32::WHITE.lerp_to_gamma(PLAY_HOVER, hover);
        painter.circle_filled(center, diameter / 2.0, theme::with_opacity(fill, opacity));
        paint(painter, theme::with_opacity(Color32::BLACK, opacity));
    } else {
        if hover > 0.0 {
            painter.circle_filled(center, diameter / 2.0, white(0.14 * hover));
        }
        paint(painter, theme::with_opacity(Color32::WHITE, opacity));
    }
    focus_ring(ui, &response, rect, diameter / 2.0);
    enabled && response.clicked()
}

/// A button of the end screen (`.av__btn`): blue, or translucent white. Returns its width and
/// whether it was clicked.
fn end_button(ui: &Ui, min: Pos2, id: Id, label: &str, primary: bool) -> bool {
    let rect = Rect::from_min_size(min, vec2(end_button_width(ui.ctx(), label), END_BUTTON_HEIGHT));
    let response = widgets::hit(ui, rect, id, label);
    let fill = match (primary, response.hovered()) {
        (true, false) => theme::LIGHT.blue,
        (true, true) => theme::LIGHT.blue_strong,
        (false, false) => white(0.16),
        (false, true) => white(0.26),
    };
    let painter = ui.painter();
    painter.rect_filled(rect, 8.0, fill);
    let galley = widgets::line(ui.ctx(), label, theme::semibold(14.0), Color32::WHITE, theme::normal_line_height(14.0));
    widgets::paint_line(painter, rect.center().x - galley.size().x / 2.0, rect.center().y, galley);
    focus_ring(ui, &response, rect, 8.0);
    response.clicked()
}

/// The width of a button of the end screen with this label.
fn end_button_width(ctx: &egui::Context, label: &str) -> f32 {
    (widgets::line(ctx, label, theme::semibold(14.0), Color32::WHITE, theme::normal_line_height(14.0)).size().x + 2.0 * END_BUTTON_PADDING_X).ceil()
}

/// The description line of the show: the bold username, then the excerpt on one flow of text
/// (line breaks of the description are spaces here, as in the web page), cut after two lines.
fn caption_galley(ctx: &egui::Context, username: &str, caption: &str, max_width: f32) -> Option<(Arc<Galley>, String)> {
    let excerpt = text::auto_view_caption(caption)?;
    let excerpt = excerpt.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut job = LayoutJob { wrap: TextWrapping { max_width, max_rows: CAPTION_MAX_ROWS, break_anywhere: false, ..TextWrapping::default() }, break_on_newline: false, ..LayoutJob::default() };
    job.append(username, 0.0, widgets::format(theme::semibold(14.0), Color32::WHITE, CAPTION_LINE_HEIGHT));
    job.append(" ", 0.0, widgets::format(theme::regular(14.0), Color32::WHITE, CAPTION_LINE_HEIGHT));
    widgets::append_text(&mut job, &excerpt, &widgets::format(theme::regular(14.0), Color32::WHITE, CAPTION_LINE_HEIGHT));
    let label = job.text.clone();
    Some((widgets::layout_job(ctx, job), label))
}

/// Where the parts of the show are in a window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ShowLayout {
    /// The progress bar: the whole width, 3 px.
    pub progress: Rect,
    /// The top bar.
    pub top: Rect,
    /// The picture (or the end screen): what the other parts leave.
    pub stage: Rect,
    /// The two lines under the picture; empty on the end screen.
    pub info: Rect,
    /// The row of controls; empty when there is nothing to show.
    pub controls: Rect,
}

/// The geometry of the show in a window of `screen` (`autoview.css:33-94`): the bars keep
/// their heights, the lines about the picture take `info_height`, and the picture gets the
/// rest.
pub fn show_layout(screen: Rect, info_height: f32, has_controls: bool) -> ShowLayout {
    let progress = Rect::from_min_size(screen.min, vec2(screen.width(), PROGRESS_HEIGHT));
    let top = Rect::from_min_size(pos2(screen.left(), progress.bottom()), vec2(screen.width(), TOP_HEIGHT));
    let controls_height = if has_controls { CONTROLS_PADDING_TOP + PLAY_BUTTON_SIZE + CONTROLS_PADDING_BOTTOM } else { 0.0 };
    let controls = Rect::from_min_max(pos2(screen.left(), screen.bottom() - controls_height), screen.max);
    let info_top = (controls.top() - info_height).max(top.bottom());
    let info_width = INFO_MAX_WIDTH.min(screen.width());
    let info = Rect::from_min_max(pos2((screen.center().x - info_width / 2.0).round(), info_top), pos2((screen.center().x - info_width / 2.0).round() + info_width, controls.top()));
    let stage = Rect::from_min_max(pos2(screen.left(), top.bottom()), pos2(screen.right(), info_top));
    ShowLayout { progress, top, stage, info, controls }
}

/// The running show: what it remembers between frames is only whether the last frame showed
/// everything there is to show.
#[derive(Default)]
pub struct AutoViewView {
    /// Since when the picture on show could not be painted, in seconds of the app clock.
    loading_since: Option<f64>,
    /// False while something is still changing (the screenshot harness waits for it).
    settled: bool,
    /// When the controls bar of a video was last called for.
    video_memory: StageMemory,
    /// The window is cleared to black under the show (the app sets it every frame): the show
    /// then needs no black backdrop of its own.
    pub window_is_black: bool,
}

impl AutoViewView {
    /// True when the last frame showed the picture as it stays: decoded for the stage, faded in.
    pub fn is_settled(&self) -> bool {
        self.settled
    }

    /// Shows the running auto view over the whole window. Call it only while `state.auto_view`
    /// is set.
    pub fn show(&mut self, ctx: &egui::Context, env: &mut Env<'_>) {
        let state = env.state;
        let Some(run) = &state.auto_view else {
            return;
        };
        let screen = ctx.content_rect();
        let id = Id::unique("auto-view");
        let total = run.player.cursor.order.len();
        let finished = run.player.finished;
        let current = state.auto_view_post();
        // A dialog above the show has the keyboard.
        let has_keyboard = !state.modal_open();
        self.settled = true;

        let area = egui::Modal::default_area(id).anchor(Align2::LEFT_TOP, Vec2::ZERO);
        // Black under everything: the window's own clear colour when it is black already, so
        // that a software rasteriser does not fill the window twice.
        let backdrop = if self.window_is_black { Color32::TRANSPARENT } else { Color32::BLACK };
        egui::Modal::new(id).area(area).backdrop_color(backdrop).frame(egui::Frame::NONE).show(ctx, |ui| {
            ui.set_min_size(screen.size());
            ui.set_clip_rect(screen);
            // What a screen reader announces: the dialog, and what it shows right now.
            let username = current.map(|(post, _)| post.user.username.as_str());
            let status = run.player.announcement(username);
            ui.response().widget_info(|| WidgetInfo::labeled(Role::Dialog, true, strings::AUTO_VIEW_TITLE));
            // The layer swallows every click: nothing behind the show reacts.
            // The backdrop of the layer (or the window cleared to black) is the black of the
            // show; no second fill is needed.
            ui.interact(screen, id.with("background"), Sense::CLICK);

            let info = current.map(|(post, _)| InfoLines::new(ctx, post, screen.width()));
            let info_height = info.as_ref().map_or(0.0, InfoLines::height);
            let layout = show_layout(screen, info_height, total > 0);
            widgets::text_label(ui, layout.progress, id.with("status"), &status);

            self.show_top(ui, id, &layout, run, current.map(|(post, _)| post), env);
            match current {
                Some((post, slide)) => {
                    self.show_progress(ui, &layout, run, post, state);
                    self.show_stage(ui, id, &layout, run, post, slide, env);
                    if let Some(info) = info {
                        info.show(ui, id, layout.info, post, env);
                    }
                }
                None => {
                    // Only the track of the progress bar is left on the end screen.
                    ui.painter().rect_filled(layout.progress, 0.0, white(0.25));
                    self.show_end(ui, id, &layout, total, env);
                }
            }
            if total > 0 {
                show_controls(ui, id, &layout, run, env);
            }
        });

        if has_keyboard {
            self.handle_keys(ctx, finished, total, env);
        }
        self.decode_ahead(ctx, run, env);
    }

    /// The keys of the show (`AutoView.tsx:331-381`): Escape leaves, Space pauses, the arrows
    /// change the post, `+` and `-` the time. They are left alone while Alt or Ctrl is held; with
    /// a focused button Space, Enter and the arrows belong to that button, as everywhere.
    fn handle_keys(&mut self, ctx: &egui::Context, finished: bool, total: usize, env: &mut Env<'_>) {
        if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
            env.push(Action::ExitAutoView);
            return;
        }
        if ctx.input(|input| input.modifiers.alt || input.modifiers.command || input.modifiers.ctrl) {
            return;
        }
        let pressed = |key: Key| ctx.input_mut(|input| input.consume_key(Modifiers::NONE, key));
        if pressed(Key::Plus) || pressed(Key::Equals) {
            env.push(Action::AutoViewCommand(PlayerCommand::Longer));
        } else if pressed(Key::Minus) {
            env.push(Action::AutoViewCommand(PlayerCommand::Shorter));
        }
        if total == 0 || ctx.memory(|memory| memory.focused().is_some()) {
            return;
        }
        if pressed(Key::ArrowRight) {
            env.push(Action::AutoViewCommand(PlayerCommand::Next));
        } else if pressed(Key::ArrowLeft) {
            env.push(Action::AutoViewCommand(PlayerCommand::Previous));
        } else if finished {
            // "Restart" has the focus on the end screen of the web project: Enter and Space
            // start the show again.
            if pressed(Key::Enter) || pressed(Key::Space) {
                env.push(Action::AutoViewCommand(PlayerCommand::Restart));
            }
        } else if pressed(Key::Space) {
            env.push(Action::AutoViewCommand(PlayerCommand::TogglePause));
        }
    }

    /// The top bar: the author of the post on show, the counter and the X.
    fn show_top(&self, ui: &Ui, id: Id, layout: &ShowLayout, run: &AutoViewRun, post: Option<&Post>, env: &mut Env<'_>) {
        let ctx = ui.ctx().clone();
        let palette = env.palette();
        let center_y = layout.top.center().y;
        let exit_center = pos2(layout.top.right() - TOP_PADDING_RIGHT - ICON_BUTTON_SIZE / 2.0, center_y);
        let clicked = round_button(ui, exit_center, ICON_BUTTON_SIZE, id.with("exit"), strings::AUTO_VIEW_EXIT_LABEL, strings::AUTO_VIEW_EXIT_TOOLTIP, true, false, |painter, color| {
            icons::paint(painter, Icon::Close, exit_center, IconStyle::outline(24.0), color);
        });
        if clicked {
            env.push(Action::ExitAutoView);
        }
        let mut right = exit_center.x - ICON_BUTTON_SIZE / 2.0 - TOP_GAP;

        // The counter: "2 / 6", and "6 / 6" on the end screen.
        let total = run.player.cursor.order.len();
        if total > 0 {
            let number = if run.player.finished { total } else { run.player.cursor.position + 1 };
            let label = messages::auto_view_counter(number, total);
            let galley = widgets::line(&ctx, &label, theme::regular(13.0), Color32::WHITE, theme::normal_line_height(13.0));
            let size = galley.size() + vec2(20.0, 8.0);
            let pill = Rect::from_min_size(widgets::snap(&ctx, pos2(right - size.x, center_y - size.y / 2.0)), size);
            let painter = ui.painter();
            painter.rect_filled(pill, 14.0, white(0.14));
            widgets::paint_line(painter, pill.left() + 10.0, pill.center().y, galley);
            widgets::text_label(ui, pill, id.with("counter"), &label);
            right = pill.left() - TOP_GAP;
        }

        let Some(post) = post else {
            return;
        };
        let painter = ui.painter();
        let avatar_center = pos2(layout.top.left() + TOP_PADDING_LEFT + TOP_AVATAR_SIZE / 2.0, center_y);
        let avatar = feed::avatar_picture(env.loader, &ctx, post, TOP_AVATAR_SIZE, Priority::visible(1));
        // The avatar keeps the border in the surface colour of the appearance, as in the web page.
        widgets::avatar(painter, avatar_center, TOP_AVATAR_SIZE, false, &post.user.username, avatar.as_ref(), palette.surface);
        let name_left = avatar_center.x + TOP_AVATAR_SIZE / 2.0 + TOP_GAP;
        let time_label = post.taken_at.map(|taken_at| text::relative_time_label(text::now_ms(), taken_at));
        let time = time_label.as_deref().map(|label| widgets::line(&ctx, label, theme::regular(13.0), white(0.7), theme::normal_line_height(13.0)));
        let time_width = time.as_ref().map_or(0.0, |galley| TOP_GAP + galley.size().x);
        let name = widgets::line_elided(&ctx, &post.user.username, theme::semibold(14.0), Color32::WHITE, theme::normal_line_height(14.0), (right - name_left - time_width).max(24.0));
        let name_rect = widgets::paint_line(painter, name_left, center_y, name);
        widgets::text_label(ui, name_rect, id.with("username"), &post.user.username);
        if let (Some(galley), Some(label)) = (time, &time_label) {
            let rect = widgets::paint_line(painter, name_rect.right() + TOP_GAP, center_y, galley);
            widgets::text_label(ui, rect, id.with("time"), label);
        }
    }

    /// The progress bar of the post on show, and the request for the frame in which it will
    /// have moved on.
    fn show_progress(&self, ui: &Ui, layout: &ShowLayout, run: &AutoViewRun, post: &Post, state: &AppState) {
        let now_ms = state.auto_view_now_ms();
        let painter = ui.painter();
        painter.rect_filled(layout.progress, 0.0, white(0.25));
        let fraction = run.progress(post.media.len(), now_ms) as f32;
        let bar = Rect::from_min_size(layout.progress.min, vec2(layout.progress.width() * fraction, PROGRESS_HEIGHT));
        painter.rect_filled(bar, 0.0, Color32::WHITE);

        // The show moves by itself only while its clock runs. The next frame is due when the
        // bar has grown by a pixel, and at the latest when the time of the slide is up.
        if let Some(remaining_ms) = run.timer.remaining_ms(now_ms) {
            let pixels = (layout.progress.width() * ui.ctx().pixels_per_point()).max(1.0) as f64;
            let shortest = if state.software_renderer { MIN_FRAME_SECONDS_SOFTWARE } else { MIN_FRAME_SECONDS };
            let per_pixel = (run.expected_ms / 1000.0 / pixels).clamp(shortest, MAX_FRAME_SECONDS);
            let until_due = (remaining_ms / 1000.0).max(0.0) + 0.002;
            ui.ctx().request_repaint_after_secs(per_pixel.min(until_due) as f32);
        }
    }

    /// The picture of the post on show, contained on black, with the controls of a carousel
    /// and the "Paused" pill.
    #[allow(clippy::too_many_arguments)]
    fn show_stage(&mut self, ui: &Ui, id: Id, layout: &ShowLayout, run: &AutoViewRun, post: &Post, slide: usize, env: &mut Env<'_>) {
        let ctx = ui.ctx().clone();
        let state = env.state;
        let stage = layout.stage;
        if stage.height() < 1.0 {
            return;
        }
        let scale = ctx.pixels_per_point();
        let slide_count = post.media.len().max(1);
        let painter = ui.painter().with_clip_rect(stage.intersect(ui.clip_rect()));

        let contained = viewer::fitted_picture(env.loader, post, slide, stage.size(), scale, Priority::focus());
        viewer::paint_contained(&painter, stage, &contained, Zoom::FIT, scale, slide_count > 1);
        let failure = match &contained {
            Contained::Failed(message) => Some(message.as_str()),
            _ => None,
        };
        match &contained {
            Contained::Picture { exact: true, .. } | Contained::Failed(_) | Contained::NoPicture => self.loading_since = None,
            Contained::Picture { exact: false, .. } => {
                self.loading_since = None;
                self.settled = false;
            }
            Contained::Loading { .. } => {
                self.settled = false;
                // A picture that takes long to decode gets the spinner, a quick one does not.
                let since = *self.loading_since.get_or_insert(state.time);
                let waited = state.time - since;
                if waited >= SPINNER_DELAY_SECONDS {
                    widgets::spinner(&painter, widgets::snap(&ctx, stage.center()), 32.0, 3.0, state);
                } else {
                    ctx.request_repaint_after_secs((SPINNER_DELAY_SECONDS - waited) as f32);
                }
            }
        }
        widgets::picture_area(ui, stage, id.with("stage"), &media::slide_label(post, slide, failure));
        // A video plays over its poster while the show runs (video brief 5.6).
        let options = StageOptions { surface: Surface::AutoView, visit: run.player.visit, has_keyboard: !state.modal_open() };
        let poster_size = match &contained {
            Contained::Picture { source, .. } | Contained::Loading { source: Some(source) } => Some(*source),
            _ => None,
        };
        let is_video_slide = video_stage::show(ui, stage, id.with("video"), post, slide, poster_size, options, &mut self.video_memory, env).is_some();

        // A new post fades in (`av-in .3s ease`).
        if state.animations {
            let shown_for = state.auto_view_now_ms() / 1000.0 - run.post_shown_at;
            if (0.0..FADE_SECONDS).contains(&shown_for) {
                let covered = 1.0 - crate::layout::ease((shown_for / FADE_SECONDS) as f32);
                painter.rect_filled(stage, 0.0, theme::rgba(0, 0, 0, covered));
                ctx.request_repaint();
            }
        }

        if post.is_video() || is_video_slide {
            media::video_badge(ui, stage, id.with("video-badge"));
        }

        // Paging the carousel by hand starts the time of the new slide from zero.
        let chrome = CarouselChrome { frame: stage, current: slide, count: slide_count, arrow_inset: media::CAROUSEL_ARROW_INSET, hide_dots: is_video_slide };
        if let Some(wanted) = media::carousel_chrome(ui, id.with("carousel"), chrome) {
            env.push(Action::AutoViewCommand(PlayerCommand::Slide(wanted)));
        }

        if run.player.paused {
            // `.av__paused`: a pill in the middle of the picture; clicks go through it.
            let galley = widgets::line(&ctx, strings::AUTO_VIEW_PAUSED, theme::semibold(14.0), Color32::WHITE, theme::normal_line_height(14.0));
            let size = galley.size() + vec2(32.0, 16.0);
            let pill = Rect::from_min_size(widgets::snap(&ctx, stage.center() - size / 2.0), size);
            let painter = ui.painter();
            painter.rect_filled(pill, 18.0, theme::rgba(18, 18, 18, 0.78));
            widgets::paint_line(painter, pill.left() + 16.0, pill.center().y, galley);
            widgets::text_label(ui, pill, id.with("paused"), strings::AUTO_VIEW_PAUSED);
        }
    }

    /// The end screen (`.av__end`): the title, the sentence, "Restart" and "Exit"; without
    /// posts only the sentence that there is nothing to show, and "Exit".
    fn show_end(&self, ui: &Ui, id: Id, layout: &ShowLayout, total: usize, env: &mut Env<'_>) {
        let ctx = ui.ctx().clone();
        let area = layout.stage;
        let title_text = if total == 0 { messages::AUTO_VIEW_NO_POSTS } else { messages::AUTO_VIEW_END_TITLE };
        let title = widgets::line(&ctx, title_text, theme::semibold(22.0), Color32::WHITE, theme::normal_line_height(22.0));
        let sentence_text = (total > 0).then(|| messages::auto_view_end_text(total));
        let sentence = sentence_text.as_deref().map(|text| widgets::line(&ctx, text, theme::regular(14.0), white(0.75), theme::normal_line_height(14.0)));
        // `gap: 12px`; the buttons have `margin-top: 6px` on top of it.
        let height = title.size().y + sentence.as_ref().map_or(0.0, |galley| 12.0 + galley.size().y) + 12.0 + 6.0 + END_BUTTON_HEIGHT;
        let mut y = (area.center().y - height / 2.0).round().max(area.top());
        let center_x = area.center().x.round();
        let painter = ui.painter();

        let title_height = title.size().y;
        let title_rect = widgets::paint_line(painter, center_x - title.size().x / 2.0, y + title_height / 2.0, title);
        widgets::text_label(ui, title_rect, id.with("end-title"), title_text);
        y += title_height + 12.0;
        if let (Some(galley), Some(text)) = (sentence, &sentence_text) {
            let sentence_height = galley.size().y;
            let rect = widgets::paint_line(painter, center_x - galley.size().x / 2.0, y + sentence_height / 2.0, galley);
            widgets::text_label(ui, rect, id.with("end-text"), text);
            y += sentence_height + 12.0;
        }
        y += 6.0;

        let exit_width = end_button_width(&ctx, strings::AUTO_VIEW_EXIT);
        if total == 0 {
            if end_button(ui, widgets::snap(&ctx, pos2(center_x - exit_width / 2.0, y)), id.with("end-exit"), strings::AUTO_VIEW_EXIT, false) {
                env.push(Action::ExitAutoView);
            }
            return;
        }
        let restart_width = end_button_width(&ctx, strings::AUTO_VIEW_RESTART);
        let left = widgets::snap(&ctx, pos2(center_x - (restart_width + 10.0 + exit_width) / 2.0, y));
        if end_button(ui, left, id.with("end-restart"), strings::AUTO_VIEW_RESTART, true) {
            env.push(Action::AutoViewCommand(PlayerCommand::Restart));
        }
        if end_button(ui, left + vec2(restart_width + 10.0, 0.0), id.with("end-exit"), strings::AUTO_VIEW_EXIT, false) {
            env.push(Action::ExitAutoView);
        }
    }

    /// Decodes the first picture of the post that comes next ahead of time, for the size the
    /// stage has now, so that the change shows a picture and not a spinner.
    fn decode_ahead(&self, ctx: &egui::Context, run: &AutoViewRun, env: &mut Env<'_>) {
        let state = env.state;
        let order = &run.player.cursor.order;
        if run.player.finished || order.len() < 2 {
            return;
        }
        let Some(next) = rules::next_position(run.player.cursor.position, order.len(), run.settings.loop_posts) else {
            return;
        };
        let Some(post) = order.get(next).and_then(|key| state.post(key)) else {
            return;
        };
        // The stage of the next post is about as large as this one's: its two lines may differ.
        let stage = show_layout(ctx.content_rect(), INFO_PADDING_TOP + CAPTION_LINE_HEIGHT * 2.0 + META_MARGIN_TOP + META_LINE_HEIGHT, true).stage;
        let _ = viewer::fitted_picture(env.loader, post, 0, stage.size(), ctx.pixels_per_point(), Priority::near(1));
    }
}

/// The two lines under the picture (`.av__info`): the description, and the counts and links.
struct InfoLines {
    caption: Option<(Arc<Galley>, String)>,
}

impl InfoLines {
    fn new(ctx: &egui::Context, post: &Post, window_width: f32) -> InfoLines {
        let text_width = INFO_MAX_WIDTH.min(window_width) - 2.0 * INFO_PADDING_X;
        InfoLines { caption: caption_galley(ctx, &post.user.username, &post.caption, text_width) }
    }

    /// The height the lines take, padding included.
    fn height(&self) -> f32 {
        INFO_PADDING_TOP + self.caption.as_ref().map_or(0.0, |(galley, _)| galley.size().y) + META_MARGIN_TOP + META_LINE_HEIGHT
    }

    fn show(self, ui: &Ui, id: Id, rect: Rect, post: &Post, env: &mut Env<'_>) {
        let ctx = ui.ctx().clone();
        let palette = env.palette();
        let left = rect.left() + INFO_PADDING_X;
        let mut y = rect.top() + INFO_PADDING_TOP;
        if let Some((galley, label)) = self.caption {
            let pos = widgets::snap(&ctx, pos2(left, y));
            let size = galley.size();
            widgets::paint_galley(ui.painter(), pos, galley);
            widgets::text_label(ui, Rect::from_min_size(pos, size), id.with("caption"), &label);
            y += size.y;
        }
        y += META_MARGIN_TOP;
        let center_y = y + META_LINE_HEIGHT / 2.0;

        // The counts an export knows; a loose picture has neither likes nor comments.
        let mut x = left;
        let mut counts: Vec<String> = post.like_count.map(strings::auto_view_likes).into_iter().collect();
        if env.state.collection.as_ref().is_some_and(|collection| collection.is_export()) {
            counts.push(strings::auto_view_comments(post.comment_count));
        }
        for (index, label) in counts.iter().enumerate() {
            let galley = widgets::line(&ctx, label, theme::regular(13.0), white(0.75), META_LINE_HEIGHT);
            let text_rect = widgets::paint_line(ui.painter(), x, center_y, galley);
            widgets::text_label(ui, text_rect, id.with(("count", index)), label);
            x = text_rect.right() + META_GAP;
        }
        // The links: white, semibold, underlined under the pointer (`.av__link`).
        let link = |label: &str, icon: Option<(Icon, f32, bool)>| -> Vec2 {
            TextButton { label, font: theme::semibold(13.0), line_height: META_LINE_HEIGHT, color: Color32::WHITE, hover: TextHover::Underline, icon, tooltip: None, enabled: true, leading_space: 0.0 }.size(&ctx)
        };
        if let Some(permalink) = &post.permalink {
            let label = strings::view_on(&urls::site_label(permalink));
            let icon = Some((Icon::External, 14.0, false));
            let size = link(&label, icon);
            let button = TextButton { label: &label, font: theme::semibold(13.0), line_height: META_LINE_HEIGHT, color: Color32::WHITE, hover: TextHover::Underline, icon, tooltip: None, enabled: true, leading_space: 0.0 };
            if button.show(ui, widgets::snap(&ctx, pos2(x, center_y - size.y / 2.0)), id.with("permalink"), palette).clicked() {
                env.push(Action::OpenLink(permalink.clone()));
            }
            x += size.x + META_GAP;
        }
        let size = link(strings::AUTO_VIEW_OPEN_POST, None);
        let button = TextButton { label: strings::AUTO_VIEW_OPEN_POST, font: theme::semibold(13.0), line_height: META_LINE_HEIGHT, color: Color32::WHITE, hover: TextHover::Underline, icon: None, tooltip: None, enabled: true, leading_space: 0.0 };
        if button.show(ui, widgets::snap(&ctx, pos2(x, center_y - size.y / 2.0)), id.with("open-post"), palette).clicked() {
            env.push(Action::AutoViewOpenPost(post.key.clone()));
        }
    }
}

/// The row of controls (`.av__controls`): previous, play / pause, next, and the stepper of the
/// time per picture.
fn show_controls(ui: &Ui, id: Id, layout: &ShowLayout, run: &AutoViewRun, env: &mut Env<'_>) {
    let ctx = ui.ctx().clone();
    let finished = run.player.finished;
    let order_length = run.player.cursor.order.len();
    let center_y = layout.controls.top() + CONTROLS_PADDING_TOP + PLAY_BUTTON_SIZE / 2.0;
    let interval_width = 2.0 * INTERVAL_PADDING + 2.0 * STEP_BUTTON_SIZE + 2.0 * INTERVAL_PADDING + INTERVAL_VALUE_WIDTH;
    let row_width = ICON_BUTTON_SIZE + CONTROLS_GAP + PLAY_BUTTON_SIZE + CONTROLS_GAP + ICON_BUTTON_SIZE + CONTROLS_GAP + interval_width;
    let mut x = widgets::snap(&ctx, pos2(layout.controls.center().x - row_width / 2.0, 0.0)).x;
    let mut command = None;

    // Previous: from the end screen it leads back to the last post.
    let can_go_back = if finished { order_length > 0 } else { rules::previous_position(run.player.cursor.position, order_length, run.settings.loop_posts).is_some() };
    let center = pos2(x + ICON_BUTTON_SIZE / 2.0, center_y);
    if round_button(ui, center, ICON_BUTTON_SIZE, id.with("previous"), strings::AUTO_VIEW_PREVIOUS_LABEL, strings::AUTO_VIEW_PREVIOUS_TOOLTIP, can_go_back, false, |painter, color| {
        icons::paint(painter, Icon::ChevronLeft, center, IconStyle::outline(24.0).with_stroke_width(2.2), color);
    }) {
        command = Some(PlayerCommand::Previous);
    }
    x += ICON_BUTTON_SIZE + CONTROLS_GAP;

    // Play / pause: the white disc.
    let center = pos2(x + PLAY_BUTTON_SIZE / 2.0, center_y);
    let (label, tooltip, icon) = if run.player.paused {
        (strings::AUTO_VIEW_RESUME_LABEL, strings::AUTO_VIEW_RESUME_TOOLTIP, Icon::Play)
    } else {
        (strings::AUTO_VIEW_PAUSE_LABEL, strings::AUTO_VIEW_PAUSE_TOOLTIP, Icon::Pause)
    };
    if round_button(ui, center, PLAY_BUTTON_SIZE, id.with("play"), label, tooltip, !finished, true, |painter, color| {
        icons::paint(painter, icon, center, IconStyle::filled(22.0), color);
    }) {
        command = Some(PlayerCommand::TogglePause);
    }
    x += PLAY_BUTTON_SIZE + CONTROLS_GAP;

    let center = pos2(x + ICON_BUTTON_SIZE / 2.0, center_y);
    if round_button(ui, center, ICON_BUTTON_SIZE, id.with("next"), strings::AUTO_VIEW_NEXT_LABEL, strings::AUTO_VIEW_NEXT_TOOLTIP, !finished, false, |painter, color| {
        icons::paint(painter, Icon::ChevronRight, center, IconStyle::outline(24.0).with_stroke_width(2.2), color);
    }) {
        command = Some(PlayerCommand::Next);
    }
    x += ICON_BUTTON_SIZE + CONTROLS_GAP;

    // The stepper: a pill with "−", the time and "+".
    let pill = Rect::from_min_size(pos2(x, center_y - ICON_BUTTON_SIZE / 2.0), vec2(interval_width, ICON_BUTTON_SIZE));
    ui.painter().rect_filled(pill, ICON_BUTTON_SIZE / 2.0, white(0.14));
    let interval_sec = run.settings.interval_sec;
    // The signs sit a pixel above the middle of their line, as in the browser.
    let glyph = |painter: &Painter, center: Pos2, glyph: &str, color: Color32| {
        painter.text(center - vec2(0.0, 1.0), Align2::CENTER_CENTER, glyph, theme::regular(20.0), color);
    };
    let less_center = pos2(pill.left() + INTERVAL_PADDING + STEP_BUTTON_SIZE / 2.0, center_y);
    if round_button(ui, less_center, STEP_BUTTON_SIZE, id.with("less"), strings::AUTO_VIEW_LESS_LABEL, strings::AUTO_VIEW_LESS_TOOLTIP, interval_sec > MIN_INTERVAL_SEC, false, |painter, color| {
        glyph(painter, less_center, strings::AUTO_VIEW_LESS_GLYPH, color);
    }) {
        command = Some(PlayerCommand::Shorter);
    }
    let value = rules::format_interval(interval_sec);
    let value_center_x = less_center.x + STEP_BUTTON_SIZE / 2.0 + INTERVAL_PADDING + INTERVAL_VALUE_WIDTH / 2.0;
    let galley = widgets::line(&ctx, &value, theme::regular(13.0), Color32::WHITE, theme::normal_line_height(13.0));
    let value_rect = widgets::paint_line(ui.painter(), value_center_x - galley.size().x / 2.0, center_y, galley);
    widgets::text_label(ui, value_rect, id.with("interval"), &value);
    let more_center = pos2(pill.right() - INTERVAL_PADDING - STEP_BUTTON_SIZE / 2.0, center_y);
    if round_button(ui, more_center, STEP_BUTTON_SIZE, id.with("more"), strings::AUTO_VIEW_MORE_LABEL, strings::AUTO_VIEW_MORE_TOOLTIP, interval_sec < MAX_INTERVAL_SEC, false, |painter, color| {
        glyph(painter, more_center, strings::AUTO_VIEW_MORE_GLYPH, color);
    }) {
        command = Some(PlayerCommand::Longer);
    }

    if let Some(command) = command {
        env.push(Action::AutoViewCommand(command));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_show_has_the_web_projects_geometry_at_the_reference_size() {
        // 1440 x 900 with two lines of description: measured in the web project.
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 900.0));
        let info_height = INFO_PADDING_TOP + 2.0 * CAPTION_LINE_HEIGHT + META_MARGIN_TOP + META_LINE_HEIGHT;
        let layout = show_layout(screen, info_height, true);
        assert_eq!(layout.progress, Rect::from_min_size(pos2(0.0, 0.0), vec2(1440.0, 3.0)));
        assert_eq!(layout.top, Rect::from_min_size(pos2(0.0, 3.0), vec2(1440.0, 56.0)));
        assert_eq!(layout.controls, Rect::from_min_size(pos2(0.0, 824.0), vec2(1440.0, 76.0)));
        assert_eq!(layout.info.left(), 340.0);
        assert_eq!(layout.info.width(), 760.0);
        assert!((layout.info.top() - 750.6).abs() < 0.01);
        assert_eq!(layout.stage.top(), 59.0);
        assert!((layout.stage.height() - 691.6).abs() < 0.01);
    }

    #[test]
    fn the_end_screen_and_a_small_window_keep_their_parts_inside() {
        // Without posts there are no controls and no lines: the stage takes it all.
        let screen = Rect::from_min_size(pos2(0.0, 0.0), vec2(640.0, 480.0));
        let empty = show_layout(screen, 0.0, false);
        assert_eq!(empty.stage, Rect::from_min_max(pos2(0.0, 59.0), pos2(640.0, 480.0)));
        assert_eq!(empty.controls.height(), 0.0);
        // In the smallest window the lines are as wide as the window.
        let small = show_layout(screen, 73.4, true);
        assert_eq!(small.info.left(), 0.0);
        assert_eq!(small.info.width(), 640.0);
        assert!(small.stage.height() > 250.0);
    }
}
