//! A video on the stage of the viewer or of auto view (video brief 5.5, 5.6): contained on black
//! like a picture, the poster until the first frame, the controls bar over its bottom, the panel
//! of a video that cannot be played, and the keys of a video slide.

use egui::{Color32, Id, Key, Modifiers, Rect, Ui};
use instagram_core::model::{MediaKind, Post};
use instagram_core::video_rules::{self, CONTROLS_SHOWN_SEC};

use super::controls::{self, BarInput};
use super::{Surface, VideoKey};
use crate::state::{Action, Env, PlayerCommand};
use crate::widgets;

/// How the stage of a surface behaves.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct StageOptions {
    /// The viewer or auto view.
    pub surface: Surface,
    /// Auto view's visit counter (a post shown again starts its video again); 0 in the viewer.
    pub visit: u64,
    /// The view has the keyboard (no dialog above it).
    pub has_keyboard: bool,
}

/// What the stage of a video slide remembers between frames: when the user last moved the
/// pointer or pressed a key (the controls bar stays for `CONTROLS_SHOWN_SEC` after that).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct StageMemory {
    last_activity: f64,
    last_pointer: Option<egui::Pos2>,
}

/// Shows the video of slide `slide` of `post` in `stage`, when that slide is a video file.
/// `poster_size` is the size of the picture shown under it, while the video's own size is not
/// known. Returns `None` for a slide that is no video; otherwise the rectangle the video takes.
#[allow(clippy::too_many_arguments)]
pub fn show(ui: &Ui, stage: Rect, id: Id, post: &Post, slide: usize, poster_size: Option<(u32, u32)>, options: StageOptions, memory: &mut StageMemory, env: &mut Env<'_>) -> Option<Rect> {
    let media = post.media.get(slide).filter(|media| media.kind == MediaKind::Video)?;
    let ctx = ui.ctx().clone();
    let surface = options.surface;
    let key = VideoKey { post: post.key.clone(), slide, visit: options.visit };
    let name = post.media_file_name(slide).unwrap_or_default();
    let shown = env.video.shown(surface, &key);

    // The rectangle of the picture: by the video's own size once known, else the poster's.
    let size = shown.as_ref().and_then(|shown| shown.status.native_size).or(poster_size).or_else(|| shown.as_ref().and_then(|shown| shown.texture.map(|(_, [w, h])| (w as u32, h as u32))));
    let aspect = size.map_or(f32::NAN, |(w, h)| w as f32 / h.max(1) as f32);
    let video_rect = controls::contained_rect(&ctx, stage, aspect);
    let scale = ctx.pixels_per_point();
    let rect_px = ((video_rect.width() * scale).round() as u32, (video_rect.height() * scale).round() as u32);
    env.video.claim(surface, key, &media.path, &name, 1.0, rect_px);

    let painter = ui.painter().with_clip_rect(stage.intersect(ui.clip_rect()));
    if let Some((texture, _)) = shown.as_ref().and_then(|shown| shown.texture) {
        painter.rect_filled(video_rect, 0.0, Color32::BLACK);
        painter.image(texture, video_rect, widgets::full_uv(), Color32::WHITE);
    }
    if let Some(sentence) = shown.as_ref().and_then(|shown| shown.failure(&name)) {
        if controls::failure_panel(ui, stage, id.with("failed"), &sentence) {
            env.push(Action::ShowInFolder(post.key.clone()));
        }
        return Some(video_rect);
    }

    let status = shown.as_ref().map(|shown| shown.status.clone()).unwrap_or_default();
    let playing = shown.as_ref().is_some_and(|shown| shown.is_playing());
    let time = env.state.time;

    // The keys of a video slide; a focused button keeps Space for itself.
    let keys_free = options.has_keyboard && ctx.memory(|memory| memory.focused().is_none());
    let mut pressed_any = false;
    if keys_free {
        let pressed = |modifiers: Modifiers, key: Key| ctx.input_mut(|input| input.consume_key(modifiers, key));
        if surface == Surface::Viewer && (pressed(Modifiers::NONE, Key::Space) || pressed(Modifiers::NONE, Key::K)) {
            env.push(Action::VideoTogglePlay(surface));
            pressed_any = true;
        }
        if pressed(Modifiers::NONE, Key::M) {
            env.push(Action::ToggleVideoSound);
            pressed_any = true;
        }
        if surface == Surface::Viewer {
            // Shift+Left / Shift+Right before the plain arrows of the viewer take them.
            for (key, forward) in [(Key::ArrowLeft, false), (Key::ArrowRight, true)] {
                if pressed(Modifiers::SHIFT, key) {
                    env.push(Action::VideoSeek(surface, video_rules::seek_target(status.current_time, forward, status.duration), true));
                    pressed_any = true;
                }
            }
        }
    }

    // The controls bar: in the viewer while the pointer is over the video, while it is paused,
    // while a control is used, and for a moment after any input; in auto view on hover only.
    let pointer = ctx.input(|input| input.pointer.hover_pos());
    if pressed_any || (pointer.is_some() && pointer != memory.last_pointer && pointer.is_some_and(|at| stage.contains(at))) {
        memory.last_activity = time;
    }
    memory.last_pointer = pointer;
    let over_video = pointer.is_some_and(|at| video_rect.contains(at));
    let recent = time - memory.last_activity < CONTROLS_SHOWN_SEC;
    let engaged_before: bool = ctx.data(|data| data.get_temp(id.with("engaged"))).unwrap_or(false);
    let wanted = match surface {
        Surface::AutoView => over_video || engaged_before,
        _ => over_video || !playing || recent || engaged_before,
    };
    let opacity = widgets::transition(ui, id.with("controls"), wanted, 0.2);
    if wanted && recent && surface != Surface::AutoView {
        // Fade out once the moment is over.
        ctx.request_repaint_after_secs((CONTROLS_SHOWN_SEC - (time - memory.last_activity)).max(0.05) as f32);
    }
    let input = BarInput { status: &status, playing, sound_on: env.state.video_sound_on, volume: env.state.video_volume, opacity };
    let output = controls::controls_bar(ui, video_rect, id.with("bar"), &input);
    ctx.data_mut(|data| data.insert_temp(id.with("engaged"), output.engaged));
    if output.toggle_play {
        match surface {
            // Auto view's own pause holds the video and the clock together.
            Surface::AutoView => env.push(Action::AutoViewCommand(PlayerCommand::TogglePause)),
            _ => env.push(Action::VideoTogglePlay(surface)),
        }
    }
    if let Some((seconds, exact)) = output.seek {
        env.push(Action::VideoSeek(surface, seconds, exact));
    }
    if output.toggle_sound {
        env.push(Action::ToggleVideoSound);
    }
    if let Some(volume) = output.volume {
        env.push(Action::VideoVolume(volume));
    }
    Some(video_rect)
}
