//! What the interface knows, and the one place that changes it.
//!
//! The views never change the state themselves. They read `AppState` and push an `Action` for
//! everything the user does; after the frame the app hands each action to `apply`, the only
//! function that mutates the state or starts work (opening, copying, saving state.json). A new
//! feature therefore adds an `Action`, a few lines in `apply`, and its own view code, without
//! touching the views of others.
//!
//! Like, save and expanded captions are kept in memory only, as in the reference web project:
//! they vanish when another collection is opened.
//!
//! Layers above the main view, innermost first, as Escape closes them (`App.tsx:1028-1040`): a
//! dialog (a question, the post options sheet, the description editor, the auto view setup, the
//! warnings, About; a question may lie over another dialog, otherwise never two at once), then
//! auto view, then the viewer, then the More menu. `AppState::modal_open` and `overlay_open`
//! tell the views which of them has the keyboard.
//!
//! Things that run beside the frames are taken over here: a download (`handle_download_event`,
//! spec-edit 8-9), the clock of auto view (`tick_auto_view`, spec-viewer 4.5), and the changes a
//! browser asks for (`handle_web_command`, web brief 4.9): the HTTP threads never touch the
//! state, they hand each change to this thread and wait for the answer.

use std::collections::HashMap;
use std::path::PathBuf;

use instagram_core::archive::cache::ArchiveLease;
use instagram_core::autoview::{self, ActiveTimer, AutoViewRequest, PlayerAction, PlayerState, SeededRandom, VideoState};
use instagram_core::catalog::{self, OpenContext, OpenError, OpenOptions};
use instagram_core::export::{self, ExportErrorCode, ExportProgress};
use instagram_core::messages;
use instagram_core::model::{fold_path, Collection, ExportKind, Post, PostKey, PostOrigin};
use instagram_core::settings::AppPaths;
use instagram_core::store::{self, DescriptionStore, SaveOutcome, Theme, UiState};
use instagram_core::text;
use instagram_core::web::{ApiError, WebCommand, WebCommandKind};

use crate::download::{self, DownloadEvent, Downloader, SaveRequest};
use crate::loader::Loader;
use crate::opening::{DialogKind, OpenEvent, Opener};
use crate::platform;
use crate::strings;
use crate::theme;
use crate::video::{Surface, VideoHub};
use crate::web::{SaveRequest as WebSaveRequest, WebEvent, WebService};

/// The carousel track slides for this long (`transition: transform .28s ease`).
pub const SLIDE_SECONDS: f64 = 0.28;
/// The like heart is on screen for this long (`heartpop .65s`).
pub const BURST_SECONDS: f64 = 0.65;
/// A sheet scales in for this long (`sheetin .12s ease-out`).
pub const SHEET_IN_SECONDS: f64 = 0.12;
/// The "Opening pictures…" chip appears only when opening takes longer than this, so that a
/// quick open does not flash it.
pub const BUSY_CHIP_DELAY_SECONDS: f64 = 0.2;
/// A port field of the Browser view sheet takes at most this many characters: more can never be
/// a port.
pub const PORT_FIELD_MAX_CHARS: usize = 5;

/// The main views. The viewer, the sheets and auto view are layers above them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum View {
    /// The feed of post cards.
    #[default]
    Home,
    /// The grid of square tiles.
    Explore,
}

/// A carousel on its way from one slide to another.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SlideMotion {
    /// The track position (in slides) the motion started from.
    pub from: f32,
    /// When it started, in seconds of the app clock.
    pub started_at: f64,
}

/// The in-memory state of one post: what the user did with it in this session.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PostUi {
    /// The like heart is on.
    pub liked: bool,
    /// The bookmark is on.
    pub saved: bool,
    /// "more" was clicked: the whole caption is shown. There is no way back, as in the web project.
    pub expanded: bool,
    /// The slide of a carousel that is shown.
    pub slide: usize,
    /// The running slide animation, if any.
    pub slide_motion: Option<SlideMotion>,
    /// When the like heart last started to pop, in seconds of the app clock.
    pub burst_at: Option<f64>,
}

impl PostUi {
    /// Where the carousel track is at `time`: a slide index, fractional while it moves.
    pub fn track_position(&self, time: f64, animations: bool) -> f32 {
        let target = self.slide as f32;
        match self.slide_motion {
            // A motion that never starts holds the track where it is (screenshots of a slide
            // change in progress).
            Some(motion) if motion.started_at == f64::INFINITY => motion.from,
            Some(motion) if animations => {
                let progress = ((time - motion.started_at) / SLIDE_SECONDS).clamp(0.0, 1.0) as f32;
                motion.from + (target - motion.from) * crate::layout::ease(progress)
            }
            _ => target,
        }
    }

    /// True while the carousel is moving at `time`.
    pub fn is_sliding(&self, time: f64, animations: bool) -> bool {
        animations && self.slide_motion.is_some_and(|motion| motion.started_at.is_finite() && time - motion.started_at < SLIDE_SECONDS)
    }
}

/// One toast.
#[derive(Debug, Clone, PartialEq)]
pub struct Toast {
    /// The sentence.
    pub message: String,
    /// When it disappears, in seconds of the app clock.
    pub expires_at: f64,
}

/// The toasts at the bottom of the window (`App.tsx:356-361`): at most three, the newest last,
/// and a repeated message replaces its older copy.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ToastStack {
    toasts: Vec<Toast>,
}

impl ToastStack {
    /// Shows `message` for `duration_ms`, starting at `time`.
    pub fn show(&mut self, message: impl Into<String>, duration_ms: u64, time: f64) {
        let message = message.into();
        self.toasts.retain(|toast| toast.message != message);
        let keep = messages::MAX_TOASTS.saturating_sub(1);
        if self.toasts.len() > keep {
            let excess = self.toasts.len() - keep;
            self.toasts.drain(..excess);
        }
        self.toasts.push(Toast { message, expires_at: time + duration_ms as f64 / 1000.0 });
    }

    /// Removes the toasts whose time is over. Returns the seconds until the next one expires.
    pub fn expire(&mut self, time: f64) -> Option<f64> {
        self.toasts.retain(|toast| toast.expires_at > time);
        self.toasts.iter().map(|toast| toast.expires_at - time).fold(None, |soonest: Option<f64>, left| Some(soonest.map_or(left, |value| value.min(left))))
    }

    /// The toasts, oldest first.
    pub fn visible(&self) -> &[Toast] {
        &self.toasts
    }

    /// True when no toast is shown.
    pub fn is_empty(&self) -> bool {
        self.toasts.is_empty()
    }
}

/// The viewer (the post detail) is open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewerState {
    /// The post on show.
    pub key: PostKey,
    /// The post the viewer was opened on. When the user has moved on to another one, the feed
    /// follows to it as the viewer closes.
    pub opened_on: PostKey,
}

/// The "Post options" sheet is open.
#[derive(Debug, Clone, PartialEq)]
pub struct PostSheet {
    /// The post it belongs to.
    pub key: PostKey,
    /// It was opened from the viewer, which has no "Go to post" (`canGoToPost`).
    pub from_viewer: bool,
    /// When it opened, in seconds of the app clock (for the opening animation).
    pub opened_at: f64,
}

/// The "Edit description" dialog is open.
#[derive(Debug, Clone, PartialEq)]
pub struct EditorState {
    /// The post whose description is edited.
    pub key: PostKey,
    /// The text of the field: line breaks are `\n`, at most 10,000 UTF-16 code units.
    pub text: String,
    /// Why the last attempt to save failed, shown inside the dialog.
    pub error: Option<String>,
    /// When it opened, in seconds of the app clock.
    pub opened_at: f64,
    /// Counts the dialogs opened in this session; the field of every editor is a new widget
    /// with its own caret and undo history.
    pub serial: u64,
}

/// A widget that gets the keyboard focus back after a dialog or the viewer closed.
///
/// egui keeps the focus away from everything below a modal layer, and it still counts the
/// layer that closed during the next frame. So the focus is handed over one painted frame
/// later (`App::frame_ui`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingFocus {
    /// The widget.
    pub id: egui::Id,
    /// Frames painted since the layer closed.
    pub painted_frames: u8,
}

/// What a download exports.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadKind {
    /// "Download all": every post on screen.
    All,
    /// "Download post": this one.
    Post(PostKey),
}

/// How far a download has come.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DownloadPhase {
    /// The Save dialog is open.
    Choosing,
    /// The user is asked whether the part files of an earlier download may be replaced.
    Confirming {
        /// The folder the archive goes to.
        target_dir: PathBuf,
        /// The name of the archive without ".zip".
        name: String,
        /// The files of the earlier download.
        stale: Vec<PathBuf>,
    },
    /// The archive is being written. `stale` are files of an earlier download the user agreed
    /// to replace; what is left of them is removed when this download has succeeded.
    Running { stale: Vec<PathBuf> },
}

/// The one download that may exist at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DownloadState {
    /// What is exported.
    pub kind: DownloadKind,
    /// Where it stands.
    pub phase: DownloadPhase,
    /// Posts handled so far, once the export has reported.
    pub progress: Option<ExportProgress>,
}

impl DownloadState {
    /// The figures of the banner button "Preparing {done} of {total} · Cancel": only "Download
    /// all" shows them, and only while the archive is being written.
    pub fn banner_progress(&self) -> Option<ExportProgress> {
        match (&self.kind, &self.phase) {
            (DownloadKind::All, DownloadPhase::Running { .. }) => Some(self.progress.unwrap_or(ExportProgress { done: 0, total: 0 })),
            _ => None,
        }
    }
}

/// What a question asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfirmKind {
    /// "Close pictures" while a download is being prepared: closing cancels it.
    ClosePictures,
    /// A download that may be split into parts would replace the part files of an earlier one.
    ReplaceParts {
        /// How many such files there are.
        count: usize,
        /// The first of them, by its file name.
        first: String,
    },
    /// Not a question but a notice with one "OK": descriptions.json was damaged and set aside,
    /// or could not be read. It explains why every description is missing and where the old
    /// file is, so it stays until the user has closed it (a toast would be gone before the
    /// first picture is on screen).
    StoreNotice(String),
}

/// A question in the shape of the reference web project's confirm dialog is open.
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmState {
    /// What it asks.
    pub kind: ConfirmKind,
    /// When it opened, in seconds of the app clock.
    pub opened_at: f64,
}

/// The Browser view sheet while it is open: what the fields and switches hold.
#[derive(Debug, Clone, PartialEq)]
pub struct BrowserSheet {
    /// The server switch.
    pub enabled: bool,
    /// "Allow other devices on this network".
    pub lan: bool,
    /// The text of the page address port field.
    pub frontend_text: String,
    /// The text of the API address port field.
    pub backend_text: String,
    /// What the page port field showed when the sheet opened.
    pub frontend_shown: String,
    /// What the API port field showed when the sheet opened.
    pub backend_shown: String,
    /// Why the last Save was refused, shown in the sheet.
    pub error: Option<String>,
    /// A Save is moving the server.
    pub saving: bool,
    /// When it opened (app clock).
    pub opened_at: f64,
    /// Tells two openings apart (the fields' focus).
    pub serial: u64,
}

/// Which port field of the Browser view sheet.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortField {
    /// The page address port.
    Frontend,
    /// The API address port.
    Backend,
}

/// The setup dialog of auto view is open (`AutoView.tsx:51-177`): a fresh form each time,
/// seeded from the remembered settings.
#[derive(Debug, Clone, PartialEq)]
pub struct AutoViewSetup {
    /// "Auto view from here": the post the show starts at; None starts at the first.
    pub start: Option<PostKey>,
    /// The text of the seconds field, as typed.
    pub text: String,
    /// "Let videos play to the end".
    pub wait_for_videos: bool,
    /// "Start again after the last post".
    pub loop_posts: bool,
    /// "Shuffle the order".
    pub shuffle: bool,
    /// "Start" found the seconds unusable: the field is marked and the error line shown, until
    /// the user types or picks a quick choice.
    pub invalid: bool,
    /// When it opened, in seconds of the app clock.
    pub opened_at: f64,
    /// Counts the setup dialogs of this session; the field of each is a new widget.
    pub serial: u64,
}

