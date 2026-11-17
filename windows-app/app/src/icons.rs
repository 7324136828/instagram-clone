//! The icons of the app, rasterised from the reference web project's SVG glyphs.
//!
//! The web project has one glyph per icon (`icons.tsx`): the outline variant strokes it with
//! round caps and joins, the filled variant fills the same geometry. The on-screen stroke is
//! `stroke_width * size / 24`, so a bitmap cannot be scaled without changing its weight: every
//! (icon, size in pixels, filled, stroke width) gets its own white texture, rasterised by resvg
//! through `egui_extras` and tinted with the current colour when painted. The textures are cached
//! in the egui context, so any painting code can draw an icon with nothing but a painter.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use egui::{pos2, vec2, Color32, ColorImage, Painter, Pos2, Rect, TextureHandle, TextureOptions};

/// The stroke width of an outline icon, in units of the 24 x 24 view box (`strokeWidth = 1.9`).
pub const DEFAULT_STROKE_WIDTH: f32 = 1.9;
/// The stroke width of the carousel arrows.
pub const CAROUSEL_STROKE_WIDTH: f32 = 2.6;
/// The side of the view box every glyph is drawn in.
const VIEW_BOX: f32 = 24.0;

/// Every icon the app uses. The names follow `icons.tsx`; `Pause`, `Logo`, `Verified` are the
/// web project's inline SVGs, `Info` is NEW (the About entry has no glyph in the web project).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Icon {
    Home,
    Search,
    Compass,
    Heart,
    Create,
    More,
    Settings,
    Bookmark,
    Comment,
    Share,
    Ellipsis,
    Emoji,
    ChevronLeft,
    ChevronRight,
    Close,
    Back,
    Sun,
    Moon,
    Grid,
    Clip,
    Carousel,
    Tag,
    Clock,
    Plus,
    Import,
    External,
    Folder,
    Archive,
    Link,
    Warning,
    Check,
    Play,
    Pause,
    Download,
    Info,
    Logo,
    Verified,
    Volume,
    VolumeOff,
}

