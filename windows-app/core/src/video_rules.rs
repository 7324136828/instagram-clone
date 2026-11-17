//! The rules of video playback that need neither a window nor a decoder: which feed video plays,
//! how a time is written, the size frames are handed over in, and the sentence a failure
//! becomes. The app's players (app/src/video) follow them; they are kept here so that every rule
//! has a unit test.
//!
//! Ported from the reference web project where it has the rule (`components/videoPlayback.ts`:
//! `MostVisiblePicker`, `reportableDuration`); the rest is this app's own (video brief 5, 6.5,
//! 6.8): the frame sizes come from the measurements of the video spike on a machine without a
//! graphics card, the sentences from the HEIC rules of `decode`.

use std::time::Duration;

use crate::messages;

/// A feed video has to be at least this much on screen to play (`MIN_VISIBLE_RATIO`).
pub const MIN_VISIBLE_RATIO: f32 = 0.4;
/// At most this many players are alive at once: one each for the feed, the viewer and auto view.
/// Only one of them plays (several engines with a sound track starve each other on a machine
/// without a sound device; video brief 3.1).
pub const MAX_PLAYERS: usize = 3;
/// The feed's player is kept, paused, this long while something covers the feed, so that the
/// video resumes where it was when the viewer closes.
pub const FEED_KEEP_PAUSED_SEC: f64 = 30.0;
/// A feed player whose card has left the screen is closed after this long.
pub const FEED_UNSEEN_CLOSE_SEC: f64 = 5.0;
/// On a graphics card frames are handed over at the video's own size, but at most this many
/// pixels per side: a 4K video is scaled down by the engine on the card.
pub const GPU_MAX_FRAME_SIDE: u32 = 1920;
/// On a software renderer (WARP) frames are handed over with at most this many pixels: uploading
/// a texture there costs about 72 ns per pixel, so 640 x 360 takes about 17 ms (video brief 3.6).
pub const CPU_MAX_FRAME_PIXELS: u32 = 640 * 360;
/// On a software renderer the window is repainted this often while a video plays, and each
/// repaint shows the newest frame. A repaint of a window of 1920 x 1290 pixels costs about a
/// tenth of a second of processor time there (README, Videos): repainting for every frame of a
/// 30 fps video was measured at about 12 repaints a second with both processors of a
/// two-processor machine nearly busy.
pub const CPU_MAX_REPAINT_HZ: u32 = 10;
/// The shortest delay a repaint is asked for with. egui answers a request without any delay with
/// two repaints instead of one, which would put an extra repaint between two beats.
pub const MIN_REPAINT_DELAY: Duration = Duration::from_millis(1);
/// How often a player thread asks the engine for a new frame. A paused player costs nothing.
pub const PLAYER_TICK_MS: u64 = 8;
/// Shift+Left / Shift+Right move this many seconds.
pub const SEEK_STEP_SEC: f64 = 5.0;
/// While the seek bar is dragged, a fast seek is sent at most this often.
pub const SEEK_DRAG_INTERVAL_MS: u64 = 100;
/// The controls bar stays this long after the last key or pointer movement.
pub const CONTROLS_SHOWN_SEC: f64 = 2.5;

/// What the display adapter can do, decided once at the start from the wgpu adapter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoCaps {
    /// A graphics card: the feed plays its most visible video, frames at their own size.
    Gpu,
    /// A software renderer (WARP, "Microsoft Basic Render Driver"): no autoplay in the feed,
    /// small frames and a slow repaint in the viewer and auto view.
    Cpu,
}

impl VideoCaps {
    /// The class of an adapter: a software (CPU) adapter or anything else.
    pub fn from_adapter(is_cpu: bool) -> VideoCaps {
        if is_cpu {
            VideoCaps::Cpu
        } else {
            VideoCaps::Gpu
        }
    }

