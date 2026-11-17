//! Video playback: who plays, the textures of the frames, the sound flag (video brief 6).
//!
//! Videos are played by Windows Media Foundation's Media Engine, one thread per player
//! (`player.rs`, `engine.rs`); this module decides which players exist and which one plays.
//!
//! Every frame the views CLAIM the videos they show: the feed each video card it lays out (with
//! the share of its media frame on screen), the viewer and auto view the slide on show. At the
//! end of the frame the hub resolves (`VideoHub::end_frame`): auto view open -> only its video
//! plays; else the viewer open -> only the viewer's; else the feed's most visible video
//! (`video_rules::pick_most_visible`) on a graphics card, none on a software renderer. At most
//! one player plays at any time: engines with a sound track starve each other on a machine
//! without a sound device (video brief 3.1). Everything else is paused; a player whose video is
//! no longer claimed is closed (the viewer's at once, the feed's after a grace time, so that it
//! resumes where it was when the viewer closes).
//!
//! A minimized window draws nothing, so nothing claims and `end_frame` does not run: the window
//! glue tells the hub in every pass (`window_minimized`), which pauses every player and keeps it
//! until the window is back.
//!
//! The UI thread never calls Media Foundation: it sends commands, takes the newest frame of each
//! player into a texture at the start of a frame (`begin_frame`) and reads statuses.
//!
//! How often a playing video is repainted decides how smooth it looks, since every repaint takes
//! the newest frame: on a graphics card each new frame asks for a repaint; on a software renderer
//! the hub keeps a beat of `CPU_MAX_REPAINT_HZ` repaints a second (`end_frame`). Measured there,
//! the engine handed over all 30 frames a second of a 720p video and every repaint showed a new
//! one, so the beat is what the eye gets.

pub mod backend;
pub mod controls;
#[cfg(windows)]
mod engine;
pub mod player;
pub mod stage;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use instagram_core::autoview::VideoState;
use instagram_core::model::PostKey;
use instagram_core::video_rules::{self, VideoCaps, FEED_KEEP_PAUSED_SEC, FEED_UNSEEN_CLOSE_SEC, MIN_VISIBLE_RATIO};

pub use backend::{FakeBackend, FakeLog, FakeScript, MfBackend, PlayerHandle, VideoBackend};
pub use player::{bgra_to_image, Command, FramePath, Phase, PlayerOptions, PlayerStatus, VideoFrame, VideoPlayer, Waker};

/// A frame size on screen must change by more than this many pixels before the player is told:
/// a window being resized would otherwise send a command per frame.
const TARGET_SIZE_SLACK_PX: u32 = 8;

/// The pass that closes the feed's player of a minimized window is asked for this long after its
/// `FEED_KEEP_PAUSED_SEC` are up, so that it finds them over, not just reached.
const MINIMIZED_CLOSE_MARGIN: Duration = Duration::from_millis(100);

/// The places a video plays in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Surface {
    /// A card of the home feed.
    Feed,
    /// The viewer (post detail).
    Viewer,
    /// Auto view.
    AutoView,
}

/// Which video a player plays: a slide of a post, and for auto view the visit (a post shown
/// again starts its video again).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct VideoKey {
    /// The post.
    pub post: PostKey,
    /// The slide (0-based).
    pub slide: usize,
    /// Auto view's visit counter; 0 elsewhere.
    pub visit: u64,
}

impl VideoKey {
    /// The key of slide `slide` of `post` outside auto view.
    pub fn new(post: &PostKey, slide: usize) -> VideoKey {
        VideoKey { post: post.clone(), slide, visit: 0 }
    }
}

/// A view shows a video this frame.
#[derive(Clone, Debug)]
struct Claim {
    key: VideoKey,
    path: PathBuf,
    /// The name the user knows the file by (failure sentences, the log).
    name: String,
    /// The share of the media frame on screen (feed only).
    ratio: f32,
    /// The rectangle it is shown in, in pixels.
    rect_px: (u32, u32),
}

/// What a view shows of its video this frame.
#[derive(Clone, Debug)]
pub struct Shown {
    /// The newest frame and its size in pixels, once one has arrived.
    pub texture: Option<(egui::TextureId, [usize; 2])>,
    /// The player's state.
    pub status: PlayerStatus,
    /// The user paused it (viewer).
    pub user_paused: bool,
}

impl Shown {
    /// The sentence to show when the video cannot be played (`video_rules::classify`).
    pub fn failure(&self, file: &str) -> Option<String> {
        if self.status.phase != Phase::Failed {
            return None;
        }
        let failure = self.status.error.clone().unwrap_or_default();
        Some(video_rules::classify(file, &self.status.mime, &failure))
    }

    /// True when the video cannot be played (its failure panel is shown).
    pub fn has_failed(&self) -> bool {
        self.status.phase == Phase::Failed
    }

    /// True while the video plays or is about to (the user did not pause it, it has not ended).
    pub fn is_playing(&self) -> bool {
        !self.user_paused && matches!(self.status.phase, Phase::Playing | Phase::Loading)
    }
}

/// What decides who plays, from the state of the interface this frame.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Wants {
    /// The feed is on screen and nothing covers it (no viewer, auto view, dialog, menu or drop
    /// overlay), and the window is not minimized.
    pub feed_uncovered: bool,
    /// The viewer is open.
    pub viewer_open: bool,
    /// Auto view is running.
    pub auto_view_open: bool,
    /// Auto view's clock runs (not paused by Space, not on the end screen).
    pub auto_view_playing: bool,
    /// Auto view lets its video loop (it does not wait for the end: `SlidePlan::listen_for_end`).
    pub auto_view_looping: bool,
    /// The window is minimized: nothing plays. (eframe seldom paints a minimized window; the
    /// passes that draw nothing pause the players through `VideoHub::window_minimized`.)
    pub minimized: bool,
    /// The run-wide sound flag.
    pub sound_on: bool,
    /// The run-wide volume, 0..=1.
    pub volume: f64,
}