impl Icon {
    /// The shapes of the glyph, as the inside of an `<svg>` element. Strokes and fills are
    /// inherited from the root element, which is white: the texture is tinted when painted.
    fn shapes(self) -> &'static str {
        match self {
            Icon::Home => r#"<path d="M3 9.6 12 2.8l9 6.8V20a1 1 0 0 1-1 1h-5v-6.5H9V21H4a1 1 0 0 1-1-1V9.6Z"/>"#,
            Icon::Search => r#"<circle cx="10.8" cy="10.8" r="6.8"/><path d="m16 16 5 5"/>"#,
            Icon::Compass => r#"<circle cx="12" cy="12" r="9.2"/><path d="m15.6 8.4-2.1 5.1-5.1 2.1 2.1-5.1 5.1-2.1Z"/>"#,
            Icon::Heart => {
                r#"<path d="M12 20.6 10.8 19.5C5.2 14.4 2.4 11.8 2.4 8.4 2.4 5 5 2.8 8.2 2.8c1.8 0 3.6.9 4.6 2.3 1-1.4 2.8-2.3 4.6-2.3 3.2 0 5.8 2.2 5.8 5.6 0 3.4-2.8 6-8.4 11.1L12 20.6Z"/>"#
            }
            Icon::Create => r#"<rect x="2.8" y="2.8" width="18.4" height="18.4" rx="5"/><path d="M12 8v8M8 12h8"/>"#,
            Icon::More => r#"<path d="M4 6.5h16M4 12h16M4 17.5h16"/>"#,
            Icon::Settings => {
                r#"<circle cx="12" cy="12" r="3"/><path d="M19.1 14.5a1.6 1.6 0 0 0 .32 1.77l.06.06a1.9 1.9 0 1 1-2.69 2.69l-.06-.06a1.6 1.6 0 0 0-1.77-.32 1.6 1.6 0 0 0-.97 1.47v.17a1.9 1.9 0 1 1-3.8 0v-.09a1.6 1.6 0 0 0-1.05-1.47 1.6 1.6 0 0 0-1.77.32l-.06.06a1.9 1.9 0 1 1-2.69-2.69l.06-.06a1.6 1.6 0 0 0 .32-1.77 1.6 1.6 0 0 0-1.47-.97H3.3a1.9 1.9 0 1 1 0-3.8h.09a1.6 1.6 0 0 0 1.47-1.05 1.6 1.6 0 0 0-.32-1.77l-.06-.06a1.9 1.9 0 1 1 2.69-2.69l.06.06a1.6 1.6 0 0 0 1.77.32H9a1.6 1.6 0 0 0 .97-1.47V3.3a1.9 1.9 0 1 1 3.8 0v.09a1.6 1.6 0 0 0 .97 1.47 1.6 1.6 0 0 0 1.77-.32l.06-.06a1.9 1.9 0 1 1 2.69 2.69l-.06.06a1.6 1.6 0 0 0-.32 1.77V9a1.6 1.6 0 0 0 1.47.97h.17a1.9 1.9 0 1 1 0 3.8h-.09a1.6 1.6 0 0 0-1.47.73Z"/>"#
            }
            Icon::Bookmark => r#"<path d="M6.5 2.8h11a1 1 0 0 1 1 1v17.4l-6.5-3.9-6.5 3.9V3.8a1 1 0 0 1 1-1Z"/>"#,
            Icon::Comment => {
                r#"<path d="M20.8 11.6a8.7 8.7 0 0 1-9.3 8.7 9.8 9.8 0 0 1-3.6-.7L3 21l1.5-4.6a8.5 8.5 0 0 1-1.3-4.6 8.7 8.7 0 0 1 9.3-8.6 8.7 8.7 0 0 1 8.3 8.4Z"/>"#
            }
            Icon::Share => r#"<path d="M21.6 2.4 11 13"/><path d="M21.6 2.4 15 21.6l-4-8.6-8.6-4 19.2-6.6Z"/>"#,
            Icon::Ellipsis => r#"<circle cx="5" cy="12" r="1.5"/><circle cx="12" cy="12" r="1.5"/><circle cx="19" cy="12" r="1.5"/>"#,
            Icon::Emoji => {
                r#"<circle cx="12" cy="12" r="9.2"/><path d="M8.2 14.2a4.6 4.6 0 0 0 7.6 0"/><circle cx="9" cy="9.6" r="1.05" fill="white" stroke="none"/><circle cx="15" cy="9.6" r="1.05" fill="white" stroke="none"/>"#
            }
            Icon::ChevronLeft => r#"<path d="m15 5-7 7 7 7"/>"#,
            Icon::ChevronRight => r#"<path d="m9 5 7 7-7 7"/>"#,
            Icon::Close => r#"<path d="M5 5 19 19M19 5 5 19"/>"#,
            Icon::Back => r#"<path d="M15 4 7 12l8 8"/>"#,
            Icon::Sun => {
                r#"<circle cx="12" cy="12" r="4.2"/><path d="M12 2v2.2M12 19.8V22M4.9 4.9l1.6 1.6M17.5 17.5l1.6 1.6M2 12h2.2M19.8 12H22M4.9 19.1l1.6-1.6M17.5 6.5l1.6-1.6"/>"#
            }
            Icon::Moon => r#"<path d="M20.5 14.2A8.6 8.6 0 0 1 9.8 3.5 8.8 8.8 0 1 0 20.5 14.2Z"/>"#,
            Icon::Grid => {
                r#"<rect x="3" y="3" width="7" height="7" rx="1"/><rect x="14" y="3" width="7" height="7" rx="1"/><rect x="3" y="14" width="7" height="7" rx="1"/><rect x="14" y="14" width="7" height="7" rx="1"/>"#
            }
            Icon::Clip => {
                r#"<rect x="2.8" y="4.5" width="18.4" height="15" rx="3"/><path d="m2.8 8.5 4-3.9M9 4.6l3.5 3.9M16.8 4.6l4.4 3.9"/><path d="m10.4 10.5 4.2 2.7-4.2 2.7v-5.4Z"/>"#
            }
            Icon::Carousel => r#"<rect x="7.5" y="3" width="13" height="13" rx="2.5"/><path d="M4.5 7.5v13a1 1 0 0 0 1 1h11"/>"#,
            Icon::Tag => {
                r#"<path d="M20.8 13.3 13.3 20.8a2 2 0 0 1-2.8 0L3 13.3V3.5a.5.5 0 0 1 .5-.5h9.8l7.5 7.5a2 2 0 0 1 0 2.8Z"/><circle cx="7.6" cy="7.6" r="1.3"/>"#
            }
            Icon::Clock => r#"<circle cx="12" cy="12" r="9"/><path d="M12 7v5.3l3.4 2"/>"#,
            Icon::Plus => r#"<path d="M12 5v14M5 12h14"/>"#,
            Icon::Import => {
                r#"<path d="M12 3v11.5"/><path d="m7.5 10.5 4.5 4.5 4.5-4.5"/><path d="M3.5 14.5v3.7a2.8 2.8 0 0 0 2.8 2.8h11.4a2.8 2.8 0 0 0 2.8-2.8v-3.7"/>"#
            }
            Icon::External => {
                r#"<path d="M18.5 13.5v4.7a2.3 2.3 0 0 1-2.3 2.3H5.8a2.3 2.3 0 0 1-2.3-2.3V7.8a2.3 2.3 0 0 1 2.3-2.3h4.7"/><path d="M14.5 3.5h6v6"/><path d="M20.5 3.5 11 13"/>"#
            }
            Icon::Folder => {
                r#"<path d="M3 7.2A2.2 2.2 0 0 1 5.2 5h3.6a2 2 0 0 1 1.5.7L11.6 7.2h7.2A2.2 2.2 0 0 1 21 9.4v7.9a2.2 2.2 0 0 1-2.2 2.2H5.2A2.2 2.2 0 0 1 3 17.3V7.2Z"/>"#
            }
            Icon::Archive => {
                r#"<rect x="3" y="3.5" width="18" height="5" rx="1.5"/><path d="M4.8 8.5v9.8a2.2 2.2 0 0 0 2.2 2.2h10a2.2 2.2 0 0 0 2.2-2.2V8.5"/><path d="M9.8 12.5h4.4"/>"#
            }
            Icon::Link => {
                r#"<path d="M10.2 13.8a4.2 4.2 0 0 0 5.9 0l3.2-3.2a4.2 4.2 0 0 0-5.9-5.9l-1.2 1.2"/><path d="M13.8 10.2a4.2 4.2 0 0 0-5.9 0l-3.2 3.2a4.2 4.2 0 0 0 5.9 5.9l1.2-1.2"/>"#
            }
            Icon::Warning => r#"<path d="M12 3.5 2.8 19.5h18.4L12 3.5Z"/><path d="M12 10v4.5"/><path d="M12 17.2v.1"/>"#,
            Icon::Check => r#"<path d="m5 12.5 4.5 4.5L19 7.5"/>"#,
            Icon::Play => r#"<path d="M7 4.8v14.4a.8.8 0 0 0 1.2.7l11.3-7.2a.8.8 0 0 0 0-1.4L8.2 4.1a.8.8 0 0 0-1.2.7Z"/>"#,
            Icon::Pause => r#"<rect x="6" y="4.5" width="4" height="15" rx="1"/><rect x="14" y="4.5" width="4" height="15" rx="1"/>"#,
            Icon::Download => r#"<path d="M12 3.5v11"/><path d="m7.5 10 4.5 4.5 4.5-4.5"/><path d="M4.5 19.5h15"/>"#,
            // The sound button of a video (icons.tsx:231-242).
            Icon::Volume => r#"<path d="M4 9.5h3.2L12 5.5v13l-4.8-4H4a1 1 0 0 1-1-1v-3a1 1 0 0 1 1-1Z"/><path d="M15.5 9a4.2 4.2 0 0 1 0 6M18.2 6.5a7.8 7.8 0 0 1 0 11"/>"#,
            Icon::VolumeOff => r#"<path d="M4 9.5h3.2L12 5.5v13l-4.8-4H4a1 1 0 0 1-1-1v-3a1 1 0 0 1 1-1Z"/><path d="m15.5 9.5 5 5M20.5 9.5l-5 5"/>"#,
            Icon::Info => r#"<circle cx="12" cy="12" r="9.2"/><path d="M12 11.2v5.6"/><path d="M12 7.4v.1"/>"#,
            Icon::Logo => {
                r#"<rect x="2.5" y="2.5" width="19" height="19" rx="5.5"/><circle cx="12" cy="12" r="4.4"/><circle cx="17.6" cy="6.4" r="1.2" fill="white" stroke="none"/>"#
            }
            // The badge is the one icon with colours of its own; see `document`.
            Icon::Verified => {
                r##"<path fill="#0095f6" d="M20 2.2 24.4 5.5 29.7 4.8 31.9 9.8 36.7 12.2 36 17.5 39.4 21.8 36 26.1 36.7 31.4 31.9 33.8 29.7 38.8 24.4 38.1 20 41.4 15.6 38.1 10.3 38.8 8.1 33.8 3.3 31.4 4 26.1.6 21.8 4 17.5 3.3 12.2 8.1 9.8 10.3 4.8 15.6 5.5 20 2.2Z"/><path d="m12.5 21 5 5 10-11" fill="none" stroke="#fff" stroke-width="3.4" stroke-linecap="round" stroke-linejoin="round"/>"##
            }
        }
    }

    /// Icons the web project always fills (`FILLED` in `icons.tsx`), plus the inline pause glyph.
    fn always_filled(self) -> bool {
        matches!(self, Icon::Ellipsis | Icon::Pause)
    }

    /// True for the icon that carries its own colours and must not be tinted.
    fn is_coloured(self) -> bool {
        matches!(self, Icon::Verified)
    }
}

