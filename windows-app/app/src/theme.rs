//! Colours, fonts and the egui style of both appearances.
//!
//! The palettes are the reference web project's CSS variables, resolved (`styles.css:1-33` over
//! `instagram-tokens.css`): light is the default, dark is switched on by More -> "Switch
//! appearance". Nothing here is derived from the system colour scheme, exactly as in the web
//! project. Fonts are the system's Segoe UI files, loaded from the Windows fonts folder at run
//! time; egui has no font weight, so every weight is a family of its own.

use std::path::PathBuf;
use std::sync::Arc;

use egui::{Color32, FontData, FontDefinitions, FontFamily, FontId};
use instagram_core::store::Theme;

// --- Palette ------------------------------------------------------------------------------------

/// The colours of one appearance. Field names follow the web project's `--ig-*` variables.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Palette {
    /// True for the dark appearance.
    pub dark: bool,
    /// Page background behind the feed.
    pub bg: Color32,
    /// Sidebar, post card, banner.
    pub surface: Color32,
    /// Sheets, dialogs, toasts, the More menu.
    pub elevated: Color32,
    /// Primary text and icons.
    pub text: Color32,
    /// Secondary text: times, counts, hints.
    pub muted: Color32,
    /// Every 1 px border and divider.
    pub border: Color32,
    /// Primary button, focus ring, selection, progress.
    pub blue: Color32,
    /// Primary button while hovered.
    pub blue_strong: Color32,
    /// Hashtags and mentions.
    pub link: Color32,
    /// Translucent layer over a hovered row or menu item.
    pub hover: Color32,
    /// Background of input fields.
    pub input: Color32,
    /// Secondary button, picture placeholder.
    pub soft: Color32,
    /// Secondary button while hovered, scrollbar thumb.
    pub soft_hover: Color32,
    /// Accent rows of a sheet, the banner's "Auto view".
    pub accent_text: Color32,
    /// Warning glyphs.
    pub warning: Color32,
}

/// Like heart of a liked post.
pub const LIKE_RED: Color32 = Color32::from_rgb(0xFF, 0x30, 0x40);
/// Destructive rows and error text.
pub const DANGER_RED: Color32 = Color32::from_rgb(0xED, 0x49, 0x56);
/// The area behind a picture in the feed wrapper and the viewer.
pub const MEDIA_BLACK: Color32 = Color32::BLACK;
/// The app icon's pink.
pub const BRAND_PINK: Color32 = Color32::from_rgb(0xE1, 0x30, 0x6C);
/// Glyph of a carousel arrow (always dark, it sits on a white disc).
pub const CAROUSEL_ARROW_GLYPH: Color32 = Color32::from_rgb(0x26, 0x26, 0x26);

/// The light appearance (the default).
pub const LIGHT: Palette = Palette {
    dark: false,
    bg: Color32::from_rgb(0xFA, 0xFA, 0xFA),
    surface: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    elevated: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    text: Color32::from_rgb(0x26, 0x26, 0x26),
    muted: Color32::from_rgb(0x8E, 0x8E, 0x8E),
    border: Color32::from_rgb(0xDB, 0xDB, 0xDB),
    blue: Color32::from_rgb(0x00, 0x95, 0xF6),
    blue_strong: Color32::from_rgb(0x18, 0x77, 0xF2),
    link: Color32::from_rgb(0x00, 0x37, 0x6B),
    // rgba(0, 0, 0, .05), premultiplied.
    hover: Color32::from_rgba_premultiplied(0, 0, 0, 13),
    input: Color32::from_rgb(0xFF, 0xFF, 0xFF),
    soft: Color32::from_rgb(0xEF, 0xEF, 0xEF),
    soft_hover: Color32::from_rgb(0xDB, 0xDB, 0xDB),
    accent_text: Color32::from_rgb(0x00, 0x64, 0xD1),
    warning: Color32::from_rgb(0xC7, 0x77, 0x00),
};

