//! The banner above the feed: what is open, and what can be done with all of it.
//!
//! It is the reference web project's import banner (`App.tsx:1159-1252`, `styles.css:475-480`):
//! a wrapping flex row with the sentence on the left and the text buttons on the right. The
//! layout is computed first (`plan`), because the feed needs the banner's height before it paints
//! anything, and painted from that plan. "N warnings" opens the dialog with the list
//! (`sheets::warnings_dialog`). While "Download all" is being prepared its button turns into
//! "Preparing {done} of {total} · Cancel" with a small spinner and cancels on a click; while
//! another download is on its way the button cannot be used.

use std::sync::Arc;

use egui::epaint::text::{LayoutJob, TextWrapping};
use egui::epaint::StrokeKind;
use egui::{pos2, vec2, Galley, Id, Pos2, Rect, Stroke, Ui, Vec2};
use instagram_core::messages;
use instagram_core::model::Collection;

use crate::icons::{self, Icon, IconStyle};
use crate::state::{Action, Env};
use crate::strings;
use crate::theme::{self, Palette};
use crate::widgets::{self, TextButton, TextHover};

/// Horizontal padding inside the border (`padding: 10px 14px`).
const PADDING_X: f32 = 14.0;
/// Vertical padding inside the border.
const PADDING_Y: f32 = 10.0;
/// Width of the border.
const BORDER: f32 = 1.0;
/// Corner radius.
const RADIUS: f32 = 8.0;
/// Space between the sentence and the buttons when they share a row (`gap: 8px 16px`).
const COLUMN_GAP: f32 = 16.0;
/// Space between the rows of the banner.
const ROW_GAP: f32 = 8.0;
/// Space between two buttons (`gap: 6px 14px`).
const ACTION_GAP_X: f32 = 14.0;
/// Space between two rows of buttons.
const ACTION_GAP_Y: f32 = 6.0;
/// Font size of the sentence.
const TEXT_SIZE: f32 = 13.0;
/// Line height of the sentence (`line-height: 1.4`).
const TEXT_LINE_HEIGHT: f32 = 18.2;
/// Font size of the buttons and of the warnings list.
const ACTION_SIZE: f32 = 12.0;
/// Line height of the buttons and of the warnings list.
const ACTION_LINE_HEIGHT: f32 = 16.8;
/// Side of the icon before the sentence.
const TEXT_ICON_SIZE: f32 = 16.0;
/// Space between that icon and the sentence.
const TEXT_ICON_GAP: f32 = 8.0;

/// The buttons of the banner, in the order they are shown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BannerButton {
    Warnings,
    AutoView,
    DownloadAll,
    OpenMore,
    Close,
}

/// One button with its place inside the banner.
struct PlacedButton {
    button: BannerButton,
    label: String,
    /// Top-left corner, relative to the banner's top-left corner.
    offset: Vec2,
}

/// The computed layout of the banner.
pub struct BannerPlan {
    /// Total height, border included, rounded up to whole points so the cards below it sit on
    /// the pixel grid.
    pub height: f32,
    sentence: Arc<Galley>,
    sentence_offset: Vec2,
    /// The whole sentence as plain text, for screen readers and tests.
    sentence_text: String,
    buttons: Vec<PlacedButton>,
    download: DownloadLook,
}

/// What "Download all" is at the moment.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DownloadLook {
    /// The button as the web project shows it at rest.
    Idle,
    /// A download is being chosen or a single post is being prepared: the button cannot be used.
    Disabled,
    /// "Preparing {done} of {total} · Cancel" with a spinner: a click cancels.
    Preparing,
}

/// Diameter of the spinner of a running download (`.spinner--tiny`).
const DOWNLOAD_SPINNER_SIZE: f32 = 12.0;

