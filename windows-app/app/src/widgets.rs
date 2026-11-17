//! The building blocks every view shares: text, buttons, the avatar, sheets and toasts.
//!
//! The reference web project is laid out in CSS pixels, so the views place their parts by
//! rectangle (1 CSS px = 1 point) instead of using egui's automatic layout, and the helpers here
//! paint into a rectangle or at a position they are given. Every interactive part goes through
//! `hit`, which gives it an accessible name (screen readers and the headless tests find widgets
//! by it), the hand cursor and keyboard focus; `focus_ring` paints the web project's 2 px blue
//! outline for keyboard focus.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use egui::accesskit::Role;
use egui::emath::TSTransform;
use egui::epaint::text::{LayoutJob, TextFormat, TextWrapping};
use egui::epaint::{RectShape, StrokeKind};
use egui::{pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, FontId, Galley, Id, Painter, Pos2, Rect, Response, Sense, Stroke, Ui, Vec2, WidgetInfo};

use crate::gradients;
use crate::icons::{self, Icon, IconStyle};
use crate::layout;
use crate::loader::Picture;
use crate::state::{AppState, SHEET_IN_SECONDS};
use crate::theme::{self, Palette};

// --- Geometry helpers ---------------------------------------------------------------------------

/// `pos` moved onto the pixel grid, so that 1 px lines and bitmaps stay sharp.
pub fn snap(ctx: &egui::Context, pos: Pos2) -> Pos2 {
    let scale = ctx.pixels_per_point();
    pos2((pos.x * scale).round() / scale, (pos.y * scale).round() / scale)
}

/// The whole texture.
pub fn full_uv() -> Rect {
    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0))
}

// --- Text ---------------------------------------------------------------------------------------

/// A text format with an explicit line height, as CSS has it: the glyphs are centred in the line.
pub fn format(font: FontId, color: Color32, line_height: f32) -> TextFormat {
    TextFormat { font_id: font, color, line_height: Some(line_height), ..TextFormat::default() }
}

/// Lays out `job`.
pub fn layout_job(ctx: &egui::Context, job: LayoutJob) -> Arc<Galley> {
    ctx.fonts_mut(|fonts| fonts.layout_job(job))
}

/// One line of text, never wrapped.
pub fn line(ctx: &egui::Context, text: &str, font: FontId, color: Color32, line_height: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), format(font, color, line_height));
    job.wrap = TextWrapping { max_width: f32::INFINITY, ..TextWrapping::default() };
    job.break_on_newline = false;
    layout_job(ctx, job)
}

/// One line of text, cut with "…" when it is wider than `max_width` (`text-overflow: ellipsis`).
pub fn line_elided(ctx: &egui::Context, text: &str, font: FontId, color: Color32, line_height: f32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), format(font, color, line_height));
    job.wrap = TextWrapping { max_width: max_width.max(0.0), max_rows: 1, break_anywhere: true, ..TextWrapping::default() };
    job.break_on_newline = false;
    layout_job(ctx, job)
}

/// Text wrapped at `max_width`, keeping its line breaks (`white-space: pre-wrap`).
pub fn wrapped(ctx: &egui::Context, text: &str, font: FontId, color: Color32, line_height: f32, max_width: f32) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), format(font, color, line_height));
    job.wrap = TextWrapping { max_width: max_width.max(0.0), ..TextWrapping::default() };
    layout_job(ctx, job)
}

/// The character that asks for the emoji form of the character before it (VARIATION SELECTOR-16).
const EMOJI_PRESENTATION_SELECTOR: char = '\u{FE0F}';

/// True for a character of a script written from right to left (Hebrew, Arabic, Syriac, Thaana,
/// N'Ko and their presentation forms; the blocks Unicode sets aside for such scripts).
fn is_right_to_left(character: char) -> bool {
    matches!(character as u32, 0x0590..=0x08FF | 0xFB1D..=0xFDFF | 0xFE70..=0xFEFC | 0x1_0800..=0x1_0FFF | 0x1_E800..=0x1_EFFF)
}

/// The stretches of `text` that are written in one direction, as byte ranges in text order,
/// with true for right to left.
///
/// egui shapes a stretch of text as a whole and takes its direction from the first letter, so
/// a Latin word or a number after Hebrew or Arabic letters would come out mirrored. Given every
/// direction as a stretch of its own, each is shaped the right way round. Letters and digits
/// decide; what lies between two stretches of right-to-left letters (spaces, punctuation)
/// belongs to them, everything else to the left-to-right text around it. The stretches stay in
/// the order of the text: there is no bidirectional reordering in egui.
fn direction_runs(text: &str) -> Vec<(std::ops::Range<usize>, bool)> {
    let mut runs: Vec<(std::ops::Range<usize>, bool)> = Vec::new();
    // Where the neutral characters since the last letter began.
    let mut neutral_start: Option<usize> = None;
    for (index, character) in text.char_indices() {
        let end = index + character.len_utf8();
        let right_to_left = is_right_to_left(character);
        if !right_to_left && !character.is_alphanumeric() {
            neutral_start.get_or_insert(index);
            continue;
        }
        let neutrals = neutral_start.take();
        match runs.last_mut() {
            // The same direction goes on; what lay between belongs to it.
            Some((range, direction)) if *direction == right_to_left => range.end = end,
            // A change of direction: the neutral characters go with the left-to-right side.
            Some((range, _)) => {
                let start = if right_to_left { index } else { neutrals.unwrap_or(index) };
                range.end = start;
                runs.push((start..end, right_to_left));
            }
            // The first letter of the text; what stands before it is left to right.
            None if right_to_left => {
                if index > 0 {
                    runs.push((0..index, false));
                }
                runs.push((index..end, true));
            }
            None => runs.push((0..end, false)),
        }
    }
    // What follows the last letter: with left-to-right text it stays; after right-to-left
    // text it is a stretch of its own (a full stop at the end of the text stands on the right).
    match (runs.last_mut(), neutral_start) {
        (Some((range, false)), _) => range.end = text.len(),
        (Some((_, true)), Some(start)) => runs.push((start..text.len(), false)),
        (Some((_, true)), None) => {}
        (None, _) if !text.is_empty() => runs.push((0..text.len(), false)),
        (None, _) => {}
    }
    runs
}

