//! The CSS gradients of the reference web project, rendered into small textures.
//!
//! egui's painter has no gradient primitive, and the web project uses two kinds that matter for
//! the look: the conic "story" ring around an avatar (`styles.css:134`) and the twelve placeholder
//! backgrounds (`data.ts:116-139`) that stand in for an avatar without a picture and for a picture
//! that is loading or cannot be shown. They are static, so each is evaluated once per size on the
//! CPU, exactly as CSS defines it (premultiplied interpolation in sRGB), and cached as a texture.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::{Color32, ColorImage, TextureHandle, TextureOptions};
use instagram_core::text::PLACEHOLDER_KEYS;

/// One colour stop: position along the gradient (0..=1) and a straight RGBA colour.
type Stop = (f32, [f32; 4]);

/// One layer of a CSS `background`.
#[derive(Debug, Clone, PartialEq)]
enum Layer {
    /// `linear-gradient(<angle>deg, ...)`.
    Linear { angle_deg: f32, stops: Vec<Stop> },
    /// `radial-gradient(circle at <x>% <y>%, ...)`, sized to the farthest corner.
    Radial { center: [f32; 2], stops: Vec<Stop> },
    /// `conic-gradient(from <angle>deg, ...)`.
    Conic { from_deg: f32, stops: Vec<Stop> },
}

/// A colour from `#rrggbb` digits.
const fn hex(value: u32) -> [f32; 4] {
    [((value >> 16) & 0xFF) as f32 / 255.0, ((value >> 8) & 0xFF) as f32 / 255.0, (value & 0xFF) as f32 / 255.0, 1.0]
}

/// A CSS `rgba(r, g, b, a)` colour.
const fn rgba(red: u8, green: u8, blue: u8, alpha: f32) -> [f32; 4] {
    [red as f32 / 255.0, green as f32 / 255.0, blue as f32 / 255.0, alpha]
}

/// The CSS keyword `transparent`.
const TRANSPARENT: [f32; 4] = [0.0, 0.0, 0.0, 0.0];

/// Stops spread evenly from 0 to 1, as CSS does for stops without a position.
fn even(colors: &[[f32; 4]]) -> Vec<Stop> {
    let last = (colors.len().max(2) - 1) as f32;
    colors.iter().enumerate().map(|(index, color)| (index as f32 / last, *color)).collect()
}