    /// The hidden harness option `--video-adapter=cpu|gpu`.
    pub fn parse(word: &str) -> Option<VideoCaps> {
        match word.trim().to_ascii_lowercase().as_str() {
            "cpu" => Some(VideoCaps::Cpu),
            "gpu" => Some(VideoCaps::Gpu),
            _ => None,
        }
    }

    /// True when the feed plays its most visible video by itself (the clone's rule).
    pub fn autoplays_feed(self) -> bool {
        self == VideoCaps::Gpu
    }

    /// The shortest time between two repaints while a video plays, in seconds; `None` for no
    /// limit (a repaint per frame).
    pub fn repaint_interval_sec(self) -> Option<f64> {
        match self {
            VideoCaps::Gpu => None,
            VideoCaps::Cpu => Some(1.0 / f64::from(CPU_MAX_REPAINT_HZ)),
        }
    }

    /// While a video plays: how long from now the next repaint is due, when the pass being
    /// described started `since_pass_start` ago. `None` on a graphics card, where every new frame
    /// asks for its own repaint.
    ///
    /// On a software renderer the repaints keep a beat of `repaint_interval_sec` counted from the
    /// start of each pass. Asking "one interval after the next frame arrives" instead (as each
    /// frame's wake-up does) lets the gap grow by the wait for that frame and by the pass itself:
    /// measured on a machine with two logical processors and no graphics card, that came to 8.2
    /// to 8.5 repaints a second for a 30 fps video, with a new frame in every one; the beat gives
    /// 9.9 to 10. When a pass takes longer than the interval, the next one follows at once
    /// (`MIN_REPAINT_DELAY`).
    pub fn next_repaint_delay(self, since_pass_start: Duration) -> Option<Duration> {
        let interval = Duration::from_secs_f64(self.repaint_interval_sec()?);
        Some(interval.saturating_sub(since_pass_start).max(MIN_REPAINT_DELAY))
    }
}

/// The clone's `MostVisiblePicker.pick`: the candidate (in feed order, with the share of its
/// media frame on screen, 0..=1) that plays. The best share must be at least `min_ratio`; among
/// the candidates with the best share the one playing now (`previous`) keeps playing, else the
/// first. A `previous` that is no candidate any more (its card left) hands over.
pub fn pick_most_visible<T: PartialEq + Clone>(previous: Option<&T>, candidates: &[(T, f32)], min_ratio: f32) -> Option<T> {
    let best_ratio = candidates.iter().map(|(_, ratio)| *ratio).fold(0.0_f32, f32::max);
    if best_ratio < min_ratio {
        return None;
    }
    let mut best: Option<&T> = None;
    for (target, ratio) in candidates {
        if *ratio == best_ratio && (best.is_none() || Some(target) == previous) {
            best = Some(target);
        }
    }
    best.cloned()
}

/// The share of a frame from `top` to `bottom` that lies inside the view from `view_top` to
/// `view_bottom` (all in the same units): 0 when it is outside or has no height.
pub fn visible_ratio(top: f32, bottom: f32, view_top: f32, view_bottom: f32) -> f32 {
    let height = bottom - top;
    if height <= 0.0 {
        return 0.0;
    }
    let shown = (bottom.min(view_bottom) - top.max(view_top)).max(0.0);
    (shown / height).clamp(0.0, 1.0)
}

/// The clone's `reportableDuration`: seconds when known, infinity when the video plays but
/// cannot tell (a WebM recorded in a browser), `None` when there is no length.
pub fn reportable_duration(duration: f64) -> Option<f64> {
    if duration == f64::INFINITY {
        return Some(duration);
    }
    (duration.is_finite() && duration > 0.0).then_some(duration)
}