/// What the user can do in a running auto view.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlayerCommand {
    /// The next post (Right).
    Next,
    /// The previous post (Left).
    Previous,
    /// Pause or resume (Space).
    TogglePause,
    /// "Restart" of the end screen.
    Restart,
    /// A slide of the carousel on show, picked by hand.
    Slide(usize),
    /// More time per post (`+`).
    Longer,
    /// Less time per post (`-`).
    Shorter,
}

/// A running auto view (`autoView.ts`, spec-viewer 4.5): the order of the posts, where the show
/// is, and the clock of the slide on show.
#[derive(Debug, Clone)]
pub struct AutoViewRun {
    /// The settings of this run. They come from the setup dialog or the command line.
    pub settings: autoview::Settings,
    /// The order and the position.
    pub player: PlayerState<PostKey>,
    /// Counts the time the slide on show has really been shown.
    pub timer: ActiveTimer,
    /// `(visit, post, slide)` the timer counts for.
    slide_key: Option<AutoViewSlide>,
    /// The time the timer was last given for that slide.
    due_ms: Option<f64>,
    /// The time the slide on show is planned to stay, for the progress bar.
    pub expected_ms: f64,
    /// The slide on show is a video.
    pub is_video: bool,
    /// What its player reported: the length once known, whether it ended, for the slide it
    /// belongs to (`AutoView.tsx:244-263`; the app sets it every frame).
    pub video: Option<(AutoViewSlide, VideoState)>,
    /// Auto view waits for the end of the video on show (it then plays once, not in a loop).
    pub listen_for_end: bool,
    /// When the post on show came on screen, in seconds of the auto view clock (the fade).
    pub post_shown_at: f64,
    /// The clock at the last tick, in milliseconds.
    last_tick_ms: Option<f64>,
    /// The `collection_serial` the order was last reconciled with.
    synced_serial: u64,
    /// The widget that gets the keyboard focus back when the show ends.
    pub return_focus: Option<egui::Id>,
}

/// A slide of a running show: (visit, post, slide).
pub type AutoViewSlide = (u64, Option<PostKey>, usize);

impl AutoViewRun {
    /// The slide on show as the timer counts it.
    pub fn slide_key(&self) -> Option<&AutoViewSlide> {
        self.slide_key.as_ref()
    }

    /// How much of the post on show is over, 0..=1: the progress bar. A carousel shares one bar.
    pub fn progress(&self, slide_count: usize, now_ms: f64) -> f64 {
        let slide = self.player.slide.min(slide_count.max(1) - 1);
        autoview::post_progress(slide, slide_count.max(1), self.timer.elapsed_ms(now_ms), self.expected_ms)
    }
}

/// Where the feed should scroll to at the next frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ScrollRequest {
    /// The top of the feed (the banner).
    Top,
    /// The card of this post at the top of the view.
    Post(PostKey),
}

/// Everything the user can do. The views push these; `apply` carries them out.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    // --- Navigation ---
    /// Sidebar "Home" and the logo: the feed, scrolled to the top.
    ShowHome,
    /// Sidebar "Explore": the grid.
    ShowExplore,

    // --- Opening ---
    /// "Open pictures": the native multi-select file dialog.
    PickPictures,
    /// "Open folder": the native folder dialog.
    PickFolder,
    /// Open these files or folders (command line, dialog, drop).
    OpenPaths(Vec<PathBuf>),
    /// Banner "Close" and menu "Close pictures": back to the empty state.
    ClosePictures,

    // --- Appearance and menus ---
    /// Sidebar "More": open or close the menu.
    ToggleMoreMenu,
    /// Close the More menu (Escape, a click elsewhere).
    CloseMoreMenu,
    /// Menu "Switch appearance": light <-> dark, remembered in state.json.
    SwitchAppearance,
    /// Menu "About": open the About sheet.
    ShowAbout,
    /// Close the About sheet.
    CloseAbout,
    /// Banner "N warnings": open the dialog with the list of warnings.
    ShowWarnings,
    /// Close that dialog.
    CloseWarnings,
    /// "Show all N warnings" / "Show fewer" in that list.
    ToggleAllWarnings,
    /// F11: the window fills the screen, or stops doing so.
    ToggleFullscreen,

    // --- A post ---
    /// The heart button of a feed card: like or unlike, and pop the heart either way.
    ToggleLike(PostKey),
    /// The heart button of the viewer: like or unlike; the viewer has no heart animation.
    ToggleLikeQuietly(PostKey),
    /// A double click on the picture: like (never unlike) and pop the heart.
    LikeByDoubleClick(PostKey),
    /// The bookmark button.
    ToggleSave(PostKey),
    /// "more" of a collapsed caption.
    ExpandCaption(PostKey),
    /// The arrows and dots of a carousel: show this slide.
    ShowSlide(PostKey, usize),
    /// The share button: copy the post link, or the file path of a loose picture.
    Share(PostKey),
    /// A hashtag, a mention, the time label or the link icon: open in the browser.
    OpenLink(String),
    /// The comment button, "View all N comments", a grid tile, "Go to post", the picture picked
    /// in a dialog, and the previous / next arrows of the viewer itself: show this post in the
    /// viewer.
    OpenViewer(PostKey),
    /// The X of the viewer and a click on its scrim.
    CloseViewer,
    /// Escape in the viewer: leave fullscreen first, then close the viewer.
    ViewerEscape,

    // --- Post options (the action sheet) ---
    /// The "More options" button of a post: open its action sheet.
    OpenPostOptions(PostKey),
    /// "Cancel", Escape or a click beside the sheet.
    ClosePostOptions,
    /// "Copy link": the link of the post goes to the clipboard.
    CopyLink(PostKey),
    /// "Show in folder": Explorer opens with the file of the slide on show selected.
    ShowInFolder(PostKey),

    // --- Edit description ---
    /// "Edit description": open the editor with the description of this post.
    OpenEditor(PostKey),
    /// The text of the editor's field changed to this.
    EditorInput(String),
    /// "Save" and Ctrl+Enter.
    SaveDescription,
    /// "Cancel", Escape or a click beside the dialog: the typed text is dropped.
    CloseEditor,

    // --- Auto view ---
    /// Sidebar and banner "Auto view" (None), or "Auto view from here" of a post: open the
    /// setup dialog.
    StartAutoView(Option<PostKey>),
    /// The text of the seconds field of the setup dialog changed to this.
    AutoViewInput(String),
    /// A quick choice of the setup dialog: write these seconds into the field.
    AutoViewPreset(u32),
    /// The checkbox "Start again after the last post".
    AutoViewLoop(bool),
    /// The checkbox "Shuffle the order".
    AutoViewShuffle(bool),
    /// "Start" of the setup dialog: check the seconds, remember the settings, start the show.
    AutoViewBegin,
    /// "Cancel", Escape or a click beside the setup dialog.
    CloseAutoViewSetup,
    /// A button or a key of the running show.
    AutoViewCommand(PlayerCommand),
    /// Escape, the X and "Exit" of the running show.
    ExitAutoView,
    /// "Open post" of the running show: leave it and show this post in the viewer.
    AutoViewOpenPost(PostKey),

    // --- Download ---
    /// Banner and menu "Download all".
    DownloadAll,
    /// "Download post" of the action sheet.
    DownloadPost(PostKey),
    /// "Preparing {done} of {total} · Cancel" of the banner: stop the running download.
    CancelDownload,

    // --- Questions and About ---
    /// The red row of a question: do what it asked about.
    ConfirmAccept,
    /// "Cancel", Escape or a click beside a question.
    ConfirmCancel,
    /// "Open data folder" of the About sheet: Explorer opens the folder of the descriptions.
    OpenDataFolder,

    // --- Feedback ---
    /// Show a toast for this many milliseconds.
    Toast(String, u64),

    // Browser pages
    /// More -> "Open in browser": the page address in the default browser.
    OpenInBrowser,
    /// More -> "Open browser imports".
    OpenBrowserImports,
    /// Open the Browser view sheet (More menu, About).
    ShowBrowserView,
    /// Cancel / Escape of the Browser view sheet.
    CloseBrowserView,
    /// The server switch of the sheet.
    BrowserViewServer(bool),
    /// The LAN switch of the sheet.
    BrowserViewLan(bool),
    /// The text of a port field.
    BrowserViewPort(PortField, String),
    /// Save of the sheet.
    BrowserViewSave,

    // --- Videos ---
    /// The sound button of a feed video, a click on a playing feed video, M, the mute button of
    /// the controls bar: the run-wide sound flag.
    ToggleVideoSound,
    /// The volume slider of the controls bar (0..=1).
    VideoVolume(f64),
    /// Space, K, the play button, a click on the video: play or pause the video of a surface.
    VideoTogglePlay(Surface),
    /// The seek bar and Shift+Left / Shift+Right: seconds, and whether to the frame.
    VideoSeek(Surface, f64, bool),
    /// The play badge of a feed video on a software renderer: the viewer on that slide.
    PlayInViewer(PostKey, usize),
    /// "Let videos play to the end" of the auto view setup dialog.
    AutoViewWaitForVideos(bool),
}

/// What the interface knows. Plain data; `apply` is the only writer.
pub struct AppState {
    /// The appearance.
    pub theme: Theme,
    /// The main view.
    pub view: View,
    /// What is open, or None for the empty state.
    pub collection: Option<Collection>,
    /// Counts up whenever the list of posts is replaced or extended; views drop their layout
    /// caches when it changes.
    pub collection_serial: u64,
    /// Counts up whenever what a browser would see changes: another collection, a closed one, a
    /// saved description (here or from a browser), a refreshed browser imports folder. The app
    /// publishes a new snapshot for the browser pages when it changed (web brief 4.9).
    pub content_serial: u64,
    /// Likes, saves, expanded captions and carousel positions, by post.
    pub post_ui: HashMap<PostKey, PostUi>,
    /// The More menu is open.
    pub more_menu_open: bool,
    /// The web app's library folder, when it held a post.json as the More menu opened: the menu
    /// then offers "Open the web app's library".
    pub web_app_library: Option<PathBuf>,
    /// The browser imports folder, when it held a post.json and was not open as the More menu
    /// opened: the menu then offers "Open browser imports".
    pub browser_imports: Option<PathBuf>,
    /// The Browser view sheet, when it is open.
    pub browser_sheet: Option<BrowserSheet>,
    /// How many Browser view sheets have been opened.
    pub browser_sheet_serial: u64,
    /// The About sheet is open since this time of the app clock.
    pub about_opened_at: Option<f64>,
    /// The warnings dialog is open since this time of the app clock.
    pub warnings_opened_at: Option<f64>,
    /// The dialog shows every warning instead of the first five.
    pub warnings_all: bool,
    /// The viewer, when it is open.
    pub viewer: Option<ViewerState>,
    /// The window fills the screen (F11).
    pub fullscreen: bool,
    /// The "Post options" sheet, when it is open.
    pub sheet: Option<PostSheet>,
    /// The "Edit description" dialog, when it is open.
    pub editor: Option<EditorState>,
    /// How many description editors have been opened.
    pub editor_serial: u64,
    /// The widget that had the keyboard focus when a dialog opened; it gets the focus back when
    /// the dialog (or the editor that followed a sheet) closes.
    pub dialog_return_focus: Option<egui::Id>,
    /// The same for the viewer.
    pub viewer_return_focus: Option<egui::Id>,
    /// A widget that is to get the keyboard focus back, once the layer that closed has left
    /// the screen.
    pub pending_focus: Option<PendingFocus>,
    /// The toasts.
    pub toasts: ToastStack,
    /// Where the feed scrolls at the next frame.
    pub scroll_request: Option<ScrollRequest>,
    /// Files are being dragged over the window.
    pub drop_hover: bool,
    /// A folder is being read since this time of the app clock.
    pub opening_since: Option<f64>,
    /// `--autoview` of the command line, until auto view has started with it.
    pub auto_view_request: Option<AutoViewRequest>,
    /// The setup dialog of auto view, when it is open.
    pub auto_view_setup: Option<AutoViewSetup>,
    /// How many setup dialogs have been opened.
    pub auto_view_serial: u64,
    /// The running auto view.
    pub auto_view: Option<AutoViewRun>,
    /// A clock for auto view in seconds that stands in for the app clock: tests and the
    /// screenshot harness move it by hand, so that a show can be looked at second by second.
    pub auto_view_clock: Option<f64>,
    /// The window is minimized: auto view stops its clock without showing "Paused".
    pub window_minimized: bool,
    /// The window is drawn by a software rasteriser (a machine without a usable graphics
    /// card): every frame costs processor time, so what moves by itself moves in fewer steps.
    pub software_renderer: bool,
    /// The download that is being chosen or prepared.
    pub download: Option<DownloadState>,
    /// The question that is open.
    pub confirm: Option<ConfirmState>,
    /// What state.json holds: the theme, the auto view settings, the folder opened last.
    pub remembered: UiState,
    /// A finished open that would replace the collection while the description editor is open:
    /// it waits until the editor has closed, so that typed text is never thrown away by work
    /// that happened in the background (`release_held_open`).
    pub held_open: Option<Box<instagram_core::catalog::Opened>>,
    /// The app clock in seconds (egui's input time), set at the start of every frame.
    pub time: f64,
    /// False in screenshot runs and tests that want the final state at once: every animation
    /// jumps to its end and toasts never expire.
    pub animations: bool,
    /// Draw every like heart at this fraction of its animation (screenshots of the pop).
    pub burst_freeze: Option<f32>,
    /// The sound of videos is on: one flag for the feed, the viewer and auto view, in memory for
    /// the run (the clone's `feedSound`; video brief 5.11). Off at the start.
    pub video_sound_on: bool,
    /// The volume of videos, 0..=1, in memory for the run.
    pub video_volume: f64,
}