/// The layers of a placeholder background, topmost first, verbatim from `data.ts:116-139`.
fn placeholder_layers(key: &str) -> Vec<Layer> {
    match key {
        "books" => vec![
            Layer::Radial { center: [0.28, 0.22], stops: vec![(0.0, rgba(255, 240, 210, 0.55)), (0.45, TRANSPARENT)] },
            Layer::Linear { angle_deg: 135.0, stops: vec![(0.0, hex(0x7b5e3c)), (0.42, hex(0xb08b5e)), (1.0, hex(0xdcc09a))] },
        ],
        "finance" => vec![
            Layer::Radial { center: [0.78, 0.18], stops: vec![(0.0, rgba(120, 200, 255, 0.35)), (0.40, TRANSPARENT)] },
            Layer::Linear { angle_deg: 160.0, stops: vec![(0.0, hex(0x0a2a43)), (0.48, hex(0x14507a)), (1.0, hex(0x3a95b8))] },
        ],
        "moon" => vec![
            Layer::Radial {
                center: [0.63, 0.30],
                stops: vec![(0.0, hex(0xfdf6d8)), (0.05, hex(0xfdf6d8)), (0.09, rgba(253, 246, 216, 0.3)), (0.18, TRANSPARENT)],
            },
            Layer::Radial { center: [0.18, 0.82], stops: vec![(0.0, rgba(120, 80, 170, 0.45)), (0.45, TRANSPARENT)] },
            Layer::Linear {
                angle_deg: 170.0,
                stops: vec![(0.0, hex(0x0b1026)), (0.46, hex(0x241a4a)), (0.76, hex(0x5b3a86)), (1.0, hex(0xb06a9e))],
            },
        ],
        "floral" => vec![
            Layer::Radial { center: [0.24, 0.28], stops: vec![(0.0, rgba(255, 255, 255, 0.45)), (0.38, TRANSPARENT)] },
            Layer::Linear { angle_deg: 135.0, stops: vec![(0.0, hex(0xffd9e8)), (0.46, hex(0xf7a8c4)), (1.0, hex(0xbd5f97))] },
        ],
        "sunset" => vec![
            Layer::Radial { center: [0.50, 0.78], stops: vec![(0.0, hex(0xffd884)), (0.07, hex(0xffd884)), (0.28, TRANSPARENT)] },
            Layer::Linear {
                angle_deg: 180.0,
                stops: vec![(0.0, hex(0xf6d365)), (0.40, hex(0xf79b5b)), (0.74, hex(0xef476f)), (1.0, hex(0x6a2c70))],
            },
        ],
        "city" => vec![Layer::Linear { angle_deg: 180.0, stops: vec![(0.0, hex(0x2b3a55)), (0.58, hex(0x4b5d7a)), (1.0, hex(0xd4a373))] }],
        "coffee" => vec![
            Layer::Radial {
                center: [0.50, 0.46],
                stops: vec![
                    (0.0, hex(0xd9b48f)),
                    (0.29, hex(0xd9b48f)),
                    (0.30, hex(0xa9774f)),
                    (0.33, hex(0xa9774f)),
                    (0.35, hex(0x6f4a2f)),
                    (0.36, TRANSPARENT),
                ],
            },
            Layer::Linear { angle_deg: 135.0, stops: even(&[hex(0x3b2a20), hex(0x7a5a42)]) },
        ],
        "mountains" => vec![Layer::Linear {
            angle_deg: 180.0,
            stops: vec![(0.0, hex(0x90c4e4)), (0.40, hex(0xcfe6f2)), (0.41, hex(0x7fa07a)), (1.0, hex(0x3f5e42))],
        }],
        "cat" => vec![Layer::Linear { angle_deg: 135.0, stops: even(&[hex(0xf6d365), hex(0xfda085)]) }],
        "art" => vec![Layer::Conic {
            from_deg: 90.0,
            stops: even(&[hex(0xff5f6d), hex(0xffc371), hex(0x47e6a0), hex(0x4aa8ff), hex(0xa06bff), hex(0xff5f6d)]),
        }],
        "forest" => vec![
            Layer::Radial { center: [0.40, 0.18], stops: vec![(0.0, rgba(255, 255, 220, 0.35)), (0.42, TRANSPARENT)] },
            Layer::Linear { angle_deg: 160.0, stops: even(&[hex(0x1e3d2f), hex(0x3a6b46), hex(0x8ab661)]) },
        ],
        // "ocean" is also the web project's fallback for an unknown key.
        _ => vec![
            Layer::Radial { center: [0.70, 0.20], stops: vec![(0.0, rgba(255, 255, 255, 0.25)), (0.35, TRANSPARENT)] },
            Layer::Linear { angle_deg: 180.0, stops: vec![(0.0, hex(0x4facfe)), (0.55, hex(0x1e6fb0)), (1.0, hex(0x0b3d6b))] },
        ],
    }
}

/// The colour of the initial letter on a placeholder avatar (`AVATAR_TINT`, `common.tsx:13-17`).
pub fn placeholder_tint(key: &str) -> Color32 {
    let value: u32 = match key {
        "ocean" => 0x0b3d6b,
        "forest" => 0x173a26,
        "city" => 0x2b3a55,
        "moon" => 0x2a1a4a,
        "books" => 0x5b4228,
        "floral" => 0xa03f75,
        "sunset" => 0x7a2f5a,
        "coffee" => 0x3b2a20,
        "mountains" => 0x2f4a34,
        "cat" => 0x8a4a2a,
        "art" => 0x3a2a6a,
        "finance" => 0x0a2a43,
        _ => 0x262626,
    };
    Color32::from_rgb((value >> 16) as u8, (value >> 8) as u8, value as u8)
}