/// The dark appearance.
pub const DARK: Palette = Palette {
    dark: true,
    bg: Color32::from_rgb(0x18, 0x19, 0x1A),
    surface: Color32::from_rgb(0x24, 0x25, 0x26),
    elevated: Color32::from_rgb(0x24, 0x25, 0x26),
    text: Color32::from_rgb(0xE4, 0xE6, 0xEB),
    muted: Color32::from_rgb(0xB0, 0xB3, 0xB8),
    border: Color32::from_rgb(0x3E, 0x40, 0x42),
    blue: Color32::from_rgb(0x00, 0x95, 0xF6),
    blue_strong: Color32::from_rgb(0x18, 0x77, 0xF2),
    link: Color32::from_rgb(0xE0, 0xF1, 0xFF),
    // rgba(255, 255, 255, .1), premultiplied.
    hover: Color32::from_rgba_premultiplied(26, 26, 26, 26),
    input: Color32::from_rgb(0x24, 0x25, 0x26),
    soft: Color32::from_rgb(0x26, 0x26, 0x26),
    soft_hover: Color32::from_rgb(0x36, 0x36, 0x36),
    accent_text: Color32::from_rgb(0x4C, 0xB5, 0xF9),
    warning: Color32::from_rgb(0xF5, 0xB0, 0x41),
};

/// The palette of an appearance.
pub fn palette(theme: Theme) -> &'static Palette {
    match theme {
        Theme::Light => &LIGHT,
        Theme::Dark => &DARK,
    }
}

/// `color` with its opacity multiplied by `opacity` (0..=1), for the CSS `opacity` property.
pub fn with_opacity(color: Color32, opacity: f32) -> Color32 {
    if opacity >= 1.0 {
        color
    } else {
        color.gamma_multiply(opacity.clamp(0.0, 1.0))
    }
}

/// A CSS `rgba(r, g, b, a)` colour.
pub fn rgba(red: u8, green: u8, blue: u8, alpha: f32) -> Color32 {
    Color32::from_rgba_unmultiplied(red, green, blue, (alpha.clamp(0.0, 1.0) * 255.0).round() as u8)
}

// --- Geometry constants shared by several views -------------------------------------------------

/// Width of the sidebar with labels.
pub const SIDEBAR_WIDTH: f32 = 244.0;
/// Width of the icon rail the sidebar collapses to.
pub const SIDEBAR_RAIL_WIDTH: f32 = 72.0;
/// The sidebar is an icon rail while the window is at most this wide (`max-width: 1264px`).
pub const SIDEBAR_RAIL_MAX_WINDOW_WIDTH: f32 = 1264.0;
/// Width of the feed column, borders of the cards included.
pub const FEED_WIDTH: f32 = 470.0;
/// Horizontal padding of the main area on each side.
pub const MAIN_PADDING_X: f32 = 20.0;
/// Space above the first element of the main area.
pub const MAIN_PADDING_TOP: f32 = 30.0;
/// Space below the last element of the main area.
pub const MAIN_PADDING_BOTTOM: f32 = 60.0;
/// The smallest window: the icon rail, the feed column and its padding still fit.
pub const MIN_WINDOW_SIZE: [f32; 2] = [640.0, 480.0];
/// The window size of a first start.
pub const DEFAULT_WINDOW_SIZE: [f32; 2] = [1280.0, 860.0];
/// Width of the focus ring (`outline: 2px solid`).
pub const FOCUS_RING_WIDTH: f32 = 2.0;
/// Distance of the focus ring from the widget (`outline-offset: 2px`).
pub const FOCUS_RING_OFFSET: f32 = 2.0;
/// Width of a scrollbar.
pub const SCROLLBAR_WIDTH: f32 = 8.0;

// --- Fonts --------------------------------------------------------------------------------------

/// Name of the family that holds Segoe UI Semibold (CSS weight 600).
const FAMILY_SEMIBOLD: &str = "semibold";
/// Name of the family that holds Segoe UI Bold (CSS weight 700).
const FAMILY_BOLD: &str = "bold";
/// Name of the family that holds Segoe UI Light (CSS weight 300).
const FAMILY_LIGHT: &str = "light";
/// Name of the family that holds the script face of the wordmark.
const FAMILY_WORDMARK: &str = "wordmark";
/// Name of the family that lists the emoji face before the text face.
const FAMILY_EMOJI: &str = "emoji";
/// How far Segoe UI Emoji is moved down, as a fraction of the font size, to sit on the baseline
/// of the text around it (measured against the web project in Edge: 2 px at 14 px).
const EMOJI_BASELINE_SHIFT_EM: f32 = 0.145;