/// What was last sent to a player, so that a command goes out only when something changes.
#[derive(Clone, Debug, Default, PartialEq)]
struct Sent {
    playing: Option<bool>,
    muted: Option<bool>,
    volume: Option<f64>,
    looping: Option<bool>,
    target: Option<(u32, u32)>,
}

/// A player and what the hub knows of it.
struct Live {
    handle: Box<dyn PlayerHandle>,
    key: VideoKey,
    path: PathBuf,
    name: String,
    texture: Option<egui::TextureHandle>,
    frame_size: [usize; 2],
    serial: u64,
    status: PlayerStatus,
    /// When a view last claimed it (app clock).
    last_claimed: f64,
    /// Since when it is paused because its surface is covered.
    covered_since: Option<f64>,
    /// The user paused it (viewer: Space, K, a click).
    user_paused: bool,
    sent: Sent,
    /// What of it reached the screen.
    on_screen: OnScreen,
}

/// How many new frames of a player were painted, and how long it played meanwhile (app clock).
/// It is the figure that tells how smooth a video looked: the engine hands over every frame,
/// whether a repaint shows it or not.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct OnScreen {
    /// Painted frames that showed a new frame of the video.
    pub frames: u64,
    /// Seconds the player was playing.
    pub playing_sec: f64,
    /// The app clock at the last frame of the interface, to add up `playing_sec`.
    last_time: Option<f64>,
    /// A new frame became the texture since the last painted frame. Only a painted frame
    /// (`end_frame`) counts it: a minimized window still runs `begin_frame` without painting.
    fresh: bool,
}

impl OnScreen {
    /// New frames a second while it played.
    pub fn rate(&self) -> f64 {
        if self.playing_sec > 0.0 {
            self.frames as f64 / self.playing_sec
        } else {
            0.0
        }
    }
}

impl Drop for Live {
    fn drop(&mut self) {
        // The counterpart of the player's own statistics line: what the window showed of it.
        if self.on_screen.frames > 0 {
            log::info!("video on screen for {}: {} new frames in {:.1} s of playing ({:.1} a second)", self.name, self.on_screen.frames, self.on_screen.playing_sec, self.on_screen.rate());
        }
    }
}

/// The players of the app.
pub struct VideoHub {
    backend: Box<dyn VideoBackend>,
    caps: VideoCaps,
    ctx: Option<egui::Context>,
    time: f64,
    players: HashMap<Surface, Live>,
    claims: HashMap<Surface, Vec<Claim>>,
    /// The feed video that plays (or played last, while the feed is covered).
    feed_choice: Option<VideoKey>,
    /// Feed videos that failed, with their sentence: they no longer compete, and their card
    /// keeps showing why after the player has gone.
    failed: HashMap<VideoKey, String>,
    /// Files whose failure has been logged.
    logged: HashSet<PathBuf>,
    /// When the next pass started, as the window glue marked it (`mark_pass_start`).
    pass_marked: Option<Instant>,
    /// When the pass being described started.
    pass_started: Option<Instant>,
    /// The repaint the last pass asked for to keep the beat of a software renderer.
    repaint_due: Option<Duration>,
    /// Video frames that became textures since the hub was made (`--frame-log`).
    frames_on_screen: u64,
    /// Since when the window is minimized (`window_minimized`), on the real clock.
    minimized_since: Option<Instant>,
}

impl VideoHub {
    /// A hub with `backend` for an adapter of class `caps`.
    pub fn new(backend: Box<dyn VideoBackend>, caps: VideoCaps) -> VideoHub {
        VideoHub {
            backend,
            caps,
            ctx: None,
            time: 0.0,
            players: HashMap::new(),
            claims: HashMap::new(),
            feed_choice: None,
            failed: HashMap::new(),
            logged: HashSet::new(),
            pass_marked: None,
            pass_started: None,
            repaint_due: None,
            frames_on_screen: 0,
            minimized_since: None,
        }
    }

    /// The window glue says when a pass started, before it waits for the renderer to finish the
    /// last frame: the beat of a software renderer counts from there (`next_repaint_delay`), so
    /// that the wait does not stretch it. Without a mark (headless tests) `begin_frame` is the
    /// start.
    pub fn mark_pass_start(&mut self, at: Instant) {
        self.pass_marked = Some(at);
    }

    /// The delay of the repaint the last pass asked for to keep the beat of a software renderer
    /// while a video plays; `None` when it asked for none.
    pub fn requested_repaint(&self) -> Option<Duration> {
        self.repaint_due
    }

    /// What of the video of `surface` reached the screen so far.
    pub fn on_screen(&self, surface: Surface) -> Option<OnScreen> {
        self.players.get(&surface).map(|live| live.on_screen)
    }

    /// Video frames that became textures since the start (`--frame-log`).
    pub fn frames_on_screen(&self) -> u64 {
        self.frames_on_screen
    }

    /// The adapter class.
    pub fn caps(&self) -> VideoCaps {
        self.caps
    }

    /// Changes the adapter class (known only at the first frame with a window).
    pub fn set_caps(&mut self, caps: VideoCaps) {
        if caps != self.caps {
            self.caps = caps;
            self.close_all();
        }
    }