/// The placeholder key of a username: the web project's FNV-1a pick among the twelve gradients.
pub fn placeholder_key(username: &str) -> &'static str {
    PLACEHOLDER_KEYS[instagram_core::text::placeholder_index(username) % PLACEHOLDER_KEYS.len()]
}

/// The colour of a gradient at position `t`, interpolated between premultiplied colours as CSS
/// does (so a fade to `transparent` does not darken).
fn sample(stops: &[Stop], t: f32) -> [f32; 4] {
    let premultiplied = |color: [f32; 4]| [color[0] * color[3], color[1] * color[3], color[2] * color[3], color[3]];
    let Some(first) = stops.first() else {
        return TRANSPARENT;
    };
    if t <= first.0 {
        return premultiplied(first.1);
    }
    for pair in stops.windows(2) {
        let (start, end) = (pair[0], pair[1]);
        if t <= end.0 {
            let span = end.0 - start.0;
            let local = if span <= f32::EPSILON { 1.0 } else { (t - start.0) / span };
            let (from, to) = (premultiplied(start.1), premultiplied(end.1));
            return [
                from[0] + (to[0] - from[0]) * local,
                from[1] + (to[1] - from[1]) * local,
                from[2] + (to[2] - from[2]) * local,
                from[3] + (to[3] - from[3]) * local,
            ];
        }
    }
    premultiplied(stops[stops.len() - 1].1)
}

/// The premultiplied colour of one layer at the point (`x`, `y`) of a `width` x `height` box.
fn layer_color(layer: &Layer, x: f32, y: f32, width: f32, height: f32) -> [f32; 4] {
    match layer {
        Layer::Linear { angle_deg, stops } => {
            // CSS: 0deg points up, 90deg to the right; the gradient line passes through the
            // centre and is as long as the box measured along it.
            let angle = angle_deg.to_radians();
            let (direction_x, direction_y) = (angle.sin(), -angle.cos());
            let length = (width * direction_x).abs() + (height * direction_y).abs();
            let along = (x - width / 2.0) * direction_x + (y - height / 2.0) * direction_y;
            sample(stops, along / length.max(f32::EPSILON) + 0.5)
        }
        Layer::Radial { center, stops } => {
            let (center_x, center_y) = (center[0] * width, center[1] * height);
            // `circle` without a size reaches the farthest corner.
            let radius = [(0.0, 0.0), (width, 0.0), (0.0, height), (width, height)]
                .iter()
                .map(|(corner_x, corner_y)| ((corner_x - center_x).powi(2) + (corner_y - center_y).powi(2)).sqrt())
                .fold(0.0_f32, f32::max);
            let distance = ((x - center_x).powi(2) + (y - center_y).powi(2)).sqrt();
            sample(stops, distance / radius.max(f32::EPSILON))
        }
        Layer::Conic { from_deg, stops } => sample(stops, conic_position(x - width / 2.0, y - height / 2.0, *from_deg)),
    }
}

/// The position (0..1) of the direction (`dx`, `dy`) in a conic gradient that starts at
/// `from_deg`, measured clockwise from 12 o'clock.
fn conic_position(dx: f32, dy: f32, from_deg: f32) -> f32 {
    let clockwise_from_top = dx.atan2(-dy).to_degrees();
    ((clockwise_from_top - from_deg) / 360.0).rem_euclid(1.0)
}

/// Renders the placeholder background `key` into a `width` x `height` bitmap.
pub fn placeholder_image(key: &str, width: usize, height: usize) -> ColorImage {
    let layers = placeholder_layers(key);
    let mut pixels = Vec::with_capacity(width * height);
    for row in 0..height {
        for column in 0..width {
            let (x, y) = (column as f32 + 0.5, row as f32 + 0.5);
            // Composite from the bottom layer up; the bottom layer of every key is opaque.
            let mut color = [0.0_f32; 4];
            for layer in layers.iter().rev() {
                let top = layer_color(layer, x, y, width as f32, height as f32);
                for channel in 0..4 {
                    color[channel] = top[channel] + color[channel] * (1.0 - top[3]);
                }
            }
            pixels.push(to_color32(color));
        }
    }
    ColorImage::new([width, height], pixels)
}

