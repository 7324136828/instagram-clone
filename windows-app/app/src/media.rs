//! What the feed, the grid and the viewer share when they show the pictures of a post: asking
//! the loader for a cover-cropped slide, the placeholder of a picture that is loading or cannot
//! be shown, and the controls of a carousel.
//!
//! The reference web project has one component for all of this (`Media.tsx`, `common.tsx:150-246`,
//! `styles.css:376-390`): slides side by side on a track, a counter pill, dots, two round arrows.

use std::path::PathBuf;

use egui::epaint::Mesh;
use egui::{pos2, vec2, Align2, Color32, Id, Painter, Pos2, Rect, Ui, Vec2};
use instagram_core::messages;
use instagram_core::model::{MediaItem, MediaKind, Post};

use crate::gradients;
use crate::icons::{self, Icon, IconStyle, CAROUSEL_STROKE_WIDTH};
use crate::layout;
use crate::loader::{ImageState, Loader, Picture, PosterState, Priority, SizeState, Variant};
use crate::strings;
use crate::theme::{self, Palette};
use crate::widgets;

/// Diameter of a carousel arrow (`.media__nav`).
pub const CAROUSEL_ARROW_SIZE: f32 = 30.0;
/// Distance of a carousel arrow from the side of the frame.
pub const CAROUSEL_ARROW_INSET: f32 = 10.0;
/// Distance of the counter pill, the dots and the badges from the edge of the frame.
const CHROME_INSET: f32 = 12.0;
/// Diameter of a carousel dot.
const DOT_SIZE: f32 = 6.0;
/// Space between two dots.
const DOT_GAP: f32 = 4.0;
/// Font size of the pills drawn on a picture (counter, reason, note).
const PILL_FONT_SIZE: f32 = 12.0;
/// The dark, translucent background of those pills: `rgba(18, 18, 18, .72)`.
pub fn pill_fill() -> Color32 {
    theme::rgba(18, 18, 18, 0.72)
}

/// What a slide shows.
pub enum SlideState {
    /// The picture in the wanted rendition.
    Ready(Picture),
    /// A stand-in (another rendition of the same file) while the right one is decoded.
    StandIn(Picture),
    /// Nothing yet.
    Loading,
    /// The picture cannot be shown; the sentence says why and names the file.
    Failed(String),
    /// A video without a cover picture.
    NoPicture,
}

impl SlideState {
    /// The reason when the picture cannot be shown.
    pub fn failure(&self) -> Option<&str> {
        match self {
            SlideState::Failed(message) => Some(message),
            _ => None,
        }
    }
}

/// The picture file that stands for a media item on screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Display {
    /// This file: the picture itself, the poster an export ships, or the poster made from the
    /// first frame of a video without one.
    File(PathBuf),
    /// The poster of a video is being made.
    Pending,
    /// None: a video whose first frame cannot be read (the placeholder is shown).
    Nothing,
}

/// What stands for `media` on screen. A video without a poster of its own gets one made from its
/// first frame (video brief 5.2: for display only, never in an export).
pub fn display_file(loader: &mut Loader, media: &MediaItem, priority: Priority) -> Display {
    if let Some(path) = media.display_path() {
        return Display::File(path.to_path_buf());
    }
    if media.kind != MediaKind::Video {
        return Display::Nothing;
    }
    match loader.poster_of(&media.path, priority) {
        PosterState::Ready(poster) => Display::File(poster),
        PosterState::Pending => Display::Pending,
        PosterState::Failed(_) => Display::Nothing,
    }
}

/// `display_file` without starting any work.
pub fn peek_display_file(loader: &Loader, media: &MediaItem) -> Display {
    if let Some(path) = media.display_path() {
        return Display::File(path.to_path_buf());
    }
    if media.kind != MediaKind::Video {
        return Display::Nothing;
    }
    match loader.peek_poster(&media.path) {
        PosterState::Ready(poster) => Display::File(poster),
        PosterState::Pending => Display::Pending,
        PosterState::Failed(_) => Display::Nothing,
    }
}