impl AppState {
    /// The state of a fresh start with what state.json remembered.
    pub fn new(remembered: UiState) -> AppState {
        AppState {
            theme: remembered.theme,
            view: View::Home,
            collection: None,
            collection_serial: 0,
            content_serial: 0,
            post_ui: HashMap::new(),
            more_menu_open: false,
            web_app_library: None,
            browser_imports: None,
            browser_sheet: None,
            browser_sheet_serial: 0,
            about_opened_at: None,
            warnings_opened_at: None,
            warnings_all: false,
            viewer: None,
            fullscreen: false,
            sheet: None,
            editor: None,
            editor_serial: 0,
            dialog_return_focus: None,
            viewer_return_focus: None,
            pending_focus: None,
            toasts: ToastStack::default(),
            scroll_request: None,
            drop_hover: false,
            opening_since: None,
            auto_view_request: None,
            auto_view_setup: None,
            auto_view_serial: 0,
            auto_view: None,
            auto_view_clock: None,
            window_minimized: false,
            software_renderer: false,
            download: None,
            confirm: None,
            remembered,
            held_open: None,
            time: 0.0,
            animations: true,
            burst_freeze: None,
            video_sound_on: false,
            video_volume: 1.0,
        }
    }

    /// The colours of the current appearance.
    pub fn palette(&self) -> &'static theme::Palette {
        theme::palette(self.theme)
    }

    /// The posts on screen (empty when nothing is open).
    pub fn posts(&self) -> &[Post] {
        self.collection.as_ref().map(|collection| collection.posts.as_slice()).unwrap_or(&[])
    }

    /// The in-memory state of a post; the default for a post the user has not touched.
    pub fn post_ui(&self, key: &PostKey) -> PostUi {
        self.post_ui.get(key).cloned().unwrap_or_default()
    }

    /// True while a dialog is open: a question, the post options sheet, the description
    /// editor, the auto view setup, the warnings or About. It has the keyboard; nothing can be
    /// dropped on the window and the layers behind it do not react to keys.
    pub fn modal_open(&self) -> bool {
        self.about_opened_at.is_some() || self.warnings_opened_at.is_some() || self.sheet.is_some() || self.editor.is_some() || self.auto_view_setup.is_some() || self.confirm.is_some() || self.browser_sheet.is_some()
    }

    /// True while a layer covers the main view: a dialog, auto view or the viewer. The feed
    /// and the grid then leave the keyboard alone.
    pub fn overlay_open(&self) -> bool {
        self.modal_open() || self.viewer.is_some() || self.auto_view.is_some()
    }

    /// The clock of auto view in milliseconds: the app clock, or the one a test moves by hand.
    pub fn auto_view_now_ms(&self) -> f64 {
        self.auto_view_clock.unwrap_or(self.time) * 1000.0
    }

    /// The post auto view shows, with the slide of it that is on show; None on the end screen
    /// and when there is nothing to show.
    pub fn auto_view_post(&self) -> Option<(&Post, usize)> {
        let run = self.auto_view.as_ref().filter(|run| !run.player.finished)?;
        let post = self.post(run.player.current()?)?;
        Some((post, run.player.slide.min(post.media.len().saturating_sub(1))))
    }

    /// True while a download is at a point where it can be cancelled or would be lost: its
    /// archive is being written.
    pub fn download_running(&self) -> bool {
        self.download.as_ref().is_some_and(|download| matches!(download.phase, DownloadPhase::Running { .. }))
    }

    /// The post a key belongs to, if it is still open.
    pub fn post(&self, key: &PostKey) -> Option<&Post> {
        self.collection.as_ref().and_then(|collection| collection.post(key))
    }

    /// The slide of `post` that is on show: its remembered slide, kept inside the carousel.
    pub fn shown_slide(&self, post: &Post) -> usize {
        self.post_ui.get(&post.key).map_or(0, |post_ui| post_ui.slide).min(post.media.len().saturating_sub(1))
    }

    /// The title of the window: "<file name> - Instagram Windows App" while the viewer shows a
    /// picture, the name of the app otherwise.
    pub fn window_title(&self) -> String {
        let file_name = match (&self.auto_view, &self.viewer) {
            // Auto view lies over the viewer: its picture is the one on show.
            (Some(_), _) => self.auto_view_post().and_then(|(post, slide)| post.media_file_name(slide)),
            (None, Some(viewer)) => self.post(&viewer.key).and_then(|post| post.media_file_name(self.shown_slide(post))),
            (None, None) => None,
        };
        messages::window_title(file_name.as_deref())
    }

    /// True while the "Opening pictures…" chip should be shown.
    pub fn shows_busy_chip(&self) -> bool {
        self.opening_since.is_some_and(|since| !self.animations || self.time - since >= BUSY_CHIP_DELAY_SECONDS)
    }

    /// Shows a toast for `duration_ms`.
    pub fn toast(&mut self, message: impl Into<String>, duration_ms: u64) {
        self.toasts.show(message, duration_ms, self.time);
    }

    /// Puts `collection` on screen. `replaced` is false when pictures were added to the
    /// collection that was already shown: likes and carousel positions then survive.
    fn show_collection(&mut self, collection: Collection, replaced: bool) {
        if replaced {
            self.post_ui.clear();
            self.close_layers();
            self.view = View::Home;
        }
        // Added pictures leave the view alone: in Explore the grid simply grows, and the feed
        // scrolls to them once it is shown again.
        self.collection = Some(collection);
        self.collection_serial += 1;
        self.content_serial += 1;
        self.more_menu_open = false;
    }

    /// Closes the viewer and every dialog that belongs to the posts on screen: they are about
    /// to be replaced or closed. A notice about the app's own files stays: it does not belong
    /// to the posts.
    fn close_layers(&mut self) {
        self.viewer = None;
        self.sheet = None;
        self.editor = None;
        self.auto_view_setup = None;
        self.auto_view = None;
        if !self.confirm.as_ref().is_some_and(|confirm| matches!(confirm.kind, ConfirmKind::StoreNotice(_))) {
            self.confirm = None;
        }
        // A download that was still waiting for an answer to its question is dropped with it.
        if self.download.as_ref().is_some_and(|download| matches!(download.phase, DownloadPhase::Confirming { .. })) {
            self.download = None;
        }
        self.warnings_opened_at = None;
        self.warnings_all = false;
        self.dialog_return_focus = None;
        self.viewer_return_focus = None;
        self.pending_focus = None;
    }
}

/// What a view gets to draw one frame: the state to read, the loader to ask for pictures, and
/// the list its actions go to.
pub struct Env<'a> {
    /// The state; views never change it.
    pub state: &'a AppState,
    /// Pictures for the screen.
    pub loader: &'a mut Loader,
    /// Where the user's actions of this frame are collected.
    pub actions: &'a mut Vec<Action>,
    /// Where the app's files are (shown by the About sheet).
    pub paths: &'a AppPaths,
    /// The browser pages (the About sheet and the Browser view sheet show their state).
    pub web: &'a WebService,
    /// The video players: the views claim what they show and read the frames.
    pub video: &'a mut VideoHub,
}

impl Env<'_> {
    /// The colours of the current appearance.
    pub fn palette(&self) -> &'static theme::Palette {
        self.state.palette()
    }

    /// Records an action of the user.
    pub fn push(&mut self, action: Action) {
        self.actions.push(action);
    }
}

/// What `apply` needs besides the state: the files, the background workers and the window.
pub struct Services {
    /// The egui context, attached at the first frame (clipboard, title bar). None before that
    /// and in tests that never draw.
    pub ctx: Option<egui::Context>,
    /// Where settings.json, state.json, descriptions.json, window.ron and app.log are.
    pub paths: AppPaths,
    /// The author of loose pictures.
    pub default_username: String,
    /// The descriptions typed in this app (descriptions.json).
    pub store: DescriptionStore,
    /// Decodes pictures in the background.
    pub loader: Loader,
    /// Runs dialogs and reads folders in the background.
    pub opener: Opener,
    /// Runs the Save dialog and the export in the background.
    pub downloader: Downloader,
    /// Where archives are unpacked and which folder holds the browser imports.
    pub context: OpenContext,
    /// The leases on the archive cache generations whose posts are on screen: one per archive
    /// source. Replacing or closing the collection drops them, so that the cache may clean up.
    pub leases: Vec<ArchiveLease>,
    /// The browser pages: the server, its settings and the published snapshot.
    pub web: WebService,
    /// The video players.
    pub video: VideoHub,
}

impl Services {
    /// The options every open uses.
    fn open_options(&self) -> OpenOptions {
        OpenOptions { default_username: self.default_username.clone() }
    }

    /// Keeps the leases of the archives `collection` shows: of `fresh` and of those held so far,
    /// the newest one of every archive that is still a source of the collection.
    fn keep_leases(&mut self, collection: &Collection, fresh: Vec<ArchiveLease>) {
        let shown: std::collections::HashSet<String> = collection
            .sources()
            .iter()
            .filter(|source| source.kind == ExportKind::Archive)
            .map(|source| fold_path(&source.path))
            .collect();
        let mut all = std::mem::take(&mut self.leases);
        all.extend(fresh);
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut kept: Vec<ArchiveLease> = Vec::new();
        // The newest lease of an archive is the last one; an older generation is let go.
        for lease in all.into_iter().rev() {
            let archive = fold_path(lease.archive());
            if shown.contains(&archive) && seen.insert(archive) {
                kept.push(lease);
            }
        }
        kept.reverse();
        self.leases = kept;
    }

    /// Writes state.json. A failure is only logged: the app works without the file.
    fn save_remembered(&self, remembered: &UiState) {
        if let Err(error) = remembered.save(&self.paths.state_file) {
            log::warn!("{}", error.message);
        }
    }
}