/// Appends text the user wrote to `job`: every stretch of one writing direction as a section
/// of its own (see `direction_runs`), and emoji asked for by a selector in the emoji font.
pub fn append_text(job: &mut LayoutJob, text: &str, format: &TextFormat) {
    if !text.chars().any(is_right_to_left) {
        append_text_run(job, text, format);
        return;
    }
    for (range, _) in direction_runs(text) {
        let start = job.text.len();
        append_text_run(job, &text[range], format);
        start_section_at(job, start);
    }
}

/// Makes byte `start` of the text of `job` the first byte of a section. `LayoutJob::append`
/// joins text to the section before it when the format is the same; a stretch of another
/// writing direction must not be joined.
fn start_section_at(job: &mut LayoutJob, start: usize) {
    use egui::epaint::text::{ByteIndex, LayoutSection};
    let Some(index) = job.sections.iter().position(|section| section.byte_range.start.0 < start && start < section.byte_range.end.0) else {
        return;
    };
    let end = job.sections[index].byte_range.end;
    let format = job.sections[index].format.clone();
    job.sections[index].byte_range.end = ByteIndex(start);
    job.sections.insert(index + 1, LayoutSection { leading_space: 0.0, byte_range: ByteIndex(start)..end, format });
}

/// Appends one stretch of text to `job`. A character followed by the emoji presentation
/// selector (as in "↔️" or "❤️") is taken from the emoji font even when the text font has a
/// plain glyph for it, as a browser does; egui alone would pick the first font that has it.
fn append_text_run(job: &mut LayoutJob, text: &str, format: &TextFormat) {
    if !text.contains(EMOJI_PRESENTATION_SELECTOR) {
        job.append(text, 0.0, format.clone());
        return;
    }
    let emoji_format = TextFormat { font_id: FontId::new(format.font_id.size, theme::emoji_family()), ..format.clone() };
    let mut plain_start = 0;
    let mut characters = text.char_indices().peekable();
    while let Some((index, character)) = characters.next() {
        if let Some(&(selector_index, EMOJI_PRESENTATION_SELECTOR)) = characters.peek() {
            if character != EMOJI_PRESENTATION_SELECTOR {
                if plain_start < index {
                    job.append(&text[plain_start..index], 0.0, format.clone());
                }
                let end = selector_index + EMOJI_PRESENTATION_SELECTOR.len_utf8();
                job.append(&text[index..end], 0.0, emoji_format.clone());
                plain_start = end;
                characters.next();
            }
        }
    }
    if plain_start < text.len() {
        job.append(&text[plain_start..], 0.0, format.clone());
    }
}

/// Paints a galley at `pos`, moved onto the pixel grid.
pub fn paint_galley(painter: &Painter, pos: Pos2, galley: Arc<Galley>) {
    painter.galley(snap(painter.ctx(), pos), galley, Color32::PLACEHOLDER);
}

/// Paints one line of text with its left edge at `left` and vertically centred on `center_y`.
/// Returns the rectangle of the text.
pub fn paint_line(painter: &Painter, left: f32, center_y: f32, galley: Arc<Galley>) -> Rect {
    let size = galley.size();
    let pos = snap(painter.ctx(), pos2(left, center_y - size.y / 2.0));
    painter.galley(pos, galley, Color32::PLACEHOLDER);
    Rect::from_min_size(pos, size)
}

/// The rectangles, relative to the galley, that the characters `chars` (indexes of `char`s into
/// the galley's text) occupy: one rectangle per row the range touches. This is how a link or a
/// button inside running text gets its clickable area.
pub fn char_range_rects(galley: &Galley, chars: std::ops::Range<usize>) -> Vec<Rect> {
    let mut rects = Vec::new();
    let mut row_start = 0;
    for row in &galley.rows {
        let glyph_count = row.glyphs.len();
        let row_end = row_start + glyph_count;
        let first = chars.start.max(row_start);
        let last = chars.end.min(row_end);
        if first < last {
            let left = row.glyphs[first - row_start].pos.x;
            let right = row.glyphs[last - 1 - row_start].max_x();
            rects.push(Rect::from_min_max(pos2(row.pos.x + left, row.pos.y), pos2(row.pos.x + right, row.pos.y + row.size.y)));
        }
        // The line break that ended the row is a character of the text too.
        row_start = row_end + usize::from(row.ends_with_newline);
        if row_start >= chars.end {
            break;
        }
    }
    rects
}

/// A hashtag or mention inside a piece of text.
pub struct SpanLink {
    /// Index range of its `char`s in the text of the galley.
    pub chars: std::ops::Range<usize>,
    /// The token as written, e.g. "#sourdough": its accessible name.
    pub label: String,
    /// Where it leads.
    pub url: String,
}

/// Appends a caption or a comment to `job`: plain text in the text colour, hashtags and mentions
/// in the link colour (`RichText`, `common.tsx:297-327`). The links are recorded in `links` with
/// their place in the text of the job.
pub fn append_rich(job: &mut LayoutJob, text: &str, font_size: f32, line_height: f32, palette: &Palette, links: &mut Vec<SpanLink>) {
    let plain = format(theme::regular(font_size), palette.text, line_height);
    let link_format = format(theme::regular(font_size), palette.link, line_height);
    let base_chars = job.text.chars().count();
    let mut cursor = 0;
    for link in instagram_core::text::scan_caption_links(text) {
        if link.range.start < cursor || link.range.end > text.len() {
            continue;
        }
        append_text(job, &text[cursor..link.range.start], &plain);
        let start_chars = base_chars + text[..link.range.start].chars().count();
        let token = &text[link.range.clone()];
        append_text(job, token, &link_format);
        links.push(SpanLink { chars: start_chars..start_chars + token.chars().count(), label: token.to_owned(), url: link.url() });
        cursor = link.range.end;
    }
    append_text(job, &text[cursor..], &plain);
}

