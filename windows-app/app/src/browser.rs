//! The Browser view sheet (the lead's addition to web brief 4.12): where the browser pages are,
//! and their settings, in the style of the clone's dialogs (the auto view setup's sheet).
//!
//! From top to bottom: the title and a lead; the state of the server (the page address and the
//! API address as links that open the default browser, the addresses for other devices while
//! LAN is on, a note when a port runs elsewhere than configured, or why the pages are off); the
//! switch "Show the open posts in a browser" (`web_server`); the two port fields "Page address
//! port" and "API address port"; the switch "Allow other devices on this network" (`web_host`
//! 0.0.0.0 or 127.0.0.1) with its firewall note; one note naming the values the command line
//! fixed for this run (their controls show the values of settings.json, `web::SheetValues`,
//! while the state lines show the running ones); the error of the last Save; "Save" and
//! "Cancel", which stay below the part that scrolls in a small window.
//!
//! The sheet only paints and collects actions; `state::save_browser_view` checks the fields and
//! `web::WebService::save` binds, moves the server and writes settings.json.

use std::sync::Arc;

use egui::accesskit::Role;
use egui::epaint::text::{LayoutJob, TextWrapping};
use egui::epaint::StrokeKind;
use egui::{pos2, vec2, Color32, Galley, Id, Rect, Sense, Stroke, Ui, UiBuilder, WidgetInfo};
use instagram_core::messages;

use crate::autoview;
use crate::state::{Action, BrowserSheet, Env, PortField};
use crate::strings;
use crate::theme;
use crate::web::{OffReason, WebStatus};
use crate::widgets::{self, SheetItemKind};

/// Width of the sheet (every sheet of the clone is 400 px wide).
const SHEET_WIDTH: f32 = 400.0;
/// Padding of the body: `20px 20px 18px`, as the auto view setup.
const PADDING_TOP: f32 = 20.0;
const PADDING_X: f32 = 20.0;
const PADDING_BOTTOM: f32 = 18.0;
/// Space between the parts of the body.
const GAP: f32 = 14.0;
/// Space inside a group (label and field, link lines).
const SMALL_GAP: f32 = 6.0;
/// Space between the network note and the note of the command line, two notes in a row (a
/// whole `GAP` made the sheet of a run with a moved port and command-line ports scroll at
/// 800 x 600 by a few points).
const NOTE_GAP: f32 = 8.0;
/// Line height of 14 px text (`line-height: 1.4`).
const LINE_HEIGHT: f32 = 19.6;
/// Line height of 12 px notes.
const NOTE_LINE_HEIGHT: f32 = 16.8;
/// A port field: the auto view setup's field.
const FIELD_WIDTH: f32 = 128.0;
const FIELD_HEIGHT: f32 = 39.0;
const FIELD_PADDING_X: f32 = 10.0;
const FIELD_PADDING_Y: f32 = 8.0;
const FIELD_FONT_SIZE: f32 = 16.0;
/// The error line: `#c22b38`, and `#ff7a85` in the dark appearance (the auto view setup's).
const ERROR_LIGHT: Color32 = Color32::from_rgb(0xC2, 0x2B, 0x38);
const ERROR_DARK: Color32 = Color32::from_rgb(0xFF, 0x7A, 0x85);

/// The id of a port field of the sheet with this serial.
pub fn port_field_id(field: PortField, serial: u64) -> Id {
    Id::unique(("browser-view-port", field == PortField::Frontend, serial))
}

