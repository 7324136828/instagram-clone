//! The screenshot and scenario harness: hidden command-line options that let a script (or an
//! engineer without a desktop session) put the app into a state and save what it paints.
//!
//! ```text
//! --profile-dir <dir>      use <dir> instead of %APPDATA% for settings.json and the data folder
//!                          (ALWAYS pass a fresh folder when developing or testing: the real
//!                          profile must never be read or written by such a run)
//! --screenshot <png>       render with a hidden window, save the framebuffer as a PNG once the
//!                          app is idle (nothing being opened, decoded or laid out), exit 0
//! --size <W>x<H>           inner size of the window in points (default 1440x900 with --screenshot)
//! --scale <ppp>            force pixels_per_point; 1.0 gives 1 point = 1 pixel, as the reference
//!                          screenshots of the web project
//! --theme light|dark       start in this appearance, as if "Switch appearance" had chosen it
//!                          (state.json takes it over the next time the app saves its state)
//! --scenario <step>[,...]  put the interface into a state after loading; steps run in order,
//!                          each after the app became idle (may be given more than once)
//! --watchdog <seconds>     end the run with exit code 3 after this long (a screenshot run gives
//!                          up after 120 s without it; any other run only with it)
//! --frame-log              write to app.log how many frames ran (every five seconds in which
//!                          any ran) and the frame timings at the end of the run
//! --animate                keep animations and toast timers running in a screenshot run (they
//!                          are off by default so that a screenshot shows the final state); with
//!                          a `wait:<ms>` step this captures the middle of an animation
//! --video-adapter cpu|gpu  the video rules of a software renderer (no autoplay in the feed,
//!                          small frames) or of a graphics card, whatever the adapter is; not in
//!                          --help (screenshots and tests only)
//! <paths>                  pictures / folders to open, as in normal use
//! ```
//!
//! A scenario also works without `--screenshot`: the app then runs in its real window, the
//! steps are carried out, and after the last one the app is left alone. That is how the idle
//! app is measured from outside in a chosen state (`--frame-log --scenario viewer:0`).
//!
//! Scenario steps (`SCENARIO_STEPS` is the table the error message prints):
//!
//! ```text
//! feed                     the home feed (the default view)
//! explore                  the Explore grid
//! scroll:<px>              scroll the feed (or the grid) to this offset
//! post:<index>             scroll the feed to post <index> (0-based)
//! viewer:<index>           open the viewer on post <index>
//! zoom:<factor>            zoom the picture in the viewer: 1 is 100 %, 2.5 is 250 %, "fit" fits it
//! sheet:<index>            open the "Post options" sheet of post <index> (from the viewer when
//!                          it is open, else from the feed)
//! edit:<index>             open the "Edit description" dialog of post <index>
//! edit-text:<text>         replace the text of the open editor; "\n" in <text> is a line break
//!                          (no comma in the text: it separates the steps)
//! edit-save                press "Save" of the open editor
//! block-save               make saving descriptions fail from now on: descriptions.json is held
//!                          open exclusively, as another program might (needs --profile-dir)
//! more-menu                open the More menu
//! about                    open the About sheet
//! browser-view             open the Browser view sheet (addresses, ports, LAN)
//! wait-file:<path>         wait until the file <path> exists (an end-to-end script creates it
//!                          to say when the next steps may run; no comma in the path)
//! close-pictures           "Close pictures" of the More menu (asks first while a download runs)
//! autoview-setup           open the setup dialog of auto view
//! autoview-text:<text>     replace the text of the seconds field of that dialog
//! autoview-start           press "Start" of that dialog
//! autoview:<index>         start auto view at post <index> with the remembered settings; its
//!                          clock is held two seconds into the first slide and only moves with
//!                          autoview-time
//! autoview-time:<seconds>  set the clock of the running auto view to this many seconds after
//!                          its start
//! autoview-paused          pause the running auto view
//! autoview-end             show the end screen of the running auto view (loop off, last post over)
//! export-progress          show the banner as it is in the middle of "Download all" (half of
//!                          the posts prepared); nothing is written
//! download-all:<file>      "Download all" into <file>, as if it had been picked in the Save
//!                          dialog (no comma in the path)
//! download-post:<index>@<file>   "Download post" of post <index> into <file>
//! warnings | warnings-all  open the warnings dialog (first five | all)
//! drop-overlay             show the overlay of a file drag
//! busy                     show the "Opening pictures…" chip
//! toast:<text>             show a toast (no comma in the text)
//! like:<index> save:<index> expand:<index>      toggle like / save, expand the caption
//! slide:<index>/<n>        show slide <n> (0-based) of the carousel of post <index>; a fraction
//!                          such as 0.5 holds the track between two slides
//! burst:<index>            like post <index> and freeze the heart at its peak
//! hover:<name>[#k]         move the pointer over the k-th (default first) widget with this
//!                          accessible name, e.g. hover:Home, hover:Like#2; hover:@<x>x<y> for
//!                          a position
//! click:<name>[#k]         click that widget; double-click:<name>[#k] double-clicks it
//! wheel:<notches>          turn the mouse wheel at the pointer: positive is away from the user
//!                          (zooms in on the picture of the viewer)
//! drag:<dx>x<dy>           press the mouse button at the pointer, move by this many points,
//!                          release
//! key:<name>               press Home, End, PageUp, PageDown, ArrowUp, ArrowDown, ArrowLeft,
//!                          ArrowRight, Escape, Tab, Enter, Space, F11, Plus, Minus, 0, K or M;
//!                          with "Ctrl+" or "Shift+" before the name the modifier is held
//! type:<text>              type this text into the focused field ("\n" is a line break)
//! paste:<text>             paste this text into the focused field ("\r\n" written out is the
//!                          line break of text copied in Windows)
//! sweep:<px>x<frames>      scroll down by <px> per frame for <frames> frames without waiting
//!                          for pictures (the scrolling benchmark; implies --frame-log)
//! wait:<ms>                do nothing for this many milliseconds (to look at a native dialog
//!                          or the middle of an animation from outside)
//! ```
//!
//! Build the executable with `cargo build -p instagram-windows-app` right before a harness run.
//! `cargo test` links `build/target/debug/instagram-windows-app.exe` again with the features of
//! the test tools, which switch on every wgpu backend: that variant still renders with DX12 but
//! fills the log with messages of the Vulkan loader.
//!
//! An unknown step ends the run with exit code 2 and the list above. Exit codes: 0 screenshot
//! saved, 2 bad command line or scenario, 3 watchdog, 4 the image could not be saved or is blank.
//!
//! The recipe for a reliable screenshot comes from the toolchain spike (spike-notes section 2):
//! the window stays hidden and is driven with `RequestPaintWhileHidden`, because a visible window
//! is only painted when Windows asks for it; painted frames are counted in `App::ui`; the window
//! is told it has the focus so that carets and focus rings are drawn; and the process exits hard
//! as soon as the file is written, because tearing down the GPU device can block forever on a
//! machine without a real GPU.

use std::collections::VecDeque;
use std::path::{Path, PathBuf};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use egui::{pos2, vec2, Event, Key, Modifiers, PointerButton, Pos2};
use instagram_core::model::PostKey;
use instagram_core::store::Theme;

use crate::app::App;
use crate::state::{self, Action, ScrollRequest};
use crate::widgets;

