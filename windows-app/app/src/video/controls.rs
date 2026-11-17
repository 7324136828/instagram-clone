//! What is drawn over a video: the feed's sound button (the clone's `.video__sound`), the play
//! badge of a feed card on a software renderer, the panel of a video that cannot be played
//! (`.video__failed`), and the controls bar of the viewer and auto view: the functions of the
//! browser's native controls (play / pause, time, seek bar, mute, volume) in the app's style
//! (video brief 5.3, 5.5, 5.7).

use egui::epaint::Mesh;
use egui::{pos2, vec2, Color32, CursorIcon, Id, Pos2, Rect, Sense, Ui, Vec2, WidgetInfo};
use instagram_core::video_rules::{self, SEEK_DRAG_INTERVAL_MS};

use crate::icons::{self, Icon, IconStyle};
use crate::strings;
use crate::theme;
use crate::widgets;

use super::player::PlayerStatus;

/// Diameter of the sound button (`.video__sound`: 28 px).
pub const SOUND_BUTTON_SIZE: f32 = 28.0;
/// Its distance from the right and bottom edges of the frame.
const SOUND_BUTTON_INSET: f32 = 12.0;
/// Diameter of the play badge of a feed card on a software renderer.
pub const PLAY_BADGE_SIZE: f32 = 64.0;
/// Height of the controls bar.
pub const CONTROLS_HEIGHT: f32 = 44.0;
/// Space at the sides of the bar and between its parts.
const BAR_PADDING: f32 = 12.0;
const BAR_GAP: f32 = 10.0;
/// Size of a button of the bar.
const BAR_BUTTON_SIZE: f32 = 32.0;
/// Width of the volume slider.
const VOLUME_WIDTH: f32 = 64.0;
/// Height of a track (seek bar, volume).
const TRACK_HEIGHT: f32 = 4.0;
/// Diameter of the thumb shown under the pointer.
const THUMB_SIZE: f32 = 12.0;
/// Font size of the time.
const TIME_SIZE: f32 = 12.0;
/// Padding of the failure panel: `14px 18px`.
const PANEL_PADDING: Vec2 = vec2(18.0, 14.0);
/// Space between the sentence and the button of the panel.
const PANEL_GAP: f32 = 10.0;
/// The panel keeps this far from each side of its frame (the clone keeps 16 px): clear of the
/// viewer's previous / next buttons (44 pt, 10 pt from the edge) and of a carousel's arrows.
const PANEL_SIDE_CLEARANCE: f32 = 64.0;

/// The sound button of a feed video, bottom right of its frame: "Turn sound on" / "Turn sound
/// off". Returns true when it was clicked.
pub fn sound_button(ui: &Ui, frame: Rect, id: Id, sound_on: bool) -> bool {
    let center = pos2(frame.right() - SOUND_BUTTON_INSET - SOUND_BUTTON_SIZE / 2.0, frame.bottom() - SOUND_BUTTON_INSET - SOUND_BUTTON_SIZE / 2.0);
    let rect = Rect::from_center_size(center, Vec2::splat(SOUND_BUTTON_SIZE));
    let label = if sound_on { strings::VIDEO_TURN_SOUND_OFF } else { strings::VIDEO_TURN_SOUND_ON };
    let response = widgets::hit(ui, rect, id, label).on_hover_text(label);
    let hover = widgets::transition(ui, id.with("hover"), response.hovered(), 0.15);
    let painter = ui.painter();
    painter.circle_filled(center, SOUND_BUTTON_SIZE / 2.0, theme::rgba(18, 18, 18, 0.72 + 0.18 * hover));
    let icon = if sound_on { Icon::Volume } else { Icon::VolumeOff };
    icons::paint(painter, icon, center, IconStyle::outline(16.0).with_stroke_width(2.2), Color32::WHITE);
    // `.video__sound:focus-visible { outline: 2px solid #fff }`.
    if response.has_focus() {
        painter.circle_stroke(center, SOUND_BUTTON_SIZE / 2.0 + theme::FOCUS_RING_OFFSET, egui::Stroke::new(theme::FOCUS_RING_WIDTH, Color32::WHITE));
    }
    response.clicked()
}