    /// Call at the start of every frame: the newest frames become textures, the statuses are
    /// read, last frame's claims are forgotten.
    pub fn begin_frame(&mut self, ctx: &egui::Context, time: f64) {
        self.ctx = Some(ctx.clone());
        self.time = time;
        self.claims.clear();
        self.pass_started = Some(self.pass_marked.take().unwrap_or_else(Instant::now));
        for (surface, live) in &mut self.players {
            live.status = live.handle.status();
            if live.status.phase == Phase::Playing {
                live.on_screen.playing_sec += live.on_screen.last_time.map_or(0.0, |last| (time - last).max(0.0));
            }
            live.on_screen.last_time = Some(time);
            if let Some(frame) = live.handle.take_frame(live.serial) {
                live.serial = frame.serial;
                live.frame_size = frame.image.size;
                live.on_screen.fresh = true;
                match &mut live.texture {
                    // `set` moves the pixels; egui uploads them in the paint.
                    Some(texture) => texture.set(frame.image, egui::TextureOptions::LINEAR),
                    None => live.texture = Some(ctx.load_texture(format!("video-{surface:?}"), frame.image, egui::TextureOptions::LINEAR)),
                }
            }
            if live.status.phase == Phase::Failed {
                let failure = live.status.error.clone().unwrap_or_default();
                let sentence = video_rules::classify(&live.name, &live.status.mime, &failure);
                if self.logged.insert(live.path.clone()) {
                    log::warn!("{sentence}");
                }
                if *surface == Surface::Feed {
                    self.failed.insert(live.key.clone(), sentence);
                }
            }
        }
    }

    /// A view shows `key` (the file `path`, named `name`) on `surface`, `ratio` of it on screen
    /// (feed), in a rectangle of `rect_px` pixels.
    pub fn claim(&mut self, surface: Surface, key: VideoKey, path: &Path, name: &str, ratio: f32, rect_px: (u32, u32)) {
        self.claims.entry(surface).or_default().push(Claim { key, path: path.to_path_buf(), name: name.to_owned(), ratio, rect_px });
    }

    /// What `surface` shows of `key` this frame, when its player plays that video.
    pub fn shown(&self, surface: Surface, key: &VideoKey) -> Option<Shown> {
        let live = self.players.get(&surface).filter(|live| &live.key == key)?;
        Some(Shown { texture: live.texture.as_ref().map(|texture| (texture.id(), live.frame_size)), status: live.status.clone(), user_paused: live.user_paused })
    }

    /// The sentence of a feed video that failed before (its card shows it without a player).
    pub fn feed_failure(&self, key: &VideoKey) -> Option<String> {
        self.failed.get(key).cloned()
    }

    /// Play or pause the video of `surface` (the user's choice; viewer).
    pub fn toggle_play(&mut self, surface: Surface) {
        if let Some(live) = self.players.get_mut(&surface) {
            live.user_paused = !live.user_paused;
        }
    }

    /// Seeks the video of `surface` to `seconds`, exactly or to the key frame before.
    pub fn seek(&mut self, surface: Surface, seconds: f64, exact: bool) {
        if let Some(live) = self.players.get_mut(&surface) {
            live.handle.send(Command::Seek(seconds.max(0.0), exact));
            live.status.current_time = seconds.max(0.0);
        }
    }

    /// Auto view's view of the video of `key`: its length once known, whether it ended. A video
    /// that failed has no length (the slide time applies).
    pub fn auto_view_state(&self, key: &VideoKey) -> VideoState {
        match self.players.get(&Surface::AutoView).filter(|live| &live.key == key) {
            Some(live) if live.status.phase == Phase::Failed => VideoState::UNKNOWN,
            Some(live) => VideoState { duration_sec: live.status.duration, ended: live.status.ended_events > 0 || live.status.phase == Phase::Ended },
            None => VideoState::UNKNOWN,
        }
    }

    /// True while a player that should play has not delivered its first frame yet (the
    /// screenshot harness waits for it; a failure counts as done).
    pub fn waiting_for_first_frame(&self) -> bool {
        self.players.values().any(|live| live.sent.playing == Some(true) && live.texture.is_none() && live.status.phase != Phase::Failed)
    }

    /// True while some player plays.
    pub fn is_playing(&self) -> bool {
        self.players.values().any(|live| live.sent.playing == Some(true) && live.status.phase == Phase::Playing)
    }

    /// How many players are open.
    pub fn live_count(&self) -> usize {
        self.players.len()
    }

    /// Closes every player (the collection changed, or the window closes). The threads end on
    /// their own within milliseconds; nothing waits for them.
    pub fn close_all(&mut self) {
        self.players.clear();
        self.feed_choice = None;
    }

    /// Forgets what is known about failed videos (another collection was opened).
    pub fn forget(&mut self) {
        self.close_all();
        self.failed.clear();
        self.logged.clear();
    }

    /// Call in every pass with whether the window is minimized, also in the passes that draw
    /// nothing.
    ///
    /// While the window is minimized eframe runs only the logic half of a pass (`App::logic`):
    /// no view claims its video and `end_frame` is not called, so a video that played would play
    /// on, with its sound (video brief 5.3, 6.4). `end_frame` cannot simply be called there: a
    /// pass without claims closes the viewer's and auto view's players. So here every player is
    /// paused and kept, whatever claimed it. Once the window is restored, the next painted pass
    /// (`end_frame`) plays again what should play then: a video the user paused stays paused,
    /// and auto view's video waits as its clock does (that clock stands still while the window is
    /// minimized). The feed's player is closed after `FEED_KEEP_PAUSED_SEC`, as when something
    /// covers the feed; that time is counted on the real clock (`now`), because egui's clock does
    /// not move in passes that draw nothing.
    pub fn window_minimized(&mut self, minimized: bool, now: Instant) {
        if !minimized {
            self.minimized_since = None;
            return;
        }
        let since = *self.minimized_since.get_or_insert(now);
        for live in self.players.values_mut() {
            if live.sent.playing != Some(false) {
                live.sent.playing = Some(false);
                live.handle.send(Command::Pause);
                log::info!("video paused while the window is minimized: {}", live.name);
            }
        }
        // Nothing plays: no beat of repaints.
        self.repaint_due = None;
        if self.players.contains_key(&Surface::Feed) {
            let kept = Duration::from_secs_f64(FEED_KEEP_PAUSED_SEC);
            let minimized_for = now.saturating_duration_since(since);
            if minimized_for > kept {
                self.players.remove(&Surface::Feed);
                self.feed_choice = None;
            } else if let Some(ctx) = &self.ctx {
                // A paused player wakes nobody: a pass just after the time is up closes it.
                ctx.request_repaint_after(kept - minimized_for + MINIMIZED_CLOSE_MARGIN);
            }
        }
    }

