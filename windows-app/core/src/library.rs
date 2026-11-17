//! The browser imports folder: a port of the clone's library (`backend/app/library.py`), the
//! folder a browser saves an import into (web brief 4.10).
//!
//! The folder is itself a valid "instagram-export" v1 export, so the app opens it like any
//! export folder (`ExportKind::WebLibrary`):
//!
//! ```text
//! post.json                      manifest + "library" metadata (revision, import history)
//! posts/<stem>_<k>.<ext>         media, <stem> derived from the post id
//! posts/<stem>_<k>_poster.<ext>  video posters
//! avatars/<stem>.<ext>           profile pictures, <stem> derived from the username
//! .staging/<upload_id>/...       files of uploads that are not committed yet (and the body of
//!                                their commit while it arrives, .commit.json)
//! .instagram-library             ownership marker; locked while this app uses the folder
//! ```
//!
//! Exactly the clone's rules: the library only adopts a folder that is empty or that it created
//! itself (the marker, or a post.json it wrote), and only ever deletes files it could have
//! written there; a folder that holds an export or other files is refused (503) and never
//! cleaned up. One process at a time uses the folder (the marker is locked with
//! `File::try_lock`, which on Windows covers the whole file), so two copies of the app never
//! overwrite each other's work.
//!
//! The browser parses an export, uploads its media into a staging session (`create_upload`,
//! `receive_file`) and commits the normalised posts (`commit`), which are merged into post.json
//! the way the frontend's `mergeImportResults` does: a post with a known id replaces the old
//! one, the rest is added, newest first. Unlike the clone, captions are not edited here: edits
//! from the browser go to the app's descriptions.json like every other edit.
//!
//! Differences from the clone, on purpose: texts are cut at UTF-16 code units (the app's text
//! helpers) instead of Python code points; details never name the folder's path (another
//! computer may read them), they say "the browser imports folder"; the setting is
//! `"library_directory"`.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::net::{IpAddr, Ipv4Addr};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use serde_json::{Map, Value};

use crate::manifest;
use crate::messages;
use crate::model::MediaKind;
use crate::sniff::{self, MediaFormat};
use crate::store;
use crate::text;
use crate::urls;

const KIB: u64 = 1024;
const MIB: u64 = 1024 * KIB;
const GIB: u64 = 1024 * MIB;

/// The manifest of the folder.
pub const MANIFEST_NAME: &str = "post.json";
/// The media folder.
pub const POSTS_FOLDER: &str = "posts";
/// The profile pictures folder.
pub const AVATARS_FOLDER: &str = "avatars";
/// The uploads that are not committed yet.
pub const STAGING_FOLDER: &str = ".staging";
/// The ownership marker, locked while the app uses the folder.
pub const MARKER_NAME: &str = ".instagram-library";
/// The text of the marker (library.py `_MARKER_TEXT`).
pub const MARKER_TEXT: &[u8] = b"This folder holds the posts imported into the Instagram clone (post.json, posts/,\r\navatars/). The app manages these files: do not put other files into posts/ or avatars/.\r\n";
/// The temporary files of post.json (`store::atomic_write` names them `.post.json-<x>.tmp`).
const TEMPORARY_PREFIX: &str = ".post.json-";
const TEMPORARY_SUFFIX: &str = ".tmp";
/// The probe file that proves the folder is writable.
const PROBE_PREFIX: &str = ".write-test-";

/// File name stems (post ids, usernames) are cut to this many characters.
pub const MAX_STEM_CHARS: usize = 64;
/// The body of a commit is at most this long (`LibraryLimits::max_commit_bytes` by default).
pub const MAX_COMMIT_BYTES: u64 = 32 * MIB;
/// The body of a commit while it arrives, in its session's folder (never a staged name).
const COMMIT_BODY_NAME: &str = ".commit.json";
/// The source name of an import is cut to this many characters.
pub const MAX_SOURCE_NAME_CHARS: usize = 255;
/// Bytes of an upload looked at to decide what it is.
pub const SNIFF_BYTES: usize = 4096;
/// Upload bytes collected before each write to disk.
pub const WRITE_BUFFER_BYTES: usize = MIB as usize;
/// Free disk space is checked again after this many bytes of one upload.
pub const FREE_SPACE_CHECK_BYTES: u64 = 64 * MIB;
/// The largest number JavaScript counts exactly.
const MAX_SAFE_INTEGER: u64 = (1 << 53) - 1;

/// Windows device names, which a file name stem must never be.
const WINDOWS_RESERVED_NAMES: [&str; 22] = [
    "con", "prn", "aux", "nul", "com1", "com2", "com3", "com4", "com5", "com6", "com7", "com8", "com9", "lpt1", "lpt2", "lpt3", "lpt4", "lpt5", "lpt6", "lpt7", "lpt8", "lpt9",
];
/// The extensions of pictures the library stores.
const IMAGE_EXTENSIONS: [&str; 6] = ["jpg", "png", "gif", "webp", "avif", "bmp"];
/// The extensions of videos the library stores.
const VIDEO_EXTENSIONS: [&str; 3] = ["mp4", "mov", "webm"];

/// The stem of a file name the library writes into one of its folders, or `None`.
type StemOf = fn(&str) -> Option<&str>;
/// Whether a file name is one the library writes into one of its folders.
type NameTest = fn(&str) -> bool;

/// Size limits of the library; the defaults are the contract values (library.py `LibraryLimits`),
/// tests shrink them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryLimits {
    /// One picture.
    pub max_image_bytes: u64,
    /// One video.
    pub max_video_bytes: u64,
    /// All files of one upload session.
    pub max_session_bytes: u64,
    /// The whole folder.
    pub max_cache_bytes: u64,
    /// The body of a commit.
    pub max_commit_bytes: u64,
    /// Files of one upload session.
    pub max_files_per_session: usize,
    /// Posts of one commit.
    pub max_posts_per_commit: usize,
    /// Pictures and videos of one post.
    pub max_media_per_post: usize,
    /// Sample comments of one post.
    pub max_comments_per_post: usize,
    /// Import records kept in post.json.
    pub max_imports_history: usize,
    /// An upload session nobody sent a file to for this long is dropped with its staged files.
    pub stale_staging: Duration,
    /// A session that never received a file is dropped after this long (NEW: the frontend sends
    /// its first file right after it started a session, so an empty one only holds a place).
    pub stale_empty_session: Duration,
    /// Upload sessions open at once; beyond it the longest idle one of the same client is
    /// dropped (or 429).
    pub max_open_sessions: usize,
    /// Upload sessions one client may have open at once (NEW: the clone has one browser; here
    /// devices on the network share the folder, and one of them must not crowd out, or cancel,
    /// the imports of the others). Every program on this computer is one client.
    pub max_sessions_per_client: usize,
    /// Of `max_open_sessions`, this many are kept for clients on this computer: devices on the
    /// network never take them, whatever number of addresses they use.
    pub sessions_kept_for_this_computer: usize,
    /// Commits merged into the folder at the same time. The body of a commit (up to
    /// `max_commit_bytes`) is written to its session's folder as it arrives and read into memory
    /// only for its merge, so memory stays bounded, and a body that arrives slowly holds nothing
    /// another client needs.
    pub max_commits_at_once: usize,
    /// How long a commit whose body has arrived waits for its turn to be merged; then 503.
    pub commit_wait: Duration,
    /// Uploads stop while less than this would stay free on the folder's drive.
    pub min_free_bytes: u64,
}

impl Default for LibraryLimits {
    fn default() -> LibraryLimits {
        LibraryLimits {
            max_image_bytes: 64 * MIB,
            max_video_bytes: GIB,
            max_session_bytes: 8 * GIB,
            max_cache_bytes: 64 * GIB,
            max_commit_bytes: MAX_COMMIT_BYTES,
            max_files_per_session: 100_000,
            max_posts_per_commit: 5000,
            max_media_per_post: 30,
            max_comments_per_post: 200,
            max_imports_history: 1000,
            stale_staging: Duration::from_secs(30 * 60),
            stale_empty_session: Duration::from_secs(2 * 60),
            max_open_sessions: 8,
            max_sessions_per_client: 4,
            sessions_kept_for_this_computer: 2,
            max_commits_at_once: 2,
            commit_wait: Duration::from_secs(30),
            min_free_bytes: GIB,
        }
    }
}

/// A request the library refuses; `detail` is safe to show to the user (and to another
/// computer: it names no path).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LibraryError {
    /// The HTTP status of the answer.
    pub status: u16,
    /// The sentence.
    pub detail: String,
}

impl LibraryError {
    /// An error with its status and sentence.
    pub fn new(status: u16, detail: impl Into<String>) -> LibraryError {
        LibraryError { status, detail: detail.into() }
    }
}

impl std::fmt::Display for LibraryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.detail)
    }
}

impl std::error::Error for LibraryError {}

/// The free space of the drive of a folder, in bytes; `None` when it cannot be told.
pub type FreeSpace = fn(&Path) -> Option<u64>;

/// What the library stores: a picture or video format, its MIME type and its extension.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StoredFormat {
    /// Picture or video.
    pub kind: MediaKind,
    /// "image/jpeg", ...
    pub mime: &'static str,
    /// "jpg", ...
    pub extension: &'static str,
}

/// The format the library stores for sniffed bytes; HEIC/HEIF is refused (the browser converts
/// it before it uploads), like everything that is no supported picture or video.
pub fn stored_format(head: &[u8]) -> Option<StoredFormat> {
    let sniffed = sniff::sniff_media(head)?;
    if sniffed.format == MediaFormat::Heic {
        return None;
    }
    Some(StoredFormat { kind: sniffed.kind, mime: sniffed.mime, extension: sniffed.format.extension() })
}

/// One file of an upload session.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StagedFile {
    /// The staged name: "m" + six digits + extension.
    pub name: String,
    /// Its size in bytes.
    pub size: u64,
    /// What it is.
    pub format: StoredFormat,
}

/// The answer to one uploaded file: `{"name", "bytes", "mime", "kind"}`.
pub fn stored_file_json(file: &StagedFile) -> Value {
    let mut object = Map::new();
    object.insert("name".to_owned(), Value::from(file.name.clone()));
    object.insert("bytes".to_owned(), Value::from(file.size));
    object.insert("mime".to_owned(), Value::from(file.format.mime));
    object.insert("kind".to_owned(), Value::from(file.format.kind.as_str()));
    Value::Object(object)
}

// --- Names -------------------------------------------------------------------------------------