/// True when slide `slide` of `post` is a video file (not a cover-only video post).
pub fn is_video_slide(post: &Post, slide: usize) -> bool {
    post.media.get(slide).is_some_and(|media| media.kind == MediaKind::Video)
}

/// Asks the loader for slide `slide` of `post`, cover-cropped for a frame of `frame_size` points.
pub fn cover_picture(loader: &mut Loader, ctx: &egui::Context, post: &Post, slide: usize, frame_size: Vec2, priority: Priority) -> SlideState {
    let Some(media) = post.media.get(slide) else {
        return SlideState::NoPicture;
    };
    let path = match display_file(loader, media, priority) {
        Display::File(path) => path,
        Display::Pending => return SlideState::Loading,
        Display::Nothing => return SlideState::NoPicture,
    };
    let path = path.as_path();
    let source_size = match loader.size_of(path, priority) {
        SizeState::Known(size) => size,
        SizeState::Pending => return SlideState::Loading,
        SizeState::Failed(message) => return SlideState::Failed(message),
    };
    let scale = ctx.pixels_per_point();
    let frame_px = ((frame_size.x * scale).round().max(1.0) as u32, (frame_size.y * scale).round().max(1.0) as u32);
    let variant = Variant::Cover { min_side_px: layout::cover_decode_side(source_size, frame_px) };
    match loader.image(path, variant, priority) {
        ImageState::Ready(picture) => SlideState::Ready(picture),
        ImageState::Failed(message) => SlideState::Failed(message),
        ImageState::Loading => match loader.any_rendition(path) {
            Some(picture) => SlideState::StandIn(picture),
            None => SlideState::Loading,
        },
    }
}

/// The overlay the web project lays over a placeholder (`.photo::after`): slightly darker at
/// the top and the bottom.
fn placeholder_shade(painter: &Painter, rect: Rect) {
    let stops = [(0.0, 0.06), (0.22, 0.0), (0.72, 0.0), (1.0, 0.16)];
    let mut mesh = Mesh::default();
    for (position, opacity) in stops {
        let y = rect.top() + rect.height() * position;
        let color = theme::rgba(0, 0, 0, opacity);
        mesh.colored_vertex(pos2(rect.left(), y), color);
        mesh.colored_vertex(pos2(rect.right(), y), color);
    }
    for band in 0..stops.len() as u32 - 1 {
        let first = band * 2;
        mesh.add_triangle(first, first + 1, first + 2);
        mesh.add_triangle(first + 1, first + 2, first + 3);
    }
    painter.add(egui::Shape::mesh(mesh));
}

/// A sentence in a dark pill, centred on `center_x` with its bottom edge at `bottom`: the reason
/// a picture cannot be shown. The text wraps at `max_width`.
pub fn message_pill(painter: &Painter, center_x: f32, bottom: f32, max_width: f32, message: &str) -> Rect {
    let ctx = painter.ctx().clone();
    let galley = widgets::wrapped(&ctx, message, theme::regular(PILL_FONT_SIZE), Color32::WHITE, 16.8, (max_width - 20.0).max(40.0));
    let size = galley.size() + vec2(20.0, 12.0);
    let pill = Rect::from_min_size(widgets::snap(&ctx, pos2(center_x - size.x / 2.0, bottom - size.y)), size);
    painter.rect_filled(pill, 10.0, pill_fill());
    widgets::paint_galley(painter, pill.min + vec2(10.0, 6.0), galley);
    pill
}

/// How the placeholder of a cover-cropped slide looks in its place.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PlaceholderLook {
    /// Font size of the emoji on a picture that cannot be shown (96 in the feed, 64 on a tile).
    pub emoji_size: f32,
    /// Opacity of that emoji (.28 in the feed, .3 on a tile).
    pub emoji_opacity: f32,
    /// Where the bottom edge of the reason pill is, measured from the bottom of the frame; None
    /// to leave the reason to the tooltip (a tile is too small for a sentence).
    pub reason_bottom: Option<f32>,
}

