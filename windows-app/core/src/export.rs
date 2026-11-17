//! Writes posts out as an "instagram-export" v1 archive: the layout the reference web project
//! imports, so a download can be imported there again (frontend/src/import/export.ts,
//! instagram.md sections 1, 7 and 8; spec-export A and G).
//!
//! ```text
//! post.json                    the posts in canonical form, each with its description
//! posts/post<N>.<ext>          the picture or video of a single-media post
//! posts/post<N>_<k>.<ext>      slide k of a carousel
//! posts/<slide>_poster.<ext>   the poster image of a video slide
//! avatars/<username>.<ext>     profile pictures, one per username per archive
//! ```
//!
//! N is the running number of the posts actually written (across all parts), the extension
//! comes from the file's first bytes (`sniff`). The bytes on disk are exported as they are,
//! never decoded pixels: a HEIC stays a HEIC, an EXIF-rotated JPEG stays as it is. Pictures and
//! videos are stored (they do not shrink), post.json is deflated and added last. A large export
//! is split into parts (`<name>-part1.zip`, `<name>-part2.zip`, ...), each a complete export
//! with its own post.json and the avatars its posts need.
//!
//! Natively the archive is a file: every part is streamed to a temporary file in the target
//! folder in chunks (a file is never held in memory) and renamed when the whole export is
//! complete, so a cancelled or failed export leaves nothing behind. The names the parts are
//! renamed to derive from the name the user chose in the Save dialog, which has already asked
//! about replacing a file; existing files of those names are replaced.
//!
//! Acceptance: the extracted archive passes the clone's validator
//! (core/tests/validator/validate-export.mjs) with exit code 0 and no ERROR; for loose pictures
//! the only WARN is the one about the missing "url".

use std::collections::{HashMap, HashSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;
use zip::result::ZipError;
use zip::write::SimpleFileOptions;
use zip::{CompressionMethod, DateTime, ZipWriter};

use crate::manifest::MAX_POSTS;
use crate::messages;
use crate::model::{MediaKind, MediaType, Post, PostOrigin};
use crate::poster::{NoPosters, PosterSource};
use crate::sniff;
use crate::store;
use crate::text::{self, CivilTime};
use crate::urls;

const MIB: u64 = 1024 * 1024;
const GIB: u64 = 1024 * MIB;

/// Media bytes per archive before the export is split: well below the 4 GB a plain ZIP can
/// address, and half of what the clone takes in one import.
pub const EXPORT_PART_BYTES: u64 = 2 * GIB;
/// Posts per archive: the clone imports at most this many from one archive.
pub const EXPORT_PART_POSTS: usize = MAX_POSTS;
/// A post larger than this cannot go into a plain ZIP at all (room left for headers and
/// post.json): 4 GiB - 64 MiB = 4,227,858,432 bytes. It is left out with a problem sentence.
pub const MAX_POST_BYTES: u64 = 4 * GIB - 64 * MIB;
/// Bytes read from a file and handed to the archive at a time; cancellation is checked between
/// two chunks.
pub const READ_CHUNK_BYTES: usize = 4 * 1024 * 1024;
/// The archive name of "Download all", without ".zip".
pub const DEFAULT_EXPORT_NAME: &str = "instagram-export";
/// The manifest's entry name; it is written last.
pub const MANIFEST_ENTRY_NAME: &str = "post.json";
/// post.json is deflated at this level (fflate `ZipDeflate` level 6); media are stored.
pub const MANIFEST_DEFLATE_LEVEL: i64 = 6;
/// The value of "format" in post.json.
pub const EXPORT_FORMAT: &str = "instagram-export";
/// The value of "version" in post.json.
pub const EXPORT_VERSION: u32 = 1;
/// The value of "source.type" in post.json: an export of posts never names a source link.
pub const EXPORT_SOURCE_TYPE: &str = "posts";
/// The years a ZIP entry time can hold; an instant outside is clamped (`entry_time`).
pub const ZIP_MIN_YEAR: i32 = 1980;
/// The last year a ZIP entry time can hold.
pub const ZIP_MAX_YEAR: i32 = 2107;

// --- Options, progress, result ----------------------------------------------------------------

/// How an export is written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportOptions {
    /// Archive name without ".zip"; parts get "-part1", "-part2", ... `DEFAULT_EXPORT_NAME` for
    /// "Download all", `single_post_export_name(id)` for "Download post", or the stem the user
    /// typed in the Save dialog (`split_destination`). It must be a plain file name: a name that
    /// is empty or holds a path separator, a colon or another character Windows refuses in file
    /// names fails the export with `WriteFailed` before anything is written.
    pub name: String,
    /// Media bytes per archive (`EXPORT_PART_BYTES`; tests lower it).
    pub max_part_bytes: u64,
    /// Posts per archive (`EXPORT_PART_POSTS`; tests lower it).
    pub max_part_posts: usize,
    /// Written as `exported_at` of every part and as the modified time of every archive entry.
    pub now_ms: i64,
    /// The local offset from UTC at `now_ms`, in minutes east (`text::local_offset_minutes`):
    /// archive entry times are local times, as the clone's ZIP writer stores them.
    pub local_offset_minutes: i32,
}

impl ExportOptions {
    /// Options with the clone's part limits.
    pub fn new(name: impl Into<String>, now_ms: i64, local_offset_minutes: i32) -> ExportOptions {
        ExportOptions {
            name: name.into(),
            max_part_bytes: EXPORT_PART_BYTES,
            max_part_posts: EXPORT_PART_POSTS,
            now_ms,
            local_offset_minutes,
        }
    }
}

/// How far an export is, for "Preparing {done} of {total}".
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExportProgress {
    /// Posts handled so far (written or left out).
    pub done: usize,
    /// Posts of the export in all.
    pub total: usize,
}

/// One archive that was written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportArchive {
    /// The finished file in the target folder.
    pub path: PathBuf,
    /// Its file name, for example "instagram-export.zip" or "instagram-export-part2.zip".
    pub name: String,
    /// Posts in this archive.
    pub posts: usize,
}

/// What an export produced.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportResult {
    /// One archive, or several parts of a large export; empty when no post could be exported.
    pub archives: Vec<ExportArchive>,
    /// Posts written, over all archives.
    pub posts: usize,
    /// Files or posts that were left out, one sentence each (`messages::problem_*`), in order.
    pub problems: Vec<String>,
}

/// Why an export stopped without a result.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportErrorCode {
    /// The cancel flag was set: `messages::DOWNLOAD_WAS_CANCELLED`. The UI shows the toast
    /// `messages::DOWNLOAD_CANCELLED`.
    Cancelled,
    /// An archive could not be written or completed: `messages::archive_not_written(os error)`
    /// or `messages::ARCHIVE_NOT_COMPLETED`. The UI shows the message as a toast.
    WriteFailed,
}

/// Raised when the whole export failed. Nothing is left in the target folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportError {
    /// Cancelled by the user, or failed.
    pub code: ExportErrorCode,
    /// The sentence to show (see `ExportErrorCode`).
    pub message: String,
}

impl ExportError {
    /// An error of `code` with its sentence.
    pub fn new(code: ExportErrorCode, message: impl Into<String>) -> ExportError {
        ExportError { code, message: message.into() }
    }
}

impl std::fmt::Display for ExportError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ExportError {}

// --- post.json, in the canonical field order of instagram.md section 8 ------------------------
//
// The field order of these structs IS the field order in post.json (asserted by the clone's
// export.test.ts:140-142). Optional fields are left out when `None`, except `like_count`, which
// is written as `null`. Counts are integers, so they never print as "0.0".

/// post.json of one archive part.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportManifest {
    /// Always `EXPORT_FORMAT`.
    pub format: &'static str,
    /// Always `EXPORT_VERSION`.
    pub version: u32,
    /// `text::iso_time(now)`; the same instant for every part.
    pub exported_at: String,
    /// Always exactly `{ "type": "posts" }`.
    pub source: ExportSource,
    /// This part's posts, in export order.
    pub posts: Vec<ExportPostEntry>,
}

/// post.json `source`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportSource {
    /// Written as "type": always `EXPORT_SOURCE_TYPE`.
    #[serde(rename = "type")]
    pub kind: &'static str,
}

/// post.json `media` object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportMediaEntry {
    /// "image" or "video": the SNIFFED kind of the file that was written.
    #[serde(rename = "type")]
    pub kind: &'static str,
    /// `posts/post<N>.<ext>` or `posts/post<N>_<k>.<ext>`.
    pub path: String,
    /// `<slide base>_poster.<ext>`, only when a poster was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub poster: Option<String>,
    /// Only when the slide's alt is non-empty and differs from the post's alt.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
}

/// post.json `user` object.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportUserEntry {
    /// The author's username, as the post has it.
    pub username: String,
    /// Only when non-empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub full_name: Option<String>,
    /// Only when the post says so (`true` or `false`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verified: Option<bool>,
    /// `avatars/<stem>.<ext>`, only when an avatar picture was written.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub avatar: Option<String>,
    /// Only when the profile link passes `urls::is_profile_url` (spec-export G.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile_url: Option<String>,
}

/// post.json `comments` object. The internal comment id is not written.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportCommentEntry {
    /// Who wrote the comment.
    pub username: String,
    /// The comment, verbatim.
    pub text: String,
    /// Likes of the comment.
    pub like_count: u64,
    /// `text::iso_time` of the comment's time; left out when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taken_at: Option<String>,
}

/// post.json entry of one post.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ExportPostEntry {
    /// The de-duplicated id.
    pub id: String,
    /// The permalink, only when it passes `urls::is_post_url` (spec-export G.3).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// At least one entry.
    pub media: Vec<ExportMediaEntry>,
    /// The author.
    pub user: ExportUserEntry,
    /// The current description, verbatim (may be ""; not trimmed, not cut at export).
    pub caption: String,
    /// Total number of comments on the original post.
    pub comment_count: u64,
    /// Written as `null` when unknown.
    pub like_count: Option<u64>,
    /// `text::iso_time(taken_at)`; left out when unknown.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub taken_at: Option<String>,
    /// "image", "carousel" or "video" (`export_media_type`).
    pub media_type: &'static str,
    /// Only when non-empty.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub alt: Option<String>,
    /// Only when non-empty. The header sub-line of a loose picture (`Post::context`) is NOT
    /// exported.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// Left out when the post has no comments.
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub comments: Vec<ExportCommentEntry>,
}

