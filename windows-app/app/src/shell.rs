//! The frame around the views: the sidebar with its More menu, the empty state, the drop
//! overlay and the About sheet (with the Browser section of web brief 4.12).
//!
//! The sidebar is the reference web project's (`Sidebar.tsx`, `styles.css:70-119`): 244 px wide
//! with labels, an icon rail of 72 px while the window is at most 1264 px wide. Of its items
//! only those that mean something for local pictures remain; "Open pictures" and "Open folder"
//! take the place of "Import". The empty state and the wording of the drop overlay are new: the
//! web project shows a demo feed when nothing is imported and only accepts exports.

use egui::{pos2, vec2, Color32, Id, Key, Modifiers, Pos2, Rect, Stroke, Ui, Vec2};

use crate::icons::{self, Icon, IconStyle};
use crate::state::{Action, Env, View};
use crate::strings;
use crate::theme::{self, Palette};
use crate::web::WebStatus;
use crate::widgets::{self, ButtonKind, SheetItemKind};

// --- Sidebar ------------------------------------------------------------------------------------

/// Height of the logo block at the top.
const LOGO_BLOCK_HEIGHT: f32 = 84.0;
/// Padding above the logo block.
const SIDEBAR_PADDING_TOP: f32 = 8.0;
/// Padding below the More item.
const SIDEBAR_PADDING_BOTTOM: f32 = 20.0;
/// Horizontal padding of the wide sidebar.
const SIDEBAR_PADDING_X: f32 = 12.0;
/// Horizontal padding of the icon rail.
const RAIL_PADDING_X: f32 = 6.0;
/// Height of a navigation item: 12 px padding around the 26 px icon.
const NAV_ITEM_HEIGHT: f32 = 50.0;
/// Space between two navigation items.
const NAV_ITEM_GAP: f32 = 6.0;
/// Padding inside a navigation item.
const NAV_ITEM_PADDING: f32 = 12.0;
/// Side of a navigation icon.
const NAV_ICON_SIZE: f32 = 26.0;
/// Space between icon and label.
const NAV_LABEL_GAP: f32 = 16.0;
/// Font size of a navigation label.
const NAV_LABEL_SIZE: f32 = 16.0;

/// Width of the More menu.
const MENU_WIDTH: f32 = 266.0;
/// Padding inside the More menu.
const MENU_PADDING: f32 = 8.0;
/// Height of a menu item: 13 px padding around the 20 px icon.
const MENU_ITEM_HEIGHT: f32 = 46.0;
/// The menu sits this far above the bottom of the sidebar (`bottom: 66px`).
const MENU_BOTTOM: f32 = 66.0;

/// The width of the sidebar for a window of `window_width`.
pub fn sidebar_width(window_width: f32) -> f32 {
    if window_width <= theme::SIDEBAR_RAIL_MAX_WINDOW_WIDTH {
        theme::SIDEBAR_RAIL_WIDTH
    } else {
        theme::SIDEBAR_WIDTH
    }
}

/// One navigation item.
struct NavItem {
    label: &'static str,
    icon: Icon,
    /// Bold label; `filled_when_active` icons are filled as well.
    active: bool,
    filled_when_active: bool,
    action: Action,
}