/// Weight 400 at `size` points.
pub fn regular(size: f32) -> FontId {
    FontId::new(size, FontFamily::Proportional)
}

/// Weight 600 at `size` points.
pub fn semibold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_SEMIBOLD.into()))
}

/// Weight 700 at `size` points.
pub fn bold(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_BOLD.into()))
}

/// Weight 300 at `size` points.
pub fn light(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_LIGHT.into()))
}

/// The script face of the wordmark at `size` points.
pub fn wordmark(size: f32) -> FontId {
    FontId::new(size, FontFamily::Name(FAMILY_WORDMARK.into()))
}

/// The family that prefers the colour emoji face: for a character the text asks to be shown
/// as an emoji although the text font has a plain glyph for it.
pub fn emoji_family() -> FontFamily {
    FontFamily::Name(FAMILY_EMOJI.into())
}

/// The monospace face (paths) at `size` points.
pub fn monospace(size: f32) -> FontId {
    FontId::new(size, FontFamily::Monospace)
}

/// The height of a line of Segoe UI with the CSS `line-height: normal`.
///
/// A browser rounds the ascent (1.079 em) and the descent (0.251 em) of the face to whole pixels
/// separately; that gives 19 px for 14 px text, 16 for 12, 21 for 16.
pub fn normal_line_height(size: f32) -> f32 {
    const SEGOE_UI_ASCENT_EM: f32 = 2210.0 / 2048.0;
    const SEGOE_UI_DESCENT_EM: f32 = 514.0 / 2048.0;
    (size * SEGOE_UI_ASCENT_EM).round() + (size * SEGOE_UI_DESCENT_EM).round()
}

/// What `install_fonts` found, for the log.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct FontReport {
    /// Font files that were loaded, as "family: file".
    pub loaded: Vec<String>,
    /// Font files that could not be read; the family then falls back to another face.
    pub missing: Vec<String>,
}

/// The Windows fonts folder.
fn fonts_folder() -> PathBuf {
    PathBuf::from(std::env::var_os("WINDIR").unwrap_or_else(|| "C:\\Windows".into())).join("Fonts")
}

/// Marker in the egui context: the named font families exist.
fn fonts_marker() -> egui::Id {
    egui::Id::unique("instagram-windows-app/fonts-installed")
}

/// True once `install_fonts` has run for this context. Text in a named family must not be laid
/// out before, because egui panics on an unknown family.
pub fn fonts_installed(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<bool>(fonts_marker()).unwrap_or(false))
}