// --- Pure parts (unit-tested without touching the disk) ---------------------------------------

/// The archive name of "Download post": `instagram-export-{text::archive_stem(post id)}`
/// (App.tsx:949-954), for example "instagram-export-FXTbasic004".
pub fn single_post_export_name(id: &str) -> String {
    format!("{DEFAULT_EXPORT_NAME}-{}", text::archive_stem(id))
}

/// The file name of archive `part` (1-based) of `parts`: `{name}.zip` for a single archive,
/// `{name}-part{part}.zip` when there are several (export.ts:435-437).
pub fn archive_file_name(name: &str, part: usize, parts: usize) -> String {
    if parts <= 1 {
        format!("{name}.zip")
    } else {
        format!("{name}-part{part}.zip")
    }
}

/// Splits the path chosen in the Save dialog into the target folder and the archive name: the
/// parent folder, and the file name without a trailing ".zip" (any letter case). `None` when the
/// path has no file name or no parent. `C:\Out\my trip.zip` -> (`C:\Out`, "my trip").
///
/// A file name that is not valid Unicode, or that is nothing but ".zip", gives `None` as well: an
/// archive could not be named after it. A bare file name ("trip.zip") has the current folder
/// (".") as its target folder.
pub fn split_destination(file: &Path) -> Option<(PathBuf, String)> {
    let file_name = file.file_name()?.to_str()?;
    let parent = file.parent()?;
    let name = strip_zip_suffix(file_name);
    if name.is_empty() {
        return None;
    }
    let folder = if parent.as_os_str().is_empty() { PathBuf::from(".") } else { parent.to_path_buf() };
    Some((folder, name.to_owned()))
}

/// True when `name` can be the name of an archive (`ExportOptions::name`) on Windows: not
/// empty, not "." or "..", and without a path separator, a drive colon, a control character or
/// one of the other characters Windows refuses in a file name (`< > " | ? *`).
fn is_archive_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && !name.chars().any(|character| {
            character.is_control() || matches!(character, '/' | '\\' | ':' | '<' | '>' | '"' | '|' | '?' | '*')
        })
}

/// `file_name` without a trailing ".zip" in any letter case.
fn strip_zip_suffix(file_name: &str) -> &str {
    const SUFFIX: &str = ".zip";
    let Some(cut) = file_name.len().checked_sub(SUFFIX.len()) else {
        return file_name;
    };
    match file_name.get(cut..) {
        Some(tail) if tail.eq_ignore_ascii_case(SUFFIX) => &file_name[..cut],
        _ => file_name,
    }
}

/// The modified time of every archive entry: `now_ms` as LOCAL civil time (`now_ms +
/// local_offset_minutes`), with the seconds rounded down to an even number and the milliseconds
/// 0 (DOS times have a two-second resolution), clamped to `ZIP_MIN_YEAR`..=`ZIP_MAX_YEAR`
/// (earlier -> 1980-01-01 00:00:00, later -> 2107-12-31 23:59:58).
pub fn entry_time(now_ms: i64, local_offset_minutes: i32) -> CivilTime {
    const MS_PER_MINUTE: i64 = 60_000;
    let local_ms = now_ms.saturating_add(i64::from(local_offset_minutes) * MS_PER_MINUTE);
    let local = text::civil_from_epoch_ms(local_ms);
    if local.year < ZIP_MIN_YEAR {
        return CivilTime { year: ZIP_MIN_YEAR, month: 1, day: 1, hour: 0, minute: 0, second: 0, millisecond: 0 };
    }
    if local.year > ZIP_MAX_YEAR {
        return CivilTime { year: ZIP_MAX_YEAR, month: 12, day: 31, hour: 23, minute: 59, second: 58, millisecond: 0 };
    }
    CivilTime { second: local.second - local.second % 2, millisecond: 0, ..local }
}

/// `entry_time` in the form the archive writer takes. The fields are always in range; should
/// the writer refuse them all the same, the entry gets the earliest ZIP time instead of failing.
fn zip_entry_time(now_ms: i64, local_offset_minutes: i32) -> DateTime {
    let time = entry_time(now_ms, local_offset_minutes);
    let narrow = |value: u32| u8::try_from(value).unwrap_or(0);
    DateTime::from_date_and_time(
        u16::try_from(time.year).unwrap_or(ZIP_MIN_YEAR as u16),
        narrow(time.month),
        narrow(time.day),
        narrow(time.hour),
        narrow(time.minute),
        narrow(time.second),
    )
    .unwrap_or_default()
}

/// Rule 9 of the post entry (export.ts:412-429): more than one media entry -> "carousel"; else
/// the single entry is a video, or the post is declared a video -> "video"; else "image". So a
/// carousel that lost slides down to one becomes "image" / "video", and a video post exported
/// as its poster only stays "video".
pub fn export_media_type(media: &[ExportMediaEntry], declared: MediaType) -> &'static str {
    if media.len() > 1 {
        return MediaType::Carousel.as_str();
    }
    let single_is_video = media.first().is_some_and(|entry| entry.kind == MediaKind::Video.as_str());
    if single_is_video || declared == MediaType::Video {
        MediaType::Video.as_str()
    } else {
        MediaType::Image.as_str()
    }
}

/// The text of an optional field that is written only when it is not empty (the clone tests
/// `if (value)`: an empty string is left out, white space is kept).
fn non_empty(value: &Option<String>) -> Option<String> {
    value.as_ref().filter(|text| !text.is_empty()).cloned()
}

/// The post.json entry of a post whose files were written (export.ts:381-429; spec-export A.5).
/// `id` is the de-duplicated id, `media` the entries of the slides that were written, `avatar`
/// the archive path of the profile picture when one was written.
///
/// * `url`: `post.permalink` only when `urls::is_post_url` holds, else left out.
/// * `user`: `username` always; `full_name` when non-empty; `verified` when known; `avatar` as
///   given; `profile_url` only when `urls::is_profile_url` holds.
/// * `caption`: `post.caption` verbatim. `comment_count`, `like_count` as they are.
/// * `taken_at`: `text::iso_time`, left out when `None`.
/// * `media_type`: `export_media_type(&media, post.media_type)`.
/// * `alt`, `location`: when non-empty. `post.context` is never written.
/// * `comments`: `username`, `text`, `like_count`, and `taken_at` when `text::iso_time` gives one.
pub fn build_post_entry(
    post: &Post,
    id: String,
    media: Vec<ExportMediaEntry>,
    avatar: Option<String>,
) -> ExportPostEntry {
    let media_type = export_media_type(&media, post.media_type);
    let comments = post
        .comments
        .iter()
        .map(|comment| ExportCommentEntry {
            username: comment.username.clone(),
            text: comment.text.clone(),
            like_count: comment.likes,
            taken_at: comment.taken_at.and_then(text::iso_time),
        })
        .collect();
    ExportPostEntry {
        id,
        url: post.permalink.as_ref().filter(|link| urls::is_post_url(link)).cloned(),
        media,
        user: ExportUserEntry {
            username: post.user.username.clone(),
            full_name: non_empty(&post.user.full_name),
            verified: post.user.verified,
            avatar,
            profile_url: post.user.profile_url.as_ref().filter(|link| urls::is_profile_url(link)).cloned(),
        },
        caption: post.caption.clone(),
        comment_count: post.comment_count,
        like_count: post.like_count,
        taken_at: post.taken_at.and_then(text::iso_time),
        media_type,
        alt: non_empty(&post.alt),
        location: non_empty(&post.location),
        comments,
    }
}

/// post.json of one part: `format`, `version`, `exported_at = text::iso_time(now_ms)` (an
/// instant without a four-digit year cannot occur for a wall clock; fall back to
/// "1970-01-01T00:00:00Z"), `source = {"type": "posts"}`, `posts`.
pub fn build_manifest(now_ms: i64, posts: Vec<ExportPostEntry>) -> ExportManifest {
    ExportManifest {
        format: EXPORT_FORMAT,
        version: EXPORT_VERSION,
        exported_at: text::iso_time(now_ms).unwrap_or_else(|| EPOCH_ISO_TIME.to_owned()),
        source: ExportSource { kind: EXPORT_SOURCE_TYPE },
        posts,
    }
}

/// `exported_at` of an instant that has no four-digit year (`build_manifest`).
const EPOCH_ISO_TIME: &str = "1970-01-01T00:00:00Z";

/// The bytes of post.json as text: `serde_json::to_string_pretty` (which equals JavaScript's
/// `JSON.stringify(manifest, null, 2)`: two-space indentation, non-ASCII written raw, an empty
/// list as `[]`) plus exactly one "\n". UTF-8 without BOM.
///
/// Both writers escape the same characters in the same way: `"` and `\` with a backslash,
/// U+0008, U+0009, U+000A, U+000C and U+000D as `\b`, `\t`, `\n`, `\f`, `\r`, every other
/// character below U+0020 as `\u00xx` with lower-case digits, and nothing else ("/", U+007F,
/// U+2028 and U+2029 stay as they are). The unit tests pin that, and core/tests/export.rs lets
/// Node.js re-write a post.json and compares the bytes.
///
/// The manifest holds only text, whole numbers, booleans and lists, which can always be
/// written; should the writer fail all the same, the text is empty and `export_posts` reports
/// `messages::ARCHIVE_NOT_COMPLETED` instead of writing it.
pub fn manifest_json(manifest: &ExportManifest) -> String {
    match serde_json::to_string_pretty(manifest) {
        Ok(mut text) => {
            text.push('\n');
            text
        }
        Err(_) => String::new(),
    }
}

// --- The export -------------------------------------------------------------------------------