/// Paints one navigation item and reports a click.
fn nav_item(ui: &Ui, rect: Rect, item: &NavItem, collapsed: bool, palette: &Palette) -> bool {
    let id = Id::unique(("nav", item.label));
    let mut response = widgets::hit(ui, rect, id, item.label);
    if collapsed {
        // The rail has no labels: the name appears as a tooltip.
        response = response.on_hover_text(item.label);
    }
    let hover = widgets::transition(ui, id.with("hover"), response.hovered(), 0.15);
    let painter = ui.painter();
    if hover > 0.0 {
        painter.rect_filled(rect, 8.0, theme::with_opacity(palette.hover, hover));
    }
    let icon_center = pos2(rect.left() + NAV_ITEM_PADDING + NAV_ICON_SIZE / 2.0, rect.center().y);
    icons::paint(painter, item.icon, icon_center, IconStyle::toggled(NAV_ICON_SIZE, item.active && item.filled_when_active), palette.text);
    if !collapsed {
        let font = if item.active { theme::bold(NAV_LABEL_SIZE) } else { theme::regular(NAV_LABEL_SIZE) };
        let galley = widgets::line(ui.ctx(), item.label, font, palette.text, theme::normal_line_height(NAV_LABEL_SIZE));
        widgets::paint_line(painter, rect.left() + NAV_ITEM_PADDING + NAV_ICON_SIZE + NAV_LABEL_GAP, rect.center().y, galley);
    }
    widgets::focus_ring(ui, &response, rect, 8.0, -theme::FOCUS_RING_WIDTH);
    response.clicked()
}

/// Shows the sidebar in `rect` (the left edge of the window, full height).
pub fn sidebar(ui: &Ui, rect: Rect, env: &mut Env<'_>) {
    let ctx = ui.ctx().clone();
    let state = env.state;
    let palette = state.palette();
    let collapsed = rect.width() < theme::SIDEBAR_WIDTH;
    let painter = ui.painter();
    painter.rect_filled(rect, 0.0, palette.surface);
    // `border-right: 1px solid`, inside the sidebar's width.
    painter.rect_filled(Rect::from_min_max(pos2(rect.right() - 1.0, rect.top()), rect.right_bottom()), 0.0, palette.border);

    let padding_x = if collapsed { RAIL_PADDING_X } else { SIDEBAR_PADDING_X };
    let content = Rect::from_min_max(pos2(rect.left() + padding_x, rect.top() + SIDEBAR_PADDING_TOP), pos2(rect.right() - 1.0 - padding_x, rect.bottom() - SIDEBAR_PADDING_BOTTOM));
    // In the rail the 50 px items are centred; in the wide sidebar they fill the width.
    let (item_left, item_width) = if collapsed { (widgets::snap(&ctx, pos2(content.center().x - NAV_ITEM_HEIGHT / 2.0, 0.0)).x, NAV_ITEM_HEIGHT) } else { (content.left(), content.width()) };

    // Logo: the wordmark, or the glyph in the rail. A click leads home.
    let logo_center_y = content.top() + LOGO_BLOCK_HEIGHT / 2.0;
    let logo_rect = if collapsed {
        Rect::from_center_size(pos2(content.center().x, logo_center_y), Vec2::splat(24.0))
    } else {
        let mut format = widgets::format(theme::wordmark(30.0), palette.text, 30.0);
        format.extra_letter_spacing = 0.4;
        let galley = widgets::layout_job(&ctx, egui::epaint::text::LayoutJob::single_section(strings::WORDMARK.to_owned(), format));
        widgets::paint_line(painter, content.left() + 12.0, logo_center_y, galley)
    };
    if collapsed {
        icons::paint(painter, Icon::Logo, logo_rect.center(), IconStyle::outline(24.0).with_stroke_width(2.0), palette.text);
    }
    let logo = widgets::hit(ui, logo_rect, Id::unique("nav-logo"), strings::WORDMARK);
    widgets::focus_ring(ui, &logo, logo_rect, 4.0, theme::FOCUS_RING_OFFSET);
    if logo.clicked() {
        env.push(Action::ShowHome);
    }

    let items = [
        NavItem { label: strings::NAV_HOME, icon: Icon::Home, active: state.view == View::Home, filled_when_active: true, action: Action::ShowHome },
        NavItem { label: strings::NAV_EXPLORE, icon: Icon::Compass, active: state.view == View::Explore, filled_when_active: false, action: Action::ShowExplore },
        NavItem { label: strings::NAV_OPEN_PICTURES, icon: Icon::Import, active: false, filled_when_active: false, action: Action::PickPictures },
        NavItem { label: strings::NAV_OPEN_FOLDER, icon: Icon::Folder, active: false, filled_when_active: false, action: Action::PickFolder },
        NavItem { label: strings::NAV_AUTO_VIEW, icon: Icon::Play, active: false, filled_when_active: false, action: Action::StartAutoView(None) },
    ];
    let mut top = content.top() + LOGO_BLOCK_HEIGHT;
    for item in &items {
        let item_rect = Rect::from_min_size(pos2(item_left, top), vec2(item_width, NAV_ITEM_HEIGHT));
        if nav_item(ui, item_rect, item, collapsed, palette) {
            env.push(item.action.clone());
        }
        top += NAV_ITEM_HEIGHT + NAV_ITEM_GAP;
    }

    // More, at the bottom; bold while its menu is open.
    let more = NavItem { label: strings::NAV_MORE, icon: Icon::More, active: state.more_menu_open, filled_when_active: false, action: Action::ToggleMoreMenu };
    let more_rect = Rect::from_min_size(pos2(item_left, content.bottom() - NAV_ITEM_HEIGHT), vec2(item_width, NAV_ITEM_HEIGHT));
    if nav_item(ui, more_rect, &more, collapsed, palette) {
        env.push(Action::ToggleMoreMenu);
    }
    if state.more_menu_open {
        more_menu(&ctx, rect, more_rect, collapsed, env);
    }
}

