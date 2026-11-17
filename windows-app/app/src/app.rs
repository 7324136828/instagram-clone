//! The application: one frame of logic and one frame of painting, and the glue to eframe.
//!
//! `App` holds the state (`state.rs`), the services the state's `apply` works with, and the
//! views. A frame runs in two halves, both of which also work without a window so that the
//! headless tests can drive the whole app:
//!
//! * `frame_logic`: take over what the background workers finished, carry out the changes a
//!   browser asked for and publish what is open for the browser pages (this half runs also
//!   while the window is hidden or minimised), read dropped files and global shortcuts, expire
//!   toasts.
//! * `frame_ui`: paint, back to front: page background, sidebar, main view (feed, grid or
//!   empty state), then the layers above them: More menu (inside the sidebar code), the viewer,
//!   the one dialog that may be open (post options, description editor, warnings, About),
//!   toasts and the busy chip, and the drop overlay on top of everything. The actions the
//!   views collected are applied at the end of the frame, in one place.
//!
//! The `eframe::App` implementation adds what needs the real window: native dialogs (they need
//! the window as their parent), the screenshot harness, and the exit guard.

use std::path::PathBuf;
use std::time::Instant;

use egui::{pos2, Key, KeyboardShortcut, Modifiers, Rect, Ui};
use instagram_core::archive::cache::ArchiveCache;
use instagram_core::autoview::AutoViewRequest;
use instagram_core::catalog::OpenContext;
use instagram_core::settings::Settings;
use instagram_core::poster::PosterCache;
use instagram_core::store::{DescriptionStore, Theme, UiState};
use instagram_core::text;
use instagram_core::video_rules::VideoCaps;

use crate::autoview::{self, AutoViewView};
use crate::browser;
use crate::download::Downloader;
use crate::editor;
use crate::explore::ExploreView;
use crate::feed::FeedView;
use crate::harness::{Driver, HarnessOptions};
use crate::loader::Loader;
use crate::opening::Opener;
use crate::placement;
use crate::rendering;
use crate::sheets;
use crate::shell;
use crate::state::{self, Action, AppState, Env, Services, View};
use crate::strings;
use crate::theme;
use crate::video::{MfBackend, VideoBackend, VideoHub, VideoKey, Wants};
use crate::viewer::ViewerView;
use crate::web::{WebConfig, WebService};
use crate::web_assets;
use crate::widgets;

/// After the window closed, the process ends by itself at the latest after this long. Tearing
/// down the GPU device can block forever on a machine without a real GPU (toolchain spike,
/// pitfall 2); everything worth keeping has been saved by then (descriptions and state.json are
/// written when they change, window.ron and the log before the guard starts), so the guard
/// need not wait long.
pub const EXIT_GUARD_MS: u64 = 500;
/// On a software rasteriser the app waits at most this long for the previous frame to be drawn
/// before it goes on with the next one.
const RENDERER_WAIT_MS: u64 = 1000;
/// A download that is running when the window closes gets this long to stop and to remove its
/// temporary files. It looks at its cancel flag before every 4 MiB it copies, so it needs a
/// fraction of this unless the disk is very slow.
const DOWNLOAD_CANCEL_WAIT_MS: u64 = 5000;
/// The app counts as idle when this many frames have been painted since the last action was
/// applied. A dialog that an action opened is laid out invisibly in its first frame (egui
/// measures it before it shows it), so what is on screen is final only a frame later.
const QUIET_FRAMES_WHEN_IDLE: u32 = 2;

/// Creates the data folder of `paths`. `Err` holds the sentence of a fatal settings error
/// (`messages::settings_data_directory_error`): without the folder no description can be saved,
/// and the log, state.json and window.ron would be lost without a word.
pub fn prepare_data_folder(paths: &instagram_core::settings::AppPaths) -> Result<(), String> {
    std::fs::create_dir_all(&paths.data_dir).map_err(|error| {
        let reason = instagram_core::messages::data_folder_not_created(&paths.data_dir.display().to_string(), &error.to_string());
        instagram_core::messages::settings_data_directory_error(&paths.settings_file.display().to_string(), &reason)
    })
}

/// What the app keeps of the window geometry between runs (see `placement.rs`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct WindowGeometry {
    /// eframe stores the geometry in window.ron (false in screenshot runs and tests).
    pub keeps: bool,
    /// The last usable geometry (eframe's RON text): what is stored while the window is
    /// minimised, when eframe's own reading is of no use.
    pub last_usable: Option<String>,
}

/// When the process started, for the two lines of the log that say how long the first frame
/// and the first picture took. `main` sets it before anything else.
static PROCESS_STARTED: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();

/// Remembers this moment as the start of the process (the first call counts).
pub fn mark_process_start() {
    let _ = PROCESS_STARTED.set(Instant::now());
}

/// What of the start has been written to the log already.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum StartMilestone {
    Nothing,
    FirstFrame,
    FirstPicture,
}

