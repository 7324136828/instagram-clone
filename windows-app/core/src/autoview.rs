//! Auto view: a slideshow that shows the posts one after another. This module is the pure part
//! (settings, the command-line options, timing rules, navigation state and the pausable timer);
//! the dialogs and the player are drawn by the app.
//!
//! It is a port of the reference web project's frontend/src/autoView.ts (spec-viewer 4.2, 4.5,
//! 4.6, 4.8) with its video option "Let videos play to the end" (`plan_slide`, video brief 5.6).
//! Two things the browser supplies are injected instead, which keeps every function
//! deterministic and testable: the clock (a time in milliseconds is passed to the timer) and the
//! random numbers of the shuffle (a function returning values in `[0, 1)`, like `Math.random`).
//! The length and the end of the video on show come from the app's player (`VideoState`).
//!
//! The clone's unit tests (frontend/src/autoView.test.ts) are ported at the bottom of this file;
//! the ones about localStorage and the rendered dialogs have nothing to test here.

use std::collections::HashSet;
use std::ffi::OsString;
use std::hash::Hash;

use serde_json::{Map, Value};

use crate::messages;
use crate::text::{js_round, js_trim, js_trim_end};

/// Seconds a post stays on screen when nothing else is remembered.
pub const DEFAULT_INTERVAL_SEC: u32 = 15;
/// The shortest time per post, in seconds.
pub const MIN_INTERVAL_SEC: u32 = 2;
/// The longest time per post, in seconds (one hour).
pub const MAX_INTERVAL_SEC: u32 = 3600;
/// A carousel slide is never shown for less than this; the post gets more time instead.
pub const MIN_SLIDE_SEC: u32 = 2;
/// The values the "shorter / longer" controls step through.
pub const INTERVAL_STEPS: [u32; 18] = [2, 3, 5, 8, 10, 15, 20, 30, 45, 60, 90, 120, 180, 300, 600, 900, 1800, 3600];
/// The quick choices of the setup dialog, shown as "5 s", "10 s", "15 s", "30 s", "1 min".
pub const INTERVAL_PRESETS: [u32; 5] = [5, 10, 15, 30, 60];
/// The timer fires when no more than this much of the slide time is left: a clock that
/// disagrees with the timer by a hair must not make it fire late (or not at all).
pub const DUE_TOLERANCE_MS: f64 = 1.0;

/// The name of the interval in state.json.
const KEY_INTERVAL_SEC: &str = "interval_sec";
/// The name of the loop option in state.json.
const KEY_LOOP: &str = "loop";
/// The name of the shuffle option in state.json.
const KEY_SHUFFLE: &str = "shuffle";
/// The name of the video option in state.json.
const KEY_WAIT_FOR_VIDEOS: &str = "wait_for_videos";
/// The longest auto view waits for the end of a video that plays but has no known length (a
/// WebM recorded in a browser often has none until it ended): `UNKNOWN_LENGTH_MAX_SEC`.
pub const UNKNOWN_LENGTH_MAX_SEC: f64 = 600.0;

/// The auto view settings (the clone's `AutoViewSettings`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Seconds each post stays on screen (a carousel shares them between its slides),
    /// `MIN_INTERVAL_SEC`..=`MAX_INTERVAL_SEC`.
    pub interval_sec: u32,
    /// "Let videos play to the end": a video longer than its slide time stays until it ended.
    /// ("wait_for_videos" in state.json, `--wait` on the command line.)
    pub wait_for_videos: bool,
    /// Start again from the first post after the last one; otherwise stop on an end screen.
    /// ("loop" in state.json and on the command line.)
    pub loop_posts: bool,
    /// Show the posts in a random order, fixed when auto view starts.
    pub shuffle: bool,
}

impl Default for Settings {
    /// The clone's `DEFAULT_AUTO_VIEW`: 15 seconds, videos to the end, loop on, shuffle off.
    fn default() -> Settings {
        Settings { interval_sec: DEFAULT_INTERVAL_SEC, wait_for_videos: true, loop_posts: true, shuffle: false }
    }
}

/// The direction of the "shorter / longer" controls.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Step {
    /// The `-` key: less time per post.
    Shorter,
    /// The `+` key: more time per post.
    Longer,
}

/// The clone's `clampInterval`: whole seconds between `MIN_INTERVAL_SEC` and
/// `MAX_INTERVAL_SEC`, rounded half up; a value that is not finite gives `DEFAULT_INTERVAL_SEC`.
/// 14.6 -> 15, 0 -> 2, 99999 -> 3600, NaN -> 15.
pub fn clamp_interval(seconds: f64) -> u32 {
    if !seconds.is_finite() {
        return DEFAULT_INTERVAL_SEC;
    }
    // The value lies within the limits after the clamp, so the conversion loses nothing.
    js_round(seconds).clamp(f64::from(MIN_INTERVAL_SEC), f64::from(MAX_INTERVAL_SEC)) as u32
}

/// The clone's `stepInterval`: the next value of `INTERVAL_STEPS` above (`Longer`) or below
/// (`Shorter`) the clamped current interval; `MAX_INTERVAL_SEC` / `MIN_INTERVAL_SEC` when there
/// is none. 15 longer -> 20; 15 shorter -> 10; 7 longer -> 8; 7 shorter -> 5; 2000 longer -> 3600.
pub fn step_interval(current_sec: u32, step: Step) -> u32 {
    let value = clamp_interval(f64::from(current_sec));
    match step {
        Step::Longer => INTERVAL_STEPS.iter().copied().find(|&candidate| candidate > value).unwrap_or(MAX_INTERVAL_SEC),
        Step::Shorter => {
            INTERVAL_STEPS.iter().rev().copied().find(|&candidate| candidate < value).unwrap_or(MIN_INTERVAL_SEC)
        }
    }
}

/// The clone's `formatInterval`: "15 s", "1 min", "1 min 30 s", "1 h" (the value is clamped
/// first, so 1 -> "2 s").
pub fn format_interval(seconds: u32) -> String {
    let total = clamp_interval(f64::from(seconds));
    if total < 60 {
        return format!("{total} s");
    }
    if total >= 3600 {
        return "1 h".to_owned();
    }
    let minutes = total / 60;
    let rest = total % 60;
    if rest == 0 { format!("{minutes} min") } else { format!("{minutes} min {rest} s") }
}

/// The clone's `parseIntervalInput` for the text typed in the setup dialog: it must match
/// `^\s*(\d+)\s*$` (`\s` is `text::is_js_space`, ASCII digits) and lie in
/// `MIN_INTERVAL_SEC`..=`MAX_INTERVAL_SEC`; otherwise `None` and the dialog shows
/// `messages::AUTO_VIEW_INTERVAL_ERROR` (no silent correction). " 15 " -> 15; "", "abc", "1",
/// "3601", "2.5", "-5", "1e3", "15s" -> `None`.
pub fn parse_interval_input(text: &str) -> Option<u32> {
    let digits = js_trim(text);
    if !is_ascii_digits(digits) {
        return None;
    }
    // Leading zeros do not count ("015" is 15, as it is for JavaScript's Number). What is left
    // is in range only when it is short, and a short number cannot overflow the conversion.
    let significant = digits.trim_start_matches('0');
    if significant.len() > 4 {
        return None;
    }
    let seconds: u32 = significant.parse().ok()?;
    (MIN_INTERVAL_SEC..=MAX_INTERVAL_SEC).contains(&seconds).then_some(seconds)
}

/// `parse_interval_input` for the "Start" button of the setup dialog: the seconds, or the
/// sentence the dialog shows under the field (`messages::AUTO_VIEW_INTERVAL_ERROR`).
pub fn check_interval_input(text: &str) -> Result<u32, &'static str> {
    parse_interval_input(text).ok_or(messages::AUTO_VIEW_INTERVAL_ERROR)
}

/// True for a text of one or more ASCII digits (the `\d+` of the clone's patterns).
fn is_ascii_digits(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|byte| byte.is_ascii_digit())
}

/// The clone's `normalizeSettings` for a value read from state.json (member `auto_view`): every
/// field is checked on its own and a missing or broken one gets its default. `interval_sec`
/// must be a JSON number (then `clamp_interval`), `wait_for_videos`, `loop` and `shuffle` JSON
/// booleans. A value
/// that is not an object gives the defaults. `{"interval_sec": 1e9, "loop": "yes", "shuffle":
/// true}` -> `{3600, loop on, shuffle on}`; `{"interval_sec": "20"}` -> the defaults.
pub fn normalize_settings(value: &Value) -> Settings {
    let defaults = Settings::default();
    let Some(source) = value.as_object() else {
        return defaults;
    };
    let flag = |key: &str, fallback: bool| source.get(key).and_then(Value::as_bool).unwrap_or(fallback);
    Settings {
        interval_sec: match source.get(KEY_INTERVAL_SEC) {
            Some(Value::Number(number)) => number.as_f64().map_or(defaults.interval_sec, clamp_interval),
            _ => defaults.interval_sec,
        },
        wait_for_videos: flag(KEY_WAIT_FOR_VIDEOS, defaults.wait_for_videos),
        loop_posts: flag(KEY_LOOP, defaults.loop_posts),
        shuffle: flag(KEY_SHUFFLE, defaults.shuffle),
    }
}

/// The JSON object state.json keeps for the settings: `{"interval_sec": 15, "wait_for_videos":
/// true, "loop": true, "shuffle": false}`, in this key order (the clone's), the interval clamped.
pub fn settings_to_json(settings: &Settings) -> Value {
    let mut object = Map::new();
    object.insert(KEY_INTERVAL_SEC.to_owned(), Value::from(clamp_interval(f64::from(settings.interval_sec))));
    object.insert(KEY_WAIT_FOR_VIDEOS.to_owned(), Value::Bool(settings.wait_for_videos));
    object.insert(KEY_LOOP.to_owned(), Value::Bool(settings.loop_posts));
    object.insert(KEY_SHUFFLE.to_owned(), Value::Bool(settings.shuffle));
    Value::Object(object)
}

// --- Command line (replaces the clone's "#autoview" address hash; spec-viewer 4.8) -----------

/// An auto view request from the command line. Fields that were not given (or could not be
/// read) are `None`: the remembered settings apply to them. Nothing of a request is saved.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AutoViewRequest {
    /// Seconds per post from `--autoview=SECONDS`, already clamped.
    pub interval_sec: Option<u32>,
    /// `--wait=0|1` (also `--waitforvideos`).
    pub wait_for_videos: Option<bool>,
    /// `--loop=0|1`.
    pub loop_posts: Option<bool>,
    /// `--shuffle=0|1`.
    pub shuffle: Option<bool>,
}

impl AutoViewRequest {
    /// The settings of this run: the remembered ones with the given fields laid over them.
    pub fn apply_to(&self, remembered: Settings) -> Settings {
        Settings {
            interval_sec: self.interval_sec.unwrap_or(remembered.interval_sec),
            wait_for_videos: self.wait_for_videos.unwrap_or(remembered.wait_for_videos),
            loop_posts: self.loop_posts.unwrap_or(remembered.loop_posts),
            shuffle: self.shuffle.unwrap_or(remembered.shuffle),
        }
    }
}

/// The command line, split into the auto view request and everything else.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ParsedArguments {
    /// `Some` when `--autoview` was given: the player starts at the first post as soon as a
    /// collection is open, without the setup dialog.
    pub auto_view: Option<AutoViewRequest>,
    /// Every other argument in its original order: the paths to open, and options this module
    /// does not know (the app handles or rejects them).
    pub rest: Vec<OsString>,
}

/// The option that asks for auto view; its value is the seconds per post.
const OPTION_AUTO_VIEW: &str = "autoview";
const OPTION_LOOP: &str = "loop";
const OPTION_SHUFFLE: &str = "shuffle";
/// The video option and its long name (the clone's hash keys `wait` and `waitForVideos`).
const OPTION_WAIT: &str = "wait";
const OPTION_WAIT_FOR_VIDEOS: &str = "waitforvideos";
/// What every option starts with.
const OPTION_PREFIX: &str = "--";

