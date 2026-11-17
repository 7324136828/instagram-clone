//! What keeps the window quick to draw on a machine without a usable graphics card.
//!
//! There, Direct3D 12 draws with the Microsoft Basic Render Driver (WARP) on the processor, and
//! every pixel of every shape costs processor time. Measured on such a machine (two logical
//! processors, a window of 1920 x 1290 pixels, the viewer on a camera photo): a frame took about
//! 720 ms of processor time, 2 to 4 frames per second while zooming, panning or animating.
//! Three things make it cheap again:
//!
//! * Texture filtering in the shader. egui's shader samples every texture with the hardware
//!   sampler, and WARP is very slow at that: the same frame drawn with `textureLoad` and the
//!   bilinear arithmetic in the shader takes a quarter of the time, and flat-coloured shapes
//!   need a single load. `adapt_renderer` switches the renderer to the shader of
//!   `vendor/egui-wgpu` that does this when the adapter is a processor; a graphics card keeps
//!   the hardware sampler. The result is the same picture: both filter bilinearly between the
//!   four nearest texels.
//! * Not drawing what is covered. Layers lie on top of each other (the feed, the dark scrim of
//!   the viewer, the viewer's card), and the rasteriser shades every pixel of every layer, also
//!   where an opaque layer above will cover it. `keep_out` clips the shapes below such a layer to
//!   the bands around it, so the pixels under it are shaded once, by the opaque layer. Nothing
//!   on screen changes; it helps a graphics card as well, a little.
//! * Room for two frames in the swap chain (`software_surface_config`), so that the processor
//!   draws the next frame while the last one waits for the refresh of the display.

use std::ops::Range;

use eframe::egui_wgpu::{self, RendererOptions};
use egui::epaint::ClippedShape;
use egui::layers::ShapeIdx;
use egui::{LayerId, Pos2, Rect};

/// True when the adapter draws on the processor (WARP, or another software rasteriser): the
/// textures are then filtered in the shader.
pub fn filters_in_shader(device_type: wgpu::DeviceType) -> bool {
    device_type == wgpu::DeviceType::Cpu
}

/// The renderer options of a window whose textures are filtered in the shader. The others must
/// be what eframe gave the renderer for the `NativeOptions` of `main.rs`, which also sized the
/// window's own buffers: `multisampling` and `dithering` as given there, no depth or stencil
/// buffer.
pub fn shader_filtering_options(multisampling: u16, dithering: bool) -> RendererOptions {
    RendererOptions { msaa_samples: u32::from(multisampling), depth_stencil_format: None, dithering, predictable_texture_filtering: true }
}

/// Gives the window a renderer that filters textures in the shader when the adapter is a
/// processor; returns whether it did. Call it before the first frame (from the app creator of
/// eframe): the new renderer starts without textures, and egui uploads all of its own with the
/// first frame.
pub fn adapt_renderer(render_state: &egui_wgpu::RenderState, multisampling: u16, dithering: bool) -> bool {
    if !filters_in_shader(render_state.adapter.get_info().device_type) {
        return false;
    }
    let renderer = egui_wgpu::Renderer::new(&render_state.device, render_state.target_format, shader_filtering_options(multisampling, dithering));
    *render_state.renderer.write() = renderer;
    true
}

/// How a window drawn by the processor presents its frames: with vertical sync as everywhere,
/// but with room for two frames instead of one. With one, a frame that is drawn waits for the
/// next refresh of the display before the next can start, so a frame that takes a little longer
/// than a refresh costs two; measured in a remote session of 32 Hz, a viewer frame of 1920 x 1290
/// pixels came out at 16 frames per second with one and at 23 with two, for about the same
/// processor time per frame. Showing frames without vertical sync (`Mailbox`) was as fast for slow frames
/// but drew frames the display never showed when they were quick (70 per second at 32 Hz). The
/// app still waits for the previous frame before it describes the next (`App::logic`), so frames
/// never pile up. A graphics card keeps eframe's default of one frame.
pub fn software_surface_config() -> egui_wgpu::SurfaceConfig {
    egui_wgpu::SurfaceConfig { present_mode: wgpu::PresentMode::AutoVsync, desired_maximum_frame_latency: Some(2) }
}