/// Everything `App::new` needs. `AppConfig::load` builds it from the settings; tests build it
/// for a profile folder of their own.
pub struct AppConfig {
    /// Where the app's files are and who the author of loose pictures is.
    pub settings: Settings,
    /// What state.json remembered.
    pub remembered: UiState,
    /// The descriptions typed so far.
    pub store: DescriptionStore,
    /// A message about the descriptions file to show once (it was damaged and set aside, or
    /// could not be read).
    pub store_notice: Option<String>,
    /// An appearance for this run only (`--theme`).
    pub theme_override: Option<Theme>,
    /// Paths to open at the start (command line).
    pub paths: Vec<PathBuf>,
    /// `--autoview` of the command line.
    pub auto_view: Option<AutoViewRequest>,
    /// False to jump every animation to its end (screenshots, tests).
    pub animations: bool,
    /// The hidden harness options.
    pub harness: HarnessOptions,
    /// The browser pages of this run; `None` starts none (the headless tests, screenshots without
    /// an address on the command line), though the Browser view sheet can still turn them on.
    pub web: Option<WebConfig>,
    /// Where video players come from; `None`: Media Foundation (tests put a fake here).
    pub video_backend: Option<Box<dyn VideoBackend>>,
    /// The class of the display adapter for videos; `None`: read from the wgpu adapter at the
    /// first frame (a software renderer until then, and in the headless tests). The hidden
    /// harness option `--video-adapter` sets it.
    pub video_caps: Option<VideoCaps>,
}

impl AppConfig {
    /// Reads state.json and descriptions.json of `settings`. Neither can fail the start: a
    /// damaged file gives the defaults, and a notice for the user where text may be affected.
    pub fn load(settings: Settings) -> AppConfig {
        let remembered = UiState::load(&settings.paths.state_file);
        let (store, store_notice) = match DescriptionStore::load(&settings.paths.descriptions_file, text::now_ms()) {
            Ok(loaded) => (loaded.store, loaded.notice),
            Err(error) => (DescriptionStore::new(settings.paths.descriptions_file.clone()), Some(error.message)),
        };
        AppConfig { settings, remembered, store, store_notice, theme_override: None, paths: Vec::new(), auto_view: None, animations: true, harness: HarnessOptions::default(), web: None, video_backend: None, video_caps: None }
    }
}

/// How long the frames took, for the log: the time spent in this app's own code per frame
/// (logic and painting commands), and eframe's figure for the whole frame including tessellation
/// and rendering.
#[derive(Debug, Clone, Default)]
pub struct FrameStats {
    /// Milliseconds of `frame_logic` + `frame_ui`, one entry per painted frame.
    app_ms: Vec<f32>,
    /// Milliseconds eframe reported for whole frames.
    total_ms: Vec<f32>,
    /// Milliseconds the app waited for a software rasteriser to draw the previous frame.
    renderer_wait_ms: Vec<f32>,
}

impl FrameStats {
    /// Frames beyond this are not recorded; the summary of a long session stays cheap.
    const MAX_SAMPLES: usize = 20_000;

    /// Forgets everything recorded so far.
    pub fn reset(&mut self) {
        self.app_ms.clear();
        self.total_ms.clear();
        self.renderer_wait_ms.clear();
    }

    fn record_app(&mut self, milliseconds: f32) {
        if self.app_ms.len() < Self::MAX_SAMPLES {
            self.app_ms.push(milliseconds);
        }
    }

    /// Records how long the app waited for the renderer before a frame.
    pub fn record_renderer_wait(&mut self, milliseconds: f32) {
        if self.renderer_wait_ms.len() < Self::MAX_SAMPLES {
            self.renderer_wait_ms.push(milliseconds);
        }
    }

    /// Records eframe's CPU time of the previous whole frame, in seconds.
    pub fn record_total(&mut self, seconds: Option<f32>) {
        if let Some(seconds) = seconds {
            if self.total_ms.len() < Self::MAX_SAMPLES {
                self.total_ms.push(seconds * 1000.0);
            }
        }
    }

    /// Number of painted frames recorded.
    pub fn frames(&self) -> usize {
        self.app_ms.len()
    }

    /// "avg 1.2 ms, p95 2.0 ms, max 6.1 ms" of a series, or "no frames".
    pub fn describe(samples: &[f32]) -> String {
        if samples.is_empty() {
            return "no frames".to_owned();
        }
        let mut sorted = samples.to_vec();
        sorted.sort_by(|a, b| a.total_cmp(b));
        let average = sorted.iter().sum::<f32>() / sorted.len() as f32;
        let percentile = sorted[((sorted.len() as f32 * 0.95) as usize).min(sorted.len() - 1)];
        format!("avg {average:.1} ms, p95 {percentile:.1} ms, max {:.1} ms", sorted[sorted.len() - 1])
    }

    /// Writes the summary to the log.
    pub fn log(&self, what: &str) {
        log::info!("Frame timings ({what}): {} frames; app code {}; whole frame as eframe reports it: {}.", self.frames(), Self::describe(&self.app_ms), Self::describe(&self.total_ms));
        if !self.renderer_wait_ms.is_empty() {
            log::info!("Software rendering ({what}): waited for the previous frame to be drawn: {}.", Self::describe(&self.renderer_wait_ms));
        }
    }
}

/// Counts the frames of a stretch of time for the log (`--frame-log`). An app that has nothing
/// to animate runs no frames at all, so a line of the log may cover a long time: that is the
/// point of it.
#[derive(Debug, Clone)]
struct FrameRate {
    since: Instant,
    logic_passes: u32,
    painted_frames: u32,
    /// `VideoHub::frames_on_screen` when this stretch began.
    video_frames_before: u64,
}

impl FrameRate {
    /// A line is written when this much time has passed since the last one.
    const REPORT_SECONDS: f32 = 5.0;