/// One entry of the More menu.
struct MenuEntry {
    label: &'static str,
    icon: Icon,
    enabled: bool,
    action: Action,
    /// A divider is drawn above this entry.
    divider_above: bool,
}

/// The More menu (`.more-menu`): a rounded card above the More item. A click elsewhere or
/// Escape closes it.
fn more_menu(ctx: &egui::Context, sidebar: Rect, more_rect: Rect, collapsed: bool, env: &mut Env<'_>) {
    let state = env.state;
    let palette = state.palette();
    let has_pictures = state.collection.is_some();
    let mut entries = vec![
        MenuEntry { label: strings::MENU_AUTO_VIEW, icon: Icon::Play, enabled: true, action: Action::StartAutoView(None), divider_above: false },
        MenuEntry { label: strings::MENU_SWITCH_APPEARANCE, icon: if palette.dark { Icon::Sun } else { Icon::Moon }, enabled: true, action: Action::SwitchAppearance, divider_above: false },
        // Always listed, so that it is found; it works while the browser pages run.
        MenuEntry { label: strings::MENU_OPEN_IN_BROWSER, icon: Icon::External, enabled: env.web.running().is_some(), action: Action::OpenInBrowser, divider_above: false },
        MenuEntry { label: strings::MENU_BROWSER_VIEW, icon: Icon::Settings, enabled: true, action: Action::ShowBrowserView, divider_above: false },
        // One download at a time: while one is on its way the banner shows it and cancels it.
        MenuEntry { label: strings::MENU_DOWNLOAD_ALL, icon: Icon::Download, enabled: has_pictures && state.download.is_none(), action: Action::DownloadAll, divider_above: false },
    ];
    // Only while the browser imports folder holds a post.json and is not open (web brief 4.10).
    if state.browser_imports.is_some() {
        entries.push(MenuEntry { label: strings::MENU_OPEN_BROWSER_IMPORTS, icon: Icon::Import, enabled: true, action: Action::OpenBrowserImports, divider_above: false });
    }
    // Only while the web project's backend keeps a library with a post.json (web brief 3.7).
    if let Some(folder) = &state.web_app_library {
        entries.push(MenuEntry { label: strings::MENU_OPEN_WEB_APP_LIBRARY, icon: Icon::Folder, enabled: true, action: Action::OpenPaths(vec![folder.clone()]), divider_above: false });
    }
    entries.push(MenuEntry { label: strings::MENU_CLOSE_PICTURES, icon: Icon::Close, enabled: has_pictures, action: Action::ClosePictures, divider_above: false });
    entries.push(MenuEntry { label: strings::MENU_ABOUT, icon: Icon::Info, enabled: true, action: Action::ShowAbout, divider_above: true });
    /// A divider takes 1 px with 6 px above and below (`margin: 6px 0`).
    const DIVIDER_HEIGHT: f32 = 13.0;
    let dividers = entries.iter().filter(|entry| entry.divider_above).count() as f32;
    let height = 2.0 * MENU_PADDING + entries.len() as f32 * MENU_ITEM_HEIGHT + dividers * DIVIDER_HEIGHT;
    let left = sidebar.left() + if collapsed { 8.0 } else { 12.0 };
    // In a short window the menu would leave the top of the screen: it then covers the More item.
    let top = (sidebar.bottom() - MENU_BOTTOM - height).max(sidebar.top() + MENU_PADDING);
    let menu_rect = Rect::from_min_size(pos2(left, top), vec2(MENU_WIDTH, height));

    egui::Area::new(Id::unique("more-menu")).order(egui::Order::Foreground).fixed_pos(menu_rect.min).constrain(false).show(ctx, |ui| {
        ui.set_clip_rect(ctx.content_rect());
        ui.allocate_rect(menu_rect, egui::Sense::click());
        let painter = ui.painter();
        // `box-shadow: 0 4px 24px rgba(0, 0, 0, .18), 0 0 0 1px var(--ig-border)`.
        widgets::box_shadow(painter, menu_rect, 16.0, 4, 24, 0.18);
        painter.rect_filled(menu_rect.expand(1.0), 17.0, palette.border);
        painter.rect_filled(menu_rect, 16.0, palette.elevated);

        let mut top = menu_rect.top() + MENU_PADDING;
        for entry in &entries {
            if entry.divider_above {
                painter.rect_filled(Rect::from_min_size(pos2(menu_rect.left() + MENU_PADDING, top + 6.0), vec2(MENU_WIDTH - 2.0 * MENU_PADDING, 1.0)), 0.0, palette.border);
                top += DIVIDER_HEIGHT;
            }
            let rect = Rect::from_min_size(pos2(menu_rect.left() + MENU_PADDING, top), vec2(MENU_WIDTH - 2.0 * MENU_PADDING, MENU_ITEM_HEIGHT));
            let id = Id::unique(("more-menu-item", entry.label));
            let response = if entry.enabled { widgets::hit(ui, rect, id, entry.label) } else { widgets::hit_disabled(ui, rect, id, entry.label) };
            if entry.enabled && response.hovered() {
                painter.rect_filled(rect, 8.0, palette.hover);
            }
            let color = if entry.enabled { palette.text } else { theme::with_opacity(palette.text, 0.5) };
            let galley = widgets::line(ctx, entry.label, theme::regular(14.0), color, theme::normal_line_height(14.0));
            widgets::paint_line(painter, rect.left() + 14.0, rect.center().y, galley);
            icons::paint(painter, entry.icon, pos2(rect.right() - 14.0 - 10.0, rect.center().y), IconStyle::outline(20.0), color);
            widgets::focus_ring(ui, &response, rect, 8.0, -theme::FOCUS_RING_WIDTH);
            if response.clicked() {
                env.push(entry.action.clone());
            }
            top += MENU_ITEM_HEIGHT;
        }
    });

    // A click anywhere else closes the menu; the More item toggles it by itself.
    let clicked_elsewhere = ctx.input(|input| input.pointer.any_click() && input.pointer.interact_pos().is_some_and(|pos| !menu_rect.contains(pos) && !more_rect.contains(pos)));
    let escape = ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape));
    if clicked_elsewhere || escape {
        env.push(Action::CloseMoreMenu);
    }
}