/// The complete SVG document of an icon. `stroke_width` is in view-box units; a filled icon has
/// no stroke, exactly like `icons.tsx:260-279`.
pub fn document(icon: Icon, filled: bool, stroke_width: f32) -> String {
    if icon.is_coloured() {
        // The verified badge: view box 0 0 40 40 (common.tsx:248-272). The badge path pokes out
        // of the box at the bottom, as in the web project.
        return format!(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 40 40" width="40" height="40">{}</svg>"#, icon.shapes());
    }
    let filled = filled || icon.always_filled();
    let (fill, width) = if filled { ("white", 0.0) } else { ("none", stroke_width) };
    format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width="24" height="24" fill="{fill}" stroke="white" stroke-width="{width}" stroke-linecap="round" stroke-linejoin="round">{}</svg>"#,
        icon.shapes()
    )
}

/// How an icon is drawn.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct IconStyle {
    /// Side of the icon in points (the CSS `size`).
    pub size: f32,
    /// Fill the glyph instead of stroking it.
    pub filled: bool,
    /// Stroke width in view-box units.
    pub stroke_width: f32,
    /// A soft dark shadow under the icon, for icons drawn on a picture
    /// (`drop-shadow(0 1px 3px rgba(0, 0, 0, .55))`): its opacity, or None.
    pub shadow_opacity: Option<f32>,
}