impl PlaceholderLook {
    /// The placeholder of a feed card. The reason sits above the dots of a carousel.
    pub fn feed(has_dots: bool) -> PlaceholderLook {
        PlaceholderLook { emoji_size: 96.0, emoji_opacity: 0.28, reason_bottom: Some(if has_dots { 30.0 } else { CHROME_INSET }) }
    }

    /// The placeholder of a grid tile.
    pub fn tile() -> PlaceholderLook {
        PlaceholderLook { emoji_size: 64.0, emoji_opacity: 0.3, reason_bottom: None }
    }
}

/// Paints one cover-cropped slide into `rect` (already clipped to the frame by the painter).
pub fn paint_cover_slide(painter: &Painter, rect: Rect, post: &Post, state: &SlideState, look: PlaceholderLook, palette: &Palette) {
    let ctx = painter.ctx().clone();
    let gradient = |painter: &Painter| {
        let texture = gradients::placeholder_texture(&ctx, &post.user.username);
        painter.image(texture.id(), rect, widgets::full_uv(), Color32::WHITE);
    };
    let emoji = |painter: &Painter, glyph: &str| {
        // `.photo__subject`: a large, faint emoji in the middle.
        painter.text(rect.center(), Align2::CENTER_CENTER, glyph, theme::regular(look.emoji_size), theme::with_opacity(Color32::WHITE, look.emoji_opacity));
    };
    match state {
        SlideState::Ready(picture) | SlideState::StandIn(picture) => {
            painter.rect_filled(rect, 0.0, palette.soft);
            let uv = layout::cover_uv(vec2(picture.size_px[0] as f32, picture.size_px[1] as f32), rect.size());
            painter.image(picture.texture.id(), rect, uv, Color32::WHITE);
        }
        SlideState::Loading => gradient(painter),
        SlideState::NoPicture => {
            gradient(painter);
            placeholder_shade(painter, rect);
            emoji(painter, strings::PLACEHOLDER_VIDEO_EMOJI);
        }
        SlideState::Failed(message) => {
            gradient(painter);
            placeholder_shade(painter, rect);
            emoji(painter, strings::PLACEHOLDER_PHOTO_EMOJI);
            if let Some(bottom) = look.reason_bottom {
                // The reason, in the style of the carousel counter.
                message_pill(painter, rect.center().x, rect.bottom() - bottom, rect.width() - 48.0, message);
            }
        }
    }
}

/// The accessible name of a slide: its alternative text, and the reason when it cannot be shown.
pub fn slide_label(post: &Post, slide: usize, failure: Option<&str>) -> String {
    let alt = post.alt_text(slide);
    match failure {
        Some(message) => format!("{} {message}", messages::image_unavailable(&alt)),
        None => alt,
    }
}

/// The badge of a video, top left of its frame (`.media__badge`), named "Video".
pub fn video_badge(ui: &Ui, frame: Rect, id: Id) {
    let center = pos2(frame.left() + CHROME_INSET + 10.0, frame.top() + CHROME_INSET + 10.0);
    icons::paint(ui.painter(), Icon::Clip, center, IconStyle::outline(20.0).with_shadow(0.5), Color32::WHITE);
    widgets::text_label(ui, Rect::from_center_size(center, Vec2::splat(20.0)), id, strings::POST_VIDEO);
}

/// Where the controls of a carousel go.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CarouselChrome {
    /// The frame of the pictures.
    pub frame: Rect,
    /// The slide on show (0-based).
    pub current: usize,
    /// How many slides there are.
    pub count: usize,
    /// The dots are left out (a video slide of the viewer: its controls bar is where the dots
    /// would be, `styles.css:517`).
    pub hide_dots: bool,
    /// Distance of the arrows from the left and right side of the frame. It is 10 px in the web
    /// project; the viewer moves them inwards when its own arrows sit on the picture.
    pub arrow_inset: f32,
}