/// The text button for `button`.
fn text_button<'a>(button: BannerButton, label: &'a str, palette: &Palette, download: DownloadLook) -> TextButton<'a> {
    let font = theme::semibold(ACTION_SIZE);
    match button {
        // While it runs, the download is a "link-strong" with a spinner in place of the icon.
        BannerButton::DownloadAll if download == DownloadLook::Preparing => TextButton {
            label,
            font,
            line_height: ACTION_LINE_HEIGHT,
            color: palette.text,
            hover: TextHover::Fade,
            icon: None,
            tooltip: None,
            enabled: true,
            leading_space: DOWNLOAD_SPINNER_SIZE,
        },
        BannerButton::Warnings => {
            TextButton { label, font, line_height: ACTION_LINE_HEIGHT, color: palette.text, hover: TextHover::Fade, icon: Some((Icon::Warning, 14.0, false)), tooltip: None, enabled: true, leading_space: 0.0 }
        }
        BannerButton::AutoView => {
            TextButton { label, font, line_height: ACTION_LINE_HEIGHT, color: palette.accent_text, hover: TextHover::TextColor, icon: Some((Icon::Play, 12.0, true)), tooltip: None, enabled: true, leading_space: 0.0 }
        }
        BannerButton::DownloadAll => TextButton {
            label,
            font,
            line_height: ACTION_LINE_HEIGHT,
            color: palette.blue,
            hover: TextHover::TextColor,
            icon: Some((Icon::Download, 12.0, false)),
            tooltip: Some(strings::BANNER_DOWNLOAD_ALL_TOOLTIP),
            enabled: download == DownloadLook::Idle,
            leading_space: 0.0,
        },
        BannerButton::OpenMore => TextButton { label, font, line_height: ACTION_LINE_HEIGHT, color: palette.blue, hover: TextHover::TextColor, icon: None, tooltip: None, enabled: true, leading_space: 0.0 },
        BannerButton::Close => TextButton { label, font, line_height: ACTION_LINE_HEIGHT, color: palette.text, hover: TextHover::Fade, icon: None, tooltip: None, enabled: true, leading_space: 0.0 },
    }
}

/// The sentence "Showing 6 posts from export-basic": muted, with the name in the text colour
/// and semibold, wrapped at `max_width`.
fn sentence_galley(ctx: &egui::Context, collection: &Collection, palette: &Palette, max_width: f32) -> (Arc<Galley>, String) {
    let sentence = messages::collection_banner(collection);
    // The core puts the label at the end of the sentence; everything before it is the lead.
    let lead_len = if !collection.label.is_empty() && sentence.ends_with(&collection.label) { sentence.len() - collection.label.len() } else { sentence.len() };
    let mut job = LayoutJob { wrap: TextWrapping { max_width, ..TextWrapping::default() }, ..LayoutJob::default() };
    job.append(&sentence[..lead_len], 0.0, widgets::format(theme::regular(TEXT_SIZE), palette.muted, TEXT_LINE_HEIGHT));
    job.append(&sentence[lead_len..], 0.0, widgets::format(theme::semibold(TEXT_SIZE), palette.text, TEXT_LINE_HEIGHT));
    (widgets::layout_job(ctx, job), sentence)
}

/// Computes the layout of the banner for a column of `width`.
pub fn plan(ctx: &egui::Context, env: &Env<'_>, collection: &Collection, width: f32) -> BannerPlan {
    let palette = env.palette();
    let content_width = width - 2.0 * (BORDER + PADDING_X);
    let content_left = BORDER + PADDING_X;
    let content_top = BORDER + PADDING_Y;

    let (sentence, sentence_text) = sentence_galley(ctx, collection, palette, content_width - TEXT_ICON_SIZE - TEXT_ICON_GAP);
    let text_block = vec2(TEXT_ICON_SIZE + TEXT_ICON_GAP + sentence.size().x, sentence.size().y.max(TEXT_ICON_SIZE));

    let mut buttons: Vec<(BannerButton, String)> = Vec::new();
    if !collection.warnings.is_empty() {
        buttons.push((BannerButton::Warnings, messages::warnings_label(collection.warnings.len())));
    }
    buttons.push((BannerButton::AutoView, strings::BANNER_AUTO_VIEW.to_owned()));
    // "Download all" shows how far it is while it runs, and cancels when clicked
    // (`App.tsx:1224-1238`); while another download is on its way it cannot be used.
    let (download, download_label) = match env.state.download.as_ref() {
        None => (DownloadLook::Idle, strings::BANNER_DOWNLOAD_ALL.to_owned()),
        Some(running) => match running.banner_progress() {
            Some(progress) => (DownloadLook::Preparing, format!("{}{}", messages::preparing_progress(progress.done, progress.total), messages::PROGRESS_CANCEL_SUFFIX)),
            None => (DownloadLook::Disabled, strings::BANNER_DOWNLOAD_ALL.to_owned()),
        },
    };
    buttons.push((BannerButton::DownloadAll, download_label));
    buttons.push((BannerButton::OpenMore, strings::BANNER_OPEN_MORE.to_owned()));
    buttons.push((BannerButton::Close, strings::BANNER_CLOSE.to_owned()));
    let sizes: Vec<Vec2> = buttons.iter().map(|(button, label)| text_button(*button, label, palette, download).size(ctx)).collect();
    let one_row_width: f32 = sizes.iter().map(|size| size.x).sum::<f32>() + ACTION_GAP_X * (sizes.len().saturating_sub(1)) as f32;

    // Break the buttons into rows that fit the content width (`flex-wrap: wrap`).
    let mut rows: Vec<Vec<usize>> = vec![Vec::new()];
    let mut row_width = 0.0;
    for (index, size) in sizes.iter().enumerate() {
        let needed = if rows.last().is_some_and(|row| row.is_empty()) { size.x } else { row_width + ACTION_GAP_X + size.x };
        if needed > content_width && rows.last().is_some_and(|row| !row.is_empty()) {
            rows.push(vec![index]);
            row_width = size.x;
        } else {
            if let Some(row) = rows.last_mut() {
                row.push(index);
            }
            row_width = needed;
        }
    }
    let actions_width = if rows.len() == 1 { one_row_width } else { content_width };
    let actions_height = rows.len() as f32 * ACTION_LINE_HEIGHT + (rows.len() - 1) as f32 * ACTION_GAP_Y;

    // The sentence and the buttons share a row when both fit; otherwise the buttons go below,
    // pushed to the right (`margin-left: auto`).
    let same_row = text_block.x + COLUMN_GAP + actions_width <= content_width;
    let (sentence_top, actions_top, bottom) = if same_row {
        let row_height = text_block.y.max(actions_height);
        (content_top + (row_height - text_block.y) / 2.0, content_top + (row_height - actions_height) / 2.0, content_top + row_height)
    } else {
        let actions_top = content_top + text_block.y + ROW_GAP;
        (content_top, actions_top, actions_top + actions_height)
    };
    let actions_left = content_left + content_width - actions_width;

    let mut placed = Vec::new();
    for (row_index, row) in rows.iter().enumerate() {
        let mut x = actions_left;
        let y = actions_top + row_index as f32 * (ACTION_LINE_HEIGHT + ACTION_GAP_Y);
        for &index in row {
            let (button, label) = &buttons[index];
            placed.push(PlacedButton { button: *button, label: label.clone(), offset: vec2(x, y) });
            x += sizes[index].x + ACTION_GAP_X;
        }
    }

    BannerPlan { height: (bottom + PADDING_Y + BORDER).ceil(), sentence, sentence_offset: vec2(content_left + TEXT_ICON_SIZE + TEXT_ICON_GAP, sentence_top), sentence_text, buttons: placed, download }
}