    /// Call at the end of every frame, after the views claimed: decides who plays, opens and
    /// closes players, and sends what changed.
    pub fn end_frame(&mut self, wants: &Wants) {
        // This frame is painted: a new frame taken since the last painted one reaches the screen.
        for live in self.players.values_mut() {
            if std::mem::take(&mut live.on_screen.fresh) {
                live.on_screen.frames += 1;
                self.frames_on_screen += 1;
            }
        }
        let top = if wants.auto_view_open {
            Surface::AutoView
        } else if wants.viewer_open {
            Surface::Viewer
        } else {
            Surface::Feed
        };
        let mut claims = std::mem::take(&mut self.claims);

        // The feed: its most visible video, chosen only while the feed is in charge; covered,
        // the choice stays, so that the same video resumes.
        let feed_claims = claims.remove(&Surface::Feed).unwrap_or_default();
        let feed_in_charge = top == Surface::Feed && wants.feed_uncovered && !wants.minimized;
        if !self.caps.autoplays_feed() {
            self.feed_choice = None;
        } else if feed_in_charge {
            let candidates: Vec<(VideoKey, f32)> = feed_claims.iter().filter(|claim| !self.failed.contains_key(&claim.key)).map(|claim| (claim.key.clone(), claim.ratio)).collect();
            self.feed_choice = video_rules::pick_most_visible(self.feed_choice.as_ref(), &candidates, MIN_VISIBLE_RATIO);
        }
        let feed_wanted = self.feed_choice.as_ref().and_then(|key| feed_claims.iter().find(|claim| &claim.key == key)).cloned();
        self.drive(Surface::Feed, feed_wanted.as_ref(), feed_in_charge, true, wants, &feed_claims);

        let viewer_claims = claims.remove(&Surface::Viewer).unwrap_or_default();
        let viewer_play = top == Surface::Viewer && !wants.minimized;
        self.drive(Surface::Viewer, viewer_claims.first(), viewer_play, true, wants, &viewer_claims);

        let auto_view_claims = claims.remove(&Surface::AutoView).unwrap_or_default();
        let auto_view_play = top == Surface::AutoView && wants.auto_view_playing && !wants.minimized;
        self.drive(Surface::AutoView, auto_view_claims.first(), auto_view_play, wants.auto_view_looping, wants, &auto_view_claims);

        // A software renderer repaints on a beat while a video plays, each repaint with the
        // newest frame (`VideoCaps::next_repaint_delay`); a graphics card repaints per frame.
        let since = self.pass_started.map(|started| started.elapsed()).unwrap_or_default();
        self.repaint_due = if self.is_playing() { self.caps.next_repaint_delay(since) } else { None };
        if let (Some(delay), Some(ctx)) = (self.repaint_due, &self.ctx) {
            ctx.request_repaint_after(delay);
        }
    }

    /// Opens, closes and commands the player of one surface. `wanted` is the video it should
    /// play, `play` whether its surface may play now, `claims` everything the surface showed.
    fn drive(&mut self, surface: Surface, wanted: Option<&Claim>, play: bool, looping: bool, wants: &Wants, claims: &[Claim]) {
        let time = self.time;
        // Another video on this surface: the old player goes first.
        if let Some(claim) = wanted {
            if self.players.get(&surface).is_some_and(|live| live.key != claim.key) {
                self.players.remove(&surface);
            }
            // A feed player starts only when its card becomes the one that plays (no
            // preloading); the viewer's and auto view's start with their slide.
            if !self.players.contains_key(&surface) && (play || surface != Surface::Feed) {
                self.open(surface, claim, play, looping, wants);
            }
        }
        let Some(live) = self.players.get_mut(&surface) else {
            return;
        };
        let claimed = claims.iter().find(|claim| claim.key == live.key);
        if let Some(claim) = claimed {
            live.last_claimed = time;
            if self.caps == VideoCaps::Cpu {
                let changed = live.sent.target.is_none_or(|(w, h)| w.abs_diff(claim.rect_px.0) > TARGET_SIZE_SLACK_PX || h.abs_diff(claim.rect_px.1) > TARGET_SIZE_SLACK_PX);
                if changed {
                    live.sent.target = Some(claim.rect_px);
                    live.handle.send(Command::TargetSize(Some(claim.rect_px)));
                }
            }
        }
        let is_wanted = wanted.is_some_and(|claim| claim.key == live.key);
        // Closing: the viewer's and auto view's at once when their slide is gone; the feed's
        // after a grace time (covered: it resumes where it was). Under auto view the feed is not
        // painted at all, so its chosen video counts as covered without a claim.
        let chosen = self.feed_choice.as_ref() == Some(&live.key);
        let close = match surface {
            Surface::Feed => {
                let covered = !play;
                if covered && (is_wanted || chosen) {
                    let since = *live.covered_since.get_or_insert(time);
                    time - since > FEED_KEEP_PAUSED_SEC
                } else {
                    live.covered_since = None;
                    !is_wanted && time - live.last_claimed > FEED_UNSEEN_CLOSE_SEC && claimed.is_none()
                }
            }
            Surface::Viewer | Surface::AutoView => claimed.is_none(),
        };
        if close {
            self.players.remove(&surface);
            if surface == Surface::Feed && !play {
                self.feed_choice = None;
            }
            return;
        }
        let should_play = play && is_wanted && !live.user_paused && live.status.phase != Phase::Failed;
        if live.sent.playing != Some(should_play) {
            live.sent.playing = Some(should_play);
            live.handle.send(if should_play { Command::Play } else { Command::Pause });
        }
        let muted = !wants.sound_on;
        if live.sent.muted != Some(muted) {
            live.sent.muted = Some(muted);
            live.handle.send(Command::Mute(muted));
        }
        let volume = wants.volume.clamp(0.0, 1.0);
        if live.sent.volume != Some(volume) {
            live.sent.volume = Some(volume);
            live.handle.send(Command::Volume(volume));
        }
        if live.sent.looping != Some(looping) {
            live.sent.looping = Some(looping);
            live.handle.send(Command::Loop(looping));
        }
    }

