//! Where and how large the window opens, and what of that is kept for the next start.
//!
//! eframe keeps the window's geometry in window.ron (the key `"window"`, a RON text of
//! `egui_winit::WindowSettings`) and puts the window back there at the next start. Three things
//! it does not guard against, and that this module does:
//!
//! * A window that is closed while it is minimised reports the position -32000, -32000 and a
//!   size of 0 x 0. Stored as it is, every later start would show a tiny window. Such a
//!   geometry is never stored (`geometry_to_keep`) and never restored (`plan_start`).
//! * A window that is closed in fullscreen would come back borderless, without a title bar or a
//!   close button. Fullscreen is a state of one session, so it is not kept.
//! * The first window (no window.ron yet), and a restored one, may be larger than the work area
//!   of the screen (a laptop at 150 % has 768 points of height, less the taskbar): the bottom of
//!   the sidebar with "More", and the toasts, would be off the screen. The size is cut to the
//!   work area, a first window is centred in it, and a screen too small even for the smallest
//!   window starts the window maximised.
//!
//! The decisions are pure functions of the stored text and the work area, so they are tested
//! without a window; `platform::work_area` asks Windows for the work area.

use std::collections::HashMap;
use std::path::Path;

use serde::{Deserialize, Serialize};

/// The key of the geometry in eframe's storage (`epi_integration::STORAGE_WINDOW_KEY`).
pub const STORAGE_WINDOW_KEY: &str = "window";

/// A minimised window is moved to this position by Windows; anything at or beyond it is not a
/// place on a screen.
const MINIMISED_POSITION_PX: f32 = -30_000.0;

/// A size may come back a fraction of a point below the minimum through rounding.
const SIZE_TOLERANCE_POINTS: f32 = 1.0;

/// A point or a size as egui stores it (`emath::Pos2`, `emath::Vec2`).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Xy {
    x: f32,
    y: f32,
}

/// `egui_winit::WindowGeometry`, field by field, so that its RON text can be read and written.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
struct Geometry {
    outer_position_pixels: Option<Xy>,
    scale_factor: f32,
    inner_size_points: Xy,
}

/// `egui_winit::WindowSettings`, field by field.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct SavedWindow {
    normal_geometry: Geometry,
    fullscreen: bool,
    maximized: bool,
}

impl SavedWindow {
    /// Reads the RON text eframe stores under `STORAGE_WINDOW_KEY`.
    pub fn parse(text: &str) -> Option<SavedWindow> {
        ron::from_str(text).ok()
    }

    /// The RON text in the form eframe reads.
    pub fn to_text(self) -> Option<String> {
        ron::to_string(&self).ok()
    }

    /// True when the geometry describes a window on a screen that is at least `min_size`
    /// points large: not the geometry of a minimised window, not a size of nothing.
    pub fn is_usable(&self, min_size: [f32; 2]) -> bool {
        let geometry = &self.normal_geometry;
        let size = geometry.inner_size_points;
        let size_ok = size.x.is_finite()
            && size.y.is_finite()
            && size.x >= min_size[0] - SIZE_TOLERANCE_POINTS
            && size.y >= min_size[1] - SIZE_TOLERANCE_POINTS;
        let position_ok = geometry.outer_position_pixels.is_none_or(|position| {
            position.x.is_finite() && position.y.is_finite() && position.x > MINIMISED_POSITION_PX && position.y > MINIMISED_POSITION_PX
        });
        size_ok && position_ok && geometry.scale_factor.is_finite() && geometry.scale_factor > 0.0
    }

    /// The inner size in points.
    pub fn inner_size(&self) -> [f32; 2] {
        [self.normal_geometry.inner_size_points.x, self.normal_geometry.inner_size_points.y]
    }

    /// The middle of the stored window in physical pixels: the screen it was on.
    pub fn position_px(&self) -> Option<(i32, i32)> {
        let geometry = &self.normal_geometry;
        let position = geometry.outer_position_pixels?;
        let half = |side: f32| side * geometry.scale_factor / 2.0;
        Some(((position.x + half(geometry.inner_size_points.x)) as i32, (position.y + half(geometry.inner_size_points.y)) as i32))
    }
}