/// One line of the state block: a text, or a label and a link.
enum StatusLine {
    Text(String),
    Link { label: &'static str, url: String },
}

/// The lines of the state block of the sheet.
fn status_lines(env: &Env<'_>) -> Vec<StatusLine> {
    let web = env.web;
    let mut lines = Vec::new();
    match web.status() {
        WebStatus::Running(running) => {
            lines.push(StatusLine::Link { label: strings::BROWSER_VIEW_PAGE_ADDRESS, url: running.page_url() });
            lines.push(StatusLine::Link { label: strings::BROWSER_VIEW_API_ADDRESS, url: format!("{}/docs", running.api_url()) });
            if web.plan().lan() {
                if running.lan.is_empty() {
                    lines.push(StatusLine::Text(strings::ABOUT_BROWSER_NO_LAN_ADDRESS.to_owned()));
                }
                for url in running.lan_page_urls() {
                    lines.push(StatusLine::Link { label: strings::ABOUT_BROWSER_OTHER_DEVICES, url });
                }
            }
            let (page, api) = web.moved_ports();
            for (configured, running) in [page, api].into_iter().flatten() {
                lines.push(StatusLine::Text(messages::port_moved(configured, running)));
            }
        }
        WebStatus::Starting => lines.push(StatusLine::Text(strings::ABOUT_BROWSER_STARTING.to_owned())),
        WebStatus::Off(reason) => lines.push(StatusLine::Text(sheet_off_text(*reason).to_owned())),
        WebStatus::Failed(reason) => lines.push(StatusLine::Text(messages::browser_not_started(reason))),
    }
    lines
}

/// Why the pages are off, in words (About; it leads to Browser view).
pub fn off_text(reason: OffReason) -> &'static str {
    match reason {
        OffReason::Settings => strings::BROWSER_OFF_SETTINGS,
        OffReason::CommandLine => strings::BROWSER_OFF_CLI,
        OffReason::Sheet => strings::BROWSER_OFF_SHEET,
        OffReason::NotStarted => strings::BROWSER_NOT_STARTED,
    }
}

/// Why the pages are off, inside Browser view itself, whose switch is just below.
pub fn sheet_off_text(reason: OffReason) -> &'static str {
    match reason {
        OffReason::Sheet => strings::BROWSER_VIEW_OFF,
        OffReason::NotStarted => strings::BROWSER_VIEW_NOT_STARTED,
        other => off_text(other),
    }
}

/// Wrapped text.
fn paragraph(ctx: &egui::Context, text: &str, font: egui::FontId, color: Color32, line_height: f32, max_width: f32, center: bool) -> Arc<Galley> {
    let mut job = LayoutJob::single_section(text.to_owned(), widgets::format(font, color, line_height));
    job.wrap = TextWrapping { max_width, ..TextWrapping::default() };
    if center {
        job.halign = egui::Align::Center;
    }
    widgets::layout_job(ctx, job)
}

/// A port field with its label above it; the new text when it changed.
#[allow(clippy::too_many_arguments)]
fn port_field(ui: &mut Ui, left: f32, top: f32, label: &str, text: &str, id: Id, enabled: bool, env: &Env<'_>) -> Option<String> {
    let ctx = ui.ctx().clone();
    let palette = env.palette();
    let label_galley = widgets::line(&ctx, label, theme::semibold(14.0), palette.text, theme::normal_line_height(14.0));
    let label_height = label_galley.size().y;
    widgets::paint_line(ui.painter(), left, top + label_height / 2.0, label_galley);
    let rect = Rect::from_min_size(widgets::snap(&ctx, pos2(left, top + label_height + SMALL_GAP)), vec2(FIELD_WIDTH, FIELD_HEIGHT));
    let focused = ctx.memory(|memory| memory.has_focus(id));
    let painter = ui.painter();
    painter.rect_filled(rect, 8.0, palette.input);
    painter.rect_stroke(rect, 8.0, Stroke::new(1.0, palette.border), StrokeKind::Inside);
    if focused {
        // `outline: 2px solid var(--ig-blue); outline-offset: 1px`.
        painter.rect_stroke(rect.expand(1.0), 9.0, Stroke::new(2.0, palette.blue), StrokeKind::Outside);
    }
    let mut field_text = text.to_owned();
    let mut field_ui = ui.new_child(UiBuilder::new().max_rect(rect.shrink(1.0)));
    if !enabled {
        field_ui.disable();
    }
    let margin = egui::Margin { left: FIELD_PADDING_X as i8, right: FIELD_PADDING_X as i8, top: FIELD_PADDING_Y as i8 - 1, bottom: FIELD_PADDING_Y as i8 - 1 };
    let output = egui::TextEdit::singleline(&mut field_text)
        .id(id)
        .frame(egui::Frame::NONE.inner_margin(margin))
        .font(theme::regular(FIELD_FONT_SIZE))
        .text_color(palette.text)
        .char_limit(crate::state::PORT_FIELD_MAX_CHARS)
        .desired_width(f32::INFINITY)
        .show(&mut field_ui);
    output.response.widget_info(|| {
        let mut info = WidgetInfo::labeled(Role::TextInput, enabled, label);
        info.current_text_value = Some(field_text.clone());
        info
    });
    (field_text != text).then_some(field_text)
}