/// Reads the auto view options from the arguments after the program name (design brief "Auto
/// view"; the rules of the clone's `parseAutoViewHash`, autoView.ts:117-139):
///
/// * `--autoview` or `--autoview=VALUE` asks for auto view. VALUE must match
///   `^\s*(\d+(?:\.\d+)?)\s*s?\s*$` (the "s" in any letter case) and then gives
///   `clamp_interval(value)`; an empty or unreadable value leaves the interval to the remembered
///   settings. `--autoview=15`, `--autoview=15s` -> 15; `--autoview=1` -> 2;
///   `--autoview=100000` -> 3600; `--autoview=-5`, `--autoview=abc` -> no interval.
/// * `--loop[=WORD]`, `--shuffle[=WORD]` and `--wait[=WORD]` (also `--waitforvideos[=WORD]`;
///   a readable `--wait` wins over it): true words "", "1", "true", "yes", "on" (so a bare
///   `--shuffle` is true); false words "0", "false", "no", "off"; trimmed, any letter case;
///   anything else is ignored.
/// * Option names are compared without regard to letter case; the first occurrence of an option
///   wins, also when its value cannot be read (a later one is consumed without effect, as in
///   the clone). `--loop` / `--shuffle` without `--autoview` are consumed and have no effect (as
///   "#loop=0" alone is no request in the clone).
/// * The value belongs to the argument (`--autoview=15`); a number given as the next argument
///   (`--autoview 15`) is not a value but an argument of its own.
/// * Every other argument, including ones that are not Unicode, goes to `rest` unchanged.
pub fn parse_arguments(arguments: &[OsString]) -> ParsedArguments {
    // The first value given for each option, as the clone keeps the first value of each key.
    let mut interval_value: Option<String> = None;
    let mut loop_value: Option<String> = None;
    let mut shuffle_value: Option<String> = None;
    let mut wait_value: Option<String> = None;
    let mut wait_for_videos_value: Option<String> = None;
    let mut rest = Vec::new();

    for argument in arguments {
        let option = argument.to_str().and_then(split_option);
        let slot = match option.as_ref().map(|(name, _)| name.as_str()) {
            Some(OPTION_AUTO_VIEW) => &mut interval_value,
            Some(OPTION_LOOP) => &mut loop_value,
            Some(OPTION_SHUFFLE) => &mut shuffle_value,
            Some(OPTION_WAIT) => &mut wait_value,
            Some(OPTION_WAIT_FOR_VIDEOS) => &mut wait_for_videos_value,
            _ => {
                rest.push(argument.clone());
                continue;
            }
        };
        if slot.is_none() {
            *slot = option.map(|(_, value)| value.to_owned());
        }
    }

    let auto_view = interval_value.map(|interval| AutoViewRequest {
        interval_sec: parse_interval_option(&interval),
        wait_for_videos: wait_value.as_deref().and_then(parse_flag).or_else(|| wait_for_videos_value.as_deref().and_then(parse_flag)),
        loop_posts: loop_value.as_deref().and_then(parse_flag),
        shuffle: shuffle_value.as_deref().and_then(parse_flag),
    });
    ParsedArguments { auto_view, rest }
}

/// Splits "--name=value" into the name (trimmed and lower-cased, as the clone treats the keys
/// of the hash) and the value ("" when there is no "="). `None` for anything that does not
/// start like an option.
fn split_option(argument: &str) -> Option<(String, &str)> {
    let body = argument.strip_prefix(OPTION_PREFIX)?;
    let (name, value) = body.split_once('=').unwrap_or((body, ""));
    Some((js_trim(name).to_lowercase(), value))
}

/// The seconds of `--autoview=VALUE`: `^\s*(\d+(?:\.\d+)?)\s*s?\s*$` with the "s" in either
/// letter case, clamped. `None` when the value does not have this form.
fn parse_interval_option(value: &str) -> Option<u32> {
    let text = js_trim(value);
    let number = match text.strip_suffix(|character: char| matches!(character, 's' | 'S')) {
        Some(before_unit) => js_trim_end(before_unit),
        None => text,
    };
    let (whole, fraction) = match number.split_once('.') {
        Some((whole, fraction)) => (whole, Some(fraction)),
        None => (number, None),
    };
    if !is_ascii_digits(whole) || !fraction.is_none_or(is_ascii_digits) {
        return None;
    }
    // A number too long for a float reads as infinity here and in JavaScript alike, and
    // `clamp_interval` then answers with the default, as the clone does.
    number.parse::<f64>().ok().map(clamp_interval)
}

/// The clone's `parseFlag`: the word of `--loop=WORD` / `--shuffle=WORD` as a switch, `None`
/// when it is neither a true nor a false word.
fn parse_flag(value: &str) -> Option<bool> {
    match js_trim(value).to_lowercase().as_str() {
        "" | "1" | "true" | "yes" | "on" => Some(true),
        "0" | "false" | "no" | "off" => Some(false),
        _ => None,
    }
}

// --- Timing -----------------------------------------------------------------------------------

/// The clone's `slideSeconds`: seconds per carousel slide, the (clamped) interval shared evenly
/// but never less than `MIN_SLIDE_SEC`: `max(2, interval / max(1, slide_count))`.
/// (15, 1) -> 15; (15, 3) -> 5; (15, 10) -> 2; (4, 3) -> 2; (15, 0) -> 15.
pub fn slide_seconds(interval_sec: u32, slide_count: usize) -> f64 {
    let slides = slide_count.max(1);
    // A slide count is far below 2^53, so it is exact as a float.
    f64::max(f64::from(MIN_SLIDE_SEC), f64::from(clamp_interval(f64::from(interval_sec))) / slides as f64)
}

/// The clone's `videoDeadlineSec`: how long past its end a video may take before auto view moves
/// on anyway (buffering, a stall, a decode error after the length was known):
/// `duration + max(5, duration / 4)`. 40 -> 50; 8 -> 13.
pub fn video_deadline_sec(duration_sec: f64) -> f64 {
    duration_sec + f64::max(5.0, duration_sec * 0.25)
}

/// What the player tells about the video on show (the clone's `VideoState`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct VideoState {
    /// Seconds once known (`video_rules::reportable_duration`); `None` until then, and when it
    /// never is or the video failed; infinity when the video plays but cannot tell its length.
    pub duration_sec: Option<f64>,
    /// The video played to its end.
    pub ended: bool,
}

impl VideoState {
    /// Nothing known yet (the clone's `NO_VIDEO`).
    pub const UNKNOWN: VideoState = VideoState { duration_sec: None, ended: false };
}

/// When the current slide is done.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SlidePlan {
    /// Active (not paused) milliseconds after which the slide is done.
    pub due_ms: f64,
    /// What the progress bar counts down: the time the slide is expected to take.
    pub expected_ms: f64,
    /// Auto view needs the video's end: the video then plays once instead of looping. False
    /// when it is shorter than the slide time: it loops until the time is up.
    pub listen_for_end: bool,
}

/// The clone's `planSlide`: a picture (or a video when `wait_for_videos` is off) gets its slide
/// time. With `wait_for_videos` a video stays until the later of the slide time and its end
/// (`video_deadline_sec` as a safety deadline); a video that plays without a known length stays
/// until it ends, up to `UNKNOWN_LENGTH_MAX_SEC`; without any length (not loaded yet, failed)
/// the slide time applies.
pub fn plan_slide(slide_sec: f64, is_video: bool, wait_for_videos: bool, video: VideoState) -> SlidePlan {
    let slide_ms = slide_sec * 1000.0;
    let plain = |listen_for_end: bool| SlidePlan { due_ms: slide_ms, expected_ms: slide_ms, listen_for_end };
    if !is_video || !wait_for_videos {
        return plain(false);
    }
    if !video.ended && video.duration_sec == Some(f64::INFINITY) {
        return SlidePlan { due_ms: slide_ms.max(UNKNOWN_LENGTH_MAX_SEC * 1000.0), expected_ms: slide_ms, listen_for_end: true };
    }
    let duration = video.duration_sec.filter(|seconds| seconds.is_finite() && *seconds > 0.0);
    match duration {
        _ if video.ended => plain(true),
        None => plain(true),
        Some(seconds) if seconds * 1000.0 < slide_ms => plain(false),
        Some(seconds) => SlidePlan { due_ms: video_deadline_sec(seconds) * 1000.0, expected_ms: seconds * 1000.0, listen_for_end: true },
    }
}

/// The clone's `postProgress`: the share of the post that has been shown (0..=1), finished
/// slides plus the part of the current one:
/// `n = max(1, slide_count); done = clamp(slide, 0, n - 1); part = expected_ms > 0 ?
/// clamp(elapsed_ms / expected_ms, 0, 1) : 1; min(1, (done + part) / n)`.
/// (0, 1, 5000, 10000) -> 0.5; (1, 4, 2500, 5000) -> 0.375; (9, 2, 0, 5000) -> 0.5;
/// (0, 1, 100, 0) -> 1.
pub fn post_progress(slide: usize, slide_count: usize, elapsed_ms: f64, expected_ms: f64) -> f64 {
    let slides = slide_count.max(1);
    let done = slide.min(slides - 1);
    // An elapsed time that is not a number counts as nothing shown instead of spoiling the
    // result (`clamp` alone would hand the NaN on).
    let part = match elapsed_ms / expected_ms {
        _ if expected_ms <= 0.0 || expected_ms.is_nan() => 1.0,
        share if share.is_nan() => 0.0,
        share => share.clamp(0.0, 1.0),
    };
    f64::min(1.0, (done as f64 + part) / slides as f64)
}

// --- Order and navigation ---------------------------------------------------------------------

/// The order of a run and the post on screen. `T` identifies a post (the app uses
/// `model::PostKey`; tests use short strings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Cursor<T> {
    /// Post ids in the order they are shown.
    pub order: Vec<T>,
    /// Index in `order` of the post on screen.
    pub position: usize,
}

/// The clone's `startCursor`: the order for a new auto view.
/// `start = clamp(start_index, 0, len - 1)` (0 for an empty list). Not shuffled, or fewer than
/// two posts: the ids in their order with `position = start` (the order is NOT rotated: it plays
/// from `start` to the end). Shuffled: the start post first, then the others after a
/// Fisher-Yates shuffle, `position = 0`:
///
/// ```text
/// rest = ids without index `start`
/// for i in (1..rest.len()).rev(): j = min(i, floor(random() * (i + 1))); rest.swap(i, j)
/// ```
///
/// `random` returns values in `[0, 1)`, like `Math.random` (`SeededRandom::next_f64` in the app).
pub fn start_cursor<T: Clone>(
    ids: &[T],
    start_index: usize,
    shuffle: bool,
    random: &mut dyn FnMut() -> f64,
) -> Cursor<T> {
    let start = start_index.min(ids.len().saturating_sub(1));
    if !shuffle || ids.len() < 2 {
        return Cursor { order: ids.to_vec(), position: start };
    }
    let mut order = Vec::with_capacity(ids.len());
    order.push(ids[start].clone());
    order.extend(ids.iter().enumerate().filter(|(index, _)| *index != start).map(|(_, id)| id.clone()));
    // The start post stays in front; only what follows it is shuffled.
    let rest = &mut order[1..];
    for i in (1..rest.len()).rev() {
        // The conversion of a float saturates: a random source that leaves [0, 1) or gives NaN
        // still picks a valid index.
        let j = ((random() * (i + 1) as f64).floor() as usize).min(i);
        rest.swap(i, j);
    }
    Cursor { order, position: 0 }
}

/// The clone's `reconcileCursor`: follows a change of the posts while auto view runs. Removed
/// posts leave the order, new ones are added at the end (in `ids` order), and the post on screen
/// stays on screen when it still exists; otherwise the position is
/// `max(0, min(position - removed_before, len - 1))` with `removed_before` the number of removed
/// ids before the old position (0 for an empty order). `None` when the order does not change
/// (the clone returns the same object then).
///
/// The ids are looked up through hash sets, as in the clone, so that the app may call this for
/// every frame even with thousands of posts in a shuffled order.
pub fn reconcile_cursor<T: Clone + Eq + Hash>(cursor: &Cursor<T>, ids: &[T]) -> Option<Cursor<T>> {
    // The usual case: nothing was added, removed or moved.
    if cursor.order.as_slice() == ids {
        return None;
    }
    let present: HashSet<&T> = ids.iter().collect();
    let kept: Vec<&T> = cursor.order.iter().filter(|id| present.contains(id)).collect();
    let known: HashSet<&T> = kept.iter().copied().collect();
    let order: Vec<T> =
        kept.iter().copied().chain(ids.iter().filter(|id| !known.contains(id))).cloned().collect();
    if order == cursor.order {
        return None;
    }
    let still_there =
        cursor.order.get(cursor.position).and_then(|current| order.iter().position(|id| id == current));
    let position = still_there.unwrap_or_else(|| {
        // The post on screen is gone: the one that took its place is shown.
        let removed_before = cursor.order.iter().take(cursor.position).filter(|id| !present.contains(id)).count();
        cursor.position.saturating_sub(removed_before).min(order.len().saturating_sub(1))
    });
    Some(Cursor { order, position })
}