/// Carries out one action. This is the only function that changes `AppState`.
pub fn apply(state: &mut AppState, services: &mut Services, action: Action) {
    match action {
        Action::ShowHome => {
            state.view = View::Home;
            state.more_menu_open = false;
            state.scroll_request = Some(ScrollRequest::Top);
        }
        Action::PickPictures => {
            state.more_menu_open = false;
            services.opener.request_dialog(DialogKind::Pictures);
        }
        Action::PickFolder => {
            state.more_menu_open = false;
            services.opener.request_dialog(DialogKind::Folder);
        }
        Action::OpenPaths(paths) => {
            // "Open the web app's library" is a path to open, chosen in the More menu.
            state.more_menu_open = false;
            if !paths.is_empty() {
                state.opening_since.get_or_insert(state.time);
                let current = state.collection.as_ref().map(|collection| (collection, state.collection_serial));
                services.opener.open(paths, current, &services.open_options(), &services.store, &services.context);
            }
        }
        Action::ClosePictures => {
            state.more_menu_open = false;
            if state.collection.is_none() {
                return;
            }
            if state.download_running() {
                // The download works on the pictures that are about to go: ask first.
                remember_dialog_focus(state, services);
                state.confirm = Some(ConfirmState { kind: ConfirmKind::ClosePictures, opened_at: state.time });
            } else {
                close_pictures(state, services);
            }
        }
        Action::ShowExplore => {
            state.view = View::Explore;
            state.more_menu_open = false;
        }
        Action::ToggleMoreMenu => {
            state.more_menu_open = !state.more_menu_open;
            if state.more_menu_open {
                // One look at the disk as the menu opens, not one per painted frame.
                state.web_app_library = services.paths.web_app_library_dir.clone().filter(|folder| folder.join(instagram_core::manifest::MANIFEST_NAMES[0]).is_file());
                let library = services.web.library_dir().to_path_buf();
                let open_already = state.collection.as_ref().is_some_and(|collection| collection.sources().iter().any(|source| source.kind == ExportKind::WebLibrary));
                state.browser_imports = (!open_already && library.join(instagram_core::manifest::MANIFEST_NAMES[0]).is_file()).then_some(library);
            }
        }
        Action::CloseMoreMenu => state.more_menu_open = false,
        Action::SwitchAppearance => {
            state.theme = state.theme.toggled();
            state.more_menu_open = false;
            state.remembered.theme = state.theme;
            services.save_remembered(&state.remembered);
            if let Some(ctx) = &services.ctx {
                theme::select_theme(ctx, state.theme);
            }
        }
        Action::ShowAbout => {
            state.more_menu_open = false;
            remember_dialog_focus(state, services);
            state.about_opened_at = Some(state.time);
        }
        Action::CloseAbout => {
            state.about_opened_at = None;
            restore_dialog_focus(state);
        }
        Action::ShowWarnings => {
            state.more_menu_open = false;
            if state.collection.as_ref().is_some_and(|collection| !collection.warnings.is_empty()) {
                remember_dialog_focus(state, services);
                state.warnings_opened_at = Some(state.time);
                state.warnings_all = false;
            }
        }
        Action::CloseWarnings => {
            state.warnings_opened_at = None;
            state.warnings_all = false;
            restore_dialog_focus(state);
        }
        Action::ToggleAllWarnings => state.warnings_all = !state.warnings_all,
        Action::ToggleFullscreen => set_fullscreen(state, services, !state.fullscreen),
        Action::ToggleLikeQuietly(key) => {
            let post = state.post_ui.entry(key).or_default();
            post.liked = !post.liked;
        }
        Action::ToggleLike(key) => {
            let time = state.time;
            let post = state.post_ui.entry(key).or_default();
            post.liked = !post.liked;
            // The heart pops on every click of the button, also when unliking (Feed.tsx:203-211).
            post.burst_at = Some(time);
        }
        Action::LikeByDoubleClick(key) => {
            let time = state.time;
            let post = state.post_ui.entry(key).or_default();
            post.liked = true;
            post.burst_at = Some(time);
        }
        Action::ToggleSave(key) => {
            let post = state.post_ui.entry(key).or_default();
            post.saved = !post.saved;
        }
        Action::ExpandCaption(key) => state.post_ui.entry(key).or_default().expanded = true,
        Action::ShowSlide(key, slide) => {
            let slide_count = state.collection.as_ref().and_then(|collection| collection.post(&key)).map(|post| post.media.len()).unwrap_or(0);
            if slide_count > 0 {
                let (time, animations) = (state.time, state.animations);
                let post = state.post_ui.entry(key).or_default();
                let target = slide.min(slide_count - 1);
                if target != post.slide {
                    let from = post.track_position(time, animations);
                    post.slide = target;
                    post.slide_motion = Some(SlideMotion { from, started_at: time });
                }
            }
        }
        Action::Share(key) => share(state, services, &key),
        Action::OpenLink(url) => {
            if let Err(error) = platform::open_link(&url) {
                log::warn!("{}", error.message);
                state.toast(error.message, messages::TOAST_ERROR_MS);
            }
        }
        Action::Toast(message, duration_ms) => state.toast(message, duration_ms),
        Action::OpenInBrowser => {
            state.more_menu_open = false;
            if let Some(url) = services.web.running().map(|running| running.page_url()) {
                if let Err(error) = platform::open_link(&url) {
                    log::warn!("{}", error.message);
                    state.toast(error.message, messages::TOAST_ERROR_MS);
                }
            }
        }
        Action::OpenBrowserImports => {
            let folder = services.web.library_dir().to_path_buf();
            apply(state, services, Action::OpenPaths(vec![folder]));
        }
        Action::ShowBrowserView => {
            state.more_menu_open = false;
            // About gives way: never two dialogs at once.
            if state.about_opened_at.take().is_none() {
                remember_dialog_focus(state, services);
            }
            // What a Save would keep: a value the command line fixed for this run shows the one
            // of settings.json; the running addresses are in the state lines above the fields.
            let values = services.web.sheet_values();
            state.browser_sheet_serial += 1;
            let frontend = values.frontend_port.to_string();
            let backend = values.backend_port.to_string();
            state.browser_sheet = Some(BrowserSheet {
                enabled: values.enabled,
                lan: values.lan,
                frontend_text: frontend.clone(),
                backend_text: backend.clone(),
                frontend_shown: frontend,
                backend_shown: backend,
                error: None,
                saving: false,
                opened_at: state.time,
                serial: state.browser_sheet_serial,
            });
        }
        Action::CloseBrowserView => {
            // A Save that moves the server finishes on its own; its result is then only logged.
            state.browser_sheet = None;
            restore_dialog_focus(state);
        }
        Action::BrowserViewServer(enabled) => {
            if let Some(sheet) = &mut state.browser_sheet {
                sheet.enabled = enabled;
            }
        }
        Action::BrowserViewLan(lan) => {
            if let Some(sheet) = &mut state.browser_sheet {
                sheet.lan = lan;
            }
        }
        Action::BrowserViewPort(field, text) => {
            if let Some(sheet) = &mut state.browser_sheet {
                let compact: String = text.chars().filter(|character| !character.is_whitespace()).take(PORT_FIELD_MAX_CHARS).collect();
                match field {
                    PortField::Frontend => sheet.frontend_text = compact,
                    PortField::Backend => sheet.backend_text = compact,
                }
            }
        }
        Action::BrowserViewSave => save_browser_view(state, services),

        Action::ToggleVideoSound => state.video_sound_on = !state.video_sound_on,
        Action::VideoVolume(volume) => {
            state.video_volume = volume.clamp(0.0, 1.0);
            // Moving the slider while the sound is off turns it on, as the browser's slider does.
            if state.video_volume > 0.0 {
                state.video_sound_on = true;
            }
        }
        Action::VideoTogglePlay(surface) => services.video.toggle_play(surface),
        Action::VideoSeek(surface, seconds, exact) => services.video.seek(surface, seconds, exact),
        Action::PlayInViewer(key, slide) => {
            apply(state, services, Action::ShowSlide(key.clone(), slide));
            apply(state, services, Action::OpenViewer(key));
        }
        Action::AutoViewWaitForVideos(wait_for_videos) => {
            if let Some(setup) = &mut state.auto_view_setup {
                setup.wait_for_videos = wait_for_videos;
            }
        }

        Action::OpenViewer(key) => {
            if state.post(&key).is_some() {
                state.more_menu_open = false;
                // "Go to post" comes from the sheet, which closes; the widget that opened the
                // sheet is the one to return to when the viewer closes.
                state.sheet = None;
                match &mut state.viewer {
                    Some(viewer) => viewer.key = key,
                    None => {
                        state.viewer_return_focus = state.dialog_return_focus.take().or_else(|| take_focus(services));
                        state.viewer = Some(ViewerState { key: key.clone(), opened_on: key });
                    }
                }
            }
        }
        Action::CloseViewer => close_viewer(state),
        Action::ViewerEscape => {
            if state.fullscreen {
                set_fullscreen(state, services, false);
            } else {
                close_viewer(state);
            }
        }

        Action::OpenPostOptions(key) => {
            if state.post(&key).is_some() {
                state.more_menu_open = false;
                remember_dialog_focus(state, services);
                state.sheet = Some(PostSheet { key, from_viewer: state.viewer.is_some(), opened_at: state.time });
            }
        }
        Action::ClosePostOptions => {
            state.sheet = None;
            restore_dialog_focus(state);
        }
        Action::CopyLink(key) => {
            state.sheet = None;
            restore_dialog_focus(state);
            let link = state.post(&key).and_then(|post| post.permalink.clone());
            match (link, &services.ctx) {
                (Some(link), Some(ctx)) => {
                    ctx.copy_text(link);
                    state.toast(messages::LINK_COPIED_TO_CLIPBOARD, messages::TOAST_MS);
                }
                _ => state.toast(messages::LINK_NOT_COPIED, messages::TOAST_MS),
            }
        }
        Action::ShowInFolder(key) => {
            state.sheet = None;
            restore_dialog_focus(state);
            let file = state.post(&key).and_then(|post| file_to_show(post, state.shown_slide(post)));
            if let Some(file) = file {
                if let Err(error) = platform::show_in_folder(&file) {
                    log::warn!("{}", error.message);
                    state.toast(error.message, messages::TOAST_ERROR_MS);
                }
            }
        }

        Action::OpenEditor(key) => {
            // Another window of the app may have saved descriptions since this one read the
            // file: the editor starts from what is stored now, not from what this window knew.
            refresh_descriptions(state, services);
            if let Some(post) = state.post(&key) {
                // The field holds the description as a text area would: line breaks are "\n".
                let mut field_text = post.caption.clone();
                text::enforce_caption_limit(&mut field_text);
                // The editor follows the sheet; the focus goes back to where it was before the
                // sheet opened once the editor closes.
                state.sheet = None;
                remember_dialog_focus(state, services);
                state.editor_serial += 1;
                state.editor = Some(EditorState { key, text: field_text, error: None, opened_at: state.time, serial: state.editor_serial });
            }
        }
        Action::EditorInput(mut field_text) => {
            if let Some(editor) = &mut state.editor {
                text::enforce_caption_limit(&mut field_text);
                editor.text = field_text;
            }
        }
        Action::SaveDescription => save_description(state, services),
        Action::CloseEditor => {
            state.editor = None;
            restore_dialog_focus(state);
        }

        Action::StartAutoView(start) => {
            // The setup dialog follows the sheet or the menu that asked for it; the focus goes
            // back to where it was before once the dialog (or the show) has closed.
            state.more_menu_open = false;
            state.sheet = None;
            remember_dialog_focus(state, services);
            let remembered = state.remembered.auto_view;
            state.auto_view_serial += 1;
            state.auto_view_setup = Some(AutoViewSetup {
                start: start.filter(|key| state.post(key).is_some()),
                text: remembered.interval_sec.to_string(),
                wait_for_videos: remembered.wait_for_videos,
                loop_posts: remembered.loop_posts,
                shuffle: remembered.shuffle,
                invalid: false,
                opened_at: state.time,
                serial: state.auto_view_serial,
            });
        }
        Action::AutoViewInput(field_text) => {
            if let Some(setup) = &mut state.auto_view_setup {
                setup.text = field_text;
                setup.invalid = false;
            }
        }
        Action::AutoViewPreset(seconds) => {
            if let Some(setup) = &mut state.auto_view_setup {
                setup.text = seconds.to_string();
                setup.invalid = false;
            }
        }
        Action::AutoViewLoop(loop_posts) => {
            if let Some(setup) = &mut state.auto_view_setup {
                setup.loop_posts = loop_posts;
            }
        }
        Action::AutoViewShuffle(shuffle) => {
            if let Some(setup) = &mut state.auto_view_setup {
                setup.shuffle = shuffle;
            }
        }
        Action::AutoViewBegin => begin_auto_view(state, services),
        Action::CloseAutoViewSetup => {
            state.auto_view_setup = None;
            restore_dialog_focus(state);
        }
        Action::AutoViewCommand(command) => auto_view_command(state, services, command),
        Action::ExitAutoView => exit_auto_view(state),
        Action::AutoViewOpenPost(key) => {
            // The viewer that opens takes over the widget the focus returns to.
            if let Some(run) = state.auto_view.take() {
                state.dialog_return_focus = run.return_focus;
            }
            apply(state, services, Action::OpenViewer(key));
        }

        Action::DownloadAll => {
            state.more_menu_open = false;
            if !state.posts().is_empty() {
                request_download(state, services, DownloadKind::All);
            }
        }
        Action::DownloadPost(key) => {
            state.more_menu_open = false;
            if state.sheet.take().is_some() {
                restore_dialog_focus(state);
            }
            if state.post(&key).is_some() {
                request_download(state, services, DownloadKind::Post(key));
            }
        }
        Action::CancelDownload => services.downloader.cancel(),

        Action::ConfirmAccept => {
            let Some(confirm) = state.confirm.take() else {
                return;
            };
            match confirm.kind {
                ConfirmKind::StoreNotice(_) => restore_dialog_focus(state),
                ConfirmKind::ClosePictures => {
                    // The cancelled download reports back and says so in a toast.
                    services.downloader.cancel();
                    state.dialog_return_focus = None;
                    close_pictures(state, services);
                }
                ConfirmKind::ReplaceParts { .. } => {
                    restore_dialog_focus(state);
                    if let Some(DownloadState { phase: DownloadPhase::Confirming { target_dir, name, stale }, .. }) = state.download.clone() {
                        start_export(state, services, target_dir, name, stale);
                    }
                }
            }
        }
        Action::ConfirmCancel => {
            if let Some(confirm) = state.confirm.take() {
                restore_dialog_focus(state);
                if matches!(confirm.kind, ConfirmKind::ReplaceParts { .. }) {
                    // Nothing was written; the download ends without a word, like a cancelled
                    // Save dialog.
                    state.download = None;
                }
            }
        }
        Action::OpenDataFolder => {
            let folder = services.paths.data_dir.clone();
            // The folder is created by the first description; show it even before that.
            let shown = std::fs::create_dir_all(&folder).map_err(|error| platform::PlatformError { message: strings::data_folder_not_opened(&folder.display().to_string(), &error.to_string()) }).and_then(|()| platform::open_folder(&folder));
            if let Err(error) = shown {
                log::warn!("{}", error.message);
                state.toast(error.message, messages::TOAST_ERROR_MS);
            }
        }
    }
}