/// `[0-9a-f]{32}`: an upload id.
pub fn is_upload_id(text: &str) -> bool {
    text.len() == 32 && text.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// `m[0-9]{6}\.<ext>`: a staged file name, `<ext>` one of the picture and video extensions the
/// library stores (the clone's frontend names its uploads so). library.py allows any
/// `[a-z0-9]{2,5}`; here a browser could then leave `m000001.bat` or `.hta` on the disk until
/// the upload ends, a name Windows would run when it is opened.
pub fn is_staged_name(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 10
        && bytes[0] == b'm'
        && bytes[1..7].iter().all(u8::is_ascii_digit)
        && bytes[7] == b'.'
        && (IMAGE_EXTENSIONS.contains(&&text[8..]) || VIDEO_EXTENSIONS.contains(&&text[8..]))
}

/// True for a file name stem of `[A-Za-z0-9_-]{1,64}`.
fn is_stem(text: &str) -> bool {
    (1..=MAX_STEM_CHARS).contains(&text.len()) && text.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// The stem of a media file name the library writes (library.py `POST_FILE_RE`:
/// `<stem>_<1-3 digits>[_poster].<ext>`), or `None`.
pub fn post_file_stem(name: &str) -> Option<&str> {
    let (base, extension) = name.rsplit_once('.')?;
    if !IMAGE_EXTENSIONS.contains(&extension) && !VIDEO_EXTENSIONS.contains(&extension) {
        return None;
    }
    let numbered = |text: &'_ str| -> Option<usize> {
        let (stem, digits) = text.rsplit_once('_')?;
        ((1..=3).contains(&digits.len()) && digits.bytes().all(|byte| byte.is_ascii_digit()) && is_stem(stem)).then_some(stem.len())
    };
    if let Some(without) = base.strip_suffix("_poster") {
        if let Some(length) = numbered(without) {
            return Some(&name[..length]);
        }
    }
    numbered(base).map(|length| &name[..length])
}

/// The stem of an avatar file name the library writes (library.py `AVATAR_FILE_RE`).
pub fn avatar_file_stem(name: &str) -> Option<&str> {
    let (stem, extension) = name.rsplit_once('.')?;
    (IMAGE_EXTENSIONS.contains(&extension) && is_stem(stem)).then_some(stem)
}

/// library.py `safe_stem`: `[^A-Za-z0-9_-]+` -> "_", "_" trimmed, cut to `MAX_STEM_CHARS - 1`,
/// `fallback` when nothing is left, and a Windows device name gets a "_" appended.
pub fn safe_stem(value: &str, fallback: &str) -> String {
    let mut replaced = String::with_capacity(value.len());
    let mut in_run = false;
    for character in value.chars() {
        if character.is_ascii_alphanumeric() || character == '_' || character == '-' {
            replaced.push(character);
            in_run = false;
        } else if !in_run {
            replaced.push('_');
            in_run = true;
        }
    }
    let mut stem: String = replaced.trim_matches('_').chars().take(MAX_STEM_CHARS - 1).collect();
    if stem.is_empty() {
        stem = fallback.to_owned();
    }
    if WINDOWS_RESERVED_NAMES.contains(&stem.to_lowercase().as_str()) {
        stem.push('_');
    }
    stem
}

/// library.py `_unique_stem`: `base`, `base-2`, `base-3`, ... compared without letter case.
fn unique_stem(base: &str, taken: &mut HashSet<String>) -> String {
    let mut candidate = base.to_owned();
    let mut number = 2;
    while taken.contains(&candidate.to_lowercase()) {
        let suffix = format!("-{number}");
        let keep = MAX_STEM_CHARS.saturating_sub(suffix.len());
        candidate = format!("{}{suffix}", base.chars().take(keep).collect::<String>());
        number += 1;
    }
    taken.insert(candidate.to_lowercase());
    candidate
}

// --- Cleaning of the commit body (the browser already did this; the server does not trust it) --

/// library.py `_shorten`: at most 80 characters.
fn shorten(text: &str) -> String {
    if text.chars().count() <= 80 {
        text.to_owned()
    } else {
        format!("{}...", text.chars().take(77).collect::<String>())
    }
}

/// A string or number as text (`str(value)`); `None` for booleans and everything else.
fn value_text(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(number.to_string()),
        _ => None,
    }
}

/// library.py `clean_text`: trimmed, without NUL, cut to `max_units`.
fn clean_text(value: Option<&Value>, max_units: usize) -> String {
    value_text(value).map(|raw| text::clean_text(&raw, max_units)).unwrap_or_default()
}

/// library.py `clean_username`.
fn clean_username(value: Option<&Value>) -> Option<String> {
    match value? {
        name @ (Value::String(_) | Value::Number(_)) => manifest::clean_username(name),
        _ => None,
    }
}

/// library.py `clean_id`: `[A-Za-z0-9._-]` only, like the frontend parser.
fn clean_id(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(raw) => text::clean_id(raw),
        Value::Number(number) if number.is_i64() || number.is_u64() => text::clean_id(&number.to_string()),
        _ => None,
    }
}

/// library.py `clean_count`: a finite number that is not negative, rounded.
fn clean_count(value: Option<&Value>) -> Option<u64> {
    let number = match value? {
        Value::Number(number) => number.as_f64()?,
        _ => return None,
    };
    if !number.is_finite() || number < 0.0 {
        return None;
    }
    Some((text::js_round(number) as u64).min(MAX_SAFE_INTEGER))
}

/// library.py `parse_timestamp`: ISO 8601 (UTC, milliseconds, Z) of an ISO text or an epoch
/// number; `None` when invalid.
fn parse_timestamp(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if matches!(value, Value::Bool(_)) {
        return None;
    }
    text::parse_timestamp(value).and_then(text::iso_time_millis)
}

/// library.py `safe_web_url`: an http(s) link rebuilt without credentials; `None` otherwise.
fn safe_web_url(value: Option<&Value>) -> Option<String> {
    match value? {
        Value::String(raw) if raw.trim().len() <= manifest::MAX_URL_CHARS => urls::sanitize_link(raw.trim()),
        _ => None,
    }
}

/// True for a link on one of Instagram's own hosts.
fn is_instagram_url(url: &str) -> bool {
    urls::sanitize_instagram_url(url).is_some()
}

/// The first non-blank string among `keys` of `source`, trimmed.
fn first_string(source: &Map<String, Value>, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| match source.get(*key) {
        Some(Value::String(text)) if !text.trim().is_empty() => Some(text.trim().to_owned()),
        _ => None,
    })
}

/// library.py `_string_list`.
fn string_list(value: Option<&Value>) -> Vec<String> {
    match value {
        Some(Value::String(text)) if !text.trim().is_empty() => vec![text.trim().to_owned()],
        Some(Value::Array(items)) => items.iter().filter_map(|item| item.as_str()).map(str::trim).filter(|item| !item.is_empty()).map(str::to_owned).collect(),
        _ => Vec::new(),
    }
}

/// One picture or video of a post of a commit.
struct NewMedia {
    staged: StagedFile,
    poster: Option<StagedFile>,
    alt: Option<String>,
}

/// One post of a commit.
struct NewPost {
    id: String,
    media: Vec<NewMedia>,
    media_type: String,
    user: Map<String, Value>,
    /// url, caption, comment_count, like_count, taken_at, alt, location, comments (in this order).
    fields: Map<String, Value>,
}

/// A parsed commit.
struct Commit {
    source_name: String,
    source_kind: String,
    exported_at: Option<String>,
    source_url: Option<String>,
    posts: Vec<NewPost>,
    avatars: Vec<(String, StagedFile)>,
}

/// library.py `_CommitParser`.
struct CommitParser<'a> {
    files: &'a HashMap<String, StagedFile>,
    limits: &'a LibraryLimits,
}

