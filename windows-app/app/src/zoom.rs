//! Zooming and panning a picture in the viewer: pure arithmetic, no egui context.
//!
//! The reference web project only fits a picture into its box (`object-fit: contain`); zoom is
//! what this app adds as an image viewer. The picture is either fitted (the default, and what
//! every new picture starts with) or shown at a scale, which is measured in screen pixels per
//! picture pixel so that 1.0 is "100 %" on every display. While it is larger than the stage it
//! can be moved, but never so far that an edge of the picture leaves the edge of the stage.

use egui::{Rect, Vec2};

/// The largest scale: 800 %. A small picture whose fitted size is already larger may stay fitted.
pub const MAX_SCALE: f32 = 8.0;
/// One press of `+` or `-` changes the scale by this factor.
pub const KEY_STEP: f32 = 1.25;
/// One notch of the mouse wheel changes the scale by this factor.
pub const WHEEL_NOTCH_STEP: f32 = 1.2;
/// A double click on a picture whose fitted size is 100 % zooms to this scale instead.
pub const DOUBLE_CLICK_FALLBACK_SCALE: f32 = 2.0;
/// Two scales closer than this fraction count as the same (the fitted scale is not a round number).
const SAME_SCALE_TOLERANCE: f32 = 0.001;

/// The stage a picture is shown on.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Stage {
    /// Size of the stage in points.
    pub size: Vec2,
    /// Oriented size of the picture file in pixels: its 100 % size.
    pub source: Vec2,
    /// Screen pixels per point.
    pub pixels_per_point: f32,
}

impl Stage {
    /// The scale at which the whole picture just fits the stage (`object-fit: contain`: scaled
    /// down or up).
    pub fn fit_scale(&self) -> f32 {
        if self.source.x <= 0.0 || self.source.y <= 0.0 || self.size.x <= 0.0 || self.size.y <= 0.0 {
            return 1.0;
        }
        let stage_px = self.size * self.pixels_per_point;
        (stage_px.x / self.source.x).min(stage_px.y / self.source.y)
    }

    /// The smallest scale: the fitted one, or 100 % for a picture so small that fitting
    /// enlarges it.
    pub fn min_scale(&self) -> f32 {
        self.fit_scale().min(1.0)
    }

    /// The largest scale: 800 %, or the fitted one when that is larger still.
    pub fn max_scale(&self) -> f32 {
        self.fit_scale().max(MAX_SCALE)
    }

    /// The size in points of the picture at `scale`.
    pub fn picture_size(&self, scale: f32) -> Vec2 {
        self.source * scale / self.pixels_per_point.max(f32::EPSILON)
    }

    /// True when `scale` is the fitted scale.
    fn is_fit_scale(&self, scale: f32) -> bool {
        let fit = self.fit_scale();
        (scale - fit).abs() <= fit * SAME_SCALE_TOLERANCE
    }
}

/// How the picture in the viewer is zoomed and moved.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Zoom {
    /// None while the picture is fitted to the stage; otherwise screen pixels per picture pixel.
    pub scale: Option<f32>,
    /// How far the centre of the picture is from the centre of the stage, in points.
    pub pan: Vec2,
}

impl Default for Zoom {
    fn default() -> Self {
        Zoom::FIT
    }
}

impl Zoom {
    /// The fitted picture, centred.
    pub const FIT: Zoom = Zoom { scale: None, pan: Vec2::ZERO };

    /// True while the picture is fitted to the stage.
    pub fn is_fit(&self) -> bool {
        self.scale.is_none()
    }

    /// The scale in effect on `stage`.
    pub fn scale_on(&self, stage: &Stage) -> f32 {
        self.scale.unwrap_or_else(|| stage.fit_scale())
    }

    /// The scale as a whole percentage, for the indicator: 150 for 1.5.
    pub fn percent(&self, stage: &Stage) -> u32 {
        (self.scale_on(stage) * 100.0).round().max(1.0) as u32
    }

    /// True when the picture is larger than the stage in some direction and can be moved.
    pub fn can_pan(&self, stage: &Stage) -> bool {
        let picture = stage.picture_size(self.scale_on(stage));
        picture.x > stage.size.x + 0.5 || picture.y > stage.size.y + 0.5
    }

    /// The rectangle the picture takes on a stage at `stage_rect`.
    pub fn picture_rect(&self, stage_rect: Rect, stage: &Stage) -> Rect {
        Rect::from_center_size(stage_rect.center() + self.pan, stage.picture_size(self.scale_on(stage)))
    }

