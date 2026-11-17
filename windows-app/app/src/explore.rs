//! The Explore view: every post as a square tile in a grid of three columns.
//!
//! It is the reference web project's grid of an import (`Panels.tsx:292-366`, `Media.tsx:190-214`,
//! `styles.css:315-322, 392-400`): the heading, tiles 4 px apart that show the cover of a post
//! centre-cropped, a badge for a carousel or a video, and an overlay with the like and comment
//! counts under the pointer. A click opens the viewer. Only the rows in and around the view are
//! painted and decoded, so the grid costs the same for ten pictures and for ten thousand.

use egui::{pos2, vec2, Color32, CornerRadius, Id, Key, Modifiers, Pos2, Rect, Sense, Ui, UiBuilder, Vec2};
use instagram_core::model::Post;
use instagram_core::text::format_count;

use crate::icons::{self, Icon, IconStyle};
use crate::loader::Priority;
use crate::media::{self, PlaceholderLook};
use crate::state::{Action, Env};
use crate::strings;
use crate::theme::{self, MAIN_PADDING_BOTTOM, MAIN_PADDING_TOP, MAIN_PADDING_X};
use crate::widgets;

/// The widest grid (`.main__inner` of the explore view: `max-width: 935px`).
const MAX_WIDTH: f32 = 935.0;
/// Tiles per row.
pub const COLUMNS: usize = 3;
/// Space between two tiles.
const GAP: f32 = 4.0;
/// Corner radius of a tile.
const TILE_RADIUS: u8 = 2;
/// Font size of the heading.
const TITLE_SIZE: f32 = 16.0;
/// Space between the heading and the grid.
const TITLE_MARGIN: f32 = 16.0;
/// Side of the badge and of the icons of the overlay.
const TILE_ICON_SIZE: f32 = 18.0;
/// Distance of the badge from the top and the right of its tile.
const BADGE_INSET: f32 = 8.0;
/// Space between the parts of the overlay (`gap: 8px`).
const OVERLAY_GAP: f32 = 8.0;
/// Rows this far outside the view are decoded ahead.
const OVERSCAN: f32 = 400.0;
/// One press of an arrow key scrolls this far.
const ARROW_SCROLL: f32 = 60.0;
/// Page Up / Page Down keep this much of the previous view on screen.
const PAGE_OVERLAP: f32 = 60.0;

/// Where the grid is in the main area: pure arithmetic.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct GridLayout {
    /// Left edge of the grid.
    pub left: f32,
    /// Width of the grid.
    pub width: f32,
    /// Side of a tile; the three tiles and the two gaps fill the width.
    pub tile: f32,
}

impl GridLayout {
    /// The grid for a main area from `main_left` to `main_right`: centred, at most 935 points
    /// wide, with the main area's 20 px padding on both sides.
    pub fn new(main_left: f32, main_right: f32) -> GridLayout {
        let available = (main_right - main_left - 2.0 * MAIN_PADDING_X).max(COLUMNS as f32 * 20.0);
        let width = available.min(MAX_WIDTH);
        let left = ((main_left + main_right) / 2.0 - width / 2.0).round();
        let tile = (width - GAP * (COLUMNS - 1) as f32) / COLUMNS as f32;
        GridLayout { left, width, tile }
    }

    /// How many rows `posts` posts take.
    pub fn rows(&self, posts: usize) -> usize {
        posts.div_ceil(COLUMNS)
    }

    /// The distance from the top of one row to the top of the next.
    pub fn row_pitch(&self) -> f32 {
        self.tile + GAP
    }

    /// The height of `rows` rows with the gaps between them.
    pub fn height(&self, rows: usize) -> f32 {
        if rows == 0 {
            0.0
        } else {
            rows as f32 * self.row_pitch() - GAP
        }
    }

    /// The rectangle of the tile in `row` and `column`, relative to the top of the grid, with
    /// its edges on whole points so that neighbouring tiles neither overlap nor leave a seam.
    pub fn tile_rect(&self, row: usize, column: usize) -> Rect {
        let x = self.left + column as f32 * self.row_pitch();
        let y = row as f32 * self.row_pitch();
        Rect::from_min_max(pos2(x.round(), y.round()), pos2((x + self.tile).round(), (y + self.tile).round()))
    }

