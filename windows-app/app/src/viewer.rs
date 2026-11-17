//! The viewer: the reference web project's post detail (`PostDetail.tsx`, `styles.css:283-313`)
//! with what an image viewer needs on top of it.
//!
//! From the web project: a dark scrim over the window, a card with the picture contained on
//! black on the left and a 405 px column on the right (header, the whole description, the
//! comments of an export, the action row, likes, date), the carousel controls, the X at the top
//! right, and closing by a click on the scrim or Escape.
//!
//! New here, because the web project has none of it: round arrows and the Left / Right / Home /
//! End keys (and the back / forward buttons of a mouse) to move between pictures, the pictures
//! before and after decoded ahead, and zoom: the wheel around the pointer, dragging while
//! zoomed, a double click between fitted and 100 %, the keys `+`, `-` and `0`, and a small
//! percentage while the picture is not fitted. The zoom arithmetic lives in `zoom.rs`.
//!
//! The card grows with the window so that the picture gets the room (the web project caps it at
//! 1000 x 640 and shows "More posts" below, which this app does not have); see `layout`.
//!
//! A video slide plays as soon as it is on show, contained like a picture but never zoomed, with
//! a controls bar, a click to play or pause, a double click for fullscreen, and Space / K, M and
//! Shift+Left / Shift+Right (video brief 5.5; `video/stage.rs`).

use std::sync::Arc;

use egui::accesskit::Role;
use egui::epaint::text::{LayoutJob, TextWrapping};
use egui::{pos2, vec2, Align2, Color32, CornerRadius, CursorIcon, Galley, Id, Key, Modifiers, Painter, PointerButton, Pos2, Rect, Sense, Ui, UiBuilder, Vec2, WidgetInfo};
use instagram_core::model::{Collection, Post, PostKey};
use instagram_core::text;
use instagram_core::urls;

use crate::feed;
use crate::icons::{self, Icon, IconStyle};
use crate::loader::{ImageState, Loader, Picture, Priority, SizeState, Variant};
use crate::media::{self, CarouselChrome};
use crate::rendering;
use crate::state::{Action, Env, PostUi};
use crate::strings;
use crate::theme::{self, Palette};
use crate::video::stage::{self as video_stage, StageMemory, StageOptions};
use crate::video::{Surface, VideoKey};
use crate::widgets::{self, SpanLink};
use crate::zoom::{self, Stage, Zoom};

// --- Geometry (styles.css:283-313) ----------------------------------------------------------------

/// Opacity of the scrim behind the viewer (`.pd-overlay`).
const SCRIM_OPACITY: f32 = 0.78;
/// Space between the card and the top and bottom of the window (`padding: 40px 20px`).
const OVERLAY_PADDING_Y: f32 = 40.0;
/// Space between the card and the sides of a narrow window.
const OVERLAY_PADDING_X: f32 = 20.0;
/// The widest card of the web project (`max-width: 1000px`). In a wider window the card of this
/// app keeps growing.
const CLONE_CARD_MAX_WIDTH: f32 = 1000.0;
/// Space beside the card of a wide window: room for a round arrow with 14 px around it.
const GUTTER: f32 = 72.0;
/// Corner radius of the card.
const CARD_RADIUS: u8 = 4;
/// Width of the column beside the picture (`flex: 0 0 405px`).
pub const SIDE_WIDTH: f32 = 405.0;
/// In a window too narrow for both, the column gives way until the picture has this width...
const MIN_STAGE_WIDTH: f32 = 280.0;
/// ...but it never gets narrower than this.
const MIN_SIDE_WIDTH: f32 = 240.0;
/// The smallest card height (a window is at least 480 points high).
const MIN_CARD_HEIGHT: f32 = 200.0;
/// Diameter of the arrows to the previous and next picture.
const POST_ARROW_SIZE: f32 = 44.0;
/// The arrows stand beside the card when it leaves at least this much room; otherwise they
/// lie on the picture.
const POST_ARROW_GUTTER: f32 = POST_ARROW_SIZE + 16.0;
/// Distance of an arrow that lies on the picture from the side of the picture area.
const POST_ARROW_INSET: f32 = 10.0;
/// Side of the X at the top right (`.pd-close`: `top: 14px; right: 20px`, icon 30).
const CLOSE_SIZE: f32 = 30.0;

/// Horizontal padding of the column's rows.
const SIDE_PADDING_X: f32 = 16.0;
/// Height of the header: 14 px padding around the 40 px ringed avatar, and the border below.
const HEADER_HEIGHT: f32 = 69.0;
/// Size of the header avatar (40 px with its ring).
const HEADER_AVATAR_SIZE: f32 = 34.0;
/// Space between the parts of the header line (`gap: 6px`).
const HEADER_META_GAP: f32 = 6.0;
/// Height of the row under the header (`.pd-location`: 8 px padding around a 12 px line, border).
const LOCATION_HEIGHT: f32 = 33.0;
/// Padding of the scrolling text area (`.pd-comments`).
const TEXT_PADDING: f32 = 16.0;
/// Size of the avatar beside the description and the comments.
const ROW_AVATAR_SIZE: f32 = 32.0;
/// Space between that avatar and the text (`gap: 12px`).
const ROW_GAP: f32 = 12.0;
/// Space below a row (`margin-bottom: 18px`).
const ROW_MARGIN: f32 = 18.0;
/// Font size of the description and the comments.
const TEXT_SIZE: f32 = 14.0;
/// Their line height (`line-height: 1.5`).
const TEXT_LINE_HEIGHT: f32 = 21.0;
/// Font size of the line under a row (time, likes).
const SUB_SIZE: f32 = 12.0;
/// Its line height (1.5, inherited).
const SUB_LINE_HEIGHT: f32 = 18.0;
/// Space above it (`margin-top: 8px`).
const SUB_MARGIN: f32 = 8.0;
/// Space between its parts (`gap: 14px`).
const SUB_GAP: f32 = 14.0;
/// Height of the action row: border, 10 px padding around the 26 px icons.
const ACTIONS_HEIGHT: f32 = 47.0;
/// Side of the action icons.
const ACTION_ICON_SIZE: f32 = 26.0;
/// Space between the action icons on the left.
const ACTION_ICON_GAP: f32 = 12.0;
/// Height of the likes line (`padding: 2px 16px` around a 14 px line).
const LIKES_HEIGHT: f32 = 23.0;
/// Height of the date line (`padding: 6px 16px` around a 10 px line).
const DATE_HEIGHT: f32 = 26.0;
/// Space under the last line of the column. The web project has its comment field there.
const SIDE_PADDING_BOTTOM: f32 = 8.0;

/// A new decode size is asked for when the stage has kept its size for this long, so that
/// dragging the window border does not decode the picture once per frame.
const RESIZE_SETTLE_SECONDS: f64 = 0.15;
/// The spinner appears when the picture on show has not arrived after this long.
const SPINNER_DELAY_SECONDS: f64 = 0.25;
/// The full-size rendition is asked for when the zoom needs this much more than the fitted one has.
const FULL_SIZE_THRESHOLD: f32 = 1.02;

/// Where the parts of the viewer are in a window.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ViewerLayout {
    /// The card.
    pub card: Rect,
    /// The picture area: the card without the column.
    pub stage: Rect,
    /// The column with header, description and actions.
    pub side: Rect,
    /// Centre of the arrow to the previous picture.
    pub previous: Pos2,
    /// Centre of the arrow to the next picture.
    pub next: Pos2,
    /// The arrows lie on the picture because there is no room beside the card.
    pub arrows_on_stage: bool,
    /// Centre of the X.
    pub close: Pos2,
}