/// A position or length as the controls bar shows it: "m:ss", "h:mm:ss" from one hour on,
/// "--:--" for a time that is not known (not a number, negative or infinite). Seconds are cut,
/// not rounded, as players count them.
pub fn format_media_time(seconds: f64) -> String {
    if !seconds.is_finite() || seconds < 0.0 {
        return "--:--".to_owned();
    }
    // Far below 2^53: exact as an integer.
    let total = seconds.floor() as u64;
    let (hours, minutes, rest) = (total / 3600, total / 60 % 60, total % 60);
    if hours > 0 {
        format!("{hours}:{minutes:02}:{rest:02}")
    } else {
        format!("{minutes}:{rest:02}")
    }
}

/// The time of the controls bar: "0:03 / 0:10"; "0:03 / --:--" while the length is not known;
/// only "0:03" for a video that plays without a length. The position is never shown past the
/// length: Media Foundation reports a position a little after the end of a short clip (2.02 s of
/// a 1.97 s video), and "0:02 / 0:01" would read as a fault.
pub fn format_time_pair(current: f64, duration: Option<f64>) -> String {
    match duration {
        Some(length) if length == f64::INFINITY => format_media_time(current),
        Some(length) => {
            let shown = if length.is_finite() && length >= 0.0 && current > length { length } else { current };
            format!("{} / {}", format_media_time(shown), format_media_time(length))
        }
        None => format!("{} / --:--", format_media_time(current)),
    }
}

/// Where Shift+Left / Shift+Right go from `current`: `SEEK_STEP_SEC` back or forward, kept
/// inside the video (its end when the length is known).
pub fn seek_target(current: f64, forward: bool, duration: Option<f64>) -> f64 {
    let wanted = if forward { current + SEEK_STEP_SEC } else { current - SEEK_STEP_SEC };
    let end = duration.filter(|length| length.is_finite() && *length > 0.0).unwrap_or(f64::INFINITY);
    wanted.clamp(0.0, end)
}

/// The size frames of a video of `native` pixels (as the engine reports it, already turned) are
/// handed over in. On a graphics card: the video's own size, scaled down only to fit
/// `GPU_MAX_FRAME_SIDE`. On a software renderer: scaled down so that it has at most
/// `CPU_MAX_FRAME_PIXELS` pixels and is not larger than the rectangle it is shown in (`target`,
/// in pixels). Never larger than the video; the aspect ratio is kept; at least 2 x 2.
pub fn frame_size_for(native: (u32, u32), target: Option<(u32, u32)>, caps: VideoCaps) -> (u32, u32) {
    let (width, height) = native;
    if width == 0 || height == 0 {
        return native;
    }
    let (w, h) = (f64::from(width), f64::from(height));
    let scale = match caps {
        VideoCaps::Gpu => f64::from(GPU_MAX_FRAME_SIDE) / w.max(h),
        VideoCaps::Cpu => {
            let by_pixels = (f64::from(CPU_MAX_FRAME_PIXELS) / (w * h)).sqrt();
            let by_target = target.filter(|(tw, th)| *tw > 0 && *th > 0).map_or(f64::INFINITY, |(tw, th)| (f64::from(tw) / w).min(f64::from(th) / h));
            by_pixels.min(by_target)
        }
    };
    if scale >= 1.0 {
        return native;
    }
    // Rounded down, so that the pixel cap holds after rounding.
    let scaled = |side: f64| ((side * scale).floor() as u32).max(2);
    (scaled(w), scaled(h))
}

/// What a player reported when a video could not be played (video brief 6.8).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlaybackFailure {
    /// `MF_MEDIA_ENGINE_ERR`: 1 aborted, 2 network, 3 decode, 4 source not supported; 0 when a
    /// call of the player failed.
    pub media_error: u16,
    /// The extended HRESULT.
    pub hresult: i32,
    /// The system's text for it, as "HRESULT 0x........: text".
    pub text: String,
    /// The codec of the first video stream, when the source reader could tell ("H.264", "HEVC",
    /// "VP9", "FourCC 'apcn'").
    pub codec: Option<String>,
    /// The file has no picture track (sound only).
    pub no_picture: bool,
}