    /// The rows that intersect the band `top..bottom` (relative to the top of the grid),
    /// widened by `overscan`, out of `rows` rows.
    pub fn visible_rows(&self, top: f32, bottom: f32, overscan: f32, rows: usize) -> std::ops::Range<usize> {
        if rows == 0 || bottom + overscan < 0.0 || top - overscan > self.height(rows) {
            return 0..0;
        }
        let first = ((top - overscan) / self.row_pitch()).floor().max(0.0) as usize;
        let last = ((bottom + overscan) / self.row_pitch()).floor().max(0.0) as usize;
        first.min(rows - 1)..(last + 1).min(rows)
    }
}

/// The grid view: what it remembers between frames.
#[derive(Default)]
pub struct ExploreView {
    /// How many tiles the last frame painted (for the log and the tests).
    painted_tiles: usize,
    last_offset: f32,
    /// False while the view is still moving (the screenshot harness waits for it).
    settled: bool,
    /// A scroll position asked for from outside the view (the harness), for the next frame.
    jump: Option<f32>,
}

impl ExploreView {
    /// How many tiles the last frame painted: a few rows, however many posts there are.
    pub fn painted_tiles(&self) -> usize {
        self.painted_tiles
    }

    /// The scroll position of the last frame.
    pub fn scroll_offset(&self) -> f32 {
        self.last_offset
    }

    /// True when the last frame did not scroll.
    pub fn is_settled(&self) -> bool {
        self.settled && self.jump.is_none()
    }

    /// Scrolls to `offset` points from the top at the next frame.
    pub fn jump_to(&mut self, offset: f32) {
        self.jump = Some(offset);
    }

    /// Shows the grid in `main_rect` (the window right of the sidebar).
    pub fn show(&mut self, ui: &mut Ui, main_rect: Rect, env: &mut Env<'_>) {
        let ctx = ui.ctx().clone();
        let state = env.state;
        let palette = state.palette();
        let Some(collection) = state.collection.as_ref() else {
            return;
        };
        let posts = collection.posts.as_slice();
        let grid = GridLayout::new(main_rect.left(), main_rect.right());
        let rows = grid.rows(posts.len());
        let title_height = theme::normal_line_height(TITLE_SIZE);
        let grid_origin = MAIN_PADDING_TOP + title_height + TITLE_MARGIN;
        let total_height = grid_origin + grid.height(rows) + MAIN_PADDING_BOTTOM;
        let view_height = main_rect.height();
        let max_offset = (total_height - view_height).max(0.0);

        // The keyboard scrolls while no layer is above the grid and no button has the focus.
        let mut force = self.jump.take().map(|offset| offset.clamp(0.0, max_offset));
        let mut key_delta = 0.0;
        if force.is_none() && !state.overlay_open() && ctx.memory(|memory| memory.focused().is_none()) {
            let pressed = |key: Key| ctx.input_mut(|input| input.consume_key(Modifiers::NONE, key));
            if pressed(Key::Home) {
                force = Some(0.0);
            } else if pressed(Key::End) {
                force = Some(max_offset);
            } else if pressed(Key::PageDown) {
                key_delta = -(view_height - PAGE_OVERLAP).max(ARROW_SCROLL);
            } else if pressed(Key::PageUp) {
                key_delta = (view_height - PAGE_OVERLAP).max(ARROW_SCROLL);
            } else if pressed(Key::ArrowDown) {
                key_delta = -ARROW_SCROLL;
            } else if pressed(Key::ArrowUp) {
                key_delta = ARROW_SCROLL;
            }
        }

        self.painted_tiles = 0;
        let output = ui
            .scope_builder(UiBuilder::new().max_rect(main_rect), |ui| {
                let mut area = egui::ScrollArea::vertical().id_salt("explore").auto_shrink([false, false]);
                if let Some(offset) = force {
                    area = area.vertical_scroll_offset(offset);
                }
                area.show_viewport(ui, |ui, viewport| {
                    let content_top = ui.max_rect().top();
                    ui.allocate_rect(Rect::from_min_size(pos2(main_rect.left(), content_top), vec2(ui.available_width(), total_height)), Sense::hover());
                    if key_delta != 0.0 {
                        ui.scroll_with_delta(vec2(0.0, key_delta));
                    }

                    // The heading.
                    if viewport.top() < MAIN_PADDING_TOP + title_height {
                        let title = widgets::line(&ctx, strings::EXPLORE_TITLE, theme::semibold(TITLE_SIZE), palette.text, title_height);
                        let rect = widgets::paint_line(ui.painter(), grid.left, content_top + MAIN_PADDING_TOP + title_height / 2.0, title);
                        widgets::text_label(ui, rect, Id::unique("explore-title"), strings::EXPLORE_TITLE);
                    }

                    let (band_top, band_bottom) = (viewport.top() - grid_origin, viewport.bottom() - grid_origin);
                    let first_on_screen = (band_top / grid.row_pitch()).floor().max(0.0) as usize;
                    for row in grid.visible_rows(band_top, band_bottom, OVERSCAN, rows) {
                        for column in 0..COLUMNS {
                            let index = row * COLUMNS + column;
                            let Some(post) = posts.get(index) else {
                                break;
                            };
                            let rect = grid.tile_rect(row, column).translate(vec2(0.0, content_top + grid_origin));
                            let on_screen = rect.bottom() > main_rect.top() && rect.top() < main_rect.bottom();
                            let rank = row.abs_diff(first_on_screen) * COLUMNS + column;
                            show_tile(ui, rect, post, on_screen, rank, env);
                            if on_screen {
                                self.painted_tiles += 1;
                            }
                        }
                    }
                })
            })
            .inner;

        let offset = output.state.clamped_offset().y;
        self.settled = (offset - self.last_offset).abs() <= 0.5 && key_delta == 0.0;
        self.last_offset = offset;
    }
}