impl CommitParser<'_> {
    fn staged(&self, value: &str, place: &str) -> Result<StagedFile, LibraryError> {
        let found = if is_staged_name(value) { self.files.get(value) } else { None };
        found.cloned().ok_or_else(|| LibraryError::new(422, messages::commit_not_a_file(place, &shorten(value))))
    }

    /// (path, poster, alt) triples: `media` wins over image/images/video/videos.
    fn media_items(raw: &Map<String, Value>) -> Vec<(String, Option<String>, Option<String>)> {
        if let Some(Value::Array(media)) = raw.get("media") {
            if !media.is_empty() {
                return media
                    .iter()
                    .map(|item| match item {
                        Value::String(path) => (path.trim().to_owned(), None, None),
                        Value::Object(item) => {
                            let path = first_string(item, &["path", "file", "src", "url"]).unwrap_or_default();
                            let poster = first_string(item, &["poster", "thumbnail", "cover"]);
                            let alt = Some(clean_text(item.get("alt"), manifest::MAX_ALT_CHARS)).filter(|alt| !alt.is_empty());
                            (path, poster, alt)
                        }
                        _ => (String::new(), None, None),
                    })
                    .collect();
            }
        }
        let mut images = string_list(raw.get("images"));
        if let Some(Value::String(cover)) = raw.get("image") {
            let cover = cover.trim();
            if !cover.is_empty() && !images.iter().any(|image| image == cover) {
                images.insert(0, cover.to_owned());
            }
        }
        let mut videos = string_list(raw.get("videos"));
        videos.extend(string_list(raw.get("video")));
        let poster = first_string(raw, &["poster", "thumbnail", "cover"]);
        let mut items: Vec<(String, Option<String>, Option<String>)> = images.into_iter().map(|path| (path, None, None)).collect();
        items.extend(videos.into_iter().enumerate().map(|(position, path)| (path, if position == 0 { poster.clone() } else { None }, None)));
        items
    }

    fn media(&self, raw: &Map<String, Value>, place: &str) -> Result<Vec<NewMedia>, LibraryError> {
        let mut result = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for (path, poster_name, alt) in Self::media_items(raw) {
            if !seen.insert(path.clone()) {
                continue;
            }
            let staged = self.staged(&path, place)?;
            let mut poster = None;
            if let (Some(poster_name), MediaKind::Video) = (poster_name, staged.format.kind) {
                let file = self.staged(&poster_name, place)?;
                if file.format.kind != MediaKind::Image {
                    return Err(LibraryError::new(422, messages::commit_poster_not_an_image(place, &poster_name)));
                }
                poster = Some(file);
            }
            result.push(NewMedia { staged, poster, alt });
        }
        if result.is_empty() {
            return Err(LibraryError::new(422, messages::commit_no_media(place)));
        }
        if result.len() > self.limits.max_media_per_post {
            return Err(LibraryError::new(422, messages::commit_too_many_media(place, result.len(), self.limits.max_media_per_post)));
        }
        Ok(result)
    }

    fn user(raw: &Map<String, Value>, from_instagram: bool) -> Map<String, Value> {
        let value = raw.get("user");
        let details = value.and_then(Value::as_object);
        let username = match details {
            Some(details) => clean_username(details.get("username")),
            None => clean_username(value),
        }
        .unwrap_or_else(|| messages::UNKNOWN_USERNAME.to_owned());
        let mut user = Map::new();
        user.insert("username".to_owned(), Value::from(username.clone()));
        if let Some(details) = details {
            let full_name = clean_text(details.get("full_name"), manifest::MAX_NAME_CHARS);
            if !full_name.is_empty() {
                user.insert("full_name".to_owned(), Value::from(full_name));
            }
            if let Some(Value::Bool(verified)) = details.get("verified") {
                user.insert("verified".to_owned(), Value::from(*verified));
            }
        }
        let mut profile_url = details.and_then(|details| safe_web_url(details.get("profile_url")));
        if profile_url.is_none() && from_instagram && urls::is_username(&username) {
            // No usable link: a post from Instagram (or without a link) gets the profile of the same name.
            profile_url = urls::profile_url_for(&username);
        }
        if let Some(profile_url) = profile_url {
            user.insert("profile_url".to_owned(), Value::from(profile_url));
        }
        user
    }

    fn comments(&self, value: Option<&Value>) -> Vec<Value> {
        let mut comments = Vec::new();
        let Some(Value::Array(items)) = value else {
            return comments;
        };
        for item in items {
            if comments.len() >= self.limits.max_comments_per_post {
                break;
            }
            let Value::Object(item) = item else { continue };
            let text = clean_text(item.get("text"), manifest::MAX_COMMENT_CHARS);
            if text.is_empty() {
                continue;
            }
            // v1 names the author "username"; "user" (a name or {username}) is read like the frontend parser does.
            let author = item.get("username").or_else(|| item.get("user"));
            let author = match author {
                Some(Value::Object(user)) => user.get("username"),
                other => other,
            };
            let mut comment = Map::new();
            comment.insert("username".to_owned(), Value::from(clean_username(author).unwrap_or_else(|| messages::UNKNOWN_USERNAME.to_owned())));
            comment.insert("text".to_owned(), Value::from(text));
            comment.insert("like_count".to_owned(), Value::from(clean_count(item.get("like_count")).unwrap_or(0)));
            if let Some(taken_at) = parse_timestamp(item.get("taken_at")) {
                comment.insert("taken_at".to_owned(), Value::from(taken_at));
            }
            comments.push(Value::Object(comment));
        }
        comments
    }

    fn post(&self, raw: &Value, index: usize) -> Result<NewPost, LibraryError> {
        let place = messages::commit_post_place(index + 1);
        let Value::Object(raw) = raw else {
            return Err(LibraryError::new(422, messages::commit_post_not_an_object(&place)));
        };
        let id = clean_id(raw.get("id")).ok_or_else(|| LibraryError::new(422, messages::commit_post_no_id(&place)))?;
        let media = self.media(raw, &format!("{place} ({id})"))?;
        let media_type = match raw.get("media_type").and_then(Value::as_str) {
            Some(declared @ ("video" | "carousel")) => declared.to_owned(),
            _ if media.len() > 1 => "carousel".to_owned(),
            _ => media[0].staged.format.kind.as_str().to_owned(),
        };
        let mut fields = Map::new();
        let url = safe_web_url(raw.get("url"));
        if let Some(url) = &url {
            fields.insert("url".to_owned(), Value::from(url.clone()));
        }
        fields.insert("caption".to_owned(), Value::from(clean_text(raw.get("caption"), manifest::MAX_CAPTION_CHARS)));
        fields.insert("comment_count".to_owned(), Value::from(clean_count(raw.get("comment_count")).unwrap_or(0)));
        fields.insert("like_count".to_owned(), clean_count(raw.get("like_count")).map_or(Value::Null, Value::from));
        if let Some(taken_at) = parse_timestamp(raw.get("taken_at")) {
            fields.insert("taken_at".to_owned(), Value::from(taken_at));
        }
        let alt = clean_text(raw.get("alt"), manifest::MAX_ALT_CHARS);
        if !alt.is_empty() {
            fields.insert("alt".to_owned(), Value::from(alt));
        }
        let location = clean_text(raw.get("location"), manifest::MAX_LOCATION_CHARS);
        if !location.is_empty() {
            fields.insert("location".to_owned(), Value::from(location));
        }
        let comments = self.comments(raw.get("comments"));
        if !comments.is_empty() {
            fields.insert("comments".to_owned(), Value::Array(comments));
        }
        let from_instagram = url.as_deref().is_none_or(is_instagram_url);
        Ok(NewPost { id, media, media_type, user: Self::user(raw, from_instagram), fields })
    }

    fn avatars(&self, value: Option<&Value>) -> Result<Vec<(String, StagedFile)>, LibraryError> {
        let entries = match value {
            None | Some(Value::Null) => return Ok(Vec::new()),
            Some(Value::Object(entries)) => entries,
            Some(_) => return Err(LibraryError::new(422, messages::COMMIT_AVATARS_NOT_A_MAP)),
        };
        let mut avatars: Vec<(String, StagedFile)> = Vec::new();
        for (key, name) in entries {
            let username = clean_username(Some(&Value::from(key.clone())));
            let (Some(username), Some(name)) = (username, name.as_str()) else {
                return Err(LibraryError::new(422, messages::commit_avatar_entry_invalid(&shorten(key))));
            };
            let staged = self.staged(name, &messages::commit_avatar_place(&username))?;
            if staged.format.kind != MediaKind::Image {
                return Err(LibraryError::new(422, messages::commit_avatar_not_an_image(&username)));
            }
            let folded = username.to_lowercase();
            // A later entry for the same user wins, as in a Python dict.
            avatars.retain(|(known, _)| known != &folded);
            avatars.push((folded, staged));
        }
        Ok(avatars)
    }

    fn parse(&self, body: &[u8]) -> Result<Commit, LibraryError> {
        let raw: Value = serde_json::from_slice(body).map_err(|error| LibraryError::new(422, messages::commit_not_json(&error.to_string())))?;
        let Value::Object(raw) = raw else {
            return Err(LibraryError::new(422, messages::COMMIT_NOT_AN_OBJECT));
        };
        let source_kind = match raw.get("source_kind").and_then(Value::as_str) {
            Some(kind @ ("archive" | "folder" | "multiple")) => kind.to_owned(),
            _ => return Err(LibraryError::new(422, messages::COMMIT_BAD_SOURCE_KIND)),
        };
        let posts = match raw.get("posts") {
            Some(Value::Array(posts)) if !posts.is_empty() => posts,
            _ => return Err(LibraryError::new(422, messages::COMMIT_NO_POSTS)),
        };
        if posts.len() > self.limits.max_posts_per_commit {
            return Err(LibraryError::new(422, messages::commit_too_many_posts(posts.len(), self.limits.max_posts_per_commit)));
        }
        // Later entries win on the same id; the first one keeps its position (like a JS Map).
        let mut unique: Vec<NewPost> = Vec::new();
        let mut positions: HashMap<String, usize> = HashMap::new();
        for (index, entry) in posts.iter().enumerate() {
            let post = self.post(entry, index)?;
            match positions.get(&post.id) {
                Some(position) => unique[*position] = post,
                None => {
                    positions.insert(post.id.clone(), unique.len());
                    unique.push(post);
                }
            }
        }
        let source_name = Some(clean_text(raw.get("source_name"), MAX_SOURCE_NAME_CHARS)).filter(|name| !name.is_empty()).unwrap_or_else(|| messages::IMPORT_DEFAULT_SOURCE_NAME.to_owned());
        Ok(Commit {
            source_name,
            source_kind,
            exported_at: parse_timestamp(raw.get("exported_at")),
            source_url: safe_web_url(raw.get("source_url")),
            posts: unique,
            avatars: self.avatars(raw.get("avatars"))?,
        })
    }
}

// --- Files on disk -----------------------------------------------------------------------------

/// True for a link or a junction (never followed).
fn is_link(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink())
}

/// Best effort: removes a file, folder or link (never what a link points to). What a reader
/// still holds open on Windows stays behind and is picked up by a later sweep or purge.
fn remove_tree(path: &Path) {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return;
    };
    let result = if metadata.file_type().is_symlink() {
        // A link to a folder is removed like a folder, a link to a file like a file.
        fs::remove_dir(path).or_else(|_| fs::remove_file(path))
    } else if metadata.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    };
    if let Err(error) = result {
        log::warn!("Could not remove {}: {error}", path.display());
    }
}

/// The entries of a real folder; nothing for a missing folder or a link (never followed).
fn list_folder(folder: &Path) -> Vec<fs::DirEntry> {
    if is_link(folder) {
        return Vec::new();
    }
    fs::read_dir(folder).map(|entries| entries.flatten().collect()).unwrap_or_default()
}

/// The name of a folder entry as text.
fn entry_name(entry: &fs::DirEntry) -> String {
    entry.file_name().to_string_lossy().into_owned()
}

/// True when the entry is a real folder (a link to one counts as no folder).
fn entry_is_dir(entry: &fs::DirEntry) -> bool {
    entry.file_type().is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
}

/// os.replace, retried briefly: on Windows a reader (an antivirus, an editor) can hold the target.
fn replace(source: &Path, target: &Path) -> io::Result<()> {
    store::retry_while_in_use(store::REPLACE_ATTEMPTS, &|retry| u64::from(retry) * store::REPLACE_RETRY_STEP_MS, &mut || fs::rename(source, target))
}

/// library.py `_normal_path`.
fn normal_path(value: &str) -> String {
    let mut path = value.trim().replace('\\', "/");
    while let Some(rest) = path.strip_prefix("./") {
        path = rest.to_owned();
    }
    path
}

/// library.py `_library_paths`: every file path a post references, in every spelling a v1 export
/// may use; reading all of them keeps a sweep from ever treating a referenced file as unused.
fn library_paths(post: &Map<String, Value>) -> Vec<String> {
    let mut paths = Vec::new();
    if let Some(Value::Array(media)) = post.get("media") {
        for item in media {
            match item {
                Value::String(path) => paths.push(normal_path(path)),
                Value::Object(item) => {
                    for key in ["path", "file", "src", "url", "poster", "thumbnail", "cover"] {
                        if let Some(Value::String(path)) = item.get(key) {
                            paths.push(normal_path(path));
                        }
                    }
                }
                _ => {}
            }
        }
    }
    for key in ["image", "images", "video", "videos", "poster", "thumbnail", "cover"] {
        match post.get(key) {
            Some(Value::String(path)) => paths.push(normal_path(path)),
            Some(Value::Array(items)) => paths.extend(items.iter().filter_map(Value::as_str).map(normal_path)),
            _ => {}
        }
    }
    if let Some(Value::String(avatar)) = post.get("user").and_then(|user| user.get("avatar")) {
        paths.push(normal_path(avatar));
    }
    paths
}