/// No decoder for the codec (`MF_E_TOPO_CODEC_NOT_FOUND`).
pub const HRESULT_CODEC_NOT_FOUND: i32 = 0xC00D_5212_u32 as i32;
/// No source for the container (`MF_E_UNSUPPORTED_BYTESTREAM_TYPE`): a WebM without the Web
/// Media Extensions.
pub const HRESULT_UNSUPPORTED_BYTESTREAM: i32 = 0xC00D_36C4_u32 as i32;

/// The Store extension that brings a decoder for `codec`.
pub fn codec_remedy(codec: &str) -> &'static str {
    match codec {
        "HEVC" => messages::CODEC_REMEDY_HEVC,
        "VP9" => messages::CODEC_REMEDY_VP9,
        "AV1" => messages::CODEC_REMEDY_AV1,
        "VP8" => messages::CODEC_REMEDY_WEBM,
        _ => messages::CODEC_REMEDY_CONVERT,
    }
}

/// The system text of a failure without its "HRESULT 0x........: " prefix.
pub fn failure_text(text: &str) -> &str {
    match text.strip_prefix("HRESULT 0x") {
        Some(rest) => rest.split_once(": ").map_or(rest, |(_, after)| after).trim(),
        None => text.trim(),
    }
}

/// The sentence shown for a video that cannot be played (video brief 6.8). `file` is the name
/// the user knows the file by; `mime` its sniffed type. Only the two HRESULTs that mean "no
/// decoder" and "no source" are matched: every other one varies with the bytes of the file.
pub fn classify(file: &str, mime: &str, failure: &PlaybackFailure) -> String {
    let webm = mime == "video/webm";
    if failure.hresult == HRESULT_CODEC_NOT_FOUND {
        let codec = failure.codec.clone().unwrap_or_else(|| container_name(mime).to_owned());
        let remedy = match failure.codec.as_deref() {
            Some(known) => codec_remedy(known),
            None if webm => messages::CODEC_REMEDY_WEBM,
            None => messages::CODEC_REMEDY_CONVERT,
        };
        return messages::video_needs_codec(file, &codec, remedy);
    }
    if failure.hresult == HRESULT_UNSUPPORTED_BYTESTREAM && webm {
        return messages::video_needs_codec(file, "WebM", messages::CODEC_REMEDY_WEBM);
    }
    if failure.no_picture {
        return messages::video_cannot_play(file, messages::VIDEO_NO_PICTURE);
    }
    messages::video_cannot_play(file, &messages::video_damaged(failure_text(&failure.text)))
}