/// Shows one tile (`.tile.explore__tile`): the cover of `post`, centre-cropped to a square. A
/// tile outside the view is not painted; its picture is only asked for ahead of time.
fn show_tile(ui: &Ui, rect: Rect, post: &Post, on_screen: bool, rank: usize, env: &mut Env<'_>) {
    let ctx = ui.ctx().clone();
    let state = env.state;
    let palette = state.palette();
    let priority = if on_screen { Priority::visible(rank) } else { Priority::near(rank) };
    let slide = media::cover_picture(env.loader, &ctx, post, 0, rect.size(), priority);
    if !on_screen {
        return;
    }

    let id = Id::unique(("explore-tile", post.key.as_str()));
    let mut response = widgets::hit(ui, rect, id, &strings::open_post_by(&post.user.username));
    if let Some(reason) = slide.failure() {
        // A tile is too small for the sentence; it is the tooltip.
        response = response.on_hover_text(reason);
    }
    let clipped = ui.painter().with_clip_rect(rect.intersect(ui.clip_rect()));
    media::paint_cover_slide(&clipped, rect, post, &slide, PlaceholderLook::tile(), palette);
    // `border-radius: 2px` on a square picture: the page colour takes the four corner pixels.
    round_corners(ui, rect, palette.bg);

    // The badge, top right: a carousel, else a video.
    let badge = if post.media.len() > 1 {
        Some((Icon::Carousel, strings::TILE_CAROUSEL))
    } else if post.is_video() {
        Some((Icon::Clip, strings::POST_VIDEO))
    } else {
        None
    };
    if let Some((icon, label)) = badge {
        let center = pos2(rect.right() - BADGE_INSET - TILE_ICON_SIZE / 2.0, rect.top() + BADGE_INSET + TILE_ICON_SIZE / 2.0);
        icons::paint(ui.painter(), icon, center, IconStyle::outline(TILE_ICON_SIZE).with_shadow(0.55), Color32::WHITE);
        widgets::text_label(ui, Rect::from_center_size(center, Vec2::splat(TILE_ICON_SIZE)), id.with("badge"), label);
    }

    // The overlay under the pointer and for keyboard focus: the counts, white on a dark veil.
    let shown = widgets::transition(ui, id.with("overlay"), response.hovered() || response.has_focus(), 0.15);
    if shown > 0.0 {
        let painter = ui.painter();
        painter.rect_filled(rect, CornerRadius::same(TILE_RADIUS), theme::rgba(0, 0, 0, 0.35 * shown));
        let color = theme::with_opacity(Color32::WHITE, shown);
        // Likes only when the count is known; comments only for a post of an export (a loose
        // picture has none to count).
        let mut parts: Vec<(Icon, String)> = Vec::new();
        if let Some(likes) = post.like_count {
            parts.push((Icon::Heart, format_count(likes)));
        }
        if env.state.collection.as_ref().is_some_and(|collection| collection.is_export()) {
            parts.push((Icon::Comment, format_count(post.comment_count)));
        }
        let galleys: Vec<_> = parts.iter().map(|(_, text)| widgets::line(&ctx, text, theme::semibold(16.0), color, theme::normal_line_height(16.0))).collect();
        let items = parts.len() * 2;
        let width: f32 = galleys.iter().map(|galley| TILE_ICON_SIZE + galley.size().x).sum::<f32>() + OVERLAY_GAP * items.saturating_sub(1) as f32;
        let mut x = rect.center().x - width / 2.0;
        for ((icon, text), galley) in parts.iter().zip(galleys) {
            icons::paint(painter, *icon, pos2(x + TILE_ICON_SIZE / 2.0, rect.center().y), IconStyle::filled(TILE_ICON_SIZE), color);
            x += TILE_ICON_SIZE + OVERLAY_GAP;
            let text_rect = widgets::paint_line(painter, x, rect.center().y, galley);
            widgets::text_label(ui, text_rect, id.with(("count", *icon as u8)), text);
            x = text_rect.right() + OVERLAY_GAP;
        }
    }
    // The focus outline of a tile is drawn inside it (`outline-offset: -3px`).
    widgets::focus_ring(ui, &response, rect.shrink(3.0), 0.0, 0.0);
    if response.clicked() {
        env.push(Action::OpenViewer(post.key.clone()));
    }
}