/// A post.json the library wrote: source.type "library" and a "library" object.
fn is_library_document(raw: &Value) -> bool {
    raw.get("library").is_some_and(Value::is_object) && raw.get("source").and_then(|source| source.get("type")).and_then(Value::as_str) == Some("library")
}

/// The stems of the files of `folder` that `posts` reference.
fn stems_of_posts(posts: &[Map<String, Value>], folder: &str, stem_of: StemOf) -> HashSet<String> {
    let mut stems = HashSet::new();
    for post in posts {
        for path in library_paths(post) {
            if let Some((directory, name)) = path.split_once('/') {
                if directory == folder {
                    if let Some(stem) = stem_of(name) {
                        stems.insert(stem.to_lowercase());
                    }
                }
            }
        }
    }
    stems
}

/// The stems of the files in `directory`.
fn stems_on_disk(directory: &Path, stem_of: StemOf) -> HashSet<String> {
    list_folder(directory).iter().filter_map(|entry| stem_of(&entry_name(entry)).map(str::to_lowercase)).collect()
}

/// The content of post.json that the library keeps.
#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    /// Bumped by every change.
    pub revision: u64,
    /// When it last changed (ISO, milliseconds, Z).
    pub updated_at: Option<String>,
    /// The import history, oldest first.
    pub imports: Vec<Map<String, Value>>,
    /// The posts, newest first.
    pub posts: Vec<Map<String, Value>>,
}

impl Document {
    fn empty(revision: u64) -> Document {
        Document { revision, updated_at: None, imports: Vec::new(), posts: Vec::new() }
    }

    /// The post.json text of the document (format v1 with the library metadata), two-space
    /// JSON and a newline.
    pub fn manifest_text(&self) -> String {
        let mut library = Map::new();
        library.insert("revision".to_owned(), Value::from(self.revision));
        library.insert("updated_at".to_owned(), self.updated_at.clone().map_or(Value::Null, Value::from));
        library.insert("imports".to_owned(), Value::Array(self.imports.iter().cloned().map(Value::Object).collect()));
        let mut source = Map::new();
        source.insert("type".to_owned(), Value::from("library"));
        let mut document = Map::new();
        document.insert("format".to_owned(), Value::from("instagram-export"));
        document.insert("version".to_owned(), Value::from(1));
        document.insert("exported_at".to_owned(), self.updated_at.clone().map_or(Value::Null, Value::from));
        document.insert("source".to_owned(), Value::Object(source));
        document.insert("library".to_owned(), Value::Object(library));
        document.insert("posts".to_owned(), Value::Array(self.posts.iter().cloned().map(Value::Object).collect()));
        format!("{}\n", serde_json::to_string_pretty(&Value::Object(document)).unwrap_or_default())
    }
}

/// library.py `_parse_document`.
fn parse_document(raw: &Value) -> Option<Document> {
    let posts = raw.get("posts")?.as_array()?;
    let meta = raw.get("library").and_then(Value::as_object);
    let revision = meta.and_then(|meta| meta.get("revision")).and_then(Value::as_u64).unwrap_or(0);
    let updated_at = meta.and_then(|meta| meta.get("updated_at")).and_then(Value::as_str).map(str::to_owned);
    let imports = meta.and_then(|meta| meta.get("imports")).and_then(Value::as_array).map(|items| items.iter().filter_map(Value::as_object).cloned().collect()).unwrap_or_default();
    Some(Document { revision, updated_at, imports, posts: posts.iter().filter_map(Value::as_object).cloned().collect() })
}

/// The import history of a library post.json (for the `imports` of the browser view); empty
/// when the file is not one the library wrote.
pub fn imports_of(manifest_path: &Path) -> Vec<Map<String, Value>> {
    let Ok(bytes) = fs::read(manifest_path) else {
        return Vec::new();
    };
    let Ok(raw) = serde_json::from_slice::<Value>(&bytes) else {
        return Vec::new();
    };
    if !is_library_document(&raw) {
        return Vec::new();
    }
    parse_document(&raw).map(|document| document.imports).unwrap_or_default()
}

/// The free space of the drive of `path` (through the archive cache's Windows call).
pub fn system_free_space(path: &Path) -> Option<u64> {
    crate::archive::cache::system_free_space(path)
}

/// library.py newest-first order of posts by their `taken_at` (undated last, stable).
fn sort_documents(posts: Vec<Map<String, Value>>) -> Vec<Map<String, Value>> {
    let time = |post: &Map<String, Value>| post.get("taken_at").and_then(Value::as_str).and_then(text::parse_timestamp_text);
    let (mut dated, undated): (Vec<_>, Vec<_>) = posts.into_iter().partition(|post| time(post).is_some());
    dated.sort_by_key(|post| std::cmp::Reverse(time(post)));
    dated.extend(undated);
    dated
}

// --- Upload sessions ---------------------------------------------------------------------------

/// The state of an upload session.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SessionState {
    Open,
    /// The body of its commit is being read (`Library::reserve_commit`): it takes no file and no
    /// second commit, and is never dropped to make room. Cancelling it is still allowed, as
    /// the clone allows it until the body is read.
    ReceivingCommit,
    Committing,
    Closed,
}

/// One upload session.
#[derive(Debug)]
struct Session {
    /// The address of the client that started it.
    client: IpAddr,
    directory: PathBuf,
    files: HashMap<String, StagedFile>,
    writing: HashSet<String>,
    /// Bytes counted against the session and folder limits (kept files + bytes being written).
    reserved: u64,
    state: SessionState,
    touched: Instant,
}

/// Every session of this process.
#[derive(Debug, Default)]
struct Sessions {
    open: HashMap<String, Session>,
    /// Closed sessions whose files are still being written: their folder goes when the last
    /// write ends.
    closing: HashMap<String, Session>,
    staged_bytes: u64,
}

impl Sessions {
    fn find(&mut self, upload_id: &str) -> Option<&mut Session> {
        if self.open.contains_key(upload_id) {
            self.open.get_mut(upload_id)
        } else {
            self.closing.get_mut(upload_id)
        }
    }

    /// Forgets a session and its staged bytes; returns its folder when it can go now (no write
    /// is running).
    fn close(&mut self, upload_id: &str) -> Option<PathBuf> {
        let mut session = self.open.remove(upload_id)?;
        self.staged_bytes = self.staged_bytes.saturating_sub(session.reserved);
        session.reserved = 0;
        session.state = SessionState::Closed;
        if session.writing.is_empty() {
            Some(session.directory)
        } else {
            self.closing.insert(upload_id.to_owned(), session);
            None
        }
    }
}

/// What the library knows about its folder (guarded by the "process lock").
#[derive(Debug, Default)]
struct FolderState {
    /// The folder once it is claimed and prepared.
    root: Option<PathBuf>,
    /// The locked marker while this library uses its folder.
    lock: Option<File>,
    /// post.json as it was last read or written, with the size and time it had.
    cached: Option<((u64, Option<SystemTime>), Document)>,
    /// Revisions only grow, also across a clear, so file URLs (?r=<revision>) never repeat.
    revision_floor: u64,
}

/// The library in one folder. The folder is claimed and prepared on first use.
#[derive(Debug)]
pub struct Library {
    folder: PathBuf,
    limits: LibraryLimits,
    free_space: FreeSpace,
    state: Mutex<FolderState>,
    sessions: Mutex<Sessions>,
    library_bytes: AtomicU64,
    /// Commits being merged now (at most `max_commits_at_once`).
    commits: Mutex<usize>,
    /// Signalled when a commit's turn ends.
    commit_turns: Condvar,
}

/// The right to receive and merge the commit of one upload session (`Library::reserve_commit`).
/// Dropped without `commit`, it gives the session back to its uploads.
#[derive(Debug)]
pub struct CommitReservation<'a> {
    library: &'a Library,
    upload_id: String,
    /// The body written so far (`receive`).
    body: Option<PathBuf>,
}

impl CommitReservation<'_> {
    /// Writes the commit body (`length` bytes) to the session's folder as it arrives. Nothing
    /// that other clients need is held meanwhile: not a turn to merge, not memory.
    pub fn receive(&mut self, body: &mut dyn Read, length: u64) -> Result<(), LibraryError> {
        let directory = {
            let mut sessions = lock(&self.library.sessions);
            let Some(session) = sessions.open.get_mut(&self.upload_id).filter(|session| session.state == SessionState::ReceivingCommit) else {
                return Err(LibraryError::new(409, messages::UPLOAD_CANCELLED));
            };
            // Like a file being uploaded: cancelling the session leaves its folder until this
            // write has ended (`end_write`).
            session.writing.insert(COMMIT_BODY_NAME.to_owned());
            session.directory.clone()
        };
        let path = directory.join(COMMIT_BODY_NAME);
        self.body = Some(path.clone());
        let written = self.library.write_commit_body(&directory, &path, body, length);
        self.library.end_write(&self.upload_id, COMMIT_BODY_NAME, 0);
        written
    }

    /// Waits for a turn (`LibraryLimits::commit_wait` at most, then 503), reads the body
    /// `receive` wrote and merges its posts into post.json; the library afterwards.
    pub fn commit(self) -> Result<Document, LibraryError> {
        let _turn = self.library.take_commit_turn()?;
        let Some(path) = &self.body else {
            return self.library.commit(&self.upload_id, b"");
        };
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if lock(&self.library.sessions).open.contains_key(&self.upload_id) => return Err(LibraryError::new(503, messages::upload_not_written(&error.to_string()))),
            // The session was cancelled meanwhile, its folder with it.
            Err(_) => return Err(LibraryError::new(404, messages::UPLOAD_UNKNOWN)),
        };
        let _ = fs::remove_file(path);
        self.library.commit(&self.upload_id, &bytes)
    }
}

impl Drop for CommitReservation<'_> {
    fn drop(&mut self) {
        if let Some(session) = lock(&self.library.sessions).open.get_mut(&self.upload_id) {
            if session.state == SessionState::ReceivingCommit {
                session.state = SessionState::Open;
                session.touched = Instant::now();
            }
        }
        // A body that was cut off, or not merged, is not kept.
        if let Some(path) = &self.body {
            let _ = fs::remove_file(path);
        }
    }
}

/// A commit's turn to be merged (`Library::take_commit_turn`); dropped, it lets the next one in.
struct CommitTurn<'a> {
    library: &'a Library,
}