/// The window size of a screenshot when `--size` is not given.
pub const DEFAULT_SCREENSHOT_SIZE: (f32, f32) = (1440.0, 900.0);
/// The watchdog fires after this many seconds unless `--watchdog` says otherwise.
pub const DEFAULT_WATCHDOG_SECONDS: u64 = 120;
/// The screenshot is taken after this many painted frames in which the app was idle.
const IDLE_FRAMES_BEFORE_SHOT: u32 = 3;
/// After a scenario step the app gets this many frames before the next step or the screenshot.
const SETTLE_FRAMES_AFTER_STEP: u32 = 3;
/// How often the window is asked to take the wanted size before the run goes on regardless.
const MAX_RESIZE_ATTEMPTS: u32 = 120;
/// Two scripted clicks this many seconds apart still count as a double click.
const SCRIPTED_DOUBLE_CLICK_SECONDS: f64 = 2.0;
/// The fraction of the like-heart animation `burst:<index>` freezes at: the peak of the pop.
const BURST_FREEZE_FRACTION: f32 = 0.35;
/// Frames a scripted turn of the wheel is given: egui smooths scrolling over several frames.
const WHEEL_SETTLE_FRAMES: u32 = 12;
/// Where `autoview:<index>` holds the clock of the show: this many seconds into the first slide.
const AUTO_VIEW_SHOT_SECONDS: f64 = 2.0;
/// The time between two frames a scenario asks for when the window is visible.
const VISIBLE_RUN_FRAME_INTERVAL: Duration = Duration::from_millis(30);
/// A run that waits for the app to become idle writes what it is waiting for this often.
const BUSY_REPORT_INTERVAL: Duration = Duration::from_secs(2);
/// Pointer moves a scripted drag is made of.
const DRAG_STEPS: u32 = 4;
/// The modifiers of a held Ctrl key as the window reports them on Windows: Ctrl itself and
/// egui's platform-independent "command".
pub const CTRL_KEY: Modifiers = Modifiers { alt: false, ctrl: true, shift: false, mac_cmd: false, command: true };

/// Exit code of a bad command line or scenario.
pub const EXIT_USAGE: i32 = 2;
/// Exit code of the watchdog.
pub const EXIT_WATCHDOG: i32 = 3;
/// Exit code when the screenshot could not be saved or is blank.
pub const EXIT_SCREENSHOT_FAILED: i32 = 4;

/// The scenario steps, as "name" and "what it does": printed when a step is unknown.
pub const SCENARIO_STEPS: &[(&str, &str)] = &[
    ("feed", "the home feed"),
    ("explore", "the Explore grid"),
    ("scroll:<px>", "scroll the feed (or the grid) to this offset"),
    ("post:<index>", "scroll the feed to this post (0-based)"),
    ("viewer:<index>", "open the viewer on a post"),
    ("zoom:<factor>", "zoom the picture in the viewer (1 is 100 %, or zoom:fit)"),
    ("sheet:<index>", "open the \"Post options\" sheet of a post"),
    ("edit:<index>", "open the \"Edit description\" dialog of a post"),
    ("edit-text:<text>", "replace the text of the open editor (\\n is a line break)"),
    ("edit-save", "press \"Save\" of the open editor"),
    ("block-save", "make saving descriptions fail (needs --profile-dir)"),
    ("more-menu", "open the More menu"),
    ("about", "open the About sheet"),
    ("browser-view", "open the Browser view sheet"),
    ("wait-file:<path>", "wait until this file exists (an end-to-end script creates it)"),
    ("close-pictures", "\"Close pictures\" of the More menu"),
    ("autoview-setup", "open the setup dialog of auto view"),
    ("autoview-text:<text>", "replace the text of the seconds field of that dialog"),
    ("autoview-start", "press \"Start\" of that dialog"),
    ("autoview:<index>", "start auto view at a post, its clock held two seconds into the slide"),
    ("autoview-time:<seconds>", "set the clock of the running auto view (seconds after its start)"),
    ("autoview-paused", "pause the running auto view"),
    ("autoview-end", "show the end screen of the running auto view"),
    ("export-progress", "show the banner in the middle of \"Download all\""),
    ("download-all:<file>", "\"Download all\" into this file, without the Save dialog"),
    ("download-post:<index>@<file>", "\"Download post\" of a post into this file"),
    ("warnings", "open the warnings dialog"),
    ("warnings-all", "open the warnings dialog and show every warning"),
    ("drop-overlay", "show the overlay of a file drag"),
    ("busy", "show the \"Opening pictures…\" chip"),
    ("toast:<text>", "show a toast"),
    ("like:<index>", "toggle the like of a post"),
    ("save:<index>", "toggle the bookmark of a post"),
    ("expand:<index>", "expand the caption of a post"),
    ("slide:<index>/<n>", "show slide <n> of the carousel of a post"),
    ("burst:<index>", "like a post and freeze the heart at its peak"),
    ("hover:<name>[#k]", "move the pointer over a widget by its accessible name (or hover:@<x>x<y>)"),
    ("click:<name>[#k]", "click a widget by its accessible name"),
    ("double-click:<name>[#k]", "double-click a widget by its accessible name"),
    ("wheel:<notches>", "turn the mouse wheel at the pointer (positive zooms in)"),
    ("drag:<dx>x<dy>", "drag from the pointer by this many points"),
    ("key:<name>", "press a key: Home, End, PageUp, PageDown, Arrow..., Escape, Tab, Enter, Space, F11, Plus, Minus, 0, K, M; Ctrl+ or Shift+ before it"),
    ("type:<text>", "type text into the focused field (\\n is a line break)"),
    ("paste:<text>", "paste text into the focused field"),
    ("sweep:<px>x<frames>", "scroll down by <px> per frame for <frames> frames and log the frame timings"),
    ("wait:<ms>", "do nothing for this many milliseconds"),
];

/// Where a pointer step points.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// The k-th widget (0-based) with this accessible name.
    Named(String, usize),
    /// A position in points.
    Position(Pos2),
}

/// One step of a scenario.
#[derive(Debug, Clone, PartialEq)]
pub enum Step {
    Feed,
    Explore,
    Scroll(f32),
    Post(usize),
    Viewer(usize),
    /// A scale (1.0 is 100 %), or None for the fitted picture.
    Zoom(Option<f32>),
    Sheet(usize),
    Edit(usize),
    EditText(String),
    EditSave,
    BlockSave,
    MoreMenu,
    About,
    BrowserView,
    WaitFile(PathBuf),
    ClosePictures,
    AutoViewSetup,
    AutoViewText(String),
    AutoViewStart,
    AutoView(usize),
    /// Seconds after the start of the show.
    AutoViewTime(f64),
    AutoViewPaused,
    AutoViewEnd,
    ExportProgress,
    DownloadAll(PathBuf),
    DownloadPost(usize, PathBuf),
    Warnings,
    WarningsAll,
    DropOverlay,
    Busy,
    Toast(String),
    Like(usize),
    Save(usize),
    Expand(usize),
    Slide(usize, f32),
    Burst(usize),
    Hover(Target),
    Click(Target),
    DoubleClick(Target),
    /// Notches of the mouse wheel; positive is away from the user.
    Wheel(f32),
    /// A drag from the pointer by this many points.
    Drag(egui::Vec2),
    Key(Key, Modifiers),
    Type(String),
    Paste(String),
    Sweep { pixels_per_frame: f32, frames: u32 },
    Wait(u64),
}

/// Raised for an option or a scenario step that cannot be understood. The message is ready for
/// stderr.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HarnessError {
    pub message: String,
}

