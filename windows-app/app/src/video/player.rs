//! One video being played: a thread of its own with the Media Engine (`engine.rs`), a command
//! channel to it, and the newest frame and the status it publishes.
//!
//! Threading (video brief 6.4): every Media Foundation object of a player lives on that player's
//! thread, which joins the multithreaded apartment. The UI thread (a single-threaded apartment,
//! made so by winit for drag and drop) never touches Media Foundation and never changes its
//! apartment: it sends commands, which never block, and reads the status and the newest frame
//! under a short lock. Dropping a player closes its channel; the thread shuts the engine down
//! (measured 1.5-18 ms) and ends without being waited for.

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

use instagram_core::video_rules::{PlaybackFailure, VideoCaps, PLAYER_TICK_MS};

/// Wakes the UI when something changed (a new frame, the end, an error): a repaint request in
/// the app, a counter in tests.
pub type Waker = Arc<dyn Fn() + Send + Sync>;

/// How frames get from the engine into CPU memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FramePath {
    /// Frames are transferred into a WIC bitmap (software decoding). Used on a software
    /// renderer, where it cost less than Direct3D 11 in the spike.
    Wic,
    /// A Direct3D 11 device shared with the engine: hardware decoding on a graphics card, frames
    /// read back through a staging texture. Falls back to WIC when the device cannot be made.
    D3d11,
}

impl FramePath {
    /// The path for an adapter class. `INSTAGRAM_WINDOWS_APP_VIDEO_PATH=wic` forces WIC, to tell a
    /// fault of the Direct3D path from others on a machine with a graphics card.
    pub fn for_caps(caps: VideoCaps) -> FramePath {
        let forced_wic = std::env::var("INSTAGRAM_WINDOWS_APP_VIDEO_PATH").is_ok_and(|value| value.eq_ignore_ascii_case("wic"));
        match caps {
            VideoCaps::Gpu if !forced_wic => FramePath::D3d11,
            _ => FramePath::Wic,
        }
    }
}

/// What a player is opened with.
#[derive(Debug, Clone, PartialEq)]
pub struct PlayerOptions {
    /// Start playing as soon as possible.
    pub autoplay: bool,
    /// Start without sound.
    pub muted: bool,
    /// Start again at the end instead of stopping.
    pub looping: bool,
    /// 0.0 ..= 1.0.
    pub volume: f64,
    /// See `FramePath`.
    pub frame_path: FramePath,
    /// The adapter class: it caps the size frames are transferred in (`frame_size_for`).
    pub caps: VideoCaps,
    /// The rectangle the video is shown in, in pixels (frames are never larger on a software
    /// renderer).
    pub target_size: Option<(u32, u32)>,
    /// The sniffed content type; `None`: the player thread sniffs the file itself.
    pub mime: Option<String>,
    /// How often the player thread asks the engine for a new frame.
    pub tick: Duration,
}

impl Default for PlayerOptions {
    fn default() -> PlayerOptions {
        PlayerOptions {
            autoplay: true,
            muted: true,
            looping: false,
            volume: 1.0,
            frame_path: FramePath::Wic,
            caps: VideoCaps::Gpu,
            target_size: None,
            mime: None,
            tick: Duration::from_millis(PLAYER_TICK_MS),
        }
    }
}

/// What the player is doing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Phase {
    /// Opening the file, reading its header (with a sound track on a machine without a sound
    /// device this takes 2.3 s).
    #[default]
    Loading,
    /// Ready, not playing.
    Paused,
    /// Playing.
    Playing,
    /// Played to the end (and not looping).
    Ended,
    /// It cannot be played; `PlayerStatus::error` says why.
    Failed,
}