/// The geometry of the viewer in a window of `screen`.
///
/// The rule: the card is as high as the window minus the web project's 40 px above and below.
/// Its width is the web project's (the window minus 20 px on each side, at most 1000 px) until
/// the window is wide enough for more: then it is the window minus a 72 px gutter on each side,
/// in which the arrows to the previous and next picture stand. The column keeps its 405 px; only
/// in a window narrower than 725 px does it give way so that the picture keeps 280 px. While
/// the gutter is too small for them, the arrows lie on the left and right edge of the picture.
pub fn layout(screen: Rect) -> ViewerLayout {
    let clone_width = CLONE_CARD_MAX_WIDTH.min(screen.width() - 2.0 * OVERLAY_PADDING_X);
    let width = clone_width.max(screen.width() - 2.0 * GUTTER).max(MIN_SIDE_WIDTH);
    let height = (screen.height() - 2.0 * OVERLAY_PADDING_Y).max(MIN_CARD_HEIGHT);
    let left = (screen.center().x - width / 2.0).round();
    let card = Rect::from_min_size(pos2(left, screen.top() + OVERLAY_PADDING_Y), vec2(width.round(), height.round()));
    let side_width = SIDE_WIDTH.min((card.width() - MIN_STAGE_WIDTH).max(MIN_SIDE_WIDTH)).min(card.width());
    let side = Rect::from_min_max(pos2(card.right() - side_width, card.top()), card.max);
    let stage = Rect::from_min_max(card.min, pos2(side.left(), card.bottom()));

    let gutter = card.left() - screen.left();
    let arrows_on_stage = gutter < POST_ARROW_GUTTER;
    let center_y = card.center().y.round();
    let (previous_x, next_x) = if arrows_on_stage {
        (stage.left() + POST_ARROW_INSET + POST_ARROW_SIZE / 2.0, stage.right() - POST_ARROW_INSET - POST_ARROW_SIZE / 2.0)
    } else {
        ((screen.left() + gutter / 2.0).round(), (screen.right() - gutter / 2.0).round())
    };
    ViewerLayout {
        card,
        stage,
        side,
        previous: pos2(previous_x, center_y),
        next: pos2(next_x, center_y),
        arrows_on_stage,
        close: pos2(screen.right() - 20.0 - CLOSE_SIZE / 2.0, screen.top() + 14.0 + CLOSE_SIZE / 2.0),
    }
}

/// The part of the card that is opaque for certain: the card without its rounded corners and
/// the soft edge around them. The picture area is black and the column has the surface colour,
/// both opaque, so nothing below the card shows there.
pub fn card_interior(layout: &ViewerLayout) -> Rect {
    layout.card.shrink(f32::from(CARD_RADIUS))
}

// --- Pictures for the stage -----------------------------------------------------------------------

/// What the stage shows for one slide. Auto view shows its pictures the same way.
pub(crate) enum Contained {
    /// A picture: the texture, the size of the file, and whether it is the wanted rendition.
    Picture { picture: Picture, source: (u32, u32), exact: bool },
    /// Nothing yet. The size of the file may be known already.
    Loading { source: Option<(u32, u32)> },
    /// The picture cannot be shown; the sentence says why.
    Failed(String),
    /// A video without a cover picture.
    NoPicture,
}

/// Asks the loader for the picture of `media_path` contained in a square of `side_px` pixels.
/// While it is decoded, any other rendition of the file stands in.
fn contained_picture(loader: &mut Loader, path: &std::path::Path, source: (u32, u32), side_px: u32, priority: Priority) -> Contained {
    match loader.image(path, Variant::Contain { max_side_px: side_px }, priority) {
        ImageState::Ready(picture) => Contained::Picture { picture, source, exact: true },
        ImageState::Failed(message) => Contained::Failed(message),
        ImageState::Loading => match loader.any_rendition(path) {
            Some(picture) => Contained::Picture { picture, source, exact: false },
            None => Contained::Loading { source: Some(source) },
        },
    }
}

/// What to show of a picture whose size is still being read. For the one picture the user is
/// looking at (`Priority::focus`: a picture opened by itself, the first of a show) the decoder
/// is asked at once for a rendition that fits the longer side of the stage: it arrives together
/// with the size and is on screen a frame earlier than the exactly fitted rendition, which is
/// asked for as soon as the size is known. Every other picture waits for its size.
fn first_look(loader: &mut Loader, path: &std::path::Path, stage_size: Vec2, pixels_per_point: f32, priority: Priority) -> Contained {
    if priority != Priority::focus() {
        return Contained::Loading { source: None };
    }
    let side_px = (stage_size.max_elem() * pixels_per_point).ceil().max(1.0) as u32;
    match loader.image(path, Variant::Contain { max_side_px: side_px }, priority) {
        ImageState::Ready(picture) => {
            let source = picture.source_size;
            Contained::Picture { picture, source, exact: false }
        }
        ImageState::Failed(message) => Contained::Failed(message),
        ImageState::Loading => Contained::Loading { source: None },
    }
}

/// Asks the loader for slide `slide` of `post`, decoded to fit a stage of `stage_size` points
/// without zoom: what auto view shows, and what it decodes ahead.
pub(crate) fn fitted_picture(loader: &mut Loader, post: &Post, slide: usize, stage_size: Vec2, pixels_per_point: f32, priority: Priority) -> Contained {
    let path = match slide_file(loader, post, slide, priority) {
        Ok(path) => path,
        Err(contained) => return contained,
    };
    let path = path.as_path();
    let source = match loader.size_of(path, priority) {
        SizeState::Known(size) => size,
        SizeState::Pending => return first_look(loader, path, stage_size, pixels_per_point, priority),
        SizeState::Failed(message) => return Contained::Failed(message),
    };
    let stage = Stage { size: stage_size, source: vec2(source.0 as f32, source.1 as f32), pixels_per_point };
    contained_picture(loader, path, source, zoom::decode_side(source, stage.fit_scale()), priority)
}

/// The picture file of slide `slide` (the poster of a video, made when it has none), or what to
/// show instead.
fn slide_file(loader: &mut Loader, post: &Post, slide: usize, priority: Priority) -> Result<std::path::PathBuf, Contained> {
    let Some(media) = post.media.get(slide) else {
        return Err(Contained::NoPicture);
    };
    match media::display_file(loader, media, priority) {
        media::Display::File(path) => Ok(path),
        media::Display::Pending => Err(Contained::Loading { source: None }),
        media::Display::Nothing => Err(Contained::NoPicture),
    }
}

/// The side a fitted picture is decoded to, held back while the stage is being resized.
#[derive(Debug, Clone, Copy, PartialEq)]
struct FitDecode {
    /// The side in use.
    side: u32,
    /// The side the stage asks for now.
    wanted: u32,
    /// Since when it asks for it, in seconds of the app clock.
    since: f64,
}

// --- The text column --------------------------------------------------------------------------------

/// One row of the text area: the description, or a comment.
struct TextRow {
    username: String,
    galley: Arc<Galley>,
    /// The whole text of the row, for screen readers and tests.
    text: String,
    links: Vec<SpanLink>,
    /// The `char` range of the space the verified badge is painted after.
    badge_after: Option<std::ops::Range<usize>>,
    /// The parts of the muted line below: the time, the likes.
    sub: Vec<String>,
    /// The picture of the avatar; the placeholder of the username without one.
    avatar: Option<Picture>,
}

impl TextRow {
    fn height(&self) -> f32 {
        let text_height = self.galley.size().y + if self.sub.is_empty() { 0.0 } else { SUB_MARGIN + SUB_LINE_HEIGHT };
        text_height.max(ROW_AVATAR_SIZE)
    }
}

/// What a row says, before it is laid out.
struct RowContent<'a> {
    username: &'a str,
    verified: bool,
    /// The description or the comment.
    body: &'a str,
    /// The parts of the muted line below.
    sub: Vec<String>,
    avatar: Option<Picture>,
}