impl std::fmt::Display for HarnessError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for HarnessError {}

/// A widget name cut to a length that keeps the list in an error message readable.
fn shorten(name: &str) -> String {
    const MAX_NAME_CHARS: usize = 40;
    if name.chars().count() <= MAX_NAME_CHARS {
        name.to_owned()
    } else {
        format!("{}…", name.chars().take(MAX_NAME_CHARS).collect::<String>())
    }
}

/// The list of valid steps, for the error message.
fn valid_steps() -> String {
    SCENARIO_STEPS.iter().map(|(name, what)| format!("  {name:<26} {what}")).collect::<Vec<_>>().join("\n")
}

fn unknown_step(step: &str, problem: &str) -> HarnessError {
    HarnessError { message: format!("Unknown scenario step \"{step}\": {problem}\nValid steps:\n{}", valid_steps()) }
}

/// Parses a pointer target: `Name`, `Name#2` (the second widget of that name) or `@120x340`.
fn parse_target(text: &str, step: &str) -> Result<Target, HarnessError> {
    if let Some(position) = text.strip_prefix('@') {
        let (x, y) = position.split_once('x').ok_or_else(|| unknown_step(step, "a position is written @<x>x<y>."))?;
        return match (x.parse::<f32>(), y.parse::<f32>()) {
            (Ok(x), Ok(y)) => Ok(Target::Position(pos2(x, y))),
            _ => Err(unknown_step(step, "a position is written @<x>x<y>.")),
        };
    }
    if text.is_empty() {
        return Err(unknown_step(step, "the name of a widget is missing."));
    }
    match text.rsplit_once('#') {
        Some((name, number)) if !name.is_empty() => match number.parse::<usize>() {
            Ok(number) if number >= 1 => Ok(Target::Named(name.to_owned(), number - 1)),
            _ => Ok(Target::Named(text.to_owned(), 0)),
        },
        _ => Ok(Target::Named(text.to_owned(), 0)),
    }
}

/// Parses one scenario step such as `scroll:1200` or `more-menu`.
pub fn parse_step(text: &str) -> Result<Step, HarnessError> {
    let (name, argument) = match text.split_once(':') {
        Some((name, argument)) => (name, Some(argument)),
        None => (text, None),
    };
    let index = |argument: Option<&str>| argument.and_then(|value| value.parse::<usize>().ok()).ok_or_else(|| unknown_step(text, "it needs the index of a post, e.g. like:0."));
    let none = |step: Step| if argument.is_none() { Ok(step) } else { Err(unknown_step(text, "it takes no argument.")) };
    // A text argument: "\n" written out is a line break, "\r" a carriage return (as in text
    // copied in Windows).
    let unescape = |value: &str| value.replace("\\r", "\r").replace("\\n", "\n");
    let written_text = |argument: Option<&str>| argument.filter(|value| !value.is_empty()).map(unescape);
    match name {
        "feed" => none(Step::Feed),
        "explore" => none(Step::Explore),
        "viewer" => index(argument).map(Step::Viewer),
        "sheet" => index(argument).map(Step::Sheet),
        "edit" => index(argument).map(Step::Edit),
        "edit-save" => none(Step::EditSave),
        "block-save" => none(Step::BlockSave),
        "edit-text" => Ok(Step::EditText(unescape(argument.unwrap_or("")))),
        "type" => written_text(argument).map(Step::Type).ok_or_else(|| unknown_step(text, "it needs a text, e.g. type:Hello.")),
        "paste" => written_text(argument).map(Step::Paste).ok_or_else(|| unknown_step(text, "it needs a text, e.g. paste:Hello.")),
        "zoom" => match argument {
            Some("fit") => Ok(Step::Zoom(None)),
            Some(value) => value.parse::<f32>().ok().filter(|factor| factor.is_finite() && *factor > 0.0).map(|factor| Step::Zoom(Some(factor))).ok_or_else(|| unknown_step(text, "it needs a factor, e.g. zoom:1 for 100 %, or zoom:fit.")),
            None => Err(unknown_step(text, "it needs a factor, e.g. zoom:1 for 100 %, or zoom:fit.")),
        },
        "wheel" => argument.and_then(|value| value.parse::<f32>().ok()).filter(|notches| notches.is_finite() && *notches != 0.0).map(Step::Wheel).ok_or_else(|| unknown_step(text, "it needs a number of notches, e.g. wheel:3 or wheel:-2.")),
        "drag" => {
            let parts = argument.and_then(|value| value.split_once('x'));
            match parts.map(|(dx, dy)| (dx.parse::<f32>(), dy.parse::<f32>())) {
                Some((Ok(dx), Ok(dy))) if dx.is_finite() && dy.is_finite() => Ok(Step::Drag(vec2(dx, dy))),
                _ => Err(unknown_step(text, "it is written drag:<dx>x<dy>, e.g. drag:-120x40.")),
            }
        }
        "more-menu" => none(Step::MoreMenu),
        "about" => none(Step::About),
        "browser-view" => none(Step::BrowserView),
        "wait-file" => argument.filter(|value| !value.is_empty()).map(|value| Step::WaitFile(PathBuf::from(value))).ok_or_else(|| unknown_step(text, "it needs the file to wait for, e.g. wait-file:C:/temp/go.txt.")),
        "close-pictures" => none(Step::ClosePictures),
        "autoview-setup" => none(Step::AutoViewSetup),
        "autoview-text" => Ok(Step::AutoViewText(argument.unwrap_or("").to_owned())),
        "autoview-start" => none(Step::AutoViewStart),
        "autoview" => index(argument).map(Step::AutoView),
        "autoview-time" => argument.and_then(|value| value.parse::<f64>().ok()).filter(|seconds| seconds.is_finite() && *seconds >= 0.0).map(Step::AutoViewTime).ok_or_else(|| unknown_step(text, "it needs seconds, e.g. autoview-time:7.5.")),
        "autoview-paused" => none(Step::AutoViewPaused),
        "autoview-end" => none(Step::AutoViewEnd),
        "export-progress" => none(Step::ExportProgress),
        "download-all" => argument.filter(|value| !value.is_empty()).map(|value| Step::DownloadAll(PathBuf::from(value))).ok_or_else(|| unknown_step(text, "it needs the file to write, e.g. download-all:C:/out/instagram-export.zip.")),
        "download-post" => {
            let parts = argument.and_then(|value| value.split_once('@'));
            match parts.map(|(post, file)| (post.parse::<usize>(), file)) {
                Some((Ok(post), file)) if !file.is_empty() => Ok(Step::DownloadPost(post, PathBuf::from(file))),
                _ => Err(unknown_step(text, "it is written download-post:<index>@<file>, e.g. download-post:0@C:/out/post.zip.")),
            }
        }
        "warnings" => none(Step::Warnings),
        "warnings-all" => none(Step::WarningsAll),
        "drop-overlay" => none(Step::DropOverlay),
        "busy" => none(Step::Busy),
        "scroll" => argument.and_then(|value| value.parse::<f32>().ok()).filter(|value| value.is_finite()).map(Step::Scroll).ok_or_else(|| unknown_step(text, "it needs an offset in points, e.g. scroll:1200.")),
        "post" => index(argument).map(Step::Post),
        "toast" => argument.filter(|value| !value.is_empty()).map(|value| Step::Toast(value.to_owned())).ok_or_else(|| unknown_step(text, "it needs a text, e.g. toast:Link copied.")),
        "like" => index(argument).map(Step::Like),
        "save" => index(argument).map(Step::Save),
        "expand" => index(argument).map(Step::Expand),
        "burst" => index(argument).map(Step::Burst),
        "slide" => {
            let parts = argument.and_then(|value| value.split_once('/'));
            match parts.map(|(post, slide)| (post.parse::<usize>(), slide.parse::<f32>())) {
                Some((Ok(post), Ok(slide))) if slide.is_finite() && slide >= 0.0 => Ok(Step::Slide(post, slide)),
                _ => Err(unknown_step(text, "it is written slide:<index>/<n>, e.g. slide:4/1.")),
            }
        }
        "hover" => parse_target(argument.unwrap_or(""), text).map(Step::Hover),
        "click" => parse_target(argument.unwrap_or(""), text).map(Step::Click),
        "double-click" => parse_target(argument.unwrap_or(""), text).map(Step::DoubleClick),
        "key" => {
            let mut modifiers = Modifiers::NONE;
            let mut name = argument.unwrap_or("");
            loop {
                if let Some(rest) = name.strip_prefix("Ctrl+") {
                    modifiers |= CTRL_KEY;
                    name = rest;
                } else if let Some(rest) = name.strip_prefix("Shift+") {
                    modifiers |= Modifiers::SHIFT;
                    name = rest;
                } else {
                    break;
                }
            }
            let key = match name {
                "Home" => Key::Home,
                "End" => Key::End,
                "PageUp" => Key::PageUp,
                "PageDown" => Key::PageDown,
                "ArrowUp" => Key::ArrowUp,
                "ArrowDown" => Key::ArrowDown,
                "ArrowLeft" => Key::ArrowLeft,
                "ArrowRight" => Key::ArrowRight,
                "Escape" => Key::Escape,
                "Tab" => Key::Tab,
                "Enter" => Key::Enter,
                "Space" => Key::Space,
                "F11" => Key::F11,
                "Plus" => Key::Plus,
                "Minus" => Key::Minus,
                "0" => Key::Num0,
                "K" => Key::K,
                "M" => Key::M,
                _ => {
                    return Err(unknown_step(
                        text,
                        "the key is not one of Home, End, PageUp, PageDown, ArrowUp, ArrowDown, ArrowLeft, ArrowRight, Escape, Tab, Enter, Space, F11, Plus, Minus, 0, K, M (with Ctrl+ or Shift+ before it).",
                    ))
                }
            };
            Ok(Step::Key(key, modifiers))
        }
        "sweep" => {
            let parts = argument.and_then(|value| value.split_once('x'));
            match parts.map(|(pixels, frames)| (pixels.parse::<f32>(), frames.parse::<u32>())) {
                Some((Ok(pixels), Ok(frames))) if pixels.is_finite() && frames > 0 => Ok(Step::Sweep { pixels_per_frame: pixels, frames }),
                _ => Err(unknown_step(text, "it is written sweep:<px>x<frames>, e.g. sweep:400x300.")),
            }
        }
        "wait" => argument.and_then(|value| value.parse::<u64>().ok()).filter(|milliseconds| *milliseconds <= 600_000).map(Step::Wait).ok_or_else(|| unknown_step(text, "it needs milliseconds, e.g. wait:500.")),
        _ => Err(unknown_step(text, "there is no step of this name.")),
    }
}