    fn new(video_frames: u64) -> FrameRate {
        FrameRate { since: Instant::now(), logic_passes: 0, painted_frames: 0, video_frames_before: video_frames }
    }

    /// One pass of the logic half of a frame; writes the line that is due first. `video_frames`
    /// is the hub's count of video frames that became textures so far.
    fn count_logic(&mut self, video_frames: u64) {
        if self.since.elapsed().as_secs_f32() >= Self::REPORT_SECONDS {
            self.report(video_frames);
        }
        self.logic_passes += 1;
    }

    fn count_painted(&mut self) {
        self.painted_frames += 1;
    }

    /// The line of a stretch of `seconds`. The new video frames are the figure for how smooth a
    /// video looked: the engine hands over every frame, but only a repaint shows one.
    fn line(&self, seconds: f32, video_frames: u64) -> String {
        let per_second = |count: f64| count / f64::from(seconds.max(0.001));
        let mut line = format!("Frame rate: {} logic passes and {} painted frames in the last {seconds:.1} s ({:.1} painted frames per second", self.logic_passes, self.painted_frames, per_second(f64::from(self.painted_frames)));
        let new_frames = video_frames.saturating_sub(self.video_frames_before);
        if new_frames > 0 {
            line.push_str(&format!("; {new_frames} new video frames, {:.1} per second", per_second(new_frames as f64)));
        }
        line.push_str(").");
        line
    }

    /// Writes how many frames ran since the last line and starts counting again.
    fn report(&mut self, video_frames: u64) {
        log::info!("{}", self.line(self.since.elapsed().as_secs_f32(), video_frames));
        *self = FrameRate::new(video_frames);
    }
}

/// The application.
pub struct App {
    /// What the interface knows.
    pub state: AppState,
    /// Files, background workers and the window handle for `state::apply`.
    pub services: Services,
    /// The home feed.
    pub feed: FeedView,
    /// The Explore grid.
    pub explore: ExploreView,
    /// The viewer: the zoom of the picture on show.
    pub viewer: ViewerView,
    /// The running auto view.
    pub auto_view: AutoViewView,
    /// Frame timings for the log.
    pub frame_stats: FrameStats,
    /// The harness shows the drop overlay without a real drag (`drop-overlay`).
    pub forced_drop_hover: bool,
    /// What is kept of the window geometry; `main` fills it in.
    pub window_geometry: WindowGeometry,
    /// The window was hidden because it is closing.
    hidden_for_close: bool,
    /// The app fills the window with the page colour itself. True without a window (the
    /// headless tests); the window glue switches it off, because eframe clears to that colour.
    pub paints_background: bool,
    /// The last painted frame was auto view alone, and the window is cleared to black for it.
    pub cleared_to_black: bool,
    /// Actions collected by the views in this frame.
    actions: Vec<Action>,
    /// Paths of the command line, opened at the first frame.
    startup_paths: Vec<PathBuf>,
    /// The appearance the egui context was last switched to.
    applied_theme: Option<Theme>,
    /// The title the window was last given.
    applied_title: Option<String>,
    /// Whether the window filled the screen when the app last looked. A change that did not
    /// come from this app (the state remembered in window.ron, the system) is taken over.
    seen_fullscreen: Option<bool>,
    /// Painted frames since the last action was applied.
    quiet_frames: u32,
    /// The harness driver of this run, if any.
    driver: Option<Driver>,
    frame_started: Option<Instant>,
    /// The kind of renderer has been looked up (once, at the first frame with a window).
    renderer_known: bool,
    /// How far the log has followed the start of the app.
    start_milestone: StartMilestone,
    /// `--frame-log`: how many frames ran, written to the log every few seconds.
    frame_rate: Option<FrameRate>,
    /// The clean-up of the archive cache at the start has been set off (at the first frame:
    /// `App::new` starts no thread).
    cache_cleaned: bool,
    /// The video adapter class was given by the configuration: the wgpu adapter does not change it.
    video_caps_fixed: bool,
}

impl App {
    /// Builds the app. Nothing is drawn and no thread is started yet; the egui context is
    /// attached at the first frame.
    pub fn new(config: AppConfig) -> App {
        let loader = Loader::new();
        let opener = Opener::new(loader.waker());
        let downloader = Downloader::new(loader.waker());
        let mut remembered = config.remembered;
        if let Some(theme) = config.theme_override {
            remembered.theme = theme;
        }
        let mut state = AppState::new(remembered);
        state.animations = config.animations;
        state.auto_view_request = config.auto_view;
        if let Some(notice) = config.store_notice {
            // Every description is missing and the old file is somewhere: a toast would be gone
            // before the user has read it, so the notice stays until it is closed.
            log::warn!("{notice}");
            state.confirm = Some(state::ConfirmState { kind: state::ConfirmKind::StoreNotice(notice), opened_at: 0.0 });
        }
        let log_frame_rate = config.harness.frame_log;
        let driver = if config.harness.is_active() || config.harness.frame_log { Some(Driver::new(config.harness)) } else { None };
        let web_config = config.web.unwrap_or_else(|| WebConfig::not_started(config.settings.web.clone(), web_assets::WEB_FILES));
        let paths = config.settings.paths;
        let web = WebService::new(web_config, &paths);
        let context = OpenContext { archive_cache: ArchiveCache::new(paths.archive_cache_dir.clone()), library_dir: Some(paths.library_dir.clone()) };
        let mut loader = loader;
        loader.set_poster_cache(PosterCache::new(paths.poster_cache_dir.clone()));
        let mut downloader = downloader;
        downloader.set_poster_cache(PosterCache::new(paths.poster_cache_dir.clone()));
        let video_caps_fixed = config.video_caps.is_some();
        let video = VideoHub::new(config.video_backend.unwrap_or_else(|| Box::new(MfBackend)), config.video_caps.unwrap_or(VideoCaps::Cpu));
        App {
            state,
            services: Services { ctx: None, paths, default_username: config.settings.default_username, store: config.store, loader, opener, downloader, context, leases: Vec::new(), web, video },
            cache_cleaned: false,
            video_caps_fixed,
            feed: FeedView::default(),
            explore: ExploreView::default(),
            viewer: ViewerView::default(),
            auto_view: AutoViewView::default(),
            frame_stats: FrameStats::default(),
            forced_drop_hover: false,
            window_geometry: WindowGeometry::default(),
            hidden_for_close: false,
            paints_background: true,
            cleared_to_black: false,
            actions: Vec::new(),
            startup_paths: config.paths,
            applied_theme: None,
            applied_title: None,
            seen_fullscreen: None,
            quiet_frames: 0,
            driver,
            frame_started: None,
            renderer_known: false,
            start_milestone: StartMilestone::Nothing,
            frame_rate: log_frame_rate.then(|| FrameRate::new(0)),
        }
    }