/// What eframe should store when it saves the window: the geometry it has just taken, without
/// fullscreen; or, when that geometry is unusable (the window is minimised), the last usable one.
/// `None` when there is nothing usable at all: the next start then opens like the first.
pub fn geometry_to_keep(taken: Option<&str>, last_usable: Option<&str>, min_size: [f32; 2]) -> Option<String> {
    match taken.and_then(SavedWindow::parse) {
        Some(mut saved) if saved.is_usable(min_size) => {
            saved.fullscreen = false;
            saved.to_text()
        }
        _ => last_usable.map(str::to_owned),
    }
}

/// The work area of a screen (the screen without the taskbar), in physical pixels, with what a
/// window needs around its inner area there.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct WorkArea {
    /// Left, top, right and bottom edge in pixels.
    pub rect_px: [f32; 4],
    /// Pixels per point of that screen (1.5 at 150 %).
    pub scale: f32,
    /// The frame of a window there (borders and title bar): left, top, right, bottom in pixels.
    pub frame_px: [f32; 4],
}

impl WorkArea {
    /// The largest inner size in points that fits into the work area with its frame.
    fn room(&self) -> [f32; 2] {
        let [left, top, right, bottom] = self.rect_px;
        let [frame_left, frame_top, frame_right, frame_bottom] = self.frame_px;
        [
            ((right - left) - frame_left - frame_right).max(0.0) / self.scale,
            ((bottom - top) - frame_top - frame_bottom).max(0.0) / self.scale,
        ]
    }
}

/// How the window starts.
#[derive(Debug, Clone, PartialEq)]
pub struct StartPlan {
    /// The geometry to leave in window.ron for eframe to restore, or `None` to start without one
    /// (the file is removed).
    pub restored: Option<SavedWindow>,
    /// The inner size in points for a start without a stored geometry.
    pub size: [f32; 2],
    /// The outer position in points for a start without a stored geometry; `None` lets Windows
    /// choose.
    pub position: Option<[f32; 2]>,
    /// The screen is too small even for the smallest window: start maximised.
    pub maximized: bool,
}

/// Decides how the window starts from what window.ron holds (`stored`, the RON text of the
/// geometry) and the work area of the screen it opens on (`None` when it is not known).
///
/// * A usable stored geometry is restored without fullscreen, its size cut to the work area.
/// * Without one, the window is `default_size`, cut to the work area, and centred in it.
/// * Where not even `min_size` fits, the window starts maximised.
pub fn plan_start(stored: Option<&str>, work_area: Option<WorkArea>, default_size: [f32; 2], min_size: [f32; 2]) -> StartPlan {
    let room = work_area.map(|area| area.room());
    let fit = |size: [f32; 2]| match room {
        Some(room) => [size[0].min(room[0]).max(min_size[0]), size[1].min(room[1]).max(min_size[1])],
        None => size,
    };
    let too_small = room.is_some_and(|room| room[0] < min_size[0] || room[1] < min_size[1]);

    if let Some(mut saved) = stored.and_then(SavedWindow::parse).filter(|saved| saved.is_usable(min_size)) {
        saved.fullscreen = false;
        let [width, height] = fit(saved.inner_size());
        saved.normal_geometry.inner_size_points = Xy { x: width, y: height };
        saved.maximized |= too_small;
        return StartPlan { restored: Some(saved), size: [width, height], position: None, maximized: saved.maximized };
    }

    let size = fit(default_size);
    let position = work_area.map(|area| {
        let [left, top, right, bottom] = area.rect_px;
        let [frame_left, frame_top, frame_right, frame_bottom] = area.frame_px;
        let outer_width = size[0] * area.scale + frame_left + frame_right;
        let outer_height = size[1] * area.scale + frame_top + frame_bottom;
        let x = left + ((right - left) - outer_width).max(0.0) / 2.0;
        let y = top + ((bottom - top) - outer_height).max(0.0) / 2.0;
        [x / area.scale, y / area.scale]
    });
    StartPlan { restored: None, size, position, maximized: too_small }
}

/// The geometry text window.ron holds, if any. eframe keeps its storage there as a RON map
/// from keys to RON texts.
pub fn read_stored(window_file: &Path) -> Option<String> {
    let text = std::fs::read_to_string(window_file).ok()?;
    let mut map: HashMap<String, String> = ron::from_str(&text).ok()?;
    map.remove(STORAGE_WINDOW_KEY)
}