impl Drop for CommitTurn<'_> {
    fn drop(&mut self) {
        let mut commits = lock(&self.library.commits);
        *commits = commits.saturating_sub(1);
        self.library.commit_turns.notify_one();
    }
}

/// True for a client on this computer (a loopback address).
fn is_this_computer(client: IpAddr) -> bool {
    crate::web::guard::is_local_peer(client)
}

/// The client a session is counted for: every program on this computer is one client, whatever
/// loopback address it connects from.
fn client_of(address: IpAddr) -> IpAddr {
    crate::web::peer_group(address)
}

/// Locks a mutex, also when another thread panicked while holding it (the data are counters and
/// maps that stay consistent between statements).
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

impl Library {
    /// The library in `folder` with the contract limits and the system's free space.
    pub fn new(folder: PathBuf) -> Library {
        Library::with_limits(folder, LibraryLimits::default(), system_free_space)
    }

    /// The library with other limits and another free-space function (tests).
    pub fn with_limits(folder: PathBuf, limits: LibraryLimits, free_space: FreeSpace) -> Library {
        let folder = std::path::absolute(&folder).unwrap_or(folder);
        Library {
            folder,
            limits,
            free_space,
            state: Mutex::new(FolderState::default()),
            sessions: Mutex::new(Sessions::default()),
            library_bytes: AtomicU64::new(0),
            commits: Mutex::new(0),
            commit_turns: Condvar::new(),
        }
    }

    /// The folder.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Its post.json.
    pub fn manifest_path(&self) -> PathBuf {
        self.folder.join(MANIFEST_NAME)
    }

    /// The limits.
    pub fn limits(&self) -> &LibraryLimits {
        &self.limits
    }

    /// Claims the folder now when this app wrote into it before (its marker or post.json is
    /// there), so that a second copy of the app sees it in use from the start. A folder that
    /// does not exist is left alone until the first import. Errors are only logged.
    pub fn claim_if_used(&self) {
        if self.folder.join(MARKER_NAME).is_file() || self.folder.join(MANIFEST_NAME).is_file() {
            let mut state = lock(&self.state);
            if let Err(error) = self.ready_root(&mut state, true) {
                log::warn!("The browser imports folder {} is not used: {}", self.folder.display(), error.detail);
            }
        }
    }

    // -- folder --------------------------------------------------------------------------------

    /// The usable folder; `LibraryError(503)` when it cannot be used.
    fn ready_root(&self, state: &mut FolderState, refuse_links: bool) -> Result<PathBuf, LibraryError> {
        let root = self.folder.clone();
        let cannot = |error: io::Error| LibraryError::new(503, messages::library_folder_unusable(&error.to_string()));
        fs::create_dir_all(&root).map_err(cannot)?;
        if refuse_links {
            for name in [POSTS_FOLDER, AVATARS_FOLDER, STAGING_FOLDER] {
                if is_link(&root.join(name)) {
                    return Err(LibraryError::new(503, messages::library_folder_link(name)));
                }
            }
        }
        if state.root.is_none() {
            self.claim(state, &root)?;
            if let Err(error) = self.prepare(state, &root) {
                state.lock = None;
                return Err(error);
            }
            state.root = Some(root.clone());
        }
        Ok(root)
    }

    /// library.py `_foreign_entry`: the first thing in a folder without the marker that the
    /// library did not create, or `None` when the library may adopt the folder.
    fn foreign_entry(root: &Path) -> Option<String> {
        let marker = root.join(MARKER_NAME);
        if is_link(&marker) {
            return Some(MARKER_NAME.to_owned());
        }
        if marker.is_file() {
            return None;
        }
        let manifest = root.join(MANIFEST_NAME);
        if manifest.exists() || is_link(&manifest) {
            let ours = fs::read(&manifest).ok().and_then(|bytes| serde_json::from_slice::<Value>(&bytes).ok()).is_some_and(|raw| is_library_document(&raw));
            return if ours { None } else { Some(MANIFEST_NAME.to_owned()) };
        }
        for entry in list_folder(root) {
            let name = entry_name(&entry);
            if name.starts_with(PROBE_PREFIX) || (name.starts_with(TEMPORARY_PREFIX) && name.ends_with(TEMPORARY_SUFFIX)) {
                continue;
            }
            if is_link(&entry.path()) {
                return Some(name);
            }
            if [POSTS_FOLDER, AVATARS_FOLDER, STAGING_FOLDER].contains(&name.as_str()) && entry_is_dir(&entry) {
                let inside: Vec<String> = list_folder(&entry.path()).iter().map(entry_name).filter(|inner| !(name == STAGING_FOLDER && is_upload_id(inner))).collect();
                match inside.first() {
                    None => continue,
                    Some(first) => return Some(format!("{name}/{first}")),
                }
            }
            return Some(name);
        }
        None
    }

    /// Takes the folder: refuses a folder holding files the library did not create and a folder
    /// another running copy uses (503), then locks the marker.
    fn claim(&self, state: &mut FolderState, root: &Path) -> Result<(), LibraryError> {
        if let Some(foreign) = Self::foreign_entry(root) {
            return Err(LibraryError::new(503, messages::library_folder_foreign(&shorten(&foreign))));
        }
        let cannot = |error: io::Error| LibraryError::new(503, messages::library_folder_unusable(&error.to_string()));
        let mut marker = OpenOptions::new().read(true).write(true).create(true).truncate(false).open(root.join(MARKER_NAME)).map_err(cannot)?;
        match marker.try_lock() {
            Ok(()) => {}
            Err(fs::TryLockError::WouldBlock) => return Err(LibraryError::new(503, messages::LIBRARY_FOLDER_IN_USE)),
            Err(fs::TryLockError::Error(error)) => return Err(cannot(error)),
        }
        if marker.metadata().map_err(cannot)?.len() == 0 {
            marker.write_all(MARKER_TEXT).map_err(cannot)?;
        }
        state.lock = Some(marker);
        Ok(())
    }

    /// First use in this process: prove the folder is writable, then tidy it up.
    fn prepare(&self, state: &mut FolderState, root: &Path) -> Result<(), LibraryError> {
        let probe = root.join(format!("{PROBE_PREFIX}{}", crate::web::random_hex(8)));
        let written = fs::write(&probe, b"").and_then(|()| fs::remove_file(&probe));
        if let Err(error) = written {
            let _ = fs::remove_file(&probe);
            return Err(LibraryError::new(503, messages::library_folder_unusable(&error.to_string())));
        }
        // Sessions live in memory only and this process holds the folder's lock: whatever is
        // staged belongs to an earlier run and can never be committed.
        for entry in list_folder(&root.join(STAGING_FOLDER)) {
            if is_upload_id(&entry_name(&entry)) {
                remove_tree(&entry.path());
            }
        }
        match self.load(state, root) {
            Ok(document) => Self::sweep(root, &document),
            Err(_) => log::warn!("The browser imports folder {} is damaged; it is left as it is.", root.display()),
        }
        self.library_bytes.store(Self::measure(root), Ordering::SeqCst);
        Ok(())
    }

    /// Drops sessions nobody sent a file to for a while (sooner when they never received one),
    /// and staging folders without a session.
    fn purge_stale_staging(&self, root: &Path) {
        let mut gone: Vec<PathBuf> = Vec::new();
        let live: HashSet<String> = {
            let mut sessions = lock(&self.sessions);
            let now = Instant::now();
            let stale: Vec<String> = sessions
                .open
                .iter()
                .filter(|(_, session)| {
                    let limit = if session.files.is_empty() { self.limits.stale_empty_session.min(self.limits.stale_staging) } else { self.limits.stale_staging };
                    session.state == SessionState::Open && session.writing.is_empty() && now.duration_since(session.touched) >= limit
                })
                .map(|(id, _)| id.clone())
                .collect();
            for id in stale {
                gone.extend(sessions.close(&id));
            }
            sessions.open.keys().chain(sessions.closing.keys()).cloned().collect()
        };
        for folder in gone {
            remove_tree(&folder);
        }
        for entry in list_folder(&root.join(STAGING_FOLDER)) {
            let name = entry_name(&entry);
            if !live.contains(&name) && is_upload_id(&name) {
                remove_tree(&entry.path());
            }
        }
    }

    /// The bytes of the files below posts/ and avatars/.
    fn measure(root: &Path) -> u64 {
        let mut total = 0;
        for folder in [POSTS_FOLDER, AVATARS_FOLDER] {
            for entry in list_folder(&root.join(folder)) {
                if let Ok(metadata) = fs::symlink_metadata(entry.path()) {
                    if metadata.is_file() {
                        total += metadata.len();
                    }
                }
            }
        }
        total
    }

    /// Deletes library files post.json no longer references (replaced or deleted posts, files a
    /// reader held open during an earlier delete, leftovers of an interrupted commit).
    fn sweep(root: &Path, document: &Document) {
        let referenced: HashSet<String> = document.posts.iter().flat_map(library_paths).map(|path| path.to_lowercase()).collect();
        let folders: [(&str, StemOf); 2] = [(POSTS_FOLDER, post_file_stem), (AVATARS_FOLDER, avatar_file_stem)];
        for (folder, stem_of) in folders {
            for entry in list_folder(&root.join(folder)) {
                let name = entry_name(&entry);
                if stem_of(&name).is_none() || entry_is_dir(&entry) {
                    continue;
                }
                if !referenced.contains(&format!("{folder}/{name}").to_lowercase()) {
                    remove_tree(&entry.path());
                }
            }
        }
        for entry in list_folder(root) {
            let name = entry_name(&entry);
            if name.starts_with(TEMPORARY_PREFIX) && name.ends_with(TEMPORARY_SUFFIX) {
                remove_tree(&entry.path());
            }
        }
    }

    // -- post.json -----------------------------------------------------------------------------