// --- Empty state --------------------------------------------------------------------------------

/// Shows the empty state centred in `rect`: the logo in a ring, the title, both buttons and the
/// hint about dropping. It borrows the look of the web project's empty grid (`.grid-empty`).
pub fn empty_state(ui: &Ui, rect: Rect, env: &mut Env<'_>) {
    let ctx = ui.ctx().clone();
    let palette = env.palette();
    let painter = ui.painter();

    /// Diameter of the ring around the logo.
    const RING: f32 = 62.0;
    /// Space between the rows.
    const GAP: f32 = 12.0;
    let title = widgets::line(&ctx, strings::EMPTY_TITLE, theme::bold(20.0), palette.text, theme::normal_line_height(20.0));
    let hint = widgets::line(&ctx, strings::EMPTY_HINT, theme::regular(14.0), palette.muted, theme::normal_line_height(14.0));
    let pictures_size = widgets::button_size(&ctx, strings::EMPTY_OPEN_PICTURES);
    let folder_size = widgets::button_size(&ctx, strings::EMPTY_OPEN_FOLDER);
    let buttons_width = pictures_size.x + 8.0 + folder_size.x;
    let total_height = RING + GAP + title.size().y + GAP + 6.0 + pictures_size.y + GAP + hint.size().y;

    let center_x = rect.center().x;
    let mut y = (rect.center().y - total_height / 2.0).round().max(rect.top() + 20.0);
    let ring_center = pos2(center_x.round(), y + RING / 2.0);
    painter.circle_stroke(ring_center, RING / 2.0 - 1.0, Stroke::new(2.0, palette.text));
    icons::paint(painter, Icon::Logo, ring_center, IconStyle::outline(30.0).with_stroke_width(2.0), palette.text);
    y += RING + GAP;

    let title_rect = Rect::from_min_size(widgets::snap(&ctx, pos2(center_x - title.size().x / 2.0, y)), title.size());
    widgets::paint_galley(painter, title_rect.min, title);
    widgets::text_label(ui, title_rect, Id::unique("empty-title"), strings::EMPTY_TITLE);
    y += title_rect.height() + GAP + 6.0;

    let buttons_left = widgets::snap(&ctx, pos2(center_x - buttons_width / 2.0, y));
    if widgets::button(ui, buttons_left, Id::unique("empty-open-pictures"), strings::EMPTY_OPEN_PICTURES, ButtonKind::Primary, palette).clicked() {
        env.push(Action::PickPictures);
    }
    if widgets::button(ui, buttons_left + vec2(pictures_size.x + 8.0, 0.0), Id::unique("empty-open-folder"), strings::EMPTY_OPEN_FOLDER, ButtonKind::Secondary, palette).clicked() {
        env.push(Action::PickFolder);
    }
    y += pictures_size.y + GAP;

    let hint_rect = Rect::from_min_size(widgets::snap(&ctx, pos2(center_x - hint.size().x / 2.0, y)), hint.size());
    widgets::paint_galley(painter, hint_rect.min, hint);
    widgets::text_label(ui, hint_rect, Id::unique("empty-hint"), strings::EMPTY_HINT);
}