/// The file "Show in folder" selects for slide `slide` of `post`: the file of the slide, or for a
/// post of an archive the archive (its copy in the archive cache is the app's own; the archive
/// is what the user has).
pub fn file_to_show(post: &Post, slide: usize) -> Option<PathBuf> {
    match &post.origin {
        PostOrigin::Archive { archive } => Some(archive.clone()),
        PostOrigin::File | PostOrigin::Folder { .. } => post.media.get(slide).map(|media| media.path.clone()),
    }
}

/// Back to the empty state: nothing is open any more.
fn close_pictures(state: &mut AppState, services: &mut Services) {
    state.collection = None;
    state.collection_serial += 1;
    state.content_serial += 1;
    state.post_ui.clear();
    state.view = View::Home;
    state.close_layers();
    state.scroll_request = Some(ScrollRequest::Top);
    services.loader.reset_requests();
    services.video.forget();
    // The archive cache may clean up what was shown.
    services.leases.clear();
}

// --- Auto view ----------------------------------------------------------------------------------

/// "Start" of the setup dialog (`App.tsx:982-994`): the seconds must be a whole number from 2
/// to 3600, otherwise the dialog stays and says so. The settings are remembered in state.json
/// and the show starts at the chosen post.
fn begin_auto_view(state: &mut AppState, services: &mut Services) {
    let Some(setup) = &mut state.auto_view_setup else {
        return;
    };
    if state.collection.as_ref().is_none_or(|collection| collection.posts.is_empty()) {
        // "Start" is disabled without posts.
        return;
    }
    let interval_sec = match autoview::check_interval_input(&setup.text) {
        Ok(interval_sec) => interval_sec,
        Err(_) => {
            setup.invalid = true;
            return;
        }
    };
    let settings = autoview::Settings { interval_sec, wait_for_videos: setup.wait_for_videos, loop_posts: setup.loop_posts, shuffle: setup.shuffle };
    let start = setup.start.clone();
    state.auto_view_setup = None;
    if state.remembered.auto_view != settings {
        state.remembered.auto_view = settings;
        services.save_remembered(&state.remembered);
    }
    let start_index = start.and_then(|key| state.collection.as_ref().and_then(|collection| collection.index_of(&key))).unwrap_or(0);
    start_auto_view(state, services, settings, start_index);
}

/// Starts the show at post `start_index` with `settings`. It closes the viewer and every
/// dialog, as the web project does; the widget that had the focus before gets it back at the end.
pub fn start_auto_view(state: &mut AppState, services: &mut Services, settings: autoview::Settings, start_index: usize) {
    let keys: Vec<PostKey> = state.posts().iter().map(|post| post.key.clone()).collect();
    let mut random = SeededRandom::new(text::now_ms() as u64);
    let cursor = autoview::start_cursor(&keys, start_index, settings.shuffle, &mut || random.next_f64());
    let return_focus = state.dialog_return_focus.take().or_else(|| state.viewer_return_focus.take()).or_else(|| take_focus(services));
    // The show has the keyboard: no button keeps the focus, so that Space pauses.
    let _ = take_focus(services);
    state.more_menu_open = false;
    state.viewer = None;
    state.viewer_return_focus = None;
    state.sheet = None;
    state.auto_view_setup = None;
    state.pending_focus = None;
    state.auto_view = Some(AutoViewRun {
        settings,
        player: PlayerState::new(cursor),
        timer: ActiveTimer::new(),
        slide_key: None,
        due_ms: None,
        expected_ms: 0.0,
        is_video: false,
        video: None,
        listen_for_end: false,
        post_shown_at: state.auto_view_now_ms() / 1000.0,
        last_tick_ms: None,
        synced_serial: state.collection_serial,
        return_focus,
    });
    // The clock of the first slide starts now, not at the next frame.
    tick_auto_view(state);
}

/// The time of a slide stops when no frame has run for this long while it was counting: the
/// window was minimized or the computer asleep, and nobody saw the picture meanwhile.
const AUTO_VIEW_MAX_FRAME_GAP_MS: f64 = 2_000.0;

/// Brings the clock of the running show up to date (`AutoView.tsx:265-311`, spec-viewer 4.5):
/// the timer follows the slide on show and the interval, stands still while the show is paused
/// or the window minimized, and moves the show on when the time of a slide is up. The app calls
/// it once per frame; it is cheap and does nothing without a show.
pub fn tick_auto_view(state: &mut AppState) {
    let now_ms = state.auto_view_now_ms();
    let hidden = state.window_minimized;
    let collection_serial = state.collection_serial;
    let Some(mut run) = state.auto_view.take() else {
        return;
    };
    // Pictures were added or replaced underneath: the order follows.
    if run.synced_serial != collection_serial {
        run.synced_serial = collection_serial;
        let ids: Vec<PostKey> = state.posts().iter().map(|post| post.key.clone()).collect();
        run.player.apply(PlayerAction::Sync { ids });
    }
    // Frames stopped coming while the time was running: that time does not count. (A clock
    // moved by hand jumps on purpose.)
    if let (Some(last_tick_ms), None) = (run.last_tick_ms, state.auto_view_clock) {
        if run.timer.is_running() && now_ms - last_tick_ms > AUTO_VIEW_MAX_FRAME_GAP_MS {
            run.timer.pause(last_tick_ms);
        }
    }
    run.last_tick_ms = Some(now_ms);

    let slide_count = |run: &AutoViewRun| run.player.current().and_then(|key| state.post(key)).map_or(1, |post| post.media.len().max(1));
    let slide_is_video = |run: &AutoViewRun, slide: usize| {
        run.player.current().and_then(|key| state.post(key)).and_then(|post| post.media.get(slide)).is_some_and(|media| media.kind == instagram_core::model::MediaKind::Video)
    };
    let wire = |run: &mut AutoViewRun| {
        let count = slide_count(run);
        let slide_key = (run.player.visit, run.player.current().cloned(), run.player.slide.min(count - 1));
        if run.slide_key.as_ref() != Some(&slide_key) {
            let post_changed = run.slide_key.as_ref().is_none_or(|previous| previous.0 != slide_key.0 || previous.1 != slide_key.1);
            if post_changed {
                run.post_shown_at = now_ms / 1000.0;
                log::debug!("Auto view shows post {} of {}.", run.player.cursor.position + 1, run.player.cursor.order.len());
            }
            run.slide_key = Some(slide_key);
            run.timer.reset(now_ms);
            run.due_ms = None;
        }
        // The length and end of the video on show count only for the slide they came from.
        let is_video = slide_is_video(run, run.player.slide.min(count - 1));
        let video = match &run.video {
            Some((slide, video)) if Some(slide) == run.slide_key.as_ref() => *video,
            _ => VideoState::UNKNOWN,
        };
        let plan = autoview::plan_slide(autoview::slide_seconds(run.settings.interval_sec, count), is_video, run.settings.wait_for_videos, video);
        run.is_video = is_video;
        run.listen_for_end = plan.listen_for_end;
        run.expected_ms = plan.expected_ms;
        if run.due_ms != Some(plan.due_ms) {
            run.due_ms = Some(plan.due_ms);
            run.timer.set_due(plan.due_ms);
        }
        run.timer.set_active(run.player.is_active(hidden), now_ms);
    };
    wire(&mut run);
    if run.timer.poll(now_ms) {
        let count = slide_count(&run);
        run.player.apply(PlayerAction::Advance { slide_count: count, loop_posts: run.settings.loop_posts });
        // The next slide's time starts in the same frame.
        wire(&mut run);
    }
    state.auto_view = Some(run);
}

/// A button or a key of the running show.
fn auto_view_command(state: &mut AppState, services: &mut Services, command: PlayerCommand) {
    let Some(run) = &mut state.auto_view else {
        return;
    };
    let loop_posts = run.settings.loop_posts;
    match command {
        PlayerCommand::Next => {
            run.player.apply(PlayerAction::Next { loop_posts });
        }
        PlayerCommand::Previous => {
            run.player.apply(PlayerAction::Previous { loop_posts });
        }
        PlayerCommand::TogglePause => {
            run.player.apply(PlayerAction::TogglePause);
        }
        PlayerCommand::Restart => {
            run.player.apply(PlayerAction::Restart);
        }
        PlayerCommand::Slide(slide) => {
            run.player.apply(PlayerAction::Slide { slide });
        }
        PlayerCommand::Longer | PlayerCommand::Shorter => {
            let step = if command == PlayerCommand::Longer { autoview::Step::Longer } else { autoview::Step::Shorter };
            let interval_sec = autoview::step_interval(run.settings.interval_sec, step);
            if interval_sec != run.settings.interval_sec {
                run.settings.interval_sec = interval_sec;
                // Only the time is remembered: loop and shuffle of this run may come from the
                // command line (`App.tsx:1000-1005`).
                state.remembered.auto_view.interval_sec = interval_sec;
                services.save_remembered(&state.remembered);
            }
        }
    }
    // The clock follows at once: a new slide starts its time now.
    tick_auto_view(state);
}