/// Writes `posts` as one or more archives into `target_dir` (export.ts `exportPosts`;
/// spec-export A.2-A.8, G.3, G.4). Runs on a background thread.
///
/// Per post, in input order, with `label = messages::export_label(index + 1, &post.id)`:
///
/// 1. `progress({done: 0, total})` once before the first post.
/// 2. Slides: for each media item, read the head of `item.path` and sniff it. It cannot be read
///    -> problem 1 (`problem_file_unreadable`, `{path}` = `item.manifest_path`); it does not
///    sniff -> problem 2. Only when the item has a poster AND (the file failed OR it sniffed as
///    a video): read and sniff the poster; unreadable -> problem 3, does not sniff -> problem 4,
///    a video -> problem 5 (poster dropped). File ok -> slide {file, poster?, alt}. File failed
///    but poster ok -> problem 6 and the poster becomes an ordinary image slide.
/// 3. No slide -> problem 7, progress, next post (no number is consumed).
/// 4. Avatar, when the user has one: looked at once per distinct path for the whole export.
///    Unreadable -> problem 8, does not sniff -> problem 9, a video -> problem 10; a failed
///    avatar is remembered as "none" and reported only for the first post that needed it.
/// 5. `bytes` = sizes of the slide files and posters + the avatar (counted even when this part
///    already has it). `bytes > MAX_POST_BYTES` -> problem 11, progress, next post.
/// 6. Part split: when the current part has posts and (`posts >= max_part_posts` or
///    `part media bytes + bytes > max_part_bytes`), the part is closed (its post.json written)
///    and a new one started. A part is never empty.
/// 7. `number += 1` (global over all parts). Slide `s` (0-based) is written as
///    `posts/post{number}_{s+1}.{ext}` when the post has more than one slide, else
///    `posts/post{number}.{ext}`; its poster as `{base}_poster.{ext}` directly after it; `alt`
///    of the media entry only when the slide's alt is non-empty and differs from `post.alt`.
///    `{ext}` is `sniff::MediaFormat::extension` of the sniffed bytes.
/// 8. The avatar is written as `avatars/{text::avatar_stem(username, taken)}.{ext}` after the
///    post's media when this part has none for that username yet (the stem set and the
///    username -> path map are per part; the first avatar of a username wins).
/// 9. The id is made unique over ALL parts (`text::unique_id`): "C8ZmP4RvNd7", "C8ZmP4RvNd7-2".
/// 10. The entry (`build_post_entry`) is added to the part; `progress({done: index + 1, total})`.
///
/// Archive: media, posters and avatars are STORED, post.json (`manifest_json`) is DEFLATED at
/// level `MANIFEST_DEFLATE_LEVEL` and added last; entry names use forward slashes; no directory
/// entries; every entry has the modified time `entry_time(now_ms, local_offset_minutes)`.
/// Files are copied in chunks of `READ_CHUNK_BYTES`; a file that fails while being copied
/// aborts the export (`WriteFailed`, `messages::archive_not_written`; when it is a file of a
/// post that can no longer be read, the cause names it: `"posts/a.jpg" could not be read: ...`).
///
/// Files: each part is written to a temporary file in `target_dir` (`.{name}-{unique}.tmp`,
/// the name cut to its first 40 characters). A part is started only when a post is about to be
/// written into it, so an export in which nothing can be saved creates no file at all.
/// When every part is complete they are renamed to `archive_file_name(name, k, parts)`,
/// replacing existing files of those names; files of an earlier export under other names (the
/// single `{name}.zip` when this export has parts, or parts with higher numbers) are not
/// touched.
///
/// `cancel` is checked before each post, after every file that is read, between chunks, and
/// once more before the parts are renamed (so a cancel during the last progress report still
/// leaves nothing behind): a set flag gives `Err(Cancelled)`. On every error the temporary
/// files, and parts that were already renamed, are removed.
///
/// `Ok` also when nothing could be written: `archives` is empty and `problems` say why.
/// `target_dir` must exist; it is not created. A folder that is missing or may not be written
/// to, and an `options.name` that is not a plain file name, give `Err(WriteFailed)` with
/// `messages::archive_not_written`.
pub fn export_posts(
    posts: &[Post],
    target_dir: &Path,
    options: &ExportOptions,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &AtomicBool,
) -> Result<ExportResult, ExportError> {
    export_with_post_limit(posts, target_dir, options, &NoPosters, MAX_POST_BYTES, progress, cancel)
}

/// `export_posts` that gives every loose video a poster from `posters` (video brief 5.8): a
/// video of a loose post (`PostOrigin::File`) without a poster of its own gets
/// `posts/post{N}_poster.jpg` from `PosterSource::poster_for`, counted in the archive budget
/// like an export's poster. When it cannot be made the video is exported without one and no
/// problem is reported (posters are optional in the export format). An export's own videos keep
/// their poster or none.
pub fn export_posts_with_posters(
    posts: &[Post],
    target_dir: &Path,
    options: &ExportOptions,
    posters: &dyn PosterSource,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &AtomicBool,
) -> Result<ExportResult, ExportError> {
    export_with_post_limit(posts, target_dir, options, posters, MAX_POST_BYTES, progress, cancel)
}

/// `export_posts` with the size limit of one post as a parameter, so a test can reach the
/// "too large for one archive" rule without files of four gigabytes.
fn export_with_post_limit(
    posts: &[Post],
    target_dir: &Path,
    options: &ExportOptions,
    posters: &dyn PosterSource,
    max_post_bytes: u64,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &AtomicBool,
) -> Result<ExportResult, ExportError> {
    // The archives must land in the target folder: a name with a folder in it would not.
    if !is_archive_name(&options.name) {
        return Err(write_failed(&format!("\"{}\" cannot be used as a file name", options.name)));
    }
    // Declared first, so it is dropped last: by then every archive file is closed, and whatever
    // an unfinished export has put into the target folder is removed.
    let mut pending = PendingFiles::default();
    let written = write_parts(posts, target_dir, options, posters, max_post_bytes, progress, cancel, &mut pending)?;
    check_cancelled(cancel)?;
    let archives = publish_parts(written.parts, target_dir, &options.name, &mut pending)?;
    Ok(ExportResult { archives, posts: written.posts, problems: written.problems })
}

// --- Files in the target folder ---------------------------------------------------------------

/// How often a finished part is renamed, or a left-over file removed, before giving up: a virus
/// scanner or the search indexer may hold a file that was just written for a moment.
const FILE_ATTEMPTS: u32 = 5;
/// The pause before attempt `n + 1` is `n` times this long.
const FILE_RETRY_STEP_MS: u64 = 50;
/// How many names are tried for a temporary file before the export gives up.
const TEMP_NAME_ATTEMPTS: u32 = 32;
/// How much of the archive name a temporary file name carries, so that a long archive name
/// does not push it over the length a file name may have.
const TEMP_NAME_CHARS: usize = 40;

/// The files an unfinished export has put into the target folder: temporary files, and parts
/// that were already renamed. Whatever is still listed when the value is dropped is removed, so
/// an export that fails or is cancelled leaves nothing behind.
#[derive(Default)]
struct PendingFiles {
    paths: Vec<PathBuf>,
}

impl PendingFiles {
    fn add(&mut self, path: PathBuf) {
        self.paths.push(path);
    }

    /// The file no longer exists under this name (it was renamed).
    fn forget(&mut self, path: &Path) {
        self.paths.retain(|listed| listed != path);
    }

    /// The export is complete: every listed file stays.
    fn keep_all(&mut self) {
        self.paths.clear();
    }
}

impl Drop for PendingFiles {
    fn drop(&mut self) {
        for path in &self.paths {
            // Nothing more can be done about a file that cannot be removed: the export has
            // already failed, and its error is the one the user is told.
            let _ = with_retries(|| match fs::remove_file(path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                other => other,
            });
        }
    }
}

/// Runs a file operation up to `FILE_ATTEMPTS` times while Windows answers that another program
/// holds the file (`store::retry_while_in_use`), pausing a little longer each time. Any other
/// error is permanent and returned at once.
fn with_retries(mut operation: impl FnMut() -> io::Result<()>) -> io::Result<()> {
    store::retry_while_in_use(
        FILE_ATTEMPTS,
        &|retry| FILE_RETRY_STEP_MS * u64::from(retry),
        &mut operation,
    )
}

/// A new, empty temporary file in `target_dir`, named `.{archive name}-{unique}.tmp`. It is
/// created with "create new", so a file that happens to have the name is never touched.
fn create_temp_file(target_dir: &Path, name: &str) -> io::Result<(PathBuf, File)> {
    static NEXT_SERIAL: AtomicU64 = AtomicU64::new(0);
    let short_name: String = name.chars().take(TEMP_NAME_CHARS).collect();
    let mut attempt = 1;
    loop {
        let serial = NEXT_SERIAL.fetch_add(1, Ordering::Relaxed);
        let clock = SystemTime::now().duration_since(UNIX_EPOCH).map(|since| since.subsec_nanos()).unwrap_or(0);
        let file_name = format!(".{short_name}-{:x}-{clock:x}-{serial:x}.tmp", std::process::id());
        let path = target_dir.join(file_name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt < TEMP_NAME_ATTEMPTS => attempt += 1,
            Err(error) => return Err(error),
        }
    }
}

// --- Errors -----------------------------------------------------------------------------------

/// `Err(Cancelled)` when the cancel flag is set.
fn check_cancelled(cancel: &AtomicBool) -> Result<(), ExportError> {
    if cancel.load(Ordering::Relaxed) {
        Err(ExportError::new(ExportErrorCode::Cancelled, messages::DOWNLOAD_WAS_CANCELLED))
    } else {
        Ok(())
    }
}

/// The export stops: an archive cannot be written. `cause` follows the colon of the sentence.
fn write_failed(cause: &dyn std::fmt::Display) -> ExportError {
    ExportError::new(ExportErrorCode::WriteFailed, messages::archive_not_written(&cause.to_string()))
}