/// Shown in place of the empty state while the first pictures are being opened (a start with
/// paths on the command line): the web project's spinner with the text below it
/// (`.spinner-wrap`), so that the empty state does not flash before the feed appears.
pub fn opening_state(ui: &Ui, rect: Rect, env: &Env<'_>) {
    let ctx = ui.ctx().clone();
    let palette = env.palette();
    let text = widgets::line(&ctx, strings::OPENING_PICTURES, theme::regular(14.0), palette.muted, theme::normal_line_height(14.0));
    let center = widgets::snap(&ctx, rect.center());
    widgets::spinner(ui.painter(), center - vec2(0.0, 16.0 + 5.0), 32.0, 3.0, env.state);
    let text_rect = Rect::from_min_size(widgets::snap(&ctx, pos2(center.x - text.size().x / 2.0, center.y + 5.0)), text.size());
    widgets::paint_galley(ui.painter(), text_rect.min, text);
    widgets::text_label(ui, text_rect, Id::unique("opening-state"), strings::OPENING_PICTURES);
}

// --- Drop overlay -------------------------------------------------------------------------------

/// The points of a rounded rectangle, clockwise, for a dashed outline.
fn rounded_rect_points(rect: Rect, radius: f32) -> Vec<Pos2> {
    let corners = [
        (pos2(rect.right() - radius, rect.top() + radius), -std::f32::consts::FRAC_PI_2),
        (pos2(rect.right() - radius, rect.bottom() - radius), 0.0),
        (pos2(rect.left() + radius, rect.bottom() - radius), std::f32::consts::FRAC_PI_2),
        (pos2(rect.left() + radius, rect.top() + radius), std::f32::consts::PI),
    ];
    let mut points = Vec::new();
    for (center, start) in corners {
        for step in 0..=8 {
            let angle = start + std::f32::consts::FRAC_PI_2 * step as f32 / 8.0;
            points.push(center + radius * vec2(angle.cos(), angle.sin()));
        }
    }
    if let Some(first) = points.first().copied() {
        points.push(first);
    }
    points
}