/// The play badge of a feed video on a software renderer, in the middle of its frame: "Play
/// video". Returns true when it was clicked.
pub fn play_badge(ui: &Ui, center: Pos2, id: Id) -> bool {
    let rect = Rect::from_center_size(center, Vec2::splat(PLAY_BADGE_SIZE));
    let response = widgets::hit(ui, rect, id, strings::VIDEO_PLAY_BADGE).on_hover_text(strings::VIDEO_PLAY_BADGE);
    let painter = ui.painter();
    painter.circle_filled(center, PLAY_BADGE_SIZE / 2.0, theme::rgba(18, 18, 18, 0.55));
    // The glyph's triangle sits a little left of the middle of its box: nudge it to the eye.
    icons::paint(painter, Icon::Play, center + vec2(2.0, 0.0), IconStyle::filled(28.0), Color32::WHITE);
    widgets::focus_ring(ui, &response, rect, PLAY_BADGE_SIZE / 2.0, theme::FOCUS_RING_OFFSET);
    response.clicked()
}

/// The panel of a video that cannot be played, centred in `rect`: the sentence and "Show in
/// folder" (the file is already on this computer; it replaces the clone's "Download video").
/// Returns true when the button was clicked.
pub fn failure_panel(ui: &Ui, rect: Rect, id: Id, sentence: &str) -> bool {
    let ctx = ui.ctx().clone();
    let max_text = (rect.width() - 2.0 * PANEL_SIDE_CLEARANCE - 2.0 * PANEL_PADDING.x).max(80.0);
    let text = widgets::wrapped(&ctx, sentence, theme::regular(14.0), Color32::WHITE, 19.6, max_text);
    let button_text = widgets::line(&ctx, strings::VIDEO_SHOW_IN_FOLDER, theme::semibold(14.0), Color32::BLACK, 19.6);
    let button_size = button_text.size() + vec2(24.0, 12.0);
    let width = text.size().x.max(button_size.x) + 2.0 * PANEL_PADDING.x;
    let height = text.size().y + PANEL_GAP + button_size.y + 2.0 * PANEL_PADDING.y;
    let panel = Rect::from_center_size(widgets::snap(&ctx, rect.center()), vec2(width, height));
    let painter = ui.painter();
    painter.rect_filled(panel, 10.0, theme::rgba(18, 18, 18, 0.82));
    let text_size = text.size();
    widgets::paint_galley(painter, pos2(panel.center().x - text_size.x / 2.0, panel.top() + PANEL_PADDING.y), text);
    widgets::text_label(ui, Rect::from_center_size(pos2(panel.center().x, panel.top() + PANEL_PADDING.y + text_size.y / 2.0), text_size), id.with("sentence"), sentence);
    let button = Rect::from_min_size(pos2(panel.center().x - button_size.x / 2.0, panel.bottom() - PANEL_PADDING.y - button_size.y), button_size);
    let response = widgets::hit(ui, button, id.with("show"), strings::VIDEO_SHOW_IN_FOLDER);
    let fill = if response.hovered() { Color32::from_rgb(0xDB, 0xDB, 0xDB) } else { Color32::WHITE };
    let painter = ui.painter();
    painter.rect_filled(button, 8.0, fill);
    widgets::paint_line(painter, button.left() + 12.0, button.center().y, button_text);
    widgets::focus_ring(ui, &response, button, 8.0, theme::FOCUS_RING_OFFSET);
    response.clicked()
}

/// What the controls bar shows.
#[derive(Clone, Debug)]
pub struct BarInput<'a> {
    /// The player's state.
    pub status: &'a PlayerStatus,
    /// The video plays (or is about to): the button offers "Pause".
    pub playing: bool,
    /// The run-wide sound flag.
    pub sound_on: bool,
    /// The run-wide volume.
    pub volume: f64,
    /// 0..=1: the bar fades out when nothing happened for a while.
    pub opacity: f32,
}

/// What the user did with the controls bar.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BarOutput {
    /// Play or pause.
    pub toggle_play: bool,
    /// Seek to seconds, exactly or fast.
    pub seek: Option<(f64, bool)>,
    /// Sound on or off.
    pub toggle_sound: bool,
    /// The new volume.
    pub volume: Option<f64>,
    /// The pointer is over the bar or a control of it has the focus: it stays.
    pub engaged: bool,
}

/// A vertical gradient over `rect` from transparent to `rgba(0, 0, 0, alpha)` at the bottom.
fn bottom_shade(painter: &egui::Painter, rect: Rect, alpha: f32) {
    let mut mesh = Mesh::default();
    mesh.colored_vertex(rect.left_top(), Color32::TRANSPARENT);
    mesh.colored_vertex(rect.right_top(), Color32::TRANSPARENT);
    mesh.colored_vertex(rect.left_bottom(), theme::rgba(0, 0, 0, alpha));
    mesh.colored_vertex(rect.right_bottom(), theme::rgba(0, 0, 0, alpha));
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(1, 2, 3);
    painter.add(egui::Shape::mesh(mesh));
}