/// What a container is called in a sentence when its codec is not known.
fn container_name(mime: &str) -> &'static str {
    match mime {
        "video/webm" => "WebM",
        "video/quicktime" => "QuickTime",
        _ => "MP4",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Runs the picker over a sequence of visibility reports, as the clone's tests drive the
    /// IntersectionObserver: each step gives every candidate's share; the log says who started
    /// and stopped playing.
    fn drive(steps: &[&[(&'static str, f32)]]) -> Vec<String> {
        let mut playing: Option<&'static str> = None;
        let mut log = Vec::new();
        for step in steps {
            let next = pick_most_visible(playing.as_ref(), step, MIN_VISIBLE_RATIO);
            if next != playing {
                if let Some(old) = playing {
                    log.push(format!("{old}:pause"));
                }
                if let Some(new) = next {
                    log.push(format!("{new}:play"));
                }
                playing = next;
            }
        }
        log
    }

    #[test]
    fn the_most_visible_video_plays_and_the_choice_follows_the_scroll() {
        // videoPlayback.test.ts:30-41.
        assert_eq!(drive(&[&[("a", 0.9), ("b", 0.1)], &[("a", 0.6), ("b", 0.45)], &[("a", 0.3), ("b", 0.8)]]), ["a:play", "a:pause", "b:play"]);
    }

    #[test]
    fn nothing_plays_while_no_video_is_visible_enough() {
        // videoPlayback.test.ts:43-54: b keeps its 0.2 while only a is reported.
        assert_eq!(drive(&[&[("a", 0.39), ("b", 0.2)]]), Vec::<String>::new());
        assert_eq!(drive(&[&[("a", 0.39), ("b", 0.2)], &[("a", 0.5), ("b", 0.2)], &[("a", 0.0), ("b", 0.2)]]), ["a:play", "a:pause"]);
    }

    #[test]
    fn on_a_tie_the_playing_video_keeps_playing() {
        // videoPlayback.test.ts:56-65.
        assert_eq!(drive(&[&[("a", 0.0), ("b", 1.0)], &[("a", 1.0), ("b", 1.0)]]), ["b:play"]);
        // Without one playing, the first in feed order wins.
        assert_eq!(pick_most_visible(None, &[("a", 1.0), ("b", 1.0)], MIN_VISIBLE_RATIO), Some("a"));
    }

    #[test]
    fn the_next_best_takes_over_when_the_playing_one_goes_away() {
        // videoPlayback.test.ts:67-77: a is no candidate any more.
        assert_eq!(drive(&[&[("a", 1.0), ("b", 0.6)], &[("b", 0.6)]]), ["a:play", "a:pause", "b:play"]);
    }

    #[test]
    fn a_video_that_is_no_candidate_never_plays() {
        // videoPlayback.test.ts:79-87.
        assert_eq!(pick_most_visible(Some(&"a"), &[], MIN_VISIBLE_RATIO), None);
        assert_eq!(pick_most_visible(None, &[("b", 0.0)], MIN_VISIBLE_RATIO), None);
    }

    #[test]
    fn the_visible_share_is_measured_against_the_view() {
        assert_eq!(visible_ratio(0.0, 100.0, 0.0, 500.0), 1.0);
        assert_eq!(visible_ratio(-50.0, 50.0, 0.0, 500.0), 0.5);
        assert_eq!(visible_ratio(450.0, 550.0, 0.0, 500.0), 0.5);
        assert_eq!(visible_ratio(600.0, 700.0, 0.0, 500.0), 0.0);
        assert_eq!(visible_ratio(10.0, 10.0, 0.0, 500.0), 0.0);
    }

    #[test]
    fn a_length_is_reported_like_the_clone_reports_it() {
        // videoPlayback.test.ts:119-125.
        assert_eq!(reportable_duration(8.5), Some(8.5));
        assert_eq!(reportable_duration(f64::INFINITY), Some(f64::INFINITY));
        for value in [f64::NAN, 0.0, -1.0, f64::NEG_INFINITY] {
            assert_eq!(reportable_duration(value), None, "{value}");
        }
    }

    #[test]
    fn times_are_written_as_players_write_them() {
        assert_eq!(format_media_time(0.0), "0:00");
        assert_eq!(format_media_time(59.9), "0:59");
        assert_eq!(format_media_time(60.0), "1:00");
        assert_eq!(format_media_time(3599.0), "59:59");
        assert_eq!(format_media_time(3600.0), "1:00:00");
        assert_eq!(format_media_time(36_061.0), "10:01:01");
        assert_eq!(format_media_time(f64::INFINITY), "--:--");
        assert_eq!(format_media_time(f64::NAN), "--:--");
        assert_eq!(format_media_time(-1.0), "--:--");
        assert_eq!(format_time_pair(3.2, Some(10.0)), "0:03 / 0:10");
        assert_eq!(format_time_pair(3.2, None), "0:03 / --:--");
        assert_eq!(format_time_pair(75.0, Some(f64::INFINITY)), "1:15");
        // A position reported after the end of a short clip shows as its end.
        assert_eq!(format_time_pair(2.0, Some(1.97)), "0:01 / 0:01");
        assert_eq!(format_time_pair(2.4, Some(2.0)), "0:02 / 0:02");
    }

    #[test]
    fn seeking_by_keys_stays_inside_the_video() {
        assert_eq!(seek_target(3.0, true, Some(10.0)), 8.0);
        assert_eq!(seek_target(8.0, true, Some(10.0)), 10.0);
        assert_eq!(seek_target(3.0, false, Some(10.0)), 0.0);
        assert_eq!(seek_target(30.0, true, None), 35.0);
        assert_eq!(seek_target(30.0, true, Some(f64::INFINITY)), 35.0);
    }

    #[test]
    fn frames_keep_their_size_on_a_graphics_card_up_to_its_cap() {
        assert_eq!(frame_size_for((1280, 720), None, VideoCaps::Gpu), (1280, 720));
        assert_eq!(frame_size_for((1920, 1080), Some((400, 300)), VideoCaps::Gpu), (1920, 1080));
        assert_eq!(frame_size_for((3840, 2160), None, VideoCaps::Gpu), (1920, 1080));
        assert_eq!(frame_size_for((2160, 3840), None, VideoCaps::Gpu), (1080, 1920));
        assert_eq!(frame_size_for((0, 0), None, VideoCaps::Gpu), (0, 0));
    }

    #[test]
    fn frames_are_small_on_a_software_renderer_and_never_scaled_up() {
        // The pixel cap.
        let (w, h) = frame_size_for((1280, 720), None, VideoCaps::Cpu);
        assert!(w * h <= CPU_MAX_FRAME_PIXELS, "{w}x{h}");
        assert_eq!((w, h), (640, 360));
        let (w, h) = frame_size_for((1920, 1080), Some((4000, 4000)), VideoCaps::Cpu);
        assert_eq!((w, h), (640, 360));
        // The rectangle on screen is smaller still.
        assert_eq!(frame_size_for((1280, 720), Some((320, 400)), VideoCaps::Cpu), (320, 180));
        // A small video stays as it is, whatever the rectangle.
        assert_eq!(frame_size_for((240, 300), Some((1000, 1000)), VideoCaps::Cpu), (240, 300));
        // Odd sizes and a portrait (turned) video.
        let (w, h) = frame_size_for((1001, 777), None, VideoCaps::Cpu);
        assert!(w * h <= CPU_MAX_FRAME_PIXELS && w <= 1001 && h <= 777, "{w}x{h}");
        assert_eq!(frame_size_for((360, 640), None, VideoCaps::Cpu), (360, 640));
        assert_eq!(frame_size_for((720, 1280), None, VideoCaps::Cpu), (360, 640));
        // A tiny rectangle still gives a frame.
        assert_eq!(frame_size_for((1280, 720), Some((1, 1)), VideoCaps::Cpu), (2, 2));
    }

    #[test]
    fn the_adapter_class_and_its_rules() {
        assert_eq!(VideoCaps::from_adapter(true), VideoCaps::Cpu);
        assert_eq!(VideoCaps::from_adapter(false), VideoCaps::Gpu);
        assert_eq!(VideoCaps::parse(" CPU "), Some(VideoCaps::Cpu));
        assert_eq!(VideoCaps::parse("gpu"), Some(VideoCaps::Gpu));
        assert_eq!(VideoCaps::parse("warp"), None);
        assert!(VideoCaps::Gpu.autoplays_feed() && !VideoCaps::Cpu.autoplays_feed());
        assert_eq!(VideoCaps::Cpu.repaint_interval_sec(), Some(0.1));
        assert_eq!(VideoCaps::Gpu.repaint_interval_sec(), None);
    }

    #[test]
    fn a_software_renderer_repaints_on_a_beat_counted_from_the_start_of_each_pass() {
        let ms = Duration::from_millis;
        // The next repaint is one interval after the start of this pass, whenever it is asked.
        assert_eq!(VideoCaps::Cpu.next_repaint_delay(ms(0)), Some(ms(100)));
        assert_eq!(VideoCaps::Cpu.next_repaint_delay(ms(30)), Some(ms(70)));
        assert_eq!(VideoCaps::Cpu.next_repaint_delay(ms(99)), Some(ms(1)));
        // A pass that took the whole interval or longer is followed at once, but never with a
        // zero delay (egui would repaint twice).
        assert_eq!(VideoCaps::Cpu.next_repaint_delay(ms(100)), Some(MIN_REPAINT_DELAY));
        assert_eq!(VideoCaps::Cpu.next_repaint_delay(ms(250)), Some(MIN_REPAINT_DELAY));
        assert!(MIN_REPAINT_DELAY > Duration::ZERO);
        // A graphics card repaints for every frame instead.
        assert_eq!(VideoCaps::Gpu.next_repaint_delay(ms(0)), None);
        assert_eq!(VideoCaps::Gpu.next_repaint_delay(ms(250)), None);
    }

    fn failure(hresult: u32, codec: Option<&str>) -> PlaybackFailure {
        PlaybackFailure { media_error: 4, hresult: hresult as i32, text: format!("HRESULT 0x{hresult:08X}: Some text."), codec: codec.map(str::to_owned), no_picture: false }
    }

    #[test]
    fn a_missing_decoder_names_the_store_extension() {
        let hevc = classify("IMG_1.MOV", "video/quicktime", &failure(0xC00D_5212, Some("HEVC")));
        assert_eq!(hevc, "\"IMG_1.MOV\" can't be played: Windows has no decoder for its HEVC video. HEVC videos need the HEVC Video Extensions from the Microsoft Store.");
        assert!(classify("a.webm", "video/webm", &failure(0xC00D_5212, Some("VP9"))).ends_with("VP9 videos need the VP9 Video Extensions from the Microsoft Store."));
        assert!(classify("a.mp4", "video/mp4", &failure(0xC00D_5212, Some("AV1"))).ends_with("AV1 videos need the AV1 Video Extension from the Microsoft Store."));
        assert!(classify("a.webm", "video/webm", &failure(0xC00D_5212, Some("VP8"))).ends_with("WebM videos need the Web Media Extensions from the Microsoft Store."));
        assert_eq!(
            classify("pro.mov", "video/quicktime", &failure(0xC00D_5212, Some("FourCC 'apcn'"))),
            "\"pro.mov\" can't be played: Windows has no decoder for its FourCC 'apcn' video. Convert it to MP4 (H.264)."
        );
        // The codec could not be read: the container is named.
        assert!(classify("a.mp4", "video/mp4", &failure(0xC00D_5212, None)).contains("for its MP4 video. Convert it to MP4 (H.264)."));
    }

    #[test]
    fn a_webm_without_a_source_needs_the_web_media_extensions() {
        assert_eq!(
            classify("clip.webm", "video/webm", &failure(0xC00D_36C4, None)),
            "\"clip.webm\" can't be played: Windows has no decoder for its WebM video. WebM videos need the Web Media Extensions from the Microsoft Store."
        );
        // The same HRESULT for an MP4 is a damaged file (it varies with the bytes).
        assert_eq!(classify("text.mp4", "video/mp4", &failure(0xC00D_36C4, None)), "\"text.mp4\" can't be played: the file is damaged or incomplete (Some text).");
    }

    #[test]
    fn a_file_without_a_picture_and_a_damaged_file_say_so() {
        let mut sound_only = failure(0, None);
        sound_only.no_picture = true;
        assert_eq!(classify("song.mp4", "video/mp4", &sound_only), "\"song.mp4\" can't be played: it has no picture.");
        let truncated = PlaybackFailure { media_error: 4, hresult: 0xC00D_36E5_u32 as i32, text: "HRESULT 0xC00D36E5: The operation on the current offset is not permitted.".to_owned(), codec: None, no_picture: false };
        assert_eq!(classify("cut.mp4", "video/mp4", &truncated), "\"cut.mp4\" can't be played: the file is damaged or incomplete (The operation on the current offset is not permitted).");
        assert_eq!(failure_text("plain words"), "plain words");
    }
}