/// The clone's `nextPosition`: the position after `position`, or `None` at the end when not
/// looping (and for an empty order).
pub fn next_position(position: usize, length: usize, loop_posts: bool) -> Option<usize> {
    if length == 0 {
        return None;
    }
    if position.saturating_add(1) < length {
        return Some(position + 1);
    }
    loop_posts.then_some(0)
}

/// The clone's `previousPosition`: the position before `position`, or `None` at the start when
/// not looping (and for an empty order).
pub fn previous_position(position: usize, length: usize, loop_posts: bool) -> Option<usize> {
    if length == 0 {
        return None;
    }
    if position > 0 {
        return Some(position - 1);
    }
    loop_posts.then_some(length - 1)
}

/// What the player shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlayerState<T> {
    /// The order of the run and the post on screen.
    pub cursor: Cursor<T>,
    /// Carousel slide on screen. The view clamps it to `min(slide, slide_count - 1)`.
    pub slide: usize,
    /// Counts post changes, so that showing the same post again (a loop of one) restarts it.
    pub visit: u64,
    /// The last post was shown and loop is off: the end screen is up.
    pub finished: bool,
    /// Paused by the user (a minimized window pauses too, but that is not part of this state).
    pub paused: bool,
}

/// What can happen to the player.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PlayerAction<T> {
    /// Next post (Right key, the button).
    Next {
        /// The loop setting in effect: after the last post, start again instead of ending.
        loop_posts: bool,
    },
    /// Previous post (Left key, the button).
    Previous {
        /// The loop setting in effect: before the first post, go to the last one.
        loop_posts: bool,
    },
    /// The time of the current slide is up: next slide, else next post.
    Advance {
        /// How many slides the post on screen has (1 for a single picture).
        slide_count: usize,
        /// The loop setting in effect.
        loop_posts: bool,
    },
    /// The carousel was paged by hand.
    Slide {
        /// The slide now on screen, 0-based.
        slide: usize,
    },
    /// Space, the play / pause button.
    TogglePause,
    /// "Restart" on the end screen.
    Restart,
    /// The posts changed underneath (pictures were added or the collection replaced).
    Sync {
        /// The ids of all posts now, in display order.
        ids: Vec<T>,
    },
}

impl<T: Clone + Eq + Hash> PlayerState<T> {
    /// The clone's `initPlayer`: slide 0, visit 0, not finished, not paused.
    pub fn new(cursor: Cursor<T>) -> PlayerState<T> {
        PlayerState { cursor, slide: 0, visit: 0, finished: false, paused: false }
    }

    /// The post on screen, or `None` for an empty order.
    pub fn current(&self) -> Option<&T> {
        self.cursor.order.get(self.cursor.position)
    }

    /// True while the time of the slide runs (AutoView.tsx:306-311): not paused by the user,
    /// the window not hidden (minimized), not on the end screen, and there is a post to show.
    /// A hidden window stops the time without touching `paused`, so the show goes on by itself
    /// when the window comes back and no "Paused" pill appears.
    pub fn is_active(&self, window_hidden: bool) -> bool {
        !self.paused && !window_hidden && !self.finished && self.current().is_some()
    }

    /// The clone's `playerReducer`, applied in place. Returns true when the state changed (the
    /// clone returns the same object when it did not). With `show(p)` = position `p`, slide 0,
    /// `visit + 1`, not finished, `paused` kept:
    ///
    /// ```text
    /// Next      finished -> unchanged; next_position None -> finished = true; Some(p) -> show(p)
    /// Previous  finished -> (empty order ? unchanged : show(len - 1));
    ///           previous_position None -> unchanged; Some(p) -> show(p)
    /// Advance   finished -> unchanged; slide + 1 < slide_count -> slide += 1; else as Next
    /// Slide     the same slide -> unchanged; else slide = the given one
    /// TogglePause  finished -> unchanged; else paused = !paused
    /// Restart   empty order -> unchanged; else show(0) and paused = false
    /// Sync      reconcile_cursor None -> unchanged; the same post stays on screen -> only the
    ///           cursor is replaced; else the cursor is replaced, slide = 0, visit += 1
    /// ```
    pub fn apply(&mut self, action: PlayerAction<T>) -> bool {
        let length = self.cursor.order.len();
        match action {
            PlayerAction::Next { loop_posts } => self.go_to_next(loop_posts),
            PlayerAction::Previous { loop_posts } => {
                let target = if self.finished {
                    // From the end screen, back to the last post.
                    length.checked_sub(1)
                } else {
                    previous_position(self.cursor.position, length, loop_posts)
                };
                match target {
                    Some(position) => {
                        self.show(position);
                        true
                    }
                    None => false,
                }
            }
            PlayerAction::Advance { slide_count, loop_posts } => {
                if self.finished {
                    false
                } else if self.slide.saturating_add(1) < slide_count {
                    self.slide += 1;
                    true
                } else {
                    self.go_to_next(loop_posts)
                }
            }
            PlayerAction::Slide { slide } => {
                let changed = slide != self.slide;
                self.slide = slide;
                changed
            }
            PlayerAction::TogglePause => {
                if self.finished {
                    return false;
                }
                self.paused = !self.paused;
                true
            }
            PlayerAction::Restart => {
                if length == 0 {
                    return false;
                }
                self.show(0);
                self.paused = false;
                true
            }
            PlayerAction::Sync { ids } => {
                let Some(cursor) = reconcile_cursor(&self.cursor, &ids) else {
                    return false;
                };
                let same_post = cursor.order.get(cursor.position) == self.current();
                self.cursor = cursor;
                if !same_post {
                    self.slide = 0;
                    self.visit += 1;
                }
                true
            }
        }
    }

    /// The `Next` action, which `Advance` shares after the last slide of a post.
    fn go_to_next(&mut self, loop_posts: bool) -> bool {
        if self.finished {
            return false;
        }
        match next_position(self.cursor.position, self.cursor.order.len(), loop_posts) {
            Some(position) => self.show(position),
            None => self.finished = true,
        }
        true
    }

    /// The clone's `showPosition`: the post at `position` from its first slide. The visit
    /// counter goes up even when it is the post already on screen, which restarts its time.
    fn show(&mut self, position: usize) {
        self.cursor.position = position;
        self.slide = 0;
        self.visit += 1;
        self.finished = false;
    }

    /// The status line of the player (`messages::auto_view_announcement`): what a screen reader
    /// hears when the post changes, or auto view is paused or at its end. `username` is the
    /// author of the post on screen.
    pub fn announcement(&self, username: Option<&str>) -> String {
        messages::auto_view_announcement(
            self.cursor.position + 1,
            self.cursor.order.len(),
            self.finished,
            self.paused,
            username,
        )
    }
}

// --- Random numbers for the shuffle -----------------------------------------------------------

/// A small deterministic random number generator (SplitMix64) for the shuffle. The app seeds
/// it from the clock (`text::now_ms`) when auto view starts; tests seed it with a constant, or
/// pass their own sequence to `start_cursor`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SeededRandom {
    state: u64,
}

impl SeededRandom {
    /// A generator whose sequence is fixed by `seed`.
    pub fn new(seed: u64) -> SeededRandom {
        SeededRandom { state: seed }
    }