/// The four bands around `hole` that together cover everything else: above (full width), below
/// (full width), left and right of it (the height of the hole). They do not overlap, so a shape
/// drawn once into each band is drawn exactly once wherever it is outside the hole.
pub fn bands_around(hole: Rect) -> [Rect; 4] {
    let far = f32::INFINITY;
    [
        Rect::from_min_max(Pos2::new(-far, -far), Pos2::new(far, hole.top())),
        Rect::from_min_max(Pos2::new(-far, hole.bottom()), Pos2::new(far, far)),
        Rect::from_min_max(Pos2::new(-far, hole.top()), Pos2::new(hole.left(), hole.bottom())),
        Rect::from_min_max(Pos2::new(hole.right(), hole.top()), Pos2::new(far, hole.bottom())),
    ]
}

/// The paint list `entries` with the shapes at `range` kept out of `hole`: each of them is
/// replaced by copies clipped to the bands around the hole, band after band. Within a band the
/// shapes keep their order, and the bands do not overlap, so every pixel outside the hole gets
/// the same shapes in the same order as before. Shapes before and after `range` stay as they are.
pub fn kept_out(entries: &[ClippedShape], range: Range<usize>, hole: Rect) -> Vec<ClippedShape> {
    let end = range.end.min(entries.len());
    let start = range.start.min(end);
    let mut rebuilt = Vec::with_capacity(entries.len() + 3 * (end - start));
    rebuilt.extend_from_slice(&entries[..start]);
    for band in bands_around(hole) {
        for entry in &entries[start..end] {
            let clip_rect = entry.clip_rect.intersect(band);
            if clip_rect.is_positive() {
                rebuilt.push(ClippedShape { clip_rect, ..entry.clone() });
            }
        }
    }
    rebuilt.extend_from_slice(&entries[end..]);
    rebuilt
}

/// Keeps the shapes at `range` of `layer` from drawing inside `hole`, in screen points (see
/// `kept_out`). Call it only when what is painted later covers all of `hole` with an opaque
/// colour, and after the shapes at `range` have all been painted. A layer that egui moves or
/// scales is left alone: its clip rectangles are not in screen points.
///
/// The shapes of the layer move to other places of its paint list. Call it only while no
/// container of the layer is still open: a frame fills in its background at the place it took
/// when it opened, and that place would by then hold another shape. The app calls it at the top
/// level of `App::frame_ui`, where the root Ui that eframe hands over has no frame of its own.
pub fn keep_out(ctx: &egui::Context, layer: LayerId, range: Range<usize>, hole: Rect) {
    if !hole.is_positive() || ctx.layer_transform_to_global(layer).is_some_and(|transform| transform != egui::emath::TSTransform::IDENTITY) {
        return;
    }
    ctx.graphics_mut(|graphics| {
        let Some(list) = graphics.get_mut(layer) else {
            return;
        };
        let entries: Vec<ClippedShape> = list.all_entries().cloned().collect();
        let rebuilt = kept_out(&entries, range, hole);
        // A paint list can only be changed shape by shape: the old places are overwritten in
        // order, new ones appended, and places left over emptied.
        for (index, entry) in rebuilt.iter().enumerate() {
            if index < entries.len() {
                list.mutate_shape(ShapeIdx(index), |shape| *shape = entry.clone());
            } else {
                let added = list.add(entry.clip_rect, egui::Shape::Noop);
                list.mutate_shape(added, |shape| *shape = entry.clone());
            }
        }
        for index in rebuilt.len()..entries.len() {
            list.reset_shape(ShapeIdx(index));
        }
    });
}

#[cfg(test)]
mod tests {
    use egui::{pos2, vec2, Color32, Shape};

    use super::*;

    fn rect_shape(clip: Rect, color: Color32) -> ClippedShape {
        ClippedShape::new(clip, Shape::rect_filled(clip, 0.0, color))
    }