/// Makes the links of a galley painted at `pos` clickable: each is a button named by its token,
/// underlined under the pointer (`.rich-link:hover`). Returns the address of a clicked link.
pub fn link_areas(ui: &Ui, galley: &Galley, pos: Pos2, links: &[SpanLink], id: Id, palette: &Palette) -> Option<String> {
    let mut clicked = None;
    for (link_index, link) in links.iter().enumerate() {
        for (part, rect) in char_range_rects(galley, link.chars.clone()).into_iter().enumerate() {
            let rect = rect.translate(pos.to_vec2());
            let response = hit(ui, rect, id.with(("link", link_index, part)), &link.label);
            if response.hovered() {
                let underline_y = (rect.bottom() - 4.0).round() + 0.5;
                ui.painter().hline(rect.x_range(), underline_y, Stroke::new(1.0, palette.link));
            }
            focus_ring(ui, &response, rect, 2.0, 0.0);
            if response.clicked() {
                clicked = Some(link.url.clone());
            }
        }
    }
    clicked
}

// --- Hit areas, focus, accessibility ------------------------------------------------------------

/// Set by the screenshot harness: remember where every named widget is, so that a scenario can
/// point at a widget by its name.
static LABEL_REGISTRY_ENABLED: AtomicBool = AtomicBool::new(false);

/// Where the named widgets of the frame being painted (or, between frames, of the last
/// painted frame) are.
#[derive(Default)]
struct LabelRegistry {
    widgets: Vec<(String, Rect)>,
}

fn label_registry(ctx: &egui::Context) -> Arc<Mutex<LabelRegistry>> {
    let id = Id::unique("instagram-windows-app/label-registry");
    ctx.data_mut(|data| data.get_temp_mut_or_default::<Arc<Mutex<LabelRegistry>>>(id).clone())
}

/// Switches the registry of widget names on (screenshot harness only).
pub fn enable_label_registry() {
    LABEL_REGISTRY_ENABLED.store(true, Ordering::Relaxed);
}

/// Starts a new frame of the registry. Call it at the start of painting.
pub fn begin_label_frame(ctx: &egui::Context) {
    if LABEL_REGISTRY_ENABLED.load(Ordering::Relaxed) {
        let registry = label_registry(ctx);
        if let Ok(mut registry) = registry.lock() {
            registry.widgets.clear();
        };
    }
}

/// The rectangles of the widgets named `label` in the last painted frame, in paint order. Call
/// it between frames (from the logic half of a frame), when that list is complete.
pub fn labelled_rects(ctx: &egui::Context, label: &str) -> Vec<Rect> {
    let registry = label_registry(ctx);
    let Ok(registry) = registry.lock() else {
        return Vec::new();
    };
    registry.widgets.iter().filter(|(name, _)| name == label).map(|(_, rect)| *rect).collect()
}

/// The names of the widgets of the last painted frame, without repeats, in paint order.
pub fn labelled_names(ctx: &egui::Context) -> Vec<String> {
    let registry = label_registry(ctx);
    let Ok(registry) = registry.lock() else {
        return Vec::new();
    };
    let mut names: Vec<String> = Vec::new();
    for (name, _) in &registry.widgets {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
}

fn register_label(ctx: &egui::Context, label: &str, rect: Rect) {
    if LABEL_REGISTRY_ENABLED.load(Ordering::Relaxed) {
        let registry = label_registry(ctx);
        if let Ok(mut registry) = registry.lock() {
            registry.widgets.push((label.to_owned(), rect));
        };
    }
}

/// A clickable area named `label`: a button for screen readers, the hand cursor, keyboard focus
/// (Tab, then Enter or Space), and scrolled into view when it receives the focus.
pub fn hit(ui: &Ui, rect: Rect, id: Id, label: &str) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, true, label));
    register_label(ui.ctx(), label, rect);
    if response.gained_focus() {
        response.scroll_to_me(None);
    }
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A clickable area that is a checkbox named `label` for screen readers: the hand cursor,
/// keyboard focus, and its state. The caller paints the box and the label.
pub fn checkbox_area(ui: &Ui, rect: Rect, id: Id, label: &str, checked: bool) -> Response {
    let response = ui.interact(rect, id, Sense::click());
    response.widget_info(|| WidgetInfo::selected(Role::CheckBox, true, checked, label));
    register_label(ui.ctx(), label, rect);
    response.on_hover_cursor(CursorIcon::PointingHand)
}

/// A button that cannot be used right now: named and visible to screen readers, not clickable.
pub fn hit_disabled(ui: &Ui, rect: Rect, id: Id, label: &str) -> Response {
    let response = ui.interact(rect, id, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Button, false, label));
    register_label(ui.ctx(), label, rect);
    response
}

/// The area of a picture, named by its alternative text. It senses clicks (a double click on a
/// feed picture likes the post) but is not a button: no hand cursor, no keyboard focus ring.
pub fn picture_area(ui: &Ui, rect: Rect, id: Id, alt: &str) -> Response {
    // Clicks only, not the keyboard focus: the picture is no stop of the Tab key (`.post__media`
    // is a plain element in the clone), and a stop without a ring would only lose the user.
    let response = ui.interact(rect, id, Sense::CLICK);
    response.widget_info(|| WidgetInfo::labeled(Role::Image, true, alt));
    register_label(ui.ctx(), alt, rect);
    response
}

/// The area of the picture in the viewer, named by its alternative text: it takes clicks (a
/// double click zooms) and drags (moving a zoomed picture), but not the keyboard focus.
pub fn picture_area_draggable(ui: &Ui, rect: Rect, id: Id, alt: &str) -> Response {
    let response = ui.interact(rect, id, Sense::CLICK | Sense::DRAG);
    response.widget_info(|| WidgetInfo::labeled(Role::Image, true, alt));
    register_label(ui.ctx(), alt, rect);
    response
}

/// A piece of text for screen readers and tests: it senses nothing but hovering.
pub fn text_label(ui: &Ui, rect: Rect, id: Id, text: &str) -> Response {
    let response = ui.interact(rect, id, Sense::hover());
    response.widget_info(|| WidgetInfo::labeled(Role::Label, true, text));
    register_label(ui.ctx(), text, rect);
    response
}

/// The outline of keyboard focus: `outline: 2px solid #0095F6; outline-offset: 2px`. A negative
/// `offset` draws it inside the widget (sheet items, tiles).
pub fn focus_ring(ui: &Ui, response: &Response, rect: Rect, radius: f32, offset: f32) {
    if response.has_focus() {
        let ring = rect.expand(offset);
        let ring_radius = (radius + offset).max(0.0);
        ui.painter().rect_stroke(ring, ring_radius, Stroke::new(theme::FOCUS_RING_WIDTH, theme::LIGHT.blue), StrokeKind::Outside);
    }
}