/// `write_failed` for an error of the archive writer: the operating system's own words when the
/// disk refused, the writer's otherwise.
fn archive_failed(error: ZipError) -> ExportError {
    match error {
        ZipError::Io(error) => write_failed(&error),
        other => write_failed(&other),
    }
}

// --- Reading the files of a post --------------------------------------------------------------

/// A file that was opened and recognised, ready to be copied into an archive.
#[derive(Clone)]
struct LoadedFile {
    path: PathBuf,
    /// Its size when it was looked at; the part limits are computed from it.
    size: u64,
    kind: MediaKind,
    /// The extension of the entry in the archive, from the file's first bytes.
    extension: &'static str,
    /// The sniffed MIME type (the poster of a loose video is made by it).
    mime: &'static str,
    /// How a sentence names the file: `"posts/a.jpg"`, `The poster of "posts/a.mp4"`,
    /// `The profile picture`.
    what: String,
}

/// Why a file is left out.
enum LoadFailure {
    /// It cannot be opened or read; the operating system's reason.
    Unreadable(String),
    /// Its first bytes are not those of a supported picture or video (also an empty file).
    NotMedia,
}

/// Opens `path`, takes its size and sniffs its first bytes (export.ts `load`).
fn load_file(path: &Path, what: String) -> Result<LoadedFile, LoadFailure> {
    let unreadable = |error: io::Error| LoadFailure::Unreadable(error.to_string());
    let file = File::open(path).map_err(unreadable)?;
    let size = file.metadata().map_err(unreadable)?.len();
    let head = sniff::read_head_of(&file).map_err(unreadable)?;
    let Some(sniffed) = sniff::sniff_media(&head) else {
        return Err(LoadFailure::NotMedia);
    };
    Ok(LoadedFile {
        path: path.to_path_buf(),
        size,
        kind: sniffed.kind,
        extension: sniffed.format.extension(),
        mime: sniffed.mime,
        what,
    })
}

/// One picture or video of a post that will be written, with the poster of a video.
struct LoadedSlide {
    file: LoadedFile,
    poster: Option<LoadedFile>,
    alt: Option<String>,
}

/// The slides of `post` that can be written (export.ts `loadSlides`; step 2 of `export_posts`).
/// What is left out is added to `problems`.
fn load_slides(
    post: &Post,
    label: &str,
    problems: &mut Vec<String>,
    posters: &dyn PosterSource,
    cancel: &AtomicBool,
) -> Result<Vec<LoadedSlide>, ExportError> {
    let mut slides = Vec::new();
    for item in &post.media {
        let shown = item.manifest_path.as_str();
        let file = match load_file(&item.path, format!("\"{shown}\"")) {
            Ok(file) => Some(file),
            Err(LoadFailure::Unreadable(reason)) => {
                problems.push(messages::problem_file_unreadable(label, shown, &reason));
                None
            }
            Err(LoadFailure::NotMedia) => {
                problems.push(messages::problem_file_not_media(label, shown));
                None
            }
        };
        check_cancelled(cancel)?;

        // The poster is only needed for a video, or in place of a file that is gone.
        let mut poster = None;
        let wants_poster = file.as_ref().is_none_or(|file| file.kind == MediaKind::Video);
        if let (Some(source), true) = (&item.poster, wants_poster) {
            match load_file(&source.path, format!("The poster of \"{shown}\"")) {
                Ok(loaded) if loaded.kind == MediaKind::Image => poster = Some(loaded),
                Ok(_) => problems.push(messages::problem_poster_is_a_video(label, shown)),
                Err(LoadFailure::Unreadable(reason)) => {
                    problems.push(messages::problem_poster_unreadable(label, shown, &reason));
                }
                Err(LoadFailure::NotMedia) => problems.push(messages::problem_poster_not_media(label, shown)),
            }
            check_cancelled(cancel)?;
        }
        // A loose video has no poster of its own: the app's poster cache gives it one. Failing
        // that, it goes without (posters are optional; the cache has logged why).
        if let (Some(video), None, PostOrigin::File) = (file.as_ref().filter(|file| file.kind == MediaKind::Video), &item.poster, &post.origin) {
            if let Some(made) = posters.poster_for(&video.path, video.mime) {
                match load_file(&made, format!("The poster of \"{shown}\"")) {
                    Ok(loaded) if loaded.kind == MediaKind::Image => poster = Some(loaded),
                    _ => log::warn!("The poster of \"{shown}\" could not be added to the download."),
                }
            }
            check_cancelled(cancel)?;
        }

        match (file, poster) {
            (Some(file), poster) => slides.push(LoadedSlide { file, poster, alt: item.alt.clone() }),
            (None, Some(poster)) => {
                // The video is gone: its poster stands in for it, as an ordinary picture.
                problems.push(messages::problem_only_poster_saved(label, shown));
                slides.push(LoadedSlide { file: poster, poster: None, alt: item.alt.clone() });
            }
            (None, None) => {}
        }
    }
    Ok(slides)
}

/// The profile picture at `path` when it is a picture (export.ts `loadAvatar`; step 4 of
/// `export_posts`). Each distinct path is looked at once for the whole export; a failure is
/// remembered as "none" and reported for the first post that needed it.
fn load_avatar(
    path: &Path,
    label: &str,
    cache: &mut HashMap<PathBuf, Option<LoadedFile>>,
    problems: &mut Vec<String>,
) -> Option<LoadedFile> {
    if let Some(known) = cache.get(path) {
        return known.clone();
    }
    let avatar = match load_file(path, "The profile picture".to_owned()) {
        Ok(loaded) if loaded.kind == MediaKind::Image => Some(loaded),
        Ok(_) => {
            problems.push(messages::problem_avatar_is_a_video(label));
            None
        }
        Err(LoadFailure::Unreadable(reason)) => {
            problems.push(messages::problem_avatar_unreadable(label, &reason));
            None
        }
        Err(LoadFailure::NotMedia) => {
            problems.push(messages::problem_avatar_not_media(label));
            None
        }
    };
    cache.insert(path.to_path_buf(), avatar.clone());
    avatar
}

// --- Writing the parts ------------------------------------------------------------------------

/// One archive being written (export.ts `PartWriter`).
struct PartWriter {
    /// The temporary file the archive is written to.
    temp_path: PathBuf,
    zip: ZipWriter<BufWriter<File>>,
    entries: Vec<ExportPostEntry>,
    /// Bytes of the pictures, videos, posters and avatars added so far (not post.json).
    media_bytes: u64,
    /// Username -> avatar path in this part.
    avatars: HashMap<String, String>,
    /// The avatar stems of this part, lower-cased (`text::avatar_stem`).
    avatar_stems: HashSet<String>,
}

/// A part whose archive is complete and closed, still under its temporary name.
struct FinishedPart {
    temp_path: PathBuf,
    posts: usize,
}

/// What `write_parts` produced.
struct WrittenParts {
    parts: Vec<FinishedPart>,
    posts: usize,
    problems: Vec<String>,
}

impl PartWriter {
    /// Starts a part in a new temporary file, which is listed in `pending`.
    fn create(target_dir: &Path, name: &str, pending: &mut PendingFiles) -> Result<PartWriter, ExportError> {
        let (temp_path, file) = create_temp_file(target_dir, name).map_err(|error| write_failed(&error))?;
        pending.add(temp_path.clone());
        Ok(PartWriter {
            temp_path,
            zip: ZipWriter::new(BufWriter::new(file)),
            entries: Vec::new(),
            media_bytes: 0,
            avatars: HashMap::new(),
            avatar_stems: HashSet::new(),
        })
    }

    /// True when `bytes` more, or one more post, do not fit into this part (step 6).
    fn is_full_for(&self, bytes: u64, options: &ExportOptions) -> bool {
        !self.entries.is_empty()
            && (self.entries.len() >= options.max_part_posts
                || self.media_bytes.saturating_add(bytes) > options.max_part_bytes)
    }

    /// Copies a picture, video, poster or avatar into the archive as it is on disk, stored, in
    /// chunks of the size of `buffer`. The cancel flag is checked before every chunk.
    fn add_file(
        &mut self,
        entry_name: &str,
        source: &LoadedFile,
        stored: SimpleFileOptions,
        buffer: &mut [u8],
        cancel: &AtomicBool,
    ) -> Result<(), ExportError> {
        let unreadable = |error: io::Error| write_failed(&format!("{} could not be read: {error}", source.what));
        let mut file = File::open(&source.path).map_err(unreadable)?;
        self.zip.start_file(entry_name, stored).map_err(archive_failed)?;
        loop {
            check_cancelled(cancel)?;
            let count = match file.read(buffer) {
                Ok(0) => break,
                Ok(count) => count,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(unreadable(error)),
            };
            self.zip.write_all(&buffer[..count]).map_err(|error| write_failed(&error))?;
            self.media_bytes = self.media_bytes.saturating_add(count as u64);
        }
        Ok(())
    }

    /// Adds post.json, deflated, as the last entry, and completes and closes the archive.
    fn finish(mut self, now_ms: i64, deflated: SimpleFileOptions) -> Result<FinishedPart, ExportError> {
        let posts = self.entries.len();
        let manifest = build_manifest(now_ms, std::mem::take(&mut self.entries));
        let text = manifest_json(&manifest);
        if text.is_empty() {
            return Err(ExportError::new(ExportErrorCode::WriteFailed, messages::ARCHIVE_NOT_COMPLETED));
        }
        self.zip.start_file(MANIFEST_ENTRY_NAME, deflated).map_err(archive_failed)?;
        self.zip.write_all(text.as_bytes()).map_err(|error| write_failed(&error))?;
        let writer = self.zip.finish().map_err(archive_failed)?;
        // into_inner flushes what the buffer still holds; the file is closed when it is dropped.
        writer.into_inner().map_err(|error| write_failed(error.error()))?;
        Ok(FinishedPart { temp_path: self.temp_path, posts })
    }
}