impl IconStyle {
    /// An outline icon of `size` points with the default stroke.
    pub fn outline(size: f32) -> IconStyle {
        IconStyle { size, filled: false, stroke_width: DEFAULT_STROKE_WIDTH, shadow_opacity: None }
    }

    /// A filled icon of `size` points.
    pub fn filled(size: f32) -> IconStyle {
        IconStyle { size, filled: true, stroke_width: DEFAULT_STROKE_WIDTH, shadow_opacity: None }
    }

    /// Outline or filled, by a flag (a toggle button's two states).
    pub fn toggled(size: f32, filled: bool) -> IconStyle {
        IconStyle { size, filled, stroke_width: DEFAULT_STROKE_WIDTH, shadow_opacity: None }
    }

    /// The same style with another stroke width.
    pub fn with_stroke_width(mut self, stroke_width: f32) -> IconStyle {
        self.stroke_width = stroke_width;
        self
    }

    /// The same style with a drop shadow of the given opacity.
    pub fn with_shadow(mut self, opacity: f32) -> IconStyle {
        self.shadow_opacity = Some(opacity);
        self
    }
}

/// What identifies one rasterised texture.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct TextureKey {
    icon: Icon,
    /// Side in physical pixels.
    side_px: u32,
    filled: bool,
    /// Stroke width in hundredths of a view-box unit.
    stroke_centi: u32,
    /// The blurred shadow of the glyph instead of the glyph.
    shadow: bool,
}