/// A value that follows `target` over `seconds`, for hover and press transitions. Jumps when the
/// app runs without animations.
pub fn transition(ui: &Ui, id: Id, target: bool, seconds: f32) -> f32 {
    // The app switches egui's own animation time off when it runs without animations.
    if ui.style().animation_time <= 0.0 {
        return if target { 1.0 } else { 0.0 };
    }
    ui.ctx().animate_bool_with_time(id, target, seconds)
}

// --- Buttons ------------------------------------------------------------------------------------

/// Horizontal padding of a filled button (`.btn-blue`: `padding: 8px 16px`).
const BUTTON_PADDING_X: f32 = 16.0;
/// Vertical padding of a filled button.
const BUTTON_PADDING_Y: f32 = 8.0;
/// Corner radius of a filled button.
const BUTTON_RADIUS: f32 = 8.0;
/// Font size of a filled button.
const BUTTON_FONT_SIZE: f32 = 14.0;

/// The two filled buttons of the web project.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ButtonKind {
    /// `.btn-blue`: blue, white label.
    Primary,
    /// `.btn-blue--ghost`: soft grey, text-coloured label.
    Secondary,
}

fn button_galley(ctx: &egui::Context, label: &str, color: Color32) -> Arc<Galley> {
    line(ctx, label, theme::semibold(BUTTON_FONT_SIZE), color, theme::normal_line_height(BUTTON_FONT_SIZE))
}

/// The size of a filled button with this label.
pub fn button_size(ctx: &egui::Context, label: &str) -> Vec2 {
    let text = button_galley(ctx, label, Color32::WHITE).size();
    vec2((text.x + 2.0 * BUTTON_PADDING_X).ceil(), text.y + 2.0 * BUTTON_PADDING_Y)
}

/// A filled button with its top-left corner at `min`.
pub fn button(ui: &Ui, min: Pos2, id: Id, label: &str, kind: ButtonKind, palette: &Palette) -> Response {
    let ctx = ui.ctx().clone();
    let rect = Rect::from_min_size(min, button_size(&ctx, label));
    let response = hit(ui, rect, id, label);
    let (fill, text_color) = match (kind, response.hovered()) {
        (ButtonKind::Primary, false) => (palette.blue, Color32::WHITE),
        (ButtonKind::Primary, true) => (palette.blue_strong, Color32::WHITE),
        (ButtonKind::Secondary, false) => (palette.soft, palette.text),
        (ButtonKind::Secondary, true) => (palette.soft_hover, palette.text),
    };
    let painter = ui.painter();
    painter.rect_filled(rect, BUTTON_RADIUS, fill);
    let galley = button_galley(&ctx, label, text_color);
    let text_size = galley.size();
    paint_galley(painter, rect.center() - text_size / 2.0, galley);
    focus_ring(ui, &response, rect, BUTTON_RADIUS, theme::FOCUS_RING_OFFSET);
    response
}

/// What a text button does when the pointer is over it.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TextHover {
    /// `.link-blue:hover`, `.import-banner__auto:hover`: the colour becomes the text colour.
    TextColor,
    /// `.link-strong:hover`: opacity .65.
    Fade,
    /// `.import-warnings__more:hover`: underlined.
    Underline,
}

/// A button that is only text, optionally with a small icon before it (the banner's actions).
#[derive(Debug, Clone)]
pub struct TextButton<'a> {
    pub label: &'a str,
    pub font: FontId,
    pub line_height: f32,
    pub color: Color32,
    pub hover: TextHover,
    /// An icon before the label: the glyph, its size, and whether it is filled.
    pub icon: Option<(Icon, f32, bool)>,
    /// The tooltip, when it says more than the label.
    pub tooltip: Option<&'a str>,
    /// False for a button that cannot be used right now: half transparent, not clickable.
    pub enabled: bool,
    /// Room kept free before the label (and its gap) for something the caller paints there,
    /// such as a spinner; 0 for none.
    pub leading_space: f32,
}

impl TextButton<'_> {
    /// Space between the icon and the label (`gap: 4px`).
    const ICON_GAP: f32 = 4.0;

    fn galley(&self, ctx: &egui::Context, color: Color32) -> Arc<Galley> {
        line(ctx, self.label, self.font.clone(), color, self.line_height)
    }

    /// The size the button takes.
    pub fn size(&self, ctx: &egui::Context) -> Vec2 {
        let text = self.galley(ctx, self.color).size();
        let icon = self.icon.map_or(0.0, |(_, size, _)| size + Self::ICON_GAP);
        let leading = if self.leading_space > 0.0 { self.leading_space + Self::ICON_GAP } else { 0.0 };
        vec2(text.x + icon + leading, text.y)
    }

    /// Shows the button with its top-left corner at `min`.
    pub fn show(&self, ui: &Ui, min: Pos2, id: Id, palette: &Palette) -> Response {
        let ctx = ui.ctx().clone();
        let rect = Rect::from_min_size(min, self.size(&ctx));
        let mut response = if self.enabled { hit(ui, rect, id, self.label) } else { hit_disabled(ui, rect, id, self.label) };
        if let Some(tooltip) = self.tooltip {
            response = response.on_hover_text(tooltip);
        }
        let hovered = self.enabled && response.hovered();
        let color = match (self.hover, hovered) {
            // A button that cannot be used is half transparent (`:disabled { opacity: .5 }`).
            (_, false) if !self.enabled => theme::with_opacity(self.color, 0.5),
            (_, false) | (TextHover::Underline, true) => self.color,
            (TextHover::TextColor, true) => palette.text,
            (TextHover::Fade, true) => theme::with_opacity(self.color, 0.65),
        };
        let painter = ui.painter();
        let mut left = rect.left();
        if self.leading_space > 0.0 {
            left += self.leading_space + Self::ICON_GAP;
        }
        if let Some((icon, size, filled)) = self.icon {
            icons::paint(painter, icon, pos2(left + size / 2.0, rect.center().y), IconStyle::toggled(size, filled), color);
            left += size + Self::ICON_GAP;
        }
        let text_rect = paint_line(painter, left, rect.center().y, self.galley(&ctx, color));
        if self.hover == TextHover::Underline && hovered {
            let underline_y = (text_rect.center().y + self.font.size * 0.45).round() + 0.5;
            painter.hline(text_rect.x_range(), underline_y, Stroke::new(1.0, color));
        }
        focus_ring(ui, &response, rect, 2.0, theme::FOCUS_RING_OFFSET);
        response
    }
}