    /// The same zoom made valid for `stage`: the scale inside its limits (a scale that equals the
    /// fitted one becomes "fitted"), and the picture moved back so that no gap opens between an
    /// edge of the picture and the edge of the stage. Call it whenever the stage changes.
    pub fn clamped(&self, stage: &Stage) -> Zoom {
        let Some(scale) = self.scale else {
            return Zoom::FIT;
        };
        let scale = scale.clamp(stage.min_scale(), stage.max_scale());
        if stage.is_fit_scale(scale) {
            return Zoom::FIT;
        }
        let picture = stage.picture_size(scale);
        let limit = ((picture - stage.size) / 2.0).max(Vec2::ZERO);
        Zoom { scale: Some(scale), pan: self.pan.clamp(-limit, limit) }
    }

    /// Zooms to `scale` so that the point of the picture under `anchor` stays under it. `anchor`
    /// is a position relative to the centre of the stage, in points (the pointer, or zero for
    /// the keys).
    pub fn zoomed_about(&self, stage: &Stage, scale: f32, anchor: Vec2) -> Zoom {
        let old_scale = self.scale_on(stage);
        let new_scale = scale.clamp(stage.min_scale(), stage.max_scale());
        if old_scale <= 0.0 {
            return Zoom::FIT;
        }
        // The picture point under the anchor, as an offset from the picture's centre in points
        // of the old scale; it is the same point at the new scale, `ratio` times as far away.
        let ratio = new_scale / old_scale;
        let pan = anchor - (anchor - self.pan) * ratio;
        Zoom { scale: Some(new_scale), pan }.clamped(stage)
    }

    /// Zooms by `factor` around `anchor` (the mouse wheel and pinching).
    pub fn zoomed_by(&self, stage: &Stage, factor: f32, anchor: Vec2) -> Zoom {
        if !factor.is_finite() || factor <= 0.0 {
            return *self;
        }
        self.zoomed_about(stage, self.scale_on(stage) * factor, anchor)
    }

    /// One press of `+` (`steps` = 1) or `-` (`steps` = -1): a step of 25 % around the centre of
    /// the stage. A step that would pass 100 % stops there, so the exact size is easy to reach.
    pub fn stepped(&self, stage: &Stage, steps: i32) -> Zoom {
        let old_scale = self.scale_on(stage);
        let mut scale = old_scale * KEY_STEP.powi(steps);
        let crosses_full_size = (old_scale < 1.0 && scale > 1.0) || (old_scale > 1.0 && scale < 1.0);
        if crosses_full_size {
            scale = 1.0;
        }
        self.zoomed_about(stage, scale, Vec2::ZERO)
    }

    /// Moves the picture by `delta` points (dragging), as far as its edges allow.
    pub fn panned(&self, stage: &Stage, delta: Vec2) -> Zoom {
        Zoom { scale: self.scale, pan: self.pan + delta }.clamped(stage)
    }

    /// A double click at `anchor`: from the fitted picture to 100 % around the click, and back
    /// from anything else. A picture that is fitted at 100 % already goes to 200 %.
    pub fn toggled(&self, stage: &Stage, anchor: Vec2) -> Zoom {
        if !self.is_fit() {
            return Zoom::FIT;
        }
        let target = if stage.is_fit_scale(1.0) { DOUBLE_CLICK_FALLBACK_SCALE } else { 1.0 };
        self.zoomed_about(stage, target, anchor)
    }
}