/// Leaves the running show; the widget that opened it gets the focus back.
fn exit_auto_view(state: &mut AppState) {
    if let Some(run) = state.auto_view.take() {
        if let Some(id) = run.return_focus {
            state.pending_focus = Some(PendingFocus { id, painted_frames: 0 });
        }
    }
}

/// Starts the show the command line asked for (`--autoview`), once pictures are on screen:
/// without the setup dialog, and without remembering its settings. It starts at the picture
/// the viewer was opened on (one picked picture), as "Auto view from here" does, else at the
/// first post.
pub fn start_requested_auto_view(state: &mut AppState, services: &mut Services) {
    if state.auto_view.is_some() || state.posts().is_empty() {
        return;
    }
    if let Some(request) = state.auto_view_request.take() {
        let settings = request.apply_to(state.remembered.auto_view);
        let start_index = state
            .viewer
            .as_ref()
            .and_then(|viewer| state.collection.as_ref().and_then(|collection| collection.index_of(&viewer.key)))
            .unwrap_or(0);
        start_auto_view(state, services, settings, start_index);
    }
}

// --- Download -----------------------------------------------------------------------------------

/// "Download post" and "Download all" (spec-edit 9): one download at a time; the Save dialog
/// comes first, filled with the web project's archive name.
fn request_download(state: &mut AppState, services: &mut Services, kind: DownloadKind) {
    if state.download.is_some() {
        state.toast(messages::DOWNLOAD_ALREADY_RUNNING, messages::TOAST_MS);
        return;
    }
    let (title, file_name) = match &kind {
        DownloadKind::All => (strings::DIALOG_DOWNLOAD_ALL, format!("{}.zip", export::DEFAULT_EXPORT_NAME)),
        DownloadKind::Post(key) => {
            let Some(post) = state.post(key) else {
                return;
            };
            (strings::DIALOG_DOWNLOAD_POST, format!("{}.zip", export::single_post_export_name(&post.id)))
        }
    };
    let folder = state.remembered.last_export_folder.clone().filter(|folder| folder.is_dir()).or_else(platform::downloads_folder);
    services.downloader.request_dialog(SaveRequest { title: title.to_owned(), file_name, folder });
    state.download = Some(DownloadState { kind, phase: DownloadPhase::Choosing, progress: None });
}

/// The posts a download of `kind` exports, as they are now.
fn download_posts(state: &AppState, kind: &DownloadKind) -> Vec<Post> {
    match kind {
        DownloadKind::All => state.posts().to_vec(),
        DownloadKind::Post(key) => state.post(key).cloned().into_iter().collect(),
    }
}

/// Starts writing the archive `{name}.zip` into `target_dir`.
fn start_export(state: &mut AppState, services: &mut Services, target_dir: PathBuf, name: String, stale: Vec<PathBuf>) {
    let Some(kind) = state.download.as_ref().map(|download| download.kind.clone()) else {
        return;
    };
    let posts = download_posts(state, &kind);
    let Some(download) = &mut state.download else {
        return;
    };
    if posts.is_empty() {
        // The pictures were closed while the dialog was open.
        state.download = None;
        return;
    }
    download.progress = Some(ExportProgress { done: 0, total: posts.len() });
    download.phase = DownloadPhase::Running { stale };
    if matches!(download.kind, DownloadKind::Post(_)) {
        state.toast(messages::PREPARING_DOWNLOAD, messages::TOAST_MS);
    }
    services.downloader.start(posts, target_dir, name);
}

/// Takes over what the downloader finished: the choice of the Save dialog starts the export
/// (after a question, when part files of an earlier download are in the way), and the end of
/// the export becomes a toast with the web project's wording (spec-edit 8).
pub fn handle_download_event(state: &mut AppState, services: &mut Services, event: DownloadEvent) {
    match event {
        DownloadEvent::Chosen(None) => {
            if state.download.as_ref().is_some_and(|download| download.phase == DownloadPhase::Choosing) {
                state.download = None;
            }
        }
        DownloadEvent::Chosen(Some(file)) => {
            let Some(kind) = state.download.as_ref().filter(|download| download.phase == DownloadPhase::Choosing).map(|download| download.kind.clone()) else {
                return;
            };
            let destination = export::split_destination(&file).filter(|(target_dir, _)| target_dir.is_dir());
            let Some((target_dir, name)) = destination else {
                log::warn!("{} cannot name an archive.", file.display());
                state.download = None;
                state.toast(messages::DOWNLOAD_NOT_PREPARED, messages::TOAST_ERROR_MS);
                return;
            };
            if state.remembered.last_export_folder.as_deref() != Some(target_dir.as_path()) {
                state.remembered.last_export_folder = Some(target_dir.clone());
                services.save_remembered(&state.remembered);
            }
            // The Save dialog asked about "{name}.zip" only. A large download is written as
            // "{name}-part1.zip", "{name}-part2.zip", ...: when files of those names are there
            // already, the user is asked before any of them is replaced.
            let may_split = download::may_split(&download_posts(state, &kind));
            let parts = if may_split { download::existing_parts(&target_dir, &name) } else { Vec::new() };
            let question = parts.first().map(|first| ConfirmKind::ReplaceParts { count: parts.len(), first: messages::display_name(first) });
            // "{name}.zip" itself, which the dialog did ask about, goes too when the download
            // turns out to be split: it would otherwise be taken for a part of the new one.
            let mut stale = parts;
            let whole = target_dir.join(export::archive_file_name(&name, 1, 1));
            if may_split && whole.is_file() {
                stale.push(whole);
            }
            match question {
                Some(kind) => {
                    remember_dialog_focus(state, services);
                    state.confirm = Some(ConfirmState { kind, opened_at: state.time });
                    if let Some(download) = &mut state.download {
                        download.phase = DownloadPhase::Confirming { target_dir, name, stale };
                    }
                }
                None => start_export(state, services, target_dir, name, stale),
            }
        }
        DownloadEvent::Finished(result) => {
            let stale = match state.download.take() {
                Some(DownloadState { phase: DownloadPhase::Running { stale }, .. }) => stale,
                _ => Vec::new(),
            };
            match result {
                Ok(result) => {
                    if result.archives.len() > 1 {
                        // The user agreed to replace the earlier download: what the new parts
                        // did not overwrite would otherwise be taken for a part of this one.
                        let written: Vec<PathBuf> = result.archives.iter().map(|archive| archive.path.clone()).collect();
                        download::remove_stale(&stale, &written);
                    }
                    for problem in &result.problems {
                        log::warn!("Download: {problem}");
                    }
                    let message = export::download_message(&result);
                    log::info!("{message}");
                    state.toast(message, export::download_toast_ms(&result));
                }
                Err(error) if error.code == ExportErrorCode::Cancelled => state.toast(messages::DOWNLOAD_CANCELLED, messages::TOAST_MS),
                Err(error) => {
                    log::warn!("{}", error.message);
                    state.toast(error.message, messages::TOAST_ERROR_MS);
                }
            }
        }
    }
}

/// Remembers which widget has the keyboard focus before a dialog takes it. A dialog that
/// follows another (the editor after the sheet) keeps what the first one remembered.
fn remember_dialog_focus(state: &mut AppState, services: &Services) {
    if state.dialog_return_focus.is_none() {
        state.dialog_return_focus = services.ctx.as_ref().and_then(|ctx| ctx.memory(|memory| memory.focused()));
    }
}

/// Gives the keyboard focus back to the widget that had it before the dialog opened
/// (`common.tsx:358-408`). The app hands it over once the dialog has gone from the screen
/// (`AppState::pending_focus`); a widget that is gone meanwhile simply does not take it.
fn restore_dialog_focus(state: &mut AppState) {
    if let Some(id) = state.dialog_return_focus.take() {
        state.pending_focus = Some(PendingFocus { id, painted_frames: 0 });
    }
}

/// Takes the keyboard focus away from the widget that has it and returns that widget: the
/// viewer opens without a focused widget, so that its arrow keys move between pictures.
fn take_focus(services: &Services) -> Option<egui::Id> {
    let ctx = services.ctx.as_ref()?;
    let focused = ctx.memory(|memory| memory.focused())?;
    ctx.memory_mut(|memory| memory.surrender_focus(focused));
    Some(focused)
}

/// Closes the viewer. When the user moved to another post in it, the feed follows.
fn close_viewer(state: &mut AppState) {
    let Some(viewer) = state.viewer.take() else {
        return;
    };
    if viewer.key != viewer.opened_on && state.view == View::Home {
        state.scroll_request = Some(ScrollRequest::Post(viewer.key));
    }
    if let Some(id) = state.viewer_return_focus.take() {
        state.pending_focus = Some(PendingFocus { id, painted_frames: 0 });
    }
}

/// Makes the window fill the screen, or ends that.
fn set_fullscreen(state: &mut AppState, services: &Services, fullscreen: bool) {
    state.fullscreen = fullscreen;
    if let Some(ctx) = &services.ctx {
        ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(fullscreen));
    }
}

/// "Save" of the editor (`App.tsx:857-915`, spec-edit 4): the trimmed text goes to
/// descriptions.json and to the post. A failed write keeps the dialog open with the text and
/// shows the reason inside it; nothing on screen has changed then.
fn save_description(state: &mut AppState, services: &mut Services) {
    let Some(editor) = &mut state.editor else {
        return;
    };
    let Some(collection) = &mut state.collection else {
        return;
    };
    let Some(post) = collection.post_mut(&editor.key) else {
        return;
    };
    // Save is disabled until the trimmed text differs from the trimmed description.
    if !text::caption_changed(&editor.text, &post.caption) {
        return;
    }
    // A new attempt clears the error of the last one.
    editor.error = None;
    // Where the entry goes (the picture, the post.json, the archive) follows `post.origin`.
    match store::save_description(&mut services.store, post, &editor.text, text::now_ms()) {
        Ok(outcome) => {
            state.editor = None;
            restore_dialog_focus(state);
            if outcome != SaveOutcome::Unchanged {
                state.content_serial += 1;
                state.toast(messages::DESCRIPTION_UPDATED, messages::TOAST_MS);
            }
        }
        Err(error) => {
            log::warn!("{}", error.message);
            editor.error = Some(messages::description_not_saved(&error.message));
        }
    }
}

/// The share button: the post link of an export post, the file path of a loose picture.
fn share(state: &mut AppState, services: &mut Services, key: &PostKey) {
    let Some(post) = state.collection.as_ref().and_then(|collection| collection.post(key)) else {
        return;
    };
    let (text, message) = match (&post.permalink, post.cover()) {
        (Some(link), _) => (link.clone(), messages::LINK_COPIED),
        (None, Some(media)) => (media.path.display().to_string(), messages::PATH_COPIED),
        (None, None) => return,
    };
    match &services.ctx {
        Some(ctx) => {
            ctx.copy_text(text);
            state.toast(message, messages::TOAST_MS);
        }
        None => state.toast(messages::LINK_NOT_COPIED, messages::TOAST_MS),
    }
}

/// Takes over what the opener finished: a dialog choice becomes an open, a finished open goes on
/// screen, a failure becomes a toast and leaves the screen as it was.
pub fn handle_open_event(state: &mut AppState, services: &mut Services, event: OpenEvent) {
    match event {
        OpenEvent::Picked(paths) => apply(state, services, Action::OpenPaths(paths)),
        OpenEvent::Opened(result) => match result {
            // The editor holds text the user is typing: the new pictures wait until it closes.
            // The "Opening pictures…" chip stays meanwhile.
            Ok(opened) if state.editor.is_some() => {
                log::info!("The pictures that were opened wait until the description editor is closed.");
                state.held_open = Some(Box::new(opened));
            }
            Ok(opened) => show_opened(state, services, opened),
            Err(error) => {
                state.opening_since = None;
                report_open_error(state, &error);
            }
        },
        OpenEvent::Added { base_serial, result } => {
            state.opening_since = None;
            match result {
                // The pictures they were added to were closed or replaced meanwhile: what the
                // user did last counts, and closed pictures stay closed.
                Ok(_) | Err(_) if base_serial != state.collection_serial || state.collection.is_none() => {
                    log::info!("Pictures that were being added are dropped: the pictures they belonged to were closed or replaced meanwhile.");
                }
                Ok((collection, outcome)) => show_added(state, services, collection, &outcome),
                Err(error) => report_open_error(state, &error),
            }
        }
        OpenEvent::Merged { base_serial, result } => {
            state.opening_since = None;
            match result {
                // As for added pictures: what was closed or replaced meanwhile stays so.
                Ok(_) | Err(_) if base_serial != state.collection_serial || state.collection.is_none() => {
                    log::info!("Exports that were being added are dropped: the posts they belonged to were closed or replaced meanwhile.");
                }
                Ok((mut collection, added)) => {
                    if added.cache_written {
                        crate::opening::clean_up_cache_in_background(services.context.archive_cache.clone());
                    }
                    catalog::apply_descriptions(&mut collection, &services.store);
                    services.keep_leases(&collection, added.leases);
                    remember_folder(state, services, &collection);
                    log::info!("{} posts were added from export folders or archives; {} sources are shown.", added.added, collection.sources().len());
                    state.show_collection(collection, false);
                }
                Err(error) => report_open_error(state, &error),
            }
        }
    }
    if let Some(paths) = services.opener.take_waiting() {
        apply(state, services, Action::OpenPaths(paths));
    }
}