/// A snapshot of the player for the UI.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PlayerStatus {
    /// See `Phase`.
    pub phase: Phase,
    /// Seconds once known (`video_rules::reportable_duration`); infinity for a video that plays
    /// without a length.
    pub duration: Option<f64>,
    /// Seconds.
    pub current_time: f64,
    /// Pixels of the video as shown (already turned).
    pub native_size: Option<(u32, u32)>,
    /// A seek is running.
    pub seeking: bool,
    /// The file has a sound track.
    pub has_audio: bool,
    /// The file has a picture track (known once the header is read).
    pub has_video: bool,
    /// Why it cannot be played.
    pub error: Option<PlaybackFailure>,
    /// Times the engine reported "ended" (none while it loops).
    pub ended_events: u32,
    /// Frames handed to the UI.
    pub frames: u64,
    /// The sniffed content type.
    pub mime: String,
}

/// One decoded frame, already in egui's pixel format.
#[derive(Clone)]
pub struct VideoFrame {
    /// The pixels in the transferred size (smaller than the video on a software renderer).
    pub image: egui::ColorImage,
    /// Presentation time, seconds.
    pub pts: f64,
    /// Grows with every frame.
    pub serial: u64,
}

/// What the UI asks a player to do.
#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Play (from the start after the end).
    Play,
    /// Pause.
    Pause,
    /// Seconds; exact (`true`: to the frame) or fast (`false`: the key frame before).
    Seek(f64, bool),
    /// Sound off (`true`) or on.
    Mute(bool),
    /// 0.0 ..= 1.0.
    Volume(f64),
    /// Start again at the end.
    Loop(bool),
    /// The rectangle the video is shown in, in pixels.
    TargetSize(Option<(u32, u32)>),
}

/// State shared between the player thread and the UI.
#[derive(Default)]
pub(crate) struct Shared {
    pub status: PlayerStatus,
    pub frame: Option<VideoFrame>,
}

/// A video being played. Dropping it stops the player thread without waiting for it.
pub struct VideoPlayer {
    commands: mpsc::Sender<Command>,
    shared: Arc<Mutex<Shared>>,
    thread: Option<JoinHandle<Duration>>,
    path: PathBuf,
}

impl VideoPlayer {
    /// Starts a player thread for `path`. Errors of the file arrive later as `Phase::Failed`.
    #[cfg(windows)]
    pub fn open(path: &Path, options: PlayerOptions, waker: Waker) -> std::io::Result<VideoPlayer> {
        let (commands, receiver) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        let thread_shared = Arc::clone(&shared);
        let thread_path = path.to_path_buf();
        let thread = std::thread::Builder::new().name("video-player".to_owned()).spawn(move || super::engine::run(&thread_path, options, receiver, thread_shared, waker))?;
        Ok(VideoPlayer { commands, shared, thread: Some(thread), path: path.to_path_buf() })
    }

    /// Other platforms have no player: the video fails at once.
    #[cfg(not(windows))]
    pub fn open(path: &Path, _options: PlayerOptions, waker: Waker) -> std::io::Result<VideoPlayer> {
        let (commands, _receiver) = mpsc::channel();
        let shared = Arc::new(Mutex::new(Shared::default()));
        if let Ok(mut state) = shared.lock() {
            state.status.phase = Phase::Failed;
            state.status.error = Some(PlaybackFailure { media_error: 4, hresult: 0, text: instagram_core::messages::DECODER_ONLY_ON_WINDOWS.to_owned(), codec: None, no_picture: false });
        }
        waker();
        Ok(VideoPlayer { commands, shared, thread: None, path: path.to_path_buf() })
    }

    /// The file.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Sends a command; a player whose thread ended ignores it. Never blocks.
    pub fn send(&self, command: Command) {
        let _ = self.commands.send(command);
    }

    /// The state now.
    pub fn status(&self) -> PlayerStatus {
        self.shared.lock().map(|shared| shared.status.clone()).unwrap_or_default()
    }