/// Parses the value of `--scenario`: steps separated by commas.
pub fn parse_scenario(text: &str) -> Result<Vec<Step>, HarnessError> {
    text.split(',').map(str::trim).filter(|step| !step.is_empty()).map(parse_step).collect()
}

/// Parses the value of `--size`: `1440x900`.
pub fn parse_size(text: &str) -> Result<(f32, f32), HarnessError> {
    let error = || HarnessError { message: format!("--size expects <W>x<H> in points, for example 1440x900, not \"{text}\".") };
    let (width, height) = text.split_once('x').ok_or_else(error)?;
    match (width.parse::<f32>(), height.parse::<f32>()) {
        (Ok(width), Ok(height)) if (100.0..=16_384.0).contains(&width) && (100.0..=16_384.0).contains(&height) => Ok((width, height)),
        _ => Err(error()),
    }
}

/// The hidden options of one run.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct HarnessOptions {
    /// `--profile-dir`: stands in for %APPDATA%.
    pub profile_dir: Option<PathBuf>,
    /// `--screenshot`: where the PNG goes.
    pub screenshot: Option<PathBuf>,
    /// `--size`.
    pub size: Option<(f32, f32)>,
    /// `--scale`.
    pub scale: Option<f32>,
    /// `--theme`.
    pub theme: Option<Theme>,
    /// `--scenario`, all occurrences in order.
    pub scenario: Vec<Step>,
    /// `--watchdog`.
    pub watchdog_seconds: Option<u64>,
    /// `--frame-log`.
    pub frame_log: bool,
    /// `--animate`.
    pub animate: bool,
    /// `--video-adapter`.
    pub video_adapter: Option<instagram_core::video_rules::VideoCaps>,
}

impl HarnessOptions {
    /// True when the run is driven by the harness: a screenshot is wanted or a scenario given.
    pub fn is_active(&self) -> bool {
        self.screenshot.is_some() || !self.scenario.is_empty()
    }

    /// The window size of this run, if the harness decides it.
    pub fn window_size(&self) -> Option<(f32, f32)> {
        self.size.or(self.screenshot.as_ref().map(|_| DEFAULT_SCREENSHOT_SIZE))
    }
}

/// Saves the framebuffer as a PNG and makes sure it is not blank: returns a short description
/// ("1440x900 distinct_colors=312") or the reason of the failure.
fn save_screenshot(path: &Path, image: &egui::ColorImage) -> Result<String, String> {
    let [width, height] = image.size;
    if let Some(folder) = path.parent().filter(|folder| !folder.as_os_str().is_empty()) {
        std::fs::create_dir_all(folder).map_err(|error| format!("the folder could not be created: {error}"))?;
    }
    // A window framebuffer is opaque, so the premultiplied pixels are plain RGBA.
    let mut bytes = Vec::with_capacity(width * height * 4);
    let mut distinct = std::collections::HashSet::new();
    for pixel in &image.pixels {
        bytes.extend_from_slice(&[pixel.r(), pixel.g(), pixel.b(), 255]);
        if distinct.len() < 4096 {
            distinct.insert([pixel.r(), pixel.g(), pixel.b()]);
        }
    }
    image::save_buffer(path, &bytes, width as u32, height as u32, image::ExtendedColorType::Rgba8).map_err(|error| error.to_string())?;
    if distinct.len() < 4 {
        return Err(format!("the image looks blank ({} colours)", distinct.len()));
    }
    Ok(format!("{width}x{height} distinct_colors={}{}", distinct.len(), if distinct.len() >= 4096 { "+" } else { "" }))
}