/// A premultiplied float colour as a `Color32`.
fn to_color32(color: [f32; 4]) -> Color32 {
    let byte = |value: f32| (value.clamp(0.0, 1.0) * 255.0).round() as u8;
    Color32::from_rgba_premultiplied(byte(color[0]), byte(color[1]), byte(color[2]), byte(color[3]))
}

/// The stops of the story ring: `conic-gradient(from 210deg, #ffdc80, #f77737, #e1306c, #833ab4,
/// #405de6, #ffdc80)`.
fn story_ring_stops() -> Vec<Stop> {
    even(&[hex(0xffdc80), hex(0xf77737), hex(0xe1306c), hex(0x833ab4), hex(0x405de6), hex(0xffdc80)])
}

/// Renders the story ring as a filled disc of `diameter_px` pixels with an anti-aliased edge.
/// The avatar's surface-coloured border and picture are painted on top of it.
pub fn story_ring_image(diameter_px: usize) -> ColorImage {
    let stops = story_ring_stops();
    let radius = diameter_px as f32 / 2.0;
    let mut pixels = Vec::with_capacity(diameter_px * diameter_px);
    for row in 0..diameter_px {
        for column in 0..diameter_px {
            let (dx, dy) = (column as f32 + 0.5 - radius, row as f32 + 0.5 - radius);
            let coverage = (radius - (dx * dx + dy * dy).sqrt() + 0.5).clamp(0.0, 1.0);
            let color = sample(&stops, conic_position(dx, dy, 210.0));
            pixels.push(to_color32([color[0] * coverage, color[1] * coverage, color[2] * coverage, coverage]));
        }
    }
    ColorImage::new([diameter_px, diameter_px], pixels)
}

/// Side of the cached placeholder textures in pixels. The gradients are smooth, so one size
/// serves avatars (scaled down) and picture frames (scaled up).
const PLACEHOLDER_TEXTURE_SIDE_PX: usize = 256;

/// The gradient textures made so far.
#[derive(Default)]
struct GradientCache {
    placeholders: HashMap<&'static str, TextureHandle>,
    rings: HashMap<usize, TextureHandle>,
}

fn cache(ctx: &egui::Context) -> Arc<Mutex<GradientCache>> {
    let id = egui::Id::unique("instagram-windows-app/gradient-cache");
    ctx.data_mut(|data| data.get_temp_mut_or_default::<Arc<Mutex<GradientCache>>>(id).clone())
}

/// The texture of the placeholder background of `username`.
pub fn placeholder_texture(ctx: &egui::Context, username: &str) -> TextureHandle {
    let key = placeholder_key(username);
    let cache = cache(ctx);
    let mut cache = match cache.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    cache
        .placeholders
        .entry(key)
        .or_insert_with(|| {
            let image = placeholder_image(key, PLACEHOLDER_TEXTURE_SIDE_PX, PLACEHOLDER_TEXTURE_SIDE_PX);
            ctx.load_texture(format!("placeholder-{key}"), image, TextureOptions::LINEAR)
        })
        .clone()
}