/// The longer side, in pixels, the picture has to be decoded to for `scale`: never more than the
/// file has (pictures are not enlarged by the decoder) and at least one pixel.
pub fn decode_side(source: (u32, u32), scale: f32) -> u32 {
    let longer = source.0.max(source.1).max(1);
    let wanted = (longer as f32 * scale.min(1.0)).ceil().max(1.0) as u32;
    wanted.min(longer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{pos2, vec2};

    /// A 4000 x 3000 picture on a 800 x 600 stage: fitted at 20 %.
    fn large() -> Stage {
        Stage { size: vec2(800.0, 600.0), source: vec2(4000.0, 3000.0), pixels_per_point: 1.0 }
    }

    /// A 200 x 100 picture on the same stage: fitted at 400 %.
    fn small() -> Stage {
        Stage { size: vec2(800.0, 600.0), source: vec2(200.0, 100.0), pixels_per_point: 1.0 }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    /// The picture point (in picture pixels from the picture's centre) under `anchor`.
    fn point_under(zoom: &Zoom, stage: &Stage, anchor: Vec2) -> Vec2 {
        (anchor - zoom.pan) * stage.pixels_per_point / zoom.scale_on(stage)
    }

    #[test]
    fn the_fitted_scale_contains_the_picture_and_scales_small_ones_up() {
        assert!(close(large().fit_scale(), 0.2));
        assert!(close(small().fit_scale(), 4.0));
        // A portrait picture is limited by the height of the stage.
        let portrait = Stage { size: vec2(800.0, 600.0), source: vec2(1000.0, 3000.0), pixels_per_point: 1.0 };
        assert!(close(portrait.fit_scale(), 0.2));
        // On a display with 1.5 pixels per point the same stage has more pixels.
        let dense = Stage { pixels_per_point: 1.5, ..large() };
        assert!(close(dense.fit_scale(), 0.3));
        let rect = Zoom::FIT.picture_rect(Rect::from_min_size(pos2(100.0, 50.0), dense.size), &dense);
        assert!(close(rect.left(), 100.0) && close(rect.top(), 50.0) && close(rect.width(), 800.0) && close(rect.height(), 600.0), "the fitted picture fills the stage: {rect:?}");
        // A size that is not known yet never divides by zero.
        let unknown = Stage { source: Vec2::ZERO, ..large() };
        assert_eq!(unknown.fit_scale(), 1.0);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_cursor() {
        let stage = large();
        let anchor = vec2(150.0, -90.0);
        let before = point_under(&Zoom::FIT, &stage, anchor);
        let zoomed = Zoom::FIT.zoomed_about(&stage, 1.0, anchor);
        assert_eq!(zoomed.scale, Some(1.0));
        let after = point_under(&zoomed, &stage, anchor);
        assert!(close(before.x, after.x) && close(before.y, after.y), "{before:?} became {after:?}");

        // Again from the zoomed state, further in and around another point.
        let second_anchor = vec2(-200.0, 120.0);
        let before = point_under(&zoomed, &stage, second_anchor);
        let further = zoomed.zoomed_by(&stage, 1.7, second_anchor);
        assert!(close(further.scale_on(&stage), 1.7));
        let after = point_under(&further, &stage, second_anchor);
        assert!(close(before.x, after.x) && close(before.y, after.y), "{before:?} became {after:?}");
    }

    #[test]
    fn the_scale_stays_between_fitted_and_eight_hundred_percent() {
        let stage = large();
        assert_eq!(Zoom::FIT.zoomed_about(&stage, 50.0, Vec2::ZERO).scale, Some(MAX_SCALE));
        // Zooming out below the fitted size ends fitted and centred.
        let zoomed = Zoom::FIT.zoomed_about(&stage, 2.0, vec2(300.0, 200.0));
        assert_eq!(zoomed.zoomed_about(&stage, 0.01, vec2(300.0, 200.0)), Zoom::FIT);
        assert_eq!(zoomed.zoomed_by(&stage, 0.0, Vec2::ZERO), zoomed, "a factor of zero changes nothing");
        assert_eq!(zoomed.zoomed_by(&stage, f32::NAN, Vec2::ZERO), zoomed);

        // A small picture is fitted at 400 %: it can go down to 100 % and up to 800 %.
        let stage = small();
        assert_eq!(stage.min_scale(), 1.0);
        assert_eq!(Zoom::FIT.zoomed_about(&stage, 0.1, Vec2::ZERO).scale, Some(1.0));
        assert_eq!(Zoom::FIT.zoomed_about(&stage, 100.0, Vec2::ZERO).scale, Some(8.0));
        assert_eq!(Zoom::FIT.zoomed_about(&stage, 4.0, Vec2::ZERO), Zoom::FIT);
        // A tiny picture whose fitted size is beyond 800 % keeps it as its largest scale.
        let tiny = Stage { source: vec2(20.0, 20.0), ..small() };
        assert_eq!(tiny.max_scale(), 30.0);
    }

    #[test]
    fn the_picture_cannot_be_moved_past_its_edges() {
        let stage = large();
        // At 100 % the picture is 4000 x 3000 points on a 800 x 600 stage.
        let zoomed = Zoom::FIT.zoomed_about(&stage, 1.0, Vec2::ZERO);
        assert!(zoomed.can_pan(&stage));
        let moved = zoomed.panned(&stage, vec2(100_000.0, -100_000.0));
        assert_eq!(moved.pan, vec2(1600.0, -1200.0));
        let rect = moved.picture_rect(Rect::from_min_size(pos2(0.0, 0.0), stage.size), &stage);
        assert!(close(rect.left(), 0.0) && close(rect.bottom(), 600.0), "the left and the bottom edges meet the stage: {rect:?}");
        // A small move is taken as it is.
        assert_eq!(zoomed.panned(&stage, vec2(10.0, 5.0)).pan, vec2(10.0, 5.0));

        // The fitted picture cannot be moved at all.
        assert!(!Zoom::FIT.can_pan(&stage));
        assert_eq!(Zoom::FIT.panned(&stage, vec2(50.0, 50.0)), Zoom::FIT);

        // A picture wider than the stage but not as tall moves sideways only.
        let panorama = Stage { size: vec2(800.0, 600.0), source: vec2(4000.0, 400.0), pixels_per_point: 1.0 };
        let zoomed = Zoom::FIT.zoomed_about(&panorama, 1.0, Vec2::ZERO).panned(&panorama, vec2(-5000.0, 300.0));
        assert_eq!(zoomed.pan, vec2(-1600.0, 0.0));
    }

    #[test]
    fn a_smaller_stage_pulls_the_picture_back_inside() {
        let stage = large();
        let zoomed = Zoom::FIT.zoomed_about(&stage, 0.5, Vec2::ZERO).panned(&stage, vec2(600.0, 450.0));
        assert_eq!(zoomed.pan, vec2(600.0, 450.0));
        // The window grows: the picture is 2000 x 1500 on a 1800 x 1400 stage now.
        let larger = Stage { size: vec2(1800.0, 1400.0), ..stage };
        assert_eq!(zoomed.clamped(&larger).pan, vec2(100.0, 50.0));
        // It grows further: 50 % is below the fitted scale of the new stage.
        let huge = Stage { size: vec2(4000.0, 3000.0), ..stage };
        assert_eq!(zoomed.clamped(&huge), Zoom::FIT);
    }

    #[test]
    fn a_double_click_toggles_between_fitted_and_full_size() {
        let stage = large();
        let anchor = vec2(40.0, 30.0);
        let zoomed = Zoom::FIT.toggled(&stage, anchor);
        assert_eq!(zoomed.scale, Some(1.0));
        let before = point_under(&Zoom::FIT, &stage, anchor);
        let after = point_under(&zoomed, &stage, anchor);
        assert!(close(before.x, after.x) && close(before.y, after.y));
        assert_eq!(zoomed.toggled(&stage, anchor), Zoom::FIT);
        // From any other scale the double click goes back to the fitted picture.
        assert_eq!(Zoom::FIT.zoomed_about(&stage, 3.0, anchor).toggled(&stage, Vec2::ZERO), Zoom::FIT);

        // A picture that fits at exactly 100 % has nothing to toggle to: it goes to 200 %.
        let exact = Stage { size: vec2(800.0, 600.0), source: vec2(800.0, 600.0), pixels_per_point: 1.0 };
        assert_eq!(Zoom::FIT.toggled(&exact, Vec2::ZERO).scale, Some(2.0));
        // A small picture goes down to its real size.
        assert_eq!(Zoom::FIT.toggled(&small(), Vec2::ZERO).scale, Some(1.0));
    }

    #[test]
    fn the_keys_step_by_a_quarter_and_stop_at_full_size() {
        let stage = large();
        let once = Zoom::FIT.stepped(&stage, 1);
        assert!(close(once.scale_on(&stage), 0.25));
        assert_eq!(once.stepped(&stage, -1), Zoom::FIT);
        // 0.2 x 1.25^7 = 0.95; the next step would pass 100 % and stops there.
        let mut zoom = Zoom::FIT;
        for _ in 0..7 {
            zoom = zoom.stepped(&stage, 1);
        }
        assert!(zoom.scale_on(&stage) < 1.0);
        zoom = zoom.stepped(&stage, 1);
        assert_eq!(zoom.scale, Some(1.0));
        assert!(close(zoom.stepped(&stage, 1).scale_on(&stage), 1.25));
        assert_eq!(zoom.stepped(&stage, 1).stepped(&stage, -1).scale, Some(1.0));
        assert_eq!(zoom.percent(&stage), 100);
        assert_eq!(Zoom::FIT.percent(&stage), 20);
    }

    #[test]
    fn the_decoder_is_asked_for_no_more_than_the_file_has() {
        assert_eq!(decode_side((4000, 3000), 0.2), 800);
        assert_eq!(decode_side((3000, 4000), 0.2), 800);
        assert_eq!(decode_side((4000, 3000), 1.0), 4000);
        assert_eq!(decode_side((4000, 3000), 3.5), 4000);
        assert_eq!(decode_side((200, 100), 4.0), 200);
        assert_eq!(decode_side((0, 0), 1.0), 1);
    }
}