    /// Applies one action at once (tests and the harness; the views queue theirs).
    pub fn dispatch(&mut self, action: Action) {
        state::apply(&mut self.state, &mut self.services, action);
        self.quiet_frames = 0;
    }

    /// Tells the app that the state was changed from outside the action path (the harness and
    /// tests set up states no action leads to): what is on screen is not final yet.
    pub fn touch(&mut self) {
        self.quiet_frames = 0;
    }

    /// Puts an already opened collection on screen, exactly as a finished background open does.
    /// For tests and tools that open synchronously.
    pub fn show_opened(&mut self, opened: instagram_core::catalog::Opened) {
        state::handle_open_event(&mut self.state, &mut self.services, crate::opening::OpenEvent::Opened(Ok(opened)));
    }

    /// True when nothing is being opened, decoded or laid out and no action is waiting: what is
    /// on screen is final.
    pub fn is_idle(&self) -> bool {
        self.busy_reasons().is_empty()
    }

    /// What keeps the app from being idle right now, in words for the log: the screenshot
    /// harness says what it is waiting for when a run takes long.
    pub fn busy_reasons(&self) -> Vec<&'static str> {
        let main_view_settled = match (&self.state.collection, self.state.view) {
            (None, _) => true,
            // Auto view covers the main view, which is not painted meanwhile.
            (Some(_), _) if self.state.auto_view.is_some() => true,
            (Some(_), View::Home) => self.feed.is_settled() && self.state.scroll_request.is_none(),
            (Some(_), View::Explore) => self.explore.is_settled(),
        };
        let checks = [
            (self.startup_paths.is_empty(), "the paths of the command line are not opened yet"),
            (self.actions.is_empty(), "actions are waiting"),
            (self.quiet_frames >= QUIET_FRAMES_WHEN_IDLE, "an action was applied in the last frames"),
            (self.state.pending_focus.is_none(), "the keyboard focus is being handed back"),
            (!self.services.opener.is_running(), "pictures are being opened"),
            (!self.services.downloader.is_busy(), "a download is being chosen or prepared"),
            (self.services.loader.is_idle(), "pictures are being decoded"),
            (main_view_settled, "the main view is still laying out"),
            (self.state.viewer.is_none() || self.viewer.is_settled(), "the viewer has not settled"),
            (self.state.auto_view.is_none() || self.auto_view.is_settled(), "auto view has not settled"),
            (!self.services.web.is_busy(), "the browser pages are being started or moved"),
            (!self.services.video.waiting_for_first_frame(), "a video is loading its first frame"),
        ];
        checks.into_iter().filter(|(fine, _)| !fine).map(|(_, reason)| reason).collect()
    }

    /// The first half of a frame: everything that is not painting.
    pub fn frame_logic(&mut self, ctx: &egui::Context) {
        self.frame_started = Some(Instant::now());
        if let Some(frame_rate) = &mut self.frame_rate {
            frame_rate.count_logic(self.services.video.frames_on_screen());
        }
        if self.services.ctx.is_none() {
            self.services.ctx = Some(ctx.clone());
        }
        if !theme::fonts_installed(ctx) {
            // A context nobody prepared (a test without context setup): the fonts take effect
            // at the next frame, and `frame_ui` waits for them.
            theme::install_fonts(ctx);
            ctx.request_repaint();
        }
        if self.applied_theme != Some(self.state.theme) {
            if theme::styles_installed(ctx) {
                theme::select_theme(ctx, self.state.theme);
            } else {
                theme::install_styles(ctx, self.state.theme);
            }
            if self.applied_theme.is_none() && !self.state.animations {
                // A screenshot shows the final state: no caret blink, no widget animations.
                ctx.all_styles_mut(|style| {
                    style.visuals.text_cursor.blink = false;
                    style.animation_time = 0.0;
                });
            }
            self.applied_theme = Some(self.state.theme);
        }
        self.state.time = ctx.input(|input| input.time);
        // The window is closing: it leaves the screen at once, while the last frame, the saving
        // and the release of the renderer happen out of sight.
        if !self.hidden_for_close && ctx.input(|input| input.viewport().close_requested()) {
            self.hidden_for_close = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
        }
        self.services.loader.begin_frame(ctx);
        // The newest video frames become textures.
        self.services.video.begin_frame(ctx, self.state.time);

        // What the archive cache and the poster cache hold beyond their limits goes, on threads
        // of their own.
        if !self.cache_cleaned {
            self.cache_cleaned = true;
            crate::opening::clean_up_cache_in_background(self.services.context.archive_cache.clone());
            crate::opening::clean_up_posters_in_background(PosterCache::new(self.services.paths.poster_cache_dir.clone()));
        }

        // The browser pages: started at the first frame (`App::new` starts no thread), then what
        // their threads finished and the changes browsers asked for. This half of the frame also
        // runs while the window is hidden or minimised, whenever a browser wakes it.
        self.services.web.begin(self.state.collection.as_ref(), self.state.content_serial, self.services.loader.waker());
        for event in self.services.web.poll() {
            state::handle_web_event(&mut self.state, event);
            self.quiet_frames = 0;
        }
        while let Some(command) = self.services.web.next_command() {
            state::handle_web_command(&mut self.state, &mut self.services, command);
            self.quiet_frames = 0;
        }

        // The command line: open what was passed, once.
        if !self.startup_paths.is_empty() {
            let paths = std::mem::take(&mut self.startup_paths);
            self.dispatch(Action::OpenPaths(paths));
        }

        // What the background threads finished.
        while let Some(event) = self.services.opener.poll() {
            state::handle_open_event(&mut self.state, &mut self.services, event);
            self.quiet_frames = 0;
        }
        if self.state.held_open.is_some() && self.state.editor.is_none() {
            state::release_held_open(&mut self.state, &mut self.services);
            self.quiet_frames = 0;
        }
        while let Some(event) = self.services.downloader.poll() {
            state::handle_download_event(&mut self.state, &mut self.services, event);
            self.quiet_frames = 0;
        }
        // The banner shows how far the running download is.
        if self.services.downloader.is_running() {
            if let Some(download) = &mut self.state.download {
                download.progress = Some(self.services.downloader.progress());
            }
        }

        // Auto view: `--autoview` starts it once pictures are on screen, and its clock moves
        // with every frame. A minimized window stops that clock.
        self.state.window_minimized = ctx.input(|input| input.viewport().minimized).unwrap_or(false);
        // eframe runs only this half of a frame while the window is minimized, so the views
        // claim nothing and `end_frame` does not run: the players pause here.
        self.services.video.window_minimized(self.state.window_minimized, Instant::now());
        if self.state.auto_view_request.is_some() && self.state.opening_since.is_none() && self.startup_paths.is_empty() {
            if self.state.collection.is_some() {
                state::start_requested_auto_view(&mut self.state, &mut self.services);
            } else if !self.services.opener.is_running() {
                // Nothing was opened with it: there is nothing to show.
                self.state.auto_view_request = None;
            }
        }
        // The length and the end of the video on show (`AutoView.tsx:244-263`).
        if let Some(run) = &mut self.state.auto_view {
            if let (true, Some(slide)) = (run.is_video, run.slide_key().cloned()) {
                if let Some(post) = slide.1.clone() {
                    let video = self.services.video.auto_view_state(&VideoKey { post, slide: slide.2, visit: slide.0 });
                    run.video = Some((slide, video));
                }
            }
        }
        state::tick_auto_view(&mut self.state);

        // Files dragged over or dropped on the window. A picture can be dropped on the viewer,
        // but nothing is accepted while a dialog is open or auto view runs.
        let (dropped, hovering): (Vec<PathBuf>, bool) = ctx.input(|input| (input.raw.dropped_files.iter().map(|file| file.path().to_path_buf()).collect(), !input.raw.hovered_files.is_empty()));
        let accepts_drops = !self.state.modal_open() && self.state.auto_view.is_none();
        self.state.drop_hover = accepts_drops && (hovering || self.forced_drop_hover);
        if accepts_drops && !dropped.is_empty() {
            self.dispatch(Action::OpenPaths(dropped));
        }

        // Global shortcuts: Ctrl+Shift+O before Ctrl+O, the longer one would match both.
        if accepts_drops {
            let open_folder = KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::O);
            let open_pictures = KeyboardShortcut::new(Modifiers::COMMAND, Key::O);
            if ctx.input_mut(|input| input.consume_shortcut(&open_folder)) {
                self.dispatch(Action::PickFolder);
            } else if ctx.input_mut(|input| input.consume_shortcut(&open_pictures)) {
                self.dispatch(Action::PickPictures);
            }
        }

        // F11 makes the window fill the screen, wherever the user is. When the window changed
        // by other means (it was closed in fullscreen last time), the state follows it.
        let window_fullscreen = ctx.input(|input| input.viewport().fullscreen);
        if window_fullscreen != self.seen_fullscreen {
            self.seen_fullscreen = window_fullscreen;
            if let Some(fullscreen) = window_fullscreen {
                self.state.fullscreen = fullscreen;
            }
        }
        if ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::F11)) {
            self.dispatch(Action::ToggleFullscreen);
        }

        // The window is named after the picture in the viewer.
        let title = self.state.window_title();
        if self.applied_title.as_deref() != Some(title.as_str()) {
            log::debug!("Window title: {title}");
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.applied_title = Some(title);
        }

        // Toasts disappear by themselves; without animations they stay (screenshots).
        if self.state.animations {
            if let Some(seconds) = self.state.toasts.expire(self.state.time) {
                ctx.request_repaint_after_secs(seconds as f32 + 0.01);
            }
            if self.state.opening_since.is_some() && !self.state.shows_busy_chip() {
                ctx.request_repaint_after_secs(state::BUSY_CHIP_DELAY_SECONDS as f32);
            }
        }

        if let Some(mut driver) = self.driver.take() {
            driver.tick_logic(self, ctx);
            self.driver = Some(driver);
        }
        // What a browser sees follows every change of this frame.
        self.services.web.publish(self.state.collection.as_ref(), self.state.content_serial);
    }

    /// The second half of a frame: painting, then applying what the user did.
    pub fn frame_ui(&mut self, ui: &mut Ui) {
        let ctx = ui.ctx().clone();
        if let Some(driver) = &mut self.driver {
            driver.tick_ui(&ctx);
        }
        if !theme::fonts_installed(&ctx) || self.applied_theme.is_none() {
            // `frame_logic` has not prepared this context yet; text in the named font families
            // cannot be laid out before the fonts are in place.
            ctx.request_repaint();
            return;
        }
        widgets::begin_label_frame(&ctx);

        // A dialog or the viewer closed: the widget that opened it gets the focus back, one
        // painted frame after the layer has gone (see `state::PendingFocus`).
        if let Some(pending) = &mut self.state.pending_focus {
            if pending.painted_frames >= 1 {
                let id = pending.id;
                ctx.memory_mut(|memory| memory.request_focus(id));
                self.state.pending_focus = None;
            } else {
                pending.painted_frames += 1;
            }
            ctx.request_repaint();
        }

        let scroll_request = self.state.scroll_request.take();
        let mut kept_scroll_request = None;
        let mut actions = std::mem::take(&mut self.actions);
        {
            let mut env = Env { state: &self.state, loader: &mut self.services.loader, actions: &mut actions, paths: &self.services.paths, web: &self.services.web, video: &mut self.services.video };
            let palette = env.palette();
            let screen = ctx.content_rect();
            // In the window the page colour is the clear colour already (`clear_color`). Painting
            // it once more costs a software rasteriser a whole window of pixels per frame, and
            // under auto view nothing of it would be seen.
            if self.paints_background && self.state.auto_view.is_none() {
                ui.painter().rect_filled(screen, 0.0, palette.bg);
            }

            let sidebar_width = shell::sidebar_width(screen.width());
            let sidebar_rect = Rect::from_min_max(screen.min, pos2(screen.left() + sidebar_width, screen.bottom()));
            let main_rect = Rect::from_min_max(pos2(sidebar_rect.right(), screen.top()), screen.max);

            // The sidebar first: Tab reaches its items before the content. Auto view covers the
            // whole window with black: what lies under it is not painted at all, so that a show
            // costs no more than its own picture.
            if self.state.auto_view.is_none() {
                shell::sidebar(ui, sidebar_rect, &mut env);
            }
            match (&self.state.collection, self.state.view) {
                (Some(_), _) if self.state.auto_view.is_some() => kept_scroll_request = scroll_request,
                (None, _) if self.state.opening_since.is_some() => shell::opening_state(ui, main_rect, &env),
                (None, _) => shell::empty_state(ui, main_rect, &mut env),
                (Some(_), View::Home) => self.feed.show(ui, main_rect, &mut env, scroll_request),
                (Some(_), View::Explore) => {
                    // A request for the feed waits until the feed is shown again.
                    kept_scroll_request = scroll_request;
                    self.explore.show(ui, main_rect, &mut env);
                }
            }

            // Layers above the views, bottom to top: the viewer, then the one dialog that may
            // be open. Each is modal: what lies below it does not react.
            if self.state.viewer.is_some() {
                self.viewer.show(&ctx, &mut env);
                if let Some(hole) = self.viewer.opaque_area() {
                    // The sidebar and the main view are not drawn where the card covers them.
                    rendering::keep_out(&ctx, ui.layer_id(), 0..usize::MAX, hole);
                }
            } else {
                self.viewer.forget_picture();
            }
            // Under auto view the window is cleared to its black (`clear_color`) when nothing
            // else lies below it, which saves a rasteriser a whole window of black pixels.
            self.cleared_to_black = !self.paints_background && self.state.auto_view.is_some() && self.state.viewer.is_none();
            if self.state.auto_view.is_some() {
                self.auto_view.window_is_black = self.cleared_to_black;
                self.auto_view.show(&ctx, &mut env);
            }
            if let Some(sheet) = &self.state.sheet {
                sheets::post_options_sheet(&ctx, sheet, &mut env);
            }
            if let Some(editor) = &self.state.editor {
                editor::editor_dialog(&ctx, editor, &mut env);
            }
            if let Some(setup) = &self.state.auto_view_setup {
                autoview::setup_dialog(&ctx, setup, &mut env);
            }
            if let Some(opened_at) = self.state.warnings_opened_at {
                sheets::warnings_dialog(&ctx, opened_at, &mut env);
            }
            if let Some(opened_at) = self.state.about_opened_at {
                shell::about_sheet(&ctx, opened_at, &mut env);
            }
            if let Some(sheet) = &self.state.browser_sheet {
                browser::browser_sheet(&ctx, sheet, &mut env);
            }
            // A question lies over whatever asked it.
            if let Some(confirm) = &self.state.confirm {
                sheets::confirm_dialog(&ctx, confirm, &mut env);
            }
            // Escape that no layer took leaves fullscreen.
            if self.state.fullscreen && !self.state.overlay_open() && !self.state.more_menu_open && ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Escape)) {
                env.push(Action::ToggleFullscreen);
            }
            // The chip tells that more pictures are on their way while others are on screen.
            let chip = (self.state.collection.is_some() && self.state.shows_busy_chip()).then_some(strings::OPENING_PICTURES);
            widgets::toasts_and_chip(&ctx, &self.state, chip);
            if self.state.drop_hover {
                shell::drop_overlay(&ctx);
            }
        }

        if self.state.scroll_request.is_none() {
            self.state.scroll_request = kept_scroll_request;
        }

        // Who plays: decided from what the views claimed in this frame.
        self.services.video.end_frame(&self.video_wants());

        // The one place where the user's actions change the state.
        self.quiet_frames = if actions.is_empty() { self.quiet_frames.saturating_add(1) } else { 0 };
        for action in actions {
            state::apply(&mut self.state, &mut self.services, action);
            ctx.request_repaint();
        }

        if let Some(started) = self.frame_started.take() {
            self.frame_stats.record_app(started.elapsed().as_secs_f32() * 1000.0);
        }
        if let Some(frame_rate) = &mut self.frame_rate {
            frame_rate.count_painted();
        }
        self.log_start_milestones();
    }

    /// What decides who plays (video brief 6.6): the feed only while nothing covers it, the
    /// viewer while it is open, auto view while its clock runs; nothing while minimized.
    fn video_wants(&self) -> Wants {
        let state = &self.state;
        let run = state.auto_view.as_ref();
        Wants {
            feed_uncovered: state.view == View::Home && state.collection.is_some() && !state.overlay_open() && !state.more_menu_open && !state.drop_hover,
            viewer_open: state.viewer.is_some(),
            auto_view_open: run.is_some(),
            auto_view_playing: run.is_some_and(|run| run.player.is_active(state.window_minimized)),
            auto_view_looping: run.is_none_or(|run| !run.listen_for_end),
            minimized: state.window_minimized,
            sound_on: state.video_sound_on,
            volume: state.video_volume,
        }
    }

    /// Writes to the log, once each, how long after the start of the process the first frame
    /// was described and the first frame with a decoded picture in it. (The renderer shows a
    /// frame a moment later; with a software rasteriser that moment is a tenth of a second.)
    fn log_start_milestones(&mut self) {
        if self.start_milestone == StartMilestone::FirstPicture {
            return;
        }
        let Some(started) = PROCESS_STARTED.get() else {
            return;
        };
        let milliseconds = started.elapsed().as_millis();
        if self.start_milestone == StartMilestone::Nothing {
            self.start_milestone = StartMilestone::FirstFrame;
            log::info!("Start: the first frame was painted {milliseconds} ms after the process started.");
        }
        if self.services.loader.stats().decoded > 0 {
            self.start_milestone = StartMilestone::FirstPicture;
            log::info!("Start: the first picture was painted {milliseconds} ms after the process started.");
        }
    }

    /// Shows the native dialog a view asked for. Only the window glue can do this: the dialog
    /// needs the window as its parent.
    fn show_requested_dialog(&mut self, frame: &eframe::Frame) {
        if let Some(kind) = self.services.opener.take_dialog_request() {
            let dialog = Opener::build_dialog(kind, self.state.remembered.last_folder.as_deref()).set_parent(frame);
            self.services.opener.show_dialog(kind, dialog);
        }
        if let Some(request) = self.services.downloader.take_dialog_request() {
            let dialog = Downloader::build_dialog(&request).set_parent(frame);
            self.services.downloader.show_dialog(dialog);
        }
    }

    /// Waits until the frames handed to the renderer so far have been drawn.
    ///
    /// A software rasteriser (a machine without a usable graphics card) draws on the processor,
    /// and far slower than this app can describe frames. Nothing holds the app back by itself:
    /// frames would queue up, each with its own copy of the window in memory, and what is on
    /// screen would fall seconds behind the pointer. Waiting here keeps one frame in flight,
    /// so animations simply run in as many steps as the machine can draw.
    fn wait_for_renderer(&mut self, frame: &eframe::Frame) {
        let Some(render_state) = frame.wgpu_render_state() else {
            return;
        };
        let started = Instant::now();
        let wait = wgpu::PollType::Wait { submission_index: None, timeout: Some(std::time::Duration::from_millis(RENDERER_WAIT_MS)) };
        if let Err(error) = render_state.device.poll(wait) {
            // A frame that takes this long is given up waiting for; the next wait catches up.
            log::debug!("The renderer did not finish a frame within {RENDERER_WAIT_MS} ms: {error}");
        }
        self.frame_stats.record_renderer_wait(started.elapsed().as_secs_f32() * 1000.0);
    }

    /// True when this run produces a screenshot with a hidden window.
    fn takes_screenshot(&self) -> bool {
        self.driver.as_ref().is_some_and(Driver::takes_screenshot)
    }

    /// The window is closing: the browser pages stop (at most `web::WEB_STOP_WAIT_MS`; a browser
    /// waiting for a change is told the app is closing), and a running download is cancelled and
    /// given a moment to remove its temporary files, so that no half-written archive is left in
    /// the user's folder.
    pub fn shut_down(&mut self) {
        // The players' threads end on their own within milliseconds; nothing waits for them.
        self.services.video.close_all();
        self.services.web.shut_down();
        if let Some(result) = self.services.downloader.cancel_and_wait(std::time::Duration::from_millis(DOWNLOAD_CANCEL_WAIT_MS)) {
            match result {
                Ok(result) => log::info!("The download finished while the window closed: {}", instagram_core::export::download_message(&result)),
                Err(error) => log::info!("The download ended with the window: {}", error.message),
            }
        } else if self.services.downloader.is_running() {
            log::warn!("The download did not stop within {DOWNLOAD_CANCEL_WAIT_MS} ms; a temporary file may be left in the target folder.");
        }
    }
}