/// Rounds the corners of a square picture by 2 px: the outermost pixel of each corner takes the
/// colour behind the tile.
fn round_corners(ui: &Ui, rect: Rect, background: Color32) {
    let pixel = 1.0 / ui.ctx().pixels_per_point();
    let painter = ui.painter();
    let corners: [Pos2; 4] = [rect.left_top(), pos2(rect.right() - pixel, rect.top()), pos2(rect.left(), rect.bottom() - pixel), pos2(rect.right() - pixel, rect.bottom() - pixel)];
    for corner in corners {
        painter.rect_filled(Rect::from_min_size(corner, Vec2::splat(pixel)), 0.0, background);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_grid_of_the_reference_window_has_tiles_of_309_points() {
        // 1440 wide with the 244 px sidebar: the grid is 935 wide and starts at 374.5, which this
        // app puts on a whole point.
        let grid = GridLayout::new(244.0, 1440.0);
        assert_eq!(grid.width, 935.0);
        assert_eq!(grid.tile, 309.0);
        assert_eq!(grid.left, 375.0);
        assert_eq!(grid.tile_rect(0, 0), Rect::from_min_size(pos2(375.0, 0.0), vec2(309.0, 309.0)));
        assert_eq!(grid.tile_rect(0, 1).left(), 375.0 + 313.0);
        assert_eq!(grid.tile_rect(1, 2), Rect::from_min_size(pos2(375.0 + 626.0, 313.0), vec2(309.0, 309.0)));
        assert_eq!(grid.rows(6), 2);
        assert_eq!(grid.height(2), 622.0);
        assert_eq!(grid.rows(7), 3);
        assert_eq!(grid.rows(0), 0);
        assert_eq!(grid.height(0), 0.0);
    }

    #[test]
    fn a_narrow_window_shrinks_the_tiles_and_keeps_the_gaps() {
        // 640 wide with the 72 px rail: 528 points for three tiles and two gaps.
        let grid = GridLayout::new(72.0, 640.0);
        assert_eq!(grid.width, 528.0);
        assert!((grid.tile - 520.0 / 3.0).abs() < 1e-3);
        let (first, second, third) = (grid.tile_rect(0, 0), grid.tile_rect(0, 1), grid.tile_rect(0, 2));
        // Edges sit on whole points; the gaps stay 4 points (give or take the rounding).
        for rect in [first, second, third] {
            assert_eq!(rect.left(), rect.left().round());
            assert_eq!(rect.right(), rect.right().round());
        }
        assert!((second.left() - first.right() - 4.0).abs() <= 1.0);
        assert!((third.left() - second.right() - 4.0).abs() <= 1.0);
        assert!((third.right() - (grid.left + grid.width)).abs() <= 1.0);
    }

    #[test]
    fn only_the_rows_near_the_view_are_laid_out() {
        let grid = GridLayout::new(244.0, 1440.0);
        let rows = grid.rows(30_000);
        assert_eq!(rows, 10_000);
        // The top of the grid in a view of 800 points: rows 0, 1 and 2.
        assert_eq!(grid.visible_rows(0.0, 800.0, 0.0, rows), 0..3);
        // Far down: 313 points per row.
        let top = 313.0 * 5000.0 + 10.0;
        assert_eq!(grid.visible_rows(top, top + 800.0, 0.0, rows), 5000..5003);
        assert_eq!(grid.visible_rows(top, top + 800.0, 400.0, rows), 4998..5004);
        // The end, and nothing beyond it or for an empty grid.
        let end = grid.height(rows);
        assert_eq!(grid.visible_rows(end - 100.0, end + 500.0, 0.0, rows), 9999..10_000);
        assert_eq!(grid.visible_rows(end + 1000.0, end + 2000.0, 100.0, rows), 0..0);
        assert_eq!(grid.visible_rows(0.0, 800.0, 100.0, 0), 0..0);
    }
}