    /// Starts a player for `claim` on `surface`.
    fn open(&mut self, surface: Surface, claim: &Claim, autoplay: bool, looping: bool, wants: &Wants) {
        let waker = self.waker();
        let options = PlayerOptions {
            autoplay,
            muted: !wants.sound_on,
            looping,
            volume: wants.volume.clamp(0.0, 1.0),
            frame_path: FramePath::for_caps(self.caps),
            caps: self.caps,
            target_size: (self.caps == VideoCaps::Cpu).then_some(claim.rect_px),
            ..PlayerOptions::default()
        };
        let sent = Sent { playing: Some(autoplay), muted: Some(options.muted), volume: Some(options.volume), looping: Some(looping), target: options.target_size };
        match self.backend.open(&claim.path, options, waker) {
            Ok(handle) => {
                let live = Live {
                    handle,
                    key: claim.key.clone(),
                    path: claim.path.clone(),
                    name: claim.name.clone(),
                    texture: None,
                    frame_size: [0, 0],
                    serial: 0,
                    status: PlayerStatus::default(),
                    last_claimed: self.time,
                    covered_since: None,
                    user_paused: false,
                    sent,
                    on_screen: OnScreen::default(),
                };
                self.players.insert(surface, live);
            }
            Err(error) => log::warn!("A video player for {} could not be started: {error}", claim.name),
        }
    }