/// The controls bar over the bottom of `video` (the rectangle the picture is drawn in).
pub fn controls_bar(ui: &Ui, video: Rect, id: Id, input: &BarInput<'_>) -> BarOutput {
    let mut output = BarOutput::default();
    if input.opacity <= 0.0 {
        return output;
    }
    let ctx = ui.ctx().clone();
    let white = |opacity: f32| theme::with_opacity(Color32::WHITE, opacity * input.opacity);
    let bar = Rect::from_min_max(pos2(video.left(), video.bottom() - CONTROLS_HEIGHT), video.max);
    let painter = ui.painter().with_clip_rect(video.intersect(ui.clip_rect()));
    bottom_shade(&painter, bar, 0.55 * input.opacity);
    let center_y = bar.center().y;
    let mut x = bar.left() + BAR_PADDING;

    // Play / pause.
    let play_rect = Rect::from_min_size(pos2(x, center_y - BAR_BUTTON_SIZE / 2.0), Vec2::splat(BAR_BUTTON_SIZE));
    let play_label = if input.playing { strings::VIDEO_PAUSE } else { strings::VIDEO_PLAY };
    let play = widgets::hit(ui, play_rect, id.with("play"), play_label).on_hover_text(play_label);
    icons::paint(&painter, if input.playing { Icon::Pause } else { Icon::Play }, play_rect.center(), IconStyle::filled(20.0), white(1.0));
    widgets::focus_ring(ui, &play, play_rect, 6.0, 0.0);
    output.toggle_play = play.clicked();
    x = play_rect.right() + BAR_GAP;

    // The time.
    let time_text = video_rules::format_time_pair(input.status.current_time, input.status.duration);
    let time = widgets::line(&ctx, &time_text, theme::regular(TIME_SIZE), white(1.0), 16.0);
    let time_rect = widgets::paint_line(&painter, x, center_y, time);
    widgets::text_label(ui, time_rect, id.with("time"), &time_text);
    x = time_rect.right() + BAR_GAP;

    // Right side: mute and volume.
    let volume_rect = Rect::from_min_max(pos2(bar.right() - BAR_PADDING - VOLUME_WIDTH, center_y - BAR_BUTTON_SIZE / 2.0), pos2(bar.right() - BAR_PADDING, center_y + BAR_BUTTON_SIZE / 2.0));
    let mute_rect = Rect::from_min_size(pos2(volume_rect.left() - BAR_GAP / 2.0 - BAR_BUTTON_SIZE, center_y - BAR_BUTTON_SIZE / 2.0), Vec2::splat(BAR_BUTTON_SIZE));
    let mute_label = if input.sound_on { strings::VIDEO_MUTE } else { strings::VIDEO_UNMUTE };
    let mute = widgets::hit(ui, mute_rect, id.with("mute"), mute_label).on_hover_text(mute_label);
    icons::paint(&painter, if input.sound_on { Icon::Volume } else { Icon::VolumeOff }, mute_rect.center(), IconStyle::outline(20.0).with_stroke_width(2.2), white(1.0));
    widgets::focus_ring(ui, &mute, mute_rect, 6.0, 0.0);
    output.toggle_sound = mute.clicked();

    let volume_response = ui.interact(volume_rect, id.with("volume"), Sense::click_and_drag());
    volume_response.widget_info(|| WidgetInfo::slider(true, input.volume, strings::VIDEO_VOLUME));
    let volume_track = Rect::from_center_size(volume_rect.center(), vec2(volume_rect.width(), TRACK_HEIGHT));
    let shown_volume = if input.sound_on { input.volume } else { 0.0 } as f32;
    painter.rect_filled(volume_track, TRACK_HEIGHT / 2.0, white(0.35));
    painter.rect_filled(Rect::from_min_max(volume_track.min, pos2(volume_track.left() + volume_track.width() * shown_volume, volume_track.bottom())), TRACK_HEIGHT / 2.0, white(1.0));
    if volume_response.hovered() || volume_response.dragged() {
        painter.circle_filled(pos2(volume_track.left() + volume_track.width() * shown_volume, volume_track.center().y), THUMB_SIZE / 2.0, white(1.0));
        ctx.set_cursor_icon(CursorIcon::PointingHand);
    }
    if volume_response.clicked() || volume_response.dragged() {
        if let Some(pointer) = volume_response.interact_pointer_pos() {
            output.volume = Some(f64::from(((pointer.x - volume_track.left()) / volume_track.width()).clamp(0.0, 1.0)));
        }
    }

    // The seek bar takes what is left.
    let seek_rect = Rect::from_min_max(pos2(x, center_y - BAR_BUTTON_SIZE / 2.0), pos2((mute_rect.left() - BAR_GAP).max(x + 20.0), center_y + BAR_BUTTON_SIZE / 2.0));
    let duration = input.status.duration.filter(|length| length.is_finite() && *length > 0.0);
    let played = duration.map_or(0.0, |length| (input.status.current_time / length).clamp(0.0, 1.0)) as f32;
    let seek = ui.interact(seek_rect, id.with("seek"), Sense::click_and_drag());
    seek.widget_info(|| WidgetInfo::slider(duration.is_some(), input.status.current_time, strings::VIDEO_POSITION));
    let track = Rect::from_center_size(seek_rect.center(), vec2(seek_rect.width(), TRACK_HEIGHT));
    painter.rect_filled(track, TRACK_HEIGHT / 2.0, white(0.35));
    painter.rect_filled(Rect::from_min_max(track.min, pos2(track.left() + track.width() * played, track.bottom())), TRACK_HEIGHT / 2.0, white(1.0));
    if seek.hovered() || seek.dragged() {
        painter.circle_filled(pos2(track.left() + track.width() * played, track.center().y), THUMB_SIZE / 2.0, white(1.0));
        if duration.is_some() {
            ctx.set_cursor_icon(CursorIcon::PointingHand);
        }
    }
    if let (Some(length), Some(pointer)) = (duration, seek.interact_pointer_pos()) {
        let target = f64::from(((pointer.x - track.left()) / track.width()).clamp(0.0, 1.0)) * length;
        if seek.clicked() || seek.drag_stopped() {
            // A click and the end of a drag go to the frame.
            output.seek = Some((target, true));
        } else if seek.dragged() {
            // While dragging, fast seeks to the key frame before, at most every 100 ms.
            let last_id = id.with("seek-sent");
            let now = ctx.input(|input| input.time);
            let last: f64 = ctx.data(|data| data.get_temp(last_id)).unwrap_or(f64::NEG_INFINITY);
            if (now - last) * 1000.0 >= SEEK_DRAG_INTERVAL_MS as f64 {
                ctx.data_mut(|data| data.insert_temp(last_id, now));
                output.seek = Some((target, false));
            }
        }
    }

    let hovered = ui.rect_contains_pointer(bar);
    output.engaged = hovered || play.has_focus() || mute.has_focus() || seek.dragged() || volume_response.dragged();
    output
}