/// The texture of a story ring of `diameter_px` pixels.
pub fn story_ring_texture(ctx: &egui::Context, diameter_px: usize) -> TextureHandle {
    let cache = cache(ctx);
    let mut cache = match cache.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    cache
        .rings
        .entry(diameter_px)
        .or_insert_with(|| ctx.load_texture(format!("story-ring-{diameter_px}"), story_ring_image(diameter_px), TextureOptions::LINEAR))
        .clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rgb(color: Color32) -> [i32; 3] {
        [color.r() as i32, color.g() as i32, color.b() as i32]
    }

    fn near(actual: Color32, expected: u32, tolerance: i32) -> bool {
        let expected = [(expected >> 16) as i32 & 0xFF, (expected >> 8) as i32 & 0xFF, expected as i32 & 0xFF];
        rgb(actual).iter().zip(expected).all(|(a, e)| (a - e).abs() <= tolerance)
    }

    #[test]
    fn a_vertical_gradient_runs_from_its_first_to_its_last_stop() {
        // city: linear-gradient(180deg, #2b3a55 0%, #4b5d7a 58%, #d4a373 100%).
        let image = placeholder_image("city", 8, 200);
        assert!(near(image.pixels[0], 0x2b3a55, 2));
        assert!(near(image.pixels[199 * 8], 0xd4a373, 2));
        assert!(near(image.pixels[116 * 8], 0x4b5d7a, 3));
    }

    #[test]
    fn a_diagonal_gradient_starts_in_the_top_left_corner() {
        // cat: linear-gradient(135deg, #f6d365, #fda085).
        let image = placeholder_image("cat", 100, 100);
        assert!(near(image.pixels[0], 0xf6d365, 3));
        assert!(near(image.pixels[99 * 100 + 99], 0xfda085, 3));
    }

    #[test]
    fn a_radial_highlight_brightens_its_centre_only() {
        // ocean: a white highlight at 70% 20% over a blue vertical gradient.
        let image = placeholder_image("ocean", 100, 100);
        let highlight = image.pixels[20 * 100 + 70];
        let same_row_far = image.pixels[20 * 100 + 2];
        assert!(highlight.r() > same_row_far.r() + 20);
        // Bottom edge: the last stop, untouched by the highlight.
        assert!(near(image.pixels[99 * 100 + 50], 0x0b3d6b, 4));
    }

    #[test]
    fn every_key_is_opaque_and_has_a_tint() {
        for key in PLACEHOLDER_KEYS {
            let image = placeholder_image(key, 16, 16);
            assert!(image.pixels.iter().all(|pixel| pixel.a() == 255), "{key} has transparent pixels");
            assert_ne!(placeholder_tint(key), Color32::from_rgb(0x26, 0x26, 0x26), "{key} has no tint");
        }
        assert_eq!(placeholder_tint("something else"), Color32::from_rgb(0x26, 0x26, 0x26));
    }

    #[test]
    fn the_story_ring_starts_at_seven_o_clock_and_runs_clockwise() {
        assert!(conic_position(0.0, -1.0, 0.0).abs() < 1e-4);
        assert!((conic_position(1.0, 0.0, 0.0) - 0.25).abs() < 1e-4);
        assert!((conic_position(0.0, 1.0, 0.0) - 0.5).abs() < 1e-4);
        // 210 degrees clockwise from the top is the lower left.
        let (dx, dy) = (210.0_f32.to_radians().sin(), -210.0_f32.to_radians().cos());
        assert!(conic_position(dx, dy, 210.0) < 1e-3 || conic_position(dx, dy, 210.0) > 0.999);

        let image = story_ring_image(80);
        // Centre of the disc is covered, the corners are not.
        assert_eq!(image.pixels[40 * 80 + 40].a(), 255);
        assert_eq!(image.pixels[0].a(), 0);
        // A fifth of a turn after the start (282 degrees: left, slightly up) is #f77737.
        let angle = 282.0_f32.to_radians();
        let (x, y) = (40.0 + angle.sin() * 36.0, 40.0 - angle.cos() * 36.0);
        assert!(near(image.pixels[y as usize * 80 + x as usize], 0xf77737, 12));
        // Three fifths (66 degrees: upper right) is #833ab4.
        let angle = 66.0_f32.to_radians();
        let (x, y) = (40.0 + angle.sin() * 36.0, 40.0 - angle.cos() * 36.0);
        assert!(near(image.pixels[y as usize * 80 + x as usize], 0x833ab4, 12));
    }

    #[test]
    fn usernames_map_to_one_of_the_twelve_keys() {
        for name in ["fixture.sunrise.bakery", "fixture_tidewater", "pixel.orchard.demo", "unknown", ""] {
            assert!(PLACEHOLDER_KEYS.contains(&placeholder_key(name)));
        }
    }
}