/// Shows the controls of a carousel over its frame: the counter pill top right, the dots at the
/// bottom, and the arrows (none before the first or after the last slide). Returns the slide an
/// arrow asks for.
pub fn carousel_chrome(ui: &Ui, id: Id, chrome: CarouselChrome) -> Option<usize> {
    let CarouselChrome { frame, current, count, arrow_inset, hide_dots } = chrome;
    if count < 2 {
        return None;
    }
    let ctx = ui.ctx().clone();
    let painter = ui.painter();

    // Counter pill, top right: "1/2".
    let counter = strings::carousel_counter(current + 1, count);
    let galley = widgets::line(&ctx, &counter, theme::regular(PILL_FONT_SIZE), Color32::WHITE, 14.4);
    let size = galley.size() + vec2(20.0, 10.0);
    let pill = Rect::from_min_size(widgets::snap(&ctx, pos2(frame.right() - CHROME_INSET - size.x, frame.top() + CHROME_INSET)), size);
    painter.rect_filled(pill, size.y / 2.0, pill_fill());
    widgets::paint_galley(painter, pill.min + vec2(10.0, 5.0), galley);
    widgets::text_label(ui, pill, id.with("counter"), &counter);

    // Dots, bottom centre; the current one is white and a quarter larger.
    let dots_width = count as f32 * DOT_SIZE + (count - 1) as f32 * DOT_GAP;
    let mut x = frame.center().x - dots_width / 2.0 + DOT_SIZE / 2.0;
    let y = frame.bottom() - CHROME_INSET - DOT_SIZE / 2.0;
    for slide in (0..count).filter(|_| !hide_dots) {
        let active = slide == current;
        let radius = if active { DOT_SIZE / 2.0 * 1.25 } else { DOT_SIZE / 2.0 };
        // `box-shadow: 0 0 2px rgba(0, 0, 0, .45)`.
        painter.circle_filled(pos2(x, y), radius + 1.0, theme::rgba(0, 0, 0, 0.16));
        painter.circle_filled(pos2(x, y), radius, if active { Color32::WHITE } else { theme::rgba(255, 255, 255, 0.55) });
        x += DOT_SIZE + DOT_GAP;
    }

    // Arrows: a 30 px white disc with a chevron.
    let half = CAROUSEL_ARROW_SIZE / 2.0;
    let arrows = [
        (current > 0, frame.left() + arrow_inset + half, Icon::ChevronLeft, strings::POST_PREVIOUS_PHOTO, current.saturating_sub(1), "previous"),
        (current + 1 < count, frame.right() - arrow_inset - half, Icon::ChevronRight, strings::POST_NEXT_PHOTO, current + 1, "next"),
    ];
    let mut wanted = None;
    for (shown, x, icon, label, target, salt) in arrows {
        if !shown {
            continue;
        }
        let center = pos2(x, frame.center().y);
        if paint_round_arrow(ui, center, id.with(salt), icon, label) {
            wanted = Some(target);
        }
    }
    wanted
}

/// One arrow of a carousel (`.media__nav`): a white disc with a soft shadow and a dark chevron,
/// fully opaque under the pointer. Returns true when it was clicked.
fn paint_round_arrow(ui: &Ui, center: Pos2, id: Id, icon: Icon, label: &str) -> bool {
    let half = CAROUSEL_ARROW_SIZE / 2.0;
    let rect = Rect::from_center_size(center, Vec2::splat(CAROUSEL_ARROW_SIZE));
    let response = widgets::hit(ui, rect, id, label).on_hover_text(label);
    let painter = ui.painter();
    widgets::box_shadow(painter, rect, half, 1, 6, 0.35);
    let hover = widgets::transition(ui, id.with("hover"), response.hovered(), 0.15);
    painter.circle_filled(center, half, theme::rgba(255, 255, 255, 0.88 + 0.12 * hover));
    icons::paint(painter, icon, center, IconStyle::outline(16.0).with_stroke_width(CAROUSEL_STROKE_WIDTH), theme::CAROUSEL_ARROW_GLYPH);
    widgets::focus_ring(ui, &response, rect, half, theme::FOCUS_RING_OFFSET);
    response.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_reason_of_a_feed_placeholder_clears_the_dots() {
        assert_eq!(PlaceholderLook::feed(false).reason_bottom, Some(12.0));
        assert_eq!(PlaceholderLook::feed(true).reason_bottom, Some(30.0));
        assert_eq!(PlaceholderLook::tile().reason_bottom, None);
    }
}
