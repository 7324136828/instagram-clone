//! Pure layout arithmetic of the feed: the frame of a picture, the cover crop, and which cards
//! of a long list are on screen.
//!
//! Nothing here touches egui's context, so every rule is unit tested. The numbers are the
//! reference web project's (`Media.tsx:9-14`, `styles.css:166-204`).

use std::ops::Range;

use egui::{pos2, Rect, Vec2};

/// The widest frame of a feed picture: width / height (`MAX_RATIO`).
pub const MAX_FEED_RATIO: f32 = 1.91;
/// The tallest frame of a feed picture: width / height (`MIN_RATIO`, 4:5).
pub const MIN_FEED_RATIO: f32 = 4.0 / 5.0;
/// The frame is square until the size of the cover is known.
pub const UNKNOWN_FEED_RATIO: f32 = 1.0;

/// The aspect ratio of the frame of a feed picture: the cover's own ratio, clamped to 4:5 .. 1.91,
/// and square while the size is not known (or is not a usable size).
pub fn feed_frame_ratio(source_size: Option<(u32, u32)>) -> f32 {
    match source_size {
        Some((width, height)) if width > 0 && height > 0 => (width as f32 / height as f32).clamp(MIN_FEED_RATIO, MAX_FEED_RATIO),
        _ => UNKNOWN_FEED_RATIO,
    }
}

/// The height of the feed frame for a frame `width`, rounded to whole points so that everything
/// below a picture sits on the pixel grid.
pub fn feed_frame_height(width: f32, source_size: Option<(u32, u32)>) -> f32 {
    (width / feed_frame_ratio(source_size)).round()
}

/// The texture coordinates that show a picture of `picture` size in a `frame` with the CSS
/// `object-fit: cover`: scaled until both sides cover the frame, centred, the overflow cropped.
pub fn cover_uv(picture: Vec2, frame: Vec2) -> Rect {
    let whole = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    if picture.x <= 0.0 || picture.y <= 0.0 || frame.x <= 0.0 || frame.y <= 0.0 {
        return whole;
    }
    let picture_ratio = picture.x / picture.y;
    let frame_ratio = frame.x / frame.y;
    if picture_ratio > frame_ratio {
        // The picture is wider than the frame: crop left and right.
        let visible = frame_ratio / picture_ratio;
        let margin = (1.0 - visible) / 2.0;
        Rect::from_min_max(pos2(margin, 0.0), pos2(1.0 - margin, 1.0))
    } else {
        // The picture is taller than the frame: crop top and bottom.
        let visible = picture_ratio / frame_ratio;
        let margin = (1.0 - visible) / 2.0;
        Rect::from_min_max(pos2(0.0, margin), pos2(1.0, 1.0 - margin))
    }
}

/// The rectangle a picture of `picture` size takes inside `frame` with `object-fit: contain`:
/// the largest size that fits, scaled up as well as down, centred.
pub fn contain_rect(picture: Vec2, frame: Rect) -> Rect {
    if picture.x <= 0.0 || picture.y <= 0.0 {
        return frame;
    }
    let scale = (frame.width() / picture.x).min(frame.height() / picture.y);
    Rect::from_center_size(frame.center(), picture * scale)
}

/// The shorter side, in pixels, a picture must be decoded to so that it covers a frame of
/// `frame_px` without being scaled up: the argument of `decode::decode_thumbnail`.
pub fn cover_decode_side(source_size: (u32, u32), frame_px: (u32, u32)) -> u32 {
    let (source_width, source_height) = (source_size.0.max(1) as f64, source_size.1.max(1) as f64);
    let (frame_width, frame_height) = (frame_px.0.max(1) as f64, frame_px.1.max(1) as f64);
    let scale = (frame_width / source_width).max(frame_height / source_height);
    let shorter = source_width.min(source_height) * scale;
    (shorter.ceil() as u32).max(1)
}

/// Vertical positions of the rows of a virtualised list: the heights, their running sum, and
/// the gap between two rows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RowTable {
    heights: Vec<f32>,
    /// `tops[i]` is the top of row i; `tops[len]` is the bottom of the last row plus one gap.
    tops: Vec<f32>,
    gap: f32,
}

impl RowTable {
    /// A table of `count` rows of `height` each, `gap` apart.
    pub fn uniform(count: usize, height: f32, gap: f32) -> RowTable {
        let mut table = RowTable { heights: vec![height; count], tops: Vec::new(), gap };
        table.rebuild_from(0);
        table
    }

    /// Number of rows.
    pub fn len(&self) -> usize {
        self.heights.len()
    }

    /// True when there is no row.
    pub fn is_empty(&self) -> bool {
        self.heights.is_empty()
    }