/// A button that is only an icon (`.icon-btn`): it fades to 55 % under the pointer and shrinks to
/// 88 % while pressed. `label` is its accessible name and its tooltip.
pub fn icon_button(ui: &Ui, center: Pos2, id: Id, icon: Icon, style: IconStyle, color: Color32, label: &str) -> Response {
    let rect = Rect::from_center_size(center, Vec2::splat(style.size));
    let response = hit(ui, rect, id, label).on_hover_text(label);
    let hover = transition(ui, id.with("hover"), response.hovered(), 0.15);
    let press = transition(ui, id.with("press"), response.is_pointer_button_down_on(), 0.1);
    let shown = IconStyle { size: style.size * (1.0 - 0.12 * press), ..style };
    icons::paint(ui.painter(), icon, center, shown, theme::with_opacity(color, 1.0 - 0.45 * hover));
    focus_ring(ui, &response, rect, 4.0, theme::FOCUS_RING_OFFSET);
    response
}

// --- Avatar -------------------------------------------------------------------------------------

/// Width of the story ring around an avatar (`padding: 3px`).
pub const AVATAR_RING_WIDTH: f32 = 3.0;
/// Width of the surface-coloured border between ring and picture (`border: 2px solid`).
pub const AVATAR_BORDER_WIDTH: f32 = 2.0;

/// The outer diameter of an avatar of `size`, with or without the story ring.
pub fn avatar_outer_size(size: f32, ring: bool) -> f32 {
    if ring {
        size + 2.0 * AVATAR_RING_WIDTH
    } else {
        size
    }
}

/// Paints an avatar centred at `center` (`common.tsx:43-113`). `size` is the CSS size of the
/// inner circle including its 2 px border in the `surface` colour; with `ring` the conic story
/// gradient surrounds it. Without a `picture` the placeholder gradient of the username and its
/// initial letter are shown.
pub fn avatar(painter: &Painter, center: Pos2, size: f32, ring: bool, username: &str, picture: Option<&Picture>, surface: Color32) {
    let ctx = painter.ctx().clone();
    let scale = ctx.pixels_per_point();
    if ring {
        let outer = avatar_outer_size(size, true);
        let rect = Rect::from_center_size(center, Vec2::splat(outer));
        let texture = gradients::story_ring_texture(&ctx, (outer * scale).round().max(1.0) as usize);
        painter.image(texture.id(), rect, full_uv(), Color32::WHITE);
    }
    painter.circle_filled(center, size / 2.0, surface);
    let inner = size - 2.0 * AVATAR_BORDER_WIDTH;
    let inner_rect = Rect::from_center_size(center, Vec2::splat(inner));
    let round = CornerRadius::same((inner / 2.0).ceil().min(255.0) as u8);
    match picture {
        Some(picture) => {
            let uv = layout::cover_uv(vec2(picture.size_px[0] as f32, picture.size_px[1] as f32), inner_rect.size());
            painter.add(RectShape::filled(inner_rect, round, Color32::WHITE).with_texture(picture.texture.id(), uv));
        }
        None => {
            let key = gradients::placeholder_key(username);
            let texture = gradients::placeholder_texture(&ctx, username);
            painter.add(RectShape::filled(inner_rect, round, Color32::WHITE).with_texture(texture.id(), full_uv()));
            let initial = instagram_core::text::avatar_initial(username);
            let font = theme::bold((size * 0.42).max(10.0));
            painter.text(center, Align2::CENTER_CENTER, initial, font, gradients::placeholder_tint(key));
        }
    }
}

// --- Spinner, shadows ---------------------------------------------------------------------------

/// The spinner (`.spinner`): a ring in the soft colour whose top quarter is blue and turns once
/// every 0.8 s. Without animations it stands still.
pub fn spinner(painter: &Painter, center: Pos2, diameter: f32, stroke_width: f32, state: &AppState) {
    let palette = state.palette();
    let radius = (diameter - stroke_width) / 2.0;
    painter.circle_stroke(center, radius, Stroke::new(stroke_width, palette.soft));
    let turn = if state.animations { (state.time / 0.8).fract() as f32 } else { 0.0 };
    let start = turn * std::f32::consts::TAU - std::f32::consts::FRAC_PI_2 - std::f32::consts::FRAC_PI_4;
    let points: Vec<Pos2> = (0..=12).map(|step| start + std::f32::consts::FRAC_PI_2 * step as f32 / 12.0).map(|angle| center + radius * vec2(angle.cos(), angle.sin())).collect();
    painter.add(egui::Shape::line(points, Stroke::new(stroke_width, palette.blue)));
    if state.animations {
        // Thirty steps a second look like a smooth turn and leave the processor to the work
        // the spinner stands for. A machine that draws in software gets four: there every
        // frame takes the processor away from exactly that work.
        let seconds = if state.software_renderer { SPINNER_FRAME_SECONDS_SOFTWARE } else { SPINNER_FRAME_SECONDS };
        painter.ctx().request_repaint_after_secs(seconds);
    }
}

/// The time between two frames of a turning spinner.
const SPINNER_FRAME_SECONDS: f32 = 1.0 / 30.0;
/// The same when the window is drawn by a software rasteriser.
const SPINNER_FRAME_SECONDS_SOFTWARE: f32 = 1.0 / 4.0;

/// Paints a CSS box shadow `0 {offset_y}px {blur}px rgba(0, 0, 0, {opacity})` behind `rect`.
pub fn box_shadow(painter: &Painter, rect: Rect, radius: f32, offset_y: i8, blur: u8, opacity: f32) {
    let shadow = egui::Shadow { offset: [0, offset_y], blur, spread: 0, color: theme::rgba(0, 0, 0, opacity) };
    painter.add(shadow.as_shape(rect, CornerRadius::same(radius.round().clamp(0.0, 255.0) as u8)));
}

// --- Sheets -------------------------------------------------------------------------------------