/// Puts a freshly opened collection on screen in place of the one shown.
fn show_opened(state: &mut AppState, services: &mut Services, mut opened: instagram_core::catalog::Opened) {
    state.opening_since = None;
    // A description saved while the folder was being read is laid over again: the open
    // started from the descriptions of that moment.
    catalog::apply_descriptions(&mut opened.collection, &services.store);
    let focus = opened.focus.and_then(|index| opened.collection.posts.get(index)).map(|post| post.key.clone());
    services.loader.reset_requests();
    services.video.forget();
    remember_folder(state, services, &opened.collection);
    // The new leases are taken before the old ones go: an archive opened again keeps its
    // generation all the time.
    services.leases = std::mem::take(&mut opened.leases);
    if opened.cache_written {
        crate::opening::clean_up_cache_in_background(services.context.archive_cache.clone());
    }
    state.show_collection(opened.collection, true);
    state.scroll_request = Some(ScrollRequest::Top);
    if let Some(key) = focus {
        // One picked picture opens in the viewer, and the feed behind it is at that picture
        // already.
        state.scroll_request = Some(ScrollRequest::Post(key.clone()));
        apply(state, services, Action::OpenViewer(key));
    }
    if let Some(notice) = opened.notice {
        // The one file the user picked is not among the pictures: say why at once.
        log::warn!("{notice}");
        state.toast(notice, messages::TOAST_ERROR_MS);
    }
}

/// Puts the collection with added pictures on screen. One picked picture is the one on show
/// afterwards (the viewer opens on it, or moves to it); several move an open viewer to the
/// first of them. Files of which none could be shown are explained in a toast.
fn show_added(state: &mut AppState, services: &mut Services, mut collection: Collection, outcome: &instagram_core::catalog::AddOutcome) {
    // A description saved while the folder was being read is laid over again.
    catalog::apply_descriptions(&mut collection, &services.store);
    let focus = outcome.focus.and_then(|index| collection.posts.get(index)).map(|post| post.key.clone());
    let known_warnings = state.collection.as_ref().map_or(0, |shown| shown.warnings.len());
    let new_warnings: Vec<String> = collection.warnings.iter().skip(known_warnings).map(|warning| warning.message.clone()).collect();
    remember_folder(state, services, &collection);
    state.show_collection(collection, false);
    if let Some(key) = focus {
        state.scroll_request = Some(ScrollRequest::Post(key.clone()));
        if outcome.one_file || state.viewer.is_some() {
            apply(state, services, Action::OpenViewer(key));
        }
    }
    if outcome.added == 0 {
        if let Some(first) = new_warnings.first() {
            state.toast(strings::files_not_added(first, new_warnings.len() - 1), messages::TOAST_ERROR_MS);
        }
    }
}

/// Shows an open that waited for the description editor to close (`AppState::held_open`). The
/// app calls it once per frame; it does nothing while the editor is open or nothing waits.
pub fn release_held_open(state: &mut AppState, services: &mut Services) {
    if state.editor.is_none() {
        if let Some(opened) = state.held_open.take() {
            show_opened(state, services, *opened);
        }
    }
}

/// Reads descriptions.json again and lays it over the posts on screen, so that what another
/// window of the app saved is shown (and edited) here too. A file that cannot be read or is
/// damaged changes nothing here: saving reports that with its reason.
fn refresh_descriptions(state: &mut AppState, services: &mut Services) {
    if store::refresh_from_disk(&mut services.store) {
        if let Some(collection) = &mut state.collection {
            catalog::apply_descriptions(collection, &services.store);
            state.content_serial += 1;
        }
    }
}

/// Remembers the folder of what was opened as the start folder of the next dialog.
fn remember_folder(state: &mut AppState, services: &Services, collection: &Collection) {
    if collection.folder.is_some() && collection.folder != state.remembered.last_folder {
        state.remembered.last_folder = collection.folder.clone();
        services.save_remembered(&state.remembered);
    }
}

// --- Browser pages ------------------------------------------------------------------------------

/// The two port fields of the Browser view sheet as numbers: whole numbers from 1 to 65535, and
/// two different ones. `Err` is the sentence the sheet shows.
pub fn validate_ports(frontend: &str, backend: &str) -> Result<(u16, u16), String> {
    let port = |text: &str, label: &str| -> Result<u16, String> {
        let text = text.trim();
        if text.is_empty() || !text.chars().all(|character| character.is_ascii_digit()) {
            return Err(messages::port_field_invalid(label));
        }
        match text.parse::<u32>() {
            Ok(value @ 1..=65535) => Ok(value as u16),
            _ => Err(messages::port_field_invalid(label)),
        }
    };
    let frontend = port(frontend, strings::BROWSER_VIEW_PAGE_PORT)?;
    let backend = port(backend, strings::BROWSER_VIEW_API_PORT)?;
    if frontend == backend {
        return Err(messages::PORTS_MUST_DIFFER.to_owned());
    }
    Ok((frontend, backend))
}

/// Save of the Browser view sheet: the fields are checked here; whether the typed ports can be
/// opened is found out by binding them (`WebService::save`), which may take a moment, so the
/// answer can come later (`handle_web_event`).
fn save_browser_view(state: &mut AppState, services: &mut Services) {
    let Some(sheet) = &mut state.browser_sheet else {
        return;
    };
    if sheet.saving {
        return;
    }
    let (frontend_port, backend_port) = match validate_ports(&sheet.frontend_text, &sheet.backend_text) {
        Ok(ports) => ports,
        Err(message) => {
            sheet.error = Some(message);
            return;
        }
    };
    sheet.error = None;
    let request = WebSaveRequest {
        enabled: sheet.enabled,
        lan: sheet.lan,
        frontend_port,
        backend_port,
        frontend_changed: sheet.frontend_text.trim() != sheet.frontend_shown,
        backend_changed: sheet.backend_text.trim() != sheet.backend_shown,
    };
    match services.web.save(request) {
        Some(result) => finish_browser_save(state, result),
        None => sheet.saving = true,
    }
}

/// The end of a Save of the Browser view sheet: it closes with a toast, or stays open and says
/// why (nothing was changed, or settings.json could not be written and the values apply for this
/// run only).
fn finish_browser_save(state: &mut AppState, result: Result<&'static str, String>) {
    match result {
        Ok(note) => {
            if state.browser_sheet.take().is_some() {
                restore_dialog_focus(state);
            }
            state.toast(note, messages::TOAST_MS);
        }
        Err(message) => {
            log::warn!("{message}");
            match &mut state.browser_sheet {
                Some(sheet) => {
                    sheet.saving = false;
                    sheet.error = Some(message);
                }
                None => state.toast(message, messages::TOAST_ERROR_MS),
            }
        }
    }
}

/// Takes over what the browser pages' threads reported (`WebService::poll`).
pub fn handle_web_event(state: &mut AppState, event: WebEvent) {
    match event {
        WebEvent::NotStarted(reason) => state.toast(messages::browser_not_started(&reason), messages::TOAST_ERROR_MS),
        // As long as an error: it must be seen (an open port never comes back unnoticed).
        WebEvent::OpenToNetwork => state.toast(strings::BROWSER_OPEN_TO_NETWORK, messages::TOAST_ERROR_MS),
        WebEvent::Saved(result) => finish_browser_save(state, result),
    }
}

/// Carries out a change a browser asked for (web brief 4.9) and answers it with the snapshot of
/// the new state: the caption of a post (the same code path as the editor's Save), the browser
/// imports folder added or refreshed, or what is open closed before the folder is cleared.
pub fn handle_web_command(state: &mut AppState, services: &mut Services, command: WebCommand) {
    let WebCommand { kind, reply } = command;
    let result = match kind {
        WebCommandKind::SetCaption { key, caption } => set_caption_from_browser(state, services, &key, caption),
        WebCommandKind::ShowWebLibrary { loaded, added } => show_web_library(state, services, loaded.map(|loaded| *loaded), added),
        WebCommandKind::CloseWebLibrary => {
            let only_web_library = state.collection.as_ref().is_some_and(|collection| matches!(collection.sources(), [source] if source.kind == ExportKind::WebLibrary));
            if only_web_library {
                close_pictures(state, services);
                Ok(())
            } else if state.collection.is_none() {
                Ok(())
            } else {
                Err(ApiError::new(403, messages::CLEAR_REFUSED))
            }
        }
    };
    let answer = result.and_then(|()| services.web.publish(state.collection.as_ref(), state.content_serial).ok_or_else(|| ApiError::new(503, messages::APP_CLOSING)));
    // The HTTP thread may have given up waiting meanwhile; then nobody reads the answer.
    let _ = reply.send(answer);
}

/// `SetCaption`: `store::save_clean_caption` on the open post. An open editor of the same post
/// keeps its text (the last save wins).
fn set_caption_from_browser(state: &mut AppState, services: &mut Services, key: &PostKey, caption: String) -> Result<(), ApiError> {
    let Some(post) = state.collection.as_mut().and_then(|collection| collection.post_mut(key)) else {
        return Err(ApiError::new(404, messages::LIBRARY_NO_SUCH_POST));
    };
    match store::save_clean_caption(&mut services.store, post, caption, text::now_ms()) {
        Ok(SaveOutcome::Unchanged) => Ok(()),
        Ok(_) => {
            log::info!("The description of post {} was changed in a browser.", post.id);
            state.content_serial += 1;
            Ok(())
        }
        Err(error) => {
            log::warn!("{}", error.message);
            Err(ApiError::new(503, error.message))
        }
    }
}

/// `ShowWebLibrary`: the browser imports folder joins the export collection on screen (read last:
/// its posts win on equal ids), is refreshed in place, opens on its own when nothing is open, or
/// goes when it has no post any more. Loose pictures are never mixed with it (409).
fn show_web_library(state: &mut AppState, services: &mut Services, loaded: Option<instagram_core::sources::LoadedSource>, added: usize) -> Result<(), ApiError> {
    if state.collection.as_ref().is_some_and(|collection| !collection.is_export()) {
        return Err(ApiError::new(409, messages::IMPORT_WHILE_LOOSE));
    }
    match (state.collection.clone(), loaded) {
        (None, Some(loaded)) => {
            let collection = catalog::collection_of_source(loaded, &services.store);
            services.loader.reset_requests();
            services.video.forget();
            state.show_collection(collection, true);
            state.scroll_request = Some(ScrollRequest::Top);
        }
        (Some(mut collection), Some(loaded)) => {
            catalog::add_loaded_source(&mut collection, loaded, &services.store);
            state.show_collection(collection, false);
        }
        (Some(mut collection), None) => {
            let manifest = services.web.library_dir().join(instagram_core::manifest::MANIFEST_NAMES[0]);
            let only = matches!(collection.sources(), [source] if source.kind == ExportKind::WebLibrary);
            if only {
                close_pictures(state, services);
            } else if catalog::remove_source(&mut collection, &manifest, &services.store) {
                state.show_collection(collection, false);
            }
        }
        (None, None) => {}
    }
    if added > 0 {
        log::info!("{added} posts were imported in a browser and are shown.");
        state.toast(messages::browser_import_added(added), messages::TOAST_MS);
    }
    Ok(())
}