/// Lays out one row: bold username, the verified badge, then the text with its links.
fn text_row(ctx: &egui::Context, content: RowContent<'_>, width: f32, palette: &Palette) -> TextRow {
    let RowContent { username, verified, body, sub, avatar } = content;
    let bold = widgets::format(theme::semibold(TEXT_SIZE), palette.text, TEXT_LINE_HEIGHT);
    let regular = widgets::format(theme::regular(TEXT_SIZE), palette.text, TEXT_LINE_HEIGHT);
    let mut job = LayoutJob { wrap: TextWrapping { max_width: width, ..TextWrapping::default() }, ..LayoutJob::default() };
    let mut links = Vec::new();
    job.append(username, 0.0, bold);
    let username_chars = job.text.chars().count();
    job.append(" ", 0.0, regular.clone());
    let mut badge_after = None;
    if verified {
        // The badge is painted into the gap `leading_space` leaves after the first space.
        badge_after = Some(username_chars..username_chars + 1);
        job.append(" ", 12.0, regular);
    }
    widgets::append_rich(&mut job, body, TEXT_SIZE, TEXT_LINE_HEIGHT, palette, &mut links);
    let text = job.text.clone();
    TextRow { username: username.to_owned(), galley: widgets::layout_job(ctx, job), text, links, badge_after, sub, avatar }
}

// --- The view ---------------------------------------------------------------------------------------

/// The viewer: what it remembers between frames is how the picture on show is zoomed.
#[derive(Default)]
pub struct ViewerView {
    /// The post and the slide the zoom belongs to; another picture starts fitted.
    shown: Option<(PostKey, usize)>,
    zoom: Zoom,
    fit_decode: Option<FitDecode>,
    /// The stage of the picture on show, when its size is known: what the zoom works on.
    stage: Option<Stage>,
    /// A zoom asked for from outside the view (the harness, tests), applied in the next frame:
    /// a scale, or None for the fitted picture.
    zoom_request: Option<Option<f32>>,
    /// The comment button was pressed: scroll the text area to the first comment.
    scroll_to_comments: bool,
    /// The stage changed its size in this frame and the decode size has not followed yet.
    resize_pending: bool,
    /// Since when nothing of the picture on show could be painted, in seconds of the app clock.
    loading_since: Option<f64>,
    /// False while something is still changing (the screenshot harness waits for it).
    settled: bool,
    /// The text column of the last frame: what it took, and the area it was given.
    text_column: Option<(Rect, Rect)>,
    /// When the controls bar of a video was last called for.
    video_memory: StageMemory,
    /// Where the card of this frame covers what lies below it with opaque colour.
    opaque: Option<Rect>,
}

impl ViewerView {
    /// How the picture on show is zoomed.
    pub fn zoom(&self) -> Zoom {
        self.zoom
    }

    /// The zoom as a percentage of the picture's real size, once the size of the picture is
    /// known: 100 means one picture pixel per screen pixel.
    pub fn zoom_percent(&self) -> Option<u32> {
        self.stage.as_ref().map(|stage| self.zoom.percent(stage))
    }

    /// Zooms to `scale` (1.0 is 100 %) around the centre at the next frame; None fits the picture.
    pub fn request_zoom(&mut self, scale: Option<f32>) {
        self.zoom_request = Some(scale);
    }

    /// Forgets the picture that was on show and its zoom: the viewer was closed, and a picture
    /// opened in it again starts fitted, whichever it is.
    pub fn forget_picture(&mut self) {
        if self.shown.is_some() {
            self.shown = None;
            self.zoom = Zoom::FIT;
            self.fit_decode = None;
            self.stage = None;
            self.loading_since = None;
        }
    }

    /// True when the last frame changed nothing: the zoom is what was painted, no zoom asked
    /// for from outside is waiting, and the picture is decoded for the size the stage has.
    pub fn is_settled(&self) -> bool {
        self.settled && self.zoom_request.is_none() && !self.scroll_to_comments
    }

    /// Where the viewer painted in this frame covers everything below it with opaque colour, in
    /// screen points; None when it painted nothing. What lies below need not be drawn there
    /// (`rendering::keep_out`).
    pub fn opaque_area(&self) -> Option<Rect> {
        self.opaque
    }

    /// The side the fitted picture is decoded to: `wanted`, but only once the stage has asked
    /// for the same side for a moment.
    fn debounced_side(&mut self, wanted: u32, time: f64, animations: bool) -> (u32, bool) {
        let fit = self.fit_decode.get_or_insert(FitDecode { side: wanted, wanted, since: time });
        if fit.wanted != wanted {
            fit.wanted = wanted;
            fit.since = time;
        }
        if fit.side != fit.wanted && (!animations || time - fit.since >= RESIZE_SETTLE_SECONDS) {
            fit.side = fit.wanted;
        }
        (fit.side, fit.side == fit.wanted)
    }