/// Drives one harness run: applies the scenario step by step and takes the screenshot.
pub struct Driver {
    options: HarnessOptions,
    steps: VecDeque<Step>,
    /// Pointer and key events still to be fed to egui, one batch per frame.
    event_frames: VecDeque<Vec<Event>>,
    /// Frames to wait before the next step.
    settle_frames: u32,
    /// The app was idle and nothing is left to do at the last logic tick.
    ready: bool,
    /// Painted frames since `ready` became true.
    idle_ui_frames: u32,
    resize_attempts: u32,
    screenshot_receiver: Option<mpsc::Receiver<Arc<egui::ColorImage>>>,
    /// The running scroll benchmark: pixels per frame and frames left.
    sweep: Option<(f32, u32)>,
    /// A `wait` step is over at this moment.
    wait_until: Option<Instant>,
    /// A `wait-file` step is over once this file exists.
    wait_file: Option<PathBuf>,
    /// egui's input options have been adjusted for scripted input.
    input_prepared: bool,
    /// A frame was painted since the driver last went on with the scenario.
    painted_since_logic: bool,
    /// A frame was painted since the last batch of events was fed in.
    painted_since_inject: bool,
    /// Where the scripted pointer is.
    pointer: Option<Pos2>,
    /// `block-save`: descriptions.json, held open without sharing so that no save can read or
    /// replace it.
    blocked_file: Option<std::fs::File>,
    /// Since when the run has been waiting for the app to become idle.
    busy_since: Option<Instant>,
}

impl Driver {
    /// A driver for these options.
    pub fn new(options: HarnessOptions) -> Driver {
        if options.is_active() {
            widgets::enable_label_registry();
        }
        let steps = options.scenario.iter().cloned().collect();
        Driver {
            options,
            steps,
            event_frames: VecDeque::new(),
            settle_frames: 0,
            ready: false,
            idle_ui_frames: 0,
            resize_attempts: 0,
            screenshot_receiver: None,
            sweep: None,
            wait_until: None,
            wait_file: None,
            input_prepared: false,
            painted_since_logic: false,
            painted_since_inject: true,
            pointer: None,
            blocked_file: None,
            busy_since: None,
        }
    }

    /// Opens descriptions.json of the profile without sharing it, creating it when it is not
    /// there. While the handle lives, the app can neither read nor replace the file: what a
    /// virus scanner or a second program holding the file does to a save.
    fn block_descriptions(&mut self, app: &App) {
        if self.options.profile_dir.is_none() {
            self.fail("block-save needs --profile-dir: it must never lock the descriptions of the real profile.");
        }
        let path = &app.services.paths.descriptions_file;
        if let Some(folder) = path.parent() {
            if let Err(error) = std::fs::create_dir_all(folder) {
                self.fail(&format!("the data folder could not be created: {error}"));
            }
        }
        let mut options = std::fs::OpenOptions::new();
        options.read(true).write(true).create(true).truncate(false);
        #[cfg(windows)]
        {
            use std::os::windows::fs::OpenOptionsExt as _;
            options.share_mode(0);
        }
        match options.open(path) {
            Ok(file) => self.blocked_file = Some(file),
            Err(error) => self.fail(&format!("{} could not be locked: {error}", path.display())),
        }
    }

    /// True when a screenshot is being produced (the window is hidden).
    pub fn takes_screenshot(&self) -> bool {
        self.options.screenshot.is_some()
    }

    /// True when frame timings go to the log.
    pub fn logs_frames(&self) -> bool {
        self.options.frame_log || self.options.scenario.iter().any(|step| matches!(step, Step::Sweep { .. }))
    }

    /// Ends the run because a scenario cannot be carried out.
    fn fail(&self, message: &str) -> ! {
        eprintln!("SCENARIO FAILED: {message}");
        log::error!("Scenario failed: {message}");
        std::process::exit(EXIT_USAGE);
    }

    /// The key of post `index`, or the end of the run when there is no such post.
    fn post_key(&self, app: &App, index: usize) -> PostKey {
        match app.state.posts().get(index) {
            Some(post) => post.key.clone(),
            None => self.fail(&format!("there is no post {index}: {} posts are open.", app.state.posts().len())),
        }
    }

    /// Where a pointer step points on screen.
    fn resolve(&self, ctx: &egui::Context, target: &Target) -> Pos2 {
        match target {
            Target::Position(position) => *position,
            Target::Named(name, number) => match widgets::labelled_rects(ctx, name).get(*number) {
                Some(rect) => rect.center(),
                None => {
                    let known: Vec<String> = widgets::labelled_names(ctx).into_iter().map(|known| shorten(&known)).collect();
                    self.fail(&format!("no widget named \"{name}\" (number {}) is on screen. On screen: {}.", number + 1, known.join(" | ")))
                }
            },
        }
    }