/// Paints a frame `texture` of `size` pixels cover-cropped into `rect` (feed).
pub fn paint_cover(painter: &egui::Painter, rect: Rect, texture: egui::TextureId, size: [usize; 2]) {
    let uv = crate::layout::cover_uv(vec2(size[0] as f32, size[1] as f32), rect.size());
    painter.image(texture, rect, uv, Color32::WHITE);
}

/// The rectangle a picture of `aspect` (width / height) takes contained in `stage`, snapped.
pub fn contained_rect(ctx: &egui::Context, stage: Rect, aspect: f32) -> Rect {
    if !(aspect.is_finite() && aspect > 0.0) {
        return stage;
    }
    let size = if stage.width() / stage.height() > aspect { vec2(stage.height() * aspect, stage.height()) } else { vec2(stage.width(), stage.width() / aspect) };
    let rect = Rect::from_center_size(stage.center(), size);
    Rect::from_min_size(widgets::snap(ctx, rect.min), rect.size())
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_contained_like_a_picture() {
        let ctx = egui::Context::default();
        let stage = Rect::from_min_size(Pos2::ZERO, vec2(800.0, 400.0));
        assert_eq!(contained_rect(&ctx, stage, 16.0 / 9.0).size(), vec2(711.1111, 400.0));
        assert_eq!(contained_rect(&ctx, stage, 0.5).size(), vec2(200.0, 400.0));
        assert_eq!(contained_rect(&ctx, stage, f32::NAN), stage);
    }
}