    /// Shows the viewer over the whole window. Call it only while `state.viewer` is set.
    pub fn show(&mut self, ctx: &egui::Context, env: &mut Env<'_>) {
        self.opaque = None;
        let state = env.state;
        let (Some(viewer), Some(collection)) = (&state.viewer, &state.collection) else {
            return;
        };
        let Some(index) = collection.index_of(&viewer.key) else {
            // The post is gone (it cannot be while the collection stays; be safe).
            env.push(Action::CloseViewer);
            return;
        };
        let post = &collection.posts[index];
        let post_ui = state.post_ui(&post.key);
        let palette = state.palette();
        let screen = ctx.content_rect();
        let layout = layout(screen);
        let id = Id::unique("viewer");
        // A dialog above the viewer has the keyboard.
        let has_keyboard = !state.modal_open();
        self.settled = true;

        let area = egui::Modal::default_area(id).anchor(Align2::LEFT_TOP, Vec2::ZERO);
        // The scrim is painted here rather than as the modal's backdrop, so that it can be kept
        // out of the card below (`rendering::keep_out`).
        let mut scrim = None;
        egui::Modal::new(id).area(area).backdrop_color(Color32::TRANSPARENT).frame(egui::Frame::NONE).show(ctx, |ui| {
            ui.set_min_size(screen.size());
            ui.set_clip_rect(screen);
            scrim = Some((ui.layer_id(), ui.painter().rect_filled(screen, 0.0, theme::rgba(0, 0, 0, SCRIM_OPACITY))));
            // The dialog itself is what a screen reader announces.
            ui.response().widget_info(|| WidgetInfo::labeled(Role::Dialog, true, strings::viewer_label(&post.user.username)));

            // A click beside the card closes the viewer; the card itself swallows clicks.
            if ui.interact(screen, id.with("scrim"), Sense::CLICK).clicked() {
                env.push(Action::CloseViewer);
            }
            ui.interact(layout.card, id.with("card"), Sense::CLICK);
            let painter = ui.painter();
            let left_corners = CornerRadius { nw: CARD_RADIUS, sw: CARD_RADIUS, ne: 0, se: 0 };
            let right_corners = CornerRadius { nw: 0, sw: 0, ne: CARD_RADIUS, se: CARD_RADIUS };
            painter.rect_filled(layout.stage, left_corners, theme::MEDIA_BLACK);
            painter.rect_filled(layout.side, right_corners, palette.surface);

            self.show_stage(ui, id, &layout, post, &post_ui, has_keyboard, env);
            self.show_side(ui, id, layout.side, post, &post_ui, env);
            self.show_navigation(ui, id, &layout, collection, index, post, &post_ui, has_keyboard, env);

            // The X, white on the scrim.
            if widgets::icon_button(ui, layout.close, id.with("close"), Icon::Close, IconStyle::outline(CLOSE_SIZE), Color32::WHITE, strings::VIEWER_CLOSE).clicked() {
                env.push(Action::CloseViewer);
            }
        });
        // The card is opaque: the scrim is not drawn under it, and the app keeps the feed out
        // of it too (`opaque_area`).
        let hole = card_interior(&layout);
        if let Some((layer, scrim)) = scrim {
            rendering::keep_out(ctx, layer, scrim.0..scrim.0 + 1, hole);
        }
        self.opaque = Some(hole);

        if has_keyboard && ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
            env.push(Action::ViewerEscape);
        }
        self.decode_neighbours(ctx, &layout, collection, index, env);
        if std::mem::take(&mut self.resize_pending) {
            // The stage is being resized: look again once it has kept its size for a moment.
            self.settled = false;
            ctx.request_repaint_after_secs(RESIZE_SETTLE_SECONDS as f32);
        }
    }

    /// The picture area: the slides on their track, zoom and pan of the one on show, and the
    /// carousel controls.
    #[allow(clippy::too_many_arguments)]
    fn show_stage(&mut self, ui: &Ui, id: Id, layout: &ViewerLayout, post: &Post, post_ui: &PostUi, has_keyboard: bool, env: &mut Env<'_>) {
        let ctx = ui.ctx().clone();
        let state = env.state;
        let stage_rect = layout.stage;
        let scale = ctx.pixels_per_point();
        let slide_count = post.media.len().max(1);
        let current = post_ui.slide.min(slide_count - 1);
        // A video slide is never zoomed (video brief 5.5, Q7).
        let current_is_video = media::is_video_slide(post, current);

        // Another picture starts fitted.
        let shown = (post.key.clone(), current);
        if self.shown.as_ref() != Some(&shown) {
            self.shown = Some(shown);
            self.zoom = Zoom::FIT;
            self.fit_decode = None;
            self.stage = None;
            self.loading_since = None;
        }

        let clipped = ui.painter().with_clip_rect(stage_rect.intersect(ui.clip_rect()));
        let position = post_ui.track_position(state.time, state.animations);
        // A video that cannot be played shows its failure panel alone: the clapper of a video
        // without a poster would stand out above and below the translucent panel.
        let current_video_failed = current_is_video && env.video.shown(Surface::Viewer, &VideoKey::new(&post.key, current)).is_some_and(|shown| shown.has_failed());
        let mut failure: Option<String> = None;
        let mut current_source: Option<(u32, u32)> = None;
        // The size of the picture on show is still being read: a zoom asked for can wait.
        let mut size_pending = false;
        // Nothing of the picture on show can be painted yet.
        let mut loading = false;
        for slide in 0..slide_count {
            let offset = (slide as f32 - position) * stage_rect.width();
            let on_stage = offset.abs() < stage_rect.width();
            let is_current = slide == current;
            if !on_stage && slide.abs_diff(current) != 1 {
                continue;
            }
            // The slide on show first, its neighbours decoded ahead (Media.tsx:74-81).
            let priority = if is_current { Priority::focus() } else if on_stage { Priority::visible(0) } else { Priority::near(0) };
            let rect = stage_rect.translate(vec2(widgets::snap(&ctx, pos2(offset, 0.0)).x, 0.0));
            let zoomed = is_current && offset == 0.0 && !current_is_video;
            let contained = self.slide_picture(env.loader, post, slide, stage_rect.size(), scale, priority, zoomed, state.time, state.animations);
            if !on_stage {
                continue;
            }
            if is_current {
                loading = matches!(contained, Contained::Loading { .. });
                match &contained {
                    Contained::Picture { source, .. } | Contained::Loading { source: Some(source) } => current_source = Some(*source),
                    Contained::Failed(message) => failure = Some(message.clone()),
                    Contained::Loading { source: None } => size_pending = true,
                    Contained::NoPicture => {}
                }
            }
            let zoom = if zoomed { self.zoom } else { Zoom::FIT };
            if !(is_current && current_video_failed && matches!(contained, Contained::NoPicture | Contained::Failed(_))) {
                paint_contained(&clipped, rect, &contained, zoom, scale, slide_count > 1);
            }
        }
        if post_ui.is_sliding(state.time, state.animations) {
            ctx.request_repaint();
        }
        // A picture that takes long to decode: the spinner, but not for the blink of an eye a
        // small picture needs.
        if loading {
            let since = *self.loading_since.get_or_insert(state.time);
            let waited = state.time - since;
            if waited >= SPINNER_DELAY_SECONDS {
                widgets::spinner(&clipped, widgets::snap(&ctx, stage_rect.center()), 32.0, 3.0, state);
            } else {
                ctx.request_repaint_after_secs((SPINNER_DELAY_SECONDS - waited) as f32);
            }
        } else {
            self.loading_since = None;
        }

        // The stage itself: named after the picture; it takes the wheel, drags and double clicks.
        let label = media::slide_label(post, current, failure.as_deref());
        let response = widgets::picture_area_draggable(ui, stage_rect, id.with("stage"), &label);
        self.stage = current_source.filter(|_| !current_is_video).map(|source| Stage { size: stage_rect.size(), source: vec2(source.0 as f32, source.1 as f32), pixels_per_point: scale });
        if current_is_video {
            // The video over its poster, the controls bar, the keys of a video slide. A click
            // plays or pauses, a double click is F11's action.
            self.zoom = Zoom::FIT;
            self.zoom_request = None;
            let options = StageOptions { surface: Surface::Viewer, visit: 0, has_keyboard };
            let on_stage = post_ui.track_position(state.time, state.animations) == current as f32;
            if on_stage {
                video_stage::show(ui, stage_rect, id.with("video"), post, current, current_source, options, &mut self.video_memory, env);
                if response.double_clicked() {
                    env.push(Action::ToggleFullscreen);
                } else if response.clicked() {
                    env.push(Action::VideoTogglePlay(Surface::Viewer));
                }
            }
        } else if let Some(stage) = self.stage {
            let before = self.zoom;
            let center = stage_rect.center();
            if let Some(request) = self.zoom_request.take() {
                self.zoom = match request {
                    Some(wanted) => self.zoom.zoomed_about(&stage, wanted, Vec2::ZERO),
                    None => Zoom::FIT,
                };
            }
            if let Some(pointer) = response.hover_pos() {
                // The wheel (and pinching, which egui reports as a zoom factor) around the pointer.
                let (scrolled, pinched) = ctx.input_mut(|input| (std::mem::take(&mut input.smooth_scroll_delta).y, input.zoom_delta()));
                // One notch zooms one step, however far a notch scrolls in the mouse settings.
                let factor = pinched * zoom::WHEEL_NOTCH_STEP.powf(scrolled / theme::wheel_notch_points(&ctx));
                if (factor - 1.0).abs() > f32::EPSILON {
                    self.zoom = self.zoom.zoomed_by(&stage, factor, pointer - center);
                }
            }
            if response.double_clicked() {
                let anchor = response.interact_pointer_pos().map_or(Vec2::ZERO, |pointer| pointer - center);
                self.zoom = self.zoom.toggled(&stage, anchor);
            }
            if response.dragged() {
                self.zoom = self.zoom.panned(&stage, response.drag_delta());
            }
            if has_keyboard {
                let pressed = |key: Key| ctx.input_mut(|input| input.consume_key(Modifiers::NONE, key));
                if pressed(Key::Plus) || pressed(Key::Equals) {
                    self.zoom = self.zoom.stepped(&stage, 1);
                } else if pressed(Key::Minus) {
                    self.zoom = self.zoom.stepped(&stage, -1);
                } else if pressed(Key::Num0) {
                    self.zoom = Zoom::FIT;
                }
            }
            // The window may have changed its size since the last frame.
            self.zoom = self.zoom.clamped(&stage);
            if self.zoom != before {
                // The picture was painted with the old zoom: paint again.
                self.settled = false;
                ctx.request_repaint();
            }
            if self.zoom.can_pan(&stage) {
                if response.dragged() {
                    ctx.set_cursor_icon(CursorIcon::Grabbing);
                } else if response.hovered() {
                    ctx.set_cursor_icon(CursorIcon::Grab);
                }
            }
        } else if !size_pending {
            // Nothing to zoom: the picture cannot be shown.
            self.zoom_request = None;
        }

        let painter = ui.painter();
        if post.is_video() || current_is_video {
            media::video_badge(ui, stage_rect, id.with("video-badge"));
        }

        // The zoom indicator, bottom left, only while the picture is not fitted.
        if let (Some(stage), false) = (&self.stage, self.zoom.is_fit()) {
            let text = strings::zoom_percent(self.zoom.percent(stage));
            let galley = widgets::line(&ctx, &text, theme::regular(12.0), Color32::WHITE, 14.4);
            let size = galley.size() + vec2(20.0, 10.0);
            let pill = Rect::from_min_size(widgets::snap(&ctx, pos2(stage_rect.left() + 12.0, stage_rect.bottom() - 12.0 - size.y)), size);
            painter.rect_filled(pill, size.y / 2.0, media::pill_fill());
            widgets::paint_galley(painter, pill.min + vec2(10.0, 5.0), galley);
            widgets::text_label(ui, pill, id.with("zoom"), &text);
        }

        // The carousel controls; its arrows move inwards when the viewer's own lie on the picture.
        let arrow_inset = if layout.arrows_on_stage { POST_ARROW_INSET + POST_ARROW_SIZE + 8.0 } else { media::CAROUSEL_ARROW_INSET };
        // The dots give way to the controls bar on a video slide (`styles.css:517`).
        let chrome = CarouselChrome { frame: stage_rect, current, count: slide_count, arrow_inset, hide_dots: current_is_video };
        if let Some(slide) = media::carousel_chrome(ui, id.with("carousel"), chrome) {
            env.push(Action::ShowSlide(post.key.clone(), slide));
        }
    }

    /// Asks the loader for slide `slide`, contained on a stage of `stage_size` points. The slide
    /// on show (`zoomed`) gets the full-size rendition once its zoom needs more pixels than the
    /// fitted one has.
    #[allow(clippy::too_many_arguments)]
    fn slide_picture(&mut self, loader: &mut Loader, post: &Post, slide: usize, stage_size: Vec2, pixels_per_point: f32, priority: Priority, zoomed: bool, time: f64, animations: bool) -> Contained {
        let path = match slide_file(loader, post, slide, priority) {
            Ok(path) => path,
            Err(contained) => return contained,
        };
        let path = path.as_path();
        let source = match loader.size_of(path, priority) {
            SizeState::Known(size) => size,
            SizeState::Pending => return first_look(loader, path, stage_size, pixels_per_point, priority),
            SizeState::Failed(message) => return Contained::Failed(message),
        };
        let stage = Stage { size: stage_size, source: vec2(source.0 as f32, source.1 as f32), pixels_per_point };
        let fit_side = zoom::decode_side(source, stage.fit_scale());
        if !zoomed {
            return contained_picture(loader, path, source, fit_side, priority);
        }
        let (fit_side, stable) = self.debounced_side(fit_side, time, animations);
        if !stable {
            self.resize_pending = true;
        }
        let full_side = zoom::decode_side(source, 1.0);
        let fitted_scale = fit_side as f32 / full_side as f32;
        let needs_full_size = full_side > fit_side && self.zoom.scale_on(&stage).min(1.0) > fitted_scale * FULL_SIZE_THRESHOLD;
        let fitted = contained_picture(loader, path, source, fit_side, priority);
        if !needs_full_size {
            return fitted;
        }
        // Zoomed in beyond what the fitted texture holds: the whole file (as far as a texture
        // can be large). The fitted rendition stays on screen until it has arrived.
        match loader.image(path, Variant::Contain { max_side_px: full_side }, priority) {
            ImageState::Ready(picture) => Contained::Picture { picture, source, exact: true },
            ImageState::Failed(_) => fitted,
            ImageState::Loading => match fitted {
                Contained::Picture { picture, source, .. } => Contained::Picture { picture, source, exact: false },
                other => other,
            },
        }
    }

    /// Decodes the pictures before and after the one on show ahead of time, for the size the
    /// stage has now.
    fn decode_neighbours(&self, ctx: &egui::Context, layout: &ViewerLayout, collection: &Collection, index: usize, env: &mut Env<'_>) {
        let neighbours = [index.checked_add(1), index.checked_sub(1)];
        for (rank, neighbour) in neighbours.into_iter().enumerate() {
            let Some(post) = neighbour.and_then(|neighbour| collection.posts.get(neighbour)) else {
                continue;
            };
            let slide = env.state.shown_slide(post);
            let priority = Priority::near(rank + 1);
            let Ok(path) = slide_file(env.loader, post, slide, priority) else {
                continue;
            };
            if let SizeState::Known(source) = env.loader.size_of(&path, priority) {
                let stage = Stage { size: layout.stage.size(), source: vec2(source.0 as f32, source.1 as f32), pixels_per_point: ctx.pixels_per_point() };
                let _ = env.loader.image(&path, Variant::Contain { max_side_px: zoom::decode_side(source, stage.fit_scale()) }, priority);
            }
        }
    }

    /// The arrows to the previous and next picture, and the keys and mouse buttons that do the
    /// same: Left / Right page a carousel first, Home / End go to the first and last picture.
    #[allow(clippy::too_many_arguments)]
    fn show_navigation(&mut self, ui: &Ui, id: Id, layout: &ViewerLayout, collection: &Collection, index: usize, post: &Post, post_ui: &PostUi, has_keyboard: bool, env: &mut Env<'_>) {
        let ctx = ui.ctx().clone();
        let posts = collection.posts.as_slice();
        let slide_count = post.media.len().max(1);
        let current = post_ui.slide.min(slide_count - 1);
        let previous = index.checked_sub(1).and_then(|previous| posts.get(previous));
        let next = posts.get(index + 1);

        let arrows = [(previous, layout.previous, Icon::ChevronLeft, strings::VIEWER_PREVIOUS, "previous"), (next, layout.next, Icon::ChevronRight, strings::VIEWER_NEXT, "next")];
        for (target, center, icon, label, salt) in arrows {
            // No arrow before the first or after the last picture, as on a carousel.
            let Some(target) = target else {
                continue;
            };
            let rect = Rect::from_center_size(center, Vec2::splat(POST_ARROW_SIZE));
            let arrow_id = id.with(salt);
            let response = widgets::hit(ui, rect, arrow_id, label).on_hover_text(label);
            let hover = widgets::transition(ui, arrow_id.with("hover"), response.hovered(), 0.15);
            let painter = ui.painter();
            // On the scrim the arrow is the auto view's (a white chevron, a faint disc under the
            // pointer); on a picture it needs a dark disc to stay visible.
            let fill = if layout.arrows_on_stage { theme::rgba(18, 18, 18, 0.5 + 0.22 * hover) } else { theme::rgba(255, 255, 255, 0.14 * hover) };
            painter.circle_filled(center, POST_ARROW_SIZE / 2.0, fill);
            icons::paint(painter, icon, center, IconStyle::outline(24.0).with_stroke_width(2.2), Color32::WHITE);
            widgets::focus_ring(ui, &response, rect, POST_ARROW_SIZE / 2.0, theme::FOCUS_RING_OFFSET);
            if response.clicked() {
                env.push(Action::OpenViewer(target.key.clone()));
            }
        }

        if !has_keyboard {
            return;
        }
        // The back and forward buttons of a mouse.
        let (back, forward) = ctx.input(|input| (input.pointer.button_pressed(PointerButton::Extra1), input.pointer.button_pressed(PointerButton::Extra2)));
        if let (true, Some(target)) = (back, previous) {
            env.push(Action::OpenViewer(target.key.clone()));
        } else if let (true, Some(target)) = (forward, next) {
            env.push(Action::OpenViewer(target.key.clone()));
        }
        // With a focused button the arrow keys move the focus, as everywhere in the app.
        if ctx.memory(|memory| memory.focused().is_some()) {
            return;
        }
        let pressed = |key: Key| ctx.input_mut(|input| input.consume_key(Modifiers::NONE, key));
        if pressed(Key::ArrowRight) {
            if current + 1 < slide_count {
                env.push(Action::ShowSlide(post.key.clone(), current + 1));
            } else if let Some(target) = next {
                env.push(Action::OpenViewer(target.key.clone()));
            }
        } else if pressed(Key::ArrowLeft) {
            if current > 0 {
                env.push(Action::ShowSlide(post.key.clone(), current - 1));
            } else if let Some(target) = previous {
                env.push(Action::OpenViewer(target.key.clone()));
            }
        } else if pressed(Key::Home) {
            if let Some(first) = posts.first().filter(|_| index > 0) {
                env.push(Action::OpenViewer(first.key.clone()));
            }
        } else if pressed(Key::End) {
            if let Some(last) = posts.last().filter(|_| index + 1 < posts.len()) {
                env.push(Action::OpenViewer(last.key.clone()));
            }
        }
    }

    /// The column beside the picture: header, the row under it, the scrolling text, the action
    /// row, likes and date.
    fn show_side(&mut self, ui: &mut Ui, id: Id, side: Rect, post: &Post, post_ui: &PostUi, env: &mut Env<'_>) {
        let ctx = ui.ctx().clone();
        let state = env.state;
        let palette = state.palette();
        let now_ms = text::now_ms();
        let site = post.permalink.as_deref().map(urls::site_label);
        let time_label = post.taken_at.map(|taken_at| text::relative_time_label(now_ms, taken_at));
        let divider = |painter: &Painter, y: f32| painter.rect_filled(Rect::from_min_size(pos2(side.left(), y), vec2(side.width(), 1.0)), 0.0, palette.border);

        // --- Header ---
        let painter = ui.painter();
        let header_center_y = side.top() + (HEADER_HEIGHT - 1.0) / 2.0;
        divider(painter, side.top() + HEADER_HEIGHT - 1.0);
        let avatar_outer = widgets::avatar_outer_size(HEADER_AVATAR_SIZE, true);
        let avatar_center = pos2(side.left() + SIDE_PADDING_X + avatar_outer / 2.0, header_center_y);
        let header_avatar = feed::avatar_picture(env.loader, &ctx, post, HEADER_AVATAR_SIZE, Priority::visible(1));
        widgets::avatar(painter, avatar_center, HEADER_AVATAR_SIZE, true, &post.user.username, header_avatar.as_ref(), palette.surface);

        let mut right = side.right() - SIDE_PADDING_X;
        if widgets::icon_button(ui, pos2(right - 12.0, header_center_y), id.with("more-options"), Icon::Ellipsis, IconStyle::filled(24.0), palette.text, strings::POST_MORE_OPTIONS).clicked() {
            env.push(Action::OpenPostOptions(post.key.clone()));
        }
        right -= 24.0 + 10.0;
        if let (Some(link), Some(site)) = (&post.permalink, &site) {
            let label = strings::view_on(site);
            if widgets::icon_button(ui, pos2(right - 10.0, header_center_y), id.with("external"), Icon::External, IconStyle::outline(20.0), palette.text, &label).clicked() {
                env.push(Action::OpenLink(link.clone()));
            }
            right -= 20.0 + 10.0;
        }

        let meta_left = side.left() + SIDE_PADDING_X + avatar_outer + 10.0;
        let line_height = theme::normal_line_height(TEXT_SIZE);
        let verified = post.user.verified == Some(true);
        let separator = widgets::line(&ctx, strings::POST_TIME_SEPARATOR, theme::regular(TEXT_SIZE), palette.muted, line_height);
        let time = time_label.as_deref().map(|label| widgets::line(&ctx, label, theme::regular(TEXT_SIZE), palette.muted, line_height));
        let mut reserved = 0.0;
        if verified {
            reserved += HEADER_META_GAP + 13.0;
        }
        if let Some(time) = &time {
            reserved += HEADER_META_GAP + separator.size().x + HEADER_META_GAP + time.size().x;
        }
        let username = widgets::line_elided(&ctx, &post.user.username, theme::semibold(TEXT_SIZE), palette.text, line_height, (right - meta_left - reserved).max(24.0));
        let username_rect = widgets::paint_line(painter, meta_left, header_center_y, username);
        widgets::text_label(ui, username_rect, id.with("username"), &post.user.username);
        let mut x = username_rect.right();
        if verified {
            x += HEADER_META_GAP;
            let badge_center = pos2(x + 6.5, header_center_y + 1.0);
            icons::paint(painter, Icon::Verified, badge_center, IconStyle::filled(13.0), Color32::WHITE);
            widgets::text_label(ui, Rect::from_center_size(badge_center, Vec2::splat(13.0)), id.with("verified"), strings::POST_VERIFIED);
            x += 13.0;
        }
        if let (Some(time), Some(label)) = (time, &time_label) {
            x += HEADER_META_GAP;
            let separator_width = separator.size().x;
            widgets::paint_line(painter, x, header_center_y, separator);
            x += separator_width + HEADER_META_GAP;
            let time_rect = Rect::from_min_size(pos2(x, header_center_y - line_height / 2.0), time.size());
            widgets::paint_line(painter, x, header_center_y, time);
            match (&post.permalink, &site) {
                (Some(link), Some(site)) => {
                    // With a link the time is a link: underlined under the pointer.
                    let response = widgets::hit(ui, time_rect, id.with("time"), label).on_hover_text(strings::open_post_on(site));
                    if response.hovered() {
                        let underline_y = (time_rect.bottom() - 3.0).round() + 0.5;
                        painter.hline(time_rect.x_range(), underline_y, egui::Stroke::new(1.0, palette.muted));
                    }
                    widgets::focus_ring(ui, &response, time_rect, 2.0, theme::FOCUS_RING_OFFSET);
                    if response.clicked() {
                        env.push(Action::OpenLink(link.clone()));
                    }
                }
                _ => {
                    widgets::text_label(ui, time_rect, id.with("time"), label);
                }
            }
        }

        // --- The row under the header: the location of a post, the file name of a picture ---
        let mut top = side.top() + HEADER_HEIGHT;
        if let Some(subline) = post.header_subline().filter(|line| !line.is_empty()) {
            let galley = widgets::line_elided(&ctx, subline, theme::regular(12.0), palette.muted, theme::normal_line_height(12.0), side.width() - 2.0 * SIDE_PADDING_X);
            let rect = widgets::paint_line(painter, side.left() + SIDE_PADDING_X, top + (LOCATION_HEIGHT - 1.0) / 2.0, galley);
            widgets::text_label(ui, rect, id.with("subline"), subline);
            divider(painter, top + LOCATION_HEIGHT - 1.0);
            top += LOCATION_HEIGHT;
        }

        // --- From the bottom up: date, likes, the action row ---
        let mut bottom = side.bottom() - SIDE_PADDING_BOTTOM;
        if let Some(taken_at) = post.taken_at {
            bottom -= DATE_HEIGHT;
            let label = text::date_label(now_ms, taken_at, &text::local_offset_minutes);
            let galley = widgets::line(&ctx, &label, theme::regular(10.0), palette.muted, theme::normal_line_height(10.0));
            let rect = widgets::paint_line(painter, side.left() + SIDE_PADDING_X, bottom + DATE_HEIGHT / 2.0, galley);
            widgets::text_label(ui, rect, id.with("date"), &label);
        }
        let likes = match feed::shown_likes(post, post_ui) {
            Some(count) => {
                let (number, rest) = strings::likes_line(count);
                Some((number, rest.to_owned(), true))
            }
            None if post_ui.liked => Some((strings::LIKED_BY.to_owned(), strings::LIKED_BY_YOU.to_owned(), false)),
            None => None,
        };
        if let Some((first, second, first_is_bold)) = likes {
            bottom -= LIKES_HEIGHT;
            let likes_height = theme::normal_line_height(TEXT_SIZE);
            // `.pd-likes strong` has the browser's own bold (700); only the feed card sets 600.
            let bold = widgets::format(theme::bold(TEXT_SIZE), palette.text, likes_height);
            let regular = widgets::format(theme::regular(TEXT_SIZE), palette.text, likes_height);
            let (first_format, second_format) = if first_is_bold { (bold, regular) } else { (regular, bold) };
            let mut job = LayoutJob::default();
            job.append(&first, 0.0, first_format);
            job.append(&second, 0.0, second_format);
            let label = job.text.clone();
            let rect = widgets::paint_line(painter, side.left() + SIDE_PADDING_X, bottom + LIKES_HEIGHT / 2.0, widgets::layout_job(&ctx, job));
            widgets::text_label(ui, rect, id.with("likes"), &label);
        }
        bottom -= ACTIONS_HEIGHT;
        divider(painter, bottom);
        let actions_center_y = bottom + 1.0 + 10.0 + ACTION_ICON_SIZE / 2.0;
        let first_x = side.left() + SIDE_PADDING_X + ACTION_ICON_SIZE / 2.0;
        let step = ACTION_ICON_SIZE + ACTION_ICON_GAP;
        let (like_color, like_label) = if post_ui.liked { (theme::LIKE_RED, strings::POST_UNLIKE) } else { (palette.text, strings::POST_LIKE) };
        if widgets::icon_button(ui, pos2(first_x, actions_center_y), id.with("like"), Icon::Heart, IconStyle::toggled(ACTION_ICON_SIZE, post_ui.liked), like_color, like_label).clicked() {
            env.push(Action::ToggleLikeQuietly(post.key.clone()));
        }
        if widgets::icon_button(ui, pos2(first_x + step, actions_center_y), id.with("comment"), Icon::Comment, IconStyle::outline(ACTION_ICON_SIZE), palette.text, strings::POST_COMMENT).clicked() {
            // There is no comment field in this app: the button leads to the comments.
            self.scroll_to_comments = true;
            ctx.request_repaint();
        }
        if widgets::icon_button(ui, pos2(first_x + 2.0 * step, actions_center_y), id.with("share"), Icon::Share, IconStyle::outline(ACTION_ICON_SIZE), palette.text, strings::POST_SHARE).clicked() {
            env.push(Action::Share(post.key.clone()));
        }
        let save_label = if post_ui.saved { strings::POST_UNSAVE } else { strings::POST_SAVE };
        let save_x = side.right() - SIDE_PADDING_X - ACTION_ICON_SIZE / 2.0;
        if widgets::icon_button(ui, pos2(save_x, actions_center_y), id.with("save"), Icon::Bookmark, IconStyle::toggled(ACTION_ICON_SIZE, post_ui.saved), palette.text, save_label).clicked() {
            env.push(Action::ToggleSave(post.key.clone()));
        }

        // --- The text between them: the description and the comments, scrolling ---
        let text_area = Rect::from_min_max(pos2(side.left(), top), pos2(side.right(), bottom.max(top)));
        self.show_text(ui, id, text_area, post, time_label, site.as_deref(), now_ms, env);
    }

    /// The scrolling text of the column (`.pd-comments`): the whole description as the first
    /// row, then the comments of an export, then the link to the rest of them.
    #[allow(clippy::too_many_arguments)]
    fn show_text(&mut self, ui: &mut Ui, id: Id, area: Rect, post: &Post, time_label: Option<String>, site: Option<&str>, now_ms: i64, env: &mut Env<'_>) {
        let ctx = ui.ctx().clone();
        let palette = env.palette();
        let text_width = area.width() - 2.0 * TEXT_PADDING - ROW_AVATAR_SIZE - ROW_GAP;

        let mut rows = Vec::new();
        if !text::js_trim(&post.caption).is_empty() {
            let avatar = feed::avatar_picture(env.loader, &ctx, post, ROW_AVATAR_SIZE, Priority::visible(2));
            let content = RowContent { username: &post.user.username, verified: post.user.verified == Some(true), body: &post.caption, sub: time_label.into_iter().collect(), avatar };
            rows.push(text_row(&ctx, content, text_width, palette));
        }
        let first_comment = rows.len();
        for comment in &post.comments {
            let mut sub: Vec<String> = comment.taken_at.map(|taken_at| text::relative_time_label(now_ms, taken_at)).into_iter().collect();
            if comment.likes > 0 {
                sub.push(strings::comment_likes(comment.likes));
            }
            rows.push(text_row(&ctx, RowContent { username: &comment.username, verified: false, body: &comment.text, sub, avatar: None }, text_width, palette));
        }
        // More comments than the export carries: a link to them, when the post has one.
        let view_all = match (&post.permalink, site) {
            (Some(link), Some(site)) if post.comment_count > post.comments.len() as u64 => Some((strings::view_comments_on(post.comment_count, site), link.clone())),
            _ => None,
        };

        let rows_height: f32 = rows.iter().map(|row| row.height() + ROW_MARGIN).sum();
        // `.pd-viewall`: `margin: 2px 0 8px` around a line of the font's own height.
        let view_all_line_height = theme::normal_line_height(TEXT_SIZE);
        let view_all_height = if view_all.is_some() { 2.0 + view_all_line_height + 8.0 } else { 0.0 };
        let content_height = TEXT_PADDING + rows_height + view_all_height + TEXT_PADDING;
        let first_comment_top = TEXT_PADDING + rows.iter().take(first_comment).map(|row| row.height() + ROW_MARGIN).sum::<f32>();

        ui.scope_builder(UiBuilder::new().max_rect(area), |ui| {
            // The bar floats over the right edge of the column (`.pd-comments` scrolls inside
            // itself): with room allocated for it, it would hang outside the card on the scrim.
            ui.spacing_mut().scroll.floating_allocated_width = 0.0;
            let mut scroll = egui::ScrollArea::vertical().id_salt(("viewer-text", post.key.as_str())).auto_shrink([false, false]);
            if std::mem::take(&mut self.scroll_to_comments) {
                scroll = scroll.vertical_scroll_offset(if post.comments.is_empty() { 0.0 } else { first_comment_top - TEXT_PADDING });
            }
            scroll.show_viewport(ui, |ui, viewport| {
                let content_top = ui.max_rect().top();
                ui.allocate_rect(Rect::from_min_size(pos2(area.left(), content_top), vec2(area.width(), content_height)), Sense::hover());
                let painter = ui.painter();
                let mut y = content_top + TEXT_PADDING;
                for (index, row) in rows.iter().enumerate() {
                    let height = row.height();
                    // Rows outside the view cost nothing.
                    if y - content_top + height >= viewport.top() && y - content_top <= viewport.bottom() {
                        let row_id = id.with(("text-row", index));
                        let avatar_center = pos2(area.left() + TEXT_PADDING + ROW_AVATAR_SIZE / 2.0, y + ROW_AVATAR_SIZE / 2.0);
                        widgets::avatar(painter, avatar_center, ROW_AVATAR_SIZE, false, &row.username, row.avatar.as_ref(), palette.surface);
                        let pos = widgets::snap(&ctx, pos2(area.left() + TEXT_PADDING + ROW_AVATAR_SIZE + ROW_GAP, y));
                        widgets::paint_galley(painter, pos, row.galley.clone());
                        widgets::text_label(ui, Rect::from_min_size(pos, row.galley.size()), row_id, &row.text);
                        if let Some(range) = &row.badge_after {
                            if let Some(space) = widgets::char_range_rects(&row.galley, range.clone()).first() {
                                let center = pos + vec2(space.right() + 6.0, space.center().y + 1.0);
                                icons::paint(painter, Icon::Verified, center, IconStyle::filled(12.0), Color32::WHITE);
                            }
                        }
                        if let Some(url) = widgets::link_areas(ui, &row.galley, pos, &row.links, row_id, palette) {
                            env.push(Action::OpenLink(url));
                        }
                        let mut sub_x = pos.x;
                        let sub_center_y = pos.y + row.galley.size().y + SUB_MARGIN + SUB_LINE_HEIGHT / 2.0;
                        for (part, label) in row.sub.iter().enumerate() {
                            let galley = widgets::line(&ctx, label, theme::regular(SUB_SIZE), palette.muted, SUB_LINE_HEIGHT);
                            let rect = widgets::paint_line(painter, sub_x, sub_center_y, galley);
                            widgets::text_label(ui, rect, row_id.with(("sub", part)), label);
                            sub_x = rect.right() + SUB_GAP;
                        }
                    }
                    y += height + ROW_MARGIN;
                }
                if let Some((label, link)) = &view_all {
                    // `.pd-viewall`: muted, semibold, the text colour under the pointer.
                    let button = widgets::TextButton {
                        label,
                        font: theme::semibold(TEXT_SIZE),
                        line_height: view_all_line_height,
                        color: palette.muted,
                        hover: widgets::TextHover::TextColor,
                        icon: None,
                        tooltip: None,
                        enabled: true,
                        leading_space: 0.0,
                    };
                    let pos = widgets::snap(&ctx, pos2(area.left() + TEXT_PADDING, y + 2.0));
                    if button.show(ui, pos, id.with("view-all"), palette).clicked() {
                        env.push(Action::OpenLink(link.clone()));
                    }
                }
            });
            self.text_column = Some((ui.min_rect(), area));
        });
    }

    /// The rectangle the text column of the last frame took, scroll bar included, and the area
    /// of the card's side column it was given (tests check that one lies inside the other).
    pub fn text_column(&self) -> Option<(Rect, Rect)> {
        self.text_column
    }
}