/// Steps 1 to 10 of `export_posts`: reads the posts' files and writes them into as many parts
/// as the limits ask for, each in a temporary file. Every archive file is closed on return.
#[allow(clippy::too_many_arguments)]
fn write_parts(
    posts: &[Post],
    target_dir: &Path,
    options: &ExportOptions,
    posters: &dyn PosterSource,
    max_post_bytes: u64,
    progress: &mut dyn FnMut(ExportProgress),
    cancel: &AtomicBool,
    pending: &mut PendingFiles,
) -> Result<WrittenParts, ExportError> {
    let total = posts.len();
    let entry_time = zip_entry_time(options.now_ms, options.local_offset_minutes);
    let stored =
        SimpleFileOptions::default().compression_method(CompressionMethod::Stored).last_modified_time(entry_time);
    let deflated = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Deflated)
        .compression_level(Some(MANIFEST_DEFLATE_LEVEL))
        .last_modified_time(entry_time);

    let mut problems: Vec<String> = Vec::new();
    let mut finished: Vec<FinishedPart> = Vec::new();
    let mut used_ids: HashSet<String> = HashSet::new();
    // Avatars are looked at once, whatever part they go into.
    let mut avatar_cache: HashMap<PathBuf, Option<LoadedFile>> = HashMap::new();
    // A part is started only when a post is about to be written, so an export in which nothing
    // can be saved never creates a file.
    let mut part: Option<PartWriter> = None;
    let mut buffer: Vec<u8> = Vec::new();
    let mut number = 0_usize;

    progress(ExportProgress { done: 0, total });
    for (index, post) in posts.iter().enumerate() {
        check_cancelled(cancel)?;
        let label = messages::export_label(index + 1, &post.id);
        let slides = load_slides(post, &label, &mut problems, posters, cancel)?;
        if slides.is_empty() {
            problems.push(messages::problem_post_left_out(&label));
            progress(ExportProgress { done: index + 1, total });
            continue;
        }
        let avatar = match &post.user.avatar {
            Some(path) => load_avatar(path, &label, &mut avatar_cache, &mut problems),
            None => None,
        };
        check_cancelled(cancel)?;

        let mut bytes = avatar.as_ref().map_or(0, |avatar| avatar.size);
        for slide in &slides {
            bytes = bytes.saturating_add(slide.file.size);
            bytes = bytes.saturating_add(slide.poster.as_ref().map_or(0, |poster| poster.size));
        }
        if bytes > max_post_bytes {
            problems.push(messages::problem_post_too_large(&label));
            progress(ExportProgress { done: index + 1, total });
            continue;
        }

        if part.as_ref().is_some_and(|open| open.is_full_for(bytes, options)) {
            if let Some(full) = part.take() {
                finished.push(full.finish(options.now_ms, deflated)?);
            }
        }
        let open = match part.take() {
            Some(open) => part.insert(open),
            None => part.insert(PartWriter::create(target_dir, &options.name, pending)?),
        };
        if buffer.is_empty() {
            buffer = vec![0; READ_CHUNK_BYTES];
        }

        number += 1;
        let mut media = Vec::with_capacity(slides.len());
        for (slide_index, slide) in slides.iter().enumerate() {
            let base = if slides.len() > 1 {
                format!("posts/post{number}_{}", slide_index + 1)
            } else {
                format!("posts/post{number}")
            };
            let path = format!("{base}.{}", slide.file.extension);
            open.add_file(&path, &slide.file, stored, &mut buffer, cancel)?;
            let poster = match &slide.poster {
                Some(poster) => {
                    let poster_path = format!("{base}_poster.{}", poster.extension);
                    open.add_file(&poster_path, poster, stored, &mut buffer, cancel)?;
                    Some(poster_path)
                }
                None => None,
            };
            let alt = slide.alt.as_ref().filter(|alt| !alt.is_empty() && post.alt.as_ref() != Some(*alt)).cloned();
            media.push(ExportMediaEntry { kind: slide.file.kind.as_str(), path, poster, alt });
        }

        let avatar_path = match &avatar {
            Some(avatar) => match open.avatars.get(&post.user.username) {
                Some(known) => Some(known.clone()),
                None => {
                    let stem = text::avatar_stem(&post.user.username, &mut open.avatar_stems);
                    let path = format!("avatars/{stem}.{}", avatar.extension);
                    open.add_file(&path, avatar, stored, &mut buffer, cancel)?;
                    open.avatars.insert(post.user.username.clone(), path.clone());
                    Some(path)
                }
            },
            None => None,
        };

        let id = text::unique_id(&post.id, &mut used_ids);
        open.entries.push(build_post_entry(post, id, media, avatar_path));
        progress(ExportProgress { done: index + 1, total });
    }

    if let Some(last) = part.take() {
        finished.push(last.finish(options.now_ms, deflated)?);
    }
    Ok(WrittenParts { parts: finished, posts: number, problems })
}

/// Gives the finished parts their names: `{name}.zip`, or `{name}-part{k}.zip` when there are
/// several. A file that has such a name is replaced. A part that was renamed is listed in
/// `pending` until all are, so that a failure half-way removes it again.
fn publish_parts(
    parts: Vec<FinishedPart>,
    target_dir: &Path,
    name: &str,
    pending: &mut PendingFiles,
) -> Result<Vec<ExportArchive>, ExportError> {
    let count = parts.len();
    let mut archives = Vec::with_capacity(count);
    for (index, part) in parts.into_iter().enumerate() {
        let file_name = archive_file_name(name, index + 1, count);
        let path = target_dir.join(&file_name);
        with_retries(|| fs::rename(&part.temp_path, &path)).map_err(|error| {
            ExportError::new(ExportErrorCode::WriteFailed, messages::archive_not_written_to(&file_name, &error.to_string()))
        })?;
        pending.forget(&part.temp_path);
        pending.add(path.clone());
        archives.push(ExportArchive { path, name: file_name, posts: part.posts });
    }
    pending.keep_all();
    Ok(archives)
}

/// The clone's `downloadMessage`: what a finished download tells the user
/// (`messages::download_finished`). Show it for `download_toast_ms(result)`.
pub fn download_message(result: &ExportResult) -> String {
    let names: Vec<String> = result.archives.iter().map(|archive| archive.name.clone()).collect();
    messages::download_finished(result.posts, &names, &result.problems)
}