/// A rasterised icon: the texture and how far it extends beyond the icon's own square (the
/// shadow texture has a margin for the blur).
#[derive(Clone)]
struct IconTexture {
    handle: TextureHandle,
    margin_px: u32,
}

/// The textures rasterised so far. A failed rasterisation is remembered as None so that it is
/// reported once and not retried every frame.
#[derive(Default)]
struct IconCache {
    textures: HashMap<TextureKey, Option<IconTexture>>,
}

/// The cache of the context. It lives in egui's temporary data, so it is dropped with the context.
fn cache(ctx: &egui::Context) -> Arc<Mutex<IconCache>> {
    let id = egui::Id::unique("instagram-windows-app/icon-cache");
    ctx.data_mut(|data| data.get_temp_mut_or_default::<Arc<Mutex<IconCache>>>(id).clone())
}

/// Rasterises one icon to a white-on-transparent bitmap of `side_px` pixels.
fn rasterise(icon: Icon, side_px: u32, filled: bool, stroke_width: f32) -> Result<ColorImage, String> {
    let svg = document(icon, filled, stroke_width);
    let hint = egui::load::SizeHint::Size { width: side_px, height: side_px, maintain_aspect_ratio: true };
    egui_extras::image::load_svg_bytes_with_size(svg.as_bytes(), hint, &Default::default())
}

/// The blur radius of the CSS drop shadow, in points; its Gaussian sigma is half of it.
const SHADOW_BLUR_POINTS: f32 = 3.0;
/// The shadow is offset down by this many points.
const SHADOW_OFFSET_Y_POINTS: f32 = 1.0;

/// Turns a glyph bitmap into its blurred silhouette (white, alpha = blurred coverage), with a
/// margin of `margin` pixels on every side so the blur is not cut off.
fn blurred_silhouette(glyph: &ColorImage, sigma_px: f32, margin: usize) -> ColorImage {
    let [width, height] = glyph.size;
    let (out_width, out_height) = (width + 2 * margin, height + 2 * margin);
    let mut alpha = vec![0.0_f32; out_width * out_height];
    for y in 0..height {
        for x in 0..width {
            alpha[(y + margin) * out_width + x + margin] = glyph.pixels[y * width + x].a() as f32 / 255.0;
        }
    }
    let radius = (sigma_px * 3.0).ceil().max(1.0) as isize;
    let kernel: Vec<f32> = (-radius..=radius).map(|offset| (-(offset as f32).powi(2) / (2.0 * sigma_px * sigma_px)).exp()).collect();
    let kernel_sum: f32 = kernel.iter().sum();
    let mut pass = vec![0.0_f32; alpha.len()];
    // Horizontal, then vertical: a Gaussian is separable.
    for y in 0..out_height {
        for x in 0..out_width {
            let mut sum = 0.0;
            for (index, weight) in kernel.iter().enumerate() {
                let sample_x = x as isize + index as isize - radius;
                if sample_x >= 0 && (sample_x as usize) < out_width {
                    sum += alpha[y * out_width + sample_x as usize] * weight;
                }
            }
            pass[y * out_width + x] = sum / kernel_sum;
        }
    }
    for y in 0..out_height {
        for x in 0..out_width {
            let mut sum = 0.0;
            for (index, weight) in kernel.iter().enumerate() {
                let sample_y = y as isize + index as isize - radius;
                if sample_y >= 0 && (sample_y as usize) < out_height {
                    sum += pass[sample_y as usize * out_width + x] * weight;
                }
            }
            alpha[y * out_width + x] = sum / kernel_sum;
        }
    }
    let pixels = alpha
        .iter()
        .map(|coverage| {
            let value = (coverage.clamp(0.0, 1.0) * 255.0).round() as u8;
            Color32::from_rgba_premultiplied(value, value, value, value)
        })
        .collect();
    ColorImage::new([out_width, out_height], pixels)
}