/// Leaves window.ron as `plan` needs it before eframe reads it: the planned geometry when it
/// differs from what is stored, no file at all when nothing is to be restored. A file that
/// cannot be written only costs the clean start, so the failure is logged and not reported.
pub fn apply_to_file(window_file: &Path, stored: Option<&str>, plan: &StartPlan) {
    let planned = plan.restored.and_then(|saved| saved.to_text());
    if planned.as_deref() == stored {
        return;
    }
    let result = match planned {
        Some(text) => {
            let map: HashMap<String, String> = [(STORAGE_WINDOW_KEY.to_owned(), text)].into();
            match ron::to_string(&map) {
                Ok(file_text) => std::fs::write(window_file, file_text),
                Err(error) => Err(std::io::Error::other(error)),
            }
        }
        None if window_file.exists() => std::fs::remove_file(window_file),
        None => Ok(()),
    };
    match result {
        Ok(()) => log::info!("The stored window geometry was corrected: {}", window_file.display()),
        Err(error) => log::warn!("{} could not be corrected: {error}", window_file.display()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: [f32; 2] = [640.0, 480.0];
    const DEFAULT: [f32; 2] = [1280.0, 860.0];

    /// What eframe 0.37 writes for a window of 1400 x 950 points at 150 %.
    const NORMAL: &str = "(normal_geometry:(outer_position_pixels:Some((x:333.0,y:77.0)),scale_factor:1.5,inner_size_points:(x:1400.0,y:950.0)),fullscreen:false,maximized:false)";
    /// What it writes for a window that was closed while minimised.
    const MINIMISED: &str = "(normal_geometry:(outer_position_pixels:Some((x:-32000.0,y:-32000.0)),scale_factor:1.5,inner_size_points:(x:0.0,y:0.0)),fullscreen:false,maximized:false)";
    /// ... and for one closed in fullscreen.
    const FULLSCREEN: &str = "(normal_geometry:(outer_position_pixels:Some((x:333.0,y:77.0)),scale_factor:1.5,inner_size_points:(x:1400.0,y:950.0)),fullscreen:true,maximized:false)";

    /// The screen of the review: 2048 x 1152 pixels at 150 %, a 72-pixel taskbar, and the frame
    /// of a window there.
    fn laptop() -> WorkArea {
        WorkArea { rect_px: [0.0, 0.0, 2048.0, 1080.0], scale: 1.5, frame_px: [11.0, 45.0, 11.0, 11.0] }
    }

    #[test]
    fn eframe_text_is_read_and_written_unchanged() {
        let saved = SavedWindow::parse(NORMAL).expect("eframe's text");
        assert_eq!(saved.to_text().as_deref(), Some(NORMAL));
        assert!(saved.is_usable(MIN));
        assert_eq!(saved.inner_size(), [1400.0, 950.0]);
    }

    #[test]
    fn a_minimised_window_never_replaces_the_last_usable_geometry() {
        assert!(!SavedWindow::parse(MINIMISED).expect("parses").is_usable(MIN));
        assert_eq!(geometry_to_keep(Some(MINIMISED), Some(NORMAL), MIN).as_deref(), Some(NORMAL));
        // Without an earlier one there is nothing to keep: the next start is a first start.
        assert_eq!(geometry_to_keep(Some(MINIMISED), None, MIN), None);
        // A usable geometry is kept as taken.
        assert_eq!(geometry_to_keep(Some(NORMAL), None, MIN).as_deref(), Some(NORMAL));
        // Smaller than the smallest window (a damaged file): not kept either.
        let tiny = NORMAL.replace("x:1400.0,y:950.0", "x:64.0,y:64.0");
        assert_eq!(geometry_to_keep(Some(&tiny), Some(NORMAL), MIN).as_deref(), Some(NORMAL));
    }

    #[test]
    fn fullscreen_is_not_kept() {
        let kept = geometry_to_keep(Some(FULLSCREEN), None, MIN).expect("usable");
        assert_eq!(kept, NORMAL);
        let plan = plan_start(Some(FULLSCREEN), None, DEFAULT, MIN);
        assert_eq!(plan.restored.and_then(|saved| saved.to_text()).as_deref(), Some(NORMAL));
    }

    #[test]
    fn a_first_window_fits_the_work_area_and_is_centred() {
        let plan = plan_start(None, Some(laptop()), DEFAULT, MIN);
        assert_eq!(plan.restored, None);
        assert!(!plan.maximized);
        // 1080 pixels less the frame, at 150 %: 682.7 points of height; the width fits.
        assert_eq!(plan.size[0], 1280.0);
        assert!((plan.size[1] - (1080.0 - 56.0) / 1.5).abs() < 0.01, "{:?}", plan.size);
        let [x, y] = plan.position.expect("centred");
        let outer_bottom_px = y * 1.5 + 45.0 + plan.size[1] * 1.5 + 11.0;
        assert!(outer_bottom_px <= 1080.0 + 0.01, "{outer_bottom_px}");
        assert!((x * 1.5 + (1280.0 * 1.5 + 22.0) / 2.0 - 1024.0).abs() < 0.01, "{x}");
        // A big screen keeps the default size.
        let big = WorkArea { rect_px: [0.0, 0.0, 2560.0, 1400.0], scale: 1.0, frame_px: [8.0, 31.0, 8.0, 8.0] };
        assert_eq!(plan_start(None, Some(big), DEFAULT, MIN).size, DEFAULT);
        // Unknown work area: the default, wherever Windows puts it.
        assert_eq!(plan_start(None, None, DEFAULT, MIN), StartPlan { restored: None, size: DEFAULT, position: None, maximized: false });
    }

    #[test]
    fn a_restored_window_is_cut_to_the_work_area_and_never_below_the_minimum() {
        let plan = plan_start(Some(NORMAL), Some(laptop()), DEFAULT, MIN);
        let restored = plan.restored.expect("restored");
        let [width, height] = restored.inner_size();
        assert!((width - (2048.0 - 22.0) / 1.5).abs() < 0.01 && (height - (1080.0 - 56.0) / 1.5).abs() < 0.01, "{width} x {height}");
        // The minimised geometry of an older version starts like a first window.
        let plan = plan_start(Some(MINIMISED), Some(laptop()), DEFAULT, MIN);
        assert_eq!(plan.restored, None);
        assert!(plan.size[0] >= MIN[0] && plan.size[1] >= MIN[1]);
    }

    #[test]
    fn a_screen_smaller_than_the_smallest_window_starts_maximised() {
        let small = WorkArea { rect_px: [0.0, 0.0, 800.0, 560.0], scale: 1.25, frame_px: [10.0, 40.0, 10.0, 10.0] };
        let plan = plan_start(None, Some(small), DEFAULT, MIN);
        assert!(plan.maximized);
        assert_eq!(plan.size, MIN);
        assert!(plan_start(Some(NORMAL), Some(small), DEFAULT, MIN).maximized);
    }

    #[test]
    fn the_file_is_corrected_before_eframe_reads_it() {
        let folder = instagram_core::test_folder::TestFolder::new("placement").expect("a folder");
        let file = folder.join("window.ron");
        // What eframe writes: a map with the geometry under "window".
        let eframe_file = ron::to_string(&HashMap::from([(STORAGE_WINDOW_KEY.to_owned(), MINIMISED.to_owned())])).expect("ron");
        std::fs::write(&file, eframe_file).expect("written");
        let stored = read_stored(&file);
        assert_eq!(stored.as_deref(), Some(MINIMISED));
        let plan = plan_start(stored.as_deref(), Some(laptop()), DEFAULT, MIN);
        apply_to_file(&file, stored.as_deref(), &plan);
        assert!(!file.exists(), "an unusable geometry is removed");

        std::fs::write(&file, ron::to_string(&HashMap::from([(STORAGE_WINDOW_KEY.to_owned(), FULLSCREEN.to_owned())])).expect("ron")).expect("written");
        let stored = read_stored(&file);
        let plan = plan_start(stored.as_deref(), None, DEFAULT, MIN);
        apply_to_file(&file, stored.as_deref(), &plan);
        assert_eq!(read_stored(&file).as_deref(), Some(NORMAL));
    }
}