    /// The next value in `[0, 1)`, uniformly distributed over 53 bits.
    pub fn next_f64(&mut self) -> f64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut mixed = self.state;
        mixed = (mixed ^ (mixed >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        mixed = (mixed ^ (mixed >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        mixed ^= mixed >> 31;
        (mixed >> 11) as f64 / (1u64 << 53) as f64
    }
}

// --- Timer ------------------------------------------------------------------------------------

/// Counts the active (not paused) time of the current slide and reports once when it reaches
/// the due time (the clone's `ActiveTimer`, autoView.ts:353-418). Pausing keeps the elapsed time
/// exactly; the due time can change at any moment (a new interval) and applies to the time
/// already elapsed.
///
/// The clock is injected: every method that needs the time takes `now_ms`, a monotonic time in
/// milliseconds (the app passes the seconds of `egui::InputState::time` times 1000). There is no
/// callback: the app calls `poll` every frame and applies `PlayerAction::Advance` when it
/// returns true, and asks `remaining_ms` for when to repaint next.
///
/// Wiring (AutoView.tsx:265-304): `slide_key = (visit, post, slide)`. When it changes: `reset`,
/// then `set_due(plan.due_ms)`. When only the plan changes (a new interval): `set_due` only.
/// `active = !paused && window not minimized && !finished && a post exists`
/// (`PlayerState::is_active`): active -> `resume`, otherwise `pause` (`set_active`). The test
/// `a_whole_run_wired_as_the_app_wires_it` at the bottom of this file does exactly this.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ActiveTimer {
    accumulated_ms: f64,
    /// The time the timer was last started; `None` while paused.
    started_at_ms: Option<f64>,
    /// `f64::INFINITY` when there is no due time (before `set_due`, and after it fired).
    due_ms: f64,
}

impl Default for ActiveTimer {
    fn default() -> ActiveTimer {
        ActiveTimer::new()
    }
}

impl ActiveTimer {
    /// A paused timer with no elapsed time and no due time.
    pub fn new() -> ActiveTimer {
        ActiveTimer { accumulated_ms: 0.0, started_at_ms: None, due_ms: f64::INFINITY }
    }

    /// True while the timer counts.
    pub fn is_running(&self) -> bool {
        self.started_at_ms.is_some()
    }

    /// The active time of the current slide so far, in milliseconds.
    pub fn elapsed_ms(&self, now_ms: f64) -> f64 {
        self.accumulated_ms + self.started_at_ms.map_or(0.0, |started| now_ms - started)
    }

    /// The due time set by `set_due`; `f64::INFINITY` when there is none.
    pub fn due_ms(&self) -> f64 {
        self.due_ms
    }

    /// A new slide: elapsed time back to zero, no due time until `set_due`. Keeps running or
    /// paused.
    pub fn reset(&mut self, now_ms: f64) {
        self.accumulated_ms = 0.0;
        if self.started_at_ms.is_some() {
            self.started_at_ms = Some(now_ms);
        }
        self.due_ms = f64::INFINITY;
    }

    /// The slide is done once the elapsed time reaches `due_ms`. It applies to the time already
    /// elapsed: when that is past the new due time, the next `poll` reports it.
    pub fn set_due(&mut self, due_ms: f64) {
        self.due_ms = due_ms;
    }

    /// Stops counting and keeps the elapsed time exactly. Does nothing while paused.
    pub fn pause(&mut self, now_ms: f64) {
        if self.started_at_ms.is_some() {
            self.accumulated_ms = self.elapsed_ms(now_ms);
            self.started_at_ms = None;
        }
    }

    /// Continues counting. Does nothing while running.
    pub fn resume(&mut self, now_ms: f64) {
        if self.started_at_ms.is_none() {
            self.started_at_ms = Some(now_ms);
        }
    }

    /// `resume` when `active`, otherwise `pause`: the last step of the wiring, called every
    /// frame with `PlayerState::is_active`.
    pub fn set_active(&mut self, active: bool, now_ms: f64) {
        if active {
            self.resume(now_ms);
        } else {
            self.pause(now_ms);
        }
    }

    /// The fire rule: true exactly once, when the timer is running, the due time is finite and
    /// `due_ms - elapsed_ms(now_ms) <= DUE_TOLERANCE_MS`. The due time is then cleared
    /// (`f64::INFINITY`), so the next call is false until `set_due`. Never true while paused.
    pub fn poll(&mut self, now_ms: f64) -> bool {
        if !self.is_running() || !self.due_ms.is_finite() {
            return false;
        }
        if self.due_ms - self.elapsed_ms(now_ms) > DUE_TOLERANCE_MS {
            return false;
        }
        self.due_ms = f64::INFINITY;
        true
    }

    /// Milliseconds until `poll` will report, for scheduling the next repaint: `None` while
    /// paused or without a due time, otherwise `max(0, due_ms - elapsed_ms(now_ms))`.
    pub fn remaining_ms(&self, now_ms: f64) -> Option<f64> {
        if !self.is_running() || !self.due_ms.is_finite() {
            return None;
        }
        Some(f64::max(0.0, self.due_ms - self.elapsed_ms(now_ms)))
    }
}

#[cfg(test)]
mod tests {
    //! The clone's frontend/src/autoView.test.ts, group by group, with the vectors of
    //! spec-viewer 4.6 and 4.8 and those of the video option. Tests of localStorage and of the
    //! rendered dialogs have no counterpart here.

    use serde_json::json;

    use super::*;

    fn arguments(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    /// The request of a command line that must ask for auto view.
    fn request(list: &[&str]) -> AutoViewRequest {
        parse_arguments(&arguments(list)).auto_view.expect("the arguments ask for auto view")
    }

    // --- interval -----------------------------------------------------------------------------

    #[test]
    fn clamps_to_whole_seconds_between_the_limits_and_falls_back_to_the_default_for_non_numbers() {
        assert_eq!(clamp_interval(15.0), 15);
        assert_eq!(clamp_interval(14.6), 15);
        assert_eq!(clamp_interval(0.0), MIN_INTERVAL_SEC);
        assert_eq!(clamp_interval(-30.0), MIN_INTERVAL_SEC);
        assert_eq!(clamp_interval(99_999.0), MAX_INTERVAL_SEC);
        assert_eq!(clamp_interval(f64::NAN), DEFAULT_INTERVAL_SEC);
        assert_eq!(clamp_interval(f64::INFINITY), DEFAULT_INTERVAL_SEC);
        assert_eq!(clamp_interval(f64::NEG_INFINITY), DEFAULT_INTERVAL_SEC);
    }

    #[test]
    fn rounds_a_half_up_like_math_round() {
        assert_eq!(clamp_interval(2.5), 3);
        assert_eq!(clamp_interval(3.5), 4);
        assert_eq!(clamp_interval(14.4), 14);
        assert_eq!(clamp_interval(14.5), 15);
        // The largest float below 14.5 must not be rounded up.
        assert_eq!(clamp_interval(14.499_999_999_999_998), 14);
        assert_eq!(clamp_interval(3599.5), 3600);
        assert_eq!(clamp_interval(1.5), 2);
        assert_eq!(clamp_interval(-2.5), MIN_INTERVAL_SEC);
        assert_eq!(clamp_interval(1e300), MAX_INTERVAL_SEC);
        assert_eq!(clamp_interval(-1e300), MIN_INTERVAL_SEC);
    }

    #[test]
    fn steps_through_the_preset_values_also_from_a_value_between_two_presets() {
        assert_eq!(step_interval(15, Step::Longer), 20);
        assert_eq!(step_interval(15, Step::Shorter), 10);
        assert_eq!(step_interval(7, Step::Longer), 8);
        assert_eq!(step_interval(7, Step::Shorter), 5);
        assert_eq!(step_interval(MIN_INTERVAL_SEC, Step::Shorter), MIN_INTERVAL_SEC);
        assert_eq!(step_interval(MAX_INTERVAL_SEC, Step::Longer), MAX_INTERVAL_SEC);
        assert_eq!(step_interval(2000, Step::Longer), MAX_INTERVAL_SEC);
        // A value outside the limits is clamped before it is stepped.
        assert_eq!(step_interval(0, Step::Longer), 3);
        assert_eq!(step_interval(0, Step::Shorter), MIN_INTERVAL_SEC);
        assert_eq!(step_interval(u32::MAX, Step::Shorter), 1800);
    }

    #[test]
    fn the_step_table_can_be_walked_from_end_to_end_in_both_directions() {
        assert!(INTERVAL_STEPS.windows(2).all(|pair| pair[0] < pair[1]));
        assert_eq!(INTERVAL_STEPS[0], MIN_INTERVAL_SEC);
        assert_eq!(INTERVAL_STEPS[INTERVAL_STEPS.len() - 1], MAX_INTERVAL_SEC);
        assert!(INTERVAL_STEPS.contains(&DEFAULT_INTERVAL_SEC));
        assert!(INTERVAL_PRESETS.iter().all(|preset| INTERVAL_STEPS.contains(preset)));

        let mut up = vec![MIN_INTERVAL_SEC];
        while up[up.len() - 1] < MAX_INTERVAL_SEC {
            up.push(step_interval(up[up.len() - 1], Step::Longer));
        }
        assert_eq!(up, INTERVAL_STEPS);
        let mut down = vec![MAX_INTERVAL_SEC];
        while down[down.len() - 1] > MIN_INTERVAL_SEC {
            down.push(step_interval(down[down.len() - 1], Step::Shorter));
        }
        down.reverse();
        assert_eq!(down, INTERVAL_STEPS);
    }

    #[test]
    fn formats_seconds_minutes_and_the_hour() {
        assert_eq!(format_interval(15), "15 s");
        assert_eq!(format_interval(60), "1 min");
        assert_eq!(format_interval(90), "1 min 30 s");
        assert_eq!(format_interval(MAX_INTERVAL_SEC), "1 h");
        assert_eq!(format_interval(1), "2 s");
        assert_eq!(format_interval(59), "59 s");
        assert_eq!(format_interval(1800), "30 min");
        assert_eq!(format_interval(3599), "59 min 59 s");
        assert_eq!(format_interval(100_000), "1 h");
        // The quick choices of the setup dialog.
        let presets: Vec<String> = INTERVAL_PRESETS.iter().map(|&preset| format_interval(preset)).collect();
        assert_eq!(presets, ["5 s", "10 s", "15 s", "30 s", "1 min"]);
    }

    #[test]
    fn accepts_only_whole_seconds_within_the_limits_as_typed_input() {
        assert_eq!(parse_interval_input(" 15 "), Some(15));
        assert_eq!(parse_interval_input("2"), Some(2));
        assert_eq!(parse_interval_input("3600"), Some(3600));
        for bad in ["", "abc", "1", "3601", "2.5", "-5", "1e3", "15s"] {
            assert_eq!(parse_interval_input(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn typed_input_follows_the_pattern_of_the_clone_to_the_letter() {
        // `\s` of a JavaScript pattern: also the no-break space, the line breaks and U+FEFF.
        assert_eq!(parse_interval_input("\t\u{A0}30\u{FEFF}\r\n"), Some(30));
        assert_eq!(parse_interval_input("\u{2003}45\u{2028}"), Some(45));
        // Leading zeros are digits like any other.
        assert_eq!(parse_interval_input("015"), Some(15));
        assert_eq!(parse_interval_input("0000000000000000000000003600"), Some(3600));
        for bad in [
            " ",
            "0",
            "000",
            "+15",
            "1 5",
            "15.",
            "15.0",
            "0x10",
            "１５",
            "٣٠",
            "15 seconds",
            "36000",
            "99999999999999999999999999999999",
        ] {
            assert_eq!(parse_interval_input(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn the_setup_dialog_gets_the_seconds_or_the_sentence_to_show() {
        assert_eq!(check_interval_input(" 30 "), Ok(30));
        assert_eq!(check_interval_input("1"), Err("Enter a whole number of seconds from 2 to 3600."));
        assert_eq!(check_interval_input(""), Err(messages::AUTO_VIEW_INTERVAL_ERROR));
        assert_eq!(check_interval_input("3601"), Err(messages::AUTO_VIEW_INTERVAL_ERROR));
    }

    // --- settings persistence -----------------------------------------------------------------

    #[test]
    fn round_trips_the_settings_through_json() {
        let settings = Settings { interval_sec: 30, wait_for_videos: false, loop_posts: false, shuffle: true };
        let stored = settings_to_json(&settings);
        assert_eq!(stored, json!({ "interval_sec": 30, "wait_for_videos": false, "loop": false, "shuffle": true }));
        // The key order is part of the file format.
        assert_eq!(stored.to_string(), r#"{"interval_sec":30,"wait_for_videos":false,"loop":false,"shuffle":true}"#);
        assert_eq!(normalize_settings(&stored), settings);

        let defaults = Settings::default();
        assert_eq!(defaults, Settings { interval_sec: 15, wait_for_videos: true, loop_posts: true, shuffle: false });
        assert_eq!(settings_to_json(&defaults).to_string(), r#"{"interval_sec":15,"wait_for_videos":true,"loop":true,"shuffle":false}"#);
        assert_eq!(normalize_settings(&settings_to_json(&defaults)), defaults);
    }

    #[test]
    fn writes_an_interval_within_the_limits() {
        let short = Settings { interval_sec: 0, ..Settings::default() };
        assert_eq!(settings_to_json(&short)["interval_sec"], json!(2));
        let long = Settings { interval_sec: u32::MAX, ..Settings::default() };
        assert_eq!(settings_to_json(&long)["interval_sec"], json!(3600));
    }

    #[test]
    fn repairs_damaged_or_partial_stored_values_field_by_field() {
        let defaults = Settings::default();
        assert_eq!(
            normalize_settings(&json!({ "interval_sec": 1e9, "loop": "yes", "shuffle": true })),
            Settings { interval_sec: MAX_INTERVAL_SEC, shuffle: true, ..defaults }
        );
        assert_eq!(normalize_settings(&Value::Null), defaults);
        assert_eq!(normalize_settings(&json!({ "interval_sec": "20" })), defaults);

        assert_eq!(normalize_settings(&json!({})), defaults);
        assert_eq!(normalize_settings(&json!("fast")), defaults);
        assert_eq!(normalize_settings(&json!(30)), defaults);
        assert_eq!(normalize_settings(&json!([30, false, true])), defaults);
        assert_eq!(normalize_settings(&json!({ "interval_sec": 20.4 })), Settings { interval_sec: 20, ..defaults });
        assert_eq!(normalize_settings(&json!({ "interval_sec": 20.5 })), Settings { interval_sec: 21, ..defaults });
        assert_eq!(normalize_settings(&json!({ "interval_sec": -4 })), Settings { interval_sec: 2, ..defaults });
        assert_eq!(normalize_settings(&json!({ "interval_sec": null, "loop": false })), Settings {
            loop_posts: false,
            ..defaults
        });
        assert_eq!(normalize_settings(&json!({ "loop": 0, "shuffle": 1 })), defaults);
        assert_eq!(normalize_settings(&json!({ "shuffle": "true", "loop": null })), defaults);
        // The video option is a JSON boolean too; a file written before it existed keeps the default.
        assert_eq!(normalize_settings(&json!({ "wait_for_videos": false })), Settings { wait_for_videos: false, ..defaults });
        assert_eq!(normalize_settings(&json!({ "wait_for_videos": "no" })), defaults);
        assert_eq!(normalize_settings(&json!({ "interval_sec": 30, "loop": false, "shuffle": true })), Settings { interval_sec: 30, wait_for_videos: true, loop_posts: false, shuffle: true });
        // The names of the clone's localStorage value and unknown members mean nothing here.
        assert_eq!(
            normalize_settings(&json!({ "intervalSec": 30, "Loop": false, "waitForVideos": false, "extra": [1] })),
            defaults
        );
    }

    // --- command line (the clone's parseAutoViewHash) -----------------------------------------

    #[test]
    fn is_none_when_the_arguments_do_not_ask_for_auto_view() {
        assert_eq!(parse_arguments(&[]), ParsedArguments { auto_view: None, rest: Vec::new() });
        for list in [vec!["profile"], vec!["--autoviewer=15"], vec!["--auto-view"], vec!["autoview=30"]] {
            let parsed = parse_arguments(&arguments(&list));
            assert_eq!(parsed.auto_view, None, "{list:?}");
            assert_eq!(parsed.rest, arguments(&list), "{list:?}");
        }
        // "#loop=0" alone is no request in the clone; here the option is consumed all the same.
        let parsed = parse_arguments(&arguments(&["--loop=0", "--shuffle"]));
        assert_eq!(parsed, ParsedArguments { auto_view: None, rest: Vec::new() });
    }

    #[test]
    fn reads_the_interval_and_the_flags() {
        let nothing = AutoViewRequest::default();
        assert_eq!(request(&["--autoview"]), nothing);
        assert_eq!(request(&["--autoview="]), nothing);
        assert_eq!(request(&["--autoview=15"]), AutoViewRequest { interval_sec: Some(15), ..nothing });
        assert_eq!(request(&["--autoview=30"]), AutoViewRequest { interval_sec: Some(30), ..nothing });
        assert_eq!(request(&["--autoview=15s"]), AutoViewRequest { interval_sec: Some(15), ..nothing });
        assert_eq!(request(&["--autoview=15", "--loop=0"]), AutoViewRequest {
            interval_sec: Some(15),
            loop_posts: Some(false),
            ..nothing
        });
        // autoView.test.ts:139-143.
        assert_eq!(request(&["--autoview=10", "--shuffle=1", "--wait=off", "--loop=true"]), AutoViewRequest {
            interval_sec: Some(10),
            wait_for_videos: Some(false),
            loop_posts: Some(true),
            shuffle: Some(true)
        });
        assert_eq!(request(&["--loop=no", "--AutoView=8", "--waitForVideos=yes"]), AutoViewRequest {
            interval_sec: Some(8),
            wait_for_videos: Some(true),
            loop_posts: Some(false),
            shuffle: None
        });
        // A readable --wait wins over --waitforvideos; an unreadable one leaves it the say.
        assert_eq!(request(&["--autoview", "--waitforvideos=0", "--wait=1"]).wait_for_videos, Some(true));
        assert_eq!(request(&["--autoview", "--wait=maybe", "--waitforvideos=0"]).wait_for_videos, Some(false));
        assert_eq!(request(&["--autoview", "--wait"]).wait_for_videos, Some(true));
        // A flag without a value switches it on.
        assert_eq!(request(&["--autoview", "--shuffle"]), AutoViewRequest { shuffle: Some(true), ..nothing });
        assert_eq!(request(&["--autoview", "--loop"]), AutoViewRequest { loop_posts: Some(true), ..nothing });
    }

    #[test]
    fn clamps_the_interval_and_ignores_values_it_cannot_read() {
        let nothing = AutoViewRequest::default();
        assert_eq!(request(&["--autoview=1"]).interval_sec, Some(MIN_INTERVAL_SEC));
        assert_eq!(request(&["--autoview=100000"]).interval_sec, Some(MAX_INTERVAL_SEC));
        assert_eq!(request(&["--autoview=abc", "--loop=maybe"]), nothing);
        assert_eq!(request(&["--autoview=-5"]), nothing);
    }

    #[test]
    fn reads_the_seconds_with_the_pattern_of_the_clone() {
        let seconds = |value: &str| request(&[format!("--autoview={value}").as_str()]).interval_sec;
        assert_eq!(seconds("2.5"), Some(3));
        assert_eq!(seconds("14.4"), Some(14));
        assert_eq!(seconds("0"), Some(2));
        assert_eq!(seconds("0015"), Some(15));
        assert_eq!(seconds(" 15 "), Some(15));
        assert_eq!(seconds("15S"), Some(15));
        assert_eq!(seconds(" 15 s "), Some(15));
        assert_eq!(seconds("\u{A0}20\ts\u{FEFF}"), Some(20));
        assert_eq!(seconds("7.25s"), Some(7));
        for bad in [
            " ", "s", "15ss", "15 sec", "15m", "1e3", ".5", "5.", "1.2.3", "+5", "1,5", "0x10", "１５", "15\u{17F}",
            "=15",
        ] {
            assert_eq!(seconds(bad), None, "{bad:?}");
        }
        // A number too long for a float is infinity for the clone too, which gives the default.
        assert_eq!(seconds(&"9".repeat(400)), Some(DEFAULT_INTERVAL_SEC));
        assert_eq!(seconds(&"9".repeat(40)), Some(MAX_INTERVAL_SEC));
    }

    #[test]
    fn reads_the_flag_words_trimmed_and_in_any_letter_case() {
        let flag = |word: &str| request(&["--autoview", format!("--loop={word}").as_str()]).loop_posts;
        for word in ["", "1", "true", "yes", "on", "TRUE", "Yes", " on ", "\tOn\u{A0}"] {
            assert_eq!(flag(word), Some(true), "{word:?}");
        }
        for word in ["0", "false", "no", "off", "FALSE", "No", " off ", "OFF"] {
            assert_eq!(flag(word), Some(false), "{word:?}");
        }
        for word in ["maybe", "2", "-1", "y", "n", "t", "tr ue", "1=1", "enabled"] {
            assert_eq!(flag(word), None, "{word:?}");
        }
        // The same words for the shuffle.
        assert_eq!(request(&["--autoview", "--shuffle=off"]).shuffle, Some(false));
        assert_eq!(request(&["--autoview", "--shuffle=YES"]).shuffle, Some(true));
        assert_eq!(request(&["--autoview", "--shuffle=perhaps"]).shuffle, None);
    }

    #[test]
    fn option_names_have_no_letter_case_and_the_first_occurrence_wins() {
        assert_eq!(request(&["--AUTOVIEW=20", "--Loop=0", "--SHUFFLE=1"]), AutoViewRequest {
            interval_sec: Some(20),
            wait_for_videos: None,
            loop_posts: Some(false),
            shuffle: Some(true)
        });
        // The clone trims the names of the hash; a quoted argument can carry such a space.
        assert_eq!(request(&["--autoview =12", "-- loop =off"]), AutoViewRequest {
            interval_sec: Some(12),
            wait_for_videos: None,
            loop_posts: Some(false),
            shuffle: None
        });

        let parsed = parse_arguments(&arguments(&[
            "--autoview=5",
            "--autoview=50",
            "--loop=1",
            "--loop=0",
            "--shuffle=0",
            "--Shuffle",
        ]));
        assert_eq!(
            parsed.auto_view,
            Some(AutoViewRequest { interval_sec: Some(5), wait_for_videos: None, loop_posts: Some(true), shuffle: Some(false) })
        );
        assert!(parsed.rest.is_empty());
        // The first one wins even when it cannot be read: the later ones are not looked at.
        assert_eq!(
            request(&["--autoview=abc", "--autoview=15", "--loop=maybe", "--loop=0"]),
            AutoViewRequest::default()
        );
    }

    #[test]
    fn everything_else_passes_through_in_its_order() {
        let parsed = parse_arguments(&arguments(&[
            r"C:\Pictures\one.jpg",
            "--autoview=8",
            "--wait=0",
            "--waits=1",
            r"D:\Holiday 2024",
            "--shuffle",
            "15",
            "-autoview",
            "/autoview",
            "--",
            "--loops=1",
            "---loop",
            "",
            "--loop=0",
        ]));
        assert_eq!(
            parsed.auto_view,
            Some(AutoViewRequest { interval_sec: Some(8), wait_for_videos: Some(false), loop_posts: Some(false), shuffle: Some(true) })
        );
        assert_eq!(
            parsed.rest,
            arguments(&[
                r"C:\Pictures\one.jpg",
                "--waits=1",
                r"D:\Holiday 2024",
                "15",
                "-autoview",
                "/autoview",
                "--",
                "--loops=1",
                "---loop",
                "",
            ])
        );
        // The seconds belong to the option itself: a separate number is an argument of its own.
        let separate = parse_arguments(&arguments(&["--autoview", "30"]));
        assert_eq!(separate.auto_view, Some(AutoViewRequest::default()));
        assert_eq!(separate.rest, arguments(&["30"]));
    }

    #[cfg(windows)]
    #[test]
    fn an_argument_that_is_not_unicode_passes_through_unchanged() {
        use std::os::windows::ffi::OsStringExt;

        // "--autoview=" followed by a lone surrogate: no text, so not an option.
        let mut units: Vec<u16> = "--autoview=".encode_utf16().collect();
        units.push(0xD800);
        let broken_option = OsString::from_wide(&units);
        let broken_path = OsString::from_wide(&[0x43, 0x3A, 0x5C, 0xDC00, 0x2E, 0x6A, 0x70, 0x67]);
        assert!(broken_option.to_str().is_none());

        let parsed =
            parse_arguments(&[broken_path.clone(), OsString::from("--shuffle=1"), broken_option.clone()]);
        assert_eq!(parsed.auto_view, None);
        assert_eq!(parsed.rest, vec![broken_path.clone(), broken_option]);

        let parsed = parse_arguments(&[OsString::from("--autoview=9"), broken_path.clone()]);
        assert_eq!(parsed.auto_view.and_then(|request| request.interval_sec), Some(9));
        assert_eq!(parsed.rest, vec![broken_path]);
    }

    #[test]
    fn a_request_lies_over_the_remembered_settings_for_this_run() {
        let remembered = Settings { interval_sec: 30, wait_for_videos: true, loop_posts: false, shuffle: true };
        assert_eq!(request(&["--autoview"]).apply_to(remembered), remembered);
        assert_eq!(request(&["--autoview=5", "--loop=1", "--shuffle=0", "--wait=0"]).apply_to(remembered), Settings {
            interval_sec: 5,
            wait_for_videos: false,
            loop_posts: true,
            shuffle: false
        });
        assert_eq!(request(&["--autoview=abc", "--loop=maybe"]).apply_to(remembered), remembered);
        assert_eq!(request(&["--autoview", "--loop=1"]).apply_to(Settings::default()), Settings::default());
    }

    // --- timing -------------------------------------------------------------------------------

    #[test]
    fn shares_the_interval_between_carousel_slides_at_least_2_s_per_slide() {
        assert_eq!(slide_seconds(15, 1), 15.0);
        assert_eq!(slide_seconds(15, 3), 5.0);
        assert_eq!(slide_seconds(15, 10), 2.0);
        assert_eq!(slide_seconds(4, 3), 2.0);
        assert_eq!(slide_seconds(15, 0), 15.0);
        assert_eq!(slide_seconds(10, 4), 2.5);
        // The interval is clamped first.
        assert_eq!(slide_seconds(0, 1), 2.0);
        assert_eq!(slide_seconds(100_000, 2), 1800.0);
        assert_eq!(slide_seconds(15, usize::MAX), 2.0);
    }

    /// `plan_slide` for a still picture.
    fn picture(slide_sec: f64) -> SlidePlan {
        plan_slide(slide_sec, false, true, VideoState::UNKNOWN)
    }

    #[test]
    fn gives_a_picture_its_slide_time() {
        assert_eq!(picture(15.0), SlidePlan { due_ms: 15_000.0, expected_ms: 15_000.0, listen_for_end: false });
        assert_eq!(picture(2.5), SlidePlan { due_ms: 2_500.0, expected_ms: 2_500.0, listen_for_end: false });
        assert_eq!(picture(slide_seconds(15, 3)), SlidePlan { due_ms: 5_000.0, expected_ms: 5_000.0, listen_for_end: false });
        // A carousel of many slides takes longer than the interval: two seconds for each.
        assert_eq!(picture(slide_seconds(5, 30)).due_ms * 30.0, 60_000.0);
    }

    fn video(duration_sec: Option<f64>, ended: bool) -> VideoState {
        VideoState { duration_sec, ended }
    }

    #[test]
    fn gives_videos_the_slide_time_when_not_waiting_for_them_and_lets_them_loop() {
        // autoView.test.ts:166-170.
        let forty = video(Some(40.0), false);
        assert_eq!(plan_slide(15.0, false, true, forty), SlidePlan { due_ms: 15_000.0, expected_ms: 15_000.0, listen_for_end: false });
        assert_eq!(plan_slide(15.0, true, false, forty), SlidePlan { due_ms: 15_000.0, expected_ms: 15_000.0, listen_for_end: false });
    }

    #[test]
    fn keeps_a_longer_video_until_it_ends_with_a_safety_deadline() {
        // autoView.test.ts:172-177.
        assert_eq!(plan_slide(15.0, true, true, video(Some(40.0), false)), SlidePlan { due_ms: video_deadline_sec(40.0) * 1000.0, expected_ms: 40_000.0, listen_for_end: true });
        assert_eq!(video_deadline_sec(40.0), 50.0);
        assert_eq!(video_deadline_sec(8.0), 13.0);
    }

    #[test]
    fn moves_on_at_the_slide_time_once_the_video_ended() {
        // autoView.test.ts:179-182.
        assert_eq!(plan_slide(15.0, true, true, video(Some(40.0), true)), SlidePlan { due_ms: 15_000.0, expected_ms: 15_000.0, listen_for_end: true });
    }

    #[test]
    fn lets_a_shorter_video_loop_until_the_slide_time_is_up() {
        // autoView.test.ts:184-187.
        assert_eq!(plan_slide(15.0, true, true, video(Some(6.0), false)), SlidePlan { due_ms: 15_000.0, expected_ms: 15_000.0, listen_for_end: false });
    }

    #[test]
    fn waits_for_the_end_of_a_playing_video_whose_length_is_unknown_up_to_a_cap() {
        // autoView.test.ts:189-199.
        let unknown = video(Some(f64::INFINITY), false);
        assert_eq!(plan_slide(3.0, true, true, unknown), SlidePlan { due_ms: UNKNOWN_LENGTH_MAX_SEC * 1000.0, expected_ms: 3_000.0, listen_for_end: true });
        assert_eq!(plan_slide(3.0, true, true, video(Some(f64::INFINITY), true)), SlidePlan { due_ms: 3_000.0, expected_ms: 3_000.0, listen_for_end: true });
        assert_eq!(plan_slide(3.0, true, false, unknown), SlidePlan { due_ms: 3_000.0, expected_ms: 3_000.0, listen_for_end: false });
        assert_eq!(plan_slide(3600.0, true, true, unknown).due_ms, 3_600_000.0);
    }

    #[test]
    fn falls_back_to_the_slide_time_when_the_length_is_missing_or_unusable() {
        // autoView.test.ts:201-206.
        for duration_sec in [None, Some(f64::NAN), Some(f64::NEG_INFINITY), Some(0.0)] {
            assert_eq!(plan_slide(15.0, true, true, video(duration_sec, false)), SlidePlan { due_ms: 15_000.0, expected_ms: 15_000.0, listen_for_end: true }, "{duration_sec:?}");
        }
    }

    #[test]
    fn reports_the_progress_of_the_whole_post() {
        assert_eq!(post_progress(0, 1, 0.0, 10_000.0), 0.0);
        assert_eq!(post_progress(0, 1, 5_000.0, 10_000.0), 0.5);
        assert_eq!(post_progress(0, 1, 25_000.0, 10_000.0), 1.0);
        assert_eq!(post_progress(1, 4, 2_500.0, 5_000.0), 0.375);
        assert_eq!(post_progress(9, 2, 0.0, 5_000.0), 0.5);
        assert_eq!(post_progress(0, 1, 100.0, 0.0), 1.0);

        assert_eq!(post_progress(0, 0, 5_000.0, 10_000.0), 0.5);
        assert_eq!(post_progress(3, 4, 5_000.0, 5_000.0), 1.0);
        assert_eq!(post_progress(0, 2, -400.0, 5_000.0), 0.0);
        assert_eq!(post_progress(1, 2, 100.0, -5.0), 1.0);
        // Values that are not numbers never leave the range.
        assert_eq!(post_progress(0, 1, f64::NAN, 5_000.0), 0.0);
        assert_eq!(post_progress(0, 1, 100.0, f64::NAN), 1.0);
        assert_eq!(post_progress(0, 1, f64::INFINITY, 5_000.0), 1.0);
        assert_eq!(post_progress(0, 2, 100.0, f64::INFINITY), 0.0);
    }

    // --- order and navigation -----------------------------------------------------------------

    const IDS: [&str; 5] = ["a", "b", "c", "d", "e"];

    /// A random source that must not be asked.
    fn unused() -> f64 {
        panic!("the order is not shuffled, so no random number is needed");
    }

    #[test]
    fn keeps_the_feed_order_and_starts_at_the_requested_post() {
        assert_eq!(start_cursor(&IDS, 2, false, &mut unused), Cursor { order: IDS.to_vec(), position: 2 });
        assert_eq!(start_cursor(&IDS, 99, false, &mut unused), Cursor { order: IDS.to_vec(), position: 4 });
        assert_eq!(start_cursor(&IDS, 0, false, &mut unused), Cursor { order: IDS.to_vec(), position: 0 });
        assert_eq!(start_cursor::<&str>(&[], 3, false, &mut unused), Cursor { order: Vec::new(), position: 0 });
        // Nothing to shuffle with fewer than two posts.
        assert_eq!(start_cursor::<&str>(&[], 3, true, &mut unused), Cursor { order: Vec::new(), position: 0 });
        assert_eq!(start_cursor(&["only"], 7, true, &mut unused), Cursor { order: vec!["only"], position: 0 });
    }

    #[test]
    fn shuffles_with_the_start_post_first_and_every_post_exactly_once() {
        let cursor = start_cursor(&IDS, 2, true, &mut || 0.0);
        assert_eq!(cursor.position, 0);
        assert_eq!(cursor.order[0], "c");
        let mut sorted = cursor.order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, IDS);
        assert_ne!(cursor.order, ["c", "a", "b", "d", "e"]);
        // The Fisher-Yates steps of the clone, by hand: [a b d e] -> [e b d a] -> [d b e a] -> [b d e a].
        assert_eq!(cursor.order, ["c", "b", "d", "e", "a"]);
        // The random source is used, so another one gives another order.
        let other = start_cursor(&IDS, 2, true, &mut || 0.999);
        assert_ne!(other.order, cursor.order);
        assert_eq!(other, Cursor { order: vec!["c", "a", "b", "d", "e"], position: 0 });

        // One number is drawn for each post after the first two.
        let mut numbers = [0.5, 0.25, 0.75].into_iter();
        let mut drawn = 0;
        let cursor = start_cursor(&IDS, 99, true, &mut || {
            drawn += 1;
            numbers.next().expect("three numbers are enough for five posts")
        });
        // [a b c d] -> j = 2: [a b d c] -> j = 0: [d b a c] -> j = 1: no change.
        assert_eq!(cursor, Cursor { order: vec!["e", "d", "b", "a", "c"], position: 0 });
        assert_eq!(drawn, 3);
        // Two posts: the other one follows, and there is nothing to draw.
        assert_eq!(start_cursor(&["a", "b"], 1, true, &mut unused), Cursor { order: vec!["b", "a"], position: 0 });
    }

    #[test]
    fn a_random_source_that_misbehaves_still_gives_every_post_once() {
        for value in [1.0, 7.5, -3.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let cursor = start_cursor(&IDS, 1, true, &mut || value);
            assert_eq!(cursor.position, 0, "{value}");
            assert_eq!(cursor.order[0], "b", "{value}");
            let mut sorted = cursor.order.clone();
            sorted.sort_unstable();
            assert_eq!(sorted, IDS, "{value}");
        }
    }

    #[test]
    fn the_seeded_generator_gives_the_same_shuffle_for_the_same_seed() {
        // The first value of SplitMix64 for seed 0 is 0xE220A8397B1DCDAF.
        let mut random = SeededRandom::new(0);
        assert_eq!(random.next_f64(), (0xE220_A839_7B1D_CDAF_u64 >> 11) as f64 / (1u64 << 53) as f64);

        let mut random = SeededRandom::new(2024);
        assert!((0..10_000).map(|_| random.next_f64()).all(|value| (0.0..1.0).contains(&value)));

        let ids: Vec<u32> = (0..200).collect();
        let shuffled = |seed: u64| {
            let mut random = SeededRandom::new(seed);
            start_cursor(&ids, 17, true, &mut || random.next_f64())
        };
        let first = shuffled(1);
        assert_eq!(first, shuffled(1));
        assert_ne!(first.order, shuffled(2).order);
        assert_eq!(first.position, 0);
        assert_eq!(first.order[0], 17);
        let unshuffled: Vec<u32> = ids.iter().copied().filter(|&id| id != 17).collect();
        assert_ne!(first.order[1..], unshuffled[..]);
        let mut sorted = first.order.clone();
        sorted.sort_unstable();
        assert_eq!(sorted, ids);
    }

    #[test]
    fn follows_removed_and_added_posts_and_keeps_the_post_on_screen() {
        let cursor = Cursor { order: vec!["a", "b", "c", "d"], position: 2 };
        assert_eq!(reconcile_cursor(&cursor, &["d", "c", "b", "a"]), None);
        assert_eq!(reconcile_cursor(&cursor, &["a", "b", "c", "d"]), None);
        assert_eq!(
            reconcile_cursor(&cursor, &["a", "c", "d", "x"]),
            Some(Cursor { order: vec!["a", "c", "d", "x"], position: 1 })
        );
        // The post on screen was removed: the one that took its place is shown.
        assert_eq!(reconcile_cursor(&cursor, &["a", "b", "d"]), Some(Cursor { order: vec!["a", "b", "d"], position: 2 }));
        assert_eq!(reconcile_cursor(&cursor, &["a", "d"]), Some(Cursor { order: vec!["a", "d"], position: 1 }));
        assert_eq!(
            reconcile_cursor(&Cursor { order: vec!["a", "b"], position: 1 }, &["a"]),
            Some(Cursor { order: vec!["a"], position: 0 })
        );
        assert_eq!(reconcile_cursor(&cursor, &[]), Some(Cursor { order: Vec::new(), position: 0 }));
    }

    #[test]
    fn keeps_a_shuffled_order_and_adds_new_posts_at_its_end() {
        let shuffled = Cursor { order: vec!["c", "e", "a", "d", "b"], position: 3 };
        assert_eq!(reconcile_cursor(&shuffled, &IDS), None);
        assert_eq!(
            reconcile_cursor(&shuffled, &["x", "a", "b", "c", "y", "d", "e"]),
            Some(Cursor { order: vec!["c", "e", "a", "d", "b", "x", "y"], position: 3 })
        );
        // Removed before the post on screen: it stays on screen at its new position.
        assert_eq!(
            reconcile_cursor(&shuffled, &["b", "d", "z"]),
            Some(Cursor { order: vec!["d", "b", "z"], position: 0 })
        );
        // The last post was on screen and is gone, with nothing behind it: the new last one.
        let at_end = Cursor { order: vec!["a", "b", "c"], position: 2 };
        assert_eq!(reconcile_cursor(&at_end, &["a", "b"]), Some(Cursor { order: vec!["a", "b"], position: 1 }));
        // A whole new collection: its first post.
        assert_eq!(reconcile_cursor(&at_end, &["x", "y"]), Some(Cursor { order: vec!["x", "y"], position: 0 }));
        // From nothing to something, and nothing stays nothing.
        let empty: Cursor<&str> = Cursor { order: Vec::new(), position: 0 };
        assert_eq!(reconcile_cursor(&empty, &["a", "b"]), Some(Cursor { order: vec!["a", "b"], position: 0 }));
        assert_eq!(reconcile_cursor(&empty, &[]), None);
        // A position beyond the order (the fields are public) is brought back into it.
        let beyond = Cursor { order: vec!["a", "b"], position: 9 };
        assert_eq!(reconcile_cursor(&beyond, &["a", "b"]), None);
        assert_eq!(reconcile_cursor(&beyond, &["a", "b", "c"]), Some(Cursor { order: vec!["a", "b", "c"], position: 2 }));
    }

    #[test]
    fn follows_a_large_shuffled_collection_without_delay() {
        // Five thousand posts is the most an export holds; the look-ups must not be quadratic.
        let ids: Vec<String> = (0..5000).map(|number| format!(r"file|c:\pictures\holiday\img_{number:05}.jpg")).collect();
        let mut random = SeededRandom::new(7);
        let cursor = start_cursor(&ids, 0, true, &mut || random.next_f64());
        assert_eq!(reconcile_cursor(&cursor, &ids), None);

        let mut more = ids.clone();
        more.push(r"file|c:\pictures\holiday\new.jpg".to_owned());
        let grown = reconcile_cursor(&cursor, &more).expect("a post was added");
        assert_eq!(grown.order.len(), 5001);
        assert_eq!(grown.order[..5000], cursor.order[..]);
        assert_eq!(grown.order[5000], more[5000]);
        assert_eq!(grown.position, 0);
    }

    #[test]
    fn wraps_around_only_when_looping() {
        assert_eq!(next_position(1, 3, false), Some(2));
        assert_eq!(next_position(2, 3, false), None);
        assert_eq!(next_position(2, 3, true), Some(0));
        assert_eq!(next_position(0, 0, true), None);
        assert_eq!(previous_position(1, 3, false), Some(0));
        assert_eq!(previous_position(0, 3, false), None);
        assert_eq!(previous_position(0, 3, true), Some(2));
        assert_eq!(previous_position(0, 0, true), None);

        assert_eq!(next_position(0, 1, true), Some(0));
        assert_eq!(next_position(0, 1, false), None);
        assert_eq!(previous_position(0, 1, true), Some(0));
        assert_eq!(next_position(usize::MAX, 3, false), None);
        assert_eq!(next_position(usize::MAX, 3, true), Some(0));
    }

    // --- player (the clone's playerReducer) ---------------------------------------------------

    type Player = PlayerState<&'static str>;
    type Action = PlayerAction<&'static str>;

    fn start(order: &[&'static str], position: usize) -> Player {
        PlayerState::new(Cursor { order: order.to_vec(), position })
    }

    fn run(mut state: Player, actions: &[Action]) -> Player {
        for action in actions {
            state.apply(action.clone());
        }
        state
    }

    /// The post, the slide and the end flag of a state.
    fn at(state: &Player) -> (Option<&'static str>, usize, bool) {
        (state.current().copied(), state.slide, state.finished)
    }

    /// Asserts that the actions leave the state as it is, and say so.
    fn assert_unchanged(state: &Player, actions: &[Action]) {
        let mut after = state.clone();
        for action in actions {
            assert!(!after.apply(action.clone()), "{action:?} reports a change");
        }
        assert_eq!(&after, state);
    }

    #[test]
    fn starts_at_the_first_slide_running() {
        let state = start(&["a", "b"], 1);
        assert_eq!(state, PlayerState {
            cursor: Cursor { order: vec!["a", "b"], position: 1 },
            slide: 0,
            visit: 0,
            finished: false,
            paused: false
        });
        assert_eq!(state.current(), Some(&"b"));
        assert_eq!(start(&[], 0).current(), None);
    }

    #[test]
    fn steps_through_the_slides_of_a_carousel_then_to_the_next_post() {
        let advance = Action::Advance { slide_count: 3, loop_posts: true };
        let mut state = start(&["a", "b"], 0);
        assert!(state.apply(advance.clone()));
        assert_eq!(at(&state), (Some("a"), 1, false));
        assert_eq!(state.visit, 0);
        let state = run(state, &[advance.clone(), advance]);
        assert_eq!(at(&state), (Some("b"), 0, false));
        assert_eq!(state.visit, 1);
    }

    #[test]
    fn loops_to_the_first_post_or_stops_on_the_end_screen() {
        let last = start(&["a", "b"], 1);
        assert_eq!(at(&run(last.clone(), &[Action::Advance { slide_count: 1, loop_posts: true }])), (Some("a"), 0, false));
        let mut ended = last.clone();
        assert!(ended.apply(Action::Advance { slide_count: 1, loop_posts: false }));
        assert!(ended.finished);
        // The last post stays where it is behind the end screen.
        assert_eq!(ended, PlayerState { finished: true, ..last });
        // Nothing moves on from the end screen by itself, and pausing does not apply there.
        assert_unchanged(&ended, &[
            Action::Advance { slide_count: 1, loop_posts: false },
            Action::Next { loop_posts: false },
            Action::Advance { slide_count: 3, loop_posts: true },
            Action::Next { loop_posts: true },
        ]);
        assert_unchanged(&ended, &[Action::TogglePause]);
        // Previous goes back to the last post; Restart to the first.
        assert_eq!(at(&run(ended.clone(), &[Action::Previous { loop_posts: false }])), (Some("b"), 0, false));
        assert_eq!(at(&run(ended.clone(), &[Action::Restart])), (Some("a"), 0, false));
        // Both count as a new visit, so the time of the post starts again.
        assert_eq!(run(ended.clone(), &[Action::Previous { loop_posts: false }]).visit, ended.visit + 1);
        assert_eq!(run(ended.clone(), &[Action::Restart]).visit, ended.visit + 1);
    }

    #[test]
    fn restarts_a_single_post_when_it_loops_onto_itself() {
        let state =
            run(start(&["only"], 0), &[Action::Slide { slide: 1 }, Action::Advance { slide_count: 2, loop_posts: true }]);
        assert_eq!(at(&state), (Some("only"), 0, false));
        assert_eq!(state.visit, 1);
    }

    #[test]
    fn goes_back_only_when_there_is_somewhere_to_go() {
        let first = start(&["a", "b"], 0);
        assert_unchanged(&first, &[Action::Previous { loop_posts: false }]);
        assert_eq!(at(&run(first, &[Action::Previous { loop_posts: true }])), (Some("b"), 0, false));
    }

    #[test]
    fn next_and_previous_land_on_the_first_slide_of_the_post() {
        let state = run(start(&["a", "b", "c"], 1), &[Action::Slide { slide: 2 }]);
        assert_eq!(at(&state), (Some("b"), 2, false));
        assert_eq!(state.visit, 0);
        assert_unchanged(&state, &[Action::Slide { slide: 2 }]);
        let next = run(state.clone(), &[Action::Next { loop_posts: false }]);
        assert_eq!(at(&next), (Some("c"), 0, false));
        assert_eq!(next.visit, 1);
        let previous = run(state, &[Action::Previous { loop_posts: false }]);
        assert_eq!(at(&previous), (Some("a"), 0, false));
        assert_eq!(previous.visit, 1);
        // Next at the last post without loop: the end screen.
        assert_eq!(at(&run(next, &[Action::Next { loop_posts: false }])), (Some("c"), 0, true));
    }

    #[test]
    fn keeps_the_pause_across_posts_and_clears_it_on_restart() {
        let paused = run(start(&["a", "b"], 0), &[Action::TogglePause, Action::Next { loop_posts: true }]);
        assert!(paused.paused);
        assert_eq!(at(&paused).0, Some("b"));
        let restarted = run(paused.clone(), &[Action::Next { loop_posts: false }, Action::Restart]);
        assert!(!restarted.paused);
        assert!(!run(paused, &[Action::TogglePause]).paused);
    }

    #[test]
    fn follows_the_posts_same_post_keeps_its_slide_a_replaced_one_starts_over() {
        let state = run(start(&["a", "b", "c"], 1), &[Action::Slide { slide: 2 }]);
        assert_unchanged(&state, &[Action::Sync { ids: vec!["c", "b", "a"] }]);
        let mut added = state.clone();
        assert!(added.apply(Action::Sync { ids: vec!["a", "b", "c", "d"] }));
        assert_eq!(at(&added), (Some("b"), 2, false));
        assert_eq!(added.visit, state.visit);
        assert_eq!(added.cursor.order, ["a", "b", "c", "d"]);
        let removed = run(state.clone(), &[Action::Sync { ids: vec!["a", "c"] }]);
        assert_eq!(at(&removed), (Some("c"), 0, false));
        assert_eq!(removed.visit, state.visit + 1);
    }

    #[test]
    fn a_collection_that_comes_and_goes_keeps_the_player_in_step() {
        // Everything is closed while the player is up, then pictures are opened again.
        let emptied = run(start(&["a", "b"], 1), &[Action::Sync { ids: Vec::new() }]);
        assert_eq!(at(&emptied), (None, 0, false));
        assert_eq!(emptied.visit, 1);
        assert!(!emptied.is_active(false));
        assert_eq!(emptied.announcement(None), "There are no posts to show.");
        let refilled = run(emptied.clone(), &[Action::Sync { ids: vec!["x", "y"] }]);
        assert_eq!(at(&refilled), (Some("x"), 0, false));
        assert_eq!(refilled.visit, 2);

        // Nothing to show: nothing to restart, pause still toggles, Next ends an empty show.
        let empty = start(&[], 0);
        assert_unchanged(&empty, &[
            Action::Restart,
            Action::Previous { loop_posts: true },
            Action::Sync { ids: Vec::new() },
        ]);
        let ended = run(empty, &[Action::Next { loop_posts: true }]);
        assert!(ended.finished);
        assert_unchanged(&ended, &[Action::Previous { loop_posts: true }, Action::Restart]);
    }

    #[test]
    fn announces_the_position_the_pause_and_the_end() {
        let state = start(&["a", "b", "c"], 1);
        assert_eq!(state.announcement(Some("someone")), "Post 2 of 3, by someone.");
        assert_eq!(state.announcement(None), "Post 2 of 3.");
        assert_eq!(PlayerState { paused: true, ..state.clone() }.announcement(Some("someone")), "Auto view paused.");
        assert_eq!(PlayerState { finished: true, ..state }.announcement(Some("someone")), "End of posts.");
        assert_eq!(start(&[], 0).announcement(None), "There are no posts to show.");
    }

    #[test]
    fn the_time_runs_only_while_a_post_is_shown_in_a_visible_window() {
        let state = start(&["a", "b"], 0);
        assert!(state.is_active(false));
        // A hidden (minimized) window stops the time but is no pause of the user's.
        assert!(!state.is_active(true));
        assert!(!state.paused);
        assert!(!PlayerState { paused: true, ..state.clone() }.is_active(false));
        assert!(!PlayerState { finished: true, ..state }.is_active(false));
        assert!(!start(&[], 0).is_active(false));
    }

    // --- ActiveTimer --------------------------------------------------------------------------

    /// A timer with the clock the tests move, counting how often the time was up. The clone's
    /// tests run a fake `setTimeout`; here the timer is asked once after every move, as the app
    /// asks it once per frame.
    struct Bench {
        timer: ActiveTimer,
        now_ms: f64,
        fired: u32,
    }

    impl Bench {
        fn new() -> Bench {
            Bench { timer: ActiveTimer::new(), now_ms: 0.0, fired: 0 }
        }

        fn advance(&mut self, ms: f64) {
            self.now_ms += ms;
            if self.timer.poll(self.now_ms) {
                self.fired += 1;
            }
        }

        fn elapsed_ms(&self) -> f64 {
            self.timer.elapsed_ms(self.now_ms)
        }
    }

    #[test]
    fn a_new_timer_is_paused_and_has_nothing_to_wait_for() {
        let mut timer = ActiveTimer::new();
        assert_eq!(timer, ActiveTimer::default());
        assert!(!timer.is_running());
        assert_eq!(timer.elapsed_ms(5_000.0), 0.0);
        assert_eq!(timer.due_ms(), f64::INFINITY);
        assert_eq!(timer.remaining_ms(5_000.0), None);
        assert!(!timer.poll(5_000.0));
    }

    #[test]
    fn fires_once_when_the_active_time_reaches_the_due_time() {
        let mut bench = Bench::new();
        bench.timer.resume(bench.now_ms);
        bench.timer.set_due(15_000.0);
        assert_eq!(bench.timer.due_ms(), 15_000.0);
        // The clone's vector stops one millisecond before the due time; asked directly, that
        // millisecond is within the tolerance, so this port stops two before it.
        bench.advance(14_998.0);
        assert_eq!(bench.fired, 0);
        bench.advance(2.0);
        assert_eq!(bench.fired, 1);
        assert_eq!(bench.timer.due_ms(), f64::INFINITY);
        bench.advance(60_000.0);
        assert_eq!(bench.fired, 1);
        assert_eq!(bench.timer.remaining_ms(bench.now_ms), None);
    }

    #[test]
    fn fires_within_the_tolerance_of_a_clock_that_is_a_hair_behind() {
        let mut timer = ActiveTimer::new();
        timer.resume(0.0);
        timer.set_due(1_000.0);
        assert!(!timer.poll(998.9));
        assert!(timer.poll(999.0));
        assert!(!timer.poll(999.0));
        assert!(!timer.poll(5_000.0));
    }

    #[test]
    fn keeps_the_remaining_time_exactly_across_pause_and_resume() {
        let mut bench = Bench::new();
        bench.timer.resume(bench.now_ms);
        bench.timer.set_due(10_000.0);
        bench.advance(4_000.0);
        bench.timer.pause(bench.now_ms);
        assert!(!bench.timer.is_running());
        // Nothing to wait for while paused.
        assert_eq!(bench.timer.remaining_ms(bench.now_ms), None);
        bench.advance(60_000.0);
        assert_eq!(bench.elapsed_ms(), 4_000.0);
        assert_eq!(bench.fired, 0);
        // A second pause, or a second resume, changes nothing.
        bench.timer.pause(bench.now_ms);
        assert_eq!(bench.elapsed_ms(), 4_000.0);
        bench.timer.resume(bench.now_ms);
        bench.advance(1_000.0);
        bench.timer.resume(bench.now_ms);
        assert_eq!(bench.elapsed_ms(), 5_000.0);
        assert_eq!(bench.timer.remaining_ms(bench.now_ms), Some(5_000.0));
        bench.advance(4_998.0);
        assert_eq!(bench.fired, 0);
        bench.advance(2.0);
        assert_eq!(bench.fired, 1);
    }

    #[test]
    fn applies_a_new_due_time_to_the_time_already_spent() {
        let mut bench = Bench::new();
        bench.timer.resume(bench.now_ms);
        bench.timer.set_due(30_000.0);
        bench.advance(10_000.0);
        bench.timer.set_due(20_000.0);
        assert_eq!(bench.timer.remaining_ms(bench.now_ms), Some(10_000.0));
        bench.advance(9_998.0);
        assert_eq!(bench.fired, 0);
        bench.advance(2.0);
        assert_eq!(bench.fired, 1);

        // A due time that already passed fires at the next question, not inside `set_due`.
        bench.timer.reset(bench.now_ms);
        bench.advance(8_000.0);
        bench.timer.set_due(5_000.0);
        assert_eq!(bench.fired, 1);
        assert_eq!(bench.timer.remaining_ms(bench.now_ms), Some(0.0));
        bench.advance(0.0);
        assert_eq!(bench.fired, 2);
    }

    #[test]
    fn does_not_fire_while_paused_even_when_the_due_time_is_already_reached() {
        let mut bench = Bench::new();
        bench.timer.resume(bench.now_ms);
        bench.advance(8_000.0);
        bench.timer.pause(bench.now_ms);
        bench.timer.set_due(5_000.0);
        bench.advance(10_000.0);
        assert_eq!(bench.fired, 0);
        bench.timer.resume(bench.now_ms);
        bench.advance(0.0);
        assert_eq!(bench.fired, 1);
    }

    #[test]
    fn starts_a_new_slide_from_zero_and_waits_for_its_due_time() {
        let mut bench = Bench::new();
        bench.timer.resume(bench.now_ms);
        bench.timer.set_due(5_000.0);
        bench.advance(4_000.0);
        bench.timer.reset(bench.now_ms);
        assert_eq!(bench.elapsed_ms(), 0.0);
        assert!(bench.timer.is_running());
        assert_eq!(bench.timer.remaining_ms(bench.now_ms), None);
        bench.advance(10_000.0);
        assert_eq!(bench.fired, 0);
        bench.timer.set_due(5_000.0);
        bench.advance(5_000.0);
        assert_eq!(bench.fired, 1);
    }

    #[test]
    fn a_reset_while_paused_stays_paused_at_zero() {
        let mut bench = Bench::new();
        bench.timer.resume(bench.now_ms);
        bench.timer.set_due(5_000.0);
        bench.advance(3_000.0);
        bench.timer.pause(bench.now_ms);
        bench.advance(1_000.0);
        bench.timer.reset(bench.now_ms);
        assert!(!bench.timer.is_running());
        assert_eq!(bench.elapsed_ms(), 0.0);
        bench.timer.set_due(5_000.0);
        bench.advance(20_000.0);
        assert_eq!(bench.elapsed_ms(), 0.0);
        assert_eq!(bench.fired, 0);
        bench.timer.resume(bench.now_ms);
        bench.advance(4_000.0);
        assert_eq!(bench.fired, 0);
        bench.advance(1_000.0);
        assert_eq!(bench.fired, 1);
    }

    #[test]
    fn waits_again_when_it_is_asked_before_the_clock_reached_the_due_time() {
        let mut timer = ActiveTimer::new();
        timer.resume(0.0);
        timer.set_due(1_000.0);
        assert!(!timer.poll(900.0));
        assert_eq!(timer.remaining_ms(900.0), Some(100.0));
        assert!(timer.poll(1_000.0));
    }

    #[test]
    fn a_due_time_that_is_not_a_number_never_fires() {
        let mut timer = ActiveTimer::new();
        timer.resume(0.0);
        timer.set_due(f64::NAN);
        assert!(!timer.poll(1e12));
        assert_eq!(timer.remaining_ms(1e12), None);
        timer.set_due(f64::NEG_INFINITY);
        assert!(!timer.poll(1e12));
        timer.set_due(0.0);
        assert!(timer.poll(1e12));
    }

    #[test]
    fn set_active_resumes_and_pauses() {
        let mut timer = ActiveTimer::new();
        timer.set_due(10_000.0);
        timer.set_active(true, 1_000.0);
        assert!(timer.is_running());
        timer.set_active(true, 2_000.0);
        assert_eq!(timer.elapsed_ms(3_000.0), 2_000.0);
        timer.set_active(false, 3_000.0);
        assert!(!timer.is_running());
        timer.set_active(false, 9_000.0);
        assert_eq!(timer.elapsed_ms(9_000.0), 2_000.0);
    }

    // --- the player and the timer together ----------------------------------------------------

    /// The player wired to the timer the way the app does it for every frame (the doc comment
    /// of `ActiveTimer`).
    struct Show {
        state: Player,
        timer: ActiveTimer,
        settings: Settings,
        hidden: bool,
        now_ms: f64,
        /// `(visit, post, slide)` of the slide the timer counts for.
        slide_key: Option<(u64, Option<&'static str>, usize)>,
        /// The due time the timer was last given for that slide.
        due_ms: Option<f64>,
        /// Every slide that came on screen: time, post, slide.
        shown: Vec<(f64, &'static str, usize)>,
    }

    impl Show {
        const FRAME_MS: f64 = 250.0;

        fn new(order: &[&'static str], settings: Settings) -> Show {
            let mut show = Show {
                state: start(order, 0),
                timer: ActiveTimer::new(),
                settings,
                hidden: false,
                now_ms: 0.0,
                slide_key: None,
                due_ms: None,
                shown: Vec::new(),
            };
            show.wire();
            show
        }

        /// Post "b" is a carousel of three pictures; every other post is one picture.
        fn slide_count(&self) -> usize {
            if self.state.current() == Some(&"b") { 3 } else { 1 }
        }

        fn wire(&mut self) {
            let slide_key = (self.state.visit, self.state.current().copied(), self.state.slide);
            if self.slide_key != Some(slide_key) {
                self.slide_key = Some(slide_key);
                self.timer.reset(self.now_ms);
                self.due_ms = None;
                if let Some(post) = slide_key.1 {
                    self.shown.push((self.now_ms, post, slide_key.2));
                }
            }
            let plan = plan_slide(slide_seconds(self.settings.interval_sec, self.slide_count()), false, self.settings.wait_for_videos, VideoState::UNKNOWN);
            if self.due_ms != Some(plan.due_ms) {
                self.due_ms = Some(plan.due_ms);
                self.timer.set_due(plan.due_ms);
            }
            self.timer.set_active(self.state.is_active(self.hidden), self.now_ms);
        }

        /// Something the user does between two frames.
        fn act(&mut self, action: Action) {
            self.state.apply(action);
            self.wire();
        }

        fn frame(&mut self) {
            self.now_ms += Show::FRAME_MS;
            self.wire();
            if self.timer.poll(self.now_ms) {
                let slide_count = self.slide_count();
                self.state.apply(Action::Advance { slide_count, loop_posts: self.settings.loop_posts });
                self.wire();
            }
        }

        fn run_until(&mut self, time_ms: f64) {
            while self.now_ms < time_ms {
                self.frame();
            }
        }

        fn progress(&self) -> f64 {
            let expected_ms = plan_slide(slide_seconds(self.settings.interval_sec, self.slide_count()), false, self.settings.wait_for_videos, VideoState::UNKNOWN).expected_ms;
            post_progress(self.state.slide, self.slide_count(), self.timer.elapsed_ms(self.now_ms), expected_ms)
        }
    }

    #[test]
    fn a_whole_run_wired_as_the_app_wires_it() {
        let settings = Settings { interval_sec: 6, wait_for_videos: true, loop_posts: false, shuffle: false };
        let mut show = Show::new(&["a", "b", "c"], settings);
        assert!(show.timer.is_running());
        assert_eq!(show.timer.remaining_ms(show.now_ms), Some(6_000.0));

        // The window is minimized for four seconds in the middle of the first picture: its
        // time stands still, and the user's pause is not touched.
        show.run_until(3_000.0);
        assert_eq!(show.progress(), 0.5);
        show.hidden = true;
        show.wire();
        show.run_until(7_000.0);
        assert!(!show.timer.is_running());
        assert!(!show.state.paused);
        assert_eq!(show.progress(), 0.5);
        show.hidden = false;
        show.wire();

        // The carousel shares the six seconds between its three pictures.
        show.run_until(11_000.0);
        assert_eq!(at(&show.state), (Some("b"), 0, false));
        assert_eq!(show.progress(), 1.0 / 6.0);
        show.run_until(16_000.0);
        assert_eq!(at(&show.state), (Some("c"), 0, false));

        // A pause of five seconds one second into the last picture.
        show.run_until(17_000.0);
        show.act(Action::TogglePause);
        assert_eq!(show.state.announcement(Some("someone")), "Auto view paused.");
        show.run_until(22_000.0);
        assert!(!show.timer.is_running());
        assert_eq!(show.timer.elapsed_ms(show.now_ms), 1_000.0);
        show.act(Action::TogglePause);
        assert_eq!(show.timer.remaining_ms(show.now_ms), Some(5_000.0));

        // Loop is off: the end screen after the last picture, and nothing moves from there.
        show.run_until(26_750.0);
        assert!(!show.state.finished);
        show.run_until(27_000.0);
        assert!(show.state.finished);
        assert!(!show.timer.is_running());
        assert_eq!(show.state.announcement(Some("someone")), "End of posts.");
        show.run_until(60_000.0);
        assert_eq!(at(&show.state), (Some("c"), 0, true));
        assert_eq!(show.shown, [(0.0, "a", 0), (10_000.0, "b", 0), (12_000.0, "b", 1), (14_000.0, "b", 2), (16_000.0, "c", 0)]);

        // Restart: the first picture again, with its full time.
        show.act(Action::Restart);
        assert_eq!(at(&show.state), (Some("a"), 0, false));
        assert!(show.timer.is_running());
        assert_eq!(show.timer.remaining_ms(show.now_ms), Some(6_000.0));

        // A shorter interval applies to the time already spent: 2.5 of now 3 seconds are over.
        show.run_until(62_500.0);
        show.settings.interval_sec = step_interval(step_interval(show.settings.interval_sec, Step::Shorter), Step::Shorter);
        assert_eq!(show.settings.interval_sec, 3);
        show.wire();
        assert_eq!(show.timer.remaining_ms(show.now_ms), Some(500.0));
        show.run_until(63_000.0);
        assert_eq!(at(&show.state), (Some("b"), 0, false));
        // A carousel slide keeps its two seconds however short the interval is.
        assert_eq!(show.timer.remaining_ms(show.now_ms), Some(2_000.0));

        // Paging the carousel by hand starts the time of the new slide from zero.
        show.run_until(64_000.0);
        show.act(Action::Slide { slide: 2 });
        assert_eq!(show.timer.remaining_ms(show.now_ms), Some(2_000.0));
        // Going back from the last picture's end screen shows the last picture again.
        show.run_until(69_000.0);
        assert!(show.state.finished);
        show.act(Action::Previous { loop_posts: false });
        assert_eq!(at(&show.state), (Some("c"), 0, false));
        assert!(show.timer.is_running());
        assert_eq!(show.shown[5..], [
            (60_000.0, "a", 0),
            (63_000.0, "b", 0),
            (64_000.0, "b", 2),
            (66_000.0, "c", 0),
            (69_000.0, "c", 0)
        ]);
    }

    #[test]
    fn a_looping_show_of_one_picture_starts_it_again_and_again() {
        let settings = Settings { interval_sec: 2, wait_for_videos: true, loop_posts: true, shuffle: false };
        let mut show = Show::new(&["only"], settings);
        show.run_until(7_000.0);
        assert_eq!(at(&show.state), (Some("only"), 0, false));
        assert_eq!(show.state.visit, 3);
        assert_eq!(show.shown, [(0.0, "only", 0), (2_000.0, "only", 0), (4_000.0, "only", 0), (6_000.0, "only", 0)]);
        assert_eq!(show.progress(), 0.5);
    }

    #[test]
    fn a_show_without_posts_never_runs() {
        let mut show = Show::new(&[], Settings::default());
        show.run_until(60_000.0);
        assert!(!show.timer.is_running());
        assert!(show.shown.is_empty());
        assert!(!show.state.finished);
        assert_eq!(show.state.announcement(None), "There are no posts to show.");
    }
}