/// The overlay while files are dragged over the window (`.drop-overlay`): a dark scrim over
/// everything and a dashed box with the import glyph and "Drop pictures or a folder".
pub fn drop_overlay(ctx: &egui::Context) {
    let screen = ctx.content_rect();
    let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Debug, Id::unique("drop-overlay")));
    painter.rect_filled(screen, 0.0, theme::rgba(0, 0, 0, 0.74));

    let title = widgets::line(ctx, strings::DROP_TITLE, theme::semibold(24.0), Color32::WHITE, theme::normal_line_height(24.0));
    let width = 520.0_f32.min(screen.width() - 48.0).max(120.0);
    // `padding: 48px 24px`, a 64 px glyph, a 12 px gap, the title; the 3 px border counts.
    let height = 3.0 + 48.0 + 64.0 + 12.0 + title.size().y + 48.0 + 3.0;
    let rect = Rect::from_center_size(widgets::snap(ctx, screen.center()), vec2(width, height));
    painter.rect_filled(rect, 20.0, theme::rgba(0, 0, 0, 0.62));
    let outline = rect.shrink(1.5);
    painter.extend(egui::Shape::dashed_line(&rounded_rect_points(outline, 18.5), Stroke::new(3.0, theme::rgba(255, 255, 255, 0.85)), 9.0, 6.0));
    let icon_center = pos2(rect.center().x, rect.top() + 3.0 + 48.0 + 32.0);
    icons::paint(&painter, Icon::Import, icon_center, IconStyle::outline(64.0).with_stroke_width(1.4), Color32::WHITE);
    let title_pos = pos2(rect.center().x - title.size().x / 2.0, icon_center.y + 32.0 + 12.0);
    widgets::paint_galley(&painter, title_pos, title);
}

// --- About --------------------------------------------------------------------------------------

/// How a line of the About sheet is set.
#[derive(Debug, Clone, PartialEq, Eq)]
enum AboutStyle {
    /// The name of the app: 18 px, semibold.
    Title,
    /// A section heading: 14 px, semibold.
    Heading,
    /// Running text: 14 px, muted.
    Text,
    /// A path: monospace, in the text colour, broken anywhere (`.import-banner__path`).
    Path,
    /// A link that opens in the default browser: 14 px, in the accent text colour.
    Link(String),
}