/// How long the finish toast stays: 9000 ms when something was left out, else 4000 ms.
pub fn download_toast_ms(result: &ExportResult) -> u64 {
    messages::download_toast_ms(result.problems.len())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{Comment, MediaItem, PostKey, User};

    /// 2026-11-11T10:00:00Z, the instant of the clone's export tests.
    const NOW_MS: i64 = 1_794_391_200_000;
    /// 2026-11-01T15:04:05Z.
    const TAKEN_MS: i64 = 1_793_545_445_000;

    fn media_entry(kind: &'static str, path: &str) -> ExportMediaEntry {
        ExportMediaEntry { kind, path: path.to_owned(), poster: None, alt: None }
    }

    /// A loose picture as `catalog` builds it: no link, no counts, the file name as sub-line.
    fn loose_post(id: &str, path: &Path) -> Post {
        let file_name = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        Post {
            key: PostKey::for_file(path),
            id: id.to_owned(),
            permalink: None,
            media: vec![MediaItem {
                kind: MediaKind::Image,
                path: path.to_path_buf(),
                manifest_path: file_name.clone(),
                poster: None,
                alt: None,
                bytes: 0,
            }],
            user: User::named("unknown"),
            caption: String::new(),
            original_caption: String::new(),
            comment_count: 0,
            like_count: None,
            taken_at: None,
            media_type: MediaType::Image,
            alt: None,
            location: None,
            context: Some(file_name),
            comments: Vec::new(),
            origin: crate::model::PostOrigin::File,
        }
    }

    /// The first post of the clone's export test (export.test.ts `SOURCE`), with a second
    /// comment that has neither text nor time.
    fn full_post() -> Post {
        let mut post = loose_post("C9aKq2LtXy1", Path::new("posts/post1_1.jpg"));
        post.permalink = Some("https://www.instagram.com/p/C9aKq2LtXy1/".to_owned());
        post.user = User {
            username: "lumen.atlas".to_owned(),
            full_name: Some("Lumen Atlas".to_owned()),
            verified: Some(true),
            avatar: None,
            profile_url: Some("https://www.instagram.com/lumen.atlas/".to_owned()),
        };
        post.caption = "Two mornings on the ridge 🌄\n\n#sunrise".to_owned();
        post.comment_count = 12;
        post.like_count = Some(340);
        post.taken_at = Some(TAKEN_MS);
        post.media_type = MediaType::Carousel;
        post.alt = Some("Cover alt".to_owned());
        post.location = Some("Chamonix, France".to_owned());
        post.context = None;
        post.comments = vec![
            Comment {
                id: "C9aKq2LtXy1-c1".to_owned(),
                username: "someone".to_owned(),
                text: "Wow".to_owned(),
                likes: 3,
                taken_at: Some(TAKEN_MS + 3_355_000),
            },
            Comment {
                id: "C9aKq2LtXy1-c2".to_owned(),
                username: "other".to_owned(),
                text: String::new(),
                likes: 0,
                taken_at: None,
            },
        ];
        post
    }

    fn keys(value: &serde_json::Value) -> Vec<&str> {
        value.as_object().map(|object| object.keys().map(String::as_str).collect()).unwrap_or_default()
    }

    #[test]
    fn the_instants_of_the_tests_are_what_their_comments_say() {
        assert_eq!(text::iso_time(NOW_MS).as_deref(), Some("2026-11-11T10:00:00Z"));
        assert_eq!(text::iso_time(TAKEN_MS).as_deref(), Some("2026-11-01T15:04:05Z"));
    }

    #[test]
    fn archive_names_follow_the_clone() {
        assert_eq!(archive_file_name("instagram-export", 1, 1), "instagram-export.zip");
        assert_eq!(archive_file_name("instagram-export", 1, 0), "instagram-export.zip");
        assert_eq!(archive_file_name("instagram-export", 1, 3), "instagram-export-part1.zip");
        assert_eq!(archive_file_name("instagram-export", 3, 3), "instagram-export-part3.zip");
        assert_eq!(single_post_export_name("FXTbasic004"), "instagram-export-FXTbasic004");
        assert_eq!(single_post_export_name("a b/c"), "instagram-export-a_b_c");
        assert_eq!(
            archive_file_name(&single_post_export_name("C8ZmP4RvNd7"), 1, 1),
            "instagram-export-C8ZmP4RvNd7.zip"
        );
    }

    #[test]
    fn the_save_path_is_split_into_folder_and_name() {
        let folder = Path::new("Out").join("Trips");
        let split = |file_name: &str| split_destination(&folder.join(file_name));
        assert_eq!(split("my trip.zip"), Some((folder.clone(), "my trip".to_owned())));
        assert_eq!(split("my trip.ZIP"), Some((folder.clone(), "my trip".to_owned())));
        assert_eq!(split("my trip.Zip"), Some((folder.clone(), "my trip".to_owned())));
        // Only one ".zip" is taken off, and a name without it is kept whole.
        assert_eq!(split("a.zip.zip"), Some((folder.clone(), "a.zip".to_owned())));
        assert_eq!(split("backup.2026"), Some((folder.clone(), "backup.2026".to_owned())));
        assert_eq!(split("朝.zip"), Some((folder.clone(), "朝".to_owned())));
        assert_eq!(split("朝"), Some((folder.clone(), "朝".to_owned())));
        assert_eq!(split("zip"), Some((folder.clone(), "zip".to_owned())));
        // Nothing an archive could be named after.
        assert_eq!(split(".zip"), None);
        assert_eq!(split_destination(Path::new("")), None);
        assert_eq!(split_destination(Path::new("..")), None);
        // A bare file name is saved into the current folder.
        assert_eq!(split_destination(Path::new("trip.zip")), Some((PathBuf::from("."), "trip".to_owned())));
    }

    #[test]
    fn an_archive_name_is_a_plain_file_name() {
        for name in ["instagram-export", "instagram-export-FXTbasic004", "my trip (2026)", "朝 🥐", "a.b", "..."] {
            assert!(is_archive_name(name), "{name}");
        }
        for name in ["", ".", "..", "a/b", r"a\b", "C:trip", "what?", "a*b", "a|b", "<a>", "\"a\"", "a\nb", "a\0"] {
            assert!(!is_archive_name(name), "{name:?}");
        }
        // The names the app itself makes always qualify.
        assert!(is_archive_name(&single_post_export_name("weird id/with <chars>")));
        assert!(is_archive_name(DEFAULT_EXPORT_NAME));
    }

    #[cfg(windows)]
    #[test]
    fn the_save_path_of_a_windows_dialog_is_split() {
        assert_eq!(
            split_destination(Path::new(r"C:\Out\my trip.zip")),
            Some((PathBuf::from(r"C:\Out"), "my trip".to_owned()))
        );
        assert_eq!(split_destination(Path::new(r"C:\trip.zip")), Some((PathBuf::from(r"C:\"), "trip".to_owned())));
        assert_eq!(split_destination(Path::new(r"C:\")), None);
    }

    #[test]
    fn entry_times_are_local_with_even_seconds() {
        let civil = |year, month, day, hour, minute, second| CivilTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
            millisecond: 0,
        };
        // UTC, a whole even second.
        assert_eq!(entry_time(NOW_MS, 0), civil(2026, 11, 11, 10, 0, 0));
        // Odd seconds and milliseconds are rounded down (DOS times count two-second steps).
        assert_eq!(entry_time(NOW_MS + 5_999, 0), civil(2026, 11, 11, 10, 0, 4));
        assert_eq!(entry_time(NOW_MS + 59_417, 0), civil(2026, 11, 11, 10, 0, 58));
        // Local time: east of UTC is later, west is earlier, across midnight too.
        assert_eq!(entry_time(NOW_MS, 60), civil(2026, 11, 11, 11, 0, 0));
        assert_eq!(entry_time(NOW_MS, 14 * 60 + 30), civil(2026, 11, 12, 0, 30, 0));
        assert_eq!(entry_time(NOW_MS, -11 * 60), civil(2026, 11, 10, 23, 0, 0));
        // Outside the years a ZIP entry can hold.
        assert_eq!(entry_time(0, 0), civil(1980, 1, 1, 0, 0, 0));
        assert_eq!(entry_time(i64::MIN, -900), civil(1980, 1, 1, 0, 0, 0));
        assert_eq!(entry_time(4_500_000_000_000, 0), civil(2107, 12, 31, 23, 59, 58));
        assert_eq!(entry_time(i64::MAX, 900), civil(2107, 12, 31, 23, 59, 58));
        // The first and the last second that are kept as they are.
        let first = text::epoch_ms_from_civil(&civil(1980, 1, 1, 0, 0, 0)).expect("a valid date");
        let last = text::epoch_ms_from_civil(&civil(2107, 12, 31, 23, 59, 59)).expect("a valid date");
        assert_eq!(entry_time(first, 0), civil(1980, 1, 1, 0, 0, 0));
        assert_eq!(entry_time(first - 1, 0), civil(1980, 1, 1, 0, 0, 0));
        assert_eq!(entry_time(first, -1), civil(1980, 1, 1, 0, 0, 0));
        assert_eq!(entry_time(last, 0), civil(2107, 12, 31, 23, 59, 58));
        assert_eq!(entry_time(last + 1000, 0), civil(2107, 12, 31, 23, 59, 58));
    }

    #[test]
    fn the_archive_writer_takes_every_entry_time() {
        let time = zip_entry_time(NOW_MS + 5_999, 60);
        assert_eq!(
            (time.year(), time.month(), time.day(), time.hour(), time.minute(), time.second()),
            (2026, 11, 11, 11, 0, 4)
        );
        let earliest = zip_entry_time(i64::MIN, 0);
        assert_eq!((earliest.year(), earliest.month(), earliest.day()), (1980, 1, 1));
        let latest = zip_entry_time(i64::MAX, 0);
        assert_eq!(
            (latest.year(), latest.month(), latest.day(), latest.hour(), latest.minute(), latest.second()),
            (2107, 12, 31, 23, 59, 58)
        );
    }

    #[test]
    fn the_media_type_goes_by_what_was_written() {
        let image = media_entry("image", "posts/post1.jpg");
        let video = media_entry("video", "posts/post1.mp4");
        // More than one entry is a carousel, whatever was declared.
        for declared in [MediaType::Image, MediaType::Carousel, MediaType::Video] {
            assert_eq!(export_media_type(&[image.clone(), video.clone()], declared), "carousel");
        }
        // A carousel that lost slides down to one goes by the slide that is left.
        assert_eq!(export_media_type(std::slice::from_ref(&image), MediaType::Carousel), "image");
        assert_eq!(export_media_type(std::slice::from_ref(&video), MediaType::Carousel), "video");
        assert_eq!(export_media_type(std::slice::from_ref(&image), MediaType::Image), "image");
        assert_eq!(export_media_type(std::slice::from_ref(&video), MediaType::Image), "video");
        // A video post saved as its poster only stays a video.
        assert_eq!(export_media_type(std::slice::from_ref(&image), MediaType::Video), "video");
        // No entry cannot happen in an export; it must not panic.
        assert_eq!(export_media_type(&[], MediaType::Image), "image");
        assert_eq!(export_media_type(&[], MediaType::Video), "video");
    }

    #[test]
    fn a_post_entry_has_the_canonical_field_order() {
        let media = vec![
            media_entry("image", "posts/post1_1.jpg"),
            ExportMediaEntry {
                kind: "video",
                path: "posts/post1_2.mp4".to_owned(),
                poster: Some("posts/post1_2_poster.png".to_owned()),
                alt: Some("The climb".to_owned()),
            },
        ];
        let avatar = Some("avatars/lumen.atlas.jpg".to_owned());
        let entry = build_post_entry(&full_post(), "C9aKq2LtXy1".to_owned(), media, avatar);
        let value = serde_json::to_value(&entry).expect("an entry is plain data");
        // export.test.ts:140-142.
        assert_eq!(
            keys(&value),
            [
                "id",
                "url",
                "media",
                "user",
                "caption",
                "comment_count",
                "like_count",
                "taken_at",
                "media_type",
                "alt",
                "location",
                "comments",
            ]
        );
        assert_eq!(keys(&value["media"][0]), ["type", "path"]);
        assert_eq!(keys(&value["media"][1]), ["type", "path", "poster", "alt"]);
        assert_eq!(keys(&value["user"]), ["username", "full_name", "verified", "avatar", "profile_url"]);
        assert_eq!(keys(&value["comments"][0]), ["username", "text", "like_count", "taken_at"]);
        assert_eq!(keys(&value["comments"][1]), ["username", "text", "like_count"]);
        assert_eq!(
            value,
            serde_json::json!({
                "id": "C9aKq2LtXy1",
                "url": "https://www.instagram.com/p/C9aKq2LtXy1/",
                "media": [
                    { "type": "image", "path": "posts/post1_1.jpg" },
                    {
                        "type": "video",
                        "path": "posts/post1_2.mp4",
                        "poster": "posts/post1_2_poster.png",
                        "alt": "The climb"
                    }
                ],
                "user": {
                    "username": "lumen.atlas",
                    "full_name": "Lumen Atlas",
                    "verified": true,
                    "avatar": "avatars/lumen.atlas.jpg",
                    "profile_url": "https://www.instagram.com/lumen.atlas/"
                },
                "caption": "Two mornings on the ridge 🌄\n\n#sunrise",
                "comment_count": 12,
                "like_count": 340,
                "taken_at": "2026-11-01T15:04:05Z",
                "media_type": "carousel",
                "alt": "Cover alt",
                "location": "Chamonix, France",
                "comments": [
                    { "username": "someone", "text": "Wow", "like_count": 3, "taken_at": "2026-11-01T16:00:00Z" },
                    { "username": "other", "text": "", "like_count": 0 }
                ]
            })
        );
    }

    #[test]
    fn optional_fields_are_left_out_and_an_unknown_like_count_is_null() {
        let mut post = loose_post("IMG_0042", Path::new("IMG_0042.jpg"));
        post.caption = "The description typed in the editor.".to_owned();
        post.taken_at = Some(TAKEN_MS);
        let entry = build_post_entry(&post, "IMG_0042".to_owned(), vec![media_entry("image", "posts/post1.jpg")], None);
        let value = serde_json::to_value(&entry).expect("an entry is plain data");
        // No "url", "alt", "location" or "comments"; the file name sub-line is not exported.
        assert_eq!(
            keys(&value),
            ["id", "media", "user", "caption", "comment_count", "like_count", "taken_at", "media_type"]
        );
        assert_eq!(keys(&value["user"]), ["username"]);
        assert!(value["like_count"].is_null());

        // spec-export G.2: the entry of a loose picture, byte for byte.
        let text = manifest_json(&build_manifest(NOW_MS, vec![entry]));
        let expected = r#"{
  "format": "instagram-export",
  "version": 1,
  "exported_at": "2026-11-11T10:00:00Z",
  "source": {
    "type": "posts"
  },
  "posts": [
    {
      "id": "IMG_0042",
      "media": [
        {
          "type": "image",
          "path": "posts/post1.jpg"
        }
      ],
      "user": {
        "username": "unknown"
      },
      "caption": "The description typed in the editor.",
      "comment_count": 0,
      "like_count": null,
      "taken_at": "2026-11-01T15:04:05Z",
      "media_type": "image"
    }
  ]
}
"#;
        assert_eq!(text, expected);
    }

    #[test]
    fn empty_texts_and_unknown_times_are_left_out() {
        let mut post = full_post();
        post.user.full_name = Some(String::new());
        post.user.verified = Some(false);
        post.alt = Some(String::new());
        post.location = Some(String::new());
        post.caption = String::new();
        post.comments.clear();
        // An instant without a four-digit year has no ISO form.
        post.taken_at = Some(i64::MAX);
        let entry = build_post_entry(&post, "x".to_owned(), vec![media_entry("video", "posts/post1.mp4")], None);
        assert_eq!(entry.user.full_name, None);
        assert_eq!(entry.user.verified, Some(false));
        assert_eq!(entry.user.avatar, None);
        assert_eq!(entry.alt, None);
        assert_eq!(entry.location, None);
        assert_eq!(entry.taken_at, None);
        assert_eq!(entry.caption, "");
        assert_eq!(entry.media_type, "video");
        let value = serde_json::to_value(&entry).expect("an entry is plain data");
        assert_eq!(
            keys(&value),
            ["id", "url", "media", "user", "caption", "comment_count", "like_count", "media_type"]
        );
        assert_eq!(keys(&value["user"]), ["username", "verified", "profile_url"]);
        // White space is text: only the empty string is left out, and nothing is trimmed.
        post.location = Some(" ".to_owned());
        post.caption = "  two\n".to_owned();
        let entry = build_post_entry(&post, "x".to_owned(), vec![media_entry("image", "posts/post1.jpg")], None);
        assert_eq!(entry.location.as_deref(), Some(" "));
        assert_eq!(entry.caption, "  two\n");
    }

    #[test]
    fn links_are_written_only_when_the_validator_accepts_them() {
        let entry_for = |permalink: Option<&str>, profile: Option<&str>| {
            let mut post = full_post();
            post.permalink = permalink.map(str::to_owned);
            post.user.profile_url = profile.map(str::to_owned);
            build_post_entry(&post, "x".to_owned(), vec![media_entry("image", "posts/post1.jpg")], None)
        };
        let kept =
            entry_for(Some("https://www.instagram.com/reel/C7TtW0YhBs3/"), Some("https://instagram.com/mira.films"));
        assert_eq!(kept.url.as_deref(), Some("https://www.instagram.com/reel/C7TtW0YhBs3/"));
        assert_eq!(kept.user.profile_url.as_deref(), Some("https://instagram.com/mira.films"));
        // Another site, http, a profile where a post belongs and the reverse: left out, which the
        // validator answers with the "no url" warning instead of an error.
        for (permalink, profile) in [
            ("https://example.com/p/C7TtW0YhBs3/", "https://example.com/mira.films/"),
            ("http://www.instagram.com/p/C7TtW0YhBs3/", "http://www.instagram.com/mira.films/"),
            ("https://www.instagram.com/mira.films/", "https://www.instagram.com/p/C7TtW0YhBs3/"),
            ("https://www.instagram.com.evil.example/p/C7TtW0YhBs3/", "https://www.instagram.com/explore/"),
            ("", ""),
        ] {
            let dropped = entry_for(Some(permalink), Some(profile));
            assert_eq!(dropped.url, None, "{permalink}");
            assert_eq!(dropped.user.profile_url, None, "{profile}");
        }
        let none = entry_for(None, None);
        assert_eq!((none.url, none.user.profile_url), (None, None));
    }

    #[test]
    fn post_json_is_what_json_stringify_writes() {
        let mut post = full_post();
        // Emoji, CJK, CR LF, quotes, a backslash, a slash, a tab, backspace, form feed, two other
        // control characters, DEL, the two Unicode line separators and an accented letter.
        post.caption =
            "Two mornings 🌄 朝ごはん\r\n\"quoted\" \\ back/slash\ttab \u{8}\u{c} \u{1}\u{1f} \u{7f} \u{2028}\u{2029} é"
                .to_owned();
        let media = vec![
            media_entry("image", "posts/post1_1.jpg"),
            ExportMediaEntry {
                kind: "video",
                path: "posts/post1_2.mp4".to_owned(),
                poster: Some("posts/post1_2_poster.png".to_owned()),
                alt: Some("The climb".to_owned()),
            },
        ];
        let avatar = Some("avatars/lumen.atlas.jpg".to_owned());
        let first = build_post_entry(&post, "C9aKq2LtXy1".to_owned(), media, avatar);
        let second = build_post_entry(
            &loose_post("IMG_0042", Path::new("IMG_0042.jpg")),
            "IMG_0042".to_owned(),
            vec![media_entry("image", "posts/post2.jpg")],
            None,
        );
        let text = manifest_json(&build_manifest(NOW_MS + 417, vec![first, second]));

        // What Node.js prints for the same data with JSON.stringify(manifest, null, 2) + "\n".
        // The caption line is spelled with Rust escapes, because it holds invisible characters.
        let caption = concat!(
            "\"Two mornings 🌄 朝ごはん\\r\\n\\\"quoted\\\" \\\\ back/slash\\ttab \\b\\f ",
            "\\u0001\\u001f \u{7f} \u{2028}\u{2029} é\""
        );
        let expected = r#"{
  "format": "instagram-export",
  "version": 1,
  "exported_at": "2026-11-11T10:00:00.417Z",
  "source": {
    "type": "posts"
  },
  "posts": [
    {
      "id": "C9aKq2LtXy1",
      "url": "https://www.instagram.com/p/C9aKq2LtXy1/",
      "media": [
        {
          "type": "image",
          "path": "posts/post1_1.jpg"
        },
        {
          "type": "video",
          "path": "posts/post1_2.mp4",
          "poster": "posts/post1_2_poster.png",
          "alt": "The climb"
        }
      ],
      "user": {
        "username": "lumen.atlas",
        "full_name": "Lumen Atlas",
        "verified": true,
        "avatar": "avatars/lumen.atlas.jpg",
        "profile_url": "https://www.instagram.com/lumen.atlas/"
      },
      "caption": @CAPTION@,
      "comment_count": 12,
      "like_count": 340,
      "taken_at": "2026-11-01T15:04:05Z",
      "media_type": "carousel",
      "alt": "Cover alt",
      "location": "Chamonix, France",
      "comments": [
        {
          "username": "someone",
          "text": "Wow",
          "like_count": 3,
          "taken_at": "2026-11-01T16:00:00Z"
        },
        {
          "username": "other",
          "text": "",
          "like_count": 0
        }
      ]
    },
    {
      "id": "IMG_0042",
      "media": [
        {
          "type": "image",
          "path": "posts/post2.jpg"
        }
      ],
      "user": {
        "username": "unknown"
      },
      "caption": "",
      "comment_count": 0,
      "like_count": null,
      "media_type": "image"
    }
  ]
}
"#
        .replace("@CAPTION@", caption);
        assert_eq!(text, expected);
        // UTF-8 without a byte order mark, exactly one line feed at the end.
        assert!(text.starts_with('{'));
        assert!(text.ends_with("}\n") && !text.ends_with("\n\n"));
    }

    #[test]
    fn a_manifest_without_posts_and_large_counts_print_as_json_stringify_does() {
        assert_eq!(
            manifest_json(&build_manifest(NOW_MS, Vec::new())),
            concat!(
                "{\n  \"format\": \"instagram-export\",\n  \"version\": 1,\n",
                "  \"exported_at\": \"2026-11-11T10:00:00Z\",\n  \"source\": {\n    \"type\": \"posts\"\n  },\n",
                "  \"posts\": []\n}\n"
            )
        );
        // Counts are whole numbers: never "0.0", never an exponent.
        let mut post = full_post();
        post.comment_count = 1_234_567;
        post.like_count = Some(9_007_199_254_740_991);
        let entry = build_post_entry(&post, "x".to_owned(), vec![media_entry("image", "posts/post1.jpg")], None);
        let text = manifest_json(&build_manifest(NOW_MS, vec![entry]));
        assert!(text.contains("\"comment_count\": 1234567,\n"));
        assert!(text.contains("\"like_count\": 9007199254740991,\n"));
        // An instant that has no ISO form falls back to the epoch.
        assert_eq!(build_manifest(i64::MAX, Vec::new()).exported_at, "1970-01-01T00:00:00Z");
    }

    #[test]
    fn the_finish_toast_follows_the_clone() {
        let archive = |name: &str, posts| ExportArchive { path: PathBuf::from(name), name: name.to_owned(), posts };
        let one = ExportResult {
            archives: vec![archive("instagram-export-FXTbasic005.zip", 1)],
            posts: 1,
            problems: Vec::new(),
        };
        assert_eq!(download_message(&one), "Downloaded 1 post as instagram-export-FXTbasic005.zip.");
        assert_eq!(download_toast_ms(&one), 4000);

        let parts = ExportResult {
            archives: vec![archive("instagram-export-part1.zip", 5000), archive("instagram-export-part2.zip", 1200)],
            posts: 6200,
            problems: vec![
                "Post 3 (a): \"posts/a.jpg\" is not a picture or video.".to_owned(),
                "Post 3 (a) was left out: none of its pictures or videos could be saved.".to_owned(),
            ],
        };
        assert_eq!(
            download_message(&parts),
            concat!(
                "Downloaded 6,200 posts in 2 archives (instagram-export-part1.zip …). ",
                "Left out: Post 3 (a): \"posts/a.jpg\" is not a picture or video. (and 1 more problem)"
            )
        );
        assert_eq!(download_toast_ms(&parts), 9000);

        let nothing = ExportResult {
            archives: Vec::new(),
            posts: 0,
            problems: vec!["Post 1 (a) was left out: none of its pictures or videos could be saved.".to_owned()],
        };
        assert_eq!(
            download_message(&nothing),
            "Nothing could be downloaded. Post 1 (a) was left out: none of its pictures or videos could be saved."
        );
        assert_eq!(download_toast_ms(&nothing), 9000);
    }

    /// A folder of its own under the system's temporary folder, removed when the test ends.
    struct TempFolder(PathBuf);

    impl TempFolder {
        fn new(tag: &str) -> TempFolder {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let unique = format!(
                "instagram-core-export-unit-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let path = std::env::temp_dir().join(unique);
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("a test folder can be created");
            TempFolder(path)
        }

        fn file_names(&self) -> Vec<String> {
            let mut names: Vec<String> = fs::read_dir(&self.0)
                .expect("the test folder can be listed")
                .map(|entry| entry.expect("a folder entry").file_name().to_string_lossy().into_owned())
                .collect();
            names.sort();
            names
        }
    }

    impl Drop for TempFolder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    /// 64 bytes that start like a JPEG (the clone's testkit `IMAGES.jpeg`).
    fn jpeg_bytes(fill: u8) -> Vec<u8> {
        let mut bytes = vec![fill; 64];
        bytes[..11].copy_from_slice(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00]);
        bytes
    }

    /// A poster source that writes a small JPEG beside the test's folder, or fails.
    struct FakePosters {
        folder: PathBuf,
        works: bool,
        asked: std::sync::Mutex<Vec<(PathBuf, String)>>,
    }

    impl PosterSource for FakePosters {
        fn poster_for(&self, video: &Path, mime: &str) -> Option<PathBuf> {
            self.asked.lock().expect("log").push((video.to_path_buf(), mime.to_owned()));
            if !self.works {
                return None;
            }
            let poster = self.folder.join("made-poster.jpg");
            fs::write(&poster, jpeg_bytes(9)).ok()?;
            Some(poster)
        }
    }

    /// The names and post.json of the one archive in `folder`.
    fn read_archive(folder: &Path) -> (Vec<String>, serde_json::Value) {
        let path = folder.join("export.zip");
        let mut archive = zip::ZipArchive::new(File::open(&path).expect("the archive")).expect("a zip");
        let names: Vec<String> = (0..archive.len()).filter_map(|index| archive.by_index(index).ok().map(|file| String::from_utf8_lossy(file.name_raw()).into_owned())).collect();
        let mut text = String::new();
        archive.by_name("post.json").expect("post.json").read_to_string(&mut text).expect("text");
        (names, serde_json::from_str(&text).expect("JSON"))
    }

    #[test]
    fn a_loose_video_is_exported_with_a_poster_from_the_cache() {
        let source = TempFolder::new("video-source");
        let target = TempFolder::new("video-target");
        // An M4V is an MP4: it is written as .mp4, as the clone's EXTENSIONS map does.
        let video = source.0.join("Beach.m4v");
        let mp4 = b"\x00\x00\x00\x14ftypM4V \x20\x20\x20\x20mp42 some video bytes";
        fs::write(&video, mp4).expect("a video");
        let mut post = loose_post("Beach", &video);
        post.media[0].kind = MediaKind::Video;
        post.media_type = MediaType::Video;
        let posters = FakePosters { folder: source.0.clone(), works: true, asked: std::sync::Mutex::new(Vec::new()) };
        let options = ExportOptions::new("export", NOW_MS, 0);
        let result = export_posts_with_posters(std::slice::from_ref(&post), &target.0, &options, &posters, &mut |_| {}, &AtomicBool::new(false)).expect("exports");
        assert!(result.problems.is_empty(), "{:?}", result.problems);
        assert_eq!(posters.asked.lock().expect("log").as_slice(), [(video.clone(), "video/mp4".to_owned())]);
        let (names, manifest) = read_archive(&target.0);
        assert_eq!(names, ["posts/post1.mp4", "posts/post1_poster.jpg", "post.json"]);
        let entry = &manifest["posts"][0];
        assert_eq!(entry["media_type"], "video");
        assert_eq!(entry["media"], serde_json::json!([{ "type": "video", "path": "posts/post1.mp4", "poster": "posts/post1_poster.jpg" }]));
        let mut archive = zip::ZipArchive::new(File::open(target.0.join("export.zip")).expect("archive")).expect("zip");
        let mut copied = Vec::new();
        archive.by_name("posts/post1.mp4").expect("the video").read_to_end(&mut copied).expect("bytes");
        assert_eq!(copied, mp4, "the video bytes are copied as they are");

        // Without a poster the video goes alone and nothing is reported.
        let empty = TempFolder::new("video-target-2");
        let failing = FakePosters { folder: source.0.clone(), works: false, asked: std::sync::Mutex::new(Vec::new()) };
        let result = export_posts_with_posters(std::slice::from_ref(&post), &empty.0, &options, &failing, &mut |_| {}, &AtomicBool::new(false)).expect("exports");
        assert!(result.problems.is_empty());
        let (names, manifest) = read_archive(&empty.0);
        assert_eq!(names, ["posts/post1.mp4", "post.json"]);
        assert_eq!(manifest["posts"][0]["media"], serde_json::json!([{ "type": "video", "path": "posts/post1.mp4" }]));

        // A video of an export is never given a made-up poster.
        let mut exported = post.clone();
        exported.origin = PostOrigin::Folder { manifest: source.0.join("post.json") };
        let third = TempFolder::new("video-target-3");
        export_posts_with_posters(&[exported], &third.0, &options, &posters, &mut |_| {}, &AtomicBool::new(false)).expect("exports");
        assert_eq!(read_archive(&third.0).0, ["posts/post1.mp4", "post.json"]);
        assert_eq!(posters.asked.lock().expect("log").len(), 1);
        assert!(target.file_names().iter().all(|name| name == "export.zip"));
    }

    #[test]
    fn a_post_larger_than_one_archive_is_left_out_without_taking_a_number() {
        let source = TempFolder::new("large-source");
        let target = TempFolder::new("large-target");
        let mut posts = Vec::new();
        for (id, fill) in [("big", 1_u8), ("small", 2)] {
            let path = source.0.join(format!("{id}.jpg"));
            fs::write(&path, jpeg_bytes(fill)).expect("a source file can be written");
            posts.push(loose_post(id, &path));
        }
        // The first post gets a second slide, so its files add up to 128 bytes.
        let second = source.0.join("big-2.jpg");
        fs::write(&second, jpeg_bytes(3)).expect("a source file can be written");
        let mut slide = posts[0].media[0].clone();
        slide.path = second;
        slide.manifest_path = "big-2.jpg".to_owned();
        posts[0].media.push(slide);

        let options = ExportOptions::new(DEFAULT_EXPORT_NAME, NOW_MS, 0);
        let mut reports = Vec::new();
        let result = export_with_post_limit(
            &posts,
            &target.0,
            &options,
            &NoPosters,
            100,
            &mut |progress| reports.push((progress.done, progress.total)),
            &AtomicBool::new(false),
        )
        .expect("the export succeeds");
        assert_eq!(result.problems, ["Post 1 (big) was left out: its files are too large for one archive."]);
        assert_eq!(result.posts, 1);
        assert_eq!(reports, [(0, 2), (1, 2), (2, 2)]);
        assert_eq!(target.file_names(), ["instagram-export.zip"]);

        let file = File::open(&result.archives[0].path).expect("the archive exists");
        let mut archive = zip::ZipArchive::new(io::BufReader::new(file)).expect("the archive can be read");
        let names: Vec<String> = (0..archive.len())
            .map(|index| archive.by_index(index).expect("an entry").name().expect("a name").into_owned())
            .collect();
        // The post that was written is number 1, although it is the second of the list.
        assert_eq!(names, ["posts/post1.jpg", "post.json"]);

        // Exactly at the limit a post is still written.
        let all = export_with_post_limit(&posts, &target.0, &options, &NoPosters, 128, &mut |_| {}, &AtomicBool::new(false))
            .expect("the export succeeds");
        assert_eq!((all.posts, all.problems.len()), (2, 0));
    }

    #[test]
    fn a_temporary_file_never_takes_an_existing_name() {
        let target = TempFolder::new("temp-names");
        let long_name = "n".repeat(300);
        let (first_path, first) = create_temp_file(&target.0, &long_name).expect("a temporary file can be created");
        let (second_path, second) = create_temp_file(&target.0, &long_name).expect("a temporary file can be created");
        drop((first, second));
        assert_ne!(first_path, second_path);
        let names = target.file_names();
        assert_eq!(names.len(), 2);
        for name in names {
            assert!(name.starts_with(&format!(".{}-", "n".repeat(TEMP_NAME_CHARS))), "{name}");
            assert!(name.ends_with(".tmp"), "{name}");
            assert!(name.len() < 100, "{name}");
        }
        // A missing folder is an error, not a panic.
        assert!(create_temp_file(&target.0.join("missing"), "x").is_err());
    }
}