/// Corner radius of a sheet.
pub const SHEET_RADIUS: f32 = 12.0;
/// The scrim behind a sheet: `rgba(0, 0, 0, .65)`.
pub const SHEET_SCRIM_OPACITY: f32 = 0.65;
/// Space kept between a sheet and the window edge (`.sheet-overlay { padding: 20px }`).
pub const SHEET_WINDOW_MARGIN: f32 = 20.0;
/// Vertical padding of a sheet item (`padding: 14px 16px`).
const SHEET_ITEM_PADDING_Y: f32 = 14.0;
/// Font size of a sheet item.
const SHEET_ITEM_FONT_SIZE: f32 = 14.0;
/// Line height of a sheet item (`line-height: 1.4`).
const SHEET_ITEM_LINE_HEIGHT: f32 = 19.6;

/// How a sheet item looks.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SheetItemKind {
    /// Text colour, weight 400.
    Plain,
    /// `.sheet__item--accent`: accent colour, weight 700.
    Accent,
    /// `.sheet__item--danger`: red, weight 700.
    Danger,
}

/// What `sheet` reports.
pub struct SheetOutput<R> {
    /// What the content closure returned.
    pub inner: R,
    /// The user clicked the scrim or pressed Escape.
    pub close_requested: bool,
}

/// How a centred dialog looks. The web project has two kinds: the sheets (`.sheet`) and the
/// larger import dialog (`.import-modal`).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DialogLook {
    /// Width of the card; it shrinks to keep 20 px to the window edges.
    pub width: f32,
    /// Opacity of the black scrim behind the card.
    pub scrim_opacity: f32,
    /// The card has the soft shadow of the import dialog (`0 12px 50px rgba(0, 0, 0, .35)`).
    pub shadow: bool,
    /// The card scales in from 108 % and fades in over 120 ms (`sheetin`).
    pub scale_in: bool,
}

impl DialogLook {
    /// A sheet of `width`: scrim .65, no shadow, scales in.
    pub fn sheet(width: f32) -> DialogLook {
        DialogLook { width, scrim_opacity: SHEET_SCRIM_OPACITY, shadow: false, scale_in: true }
    }

    /// The import dialog (`.import-overlay`, `.import-modal`): 560 px, scrim .7, a shadow, no
    /// animation.
    pub fn report() -> DialogLook {
        DialogLook { width: 560.0, scrim_opacity: 0.7, shadow: true, scale_in: false }
    }
}

/// Shows a sheet (`.sheet`): a centred card of `width` on the dark scrim. It scales in from 108 %
/// and fades in over 120 ms (`sheetin`); the scrim itself does not fade. `opened_at` is the
/// app-clock time the sheet was opened at. The content is laid out top to bottom without
/// spacing; use `sheet_item` for the rows.
pub fn sheet<R>(ctx: &egui::Context, id: Id, label: &str, width: f32, opened_at: f64, state: &AppState, add_contents: impl FnOnce(&mut Ui) -> R) -> SheetOutput<R> {
    dialog(ctx, id, label, DialogLook::sheet(width), opened_at, state, add_contents)
}

/// `sheet` whose last rows (`footer`, `footer_height` points tall: `sheet_item`s such as Save
/// and Cancel) stay below the part that scrolls, so that in a small window the body scrolls and
/// the rows are never cut off at the window's edge.
#[allow(clippy::too_many_arguments)]
pub fn sheet_with_footer<R, F>(ctx: &egui::Context, id: Id, label: &str, width: f32, opened_at: f64, state: &AppState, footer_height: f32, add_contents: impl FnOnce(&mut Ui) -> R, footer: impl FnOnce(&mut Ui) -> F) -> SheetOutput<(R, F)> {
    dialog_parts(ctx, id, label, DialogLook::sheet(width), opened_at, state, footer_height, add_contents, footer)
}

/// Shows a centred dialog on a dark scrim, built on `egui::Modal`, which blocks everything
/// behind it and keeps Tab inside. A click on the scrim or Escape asks to close it; a dialog
/// taller than the window scrolls.
pub fn dialog<R>(ctx: &egui::Context, id: Id, label: &str, look: DialogLook, opened_at: f64, state: &AppState, add_contents: impl FnOnce(&mut Ui) -> R) -> SheetOutput<R> {
    let output = dialog_parts(ctx, id, label, look, opened_at, state, 0.0, add_contents, |_| ());
    SheetOutput { inner: output.inner.0, close_requested: output.close_requested }
}

/// `dialog` with rows below the part that scrolls (none when `footer_height` is 0).
#[allow(clippy::too_many_arguments)]
fn dialog_parts<R, F>(ctx: &egui::Context, id: Id, label: &str, look: DialogLook, opened_at: f64, state: &AppState, footer_height: f32, add_contents: impl FnOnce(&mut Ui) -> R, footer: impl FnOnce(&mut Ui) -> F) -> SheetOutput<(R, F)> {
    let palette = state.palette();
    let progress = if state.animations && look.scale_in { ((state.time - opened_at) / SHEET_IN_SECONDS).clamp(0.0, 1.0) as f32 } else { 1.0 };
    let eased = layout::ease_out(progress);
    let screen = ctx.content_rect();
    let width = look.width.min(screen.width() - 2.0 * SHEET_WINDOW_MARGIN).max(120.0);
    let max_height = dialog_max_height(screen.height());

    let area = egui::Modal::default_area(id);
    let layer = area.layer();
    let modal = egui::Modal::new(id).area(area).backdrop_color(theme::rgba(0, 0, 0, look.scrim_opacity)).frame(egui::Frame::NONE).show(ctx, |ui| {
        ui.set_width(width);
        ui.set_max_height(max_height);
        ui.spacing_mut().item_spacing = Vec2::ZERO;
        ui.set_opacity(eased);
        let mut frame = egui::Frame::new().fill(palette.elevated).corner_radius(CornerRadius::same(SHEET_RADIUS as u8));
        if look.shadow {
            frame = frame.shadow(egui::Shadow { offset: [0, 12], blur: 50, spread: 0, color: theme::rgba(0, 0, 0, 0.35) });
        }
        let inner = frame
            .show(ui, |ui| {
                ui.set_width(width);
                // The scroll bar keeps clear of the card's rounded corners: it starts the corner
                // radius below the top, and ends that far above the bottom when no rows follow.
                // Its place is known after the first frame (egui wants it before the content).
                let bar_key = id.with("scroll-bar-rect");
                let mut scroll = egui::ScrollArea::vertical().max_height((max_height - footer_height).max(SHEET_RADIUS * 6.0));
                if let Some(bar) = ctx.data(|data| data.get_temp::<Rect>(bar_key)) {
                    scroll = scroll.scroll_bar_rect(bar);
                }
                let scrolled = scroll.show(ui, |ui| {
                    ui.set_width(width);
                    add_contents(ui)
                });
                let visible = scrolled.inner_rect;
                let bottom_inset = if footer_height > 0.0 { 0.0 } else { SHEET_RADIUS };
                let bar = Rect::from_min_max(pos2(visible.left(), visible.top() + SHEET_RADIUS), pos2(ui.max_rect().right().max(visible.right()), visible.bottom() - bottom_inset));
                ctx.data_mut(|data| data.insert_temp(bar_key, bar));
                (scrolled.inner, footer(ui))
            })
            .inner;
        // The dialog itself is what a screen reader announces.
        ui.response().widget_info(|| WidgetInfo::labeled(Role::Dialog, true, label));
        inner
    });

    if progress < 1.0 {
        let center = screen.center().to_vec2();
        let scale = 1.08 - 0.08 * eased;
        ctx.set_transform_layer(layer, TSTransform::from_translation(center) * TSTransform::from_scaling(scale) * TSTransform::from_translation(-center));
        ctx.request_repaint();
    } else {
        ctx.set_transform_layer(layer, TSTransform::IDENTITY);
    }
    // A dialog is always the top layer. egui raises a layer when it appears, so the viewer
    // would cover a dialog that was open before the viewer opened (a picture arriving from a
    // finished open, for instance).
    ctx.move_to_top(layer);
    // Escape closes the dialog on top only (one layer per press, `App.tsx:1028-1040`), also
    // while a tooltip is showing.
    let escape = modal.is_top_modal && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    SheetOutput { close_requested: modal.backdrop_response.clicked() || escape, inner: modal.inner }
}