    fn load(&self, state: &mut FolderState, root: &Path) -> Result<Document, LibraryError> {
        let path = root.join(MANIFEST_NAME);
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Document::empty(state.revision_floor)),
            Err(error) => return Err(LibraryError::new(503, messages::library_file_damaged(&error.to_string()))),
        };
        let signature = (metadata.len(), metadata.modified().ok());
        if let Some((known, document)) = &state.cached {
            if *known == signature {
                return Ok(document.clone());
            }
        }
        let bytes = fs::read(&path).map_err(|error| LibraryError::new(503, messages::library_file_damaged(&error.to_string())))?;
        let raw: Value = serde_json::from_slice(&bytes).map_err(|error| LibraryError::new(503, messages::library_file_damaged(&error.to_string())))?;
        let document = parse_document(&raw).ok_or_else(|| LibraryError::new(503, messages::LIBRARY_FILE_DAMAGED_SHAPE))?;
        state.cached = Some((signature, document.clone()));
        Ok(document)
    }

    fn save(state: &mut FolderState, root: &Path, document: &Document) -> io::Result<()> {
        let path = root.join(MANIFEST_NAME);
        store::atomic_write(&path, document.manifest_text().as_bytes(), store::WriteMode::Replace)?;
        let metadata = fs::metadata(&path)?;
        state.cached = Some(((metadata.len(), metadata.modified().ok()), document.clone()));
        Ok(())
    }

    fn next_revision(state: &FolderState, document: &Document) -> u64 {
        document.revision.max(state.revision_floor) + 1
    }

    // -- public API ----------------------------------------------------------------------------

    /// Starts an upload session for a client on this computer; its 32-hex id.
    pub fn create_upload(&self) -> Result<String, LibraryError> {
        self.create_upload_from(IpAddr::V4(Ipv4Addr::LOCALHOST))
    }

    /// Starts an upload session for the client at `client`; its 32-hex id.
    pub fn create_upload_from(&self, client: IpAddr) -> Result<String, LibraryError> {
        let client = client_of(client);
        let mut state = lock(&self.state);
        let root = self.ready_root(&mut state, true)?;
        self.purge_stale_staging(&root);
        self.make_room_for_a_session(client)?;
        let upload_id = crate::web::random_hex(16);
        let directory = root.join(STAGING_FOLDER).join(&upload_id);
        fs::create_dir_all(&directory).map_err(|error| LibraryError::new(503, messages::upload_not_started(&error.to_string())))?;
        let session = Session { client, directory, files: HashMap::new(), writing: HashSet::new(), reserved: 0, state: SessionState::Open, touched: Instant::now() };
        lock(&self.sessions).open.insert(upload_id.clone(), session);
        Ok(upload_id)
    }

    /// Takes the commit of `upload_id` before its body (up to 32 MiB) is read: 404 when there is
    /// no such session, 409 while its files are still coming or another commit of it is being
    /// received or saved. Every connection could otherwise receive a whole body for the same
    /// session at once.
    pub fn reserve_commit(&self, upload_id: &str) -> Result<CommitReservation<'_>, LibraryError> {
        let mut sessions = lock(&self.sessions);
        let session = if is_upload_id(upload_id) { sessions.open.get_mut(upload_id) } else { None };
        let Some(session) = session else {
            return Err(LibraryError::new(404, messages::UPLOAD_UNKNOWN));
        };
        if session.state != SessionState::Open {
            return Err(LibraryError::new(409, messages::UPLOAD_ALREADY_BEING_SAVED));
        }
        if !session.writing.is_empty() {
            return Err(LibraryError::new(409, messages::UPLOAD_STILL_SENDING));
        }
        session.state = SessionState::ReceivingCommit;
        session.touched = Instant::now();
        Ok(CommitReservation { library: self, upload_id: upload_id.to_owned(), body: None })
    }

    /// Writes `length` bytes of a commit body to `path` in `directory` (`CommitReservation::receive`).
    fn write_commit_body(&self, directory: &Path, path: &Path, body: &mut dyn Read, length: u64) -> Result<(), LibraryError> {
        let write_error = |error: io::Error| LibraryError::new(503, messages::upload_not_written(&error.to_string()));
        self.check_free_space(directory, length)?;
        let mut file = File::create(path).map_err(write_error)?;
        let mut chunk = vec![0_u8; 64 * 1024];
        let mut left = length;
        while left > 0 {
            let wanted = chunk.len().min(usize::try_from(left).unwrap_or(usize::MAX));
            let count = match body.read(&mut chunk[..wanted]) {
                Ok(0) => return Err(LibraryError::new(400, messages::BODY_CUT_OFF)),
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::ConnectionAborted => return Err(LibraryError::new(409, messages::UPLOAD_CANCELLED)),
                Err(_) => return Err(LibraryError::new(400, messages::BODY_CUT_OFF)),
            };
            file.write_all(&chunk[..count]).map_err(write_error)?;
            left -= count as u64;
        }
        Ok(())
    }

    /// Waits for a turn to merge a commit: at most `max_commits_at_once` bodies are in memory at
    /// once. 503 when none came within `LibraryLimits::commit_wait`.
    fn take_commit_turn(&self) -> Result<CommitTurn<'_>, LibraryError> {
        let deadline = Instant::now() + self.limits.commit_wait;
        let mut commits = lock(&self.commits);
        while *commits >= self.limits.max_commits_at_once {
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(LibraryError::new(503, messages::TOO_MANY_COMMITS));
            }
            commits = match self.commit_turns.wait_timeout(commits, left) {
                Ok((guard, _)) => guard,
                Err(poisoned) => poisoned.into_inner().0,
            };
        }
        *commits += 1;
        Ok(CommitTurn { library: self })
    }

    /// Streams one upload body to disk, checking its type and every size limit on the way.
    pub fn receive_file(&self, upload_id: &str, name: &str, body: &mut dyn Read, content_length: Option<u64>) -> Result<StagedFile, LibraryError> {
        if !is_staged_name(name) {
            return Err(LibraryError::new(422, messages::upload_bad_file_name(&shorten(name))));
        }
        let largest = self.limits.max_image_bytes.max(self.limits.max_video_bytes);
        if content_length.is_some_and(|length| length > largest) {
            return Err(LibraryError::new(413, messages::upload_file_too_large(largest)));
        }
        let directory = self.start_write(upload_id, name)?;
        let part = directory.join(format!("{name}.part"));
        let mut owned: u64 = 0;
        let result = self.write_upload(upload_id, name, body, content_length, &directory, &part, &mut owned);
        let _ = fs::remove_file(&part);
        self.end_write(upload_id, name, owned);
        result
    }

    /// The body of `receive_file`; `owned` counts the bytes reserved for the file, which
    /// `end_write` releases again unless the file was kept.
    #[allow(clippy::too_many_arguments)]
    fn write_upload(&self, upload_id: &str, name: &str, body: &mut dyn Read, content_length: Option<u64>, directory: &Path, part: &Path, owned: &mut u64) -> Result<StagedFile, LibraryError> {
        let write_error = |error: io::Error| LibraryError::new(503, messages::upload_not_written(&error.to_string()));
        self.check_free_space(directory, content_length.unwrap_or(0))?;
        let mut file = File::create(part).map_err(write_error)?;
        let mut head: Vec<u8> = Vec::new();
        let mut format: Option<StoredFormat> = None;
        let mut pending: Vec<u8> = Vec::new();
        let mut checked: u64 = 0;
        let mut chunk = vec![0_u8; 64 * 1024];
        loop {
            let count = match body.read(&mut chunk) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::ConnectionAborted => return Err(LibraryError::new(409, messages::UPLOAD_CANCELLED)),
                Err(_) => return Err(LibraryError::new(400, messages::UPLOAD_CUT_OFF)),
            };
            let mut piece = chunk[..count].to_vec();
            let found = match format {
                Some(found) => found,
                None => {
                    head.extend_from_slice(&piece);
                    if head.len() < SNIFF_BYTES {
                        continue;
                    }
                    let found = Self::accept(&head)?;
                    format = Some(found);
                    piece = std::mem::take(&mut head);
                    found
                }
            };
            self.reserve(upload_id, found, *owned, piece.len() as u64)?;
            *owned += piece.len() as u64;
            pending.extend_from_slice(&piece);
            if pending.len() >= WRITE_BUFFER_BYTES {
                if *owned - checked >= FREE_SPACE_CHECK_BYTES {
                    self.check_free_space(directory, pending.len() as u64)?;
                    checked = *owned;
                }
                file.write_all(&pending).map_err(write_error)?;
                pending.clear();
            }
        }
        let found = match format {
            Some(found) => found,
            None => {
                let found = Self::accept(&head)?;
                self.reserve(upload_id, found, *owned, head.len() as u64)?;
                *owned += head.len() as u64;
                pending.extend_from_slice(&head);
                found
            }
        };
        if !pending.is_empty() {
            file.write_all(&pending).map_err(write_error)?;
        }
        file.sync_all().map_err(write_error)?;
        drop(file);
        replace(part, &directory.join(name)).map_err(write_error)?;
        let staged = StagedFile { name: name.to_owned(), size: *owned, format: found };
        self.keep(upload_id, staged.clone())?;
        *owned = 0;
        Ok(staged)
    }

    /// Cancels an upload session and removes its files; 409 while it is being committed.
    pub fn abort_upload(&self, upload_id: &str) -> Result<(), LibraryError> {
        if !is_upload_id(upload_id) {
            return Ok(());
        }
        let folder = {
            let mut sessions = lock(&self.sessions);
            match sessions.open.get(upload_id) {
                Some(session) if session.state == SessionState::Committing => return Err(LibraryError::new(409, messages::UPLOAD_BEING_SAVED)),
                Some(_) => Some(sessions.close(upload_id)),
                None => None,
            }
        };
        match folder {
            Some(Some(folder)) => remove_tree(&folder),
            Some(None) => {}
            None => {
                // Not known to this process (a previous run): still remove what it left behind.
                if let Some(root) = lock(&self.state).root.clone() {
                    remove_tree(&root.join(STAGING_FOLDER).join(upload_id));
                }
            }
        }
        Ok(())
    }

    /// Merges the posts of a commit into post.json; the library afterwards (through
    /// `CommitReservation::commit`).
    fn commit(&self, upload_id: &str, body: &[u8]) -> Result<Document, LibraryError> {
        let mut state = lock(&self.state);
        let root = self.ready_root(&mut state, true)?;
        let document = self.load(&mut state, &root)?;
        let files = self.begin_commit(upload_id)?;
        let commit = match (CommitParser { files: &files, limits: &self.limits }).parse(body) {
            Ok(commit) => commit,
            Err(error) => {
                if let Some(session) = lock(&self.sessions).open.get_mut(upload_id) {
                    session.state = SessionState::Open;
                }
                return Err(error);
            }
        };
        let directory = lock(&self.sessions).open.get(upload_id).map(|session| session.directory.clone()).unwrap_or_default();
        let applied = self.apply(&mut state, &root, upload_id, &directory, &document, commit);
        self.close_session(upload_id);
        let updated = applied.map_err(|error| LibraryError::new(503, messages::import_not_saved(&error.to_string())))?;
        Self::sweep(&root, &updated);
        self.library_bytes.store(Self::measure(&root), Ordering::SeqCst);
        Ok(updated)
    }

    /// Removes one post; 404 for an unknown id.
    pub fn delete_post(&self, post_id: &str) -> Result<Document, LibraryError> {
        let mut state = lock(&self.state);
        let root = self.ready_root(&mut state, true)?;
        let document = self.load(&mut state, &root)?;
        let posts: Vec<Map<String, Value>> = document.posts.iter().filter(|post| post.get("id").and_then(Value::as_str) != Some(post_id)).cloned().collect();
        if posts.len() == document.posts.len() {
            return Err(LibraryError::new(404, messages::LIBRARY_NO_SUCH_POST));
        }
        let updated = Document { revision: Self::next_revision(&state, &document), updated_at: Some(now_iso()), imports: document.imports.clone(), posts };
        Self::save(&mut state, &root, &updated).map_err(|error| LibraryError::new(503, messages::library_file_not_updated(&error.to_string())))?;
        Self::sweep(&root, &updated);
        self.library_bytes.store(Self::measure(&root), Ordering::SeqCst);
        Ok(updated)
    }

    /// Removes what the library wrote (post.json, its media and avatar files, staged uploads)
    /// and nothing else; posts/, avatars/ and .staging/ go only once they are empty.
    pub fn clear(&self) -> Result<(), LibraryError> {
        let mut state = lock(&self.state);
        let root = self.ready_root(&mut state, false)?;
        let revision = self.load(&mut state, &root).map(|document| document.revision).unwrap_or(state.revision_floor);
        self.cancel_uploads();
        match fs::remove_file(root.join(MANIFEST_NAME)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(LibraryError::new(503, messages::library_file_not_removed(&error.to_string()))),
        }
        state.cached = None;
        state.revision_floor = revision.max(state.revision_floor) + 1;
        // Files a reader still holds open stay behind; the next sweep removes them.
        let folders: [(&str, NameTest); 3] =[(POSTS_FOLDER, |name| post_file_stem(name).is_some()), (AVATARS_FOLDER, |name| avatar_file_stem(name).is_some()), (STAGING_FOLDER, is_upload_id)];
        for (folder, matches) in folders {
            let directory = root.join(folder);
            if is_link(&directory) {
                // The link only, never its target.
                remove_tree(&directory);
                continue;
            }
            for entry in list_folder(&directory) {
                if matches(&entry_name(&entry)) && (folder == STAGING_FOLDER || !entry_is_dir(&entry)) {
                    remove_tree(&entry.path());
                }
            }
            let _ = fs::remove_dir(&directory);
        }
        Self::sweep(&root, &Document::empty(state.revision_floor));
        self.library_bytes.store(Self::measure(&root), Ordering::SeqCst);
        Ok(())
    }

    /// Ends every upload session and removes its staged files (a server that restarts on other
    /// ports, or stops); uploads that are being received end with 409 at their next piece.
    pub fn cancel_uploads(&self) {
        let folders: Vec<PathBuf> = {
            let mut sessions = lock(&self.sessions);
            let ids: Vec<String> = sessions.open.keys().cloned().collect();
            ids.iter().filter_map(|id| sessions.close(id)).collect()
        };
        for folder in folders {
            remove_tree(&folder);
        }
    }

    /// Ends this library's use of its folder: drops open uploads and unlocks the folder.
    pub fn close(&self) {
        let mut state = lock(&self.state);
        self.cancel_uploads();
        if let Some(marker) = state.lock.take() {
            let _ = marker.unlock();
        }
        state.root = None;
        state.cached = None;
    }

    // -- sessions ------------------------------------------------------------------------------

    fn start_write(&self, upload_id: &str, name: &str) -> Result<PathBuf, LibraryError> {
        let mut sessions = lock(&self.sessions);
        let max_files = self.limits.max_files_per_session;
        let session = if is_upload_id(upload_id) { sessions.open.get_mut(upload_id) } else { None };
        let Some(session) = session else {
            return Err(LibraryError::new(404, messages::UPLOAD_UNKNOWN));
        };
        if session.state != SessionState::Open {
            return Err(LibraryError::new(409, messages::UPLOAD_SAVING_NO_MORE_FILES));
        }
        if session.writing.contains(name) {
            return Err(LibraryError::new(409, messages::upload_already_being_sent(name)));
        }
        if !session.files.contains_key(name) && session.files.len() + session.writing.len() >= max_files {
            return Err(LibraryError::new(413, messages::upload_too_many_files(max_files)));
        }
        session.writing.insert(name.to_owned());
        session.touched = Instant::now();
        Ok(session.directory.clone())
    }

    /// library.py `_make_room_for_a_session`, per client: when the folder or `client` has no
    /// session to spare, the longest idle session of `client` itself is dropped. Another
    /// client's session never is: with LAN on, one device could otherwise cancel the import of
    /// another one by starting new ones. Devices on the network together never hold the
    /// sessions kept for this computer, so the user's own browser can always import.
    fn make_room_for_a_session(&self, client: IpAddr) -> Result<(), LibraryError> {
        let folder = {
            let mut sessions = lock(&self.sessions);
            let of_client = sessions.open.values().filter(|session| session.client == client).count();
            let of_network = sessions.open.values().filter(|session| !is_this_computer(session.client)).count();
            let network_room = is_this_computer(client) || of_network < self.limits.max_open_sessions.saturating_sub(self.limits.sessions_kept_for_this_computer);
            if sessions.open.len() < self.limits.max_open_sessions && of_client < self.limits.max_sessions_per_client && network_room {
                return Ok(());
            }
            let idle = sessions
                .open
                .iter()
                .filter(|(_, session)| session.client == client && session.state == SessionState::Open && session.writing.is_empty())
                .min_by_key(|(_, session)| session.touched)
                .map(|(id, _)| id.clone());
            let Some(idle) = idle else {
                return Err(LibraryError::new(429, messages::TOO_MANY_IMPORTS));
            };
            sessions.close(&idle)
        };
        if let Some(folder) = folder {
            remove_tree(&folder);
        }
        Ok(())
    }

    fn check_free_space(&self, directory: &Path, upcoming: u64) -> Result<(), LibraryError> {
        if let Some(free) = (self.free_space)(directory) {
            if free.saturating_sub(upcoming) < self.limits.min_free_bytes || free < upcoming {
                return Err(LibraryError::new(507, messages::library_no_space(free, self.limits.min_free_bytes)));
            }
        }
        Ok(())
    }

    fn accept(head: &[u8]) -> Result<StoredFormat, LibraryError> {
        stored_format(head).ok_or_else(|| LibraryError::new(415, messages::UPLOAD_TYPE_NOT_STORED))
    }

    fn reserve(&self, upload_id: &str, format: StoredFormat, already: u64, extra: u64) -> Result<(), LibraryError> {
        let library_bytes = self.library_bytes.load(Ordering::SeqCst);
        let mut sessions = lock(&self.sessions);
        let staged_bytes = sessions.staged_bytes;
        let limit = if format.kind == MediaKind::Image { self.limits.max_image_bytes } else { self.limits.max_video_bytes };
        let Some(session) = sessions.find(upload_id) else {
            return Err(LibraryError::new(409, messages::UPLOAD_CANCELLED));
        };
        if session.state != SessionState::Open {
            return Err(LibraryError::new(409, messages::UPLOAD_CANCELLED));
        }
        if already + extra > limit {
            return Err(LibraryError::new(413, messages::upload_single_too_large(format.kind.as_str(), limit)));
        }
        if session.reserved + extra > self.limits.max_session_bytes {
            return Err(LibraryError::new(413, messages::upload_session_too_large(self.limits.max_session_bytes)));
        }
        if library_bytes + staged_bytes + extra > self.limits.max_cache_bytes {
            return Err(LibraryError::new(413, messages::library_full(self.limits.max_cache_bytes)));
        }
        session.reserved += extra;
        session.touched = Instant::now();
        sessions.staged_bytes += extra;
        Ok(())
    }

    fn keep(&self, upload_id: &str, staged: StagedFile) -> Result<(), LibraryError> {
        let mut sessions = lock(&self.sessions);
        let mut released = 0;
        {
            let Some(session) = sessions.find(upload_id) else {
                return Err(LibraryError::new(409, messages::UPLOAD_CANCELLED));
            };
            if session.state != SessionState::Open {
                return Err(LibraryError::new(409, messages::UPLOAD_CANCELLED));
            }
            if let Some(previous) = session.files.insert(staged.name.clone(), staged) {
                session.reserved = session.reserved.saturating_sub(previous.size);
                released = previous.size;
            }
        }
        sessions.staged_bytes = sessions.staged_bytes.saturating_sub(released);
        Ok(())
    }

    fn end_write(&self, upload_id: &str, name: &str, unused: u64) {
        let orphan = {
            let mut sessions = lock(&self.sessions);
            let mut released = 0;
            let mut orphaned = false;
            if let Some(session) = sessions.find(upload_id) {
                session.writing.remove(name);
                if session.state != SessionState::Closed && unused > 0 {
                    session.reserved = session.reserved.saturating_sub(unused);
                    released = unused;
                }
                orphaned = session.state == SessionState::Closed && session.writing.is_empty();
            }
            sessions.staged_bytes = sessions.staged_bytes.saturating_sub(released);
            if orphaned {
                sessions.closing.remove(upload_id).map(|session| session.directory)
            } else {
                None
            }
        };
        if let Some(folder) = orphan {
            remove_tree(&folder);
        }
    }

    fn begin_commit(&self, upload_id: &str) -> Result<HashMap<String, StagedFile>, LibraryError> {
        let mut sessions = lock(&self.sessions);
        let session = if is_upload_id(upload_id) { sessions.open.get_mut(upload_id) } else { None };
        let Some(session) = session else {
            return Err(LibraryError::new(404, messages::UPLOAD_UNKNOWN));
        };
        if !matches!(session.state, SessionState::Open | SessionState::ReceivingCommit) {
            return Err(LibraryError::new(409, messages::UPLOAD_ALREADY_BEING_SAVED));
        }
        if !session.writing.is_empty() {
            return Err(LibraryError::new(409, messages::UPLOAD_STILL_SENDING));
        }
        session.state = SessionState::Committing;
        Ok(session.files.clone())
    }

    fn close_session(&self, upload_id: &str) {
        let folder = lock(&self.sessions).close(upload_id);
        if let Some(folder) = folder {
            remove_tree(&folder);
        }
    }

    // -- merging -------------------------------------------------------------------------------

    /// Moves the staged files to stable names and writes the merged post.json.
    fn apply(&self, state: &mut FolderState, root: &Path, upload_id: &str, staging: &Path, document: &Document, commit: Commit) -> io::Result<Document> {
        let posts_directory = root.join(POSTS_FOLDER);
        let avatars_directory = root.join(AVATARS_FOLDER);
        fs::create_dir_all(&posts_directory)?;
        fs::create_dir_all(&avatars_directory)?;
        // A name is free when neither post.json nor the disk uses it (the old files of a replaced
        // post stay until the new post.json is in place).
        let mut post_stems = stems_of_posts(&document.posts, POSTS_FOLDER, post_file_stem);
        post_stems.extend(stems_on_disk(&posts_directory, post_file_stem));
        let mut avatar_stems = stems_of_posts(&document.posts, AVATARS_FOLDER, avatar_file_stem);
        avatar_stems.extend(stems_on_disk(&avatars_directory, avatar_file_stem));
        let mut moves: Vec<(StagedFile, String)> = Vec::new();

        let mut avatar_by_user: HashMap<String, String> = HashMap::new();
        for post in &document.posts {
            if let Some(Value::Object(user)) = post.get("user") {
                if let Some(Value::String(avatar)) = user.get("avatar") {
                    let name = user.get("username").and_then(Value::as_str).unwrap_or_default().to_lowercase();
                    avatar_by_user.entry(name).or_insert_with(|| avatar.clone());
                }
            }
        }
        let mut new_avatars: HashMap<String, String> = HashMap::new();
        for (username, staged) in &commit.avatars {
            let stem = unique_stem(&safe_stem(username, "user"), &mut avatar_stems);
            let path = format!("{AVATARS_FOLDER}/{stem}.{}", staged.format.extension);
            moves.push((staged.clone(), path.clone()));
            new_avatars.insert(username.clone(), path);
        }
        avatar_by_user.extend(new_avatars.clone());

        let mut entries: Vec<Map<String, Value>> = Vec::new();
        for new_post in &commit.posts {
            let stem = unique_stem(&safe_stem(&new_post.id, "post"), &mut post_stems);
            let mut media: Vec<Value> = Vec::new();
            let mut pictures: Vec<String> = Vec::new();
            for (number, item) in new_post.media.iter().enumerate() {
                let number = number + 1;
                let path = format!("{POSTS_FOLDER}/{stem}_{number}.{}", item.staged.format.extension);
                moves.push((item.staged.clone(), path.clone()));
                let mut described = Map::new();
                described.insert("type".to_owned(), Value::from(item.staged.format.kind.as_str()));
                described.insert("path".to_owned(), Value::from(path.clone()));
                let mut picture = (item.staged.format.kind == MediaKind::Image).then(|| path.clone());
                if let Some(poster) = &item.poster {
                    let poster_path = format!("{POSTS_FOLDER}/{stem}_{number}_poster.{}", poster.format.extension);
                    moves.push((poster.clone(), poster_path.clone()));
                    described.insert("poster".to_owned(), Value::from(poster_path.clone()));
                    picture = Some(poster_path);
                }
                if let Some(alt) = &item.alt {
                    described.insert("alt".to_owned(), Value::from(alt.clone()));
                }
                described.insert("mime".to_owned(), Value::from(item.staged.format.mime));
                described.insert("bytes".to_owned(), Value::from(item.staged.size));
                media.push(Value::Object(described));
                pictures.extend(picture);
            }
            let mut user = new_post.user.clone();
            let username = user.get("username").and_then(Value::as_str).unwrap_or_default().to_lowercase();
            if let Some(avatar) = avatar_by_user.get(&username) {
                user.insert("avatar".to_owned(), Value::from(avatar.clone()));
            }
            let mut entry = Map::new();
            entry.insert("id".to_owned(), Value::from(new_post.id.clone()));
            if let Some(url) = new_post.fields.get("url") {
                entry.insert("url".to_owned(), url.clone());
            }
            entry.insert("media_type".to_owned(), Value::from(new_post.media_type.clone()));
            entry.insert("media".to_owned(), Value::Array(media));
            if let Some(first) = pictures.first() {
                entry.insert("image".to_owned(), Value::from(first.clone()));
                entry.insert("images".to_owned(), Value::Array(pictures.iter().cloned().map(Value::from).collect()));
            }
            entry.insert("user".to_owned(), Value::Object(user));
            for (key, value) in &new_post.fields {
                if key != "url" {
                    entry.insert(key.clone(), value.clone());
                }
            }
            entries.push(entry);
        }

        let mut merged: Vec<Map<String, Value>> = document.posts.iter().map(|post| with_avatar(post, &new_avatars)).collect();
        let mut positions: HashMap<String, usize> = merged.iter().enumerate().filter_map(|(index, post)| post.get("id").and_then(Value::as_str).map(|id| (id.to_owned(), index))).collect();
        let (mut added, mut replaced) = (0_u64, 0_u64);
        let count = entries.len();
        for entry in entries {
            let id = entry.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
            match positions.get(&id) {
                Some(index) => {
                    merged[*index] = entry;
                    replaced += 1;
                }
                None => {
                    positions.insert(id, merged.len());
                    merged.push(entry);
                    added += 1;
                }
            }
        }

        let now = now_iso();
        let mut record = Map::new();
        record.insert("id".to_owned(), Value::from(upload_id));
        record.insert("source_name".to_owned(), Value::from(commit.source_name.clone()));
        record.insert("source_kind".to_owned(), Value::from(commit.source_kind.clone()));
        record.insert("imported_at".to_owned(), Value::from(now.clone()));
        record.insert("posts".to_owned(), Value::from(count));
        record.insert("new".to_owned(), Value::from(added));
        record.insert("updated".to_owned(), Value::from(replaced));
        if let Some(exported_at) = &commit.exported_at {
            record.insert("exported_at".to_owned(), Value::from(exported_at.clone()));
        }
        if let Some(source_url) = &commit.source_url {
            record.insert("source_url".to_owned(), Value::from(source_url.clone()));
        }
        let mut imports = document.imports.clone();
        imports.push(record);
        let keep_from = imports.len().saturating_sub(self.limits.max_imports_history);
        let updated = Document { revision: Self::next_revision(state, document), updated_at: Some(now), imports: imports.split_off(keep_from), posts: sort_documents(merged) };

        let mut placed: HashMap<String, PathBuf> = HashMap::new();
        let mut created: Vec<PathBuf> = Vec::new();
        let result = (|| -> io::Result<()> {
            for (staged, path) in &moves {
                let target = root.join(path);
                match placed.get(&staged.name) {
                    None => {
                        replace(&staging.join(&staged.name), &target)?;
                        placed.insert(staged.name.clone(), target.clone());
                    }
                    Some(first) => {
                        fs::copy(first, &target)?;
                    }
                }
                created.push(target);
            }
            Self::save(state, root, &updated)
        })();
        if let Err(error) = result {
            for target in created {
                let _ = fs::remove_file(target);
            }
            return Err(error);
        }
        Ok(updated)
    }
}