    fn screen() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(1280.0, 860.0))
    }

    #[test]
    fn only_a_processor_filters_in_the_shader() {
        assert!(filters_in_shader(wgpu::DeviceType::Cpu));
        for device in [wgpu::DeviceType::DiscreteGpu, wgpu::DeviceType::IntegratedGpu, wgpu::DeviceType::VirtualGpu, wgpu::DeviceType::Other] {
            assert!(!filters_in_shader(device), "{device:?}");
        }
    }

    #[test]
    fn shader_filtering_keeps_what_eframe_chose_for_the_window() {
        let options = shader_filtering_options(0, false);
        assert!(options.predictable_texture_filtering);
        assert_eq!((options.msaa_samples, options.depth_stencil_format, options.dithering), (0, None, false));
        let other = shader_filtering_options(4, true);
        assert_eq!((other.msaa_samples, other.dithering), (4, true));
    }

    #[test]
    fn software_rendering_keeps_vertical_sync_with_room_for_two_frames() {
        let config = software_surface_config();
        // The same present mode as eframe's default for every window, one frame more.
        assert_eq!(config.present_mode, egui_wgpu::SurfaceConfig::LOW_LATENCY.present_mode);
        assert_eq!(config.present_mode, wgpu::PresentMode::AutoVsync);
        assert_eq!(config.desired_maximum_frame_latency, Some(2));
    }

    #[test]
    fn the_bands_cover_everything_but_the_hole_once() {
        let hole = Rect::from_min_max(pos2(72.0, 44.0), pos2(1208.0, 816.0));
        let bands = bands_around(hole);
        // Points of the screen: in the hole no band, elsewhere exactly one.
        for x in (0..=1280).step_by(8) {
            for y in (0..=860).step_by(4) {
                let point = pos2(x as f32 + 0.5, y as f32 + 0.5);
                let covering = bands.iter().filter(|band| band.contains(point)).count();
                assert_eq!(covering, usize::from(!hole.contains(point)), "{point:?}");
            }
        }
    }

    #[test]
    fn shapes_are_clipped_band_by_band_in_their_order() {
        let hole = Rect::from_min_max(pos2(100.0, 100.0), pos2(300.0, 200.0));
        let entries = vec![rect_shape(screen(), Color32::RED), rect_shape(screen(), Color32::BLUE)];
        let rebuilt = kept_out(&entries, 0..2, hole);
        assert_eq!(rebuilt.len(), 8);
        let bands = bands_around(hole);
        for (band_index, band) in bands.iter().enumerate() {
            let pair = &rebuilt[band_index * 2..band_index * 2 + 2];
            assert_eq!(pair[0].shape, entries[0].shape, "red first in band {band_index}");
            assert_eq!(pair[1].shape, entries[1].shape, "then blue in band {band_index}");
            for entry in pair {
                assert_eq!(entry.clip_rect, screen().intersect(*band));
                assert!(!entry.clip_rect.intersects(hole.shrink(0.001)));
            }
        }
    }

    #[test]
    fn shapes_inside_the_hole_go_and_shapes_outside_the_range_stay() {
        let hole = Rect::from_min_max(pos2(100.0, 100.0), pos2(300.0, 200.0));
        let inside = rect_shape(Rect::from_min_max(pos2(120.0, 120.0), pos2(200.0, 180.0)), Color32::GREEN);
        let beside = rect_shape(Rect::from_min_max(pos2(10.0, 120.0), pos2(50.0, 180.0)), Color32::GRAY);
        let before = rect_shape(screen(), Color32::BLACK);
        let after = rect_shape(screen(), Color32::WHITE);
        let entries = vec![before.clone(), inside, beside.clone(), after.clone()];
        let rebuilt = kept_out(&entries, 1..3, hole);
        // The shape in the hole is gone; the one beside it is kept once, in the left band.
        assert_eq!(rebuilt, vec![before, beside, after]);
        // A range beyond the list changes nothing.
        assert_eq!(kept_out(&entries, 4..9, hole), entries);
    }
}