/// Looks a texture up, rasterising it on first use.
fn texture(ctx: &egui::Context, key: TextureKey) -> Option<IconTexture> {
    let cache = cache(ctx);
    let mut cache = match cache.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    if let Some(entry) = cache.textures.get(&key) {
        return entry.clone();
    }
    let stroke_width = key.stroke_centi as f32 / 100.0;
    let entry = match rasterise(key.icon, key.side_px, key.filled, stroke_width) {
        Ok(glyph) => {
            let (image, margin_px) = if key.shadow {
                let sigma_px = (SHADOW_BLUR_POINTS / 2.0) * ctx.pixels_per_point();
                let margin = (sigma_px * 3.0).ceil() as usize + 1;
                (blurred_silhouette(&glyph, sigma_px, margin), margin as u32)
            } else {
                (glyph, 0)
            };
            let name = format!("icon-{:?}-{}-{}-{}-{}", key.icon, key.side_px, key.filled, key.stroke_centi, key.shadow);
            Some(IconTexture { handle: ctx.load_texture(name, image, TextureOptions::LINEAR), margin_px })
        }
        Err(error) => {
            log::error!("The icon {:?} could not be rasterised: {error}", key.icon);
            None
        }
    };
    cache.textures.insert(key, entry.clone());
    entry
}

/// The rectangle of an icon of `size` points centred at `center`, moved onto the pixel grid so
/// the bitmap is not resampled.
pub fn icon_rect(ctx: &egui::Context, center: Pos2, size: f32) -> Rect {
    let pixels_per_point = ctx.pixels_per_point();
    let side_px = (size * pixels_per_point).round().max(1.0);
    let side = side_px / pixels_per_point;
    let min = center - vec2(side, side) / 2.0;
    let snapped = pos2((min.x * pixels_per_point).round() / pixels_per_point, (min.y * pixels_per_point).round() / pixels_per_point);
    Rect::from_min_size(snapped, vec2(side, side))
}

/// Paints `icon` centred at `center` in `color`.
pub fn paint(painter: &Painter, icon: Icon, center: Pos2, style: IconStyle, color: Color32) {
    let ctx = painter.ctx().clone();
    let pixels_per_point = ctx.pixels_per_point();
    let rect = icon_rect(&ctx, center, style.size);
    let side_px = (rect.width() * pixels_per_point).round().max(1.0) as u32;
    let key = TextureKey {
        icon,
        side_px,
        filled: style.filled || icon.always_filled(),
        stroke_centi: (style.stroke_width * 100.0).round() as u32,
        shadow: false,
    };
    let whole = Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0));
    if let Some(opacity) = style.shadow_opacity {
        if let Some(shadow) = texture(&ctx, TextureKey { shadow: true, ..key }) {
            let margin = shadow.margin_px as f32 / pixels_per_point;
            let shadow_rect = rect.expand(margin).translate(vec2(0.0, SHADOW_OFFSET_Y_POINTS));
            // The shadow follows the icon's own opacity, as a CSS filter does.
            let alpha = opacity * color.a() as f32 / 255.0;
            painter.image(shadow.handle.id(), shadow_rect, whole, Color32::from_black_alpha((alpha * 255.0).round() as u8));
        }
    }
    if let Some(glyph) = texture(&ctx, key) {
        // The verified badge keeps its own colours and only follows the opacity.
        let tint = if icon.is_coloured() { Color32::from_white_alpha(color.a()) } else { color };
        painter.image(glyph.handle.id(), rect, whole, tint);
    }
}

/// Paints an outline icon of `size` points with the default stroke.
pub fn paint_outline(painter: &Painter, icon: Icon, center: Pos2, size: f32, color: Color32) {
    paint(painter, icon, center, IconStyle::outline(size), color);
}