    /// Queues the events of `clicks` clicks at `position`: move, then press and release, each
    /// in a frame of its own, as a person's click arrives (egui does not see a click whose
    /// press shares a frame with the release before it).
    fn queue_clicks(&mut self, position: Pos2, clicks: usize) {
        let button = |pressed: bool| Event::PointerButton { pos: position, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
        self.event_frames.push_back(vec![Event::PointerMoved(position)]);
        for _ in 0..clicks {
            self.event_frames.push_back(vec![button(true)]);
            self.event_frames.push_back(vec![button(false)]);
        }
    }

    /// Carries out one step.
    fn run(&mut self, step: Step, app: &mut App, ctx: &egui::Context) {
        match step {
            Step::Feed => app.dispatch(Action::ShowHome),
            Step::Explore => app.dispatch(Action::ShowExplore),
            Step::Scroll(offset) => match app.state.view {
                state::View::Home => app.feed.jump_to(offset),
                state::View::Explore => app.explore.jump_to(offset),
            },
            Step::Post(index) => {
                let key = self.post_key(app, index);
                app.state.scroll_request = Some(ScrollRequest::Post(key));
            }
            Step::Viewer(index) => {
                let key = self.post_key(app, index);
                app.dispatch(Action::OpenViewer(key));
            }
            Step::Zoom(scale) => {
                if app.state.viewer.is_none() {
                    self.fail("zoom needs the viewer: open it first, e.g. viewer:0.");
                }
                app.viewer.request_zoom(scale);
            }
            Step::Sheet(index) => {
                let key = self.post_key(app, index);
                app.dispatch(Action::OpenPostOptions(key));
            }
            Step::Edit(index) => {
                let key = self.post_key(app, index);
                app.dispatch(Action::OpenEditor(key));
            }
            Step::EditText(text) => {
                if app.state.editor.is_none() {
                    self.fail("edit-text needs the editor: open it first, e.g. edit:0.");
                }
                app.dispatch(Action::EditorInput(text));
            }
            Step::EditSave => {
                if app.state.editor.is_none() {
                    self.fail("edit-save needs the editor: open it first, e.g. edit:0.");
                }
                app.dispatch(Action::SaveDescription);
            }
            Step::BlockSave => self.block_descriptions(app),
            Step::MoreMenu => {
                if !app.state.more_menu_open {
                    app.dispatch(Action::ToggleMoreMenu);
                }
            }
            Step::About => app.dispatch(Action::ShowAbout),
            Step::BrowserView => app.dispatch(Action::ShowBrowserView),
            Step::WaitFile(path) => {
                log::info!("The scenario waits for {}.", path.display());
                self.wait_file = Some(path);
            }
            Step::ClosePictures => app.dispatch(Action::ClosePictures),
            Step::AutoViewSetup => app.dispatch(Action::StartAutoView(None)),
            Step::AutoViewText(text) => {
                if app.state.auto_view_setup.is_none() {
                    self.fail("autoview-text needs the setup dialog: open it first with autoview-setup.");
                }
                app.dispatch(Action::AutoViewInput(text));
            }
            Step::AutoViewStart => {
                if app.state.auto_view_setup.is_none() {
                    self.fail("autoview-start needs the setup dialog: open it first with autoview-setup.");
                }
                app.dispatch(Action::AutoViewBegin);
            }
            Step::AutoView(index) => {
                // The key is only looked up to make sure the post exists.
                let _ = self.post_key(app, index);
                // The clock of the show is held, so that the screenshot shows a chosen moment.
                app.state.auto_view_clock = Some(0.0);
                let settings = app.state.remembered.auto_view;
                state::start_auto_view(&mut app.state, &mut app.services, settings, index);
                app.state.auto_view_clock = Some(AUTO_VIEW_SHOT_SECONDS);
                app.touch();
            }
            Step::AutoViewTime(seconds) => {
                if app.state.auto_view.is_none() || app.state.auto_view_clock.is_none() {
                    self.fail("autoview-time needs a show started with autoview:<index>.");
                }
                app.state.auto_view_clock = Some(seconds);
                app.touch();
            }
            Step::AutoViewPaused => match &app.state.auto_view {
                Some(run) if !run.player.paused => app.dispatch(Action::AutoViewCommand(state::PlayerCommand::TogglePause)),
                Some(_) => {}
                None => self.fail("autoview-paused needs a running auto view: start it first, e.g. autoview:0."),
            },
            Step::AutoViewEnd => {
                let Some(run) = &mut app.state.auto_view else {
                    self.fail("autoview-end needs a running auto view: start it first, e.g. autoview:0.");
                };
                // The last post with the loop off, and its time over.
                run.settings.loop_posts = false;
                run.player.cursor.position = run.player.cursor.order.len().saturating_sub(1);
                app.dispatch(Action::AutoViewCommand(state::PlayerCommand::Next));
            }
            Step::ExportProgress => {
                let total = app.state.posts().len();
                if total == 0 {
                    self.fail("export-progress needs pictures: pass a folder to open.");
                }
                let progress = instagram_core::export::ExportProgress { done: total.div_ceil(2), total };
                app.state.download = Some(state::DownloadState { kind: state::DownloadKind::All, phase: state::DownloadPhase::Running { stale: Vec::new() }, progress: Some(progress) });
                app.touch();
            }
            Step::DownloadAll(file) => {
                app.dispatch(Action::DownloadAll);
                if !app.services.downloader.answer_dialog(Some(file)) {
                    self.fail("the download could not be started: nothing is open, or another download is running.");
                }
            }
            Step::DownloadPost(index, file) => {
                let key = self.post_key(app, index);
                app.dispatch(Action::DownloadPost(key));
                if !app.services.downloader.answer_dialog(Some(file)) {
                    self.fail("the download could not be started: another download is running.");
                }
            }
            Step::Warnings | Step::WarningsAll => {
                if app.state.warnings_opened_at.is_none() {
                    app.dispatch(Action::ShowWarnings);
                }
                if app.state.warnings_opened_at.is_none() {
                    self.fail("there are no warnings to show.");
                }
                if step == Step::WarningsAll && !app.state.warnings_all {
                    app.dispatch(Action::ToggleAllWarnings);
                }
            }
            Step::DropOverlay => app.forced_drop_hover = true,
            Step::Busy => app.state.opening_since = Some(app.state.time - state::BUSY_CHIP_DELAY_SECONDS),
            Step::Toast(text) => app.dispatch(Action::Toast(text, instagram_core::messages::TOAST_MS)),
            Step::Like(index) => {
                let key = self.post_key(app, index);
                app.dispatch(Action::ToggleLike(key));
            }
            Step::Save(index) => {
                let key = self.post_key(app, index);
                app.dispatch(Action::ToggleSave(key));
            }
            Step::Expand(index) => {
                let key = self.post_key(app, index);
                app.dispatch(Action::ExpandCaption(key));
            }
            Step::Slide(index, slide) => {
                let key = self.post_key(app, index);
                app.dispatch(Action::ShowSlide(key.clone(), slide.round() as usize));
                if slide.fract() != 0.0 {
                    // Between two slides: hold the track there, as if the slide change had
                    // stopped halfway.
                    let post = app.state.post_ui.entry(key).or_default();
                    post.slide_motion = Some(state::SlideMotion { from: slide, started_at: f64::INFINITY });
                }
            }
            Step::Burst(index) => {
                let key = self.post_key(app, index);
                app.state.burst_freeze = Some(BURST_FREEZE_FRACTION);
                app.dispatch(Action::LikeByDoubleClick(key));
            }
            Step::Hover(target) => {
                let position = self.resolve(ctx, &target);
                self.pointer = Some(position);
                self.event_frames.push_back(vec![Event::PointerMoved(position)]);
            }
            Step::Click(target) => {
                let position = self.resolve(ctx, &target);
                self.pointer = Some(position);
                self.queue_clicks(position, 1);
            }
            Step::DoubleClick(target) => {
                let position = self.resolve(ctx, &target);
                self.pointer = Some(position);
                self.queue_clicks(position, 2);
            }
            Step::Wheel(notches) => {
                if self.pointer.is_none() {
                    self.fail("wheel needs the pointer somewhere: hover over a widget first, e.g. hover:@400x300.");
                }
                let wheel = Event::MouseWheel { unit: egui::MouseWheelUnit::Line, delta: vec2(0.0, notches), phase: egui::TouchPhase::Move, source: egui::MouseWheelSource::Unknown, modifiers: Modifiers::NONE };
                self.event_frames.push_back(vec![wheel]);
                // egui spreads a turn of the wheel over several frames.
                for _ in 0..WHEEL_SETTLE_FRAMES {
                    self.event_frames.push_back(Vec::new());
                }
            }
            Step::Drag(delta) => {
                let Some(start) = self.pointer else {
                    self.fail("drag needs the pointer somewhere: hover over a widget first, e.g. hover:@400x300.");
                };
                let button = |pos: Pos2, pressed: bool| Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE };
                self.event_frames.push_back(vec![button(start, true)]);
                for step in 1..=DRAG_STEPS {
                    self.event_frames.push_back(vec![Event::PointerMoved(start + delta * (step as f32 / DRAG_STEPS as f32))]);
                }
                let end = start + delta;
                self.event_frames.push_back(vec![button(end, false)]);
                self.pointer = Some(end);
            }
            Step::Key(key, modifiers) => {
                let event = |pressed: bool| Event::Key { key, physical_key: None, pressed, repeat: false, modifiers };
                self.event_frames.push_back(vec![event(true)]);
                self.event_frames.push_back(vec![event(false)]);
            }
            Step::Type(text) => {
                // One character per frame, as from a keyboard; a line break is the Enter key.
                for character in text.chars() {
                    if character == '\n' {
                        self.event_frames.push_back(vec![Event::Key { key: Key::Enter, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE }]);
                        self.event_frames.push_back(vec![Event::Key { key: Key::Enter, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE }]);
                    } else {
                        self.event_frames.push_back(vec![Event::Text(character.to_string())]);
                    }
                }
            }
            Step::Paste(text) => self.event_frames.push_back(vec![Event::Paste(text)]),
            Step::Sweep { pixels_per_frame, frames } => {
                app.frame_stats.reset();
                self.sweep = Some((pixels_per_frame, frames));
            }
            Step::Wait(milliseconds) => self.wait_until = Some(Instant::now() + Duration::from_millis(milliseconds)),
        }
    }