/// Loads Segoe UI (regular, semibold, bold, light), Segoe Script and Consolas from the Windows
/// fonts folder and defines one family per weight.
///
/// A missing file is tolerated: its family falls back to the regular face, and without Segoe UI
/// at all to the font bundled with egui. Emoji, CJK and every other script come from the system
/// font fallback that eframe installs (`NativeOptions::system_font_fallback`). The fonts take
/// effect at the start of the next frame.
pub fn install_fonts(ctx: &egui::Context) -> FontReport {
    let folder = fonts_folder();
    let mut report = FontReport::default();
    let mut definitions = FontDefinitions::default();
    let bundled_proportional = definitions.families.get(&FontFamily::Proportional).cloned().unwrap_or_default();
    let bundled_monospace = definitions.families.get(&FontFamily::Monospace).cloned().unwrap_or_default();

    let mut load = |definitions: &mut FontDefinitions, key: &str, file: &str| -> bool {
        match std::fs::read(folder.join(file)) {
            Ok(bytes) => {
                definitions.font_data.insert(key.to_owned(), Arc::new(FontData::from_owned(bytes)));
                report.loaded.push(format!("{key}: {file}"));
                true
            }
            Err(error) => {
                report.missing.push(format!("{file} ({error})"));
                false
            }
        }
    };

    let has_regular = load(&mut definitions, "segoe-ui", "segoeui.ttf");
    let has_semibold = load(&mut definitions, "segoe-ui-semibold", "seguisb.ttf");
    let has_bold = load(&mut definitions, "segoe-ui-bold", "segoeuib.ttf");
    let has_light = load(&mut definitions, "segoe-ui-light", "segoeuil.ttf");
    let has_script = load(&mut definitions, "segoe-script", "segoesc.ttf");
    let has_mono = load(&mut definitions, "consolas", "consola.ttf");
    // Emoji would also come from the system font fallback, but egui centres a fallback face on
    // the line instead of sharing the baseline, which lifts Segoe UI Emoji about two pixels above
    // where a browser draws it. Loaded by name, the face can be moved down to the baseline.
    let has_emoji = load(&mut definitions, "segoe-ui-emoji", "seguiemj.ttf");
    if let Some(emoji) = definitions.font_data.get_mut("segoe-ui-emoji") {
        Arc::make_mut(emoji).tweak.y_offset_factor = EMOJI_BASELINE_SHIFT_EM;
    }

    // Every chain ends with the fonts bundled with egui, so a family is never empty and egui's
    // own widget glyphs (the "egui-icons" font) stay available.
    let chain = |faces: &[(&str, bool)], tail: &[String]| -> Vec<String> {
        let mut names: Vec<String> = faces.iter().filter(|(_, loaded)| *loaded).map(|(name, _)| (*name).to_owned()).collect();
        for name in tail {
            if !names.contains(name) {
                names.push(name.clone());
            }
        }
        names
    };
    let regular = ("segoe-ui", has_regular);
    let emoji = ("segoe-ui-emoji", has_emoji);
    definitions.families.insert(FontFamily::Proportional, chain(&[regular, emoji], &bundled_proportional));
    definitions
        .families
        .insert(FontFamily::Name(FAMILY_SEMIBOLD.into()), chain(&[("segoe-ui-semibold", has_semibold), ("segoe-ui-bold", has_bold), regular, emoji], &bundled_proportional));
    definitions
        .families
        .insert(FontFamily::Name(FAMILY_BOLD.into()), chain(&[("segoe-ui-bold", has_bold), ("segoe-ui-semibold", has_semibold), regular, emoji], &bundled_proportional));
    definitions.families.insert(FontFamily::Name(FAMILY_LIGHT.into()), chain(&[("segoe-ui-light", has_light), regular, emoji], &bundled_proportional));
    definitions
        .families
        .insert(FontFamily::Name(FAMILY_WORDMARK.into()), chain(&[("segoe-script", has_script), ("segoe-ui-bold", has_bold), regular, emoji], &bundled_proportional));
    definitions.families.insert(FontFamily::Monospace, chain(&[("consolas", has_mono), emoji], &bundled_monospace));
    definitions.families.insert(emoji_family(), chain(&[emoji, regular], &bundled_proportional));

    ctx.set_fonts(definitions);
    ctx.data_mut(|data| data.insert_temp(fonts_marker(), true));
    report
}

// --- egui style ---------------------------------------------------------------------------------