/// How much of the view box a stroke of `stroke_width` covers on screen, in points.
pub fn stroke_points(size: f32, stroke_width: f32) -> f32 {
    stroke_width * size / VIEW_BOX
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALL: [Icon; 39] = [
        Icon::Home,
        Icon::Search,
        Icon::Compass,
        Icon::Heart,
        Icon::Create,
        Icon::More,
        Icon::Settings,
        Icon::Bookmark,
        Icon::Comment,
        Icon::Share,
        Icon::Ellipsis,
        Icon::Emoji,
        Icon::ChevronLeft,
        Icon::ChevronRight,
        Icon::Close,
        Icon::Back,
        Icon::Sun,
        Icon::Moon,
        Icon::Grid,
        Icon::Clip,
        Icon::Carousel,
        Icon::Tag,
        Icon::Clock,
        Icon::Plus,
        Icon::Import,
        Icon::External,
        Icon::Folder,
        Icon::Archive,
        Icon::Link,
        Icon::Warning,
        Icon::Check,
        Icon::Play,
        Icon::Pause,
        Icon::Download,
        Icon::Info,
        Icon::Logo,
        Icon::Verified,
        Icon::Volume,
        Icon::VolumeOff,
    ];

    #[test]
    fn every_icon_rasterises_to_a_visible_glyph() {
        let covered = |icon: Icon, filled: bool| {
            let image = rasterise(icon, 48, filled, DEFAULT_STROKE_WIDTH).unwrap_or_else(|error| panic!("{icon:?}: {error}"));
            assert_eq!(image.size, [48, 48], "{icon:?}");
            image.pixels.iter().filter(|pixel| pixel.a() > 0).count()
        };
        for icon in ALL {
            let outline = covered(icon, false);
            assert!(outline > 20, "{icon:?} is empty ({outline} pixels)");
            assert!(outline < 48 * 48, "{icon:?} covers everything");
            // The filled variant must at least rasterise; a glyph of open lines has no area.
            assert!(covered(icon, true) < 48 * 48, "{icon:?} filled covers everything");
        }
        // The glyphs the app fills have an area to fill.
        for icon in [Icon::Home, Icon::Heart, Icon::Bookmark, Icon::Comment, Icon::Play, Icon::Ellipsis, Icon::Pause, Icon::Verified] {
            assert!(covered(icon, true) > 50, "{icon:?} filled is empty");
        }
    }

    #[test]
    fn a_filled_heart_covers_more_than_its_outline() {
        let count = |filled: bool| {
            let image = rasterise(Icon::Heart, 52, filled, DEFAULT_STROKE_WIDTH).expect("heart");
            image.pixels.iter().filter(|pixel| pixel.a() > 127).count()
        };
        assert!(count(true) > count(false) * 2);
    }

    #[test]
    fn the_filled_document_has_no_stroke() {
        let outline = document(Icon::Home, false, 1.9);
        assert!(outline.contains(r#"fill="none""#) && outline.contains(r#"stroke-width="1.9""#));
        let filled = document(Icon::Home, true, 1.9);
        assert!(filled.contains(r#"fill="white""#) && filled.contains(r#"stroke-width="0""#));
        // The ellipsis is filled even when an outline is asked for.
        assert!(document(Icon::Ellipsis, false, 1.9).contains(r#"stroke-width="0""#));
    }

    #[test]
    fn the_shadow_is_wider_and_softer_than_the_glyph() {
        let glyph = rasterise(Icon::Clip, 20, false, DEFAULT_STROKE_WIDTH).expect("clip");
        let shadow = blurred_silhouette(&glyph, 1.5, 6);
        assert_eq!(shadow.size, [32, 32]);
        let peak_glyph = glyph.pixels.iter().map(|pixel| pixel.a()).max().unwrap_or(0);
        let peak_shadow = shadow.pixels.iter().map(|pixel| pixel.a()).max().unwrap_or(0);
        assert!(peak_shadow < peak_glyph && peak_shadow > 20);
        let covered = |image: &ColorImage| image.pixels.iter().filter(|pixel| pixel.a() > 4).count();
        assert!(covered(&shadow) > covered(&glyph));
    }

    #[test]
    fn the_stroke_scales_with_the_icon() {
        assert!((stroke_points(26.0, DEFAULT_STROKE_WIDTH) - 2.058).abs() < 0.01);
        assert!((stroke_points(16.0, CAROUSEL_STROKE_WIDTH) - 1.733).abs() < 0.01);
    }
}