/// The height of a port field with its label.
fn port_field_height(ctx: &egui::Context) -> f32 {
    widgets::line(ctx, "0", theme::semibold(14.0), Color32::WHITE, theme::normal_line_height(14.0)).size().y + SMALL_GAP + FIELD_HEIGHT
}

/// Shows the Browser view sheet.
pub fn browser_sheet(ctx: &egui::Context, sheet: &BrowserSheet, env: &mut Env<'_>) {
    let state = env.state;
    let palette = state.palette();
    let web = env.web;
    let cli = web.config().overrides;
    let lines = status_lines(env);
    let mut actions: Vec<Action> = Vec::new();

    let ports_fixed = cli.frontend_port.is_some() || cli.backend_port.is_some();
    let cli_text = strings::browser_view_from_cli(cli.no_web, ports_fixed, cli.host.is_some());
    let footer_height = 2.0 * widgets::sheet_item_height(true);
    let body = |ui: &mut Ui| {
        let width = ui.available_width();
        let content_width = width - 2.0 * PADDING_X;
        let id = Id::unique("browser-view-body");
        let muted = |text: &str, size: f32, line_height: f32| paragraph(ctx, text, theme::regular(size), palette.muted, line_height, content_width, false);

        let title = widgets::line(ctx, strings::BROWSER_VIEW_TITLE, theme::semibold(18.0), palette.text, theme::normal_line_height(18.0));
        let lead = paragraph(ctx, strings::BROWSER_VIEW_LEAD, theme::regular(14.0), palette.muted, LINE_HEIGHT, content_width, true);
        let reload = muted(strings::BROWSER_VIEW_RELOAD, 12.0, NOTE_LINE_HEIGHT);
        let cli_note = cli_text.as_deref().map(|text| muted(text, 12.0, NOTE_LINE_HEIGHT));
        let lan_note = muted(strings::BROWSER_VIEW_LAN_NOTE, 12.0, NOTE_LINE_HEIGHT);
        let error_color = if palette.dark { ERROR_DARK } else { ERROR_LIGHT };
        let error = sheet.error.as_deref().map(|message| paragraph(ctx, message, theme::regular(13.0), error_color, 18.2, content_width, false));
        // A sentence of the state block wraps (a moved port, why the server did not start).
        let texts: Vec<Option<Arc<Galley>>> = lines
            .iter()
            .map(|line| match line {
                StatusLine::Text(text) => Some(muted(text, 14.0, LINE_HEIGHT)),
                StatusLine::Link { .. } => None,
            })
            .collect();
        let status_height: f32 = texts.iter().map(|galley| galley.as_ref().map_or(LINE_HEIGHT, |galley| galley.size().y.max(LINE_HEIGHT))).sum::<f32>() + lines.len().saturating_sub(1) as f32 * 2.0;

        let mut height = PADDING_TOP + title.size().y + GAP + lead.size().y + GAP + status_height;
        if matches!(web.status(), WebStatus::Running(_)) {
            height += SMALL_GAP + reload.size().y;
        }
        height += GAP + LINE_HEIGHT;
        height += GAP + port_field_height(ctx);
        height += GAP + LINE_HEIGHT + SMALL_GAP + lan_note.size().y;
        if let Some(note) = &cli_note {
            height += NOTE_GAP + note.size().y;
        }
        if let Some(error) = &error {
            height += GAP + error.size().y;
        }
        height += PADDING_BOTTOM;
        let (body, _) = ui.allocate_exact_size(vec2(width, height.ceil()), Sense::hover());
        let left = body.left() + PADDING_X;

        // Title and lead, centred.
        let mut y = body.top() + PADDING_TOP;
        let title_height = title.size().y;
        let title_rect = widgets::paint_line(ui.painter(), body.center().x - title.size().x / 2.0, y + title_height / 2.0, title);
        widgets::text_label(ui, title_rect, id.with("title"), strings::BROWSER_VIEW_TITLE);
        y += title_height + GAP;
        let lead_height = lead.size().y;
        widgets::paint_galley(ui.painter(), pos2(body.center().x, y), lead);
        widgets::text_label(ui, Rect::from_min_size(pos2(left, y), vec2(content_width, lead_height)), id.with("lead"), strings::BROWSER_VIEW_LEAD);
        y += lead_height + GAP;

        // The state of the server.
        for (index, (line, text_galley)) in lines.iter().zip(texts).enumerate() {
            let line_height = match (line, text_galley) {
                (StatusLine::Text(text), Some(galley)) => {
                    let height = galley.size().y.max(LINE_HEIGHT);
                    let rect = Rect::from_min_size(pos2(left, y), vec2(content_width, height));
                    widgets::paint_galley(ui.painter(), rect.min, galley);
                    widgets::text_label(ui, rect, id.with(("status", index)), text);
                    height
                }
                (StatusLine::Link { label, url }, _) => {
                    let label_galley = widgets::line(ctx, label, theme::semibold(14.0), palette.text, LINE_HEIGHT);
                    let label_rect = widgets::paint_line(ui.painter(), left, y + LINE_HEIGHT / 2.0, label_galley);
                    widgets::text_label(ui, label_rect, id.with(("status-label", index)), label);
                    let link_left = (left + 120.0).max(label_rect.right() + 8.0);
                    let link = widgets::line_elided(ctx, url, theme::regular(14.0), palette.accent_text, LINE_HEIGHT, body.right() - PADDING_X - link_left);
                    let link_rect = Rect::from_min_size(pos2(link_left, y), vec2(link.size().x, LINE_HEIGHT));
                    let response = widgets::hit(ui, link_rect, id.with(("link", index)), url);
                    // The clone's blue text buttons (`.link-blue`): the accent text colour, the
                    // text colour while hovered.
                    let color = if response.hovered() { palette.text } else { palette.accent_text };
                    let link = widgets::line_elided(ctx, url, theme::regular(14.0), color, LINE_HEIGHT, body.right() - PADDING_X - link_left);
                    widgets::paint_line(ui.painter(), link_left, y + LINE_HEIGHT / 2.0, link);
                    widgets::focus_ring(ui, &response, link_rect, 4.0, theme::FOCUS_RING_OFFSET);
                    if response.clicked() {
                        actions.push(Action::OpenLink(url.clone()));
                    }
                    LINE_HEIGHT
                }
                (StatusLine::Text(_), None) => LINE_HEIGHT,
            };
            y += line_height + 2.0;
        }
        y -= if lines.is_empty() { 0.0 } else { 2.0 };
        if matches!(web.status(), WebStatus::Running(_)) {
            y += SMALL_GAP;
            let height = reload.size().y;
            widgets::paint_galley(ui.painter(), pos2(left, y), reload);
            widgets::text_label(ui, Rect::from_min_size(pos2(left, y), vec2(content_width, height)), id.with("reload"), strings::BROWSER_VIEW_RELOAD);
            y += height;
        }
        y += GAP;

        // The server switch.
        let server_rect = Rect::from_min_size(pos2(left, y), vec2(content_width, LINE_HEIGHT));
        if autoview::checkbox_row(ui, server_rect, id.with("server"), strings::BROWSER_VIEW_SERVER, sheet.enabled, palette) {
            actions.push(Action::BrowserViewServer(!sheet.enabled));
        }
        y += LINE_HEIGHT + GAP;

        // The two ports, side by side.
        let page_id = port_field_id(PortField::Frontend, sheet.serial);
        let api_id = port_field_id(PortField::Backend, sheet.serial);
        if let Some(text) = port_field(ui, left, y, strings::BROWSER_VIEW_PAGE_PORT, &sheet.frontend_text, page_id, !sheet.saving, env) {
            actions.push(Action::BrowserViewPort(PortField::Frontend, text));
        }
        let second = left + content_width / 2.0 + 4.0;
        if let Some(text) = port_field(ui, second, y, strings::BROWSER_VIEW_API_PORT, &sheet.backend_text, api_id, !sheet.saving, env) {
            actions.push(Action::BrowserViewPort(PortField::Backend, text));
        }
        y += port_field_height(ctx) + GAP;

        // The LAN switch and its note.
        let lan_rect = Rect::from_min_size(pos2(left, y), vec2(content_width, LINE_HEIGHT));
        if autoview::checkbox_row(ui, lan_rect, id.with("lan"), strings::BROWSER_VIEW_LAN, sheet.lan, palette) {
            actions.push(Action::BrowserViewLan(!sheet.lan));
        }
        y += LINE_HEIGHT + SMALL_GAP;
        let note_height = lan_note.size().y;
        widgets::paint_galley(ui.painter(), pos2(left, y), lan_note);
        widgets::text_label(ui, Rect::from_min_size(pos2(left, y), vec2(content_width, note_height)), id.with("lan-note"), strings::BROWSER_VIEW_LAN_NOTE);
        y += note_height;

        // The values of the command line, once for all of them.
        if let (Some(galley), Some(text)) = (cli_note, cli_text.as_deref()) {
            y += NOTE_GAP;
            let height = galley.size().y;
            widgets::paint_galley(ui.painter(), pos2(left, y), galley);
            widgets::text_label(ui, Rect::from_min_size(pos2(left, y), vec2(content_width, height)), id.with("cli"), text);
            y += height;
        }

        // The error of the last Save.
        if let (Some(galley), Some(message)) = (error, sheet.error.as_deref()) {
            y += GAP;
            let rect = Rect::from_min_size(pos2(left, y), vec2(content_width, galley.size().y));
            widgets::paint_galley(ui.painter(), rect.min, galley);
            ui.interact(rect, id.with("error"), Sense::hover()).widget_info(|| WidgetInfo::labeled(Role::Alert, true, message));
        }
    };
    let footer = |ui: &mut Ui| {
        let save_label = if sheet.saving { strings::BROWSER_VIEW_SAVING } else { strings::BROWSER_VIEW_SAVE };
        let save = widgets::sheet_item(ui, save_label, SheetItemKind::Accent, !sheet.saving, true, false, false, palette).clicked();
        let cancel = widgets::sheet_item(ui, strings::BROWSER_VIEW_CANCEL, SheetItemKind::Plain, true, true, false, true, palette).clicked();
        (save, cancel)
    };
    let output = widgets::sheet_with_footer(ctx, Id::unique("browser-view-sheet"), strings::BROWSER_VIEW_TITLE, SHEET_WIDTH, sheet.opened_at, state, footer_height, body, footer);
    let ((), (save, cancel)) = output.inner;

    for action in actions {
        env.push(action);
    }
    if save {
        env.push(Action::BrowserViewSave);
    } else if cancel || output.close_requested {
        env.push(Action::CloseBrowserView);
    }
}