    /// Forgets the frames painted so far: the window is changing its scale or size, and what
    /// was painted before (and where its widgets were) says nothing about the final layout.
    fn restart_settling(&mut self) {
        self.painted_since_logic = false;
        self.settle_frames = SETTLE_FRAMES_AFTER_STEP;
        self.idle_ui_frames = 0;
    }

    /// Call at the end of every `App::logic`: keeps the hidden window painting, brings the
    /// window to the wanted size, runs the next step once the app is idle, and saves the
    /// screenshot when it arrived.
    pub fn tick_logic(&mut self, app: &mut App, ctx: &egui::Context) {
        self.ready = false;
        if self.takes_screenshot() && !self.input_prepared {
            // A hidden window is painted about ten times a second, so the two clicks of a
            // scripted double click are further apart than a person's.
            ctx.options_mut(|options| options.input_options.max_double_click_delay = SCRIPTED_DOUBLE_CLICK_SECONDS);
            self.input_prepared = true;
        }
        if self.takes_screenshot() {
            // eframe shows the window again after every painted frame; a pending paint request
            // holds for one frame only.
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::RequestPaintWhileHidden);
        }
        // Frames are asked for while there is something left to do. A scenario without a
        // screenshot leaves the app alone once its last step has run, so that the idle app can
        // be measured from outside in the state the scenario put it into.
        let work_left = !self.steps.is_empty() || !self.event_frames.is_empty() || self.sweep.is_some() || self.wait_until.is_some() || self.wait_file.is_some() || self.settle_frames > 0;
        if self.takes_screenshot() {
            ctx.request_repaint();
        } else if work_left {
            // A visible window without a display behind it (a disconnected session) is painted
            // as fast as the processor allows, which leaves the decoders nothing: the scenario
            // only asks for a frame every now and then.
            ctx.request_repaint_after(VISIBLE_RUN_FRAME_INTERVAL);
        }

        if let Some(receiver) = &self.screenshot_receiver {
            if let Ok(image) = receiver.try_recv() {
                let path = self.options.screenshot.clone().unwrap_or_default();
                if self.logs_frames() {
                    app.frame_stats.log("run");
                    log::info!("Pictures: {:?}; {} cards painted in the last frame.", app.services.loader.stats(), app.feed.painted_cards());
                }
                log::logger().flush();
                // Never wait for the window and the GPU device to be torn down.
                match save_screenshot(&path, &image) {
                    Ok(summary) => {
                        eprintln!("SCREENSHOT OK {} {summary} ppp={}", path.display(), ctx.pixels_per_point());
                        std::process::exit(0);
                    }
                    Err(error) => {
                        eprintln!("SCREENSHOT FAILED {}: {error}", path.display());
                        std::process::exit(EXIT_SCREENSHOT_FAILED);
                    }
                }
            }
            return;
        }

        // The scale first, then the size in points of that scale.
        if let Some(scale) = self.options.scale {
            if (ctx.pixels_per_point() - scale).abs() > 0.001 {
                ctx.set_pixels_per_point(scale);
                self.restart_settling();
                return;
            }
        }
        if let (Some((width, height)), true) = (self.options.window_size(), self.takes_screenshot()) {
            let size = ctx.content_rect().size();
            if ((size.x - width).abs() > 0.5 || (size.y - height).abs() > 0.5) && self.resize_attempts < MAX_RESIZE_ATTEMPTS {
                self.resize_attempts += 1;
                ctx.send_viewport_cmd(egui::ViewportCommand::InnerSize(vec2(width, height)));
                if self.resize_attempts == MAX_RESIZE_ATTEMPTS {
                    eprintln!("WARNING: the window is {}x{} points instead of {width}x{height}; the screen may be too small.", size.x, size.y);
                }
                self.restart_settling();
                return;
            }
        }

        // eframe also runs the logic without painting (a hidden window is painted at most ten
        // times a second). Everything below counts painted frames, so it only goes on after one.
        if !std::mem::take(&mut self.painted_since_logic) {
            return;
        }

        if let Some((pixels_per_frame, frames_left)) = self.sweep {
            // The benchmark scrolls every frame, whether the pictures have arrived or not.
            app.feed.jump_by(pixels_per_frame);
            self.sweep = if frames_left > 1 { Some((pixels_per_frame, frames_left - 1)) } else { None };
            if self.sweep.is_none() {
                app.frame_stats.log("sweep");
                app.frame_stats.reset();
                self.settle_frames = SETTLE_FRAMES_AFTER_STEP;
            }
            return;
        }

        if let Some(until) = self.wait_until {
            if Instant::now() < until {
                return;
            }
            self.wait_until = None;
        }
        if let Some(path) = &self.wait_file {
            if !path.exists() {
                return;
            }
            log::info!("The scenario goes on: {} exists.", path.display());
            self.wait_file = None;
        }
        if !self.event_frames.is_empty() {
            return;
        }
        let busy = app.busy_reasons();
        if !busy.is_empty() {
            // A run that waits long says in the log what for.
            let waiting_since = *self.busy_since.get_or_insert_with(Instant::now);
            if waiting_since.elapsed() >= BUSY_REPORT_INTERVAL {
                self.busy_since = Some(Instant::now());
                log::info!("The scenario is waiting: {}; {:?}; {}.", busy.join(", "), app.services.loader.stats(), app.services.loader.pending_summary());
            }
            return;
        }
        self.busy_since = None;
        if self.settle_frames > 0 {
            self.settle_frames -= 1;
            return;
        }
        if let Some(step) = self.steps.pop_front() {
            self.run(step, app, ctx);
            self.settle_frames = SETTLE_FRAMES_AFTER_STEP;
            return;
        }
        self.ready = true;
    }

    /// Call at the start of every `App::ui` (one painted frame): asks for the screenshot once
    /// the app has been idle for a few painted frames.
    pub fn tick_ui(&mut self, ctx: &egui::Context) {
        self.painted_since_logic = true;
        self.painted_since_inject = true;
        if !self.takes_screenshot() || self.screenshot_receiver.is_some() {
            return;
        }
        self.idle_ui_frames = if self.ready { self.idle_ui_frames + 1 } else { 0 };
        if self.idle_ui_frames >= IDLE_FRAMES_BEFORE_SHOT {
            let (sender, receiver) = mpsc::channel();
            let repaint = ctx.clone();
            ctx.request_screenshot(move |image: Arc<egui::ColorImage>| {
                let _ = sender.send(image);
                // The callback does not schedule a frame by itself.
                repaint.request_repaint();
            });
            self.screenshot_receiver = Some(receiver);
        }
    }

    /// Call from `App::raw_input_hook`: the hidden window is not the foreground window, so it is
    /// told it has the focus, and the queued pointer and key events of the scenario are fed in.
    pub fn inject(&mut self, raw_input: &mut egui::RawInput) {
        if self.takes_screenshot() {
            raw_input.focused = true;
            if let Some(viewport) = raw_input.viewports.get_mut(&raw_input.viewport_id) {
                viewport.focused = Some(true);
            }
        }
        // One batch per painted frame. Input given to a pass that paints nothing is kept by
        // eframe and delivered together with the next batch; a press and its release in one
        // frame are no click for egui.
        if std::mem::take(&mut self.painted_since_inject) {
            match self.event_frames.pop_front() {
                Some(events) => raw_input.events.extend(events),
                None => self.painted_since_inject = true,
            }
        }
    }
}