/// Paints one slide of the stage into `rect`: the picture contained (or zoomed), or what stands
/// in for it.
pub(crate) fn paint_contained(painter: &Painter, rect: Rect, contained: &Contained, zoom: Zoom, pixels_per_point: f32, has_dots: bool) {
    let ctx = painter.ctx().clone();
    let emoji = |glyph: &str| {
        // `.pd-media__subject`: 140 px, opacity .3.
        painter.text(rect.center(), Align2::CENTER_CENTER, glyph, theme::regular(140.0), theme::with_opacity(Color32::WHITE, 0.3));
    };
    match contained {
        Contained::Picture { picture, source, exact } => {
            let stage = Stage { size: rect.size(), source: vec2(source.0 as f32, source.1 as f32), pixels_per_point };
            let mut target = zoom.picture_rect(rect, &stage);
            let texture_size = vec2(picture.size_px[0] as f32, picture.size_px[1] as f32) / pixels_per_point;
            if zoom.is_fit() && *exact && (texture_size - target.size()).abs().max_elem() <= 1.0 {
                // The fitted rendition was decoded for exactly this place: one texture pixel per
                // screen pixel, on the pixel grid, so nothing is resampled. (A picture smaller
                // than the stage is enlarged to fit, as `object-fit: contain` does.)
                target = Rect::from_min_size(widgets::snap(&ctx, rect.center() - texture_size / 2.0), texture_size);
            }
            painter.image(picture.texture.id(), target, widgets::full_uv(), Color32::WHITE);
        }
        Contained::Loading { .. } => {}
        Contained::NoPicture => emoji(strings::PLACEHOLDER_VIDEO_EMOJI),
        Contained::Failed(message) => {
            emoji(strings::PLACEHOLDER_PHOTO_EMOJI);
            let bottom = rect.bottom() - if has_dots { 30.0 } else { 12.0 };
            media::message_pill(painter, rect.center().x, bottom, rect.width() - 48.0, message);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn window(width: f32, height: f32) -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(width, height))
    }

    #[test]
    fn a_window_of_the_reference_size_has_the_web_projects_card() {
        // 1100 x 720: the card is the web project's 1000 x 640 at 40 px from the top.
        let layout = layout(window(1100.0, 720.0));
        assert_eq!(layout.card, Rect::from_min_size(pos2(50.0, 40.0), vec2(1000.0, 640.0)));
        assert_eq!(layout.side.width(), SIDE_WIDTH);
        assert_eq!(layout.stage, Rect::from_min_size(pos2(50.0, 40.0), vec2(595.0, 640.0)));
        // The X is where `.pd-close` puts it.
        assert_eq!(Rect::from_center_size(layout.close, Vec2::splat(CLOSE_SIZE)), Rect::from_min_size(pos2(1050.0, 14.0), vec2(30.0, 30.0)));
    }

    #[test]
    fn the_card_grows_with_the_window() {
        let layout = layout(window(1440.0, 900.0));
        assert_eq!(layout.card, Rect::from_min_size(pos2(72.0, 40.0), vec2(1296.0, 820.0)));
        assert_eq!(layout.side.width(), SIDE_WIDTH);
        assert_eq!(layout.stage.width(), 891.0);
        // The arrows stand in the gutters, centred, at half the height of the card.
        assert!(!layout.arrows_on_stage);
        assert_eq!(layout.previous, pos2(36.0, 450.0));
        assert_eq!(layout.next, pos2(1404.0, 450.0));

        let large = super::layout(window(2560.0, 1400.0));
        assert_eq!(large.card.size(), vec2(2416.0, 1320.0));
        assert_eq!(large.stage.width(), 2416.0 - SIDE_WIDTH);
    }

    #[test]
    fn the_opaque_interior_leaves_out_the_rounded_corners() {
        let layout = layout(window(1440.0, 900.0));
        let interior = card_interior(&layout);
        assert_eq!(interior, Rect::from_min_max(pos2(76.0, 44.0), pos2(1364.0, 856.0)));
        // Inside the card, and every point of it lies in the picture area or in the column,
        // which are both painted opaque.
        assert!(layout.card.contains_rect(interior));
        assert!(layout.stage.union(layout.side).contains_rect(interior));
        assert_eq!(f32::from(CARD_RADIUS), interior.left() - layout.card.left());
        assert!(theme::MEDIA_BLACK.is_opaque());
        assert!([theme::LIGHT.surface, theme::DARK.surface].iter().all(|surface| surface.is_opaque()));
    }

    #[test]
    fn a_narrow_window_follows_the_web_project_and_puts_the_arrows_on_the_picture() {
        let layout = layout(window(900.0, 700.0));
        assert_eq!(layout.card, Rect::from_min_size(pos2(20.0, 40.0), vec2(860.0, 620.0)));
        assert_eq!(layout.side.width(), SIDE_WIDTH);
        assert!(layout.arrows_on_stage);
        assert_eq!(layout.previous.x, 20.0 + 10.0 + 22.0);
        assert_eq!(layout.next.x, layout.stage.right() - 10.0 - 22.0);

        // The smallest window: the column gives way so that the picture keeps its 280 points.
        let smallest = super::layout(window(640.0, 480.0));
        assert_eq!(smallest.card.size(), vec2(600.0, 400.0));
        assert_eq!(smallest.stage.width(), MIN_STAGE_WIDTH);
        assert_eq!(smallest.side.width(), 320.0);
    }

    #[test]
    fn the_decode_size_waits_until_the_stage_stops_changing() {
        let mut view = ViewerView::default();
        assert_eq!(view.debounced_side(800, 10.0, true), (800, true));
        // The window is being resized: the old side stays in use.
        assert_eq!(view.debounced_side(820, 10.02, true), (800, false));
        assert_eq!(view.debounced_side(840, 10.04, true), (800, false));
        assert_eq!(view.debounced_side(840, 10.10, true), (800, false));
        // It has been 840 for 150 ms now.
        assert_eq!(view.debounced_side(840, 10.20, true), (840, true));
        // Without animations (screenshots, tests) the new side is taken at once.
        assert_eq!(view.debounced_side(900, 10.21, false), (900, true));
    }
}