    /// The height of row `index`.
    pub fn height(&self, index: usize) -> f32 {
        self.heights.get(index).copied().unwrap_or(0.0)
    }

    /// The top of row `index`, measured from the top of the first row.
    pub fn top(&self, index: usize) -> f32 {
        self.tops.get(index).copied().unwrap_or_else(|| self.total_height())
    }

    /// The height of all rows with the gaps between them (no gap after the last row).
    pub fn total_height(&self) -> f32 {
        match self.tops.last() {
            Some(end) if !self.heights.is_empty() => end - self.gap,
            _ => 0.0,
        }
    }

    /// Sets the height of row `index`. Returns true when it changed; the rows below move.
    pub fn set_height(&mut self, index: usize, height: f32) -> bool {
        match self.heights.get_mut(index) {
            Some(current) if (*current - height).abs() > 0.01 => {
                *current = height;
                self.rebuild_from(index);
                true
            }
            _ => false,
        }
    }

    /// Recomputes the running sum from row `first` on.
    fn rebuild_from(&mut self, first: usize) {
        self.tops.resize(self.heights.len() + 1, 0.0);
        let mut top = if first == 0 { 0.0 } else { self.tops[first] };
        for index in first..self.heights.len() {
            self.tops[index] = top;
            top += self.heights[index] + self.gap;
        }
        if let Some(end) = self.tops.last_mut() {
            *end = top;
        }
    }

    /// The row that contains the vertical position `y` (or the gap below it); the last row for a
    /// position below everything. None for an empty table.
    pub fn row_at(&self, y: f32) -> Option<usize> {
        if self.heights.is_empty() {
            return None;
        }
        let count = self.heights.len();
        // The first row whose bottom (plus gap) lies below `y`.
        let index = self.tops[1..=count].partition_point(|bottom| *bottom <= y);
        Some(index.min(count - 1))
    }

    /// The rows that intersect the band `top..bottom`, widened by `overscan` points on both
    /// sides. Only these are laid out and painted.
    pub fn visible_rows(&self, top: f32, bottom: f32, overscan: f32) -> Range<usize> {
        if self.heights.is_empty() || bottom < top {
            return 0..0;
        }
        let first = self.row_at(top - overscan).unwrap_or(0);
        let last = self.row_at(bottom + overscan).unwrap_or(0);
        // A band entirely above the first row or below the last one shows nothing.
        if bottom + overscan < 0.0 || top - overscan > self.total_height() {
            return 0..0;
        }
        first..last + 1
    }
}

/// Where the view is, expressed relative to a row, so that rows above changing their height (a
/// picture's size becoming known) do not move what the user is looking at.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScrollAnchor {
    /// The row at the top edge of the view.
    pub row: usize,
    /// How far the top edge of the view is below the top of that row.
    pub offset: f32,
}

impl ScrollAnchor {
    /// The anchor of a view whose top edge is at `scroll_y` in list coordinates. A position in
    /// the gap between two rows is anchored to the row below (with a negative offset): a view
    /// that was scrolled to a row, showing the gap above it, stays on that row.
    pub fn capture(table: &RowTable, scroll_y: f32) -> Option<ScrollAnchor> {
        let row = table.row_at((scroll_y + table.gap).max(0.0))?;
        Some(ScrollAnchor { row, offset: scroll_y - table.top(row) })
    }

    /// The scroll position that shows the same content again after rows changed their height.
    pub fn resolve(&self, table: &RowTable) -> f32 {
        if table.is_empty() {
            return 0.0;
        }
        let row = self.row.min(table.len() - 1);
        table.top(row) + self.offset
    }
}

/// The CSS `ease` timing function, cubic-bezier(.25, .1, .25, 1), for a progress `t` in 0..=1.
pub fn ease(t: f32) -> f32 {
    cubic_bezier(0.25, 0.1, 0.25, 1.0, t)
}

/// The CSS `ease-out` timing function, cubic-bezier(0, 0, .58, 1).
pub fn ease_out(t: f32) -> f32 {
    cubic_bezier(0.0, 0.0, 0.58, 1.0, t)
}

/// Evaluates a CSS cubic-bezier timing function at time fraction `t`.
fn cubic_bezier(x1: f32, y1: f32, x2: f32, y2: f32, t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let curve = |a: f32, b: f32, s: f32| {
        let inverse = 1.0 - s;
        3.0 * inverse * inverse * s * a + 3.0 * inverse * s * s * b + s * s * s
    };
    // Solve x(s) = t by bisection; the curve is monotonic in x.
    let (mut low, mut high) = (0.0_f32, 1.0_f32);
    for _ in 0..24 {
        let middle = (low + high) / 2.0;
        if curve(x1, x2, middle) < t {
            low = middle;
        } else {
            high = middle;
        }
    }
    curve(y1, y2, (low + high) / 2.0)
}