/// The lines of the About sheet with the space above each: the name and the version, what the
/// app is, where the descriptions are kept, where the settings are, the licence note, and the
/// Browser section (web brief 4.12): where the pages are, or why they are off.
fn about_lines(paths: &instagram_core::settings::AppPaths, web: &crate::web::WebService) -> Vec<(String, AboutStyle, f32)> {
    let mut lines = vec![
        (strings::APP_TITLE.to_owned(), AboutStyle::Title, 0.0),
        (strings::about_version(), AboutStyle::Text, 8.0),
        (strings::ABOUT_SUMMARY.to_owned(), AboutStyle::Text, 2.0),
        (strings::ABOUT_DATA_LEAD.to_owned(), AboutStyle::Text, 14.0),
        (paths.data_dir.display().to_string(), AboutStyle::Path, 2.0),
        (strings::ABOUT_SETTINGS_LEAD.to_owned(), AboutStyle::Text, 14.0),
        (paths.settings_file.display().to_string(), AboutStyle::Path, 2.0),
        (strings::ABOUT_LICENCE.to_owned(), AboutStyle::Text, 14.0),
        (strings::ABOUT_BROWSER_TITLE.to_owned(), AboutStyle::Heading, 18.0),
        (strings::ABOUT_BROWSER_LEAD.to_owned(), AboutStyle::Text, 4.0),
    ];
    match web.status() {
        WebStatus::Running(running) => {
            lines.push((strings::ABOUT_BROWSER_THIS_COMPUTER.to_owned(), AboutStyle::Text, 10.0));
            lines.push((running.page_url(), AboutStyle::Link(running.page_url()), 2.0));
            lines.push((strings::ABOUT_BROWSER_API.to_owned(), AboutStyle::Text, 8.0));
            let docs = format!("{}/docs", running.api_url());
            lines.push((running.api_url(), AboutStyle::Link(docs), 2.0));
            if web.plan().lan() {
                lines.push((strings::ABOUT_BROWSER_OTHER_DEVICES.to_owned(), AboutStyle::Text, 8.0));
                if running.lan.is_empty() {
                    lines.push((strings::ABOUT_BROWSER_NO_LAN_ADDRESS.to_owned(), AboutStyle::Text, 2.0));
                }
                for url in running.lan_page_urls() {
                    lines.push((url.clone(), AboutStyle::Link(url), 2.0));
                }
            }
            lines.push((strings::ABOUT_BROWSER_RELOAD.to_owned(), AboutStyle::Text, 10.0));
        }
        WebStatus::Starting => lines.push((strings::ABOUT_BROWSER_STARTING.to_owned(), AboutStyle::Text, 10.0)),
        WebStatus::Off(reason) => lines.push((crate::browser::off_text(*reason).to_owned(), AboutStyle::Text, 10.0)),
        WebStatus::Failed(reason) => lines.push((instagram_core::messages::browser_not_started(reason), AboutStyle::Text, 10.0)),
    }
    lines
}

/// `path` with line breaks after backslashes (or slashes), so that each line of `max_width`
/// points ends with whole folder names, the way a browser wraps a path between its parts. A
/// single name longer than a line stays on a line of its own (the layout then breaks it).
pub fn wrapped_path(ctx: &egui::Context, path: &str, font: &egui::FontId, max_width: f32) -> String {
    let width_of = |text: &str| widgets::layout_job(ctx, egui::epaint::text::LayoutJob::simple_singleline(text.to_owned(), font.clone(), egui::Color32::WHITE)).size().x;
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for part in path.split_inclusive(['\\', '/']) {
        let longer = format!("{line}{part}");
        if line.is_empty() || width_of(&longer) <= max_width {
            line = longer;
        } else {
            lines.push(std::mem::replace(&mut line, part.to_owned()));
        }
    }
    lines.push(line);
    lines.join("\n")
}