/// An open that failed: the collection on screen stays, the reason is shown as a toast and the
/// collected details go to the log.
fn report_open_error(state: &mut AppState, error: &OpenError) {
    log::warn!("{}", error.message);
    for warning in &error.warnings {
        log::warn!("  {}", warning.message);
    }
    state.toast(error.message.clone(), messages::TOAST_ERROR_MS);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn at_most_three_toasts_and_a_repeat_replaces_its_copy() {
        let mut toasts = ToastStack::default();
        toasts.show("one", 2600, 0.0);
        toasts.show("two", 2600, 0.1);
        toasts.show("three", 2600, 0.2);
        toasts.show("four", 2600, 0.3);
        let shown: Vec<&str> = toasts.visible().iter().map(|toast| toast.message.as_str()).collect();
        assert_eq!(shown, ["two", "three", "four"]);
        // The repeated message moves to the end with a new lifetime; nothing else is dropped.
        toasts.show("three", 5200, 1.0);
        let shown: Vec<&str> = toasts.visible().iter().map(|toast| toast.message.as_str()).collect();
        assert_eq!(shown, ["two", "four", "three"]);
        assert!((toasts.visible()[2].expires_at - 6.2).abs() < 1e-9);
    }

    /// A loose picture named `name`, as the catalog would make it.
    fn picture(name: &str) -> Post {
        use instagram_core::model::{MediaItem, MediaKind, MediaType, User};
        let path = PathBuf::from(format!("C:\\pictures\\{name}"));
        Post {
            key: PostKey::for_file(&path),
            id: name.to_owned(),
            permalink: None,
            media: vec![MediaItem { kind: MediaKind::Image, path, manifest_path: name.to_owned(), poster: None, alt: None, bytes: 1 }],
            user: User::named("tester"),
            caption: String::new(),
            original_caption: String::new(),
            comment_count: 0,
            like_count: None,
            taken_at: None,
            media_type: MediaType::Image,
            alt: None,
            location: None,
            context: Some(name.to_owned()),
            comments: Vec::new(),
            origin: PostOrigin::File,
        }
    }

    /// A state with three loose pictures on screen and services that touch no real file: the
    /// profile is a folder that does not exist, and nothing in these tests saves.
    fn state_with_pictures() -> (AppState, Services) {
        let mut state = AppState::new(UiState::default());
        state.collection = Some(Collection {
            posts: vec![picture("a.jpg"), picture("b.jpg"), picture("c.jpg")],
            source: instagram_core::model::CollectionSource::Loose,
            label: "pictures".to_owned(),
            folder: None,
            warnings: Vec::new(),
            source_url: None,
            total_entries: 3,
        });
        let profile = std::env::temp_dir().join("instagram-windows-app-tests").join("no-such-profile");
        let paths = AppPaths::new(profile.join("settings.json"), profile.join("data"));
        let loader = Loader::new();
        let services = Services {
            ctx: None,
            default_username: "tester".to_owned(),
            store: DescriptionStore::new(paths.descriptions_file.clone()),
            opener: Opener::new(loader.waker()),
            downloader: Downloader::new(loader.waker()),
            context: OpenContext::new(paths.archive_cache_dir.clone()),
            leases: Vec::new(),
            web: WebService::new(crate::web::WebConfig::not_started(Default::default(), &[]), &paths),
            video: VideoHub::new(Box::new(crate::video::FakeBackend::default()), instagram_core::video_rules::VideoCaps::Cpu),
            loader,
            paths,
        };
        (state, services)
    }

    /// Runs the clock of the show forward to `time` in steps of a quarter of a second, as
    /// frames would.
    fn run_show_until(state: &mut AppState, time: f64) {
        while state.time < time {
            state.time = (state.time + 0.25).min(time);
            tick_auto_view(state);
        }
    }

    #[test]
    fn a_start_on_the_network_is_announced_as_long_as_an_error() {
        let (mut state, _services) = state_with_pictures();
        state.time = 3.0;
        handle_web_event(&mut state, WebEvent::OpenToNetwork);
        let shown = state.toasts.visible();
        assert_eq!(shown.len(), 1);
        assert_eq!(shown[0].message, "The browser pages are open to other devices on this network. Turn that off in Browser view.");
        assert!((shown[0].expires_at - (3.0 + messages::TOAST_ERROR_MS as f64 / 1000.0)).abs() < 1e-9);
    }

    #[test]
    fn the_show_follows_the_clock_of_the_app() {
        let (mut state, mut services) = state_with_pictures();
        state.time = 10.0;
        let settings = autoview::Settings { interval_sec: 4, wait_for_videos: true, loop_posts: false, shuffle: false };
        start_auto_view(&mut state, &mut services, settings, 1);
        let position = |state: &AppState| state.auto_view.as_ref().map(|run| (run.player.cursor.position, run.player.finished));
        // Started at the second picture: the order is not rotated.
        assert_eq!(position(&state), Some((1, false)));
        assert_eq!(state.window_title(), "b.jpg - Instagram Windows App");
        run_show_until(&mut state, 13.75);
        assert_eq!(position(&state), Some((1, false)));
        let progress = state.auto_view.as_ref().map(|run| run.progress(1, state.auto_view_now_ms())).unwrap_or_default();
        assert!((progress - 3.75 / 4.0).abs() < 1e-9, "{progress}");
        run_show_until(&mut state, 14.0);
        assert_eq!(position(&state), Some((2, false)));
        // The loop is off: the end screen after the last picture, and the title of the app.
        run_show_until(&mut state, 18.0);
        assert_eq!(position(&state), Some((2, true)));
        assert!(state.auto_view_post().is_none());
        assert_eq!(state.window_title(), "Instagram Windows App");
        // Leaving the show is all that is left.
        apply(&mut state, &mut services, Action::ExitAutoView);
        assert!(state.auto_view.is_none());
    }

    #[test]
    fn time_that_no_frame_saw_does_not_count_for_a_slide() {
        let (mut state, mut services) = state_with_pictures();
        let settings = autoview::Settings { interval_sec: 5, wait_for_videos: true, loop_posts: true, shuffle: false };
        start_auto_view(&mut state, &mut services, settings, 0);
        run_show_until(&mut state, 2.0);
        // The computer sleeps for an hour (or the window is dragged for a while): no frames.
        state.time = 3_602.0;
        tick_auto_view(&mut state);
        let run = state.auto_view.as_ref().expect("the show");
        assert_eq!(run.player.cursor.position, 0, "the picture is still the first");
        assert!((run.timer.elapsed_ms(state.auto_view_now_ms()) - 2_000.0).abs() < 1e-6);
        // It gets the three seconds it had left.
        run_show_until(&mut state, 3_604.75);
        assert_eq!(state.auto_view.as_ref().map(|run| run.player.cursor.position), Some(0));
        run_show_until(&mut state, 3_605.0);
        assert_eq!(state.auto_view.as_ref().map(|run| run.player.cursor.position), Some(1));
    }

    #[test]
    fn pictures_added_under_the_show_join_its_order() {
        let (mut state, mut services) = state_with_pictures();
        let settings = autoview::Settings { interval_sec: 5, wait_for_videos: true, loop_posts: false, shuffle: false };
        start_auto_view(&mut state, &mut services, settings, 2);
        // A picture arrives (an open that was on its way): it sorts before the one on show.
        if let Some(collection) = &mut state.collection {
            collection.posts.insert(0, picture("0.jpg"));
        }
        state.collection_serial += 1;
        tick_auto_view(&mut state);
        let run = state.auto_view.as_ref().expect("the show");
        assert_eq!(run.player.cursor.order.len(), 4);
        // The picture on show stays on show.
        assert_eq!(run.player.current(), Some(&PostKey::for_file(std::path::Path::new("C:\\pictures\\c.jpg"))));
    }

    #[test]
    fn show_in_folder_selects_the_archive_of_an_archive_post() {
        let mut post = picture("a.jpg");
        assert_eq!(file_to_show(&post, 0), Some(PathBuf::from("C:\\pictures\\a.jpg")));
        assert_eq!(file_to_show(&post, 3), None);
        post.origin = PostOrigin::Archive { archive: PathBuf::from("C:\\Downloads\\export.zip") };
        assert_eq!(file_to_show(&post, 0), Some(PathBuf::from("C:\\Downloads\\export.zip")));
    }

    #[test]
    fn leases_are_kept_for_the_archives_on_screen_only() {
        let (_state, mut services) = state_with_pictures();
        let folder = instagram_core::test_folder::TestFolder::new("leases").expect("a test folder");
        let cache = instagram_core::archive::cache::ArchiveCache::new(folder.join("cache"));
        let fixtures = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("core").join("tests").join("fixtures").join("archives");
        let mut budget = instagram_core::archive::MediaBudget::default();
        let one = instagram_core::archive::open_archive(&fixtures.join("export-part1.zip"), &cache, &mut budget).expect("part 1");
        let two = instagram_core::archive::open_archive(&fixtures.join("export-part2.zip"), &cache, &mut budget).expect("part 2");
        let again = instagram_core::archive::open_archive(&fixtures.join("export-part1.zip"), &cache, &mut budget).expect("part 1 again");
        let source = |path: PathBuf| instagram_core::model::ExportSource {
            kind: ExportKind::Archive,
            path,
            manifest: PathBuf::from("post.json"),
            name: String::new(),
            opened_at: 0,
            exported_at: None,
            source_url: None,
            total_entries: 0,
            shown: 0,
            replaced: 0,
        };
        let collection = Collection {
            posts: Vec::new(),
            source: instagram_core::model::CollectionSource::Exports(vec![source(fixtures.join("export-part1.zip"))]),
            label: String::new(),
            folder: None,
            warnings: Vec::new(),
            source_url: None,
            total_entries: 0,
        };
        services.keep_leases(&collection, vec![one.lease, two.lease, again.lease]);
        // One lease per archive that is shown, the newest one.
        assert_eq!(services.leases.len(), 1);
        assert_eq!(fold_path(services.leases[0].archive()), fold_path(&fixtures.join("export-part1.zip")));
        services.leases.clear();
    }

    #[test]
    fn toasts_expire_after_their_duration() {
        let mut toasts = ToastStack::default();
        toasts.show("short", 2600, 0.0);
        toasts.show("long", 5200, 0.0);
        let next = toasts.expire(1.0).expect("two toasts left");
        assert!((next - 1.6).abs() < 1e-9);
        toasts.expire(2.7);
        assert_eq!(toasts.visible().len(), 1);
        assert_eq!(toasts.visible()[0].message, "long");
        assert_eq!(toasts.expire(5.3), None);
        assert!(toasts.is_empty());
    }

    #[test]
    fn the_carousel_track_eases_from_where_it_was() {
        let mut post = PostUi { slide: 1, slide_motion: Some(SlideMotion { from: 0.0, started_at: 10.0 }), ..PostUi::default() };
        assert!(post.track_position(10.0, true).abs() < 1e-4);
        let half = post.track_position(10.14, true);
        assert!(half > 0.5 && half < 1.0);
        assert!((post.track_position(10.28, true) - 1.0).abs() < 1e-4);
        assert!(post.is_sliding(10.1, true));
        assert!(!post.is_sliding(10.3, true));
        // Without animations the track is at its target at once.
        assert_eq!(post.track_position(10.0, false), 1.0);
        assert!(!post.is_sliding(10.1, false));
        post.slide_motion = None;
        assert_eq!(post.track_position(0.0, true), 1.0);
        // A motion that never starts holds the track, with or without animations.
        post.slide_motion = Some(SlideMotion { from: 0.4, started_at: f64::INFINITY });
        assert_eq!(post.track_position(99.0, true), 0.4);
        assert_eq!(post.track_position(99.0, false), 0.4);
        assert!(!post.is_sliding(99.0, true));
    }
}