/// The egui style of one appearance. Almost everything in this app is painted by hand from the
/// palette; the style covers what egui paints itself: scrollbars, text selection, the caret,
/// tooltips and the widgets later dialogs use.
pub fn style(theme: Theme) -> egui::Style {
    let colors = palette(theme);
    let mut style = egui::Style::default();
    let mut visuals = if colors.dark { egui::Visuals::dark() } else { egui::Visuals::light() };

    visuals.panel_fill = colors.bg;
    visuals.window_fill = colors.elevated;
    visuals.window_stroke = egui::Stroke::new(1.0, colors.border);
    visuals.window_corner_radius = egui::CornerRadius::same(12);
    visuals.window_shadow = egui::Shadow::NONE;
    visuals.menu_corner_radius = egui::CornerRadius::same(8);
    visuals.popup_shadow = egui::Shadow { offset: [0, 4], blur: 24, spread: 0, color: Color32::from_black_alpha(46) };
    visuals.extreme_bg_color = colors.surface;
    visuals.faint_bg_color = colors.soft;
    visuals.code_bg_color = colors.soft;
    visuals.hyperlink_color = colors.link;
    visuals.error_fg_color = DANGER_RED;
    visuals.warn_fg_color = colors.warning;
    // `::selection { background: #0095F6; color: #fff }`.
    visuals.selection.bg_fill = colors.blue;
    visuals.selection.stroke = egui::Stroke::new(1.0, Color32::WHITE);
    visuals.text_cursor.stroke = egui::Stroke::new(1.0, colors.text);

    let widgets = &mut visuals.widgets;
    widgets.noninteractive.fg_stroke.color = colors.text;
    widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0, colors.border);
    widgets.noninteractive.bg_fill = colors.surface;
    widgets.noninteractive.weak_bg_fill = colors.surface;
    for state in [&mut widgets.inactive, &mut widgets.hovered, &mut widgets.active, &mut widgets.open] {
        // `bg_fill` is what egui paints a scrollbar thumb with; the web project's thumb is the
        // same colour in every state and 8 px round.
        state.bg_fill = colors.soft_hover;
        state.corner_radius = egui::CornerRadius::same(8);
        state.fg_stroke.color = colors.text;
        state.bg_stroke = egui::Stroke::NONE;
        state.expansion = 0.0;
    }
    widgets.inactive.weak_bg_fill = colors.soft;
    widgets.hovered.weak_bg_fill = colors.soft_hover;
    widgets.active.weak_bg_fill = colors.soft_hover;
    widgets.open.weak_bg_fill = colors.soft_hover;
    style.visuals = visuals;

    // An 8 px bar that is always there, with a transparent track.
    let scroll = &mut style.spacing.scroll;
    *scroll = egui::style::ScrollStyle::solid();
    scroll.floating = true;
    scroll.bar_width = SCROLLBAR_WIDTH;
    scroll.floating_width = SCROLLBAR_WIDTH;
    scroll.floating_allocated_width = SCROLLBAR_WIDTH;
    scroll.bar_inner_margin = 0.0;
    scroll.bar_outer_margin = 0.0;
    scroll.handle_min_length = 24.0;
    scroll.foreground_color = false;
    scroll.dormant_background_opacity = 0.0;
    scroll.active_background_opacity = 0.0;
    scroll.interact_background_opacity = 0.0;
    scroll.dormant_handle_opacity = 1.0;
    scroll.active_handle_opacity = 1.0;
    scroll.interact_handle_opacity = 1.0;

    style.text_styles = [
        (egui::TextStyle::Small, regular(12.0)),
        (egui::TextStyle::Body, regular(14.0)),
        (egui::TextStyle::Button, regular(14.0)),
        (egui::TextStyle::Heading, semibold(18.0)),
        (egui::TextStyle::Monospace, monospace(12.0)),
    ]
    .into();
    style.interaction.selectable_labels = false;
    style.interaction.tooltip_delay = 0.5;
    style
}

/// Marker in the egui context: the styles of both appearances are installed.
fn styles_marker() -> egui::Id {
    egui::Id::unique("instagram-windows-app/styles-installed")
}

/// True once `install_styles` has run for this context.
pub fn styles_installed(ctx: &egui::Context) -> bool {
    ctx.data(|data| data.get_temp::<bool>(styles_marker()).unwrap_or(false))
}

/// Installs the styles of both appearances and selects `theme`.
pub fn install_styles(ctx: &egui::Context, theme: Theme) {
    ctx.set_style_of(egui::Theme::Light, style(Theme::Light));
    ctx.set_style_of(egui::Theme::Dark, style(Theme::Dark));
    ctx.data_mut(|data| data.insert_temp(styles_marker(), true));
    select_theme(ctx, theme);
}

/// Switches the appearance: the egui style and the native title bar, which would otherwise stay
/// light above a dark page.
pub fn select_theme(ctx: &egui::Context, theme: Theme) {
    let (egui_theme, system_theme) = match theme {
        Theme::Light => (egui::Theme::Light, egui::SystemTheme::Light),
        Theme::Dark => (egui::Theme::Dark, egui::SystemTheme::Dark),
    };
    ctx.set_theme(egui_theme);
    ctx.send_viewport_cmd(egui::ViewportCommand::SetTheme(system_theme));
}

/// Everything a fresh egui context needs before the first frame: fonts, styles and the mouse
/// settings of Windows. The app calls it when the window is created; headless tests call it
/// from their context setup.
pub fn setup_context(ctx: &egui::Context, theme: Theme) -> FontReport {
    let report = install_fonts(ctx);
    install_styles(ctx, theme);
    apply_mouse_settings(ctx, crate::platform::double_click_ms(), crate::platform::wheel_scroll_lines());
    report
}

/// How far one notch of the wheel scrolls per line of the mouse settings: a browser scrolls
/// 100 pixels per notch at Windows' default of three lines, so the feed does the same.
pub const WHEEL_POINTS_PER_LINE: f32 = 100.0 / 3.0;