/// The height of one `sheet_item`, with its divider when `divider_above`.
pub fn sheet_item_height(divider_above: bool) -> f32 {
    let divider = if divider_above { 1.0 } else { 0.0 };
    divider + 2.0 * SHEET_ITEM_PADDING_Y + SHEET_ITEM_LINE_HEIGHT
}

/// The tallest a dialog may be in a window of `screen_height` points before it has to scroll.
pub fn dialog_max_height(screen_height: f32) -> f32 {
    (screen_height - 2.0 * SHEET_WINDOW_MARGIN).max(120.0)
}

/// One row of a sheet (`.sheet__item`): centred text, 14 px padding, a divider above unless it is
/// the first thing in the sheet. `is_first` / `is_last` round the hover background with the
/// sheet's corners.
#[allow(clippy::too_many_arguments)]
pub fn sheet_item(ui: &mut Ui, label: &str, kind: SheetItemKind, enabled: bool, divider_above: bool, is_first: bool, is_last: bool, palette: &Palette) -> Response {
    let width = ui.available_width();
    let divider = if divider_above { 1.0 } else { 0.0 };
    let height = divider + 2.0 * SHEET_ITEM_PADDING_Y + SHEET_ITEM_LINE_HEIGHT;
    let (rect, _) = ui.allocate_exact_size(vec2(width, height), Sense::hover());
    let id = ui.scope_id().with(("sheet-item", label));
    let response = if enabled { hit(ui, rect, id, label) } else { hit_disabled(ui, rect, id, label) };

    let (color, font) = match kind {
        SheetItemKind::Plain => (palette.text, theme::regular(SHEET_ITEM_FONT_SIZE)),
        SheetItemKind::Accent => (palette.accent_text, theme::bold(SHEET_ITEM_FONT_SIZE)),
        SheetItemKind::Danger => (theme::DANGER_RED, theme::bold(SHEET_ITEM_FONT_SIZE)),
    };
    let color = if enabled { color } else { theme::with_opacity(color, 0.6) };
    let painter = ui.painter();
    if enabled && response.hovered() {
        let radius = SHEET_RADIUS as u8;
        let corners = CornerRadius { nw: if is_first { radius } else { 0 }, ne: if is_first { radius } else { 0 }, sw: if is_last { radius } else { 0 }, se: if is_last { radius } else { 0 } };
        painter.rect_filled(rect, corners, palette.hover);
    }
    if divider_above {
        painter.rect_filled(Rect::from_min_size(rect.min, vec2(width, 1.0)), 0.0, palette.border);
    }
    let galley = line_elided(ui.ctx(), label, font, color, SHEET_ITEM_LINE_HEIGHT, width - 32.0);
    let text_size = galley.size();
    paint_galley(painter, pos2(rect.center().x - text_size.x / 2.0, rect.top() + divider + SHEET_ITEM_PADDING_Y), galley);
    // The focus outline of a sheet item is drawn inside it (`outline-offset: -2px`).
    focus_ring(ui, &response, rect.shrink(theme::FOCUS_RING_WIDTH), 0.0, 0.0);
    response
}

// --- Toasts and the busy chip -------------------------------------------------------------------

/// Distance of the toast stack from the bottom of the window.
const TOAST_BOTTOM: f32 = 28.0;
/// The stack moves up to this distance while the busy chip is shown (`.import-chip + .toasts`).
const TOAST_BOTTOM_ABOVE_CHIP: f32 = 84.0;
/// Space between two toasts.
const TOAST_GAP: f32 = 8.0;
/// Toasts and the chip keep this distance from the window sides (`max-width: 100vw - 32px`).
const TOAST_WINDOW_MARGIN: f32 = 16.0;

/// Paints one pill at the bottom of the window: elevated background, radius 8, soft shadow.
fn bottom_pill(ui: &Ui, rect: Rect, with_outline: bool, palette: &Palette) {
    let painter = ui.painter();
    box_shadow(painter, rect, 8.0, 4, 24, 0.2);
    if with_outline {
        // `box-shadow: 0 0 0 1px var(--ig-border)`.
        painter.rect_filled(rect.expand(1.0), 9.0, palette.border);
    }
    painter.rect_filled(rect, 8.0, palette.elevated);
}