/// Starts the watchdog of a harness run: the process ends with code 3 when the screenshot has
/// not been written in time, or when a scripted run (`--watchdog` without `--screenshot`) has
/// not ended by itself, so that an automated run can never hang.
pub fn start_watchdog(seconds: u64) {
    let spawned = std::thread::Builder::new().name("watchdog".to_owned()).spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(seconds));
        eprintln!("WATCHDOG: the run did not end within {seconds} s, exiting with code {EXIT_WATCHDOG}.");
        log::error!("The watchdog ended the run after {seconds} s.");
        log::logger().flush();
        std::process::exit(EXIT_WATCHDOG);
    });
    if let Err(error) = spawned {
        eprintln!("The watchdog thread could not be started: {error}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn steps_are_parsed_with_their_arguments() {
        assert_eq!(parse_step("feed"), Ok(Step::Feed));
        assert_eq!(parse_step("scroll:1200"), Ok(Step::Scroll(1200.0)));
        assert_eq!(parse_step("post:4"), Ok(Step::Post(4)));
        assert_eq!(parse_step("slide:4/1"), Ok(Step::Slide(4, 1.0)));
        assert_eq!(parse_step("slide:4/0.5"), Ok(Step::Slide(4, 0.5)));
        assert_eq!(parse_step("toast:Link copied."), Ok(Step::Toast("Link copied.".to_owned())));
        assert_eq!(parse_step("hover:Home"), Ok(Step::Hover(Target::Named("Home".to_owned(), 0))));
        assert_eq!(parse_step("click:Like#2"), Ok(Step::Click(Target::Named("Like".to_owned(), 1))));
        assert_eq!(parse_step("hover:@120x340.5"), Ok(Step::Hover(Target::Position(pos2(120.0, 340.5)))));
        assert_eq!(parse_step("key:PageDown"), Ok(Step::Key(Key::PageDown, Modifiers::NONE)));
        assert_eq!(parse_step("key:Ctrl+Enter"), Ok(Step::Key(Key::Enter, CTRL_KEY)));
        assert_eq!(parse_step("key:Ctrl+Shift+End"), Ok(Step::Key(Key::End, CTRL_KEY | Modifiers::SHIFT)));
        assert_eq!(parse_step("key:Shift+Tab"), Ok(Step::Key(Key::Tab, Modifiers::SHIFT)));
        assert_eq!(parse_step("key:0"), Ok(Step::Key(Key::Num0, Modifiers::NONE)));
        assert_eq!(parse_step("sweep:400x300"), Ok(Step::Sweep { pixels_per_frame: 400.0, frames: 300 }));
        // A name that itself contains "#" followed by text is one name.
        assert_eq!(parse_step("click:#sourdough"), Ok(Step::Click(Target::Named("#sourdough".to_owned(), 0))));
    }

    #[test]
    fn the_steps_of_the_viewer_and_the_editor_are_parsed() {
        assert_eq!(parse_step("explore"), Ok(Step::Explore));
        assert_eq!(parse_step("viewer:2"), Ok(Step::Viewer(2)));
        assert_eq!(parse_step("zoom:2.5"), Ok(Step::Zoom(Some(2.5))));
        assert_eq!(parse_step("zoom:fit"), Ok(Step::Zoom(None)));
        assert_eq!(parse_step("sheet:0"), Ok(Step::Sheet(0)));
        assert_eq!(parse_step("edit:3"), Ok(Step::Edit(3)));
        // "\n" written out is a line break; a colon in the text belongs to the text.
        assert_eq!(parse_step("edit-text:Croissants 🥐\\nBaked at dawn."), Ok(Step::EditText("Croissants 🥐\nBaked at dawn.".to_owned())));
        assert_eq!(parse_step("edit-text:Open: 9 to 5"), Ok(Step::EditText("Open: 9 to 5".to_owned())));
        // An empty text clears the field.
        assert_eq!(parse_step("edit-text:"), Ok(Step::EditText(String::new())));
        assert_eq!(parse_step("edit-save"), Ok(Step::EditSave));
        assert_eq!(parse_step("block-save"), Ok(Step::BlockSave));
        assert_eq!(parse_step("wheel:-2"), Ok(Step::Wheel(-2.0)));
        assert_eq!(parse_step("drag:-120x40"), Ok(Step::Drag(vec2(-120.0, 40.0))));
        assert_eq!(parse_step("type:a\\nb"), Ok(Step::Type("a\nb".to_owned())));
        assert_eq!(parse_step("paste:two words"), Ok(Step::Paste("two words".to_owned())));
        assert_eq!(parse_step("paste:one\\r\\ntwo"), Ok(Step::Paste("one\r\ntwo".to_owned())));
        for wrong in ["zoom", "zoom:0", "zoom:big", "viewer", "wheel:0", "drag:10", "type:", "edit-save:1", "key:Ctrl+"] {
            assert!(parse_step(wrong).is_err(), "{wrong} must not parse");
        }
    }

    #[test]
    fn an_unknown_step_fails_loudly_with_the_list_of_valid_ones() {
        let error = parse_step("teleport:2").expect_err("there is no such step");
        assert!(error.message.starts_with("Unknown scenario step \"teleport:2\""));
        for (name, _) in SCENARIO_STEPS {
            assert!(error.message.contains(name), "{name} is missing from the list");
        }
        assert!(parse_step("scroll").is_err());
        assert!(parse_step("scroll:abc").is_err());
        assert!(parse_step("feed:1").is_err());
        assert!(parse_step("slide:4").is_err());
        assert!(parse_step("key:F13").is_err());
        assert!(parse_step("hover:").is_err());
        assert!(parse_step("sweep:10x0").is_err());
    }

    #[test]
    fn every_step_of_the_table_has_a_parser() {
        for (name, _) in SCENARIO_STEPS {
            let example = name
                .replace("<dx>x<dy>", "10x10")
                .replace("<px>", "100")
                .replace("<factor>", "2")
                .replace("<notches>", "1")
                .replace("<index>", "0")
                .replace("<n>", "1")
                .replace("<text>", "Hello")
                .replace("<name>[#k]", "Home")
                .replace("<name>", "Home")
                .replace("<frames>", "10")
                .replace("<seconds>", "7.5")
                .replace("<file>", "C:/out/instagram-export.zip")
                .replace("<ms>", "250");
            assert!(parse_step(&example).is_ok(), "{example} does not parse");
        }
    }

    #[test]
    fn a_scenario_is_a_comma_separated_list() {
        assert_eq!(parse_scenario("feed, post:4 ,more-menu"), Ok(vec![Step::Feed, Step::Post(4), Step::MoreMenu]));
        assert_eq!(parse_scenario(""), Ok(Vec::new()));
        assert!(parse_scenario("feed,nonsense").is_err());
    }

    #[test]
    fn sizes_are_width_x_height() {
        assert_eq!(parse_size("1440x900"), Ok((1440.0, 900.0)));
        assert!(parse_size("1440").is_err());
        assert!(parse_size("0x0").is_err());
        assert!(parse_size("axb").is_err());
    }

    #[test]
    fn the_harness_decides_the_window_size_only_when_asked() {
        let mut options = HarnessOptions::default();
        assert!(!options.is_active());
        assert_eq!(options.window_size(), None);
        options.screenshot = Some(PathBuf::from("shot.png"));
        assert!(options.is_active());
        assert_eq!(options.window_size(), Some(DEFAULT_SCREENSHOT_SIZE));
        options.size = Some((1100.0, 800.0));
        assert_eq!(options.window_size(), Some((1100.0, 800.0)));
    }
}