impl eframe::App for App {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.paints_background = false;
        if !self.renderer_known {
            if let Some(render_state) = frame.wgpu_render_state() {
                self.state.software_renderer = render_state.adapter.get_info().device_type == wgpu::DeviceType::Cpu;
                self.renderer_known = true;
                if !self.video_caps_fixed {
                    self.services.video.set_caps(VideoCaps::from_adapter(self.state.software_renderer));
                }
                log::info!("Videos: {:?} adapter rules ({}).", self.services.video.caps(), render_state.adapter.get_info().name);
            }
        }
        // The beat of a playing video counts from here, before the wait for the renderer, so that
        // the wait does not stretch it (`VideoCaps::next_repaint_delay`).
        self.services.video.mark_pass_start(Instant::now());
        // The last frame of a closing window is never seen: it does not wait for the one before.
        let closing = ctx.input(|input| input.viewport().close_requested());
        if self.state.software_renderer && !self.takes_screenshot() && !closing {
            self.wait_for_renderer(frame);
        }
        if self.state.software_renderer {
            // Two frames in the swap chain (rendering.rs); eframe applies this before it paints,
            // and only when it changed.
            frame.set_wgpu_surface_config(rendering::software_surface_config());
        }
        self.frame_stats.record_total(frame.info().cpu_usage);
        self.frame_logic(ctx);
        self.show_requested_dialog(frame);
    }

    fn ui(&mut self, ui: &mut Ui, _frame: &mut eframe::Frame) {
        self.frame_ui(ui);
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(driver) = &mut self.driver {
            driver.inject(raw_input);
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // The colour of what this frame painted, not of the state its actions led to: a show
        // that starts at the end of a frame is black from the next one.
        if self.cleared_to_black {
            egui::Color32::BLACK.to_normalized_gamma_f32()
        } else {
            self.state.palette().bg.to_normalized_gamma_f32()
        }
    }

    /// Only the window geometry goes to window.ron: egui's own memory (scroll positions of a
    /// collection that may be gone) is not worth keeping.
    fn persist_egui_memory(&self) -> bool {
        false
    }

    /// eframe has just put the window geometry into its storage. It is kept only when it is
    /// usable and without fullscreen: a minimised window keeps the last usable geometry, so the
    /// next start is not a tiny window, and a window closed in fullscreen comes back with its
    /// title bar (`placement::geometry_to_keep`).
    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        if !self.window_geometry.keeps {
            return;
        }
        let taken = storage.get_string(placement::STORAGE_WINDOW_KEY);
        let kept = placement::geometry_to_keep(taken.as_deref(), self.window_geometry.last_usable.as_deref(), theme::MIN_WINDOW_SIZE);
        if kept != taken {
            match &kept {
                Some(kept) => storage.set_string(placement::STORAGE_WINDOW_KEY, kept.clone()),
                // Nothing usable: the next window opens like the first.
                None => storage.remove_string(placement::STORAGE_WINDOW_KEY),
            }
        }
        self.window_geometry.last_usable = kept;
    }

    fn on_exit(&mut self) {
        self.shut_down();
        if let Some(frame_rate) = &mut self.frame_rate {
            frame_rate.report(self.services.video.frames_on_screen());
        }
        if self.frame_stats.frames() > 0 {
            self.frame_stats.log("session");
        }
        log::logger().flush();
        // eframe has saved the window geometry by now. If tearing down the GPU device hangs,
        // leave anyway.
        let spawned = std::thread::Builder::new().name("exit-guard".to_owned()).spawn(|| {
            std::thread::sleep(std::time::Duration::from_millis(EXIT_GUARD_MS));
            std::process::exit(0);
        });
        if let Err(error) = spawned {
            log::warn!("The exit guard could not be started: {error}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_frame_rate_line_counts_the_new_video_frames_of_its_stretch() {
        let mut rate = FrameRate::new(40);
        for _ in 0..50 {
            rate.count_painted();
        }
        rate.logic_passes = 52;
        assert_eq!(rate.line(5.0, 90), "Frame rate: 52 logic passes and 50 painted frames in the last 5.0 s (10.0 painted frames per second; 50 new video frames, 10.0 per second).");
        // Without a playing video the line stays as it was.
        assert_eq!(rate.line(5.0, 40), "Frame rate: 52 logic passes and 50 painted frames in the last 5.0 s (10.0 painted frames per second).");
        // The next stretch counts from where this one ended.
        rate.report(90);
        assert_eq!(rate.video_frames_before, 90);
        assert_eq!((rate.logic_passes, rate.painted_frames), (0, 0));
    }
}