    /// What wakes the UI for a player: a repaint per frame on a graphics card; on a software
    /// renderer one interval later, which is what brings the first frame, the frame of a seek
    /// while paused and a change of the status. While the video plays, the beat of `end_frame`
    /// comes first: a frame arrives after the pass started, so its wake-up is due later.
    fn waker(&self) -> Waker {
        let ctx = self.ctx.clone();
        let interval = self.caps.repaint_interval_sec();
        std::sync::Arc::new(move || {
            if let Some(ctx) = &ctx {
                match interval {
                    Some(seconds) => ctx.request_repaint_after_secs(seconds as f32),
                    None => ctx.request_repaint(),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wants() -> Wants {
        Wants { feed_uncovered: true, viewer_open: false, auto_view_open: false, auto_view_playing: false, auto_view_looping: true, minimized: false, sound_on: false, volume: 1.0 }
    }

    fn key(name: &str) -> VideoKey {
        VideoKey::new(&PostKey::from_stored(format!("file|{name}")), 0)
    }

    fn frame(hub: &mut VideoHub, ctx: &egui::Context, time: f64, claims: &[(Surface, &str, f32)], wants: &Wants) {
        hub.begin_frame(ctx, time);
        for (surface, name, ratio) in claims {
            hub.claim(*surface, key(name), Path::new(name), name, *ratio, (640, 360));
        }
        hub.end_frame(wants);
    }

    #[test]
    fn only_the_most_visible_feed_video_plays_on_a_graphics_card() {
        let backend = FakeBackend::default();
        let mut hub = VideoHub::new(Box::new(backend.clone()), VideoCaps::Gpu);
        let ctx = egui::Context::default();
        frame(&mut hub, &ctx, 0.0, &[(Surface::Feed, "a.mp4", 0.9), (Surface::Feed, "b.mp4", 0.2)], &wants());
        assert_eq!(backend.log().opened.len(), 1);
        assert_eq!(backend.log().opened[0].0, PathBuf::from("a.mp4"));
        assert!(backend.log().opened[0].1.autoplay && backend.log().opened[0].1.looping && backend.log().opened[0].1.muted);
        // Scrolling hands over: one feed player at a time.
        frame(&mut hub, &ctx, 1.0, &[(Surface::Feed, "a.mp4", 0.3), (Surface::Feed, "b.mp4", 0.8)], &wants());
        assert_eq!(backend.log().closed, [PathBuf::from("a.mp4")]);
        assert_eq!(backend.log().opened[1].0, PathBuf::from("b.mp4"));
        assert_eq!(hub.live_count(), 1);
    }

    #[test]
    fn the_viewer_takes_over_and_the_feed_resumes_where_it_was() {
        let backend = FakeBackend::default();
        let mut hub = VideoHub::new(Box::new(backend.clone()), VideoCaps::Gpu);
        let ctx = egui::Context::default();
        frame(&mut hub, &ctx, 0.0, &[(Surface::Feed, "a.mp4", 1.0)], &wants());
        let covered = Wants { feed_uncovered: false, viewer_open: true, ..wants() };
        frame(&mut hub, &ctx, 1.0, &[(Surface::Feed, "a.mp4", 1.0), (Surface::Viewer, "v.mp4", 1.0)], &covered);
        assert_eq!(backend.log().commands_for("a.mp4"), [Command::Pause]);
        assert_eq!(hub.live_count(), 2);
        assert!(backend.log().opened[1].1.autoplay, "the viewer plays at once");
        // The viewer closes: its player goes, the feed plays on with the same player.
        frame(&mut hub, &ctx, 2.0, &[(Surface::Feed, "a.mp4", 1.0)], &wants());
        assert_eq!(backend.log().closed, [PathBuf::from("v.mp4")]);
        assert_eq!(backend.log().commands_for("a.mp4"), [Command::Pause, Command::Play]);
        // Covered for longer than the grace time, the feed's player is closed.
        frame(&mut hub, &ctx, 3.0, &[(Surface::Feed, "a.mp4", 1.0), (Surface::Viewer, "v.mp4", 1.0)], &covered);
        frame(&mut hub, &ctx, 3.0 + FEED_KEEP_PAUSED_SEC + 1.0, &[(Surface::Feed, "a.mp4", 1.0), (Surface::Viewer, "v.mp4", 1.0)], &covered);
        assert!(backend.log().closed.contains(&PathBuf::from("a.mp4")));
        // Auto view does not paint the feed: its video is kept all the same, then closed.
        frame(&mut hub, &ctx, 40.0, &[(Surface::Feed, "a.mp4", 1.0)], &wants());
        let auto_view = Wants { feed_uncovered: false, auto_view_open: true, auto_view_playing: true, ..wants() };
        frame(&mut hub, &ctx, 41.0, &[], &auto_view);
        frame(&mut hub, &ctx, 41.0 + FEED_UNSEEN_CLOSE_SEC + 1.0, &[], &auto_view);
        assert_eq!(backend.log().closed.iter().filter(|path| path.ends_with("a.mp4")).count(), 1, "kept for the grace time of a covered feed");
        frame(&mut hub, &ctx, 41.0 + FEED_KEEP_PAUSED_SEC + 1.0, &[], &auto_view);
        assert_eq!(backend.log().closed.iter().filter(|path| path.ends_with("a.mp4")).count(), 2);
    }

    #[test]
    fn a_software_renderer_never_autoplays_the_feed() {
        let backend = FakeBackend::default();
        let mut hub = VideoHub::new(Box::new(backend.clone()), VideoCaps::Cpu);
        let ctx = egui::Context::default();
        frame(&mut hub, &ctx, 0.0, &[(Surface::Feed, "a.mp4", 1.0)], &wants());
        assert!(backend.log().opened.is_empty());
        let viewer = Wants { feed_uncovered: false, viewer_open: true, ..wants() };
        frame(&mut hub, &ctx, 1.0, &[(Surface::Viewer, "a.mp4", 1.0)], &viewer);
        let log = backend.log();
        assert_eq!(log.opened.len(), 1);
        assert_eq!(log.opened[0].1.target_size, Some((640, 360)), "frames are capped to the rectangle on screen");
        assert_eq!(log.opened[0].1.frame_path, FramePath::Wic);
    }

    #[test]
    fn a_software_renderer_repaints_on_a_beat_while_its_video_plays() {
        let backend = FakeBackend::default();
        let mut hub = VideoHub::new(Box::new(backend.clone()), VideoCaps::Cpu);
        let ctx = egui::Context::default();
        let asked: std::sync::Arc<std::sync::Mutex<Vec<Duration>>> = std::sync::Arc::default();
        let recorder = std::sync::Arc::clone(&asked);
        ctx.set_request_repaint_callback(move |info| recorder.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(info.delay));
        let viewer = Wants { feed_uncovered: false, viewer_open: true, ..wants() };
        let claims = [(Surface::Viewer, "v.mp4", 1.0)];
        let interval = Duration::from_millis(100);
        let ago = |ms: u64| Instant::now().checked_sub(Duration::from_millis(ms)).expect("the clock runs");
        // The pass that opens the player does not know yet that it plays.
        frame(&mut hub, &ctx, 0.0, &claims, &viewer);
        assert_eq!(hub.requested_repaint(), None);
        // Playing: the next repaint is one interval after the start of this pass.
        frame(&mut hub, &ctx, 0.1, &claims, &viewer);
        assert!(hub.requested_repaint().is_some_and(|due| due <= interval && due >= video_rules::MIN_REPAINT_DELAY), "{:?}", hub.requested_repaint());
        // The window glue marks the start before it waits for the renderer; the beat counts
        // from there, and the request reaches egui.
        hub.mark_pass_start(ago(60));
        frame(&mut hub, &ctx, 0.2, &claims, &viewer);
        let due = hub.requested_repaint().expect("a beat");
        assert!(due <= Duration::from_millis(40), "{due:?}");
        assert_eq!(asked.lock().map(|asked| asked.last().copied()).ok().flatten(), Some(due));
        // A pass longer than the interval is followed at once.
        hub.mark_pass_start(ago(250));
        frame(&mut hub, &ctx, 0.3, &claims, &viewer);
        assert_eq!(hub.requested_repaint(), Some(video_rules::MIN_REPAINT_DELAY));
        // Paused, or minimized: no beat.
        hub.toggle_play(Surface::Viewer);
        frame(&mut hub, &ctx, 0.4, &claims, &viewer);
        frame(&mut hub, &ctx, 0.5, &claims, &viewer);
        assert_eq!(hub.requested_repaint(), None);
        hub.toggle_play(Surface::Viewer);
        frame(&mut hub, &ctx, 0.6, &claims, &viewer);
        frame(&mut hub, &ctx, 0.7, &claims, &viewer);
        assert!(hub.requested_repaint().is_some());
        let minimized = Wants { minimized: true, ..viewer };
        frame(&mut hub, &ctx, 0.8, &claims, &minimized);
        frame(&mut hub, &ctx, 0.9, &claims, &minimized);
        assert_eq!(hub.requested_repaint(), None);

        // A graphics card repaints for every frame (the waker), never on a beat.
        let gpu_backend = FakeBackend::default();
        let mut gpu = VideoHub::new(Box::new(gpu_backend), VideoCaps::Gpu);
        frame(&mut gpu, &ctx, 0.0, &claims, &viewer);
        frame(&mut gpu, &ctx, 0.1, &claims, &viewer);
        assert!(gpu.is_playing());
        assert_eq!(gpu.requested_repaint(), None);
    }

    #[test]
    fn a_minimized_window_pauses_every_player_and_closes_none() {
        let backend = FakeBackend::default();
        let mut hub = VideoHub::new(Box::new(backend.clone()), VideoCaps::Gpu);
        let ctx = egui::Context::default();
        // The viewer plays its video over the feed's, which waits under it.
        frame(&mut hub, &ctx, 0.0, &[(Surface::Feed, "a.mp4", 1.0)], &wants());
        let viewer = Wants { feed_uncovered: false, viewer_open: true, ..wants() };
        let claims = [(Surface::Feed, "a.mp4", 1.0), (Surface::Viewer, "v.mp4", 1.0)];
        frame(&mut hub, &ctx, 1.0, &claims, &viewer);
        frame(&mut hub, &ctx, 1.1, &claims, &viewer);
        assert!(backend.log().commands_for("v.mp4").is_empty() && hub.is_playing());
        assert_eq!(backend.log().commands_for("a.mp4"), [Command::Pause]);
        // Minimized: eframe runs only the logic half of each pass, so nothing claims and
        // `end_frame` is not called; the clock of egui stands still meanwhile.
        let start = Instant::now();
        for seconds in [0, 1, 20] {
            hub.begin_frame(&ctx, 1.0);
            hub.window_minimized(true, start + Duration::from_secs(seconds));
        }
        assert_eq!(backend.log().commands_for("v.mp4"), [Command::Pause], "paused, once");
        assert_eq!(backend.log().commands_for("a.mp4"), [Command::Pause], "it was paused already");
        assert_eq!(hub.live_count(), 2, "nothing is closed");
        assert!(!hub.is_playing());
        assert_eq!(hub.requested_repaint(), None);
        // Restored: the next painted pass plays the viewer's video again, the feed's waits.
        hub.window_minimized(false, start + Duration::from_secs(21));
        frame(&mut hub, &ctx, 2.0, &claims, &viewer);
        assert_eq!(backend.log().commands_for("v.mp4"), [Command::Pause, Command::Play]);
        assert_eq!(backend.log().commands_for("a.mp4"), [Command::Pause]);
        assert!(backend.log().closed.is_empty());
        // A video the user paused stays paused.
        hub.toggle_play(Surface::Viewer);
        frame(&mut hub, &ctx, 2.1, &claims, &viewer);
        hub.begin_frame(&ctx, 2.1);
        hub.window_minimized(true, start + Duration::from_secs(22));
        hub.window_minimized(false, start + Duration::from_secs(23));
        frame(&mut hub, &ctx, 2.2, &claims, &viewer);
        assert_eq!(backend.log().commands_for("v.mp4"), [Command::Pause, Command::Play, Command::Pause]);
        assert!(hub.shown(Surface::Viewer, &key("v.mp4")).is_some_and(|shown| shown.user_paused));
    }

    #[test]
    fn the_feed_video_of_a_minimized_window_is_closed_after_the_grace_time() {
        let backend = FakeBackend::default();
        let mut hub = VideoHub::new(Box::new(backend.clone()), VideoCaps::Gpu);
        let ctx = egui::Context::default();
        let asked: std::sync::Arc<std::sync::Mutex<Vec<Duration>>> = std::sync::Arc::default();
        let recorder = std::sync::Arc::clone(&asked);
        ctx.set_request_repaint_callback(move |info| recorder.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).push(info.delay));
        let claims = [(Surface::Feed, "a.mp4", 1.0)];
        frame(&mut hub, &ctx, 0.0, &claims, &wants());
        let kept = Duration::from_secs_f64(FEED_KEEP_PAUSED_SEC);
        let start = Instant::now();
        // Passes of a minimized window as eframe runs them: only the logic half. egui reports a
        // request only when it is sooner than the one waiting, and the player's wake-up asked
        // for two passes at once; each pass asks again, so the last one is the one that counts.
        for _ in 0..2 {
            let _ = ctx.clone().run_logic(&egui::RawInput::default(), |ctx| {
                hub.begin_frame(ctx, 0.0);
                hub.window_minimized(true, start);
            });
        }
        assert_eq!(backend.log().commands_for("a.mp4"), [Command::Pause]);
        // A paused player wakes nobody: a pass is asked for just after the grace time.
        let due = asked.lock().map(|asked| asked.last().copied()).ok().flatten().expect("a pass is asked for");
        assert!(due > kept && due <= kept + Duration::from_secs(1), "{due:?}");
        // Back within the grace time, the same player plays on.
        hub.window_minimized(true, start + Duration::from_secs(10));
        assert_eq!(hub.live_count(), 1);
        hub.window_minimized(false, start + Duration::from_secs(11));
        frame(&mut hub, &ctx, 0.1, &claims, &wants());
        assert_eq!(backend.log().commands_for("a.mp4"), [Command::Pause, Command::Play]);
        assert_eq!(backend.log().opened.len(), 1);
        // Minimized for longer, it is closed; restored, the feed starts its video again.
        let later = start + Duration::from_secs(60);
        hub.begin_frame(&ctx, 0.1);
        hub.window_minimized(true, later);
        hub.window_minimized(true, later + kept + Duration::from_secs(1));
        assert_eq!(backend.log().closed, [PathBuf::from("a.mp4")]);
        assert_eq!(hub.live_count(), 0);
        hub.window_minimized(false, later + kept + Duration::from_secs(2));
        frame(&mut hub, &ctx, 0.2, &claims, &wants());
        assert_eq!(backend.log().opened.len(), 2);
        assert!(backend.log().opened[1].1.autoplay, "and it plays");
    }

    #[test]
    fn the_frames_that_reach_the_screen_are_counted_per_second_of_playing() {
        let backend = FakeBackend::default();
        let mut hub = VideoHub::new(Box::new(backend.clone()), VideoCaps::Cpu);
        let ctx = egui::Context::default();
        let viewer = Wants { feed_uncovered: false, viewer_open: true, ..wants() };
        let claims = [(Surface::Viewer, "v.mp4", 1.0)];
        frame(&mut hub, &ctx, 0.0, &claims, &viewer);
        // The first frame.
        frame(&mut hub, &ctx, 0.1, &claims, &viewer);
        assert_eq!(hub.on_screen(Surface::Viewer).map(|seen| seen.frames), Some(1));
        // Two frames arrived between two repaints: only the newest becomes the texture.
        backend.log().push_frame("v.mp4");
        backend.log().push_frame("v.mp4");
        frame(&mut hub, &ctx, 0.2, &claims, &viewer);
        // Nothing new: nothing counted.
        frame(&mut hub, &ctx, 0.3, &claims, &viewer);
        let seen = hub.on_screen(Surface::Viewer).expect("the viewer's player");
        assert_eq!(seen.frames, 2);
        assert!((seen.playing_sec - 0.2).abs() < 1e-9, "{}", seen.playing_sec);
        assert!((seen.rate() - 10.0).abs() < 1e-6, "{}", seen.rate());
        assert_eq!(hub.frames_on_screen(), 2);
        // Passes that are not painted (a minimized window runs only the logic half) take frames
        // but show none: the next painted frame counts one.
        backend.log().push_frame("v.mp4");
        hub.begin_frame(&ctx, 0.31);
        backend.log().push_frame("v.mp4");
        hub.begin_frame(&ctx, 0.32);
        assert_eq!(hub.frames_on_screen(), 2);
        frame(&mut hub, &ctx, 0.33, &claims, &viewer);
        assert_eq!(hub.frames_on_screen(), 3);
        // Paused time does not count.
        hub.toggle_play(Surface::Viewer);
        frame(&mut hub, &ctx, 0.4, &claims, &viewer);
        frame(&mut hub, &ctx, 1.4, &claims, &viewer);
        assert!(hub.on_screen(Surface::Viewer).is_some_and(|seen| seen.playing_sec < 0.31), "{:?}", hub.on_screen(Surface::Viewer));
        assert_eq!(OnScreen::default().rate(), 0.0);
    }

    #[test]
    fn sound_volume_and_loop_follow_the_wishes() {
        let backend = FakeBackend::default();
        let mut hub = VideoHub::new(Box::new(backend.clone()), VideoCaps::Gpu);
        let ctx = egui::Context::default();
        let auto_view = Wants { feed_uncovered: false, auto_view_open: true, auto_view_playing: true, auto_view_looping: false, ..wants() };
        frame(&mut hub, &ctx, 0.0, &[(Surface::AutoView, "a.mp4", 1.0)], &auto_view);
        assert!(!backend.log().opened[0].1.looping);
        let louder = Wants { sound_on: true, volume: 0.5, auto_view_looping: true, ..auto_view };
        frame(&mut hub, &ctx, 0.1, &[(Surface::AutoView, "a.mp4", 1.0)], &louder);
        assert_eq!(backend.log().commands_for("a.mp4"), [Command::Mute(false), Command::Volume(0.5), Command::Loop(true)]);
        // Space pauses auto view, and with it the video.
        let paused = Wants { auto_view_playing: false, ..louder };
        frame(&mut hub, &ctx, 0.2, &[(Surface::AutoView, "a.mp4", 1.0)], &paused);
        assert_eq!(backend.log().commands_for("a.mp4").last(), Some(&Command::Pause));
        // The end screen claims nothing: the player goes.
        frame(&mut hub, &ctx, 0.3, &[], &paused);
        assert_eq!(hub.live_count(), 0);
    }

    #[test]
    fn the_user_pauses_the_viewer_and_a_failed_feed_video_stops_competing() {
        let backend = FakeBackend::default();
        backend.log().scripts.insert("bad.mp4".to_owned(), FakeScript { failure: Some(video_rules::PlaybackFailure { media_error: 4, hresult: video_rules::HRESULT_UNSUPPORTED_BYTESTREAM, text: "HRESULT 0xC00D36C4: Unsupported.".to_owned(), codec: None, no_picture: false }), ..FakeScript::default() });
        let mut hub = VideoHub::new(Box::new(backend.clone()), VideoCaps::Gpu);
        let ctx = egui::Context::default();
        frame(&mut hub, &ctx, 0.0, &[(Surface::Feed, "bad.mp4", 1.0), (Surface::Feed, "good.mp4", 0.5)], &wants());
        frame(&mut hub, &ctx, 0.1, &[(Surface::Feed, "bad.mp4", 1.0), (Surface::Feed, "good.mp4", 0.5)], &wants());
        let shown = hub.shown(Surface::Feed, &key("good.mp4")).expect("the next one plays");
        assert!(shown.is_playing());
        assert!(hub.feed_failure(&key("bad.mp4")).is_some_and(|sentence| sentence.starts_with("\"bad.mp4\" can't be played: ")));
        let viewer = Wants { feed_uncovered: false, viewer_open: true, ..wants() };
        frame(&mut hub, &ctx, 1.0, &[(Surface::Viewer, "v.mp4", 1.0)], &viewer);
        hub.toggle_play(Surface::Viewer);
        frame(&mut hub, &ctx, 1.1, &[(Surface::Viewer, "v.mp4", 1.0)], &viewer);
        assert_eq!(backend.log().commands_for("v.mp4").last(), Some(&Command::Pause));
        hub.seek(Surface::Viewer, 3.0, true);
        assert_eq!(backend.log().commands_for("v.mp4").last(), Some(&Command::Seek(3.0, true)));
        assert!(hub.shown(Surface::Viewer, &key("v.mp4")).is_some_and(|shown| shown.texture.is_some()), "the fake's frame arrived");
    }
}