/// Shows the toast stack and, below it, the "Opening pictures…" chip (`.toasts`, `.toast`,
/// `.import-chip`): bottom centre, above every dialog, newest toast at the bottom, no animation.
pub fn toasts_and_chip(ctx: &egui::Context, state: &AppState, chip_text: Option<&str>) {
    if state.toasts.is_empty() && chip_text.is_none() {
        return;
    }
    let palette = state.palette();
    let screen = ctx.content_rect();
    let max_width = (screen.width() - 2.0 * TOAST_WINDOW_MARGIN).max(80.0);
    egui::Area::new(Id::unique("instagram-windows-app/toasts"))
        .order(egui::Order::Tooltip)
        .fixed_pos(screen.min)
        .interactable(false)
        .constrain(false)
        .show(ctx, |ui| {
            ui.set_clip_rect(screen);
            let mut bottom = screen.bottom() - TOAST_BOTTOM;
            if let Some(text) = chip_text {
                // `.import-chip`: padding 10px 16px, an 18 px spinner, 14 px text.
                let galley = line_elided(ctx, text, theme::regular(14.0), palette.text, 19.6, max_width - 32.0 - 32.0);
                let size = vec2(16.0 + 18.0 + 14.0 + galley.size().x + 16.0, 10.0 + 19.6 + 10.0);
                let rect = Rect::from_min_size(snap(ctx, pos2(screen.center().x - size.x / 2.0, bottom - size.y)), size);
                bottom_pill(ui, rect, true, palette);
                spinner(ui.painter(), pos2(rect.left() + 16.0 + 9.0, rect.center().y), 18.0, 2.0, state);
                paint_line(ui.painter(), rect.left() + 16.0 + 18.0 + 14.0, rect.center().y, galley);
                text_label(ui, rect, Id::unique("instagram-windows-app/busy-chip"), text);
                bottom = screen.bottom() - TOAST_BOTTOM_ABOVE_CHIP;
            }
            for (index, toast) in state.toasts.visible().iter().enumerate().rev() {
                // `.toast`: padding 12px 20px, 14 px text with line height 1.4, wraps anywhere.
                let galley = wrapped(ctx, &toast.message, theme::regular(14.0), palette.text, 19.6, max_width - 40.0);
                let size = galley.size() + vec2(40.0, 24.0);
                let rect = Rect::from_min_size(snap(ctx, pos2(screen.center().x - size.x / 2.0, bottom - size.y)), size);
                bottom_pill(ui, rect, false, palette);
                paint_galley(ui.painter(), rect.min + vec2(20.0, 12.0), galley);
                text_label(ui, rect, Id::unique(("instagram-windows-app/toast", index)), &toast.message);
                bottom = rect.top() - TOAST_GAP;
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_character_with_the_emoji_selector_gets_the_emoji_font() {
        let plain = format(theme::regular(14.0), Color32::BLACK, 21.0);
        let mut job = LayoutJob::default();
        append_text(&mut job, "one afternoon \u{2194}\u{FE0F} swipe \u{2764}\u{FE0F}", &plain);
        assert_eq!(job.text, "one afternoon \u{2194}\u{FE0F} swipe \u{2764}\u{FE0F}");
        let sections: Vec<(&str, bool)> = job.sections.iter().map(|section| (&job.text[section.byte_range.start.0..section.byte_range.end.0], section.format.font_id.family == theme::emoji_family())).collect();
        assert_eq!(sections, [("one afternoon ", false), ("\u{2194}\u{FE0F}", true), (" swipe ", false), ("\u{2764}\u{FE0F}", true)]);

        // Text without the selector stays one section; an empty text adds nothing.
        let mut job = LayoutJob::default();
        append_text(&mut job, "plain \u{1F950}", &plain);
        assert_eq!(job.sections.len(), 1);
        append_text(&mut job, "", &plain);
        append_text(&mut job, "\u{FE0F}\u{FE0F}", &plain);
        assert_eq!(job.text, "plain \u{1F950}\u{FE0F}\u{FE0F}");
        assert!(job.sections.iter().all(|section| section.format.font_id.family != theme::emoji_family()));
    }

    #[test]
    fn every_writing_direction_is_a_stretch_of_its_own() {
        let runs = |text: &str| -> Vec<(String, bool)> { direction_runs(text).into_iter().map(|(range, right_to_left)| (text[range].to_owned(), right_to_left)).collect() };
        let owned = |parts: &[(&str, bool)]| -> Vec<(String, bool)> { parts.iter().map(|(text, right_to_left)| ((*text).to_owned(), *right_to_left)).collect() };
        // Text without right-to-left letters is one stretch, whatever it holds.
        assert_eq!(runs("Fresh out of the oven 🥐, 2 loaves!"), owned(&[("Fresh out of the oven 🥐, 2 loaves!", false)]));
        assert_eq!(runs(""), owned(&[]));
        assert_eq!(runs(" … "), owned(&[(" … ", false)]));
        // Hebrew, then English: the space between goes with the English, as does the full stop.
        assert_eq!(runs("שלום עולם, זו תמונה English words."), owned(&[("שלום עולם, זו תמונה", true), (" English words.", false)]));
        // A number inside Arabic text is left to right, with the spaces around it.
        assert_eq!(runs("أرقام 12345 ونص"), owned(&[("أرقام", true), (" 12345 ", false), ("ونص", true)]));
        // What follows the last right-to-left letter stands on its own.
        assert_eq!(runs("(مرحبا)."), owned(&[("(", false), ("مرحبا", true), (").", false)]));
        // Every byte of the text is in exactly one stretch, in order.
        for text in ["abc שלום 12 مرحبا!", "שלום", "a", "؟"] {
            let joined: String = direction_runs(text).into_iter().map(|(range, _)| &text[range]).collect();
            assert_eq!(joined, text);
        }

        // In a layout job each stretch is a section, so that each is shaped the right way round.
        let plain = format(theme::regular(14.0), Color32::BLACK, 21.0);
        let mut job = LayoutJob::default();
        append_text(&mut job, "עם English 12", &plain);
        assert_eq!(job.text, "עם English 12");
        assert_eq!(job.sections.len(), 2);
    }

    #[test]
    fn the_ring_adds_three_points_on_every_side() {
        assert_eq!(avatar_outer_size(34.0, true), 40.0);
        assert_eq!(avatar_outer_size(32.0, false), 32.0);
    }
}