/// Two clicks within the double-click time of Windows are a double click. egui compares the
/// times of the frames the clicks arrive in, and a software rasteriser draws a frame in up to a
/// quarter of a second, so this much is added.
pub const DOUBLE_CLICK_SLACK_SECONDS: f64 = 0.25;

/// Takes over the mouse settings of Windows: the double-click time (`double_click_ms`) and the
/// lines one notch of the wheel scrolls (`wheel_lines`). egui's own defaults (0.3 s, 40 points
/// per notch) are stricter and slower than the rest of Windows and the clone in a browser.
pub fn apply_mouse_settings(ctx: &egui::Context, double_click_ms: u32, wheel_lines: u32) {
    ctx.options_mut(|options| {
        options.input_options.max_double_click_delay = f64::from(double_click_ms) / 1000.0 + DOUBLE_CLICK_SLACK_SECONDS;
        options.input_options.line_scroll_speed = WHEEL_POINTS_PER_LINE * wheel_lines as f32;
    });
}

/// The points one notch of the mouse wheel scrolls in `ctx` (`apply_mouse_settings`).
pub fn wheel_notch_points(ctx: &egui::Context) -> f32 {
    ctx.options(|options| options.input_options.line_scroll_speed).max(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_mouse_settings_of_windows_are_taken_over() {
        let ctx = egui::Context::default();
        // egui's defaults: a double click within 0.3 s, 40 points per notch.
        apply_mouse_settings(&ctx, 500, 3);
        let (double_click, line) = ctx.options(|options| (options.input_options.max_double_click_delay, options.input_options.line_scroll_speed));
        assert!((double_click - 0.75).abs() < 1e-9, "{double_click}");
        assert!((line - 100.0).abs() < 1e-3, "{line}");
        assert!((wheel_notch_points(&ctx) - 100.0).abs() < 1e-3);
        apply_mouse_settings(&ctx, 900, 1);
        assert!((wheel_notch_points(&ctx) - 100.0 / 3.0).abs() < 1e-3);
        // setup_context applies them.
        let fresh = egui::Context::default();
        setup_context(&fresh, Theme::Light);
        let expected = WHEEL_POINTS_PER_LINE * crate::platform::wheel_scroll_lines() as f32;
        assert!((wheel_notch_points(&fresh) - expected).abs() < 1e-3);
        assert!(fresh.options(|options| options.input_options.max_double_click_delay) >= 0.3);
    }

    #[test]
    fn palettes_match_the_resolved_css_variables() {
        assert_eq!(LIGHT.bg, Color32::from_rgb(250, 250, 250));
        assert_eq!(LIGHT.text, Color32::from_rgb(38, 38, 38));
        assert_eq!(DARK.bg, Color32::from_rgb(24, 25, 26));
        assert_eq!(DARK.surface, Color32::from_rgb(36, 37, 38));
        assert_eq!(DARK.link, Color32::from_rgb(224, 241, 255));
        assert_eq!(palette(Theme::Dark), &DARK);
        assert_eq!(palette(Theme::Light), &LIGHT);
    }

    #[test]
    fn the_hover_layers_composite_to_the_derived_opaque_colours() {
        // Light: rgba(0,0,0,.05) over #FFFFFF is #F2F2F2; dark: rgba(255,255,255,.1) over #242526
        // is #3A3B3C (spec-style 1.2).
        let over = |layer: Color32, base: Color32| -> [u8; 3] {
            let alpha = layer.a() as f32 / 255.0;
            let channel = |top: u8, bottom: u8| (top as f32 + bottom as f32 * (1.0 - alpha)).round() as u8;
            [channel(layer.r(), base.r()), channel(layer.g(), base.g()), channel(layer.b(), base.b())]
        };
        assert_eq!(over(LIGHT.hover, LIGHT.surface), [0xF2, 0xF2, 0xF2]);
        assert_eq!(over(DARK.hover, DARK.surface), [0x3A, 0x3B, 0x3C]);
    }

    #[test]
    fn normal_line_heights_are_those_of_segoe_ui_in_a_browser() {
        assert_eq!(normal_line_height(14.0), 19.0);
        assert_eq!(normal_line_height(12.0), 16.0);
        assert_eq!(normal_line_height(16.0), 21.0);
        assert_eq!(normal_line_height(13.0), 17.0);
    }
}