/// The like-heart animation (`heartpop`, `styles.css:179-180`) at time fraction `t` of its 650 ms:
/// the scale and the opacity of the heart. 0 %: scale .2, opacity 0; 35 %: 1.12 and .95; 100 %:
/// scale 1, opacity 0. As in CSS, the `ease` timing applies to each of the two segments.
pub fn heart_burst(t: f32) -> (f32, f32) {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.35 {
        let local = ease(t / 0.35);
        (0.2 + (1.12 - 0.2) * local, 0.95 * local)
    } else {
        let local = ease((t - 0.35) / 0.65);
        (1.12 + (1.0 - 1.12) * local, 0.95 * (1.0 - local))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::vec2;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn the_feed_frame_is_clamped_between_four_to_five_and_one_point_nine_one() {
        assert_eq!(feed_frame_ratio(None), 1.0);
        assert_eq!(feed_frame_ratio(Some((0, 10))), 1.0);
        assert_eq!(feed_frame_ratio(Some((720, 720))), 1.0);
        assert_eq!(feed_frame_ratio(Some((720, 900))), 0.8);
        assert_eq!(feed_frame_ratio(Some((720, 1280))), 0.8);
        assert!(close(feed_frame_ratio(Some((1200, 800))), 1.5));
        assert_eq!(feed_frame_ratio(Some((4000, 1000))), 1.91);
    }

    #[test]
    fn the_feed_frame_height_follows_the_web_project() {
        // 468 px wide inside the card: square 468, tallest 585, widest 245.
        assert_eq!(feed_frame_height(468.0, Some((720, 720))), 468.0);
        assert_eq!(feed_frame_height(468.0, Some((1080, 1920))), 585.0);
        assert_eq!(feed_frame_height(468.0, Some((3000, 1000))), 245.0);
        assert_eq!(feed_frame_height(468.0, None), 468.0);
    }

    #[test]
    fn cover_crops_the_longer_direction_around_the_centre() {
        // Same shape: the whole picture.
        let uv = cover_uv(vec2(100.0, 100.0), vec2(50.0, 50.0));
        assert!(close(uv.min.x, 0.0) && close(uv.max.x, 1.0) && close(uv.min.y, 0.0) && close(uv.max.y, 1.0));
        // A 2:1 picture in a square frame keeps the middle half of its width.
        let uv = cover_uv(vec2(200.0, 100.0), vec2(50.0, 50.0));
        assert!(close(uv.min.x, 0.25) && close(uv.max.x, 0.75) && close(uv.min.y, 0.0) && close(uv.max.y, 1.0));
        // A 9:16 picture in a 4:5 frame is cropped top and bottom.
        let uv = cover_uv(vec2(900.0, 1600.0), vec2(468.0, 585.0));
        let visible = (900.0 / 1600.0) / (468.0 / 585.0);
        assert!(close(uv.min.x, 0.0) && close(uv.max.x, 1.0));
        assert!(close(uv.height(), visible) && close(uv.center().y, 0.5));
        // Degenerate sizes do not divide by zero.
        assert_eq!(cover_uv(vec2(0.0, 0.0), vec2(10.0, 10.0)), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)));
    }

    #[test]
    fn contain_fits_the_whole_picture_and_scales_up() {
        let frame = Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 200.0));
        let rect = contain_rect(vec2(100.0, 100.0), frame);
        assert!(close(rect.width(), 200.0) && close(rect.height(), 200.0) && close(rect.center().x, 200.0));
        let rect = contain_rect(vec2(1000.0, 100.0), frame);
        assert!(close(rect.width(), 400.0) && close(rect.height(), 40.0) && close(rect.center().y, 100.0));
    }

    #[test]
    fn the_decode_side_covers_the_frame() {
        // A square picture for a square frame: the frame side.
        assert_eq!(cover_decode_side((720, 720), (468, 468)), 468);
        // A portrait picture taller than the frame: its width must reach the frame width.
        assert_eq!(cover_decode_side((1000, 3000), (468, 585)), 468);
        // A panorama: its height must reach the frame height, which is its shorter side.
        assert_eq!(cover_decode_side((4000, 1000), (468, 245)), 245);
        // A landscape picture inside the clamp: the height is the shorter side.
        assert_eq!(cover_decode_side((1200, 800), (468, 312)), 312);
    }

    #[test]
    fn rows_are_stacked_with_gaps() {
        let mut table = RowTable::uniform(3, 100.0, 16.0);
        assert_eq!(table.len(), 3);
        assert_eq!(table.top(0), 0.0);
        assert_eq!(table.top(1), 116.0);
        assert_eq!(table.top(2), 232.0);
        assert_eq!(table.total_height(), 332.0);
        assert!(table.set_height(0, 50.0));
        assert!(!table.set_height(0, 50.0));
        assert_eq!(table.top(1), 66.0);
        assert_eq!(table.total_height(), 282.0);
        assert_eq!(RowTable::uniform(0, 100.0, 16.0).total_height(), 0.0);
    }

    #[test]
    fn only_rows_near_the_view_are_visible() {
        let table = RowTable::uniform(5000, 600.0, 16.0);
        // The top of the list.
        assert_eq!(table.visible_rows(0.0, 900.0, 0.0), 0..2);
        // Somewhere in the middle: rows of 616 points each.
        let top = 616.0 * 2500.0 + 10.0;
        assert_eq!(table.visible_rows(top, top + 900.0, 0.0), 2500..2502);
        // Overscan adds the neighbours.
        assert_eq!(table.visible_rows(top, top + 900.0, 700.0), 2498..2503);
        // The very end.
        let end = table.total_height();
        assert_eq!(table.visible_rows(end - 900.0, end, 0.0), 4998..5000);
        // Nothing for an empty table or a band outside the list.
        assert_eq!(RowTable::default().visible_rows(0.0, 900.0, 100.0), 0..0);
        assert_eq!(table.visible_rows(end + 5000.0, end + 6000.0, 100.0), 0..0);
    }

    #[test]
    fn a_position_in_a_gap_belongs_to_the_row_above() {
        let table = RowTable::uniform(3, 100.0, 16.0);
        assert_eq!(table.row_at(-5.0), Some(0));
        assert_eq!(table.row_at(99.0), Some(0));
        assert_eq!(table.row_at(110.0), Some(0));
        assert_eq!(table.row_at(116.0), Some(1));
        assert_eq!(table.row_at(10_000.0), Some(2));
        assert_eq!(RowTable::default().row_at(0.0), None);
    }

    #[test]
    fn the_anchor_keeps_the_view_when_rows_above_grow() {
        let mut table = RowTable::uniform(10, 500.0, 16.0);
        let scroll = table.top(6) + 120.0;
        let anchor = ScrollAnchor::capture(&table, scroll).expect("anchor");
        assert_eq!(anchor.row, 6);
        assert!(close(anchor.offset, 120.0));
        // Two cards above learn their real height.
        table.set_height(1, 620.0);
        table.set_height(3, 300.0);
        let resolved = anchor.resolve(&table);
        assert!(close(resolved, scroll + 120.0 - 200.0));
        assert!(close(resolved - table.top(6), 120.0));
    }

    #[test]
    fn a_view_in_the_gap_above_a_row_is_anchored_to_that_row() {
        let mut table = RowTable::uniform(10, 500.0, 16.0);
        // Scrolled to row 4 with the gap above it in view.
        let scroll = table.top(4) - 16.0;
        let anchor = ScrollAnchor::capture(&table, scroll).expect("anchor");
        assert_eq!(anchor.row, 4);
        assert!(close(anchor.offset, -16.0));
        // The row above learns its height: row 4 stays where it is on screen.
        table.set_height(3, 800.0);
        assert!(close(anchor.resolve(&table), table.top(4) - 16.0));
        // The very top of the list.
        let top = ScrollAnchor::capture(&table, 0.0).expect("anchor");
        assert_eq!((top.row, top.offset), (0, 0.0));
    }

    #[test]
    fn timing_functions_start_at_zero_and_end_at_one() {
        for function in [ease as fn(f32) -> f32, ease_out] {
            assert!(function(0.0).abs() < 1e-3);
            assert!((function(1.0) - 1.0).abs() < 1e-3);
            assert!(function(0.5) > 0.5);
        }
        // The published value of `ease` at half time is about 0.80.
        assert!((ease(0.5) - 0.8024).abs() < 0.01);
    }

    #[test]
    fn the_heart_pops_and_fades() {
        let (scale, opacity) = heart_burst(0.0);
        assert!(close(scale, 0.2) && close(opacity, 0.0));
        let (scale, opacity) = heart_burst(1.0);
        assert!(close(scale, 1.0) && close(opacity, 0.0));
        let peak = (0..=100).map(|step| heart_burst(step as f32 / 100.0).1).fold(0.0_f32, f32::max);
        assert!(peak > 0.9 && peak <= 0.95);
    }
}