    /// The newest frame when its serial is newer than `after`; an older frame that was never
    /// taken is gone (the UI never queues frames).
    pub fn take_frame(&self, after: u64) -> Option<VideoFrame> {
        let mut shared = self.shared.lock().ok()?;
        match &shared.frame {
            Some(frame) if frame.serial > after => shared.frame.take(),
            _ => None,
        }
    }

    /// Stops the player and waits for its thread (tests); returns how long the engine's shutdown
    /// took. Dropping does not wait.
    pub fn close(mut self) -> Option<Duration> {
        let thread = self.thread.take()?;
        drop(std::mem::replace(&mut self.commands, mpsc::channel().0));
        thread.join().ok()
    }
}

/// The line a player writes to the log when it closes: the frames it handed to the interface
/// (newest-frame slot: a frame the interface did not take in time is replaced, so this is more
/// than the window showed; the hub logs that), how long it played and what the engine dropped.
pub fn statistics_line(name: &str, frames: u64, playing: Duration, dropped: u64) -> String {
    let seconds = playing.as_secs_f64();
    let rate = if seconds > 0.0 { frames as f64 / seconds } else { 0.0 };
    format!("video player statistics for {name}: {frames} frames handed over in {seconds:.1} s of playing ({rate:.1} a second), {dropped} dropped by the engine")
}

/// Converts BGRA rows (as Media Foundation and Direct3D deliver them) into an opaque egui image.
/// `stride` is the distance between two rows in bytes; rows that are not in `bgra` stay black.
pub fn bgra_to_image(width: usize, height: usize, stride: usize, bgra: &[u8]) -> egui::ColorImage {
    let mut pixels = Vec::with_capacity(width * height);
    for row in 0..height {
        let start = row * stride;
        match bgra.get(start..start + width * 4) {
            Some(line) => pixels.extend(line.as_chunks::<4>().0.iter().map(|px| egui::Color32::from_rgb(px[2], px[1], px[0]))),
            None => pixels.extend(std::iter::repeat_n(egui::Color32::BLACK, width)),
        }
    }
    egui::ColorImage::new([width, height], pixels)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bgra_rows_become_opaque_rgb_pixels() {
        // Two pixels per row, a stride with two bytes of padding.
        let bytes = [1, 2, 3, 0, 4, 5, 6, 0, 9, 9, 7, 8, 9, 255, 10, 11, 12, 255];
        let image = bgra_to_image(2, 2, 10, &bytes[..18]);
        assert_eq!(image.size, [2, 2]);
        assert_eq!(image.pixels[0], egui::Color32::from_rgb(3, 2, 1));
        assert_eq!(image.pixels[1], egui::Color32::from_rgb(6, 5, 4));
        assert_eq!(image.pixels[2], egui::Color32::from_rgb(9, 8, 7));
        // A buffer shorter than its size says gives black rows instead of a panic.
        let short = bgra_to_image(2, 3, 8, &bytes[..8]);
        assert_eq!(short.pixels[4], egui::Color32::BLACK);
    }

    #[test]
    fn the_statistics_line_counts_frames_handed_over_per_second_of_playing() {
        assert_eq!(statistics_line("clip.mp4", 300, Duration::from_secs(10), 2), "video player statistics for clip.mp4: 300 frames handed over in 10.0 s of playing (30.0 a second), 2 dropped by the engine");
        // A player that never played divides by nothing.
        assert_eq!(statistics_line("a.mp4", 1, Duration::ZERO, 0), "video player statistics for a.mp4: 1 frames handed over in 0.0 s of playing (0.0 a second), 0 dropped by the engine");
    }

    #[test]
    fn a_graphics_card_uses_direct3d_and_a_software_renderer_wic() {
        assert_eq!(FramePath::for_caps(VideoCaps::Cpu), FramePath::Wic);
        if std::env::var_os("INSTAGRAM_WINDOWS_APP_VIDEO_PATH").is_none() {
            assert_eq!(FramePath::for_caps(VideoCaps::Gpu), FramePath::D3d11);
        }
    }
}