/// Paints the banner with its top-left corner at `origin`.
pub fn show(ui: &Ui, origin: Pos2, width: f32, plan: &BannerPlan, env: &mut Env<'_>) {
    let palette = env.palette();
    let rect = Rect::from_min_size(origin, vec2(width, plan.height));
    let painter = ui.painter();
    painter.rect_filled(rect, RADIUS, palette.surface);
    painter.rect_stroke(rect, RADIUS, Stroke::new(BORDER, palette.border), StrokeKind::Inside);

    // The icon is centred on the first line of the sentence.
    let sentence_pos = origin + plan.sentence_offset;
    let icon_center = pos2(origin.x + BORDER + PADDING_X + TEXT_ICON_SIZE / 2.0, sentence_pos.y + TEXT_LINE_HEIGHT / 2.0);
    icons::paint(painter, Icon::Import, icon_center, IconStyle::outline(TEXT_ICON_SIZE), palette.muted);
    let sentence_rect = Rect::from_min_size(sentence_pos, plan.sentence.size());
    widgets::paint_galley(painter, sentence_pos, plan.sentence.clone());
    widgets::text_label(ui, sentence_rect, Id::unique("banner-sentence"), &plan.sentence_text);

    for placed in &plan.buttons {
        let button = text_button(placed.button, &placed.label, palette, plan.download);
        let min = widgets::snap(ui.ctx(), origin + placed.offset);
        let response = button.show(ui, min, Id::unique(("banner-button", placed.button as u8)), palette);
        if placed.button == BannerButton::DownloadAll && plan.download == DownloadLook::Preparing {
            let center = pos2(min.x + DOWNLOAD_SPINNER_SIZE / 2.0, min.y + ACTION_LINE_HEIGHT / 2.0);
            widgets::spinner(ui.painter(), center, DOWNLOAD_SPINNER_SIZE, 2.0, env.state);
        }
        if response.clicked() {
            env.push(match placed.button {
                BannerButton::Warnings => Action::ShowWarnings,
                BannerButton::AutoView => Action::StartAutoView(None),
                BannerButton::DownloadAll if plan.download == DownloadLook::Preparing => Action::CancelDownload,
                BannerButton::DownloadAll => Action::DownloadAll,
                BannerButton::OpenMore => Action::PickPictures,
                BannerButton::Close => Action::ClosePictures,
            });
        }
    }
}