/// A library post whose user just got a new avatar points at it.
fn with_avatar(post: &Map<String, Value>, avatars: &HashMap<String, String>) -> Map<String, Value> {
    let Some(Value::Object(user)) = post.get("user") else {
        return post.clone();
    };
    let name = user.get("username").and_then(Value::as_str).unwrap_or_default().to_lowercase();
    match avatars.get(&name) {
        Some(avatar) => {
            let mut user = user.clone();
            user.insert("avatar".to_owned(), Value::from(avatar.clone()));
            let mut post = post.clone();
            post.insert("user".to_owned(), Value::Object(user));
            post
        }
        None => post.clone(),
    }
}

/// Now as ISO 8601 with milliseconds and Z.
fn now_iso() -> String {
    text::iso_time_millis(text::now_ms()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_follow_the_patterns_of_library_py() {
        assert!(is_upload_id("0123456789abcdef0123456789abcdef"));
        assert!(!is_upload_id("0123456789ABCDEF0123456789abcdef"));
        assert!(!is_upload_id("../x"));
        assert!(is_staged_name("m000001.jpg") && is_staged_name("m123456.webm"));
        assert!(!is_staged_name("m00001.jpg") && !is_staged_name("m000001.j") && !is_staged_name("m000001.JPG") && !is_staged_name("../m000001.jpg"));
        for name in ["m000001.png", "m000001.gif", "m000001.webp", "m000001.avif", "m000001.bmp", "m000001.mp4", "m000001.mov"] {
            assert!(is_staged_name(name), "{name}");
        }
        // Only the extensions the frontend names its uploads with: never one Windows would run.
        for name in ["m000001.bat", "m000002.hta", "m000003.cmd", "m000004.scr", "m000005.exe", "m000006.jpeg", "m000007.heic", "m000008.js"] {
            assert!(!is_staged_name(name), "{name}");
        }
        assert_eq!(post_file_stem("FXTbasic001_1.jpg"), Some("FXTbasic001"));
        assert_eq!(post_file_stem("a_b_12_poster.png"), Some("a_b"));
        assert_eq!(post_file_stem("a_1_2.mp4"), Some("a_1"));
        assert_eq!(post_file_stem("post2_1.webm"), Some("post2"));
        assert_eq!(post_file_stem("a_1234.jpg"), None);
        assert_eq!(post_file_stem("a_poster.jpg"), None);
        assert_eq!(post_file_stem("a_1.heic"), None);
        assert_eq!(post_file_stem("a b_1.jpg"), None);
        assert_eq!(avatar_file_stem("lumen.atlas.jpg"), None);
        assert_eq!(avatar_file_stem("lumen_atlas.jpg"), Some("lumen_atlas"));
        assert_eq!(safe_stem("lumen.atlas", "user"), "lumen_atlas");
        assert_eq!(safe_stem("###", "post"), "post");
        assert_eq!(safe_stem("CON", "post"), "CON_");
        assert_eq!(safe_stem(&"x".repeat(80), "post").len(), MAX_STEM_CHARS - 1);
        let mut taken = HashSet::new();
        assert_eq!(unique_stem("a", &mut taken), "a");
        assert_eq!(unique_stem("A", &mut taken), "A-2");
        assert_eq!(unique_stem("a", &mut taken), "a-3");
    }

    #[test]
    fn post_edits_and_commit_parts_are_cleaned_like_the_clone() {
        assert_eq!(clean_count(Some(&Value::from(12.5))), Some(13));
        assert_eq!(clean_count(Some(&Value::from(-1))), None);
        assert_eq!(clean_count(Some(&Value::Bool(true))), None);
        assert_eq!(clean_id(Some(&Value::from("weird id/x"))), Some("weird_id_x".to_owned()));
        assert_eq!(clean_id(Some(&Value::from(true))), None);
        assert_eq!(clean_text(Some(&Value::from("  a\0b  ")), 10), "ab");
        assert_eq!(parse_timestamp(Some(&Value::from(1_793_545_445))), Some("2026-11-01T15:04:05.000Z".to_owned()));
        assert_eq!(safe_web_url(Some(&Value::from("javascript:alert(1)"))), None);
        assert_eq!(shorten(&"y".repeat(100)).chars().count(), 80);
    }
}