/// The About sheet, in the shape of the web project's confirm dialog (`.sheet.confirm`): a
/// centred title and muted lines, then the rows "Open data folder" and "Close".
pub fn about_sheet(ctx: &egui::Context, opened_at: f64, env: &mut Env<'_>) {
    let state = env.state;
    let palette = state.palette();
    let lines = about_lines(env.paths, env.web);
    let mut link: Option<String> = None;
    // The rows stay below the text, which scrolls in a small window.
    let footer_height = 3.0 * widgets::sheet_item_height(true);
    let footer = |ui: &mut egui::Ui| {
        let browser_view = widgets::sheet_item(ui, strings::MENU_BROWSER_VIEW, SheetItemKind::Plain, true, true, false, false, palette).clicked();
        let open_folder = widgets::sheet_item(ui, strings::ABOUT_OPEN_DATA_FOLDER, SheetItemKind::Plain, true, true, false, false, palette).clicked();
        let close = widgets::sheet_item(ui, strings::ABOUT_CLOSE, SheetItemKind::Plain, true, true, false, true, palette).clicked();
        (browser_view, open_folder, close)
    };
    let output = widgets::sheet_with_footer(ctx, Id::unique("about-sheet"), strings::ABOUT_LABEL, 400.0, opened_at, state, footer_height, |ui| {
        let width = ui.available_width();
        // `.confirm__text`: `padding: 24px 24px 18px`, centred.
        let text_width = width - 48.0;
        let galleys: Vec<_> = lines
            .iter()
            .map(|(text, style, _)| {
                let (font, color, line_height) = match style {
                    AboutStyle::Title => (theme::semibold(18.0), palette.text, theme::normal_line_height(18.0)),
                    AboutStyle::Heading => (theme::semibold(14.0), palette.text, 20.3),
                    AboutStyle::Text => (theme::regular(14.0), palette.muted, 20.3),
                    AboutStyle::Path => (theme::monospace(12.0), palette.text, 18.0),
                    // The clone's blue text buttons (`.link-blue`), recognisable without hovering.
                    AboutStyle::Link(_) => (theme::regular(14.0), palette.accent_text, 20.3),
                };
                // Paths have no spaces to wrap at: they break after a backslash, between the
                // names of folders, and only a name longer than a line is broken inside.
                let shown = if *style == AboutStyle::Path { wrapped_path(ctx, text, &font, text_width) } else { text.clone() };
                let mut job = egui::epaint::text::LayoutJob::single_section(shown, widgets::format(font, color, line_height));
                job.wrap = egui::epaint::text::TextWrapping { max_width: text_width, break_anywhere: *style == AboutStyle::Path, ..Default::default() };
                job.halign = egui::Align::Center;
                widgets::layout_job(ctx, job)
            })
            .collect();
        let text_height: f32 = galleys.iter().zip(&lines).map(|(galley, (_, _, space_above))| galley.size().y + space_above).sum();
        let (rect, _) = ui.allocate_exact_size(vec2(width, 24.0 + text_height + 18.0), egui::Sense::hover());
        let mut y = rect.top() + 24.0;
        for (index, (galley, (text, style, space_above))) in galleys.into_iter().zip(&lines).enumerate() {
            y += space_above;
            let height = galley.size().y;
            let width = galley.size().x;
            // Every line centres itself around its anchor.
            widgets::paint_galley(ui.painter(), pos2(rect.center().x, y), galley);
            let line_rect = Rect::from_min_size(pos2(rect.left() + 24.0, y), vec2(text_width, height));
            match style {
                AboutStyle::Link(url) => {
                    let link_rect = Rect::from_center_size(pos2(rect.center().x, y + height / 2.0), vec2(width, height));
                    let response = widgets::hit(ui, link_rect, Id::unique(("about-link", index)), text);
                    widgets::focus_ring(ui, &response, link_rect, 4.0, theme::FOCUS_RING_OFFSET);
                    if response.clicked() {
                        link = Some(url.clone());
                    }
                }
                _ => {
                    widgets::text_label(ui, line_rect, Id::unique(("about-line", index)), text);
                }
            }
            y += height;
        }
    }, footer);
    let ((), (browser_view, open_folder, close)) = output.inner;
    if open_folder {
        env.push(Action::OpenDataFolder);
    }
    if let Some(url) = link {
        env.push(Action::OpenLink(url));
    }
    if browser_view {
        env.push(Action::ShowBrowserView);
        return;
    }
    if close || output.close_requested {
        env.push(Action::CloseAbout);
    }
}
