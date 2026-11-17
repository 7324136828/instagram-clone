//! Every user-visible sentence the core produces, in one place: warnings of the reader, problems
//! of the exporter, the toasts of a download, the editor's notes and errors, settings and storage
//! errors, and the few labels that are built from numbers.
//!
//! The wording is the reference web project's, copied verbatim wherever the clone has a sentence
//! (the source is named beside each one), because its tests and its documentation pin these
//! texts. Sentences marked NEW do not exist in the clone: it cannot open a picture without a
//! post.json and has no files of its own on disk. They follow the clone's style: a complete
//! sentence that ends with a period and quotes the offending item. Settings errors are the other
//! register of the clone: one line that names the path, without a trailing period.
//!
//! Typographic characters are part of the texts: "…" is U+2026, "·" is U+00B7.
//!
//! Numbers named `number` are 1-based positions (post 1 is the first entry of post.json).

use std::path::Path;

use crate::manifest::{MAX_CAPTION_CHARS, MAX_IMAGES_PER_POST, MAX_MANIFEST_BYTES, MAX_SOURCES, MAX_SOURCE_ENTRIES};
use crate::model::{Collection, MediaKind, Post, PostOrigin};
use crate::text::{format_bytes, format_count_exact, is_js_space, js_trim, truncate_utf16, utf16_len};

// --- Shared helpers -------------------------------------------------------------------------

/// The name of the app, as the window title and the About text show it.
pub const APP_TITLE: &str = "Instagram Windows App";

/// A toast stays this long unless said otherwise (App.tsx `showToast`).
pub const TOAST_MS: u64 = 2600;
/// A toast that reports a failure.
pub const TOAST_ERROR_MS: u64 = 5200;
/// The toast of a finished download.
pub const TOAST_DOWNLOAD_MS: u64 = 4000;
/// The toast of a finished download that left something out.
pub const TOAST_DOWNLOAD_PROBLEMS_MS: u64 = 9000;
/// At most this many toasts are visible; a repeated message replaces its older copy.
pub const MAX_TOASTS: usize = 3;
/// The warnings list shows this many entries before "Show all {N} warnings" (ImportModal.tsx).
pub const MAX_WARNINGS_SHOWN: usize = 5;

/// A value quoted in a warning is cut to this many UTF-16 code units (manifest.ts `shorten`).
const MAX_QUOTED_CHARS: usize = 80;
/// The JSON engine's message in "post.json is not valid JSON (...)" is cut to this length.
const MAX_JSON_DETAIL_CHARS: usize = 160;

/// The window title: "{file name} - Instagram Windows App", or just the app name when nothing
/// is shown.
pub fn window_title(file_name: Option<&str>) -> String {
    match file_name {
        Some(name) if !name.is_empty() => format!("{name} - {APP_TITLE}"),
        _ => APP_TITLE.to_owned(),
    }
}

/// What a file or folder is called in a sentence, in the banner and in the window title: its
/// last path component, or the whole path for a drive root ("C:\"), which has none. A name that
/// is not valid Unicode is shown with U+FFFD in place of what cannot be shown.
pub fn display_name(path: &Path) -> String {
    match path.file_name() {
        Some(name) => name.to_string_lossy().into_owned(),
        None => path.display().to_string(),
    }
}

/// The clone's `countLabel` (importReport.ts:31-33): the number with en-US thousands separators,
/// the word, and "s" unless the number is 1: "1 post", "1,200 posts", "0 posts".
pub fn count_label(count: u64, word: &str) -> String {
    let plural = if count == 1 { "" } else { "s" };
    format!("{} {word}{plural}", format_count_exact(count))
}

/// The clone's `describeError` (util.ts:11-15): the text of an error without a leading
/// "Error:" / "Uncaught Error:" (any letter case), trimmed; "unknown error" when nothing is left.
pub fn describe_error(text: &str) -> String {
    let mut rest = text;
    if let Some(after) = strip_prefix_ignore_case(rest, "uncaught") {
        let after_space = after.trim_start_matches(is_js_space);
        if after_space.len() < after.len() && strip_prefix_ignore_case(after_space, "error:").is_some() {
            rest = after_space;
        }
    }
    if let Some(after) = strip_prefix_ignore_case(rest, "error:") {
        rest = after;
    }
    let cleaned = js_trim(rest);
    if cleaned.is_empty() {
        "unknown error".to_owned()
    } else {
        cleaned.to_owned()
    }
}

fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &text[prefix.len()..])
}

/// The `{reason}` of an export problem sentence (spec-export A.8): `describe_error` with one
/// trailing "." removed, because the sentence adds its own.
fn problem_reason(error: &str) -> String {
    let described = describe_error(error);
    match described.strip_suffix('.') {
        Some(without) => without.to_owned(),
        None => described,
    }
}

/// The clone's `shorten` (manifest.ts:703): a quoted value longer than 80 UTF-16 code units is
/// cut to its first 77 and "...".
pub fn shorten(value: &str) -> String {
    if utf16_len(value) > MAX_QUOTED_CHARS {
        format!("{}...", truncate_utf16(value, MAX_QUOTED_CHARS - 3))
    } else {
        value.to_owned()
    }
}

fn image_or_video(is_video: bool) -> &'static str {
    if is_video {
        "video"
    } else {
        "image"
    }
}

// --- Reading an export: errors (spec-export C.1, C.2, C.5) ------------------------------------

/// core.ts:260. The limit is written without separators, as the clone does.
pub fn source_too_many_files(source_name: &str) -> String {
    format!("\"{source_name}\" contains more than {MAX_SOURCE_ENTRIES} files.")
}

/// source.ts (code `no-manifest`).
pub fn no_manifest(source_name: &str) -> String {
    format!("No post.json was found in \"{source_name}\". An export contains post.json next to a posts folder.")
}

/// core.ts:109: "post.json is larger than 32 MB."
pub fn manifest_too_large() -> String {
    format!("post.json is larger than {}.", format_bytes(MAX_MANIFEST_BYTES))
}

/// manifest.ts:785-786: "post.json is not valid JSON ({detail}).", the detail cut to 160 UTF-16
/// code units; without a detail just "post.json is not valid JSON."
pub fn manifest_invalid_json(detail: &str) -> String {
    if detail.is_empty() {
        "post.json is not valid JSON.".to_owned()
    } else {
        format!("post.json is not valid JSON ({}).", truncate_utf16(detail, MAX_JSON_DETAIL_CHARS))
    }
}

/// manifest.ts:723 (code `invalid-manifest`).
pub const MANIFEST_NO_POSTS_LIST: &str =
    "post.json does not contain a \"posts\" list. See instagram.md for the expected format.";

/// manifest.ts:730 (code `invalid-manifest`).
pub const MANIFEST_WRONG_SHAPE: &str =
    "post.json must be a JSON object with a \"posts\" list, or a JSON list of posts.";

/// manifest.ts:734 (code `no-posts`).
pub const MANIFEST_LISTS_NO_POSTS: &str = "post.json does not list any posts.";

/// core.ts:444-447 (code `no-posts`): no entry of post.json could be shown. `first_problem` is
/// the message of the earliest skipped entry's warning.
pub fn no_post_imported(total_entries: usize, first_problem: Option<&str>) -> String {
    let summary = if total_entries == 1 {
        "The only post in post.json could not be imported.".to_owned()
    } else {
        format!("None of the {total_entries} posts in post.json could be imported.")
    };
    match first_problem {
        Some(problem) if !problem.is_empty() => format!("{summary} First problem: {problem}"),
        _ => summary,
    }
}

/// core.ts:507 (code `read-failed`). The clone appends the cause after a colon without a period.
pub fn source_not_imported(source_name: &str, error: &str) -> String {
    format!("\"{source_name}\" could not be imported: {}", describe_error(error))
}

// --- Reading an export: warnings about post.json (spec-export C.2, C.4, C.7) ------------------

/// The name an entry without an author is shown under (manifest.ts:646-650), and the fallback
/// of the default username.
pub const UNKNOWN_USERNAME: &str = "unknown";

/// manifest.ts:763 (code `truncated`). Numbers are written without separators, as the clone does.
pub fn manifest_truncated(total_entries: usize, max_posts: usize) -> String {
    format!("post.json lists {total_entries} posts; only the first {max_posts} are imported.")
}

/// manifest.ts:618 (code `invalid-entry`).
pub fn entry_not_an_object(number: usize) -> String {
    format!("Post {number} is not a JSON object and was skipped.")
}

/// manifest.ts:535 (code `missing-image`): a media path that is a URL.
pub fn media_is_a_url(number: usize, value: &str, is_video: bool) -> String {
    let kind = if is_video { "Videos" } else { "Images" };
    format!(
        "Post {number}: \"{}\" is a URL. {kind} are not downloaded; the file must be inside the export.",
        shorten(value)
    )
}

/// manifest.ts:542 (code `invalid-entry`): a media path that leaves the export.
pub fn media_path_outside(number: usize, value: &str, is_video: bool) -> String {
    format!(
        "Post {number}: the {} path \"{}\" points outside the export and was ignored.",
        image_or_video(is_video),
        shorten(value)
    )
}

/// manifest.ts:561 (code `truncated`).
pub fn too_many_media(number: usize, count: usize) -> String {
    format!("Post {number} lists {count} pictures and videos; only the first {MAX_IMAGES_PER_POST} are imported.")
}

/// manifest.ts:628 (code `invalid-entry`). Only given when the entry produced no other warning.
pub fn entry_without_media(number: usize) -> String {
    format!("Post {number} has no image path and was skipped.")
}

/// manifest.ts:577 (code `duplicate-id`): a given id that was already used.
pub fn duplicate_id(number: usize, given_id: &str, used_id: &str) -> String {
    format!("Post {number} reuses the id \"{given_id}\"; it was imported as \"{used_id}\".")
}

/// manifest.ts:648 (code `invalid-entry`).
pub fn entry_without_user(number: usize) -> String {
    format!("Post {number} does not say who it is from; it is shown as \"{UNKNOWN_USERNAME}\".")
}

/// Which link of post.json a rejection warning is about (manifest.ts:440-451, 482).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkSubject {
    /// The `url` of entry `number`: "The link of post {number}".
    Post(usize),
    /// The profile link of the author of entry `number`: "The profile link of post {number}".
    PostProfile(usize),
    /// The profile link of the export-level user: "The profile link".
    Profile,
    /// `source.url`: "The source link of the export".
    Source,
}

/// manifest.ts:444-448 (code `unsafe-url`): a link that was present but dropped.
/// `wrong_instagram_page` is true when the link is a proper instagram.com link that does not
/// lead to the wanted kind of page (`urls::LinkRejection::WrongInstagramPage`):
/// "{subject} is an instagram.com link, but not to {target}, and was ignored."; otherwise
/// "{subject} is not a valid http(s) link and was ignored."
pub fn link_rejected(subject: LinkSubject, wrong_instagram_page: bool) -> String {
    let (what, target) = match subject {
        LinkSubject::Post(number) => (format!("The link of post {number}"), "an Instagram post"),
        LinkSubject::PostProfile(number) => (format!("The profile link of post {number}"), "an Instagram profile"),
        LinkSubject::Profile => ("The profile link".to_owned(), "an Instagram profile"),
        LinkSubject::Source => ("The source link of the export".to_owned(), "an Instagram profile, post or feed"),
    };
    if wrong_instagram_page {
        format!("{what} is an instagram.com link, but not to {target}, and was ignored.")
    } else {
        format!("{what} is not a valid http(s) link and was ignored.")
    }
}

// --- Reading files: warnings about one file (spec-export C.5) ---------------------------------
//
// `file` is the path of the file as the user knows it: inside an export its path relative to the
// export root, for a loose picture its file name.

/// core.ts:152, 202: "{file}" is larger than 64 MB (or 1 GB) and was skipped.
pub fn file_too_large(file: &str, limit_bytes: u64) -> String {
    format!("\"{file}\" is larger than {} and was skipped.", format_bytes(limit_bytes))
}

/// core.ts:161. The clone appends the cause after a colon without a period.
pub fn file_unreadable(file: &str, error: &str) -> String {
    format!("\"{file}\" could not be read: {}", describe_error(error))
}

/// core.ts:165.
pub fn file_empty(file: &str) -> String {
    format!("\"{file}\" is an empty file.")
}

/// core.ts:181: a well-known format that is not supported; `known` is one of `UNSUPPORTED_*`.
pub fn file_known_unsupported(file: &str, known: &str) -> String {
    format!("\"{file}\" is {known}, which is not supported. Convert it to MP4 (H.264) or WebM.")
}

/// core.ts:185: a file that post.json declares a video and that is not one.
pub fn file_not_a_video(file: &str) -> String {
    format!("\"{file}\" is not a supported video (MP4, MOV or WebM).")
}

/// core.ts:186: a file that is neither a supported picture nor a supported video.
pub fn file_not_supported(file: &str) -> String {
    format!(
        "\"{file}\" is not a supported image (JPEG, PNG, GIF, WebP, AVIF, BMP or HEIC/HEIF) or video (MP4, MOV or WebM)."
    )
}

/// core.ts:194: a video where only a picture belongs (poster, avatar, loose file).
pub fn file_is_a_video(file: &str) -> String {
    format!("\"{file}\" is a video, not a picture.")
}

/// The texts of `sniff::describe_unsupported_media` (sniff.ts:161-174), each the `known` of
/// `file_known_unsupported`. This one: an EBML file that is not WebM.
pub const UNSUPPORTED_MATROSKA: &str = "a Matroska (MKV) video";
/// An MP4-family file with an audio brand.
pub const UNSUPPORTED_AUDIO: &str = "an audio file (M4A)";
/// A RIFF file of type "AVI ".
pub const UNSUPPORTED_AVI: &str = "an AVI video";
/// An ASF container.
pub const UNSUPPORTED_WINDOWS_MEDIA: &str = "a Windows Media (WMV/ASF) video";
/// A file that starts "FLV".
pub const UNSUPPORTED_FLASH: &str = "a Flash (FLV) video";
/// An MPEG program stream or elementary video stream.
pub const UNSUPPORTED_MPEG: &str = "an MPEG video";
/// A file with a sync byte every 188 bytes.
pub const UNSUPPORTED_TRANSPORT_STREAM: &str = "an MPEG transport stream (TS) video";
/// A file that starts "OggS".
pub const UNSUPPORTED_OGG: &str = "an Ogg file";

/// core.ts:354: a file-level message about a media file of entry `number`.
pub fn for_post(number: usize, file_message: &str) -> String {
    format!("Post {number}: {file_message}")
}

/// core.ts:355 (code `missing-image`): a media path that resolves to no file.
pub fn media_not_in_export(number: usize, path: &str, is_video: bool) -> String {
    format!("Post {number}: the {} \"{path}\" is not in the export.", image_or_video(is_video))
}

/// core.ts:378-382: the poster of a video is missing; the post is kept.
pub fn poster_not_in_export(number: usize, video_path: &str, poster_path: &str) -> String {
    format!(
        "Post {number}: the video \"{video_path}\" is shown without its poster: \"{poster_path}\" is not in the export."
    )
}

/// core.ts:384: the poster of a video is itself a video.
pub fn poster_is_a_video(number: usize, video_path: &str, poster_path: &str) -> String {
    format!(
        "Post {number}: the video \"{video_path}\" is shown without its poster: \"{poster_path}\" is a video, not a picture."
    )
}

/// core.ts:385: any other problem with the poster file; `file_message` is a file-level message.
pub fn poster_problem(number: usize, video_path: &str, file_message: &str) -> String {
    format!("Post {number}: the video \"{video_path}\" is shown without its poster: {file_message}")
}

/// core.ts:411: given once per avatar path.
pub fn avatar_not_in_export(avatar_path: &str, username: &str) -> String {
    format!("The profile picture \"{avatar_path}\" of {username} is not in the export.")
}

/// core.ts:413.
pub fn avatar_is_a_video(username: &str, avatar_path: &str) -> String {
    format!("Profile picture of {username}: \"{avatar_path}\" is a video, not a picture.")
}

/// core.ts:414; `file_message` is a file-level message.
pub fn avatar_problem(username: &str, file_message: &str) -> String {
    format!("Profile picture of {username}: {file_message}")
}

// --- Opening pictures (NEW: the clone cannot open a picture without a post.json) --------------

/// NEW. Nothing was passed to open.
pub const NOTHING_TO_OPEN: &str = "There is nothing to open. Open pictures or a folder.";

/// NEW. A folder (or a selection) without a single supported picture. `name` is the folder name,
/// or the file name when one file was opened.
pub fn no_pictures_found(name: &str) -> String {
    format!("No pictures were found in \"{name}\".")
}

/// NEW. A path of the request that does not exist or cannot be listed.
pub fn path_not_opened(path: &str, error: &str) -> String {
    format!("\"{path}\" could not be opened: {}", describe_error(error))
}

/// NEW. One picture was picked in a folder with more than `MAX_SOURCE_ENTRIES` entries: it opens
/// on its own, without the other pictures of the folder.
pub fn neighbours_not_opened(folder: &str) -> String {
    format!(
        "\"{folder}\" contains more than {} files, so only the picked picture was opened.",
        format_count_exact(MAX_SOURCE_ENTRIES as u64)
    )
}

/// NEW. A description saved for a loose picture whose file has since been replaced by another
/// one of the same name (both size and modified time differ): it is not shown, and it stays in
/// descriptions.json.
pub fn description_of_earlier_file(file: &str) -> String {
    format!("The description saved for \"{file}\" belongs to an earlier file of that name and is not shown.")
}

/// NEW. Exports are shown on their own: pictures and other paths named in the same request as
/// export folders or archives are not opened.
pub fn not_opened_beside_export(name: &str) -> String {
    format!("\"{name}\" was not opened: an export is shown on its own.")
}

// --- Archives (archive.ts, zip.ts, tar.ts, core.ts; web brief 3.9) -----------------------------

/// archive.ts:28: how to get an archive the reader can open.
pub const HOW_TO_FIX_ARCHIVE: &str = "Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.";

/// archive.ts:49 (code `read-failed`): an archive file of 0 bytes.
pub fn archive_empty(name: &str) -> String {
    format!("\"{name}\" is empty.")
}

/// archive.ts:60: a known archive format that is not read; `kind` is "RAR", "7z", "xz", "bzip2"
/// or "Zstandard".
pub fn archive_kind_not_supported(kind: &str) -> String {
    format!("{kind} archives are not supported. {HOW_TO_FIX_ARCHIVE}")
}

/// archive.ts:68: no known signature, and an extension of an archive type that is not read.
pub fn archive_type_not_supported(name: &str) -> String {
    format!("\"{name}\" is an archive type that is not supported. {HOW_TO_FIX_ARCHIVE}")
}

/// archive.ts:70: no known signature and no telling extension.
pub fn not_a_supported_archive(name: &str) -> String {
    format!("\"{name}\" is not a supported archive. {HOW_TO_FIX_ARCHIVE}")
}

/// zip.ts:34 (code `read-failed`): any damage of a zip file; `detail` says what is wrong.
pub fn zip_damaged(detail: &str) -> String {
    format!("The zip file is damaged or incomplete ({detail}).")
}

/// zip.ts:88: a zip file split over several files (disks).
pub const ZIP_MULTI_PART: &str = "Multi-part zip files are not supported.";

/// zip.ts:108 (code `too-large`): the central directory is larger than 128 MiB.
pub const ZIP_TOO_MANY_FILES: &str = "The zip file lists too many files to import.";

/// zip.ts:175 (code `too-large`). The number is written without separators, as the clone does.
pub fn zip_more_files(limit: usize) -> String {
    format!("The zip file contains more than {limit} files.")
}

/// zip.ts:252: an encrypted member.
pub fn zip_password(path: &str) -> String {
    format!("\"{path}\" is password protected and cannot be read.")
}

/// zip.ts:257: a member compressed with another method than stored (0) or deflate (8).
pub fn zip_method(path: &str, method: u16) -> String {
    format!("\"{path}\" uses a zip compression method that is not supported ({method}). Re-create the zip with standard (deflate) compression.")
}

/// zip.ts:261, tar.ts:298 (code `too-large`): a member larger than what its use allows.
pub fn member_too_large(path: &str, limit_bytes: u64) -> String {
    format!("\"{path}\" is larger than {}.", format_bytes(limit_bytes))
}

/// tar.ts:323 (code `unsupported-archive`): the first block is no tar header.
pub const NOT_A_TAR: &str =
    "The file is not a tar archive. Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.";

/// tar.ts:326 (code `read-failed`): a later header with a wrong checksum.
pub const TAR_BAD_CHECKSUM: &str = "The tar archive is damaged (bad header checksum).";

/// tar.ts:332 (code `read-failed`): a size field that is no usable number.
pub const TAR_BAD_SIZE: &str = "The tar archive is damaged (bad size).";

/// tar.ts:392 (code `too-large`). The number is written without separators, as the clone does.
pub fn archive_more_files(limit: usize) -> String {
    format!("The archive contains more than {limit} files.")
}

/// tar.ts:147 (code `read-failed`): the gzip stream of a .tar.gz cannot be decoded.
pub fn archive_damaged(error: &str) -> String {
    format!("The archive is damaged or incomplete ({}).", describe_error(error))
}

/// tar.ts:111 (code `too-large`): a .tar.gz that unpacks to more than `limit_bytes`.
pub fn archive_unpacks_too_large(limit_bytes: u64) -> String {
    format!("The archive unpacks to more than {}, which is too large to import.", format_bytes(limit_bytes))
}

/// core.ts:242 (code `too-large`): the pictures and videos of the archives of one opening add up
/// to more than `limit_bytes`.
pub fn media_budget_exceeded(limit_bytes: u64) -> String {
    format!(
        "The pictures and videos of this import add up to more than {}. Import the export in smaller parts.",
        format_bytes(limit_bytes)
    )
}

/// NEW. A member of an archive could not be written into the archive cache (the disk is full,
/// the folder is not writable); the cause follows a colon without a period.
pub fn archive_cache_not_written(error: &str) -> String {
    format!("The archive could not be unpacked into the archive cache: {}", describe_error(error))
}

/// NEW. The archive cache would leave less than `min_bytes` free on its drive.
pub fn archive_cache_no_space(free_bytes: u64, min_bytes: u64) -> String {
    format!(
        "Not enough free disk space: {} is free on the drive of the archive cache and at least {} must stay free.",
        format_bytes(free_bytes),
        format_bytes(min_bytes)
    )
}

/// multi.ts:148 (code `truncated`): more archives and folders than one opening takes.
pub fn sources_truncated(selected: usize) -> String {
    format!("{selected} archives or folders were selected; only the first {MAX_SOURCES} were imported.")
}

/// multi.ts:152 (code `invalid-entry`): one of several sources failed; the others are shown.
pub fn source_failed(name: &str, message: &str) -> String {
    format!("\"{name}\" could not be imported: {message}")
}

/// multi.ts:140: every one of several sources failed. `reasons` is "{name}: {message}" of each
/// failure, joined with spaces (`failure_reasons`).
pub fn no_source_imported(selected: usize, reasons: &str) -> String {
    format!("None of the {selected} selected archives or folders could be imported. {reasons}")
}

/// multi.ts:139: the reasons of `no_source_imported`, "{name}: {message}" joined with spaces.
pub fn failure_reasons(failures: &[(String, String)]) -> String {
    failures.iter().map(|(name, message)| format!("{name}: {message}")).collect::<Vec<_>>().join(" ")
}

/// merge.ts:61-64 (code `merged-duplicate`): a post of a later source replaced one with the same
/// id from an earlier source.
pub fn merged_duplicate(id: &str, later: &str, earlier: &str) -> String {
    format!("The post \"{id}\" from {later} replaces the one with the same id from {earlier}.")
}

/// merge.ts:16: a warning of one of several merged sources, prefixed with its name.
pub fn from_source(name: &str, message: &str) -> String {
    format!("{name}: {message}")
}

/// merge.ts `describeSources` (merge.ts:22-33): "3 archives", "2 archives and 1 folder",
/// "1 folder and 2 imports", "Nothing" for none; three kinds are written as "1 archive, 1 folder
/// and 1 import".
pub fn describe_source_counts(archives: usize, folders: usize, imports: usize) -> String {
    let counted = |count: usize, noun: &str| format!("{count} {noun}{}", if count == 1 { "" } else { "s" });
    let mut parts: Vec<String> = Vec::new();
    if archives > 0 {
        parts.push(counted(archives, "archive"));
    }
    if folders > 0 {
        parts.push(counted(folders, "folder"));
    }
    if imports > 0 {
        parts.push(counted(imports, "import"));
    }
    match parts.split_last() {
        None => "Nothing".to_owned(),
        Some((only, [])) => only.clone(),
        Some((last, rest)) => format!("{} and {last}", rest.join(", ")),
    }
}

/// NEW. The name of the browser imports folder as a source of posts (stage W of the web brief).
pub const BROWSER_IMPORTS_NAME: &str = "Browser imports";

/// NEW. Further pictures can only be added to loose pictures, never to an export.
pub const CANNOT_ADD_TO_EXPORT: &str = "Pictures cannot be added to an export. Close it first.";

/// NEW. Export folders and archives can only be added to exports, never to loose pictures.
pub const CANNOT_ADD_EXPORTS_TO_PICTURES: &str = "Exports cannot be added to loose pictures. Close them first.";

/// The banner of a collection (see `showing_banner`, `showing_loose_banner`): the sentence for
/// what it holds, counting the loose pictures and videos apart.
pub fn collection_banner(collection: &Collection) -> String {
    if collection.is_export() {
        return showing_banner(collection.posts.len(), collection.total_entries, true, &collection.label);
    }
    let videos = collection.posts.iter().filter(|post| post.cover().is_some_and(|cover| cover.kind == MediaKind::Video)).count();
    showing_loose_banner(collection.posts.len() - videos, videos, &collection.label)
}

/// NEW (video brief 5.1). The banner of loose files: "Showing {N} pictures from {label}" when
/// they are all pictures, "Showing {N} videos from {label}" when they are all videos, and
/// "Showing {N} pictures and videos from {label}" for both (N counts all of them).
pub fn showing_loose_banner(pictures: usize, videos: usize, label: &str) -> String {
    match (pictures, videos) {
        (_, 0) => showing_banner(pictures, pictures, false, label),
        (0, _) => format!("Showing {} from {label}", count_label(videos as u64, "video")),
        _ => format!("Showing {} pictures and videos from {label}", format_count_exact((pictures + videos) as u64)),
    }
}

/// The banner above the feed (design brief, "Views"): "Showing {N} pictures from {label}" for
/// loose pictures, "Showing {N} posts from {label}" for an export, and, as in the clone
/// (App.tsx:1159-1252), "Showing {n} of {M} posts from {label}" when entries of post.json were
/// skipped. The UI may render `label` in bold.
pub fn showing_banner(shown: usize, total_entries: usize, is_export: bool, label: &str) -> String {
    let word = if is_export { "post" } else { "picture" };
    if is_export && total_entries > shown {
        format!(
            "Showing {} of {} from {label}",
            format_count_exact(shown as u64),
            count_label(total_entries as u64, word)
        )
    } else {
        format!("Showing {} from {label}", count_label(shown as u64, word))
    }
}

/// "{N} warning" / "{N} warnings": the banner button and the title of the warnings list.
pub fn warnings_label(count: usize) -> String {
    count_label(count as u64, "warning")
}

/// "Show all {N} warnings" (ImportModal.tsx); the toggle back is `SHOW_FEWER_WARNINGS`.
pub fn show_all_warnings(count: usize) -> String {
    format!("Show all {} warnings", format_count_exact(count as u64))
}

/// The toggle back from the full warnings list to the first `MAX_WARNINGS_SHOWN`.
pub const SHOW_FEWER_WARNINGS: &str = "Show fewer";

/// The accessible name of the cover (importedPosts.ts:132): "Photo by {username}" or
/// "Video by {username}".
pub fn cover_alt(is_video: bool, username: &str) -> String {
    format!("{} by {username}", if is_video { "Video" } else { "Photo" })
}

/// The accessible name of slide `number` (1-based, 2 or later) of a carousel
/// (importedPosts.ts:137): "Photo {number} of {total} by {username}".
pub fn slide_alt(is_video: bool, number: usize, total: usize, username: &str) -> String {
    format!("{} {number} of {total} by {username}", if is_video { "Video" } else { "Photo" })
}

/// The accessible name of a picture that could not be decoded (common.tsx:161-230):
/// "{alt} (image unavailable)", with "Photo" for an empty alt.
pub fn image_unavailable(alt: &str) -> String {
    format!("{} (image unavailable)", if alt.is_empty() { "Photo" } else { alt })
}

// --- Showing a picture (NEW: the browser decodes for the clone) -------------------------------

/// NEW. A picture file that cannot be decoded; shown on the failed-picture placeholder.
pub fn picture_not_shown(file: &str, reason: &str) -> String {
    format!("\"{file}\" could not be shown: {}.", problem_reason(reason))
}

/// NEW. A format only Windows Imaging Component can decode, and it could not (the codec is
/// probably not installed). The remedy is named, as the clone does where one exists: both
/// formats need the HEIF container extension, HEIC the HEVC codec behind it and AVIF the AV1 codec.
pub fn picture_needs_codec(file: &str, reason: &str) -> String {
    format!(
        "\"{file}\" could not be shown: Windows has no decoder for it ({}). HEIC and AVIF pictures need the HEIF Image Extensions from the Microsoft Store, together with the HEVC Video Extensions (HEIC) or the AV1 Video Extension (AVIF).",
        problem_reason(reason)
    )
}

/// NEW. A picture with more pixels than the app decodes.
pub fn picture_too_large(file: &str, width: u32, height: u32) -> String {
    format!(
        "\"{file}\" is too large to show ({} x {} pixels).",
        format_count_exact(u64::from(width)),
        format_count_exact(u64::from(height))
    )
}

/// NEW. The reason given on platforms without Windows Imaging Component.
pub const DECODER_ONLY_ON_WINDOWS: &str = "this format can only be decoded on Windows";

// --- Playing a video (NEW: video brief 5.14; the browser plays for the clone) -----------------

/// NEW. A video that cannot be played; `reason` is one of the reasons below.
pub fn video_cannot_play(file: &str, reason: &str) -> String {
    format!("\"{file}\" can't be played: {reason}")
}

/// NEW. Windows has no decoder for the codec of a video; `remedy` names the Store extension
/// (one of the `CODEC_REMEDY_*`).
pub fn video_needs_codec(file: &str, codec: &str, remedy: &str) -> String {
    format!("\"{file}\" can't be played: Windows has no decoder for its {codec} video. {remedy}")
}

/// NEW. The remedy for an HEVC (H.265) video, as iPhones record them.
pub const CODEC_REMEDY_HEVC: &str = "HEVC videos need the HEVC Video Extensions from the Microsoft Store.";
/// NEW. The remedy for a VP9 video.
pub const CODEC_REMEDY_VP9: &str = "VP9 videos need the VP9 Video Extensions from the Microsoft Store.";
/// NEW. The remedy for an AV1 video.
pub const CODEC_REMEDY_AV1: &str = "AV1 videos need the AV1 Video Extension from the Microsoft Store.";
/// NEW. The remedy for a WebM (VP8) video, or a WebM Windows has no reader for.
pub const CODEC_REMEDY_WEBM: &str = "WebM videos need the Web Media Extensions from the Microsoft Store.";
/// NEW. The remedy for a codec no Store extension brings.
pub const CODEC_REMEDY_CONVERT: &str = "Convert it to MP4 (H.264).";
/// NEW. The reason for a file with sound but without a picture.
pub const VIDEO_NO_PICTURE: &str = "it has no picture.";

/// NEW. The reason for a video Windows cannot read; `error` is the system's text.
pub fn video_damaged(error: &str) -> String {
    format!("the file is damaged or incomplete ({}).", problem_reason(error))
}

// --- Editing a description (spec-edit 3, 8, 13; design brief) ---------------------------------

/// The note under the field for a loose picture (design brief; NEW wording).
pub const EDIT_NOTE_PICTURE: &str = "Kept on this computer with the picture.";

/// The note under the field for a loose video (video brief 5.1; NEW wording).
pub const EDIT_NOTE_VIDEO: &str = "Kept on this computer with the video.";

/// The note under the field of the editor for `post`: `edit_note` of its origin, with the
/// video's wording for a loose video.
pub fn edit_note_for(post: &Post) -> &'static str {
    match post.origin {
        PostOrigin::File if post.cover().is_some_and(|cover| cover.kind == MediaKind::Video) => EDIT_NOTE_VIDEO,
        _ => edit_note(&post.origin),
    }
}

/// The note under the field for a post of an export (design brief; NEW wording).
pub const EDIT_NOTE_EXPORT: &str = "Kept on this computer. The export folder is not changed.";

/// The note under the field for a post of an export archive (web brief 3.6; NEW wording).
pub const EDIT_NOTE_ARCHIVE: &str = "Kept on this computer. The archive is not changed.";

/// The note under the field of the editor, by where the post comes from.
pub fn edit_note(origin: &PostOrigin) -> &'static str {
    match origin {
        PostOrigin::File => EDIT_NOTE_PICTURE,
        PostOrigin::Folder { .. } => EDIT_NOTE_EXPORT,
        PostOrigin::Archive { .. } => EDIT_NOTE_ARCHIVE,
    }
}

/// The counter of the editor (EditCaptionDialog.tsx; spec-edit 3.2): the UTF-16 length of the
/// raw, untrimmed field text and the limit, both with en-US separators: "38 / 10,000".
pub fn caption_counter(field_text: &str) -> String {
    format!(
        "{} / {}",
        format_count_exact(utf16_len(field_text) as u64),
        format_count_exact(MAX_CAPTION_CHARS as u64)
    )
}

/// The toast after a successful save (App.tsx:879-915), shown for `TOAST_MS`.
pub const DESCRIPTION_UPDATED: &str = "Description updated.";

/// The in-dialog error when the cause is unknown (App.tsx:902).
pub const DESCRIPTION_NOT_SAVED: &str = "The description could not be saved.";

/// The in-dialog error of a failed save (client.ts:111-131; spec-edit 3.7): "The description
/// could not be saved: {reason}." with the reason's trailing dots and white space removed. The
/// dialog stays open with the typed text.
pub fn description_not_saved(reason: &str) -> String {
    let reason = reason.trim_end_matches(|character: char| character == '.' || is_js_space(character));
    if reason.is_empty() {
        DESCRIPTION_NOT_SAVED.to_owned()
    } else {
        format!("The description could not be saved: {reason}.")
    }
}

// --- The app's own files (store) --------------------------------------------------------------

/// The clone's wording for a failed atomic write (library.py:1380-1407): "Could not update
/// {path}: {os error}". It becomes the `{reason}` of `description_not_saved`.
pub fn file_not_updated(path: &str, error: &str) -> String {
    format!("Could not update {path}: {}", describe_error(error))
}

/// NEW. descriptions.json exists but cannot be read from disk.
pub fn store_unreadable(path: &str, error: &str) -> String {
    format!("The saved descriptions in \"{path}\" could not be read: {}", describe_error(error))
}

/// NEW. descriptions.json was damaged: it was set aside and a new, empty one is used. Shown once
/// as a toast (`TOAST_ERROR_MS`) and written to the log.
pub fn store_damaged_notice(path: &str, backup_name: &str) -> String {
    format!(
        "The saved descriptions in \"{path}\" could not be read. The file was kept as \"{backup_name}\" and a new one was started."
    )
}

/// NEW. descriptions.json was damaged and could not be set aside; nothing is written over it.
pub fn store_damaged_not_set_aside(path: &str, error: &str) -> String {
    format!(
        "The saved descriptions in \"{path}\" are damaged and could not be set aside: {}",
        describe_error(error)
    )
}

// --- settings.json (spec-conventions 2.9: one line, names the path, no trailing period) -------

/// settings.py:154.
pub fn settings_unreadable(path: &str, error: &str) -> String {
    format!("Cannot read or create settings at {path}: {error}")
}

/// settings.py:157.
pub fn settings_not_an_object(path: &str) -> String {
    format!("Invalid settings at {path}: expected a JSON object")
}

/// settings.py:160-162.
pub fn settings_bad_data_directory(path: &str) -> String {
    format!("Invalid settings at {path}: data_directory must be a non-empty folder path")
}

/// settings.py:169.
pub fn settings_data_directory_error(path: &str, error: &str) -> String {
    format!("Invalid data_directory in settings at {path}: {error}")
}

/// NEW. The `{error}` of `settings_data_directory_error` when the data folder cannot be created
/// (a file is in the way, a name Windows does not allow, a device name such as NUL).
pub fn data_folder_not_created(folder: &str, error: &str) -> String {
    format!("the folder {folder} cannot be created: {}", describe_error(error))
}

/// NEW, in the pattern of the line above it: `default_username` is present and unusable.
pub fn settings_bad_default_username(path: &str) -> String {
    format!("Invalid settings at {path}: default_username must be a non-empty name")
}

/// NEW. Neither `APPDATA` nor `USERPROFILE` is set, so there is no place for settings.json.
pub const SETTINGS_FOLDER_UNKNOWN: &str =
    "Cannot locate the settings folder: neither APPDATA nor USERPROFILE is set";

// --- Download: problems of an export (spec-export A.8; export.ts) -----------------------------

/// The `{label}` of a problem sentence: "Post {number} ({id})", `number` being the 1-based
/// position of the post in the list that is exported and `id` its original (not de-duplicated) id.
pub fn export_label(number: usize, id: &str) -> String {
    format!("Post {number} ({id})")
}

/// Problem 1: a slide file cannot be read. `error` is the OS error text.
pub fn problem_file_unreadable(label: &str, path: &str, error: &str) -> String {
    format!("{label}: \"{path}\" could not be read: {}.", problem_reason(error))
}

/// Problem 2: the bytes of a slide do not sniff (also a 0-byte file).
pub fn problem_file_not_media(label: &str, path: &str) -> String {
    format!("{label}: \"{path}\" is not a picture or video.")
}

/// Problem 3: a poster cannot be read.
pub fn problem_poster_unreadable(label: &str, path: &str, error: &str) -> String {
    format!("{label}: The poster of \"{path}\" could not be read: {}.", problem_reason(error))
}

/// Problem 4: the bytes of a poster do not sniff.
pub fn problem_poster_not_media(label: &str, path: &str) -> String {
    format!("{label}: The poster of \"{path}\" is not a picture or video.")
}

/// Problem 5: a poster sniffs as a video.
pub fn problem_poster_is_a_video(label: &str, path: &str) -> String {
    format!("{label}: The poster of \"{path}\" is not a picture.")
}

/// Problem 6: the file failed but its poster is fine and was saved in its place.
pub fn problem_only_poster_saved(label: &str, path: &str) -> String {
    format!("{label}: only the poster of \"{path}\" was saved.")
}

/// Problem 7: no slide of the post is left.
pub fn problem_post_left_out(label: &str) -> String {
    format!("{label} was left out: none of its pictures or videos could be saved.")
}

/// Problem 8: the profile picture cannot be read.
pub fn problem_avatar_unreadable(label: &str, error: &str) -> String {
    format!("{label}: The profile picture could not be read: {}.", problem_reason(error))
}

/// Problem 9: the bytes of the profile picture do not sniff.
pub fn problem_avatar_not_media(label: &str) -> String {
    format!("{label}: The profile picture is not a picture or video.")
}

/// Problem 10: the profile picture sniffs as a video (the clone writes a lower-case "the" here).
pub fn problem_avatar_is_a_video(label: &str) -> String {
    format!("{label}: the profile picture is not a picture.")
}

/// Problem 11: the files of the post exceed `export::MAX_POST_BYTES`.
pub fn problem_post_too_large(label: &str) -> String {
    format!("{label} was left out: its files are too large for one archive.")
}

/// The message of a cancelled export (export.ts `cancelled`).
pub const DOWNLOAD_WAS_CANCELLED: &str = "The download was cancelled.";

/// The message of an export whose archive cannot be finished (export.ts).
pub const ARCHIVE_NOT_COMPLETED: &str = "The archive could not be completed.";

/// The message of an export whose archive cannot be written (export.ts); the cause follows a
/// colon without a period.
pub fn archive_not_written(error: &str) -> String {
    format!("The archive could not be written: {}", describe_error(error))
}

/// NEW, in the pattern of the line above: the finished archive could not get its name, because
/// a file of that name is held by another program or protected. The sentence names the file.
pub fn archive_not_written_to(file: &str, error: &str) -> String {
    format!("The archive could not be written to \"{file}\": {}", describe_error(error))
}

// --- Download: toasts and the banner button (spec-edit 8, 9; spec-export A.9) -----------------

/// Shown when a single-post download starts (`TOAST_MS`).
pub const PREPARING_DOWNLOAD: &str = "Preparing the download…";

/// Shown when a download is requested while one runs (`TOAST_MS`).
pub const DOWNLOAD_ALREADY_RUNNING: &str = "A download is already being prepared.";

/// Shown when the user cancelled a download (`TOAST_MS`).
pub const DOWNLOAD_CANCELLED: &str = "Download cancelled.";

/// Shown when a download failed for a reason without a sentence of its own (`TOAST_ERROR_MS`).
pub const DOWNLOAD_NOT_PREPARED: &str = "The download could not be prepared.";

/// The tooltip of "Download all" (App.tsx:1224-1233).
pub const DOWNLOAD_ALL_TOOLTIP: &str = "Download every post with its description as an Instagram export (.zip)";

/// The running "Download all" button: "Preparing {done} of {total}" (en-US numbers). The UI
/// appends `PROGRESS_CANCEL_SUFFIX`.
pub fn preparing_progress(done: usize, total: usize) -> String {
    format!("Preparing {} of {}", format_count_exact(done as u64), format_count_exact(total as u64))
}

/// " · Cancel" (with U+00B7), appended to `preparing_progress`.
pub const PROGRESS_CANCEL_SUFFIX: &str = " · Cancel";

/// The clone's `downloadMessage` (App.tsx:238-247): what a finished download tells the user.
///
/// * no archive: "Nothing could be downloaded. {first problem}" (just the first sentence when
///   there is no problem);
/// * one archive: "Downloaded {N post(s)} as {archive name}.";
/// * several: "Downloaded {N post(s)} in {K} archives ({first archive name} …).";
/// * with problems, appended: " Left out: {first problem}" and, for more than one,
///   " (and {M more problem(s)})".
pub fn download_finished(posts: usize, archive_names: &[String], problems: &[String]) -> String {
    let Some(first_archive) = archive_names.first() else {
        return match problems.first() {
            Some(problem) => js_trim(&format!("Nothing could be downloaded. {problem}")).to_owned(),
            None => "Nothing could be downloaded.".to_owned(),
        };
    };
    let count = count_label(posts as u64, "post");
    let saved = if archive_names.len() == 1 {
        format!("Downloaded {count} as {first_archive}.")
    } else {
        format!("Downloaded {count} in {} archives ({first_archive} …).", archive_names.len())
    };
    let Some(first_problem) = problems.first() else {
        return saved;
    };
    let more = if problems.len() > 1 {
        format!(" (and {})", count_label(problems.len() as u64 - 1, "more problem"))
    } else {
        String::new()
    };
    format!("{saved} Left out: {first_problem}{more}")
}

/// How long the toast of a finished download stays: `TOAST_DOWNLOAD_PROBLEMS_MS` when something
/// was left out, else `TOAST_DOWNLOAD_MS`.
pub fn download_toast_ms(problem_count: usize) -> u64 {
    if problem_count > 0 {
        TOAST_DOWNLOAD_PROBLEMS_MS
    } else {
        TOAST_DOWNLOAD_MS
    }
}

// --- Copying (design brief "Views"; spec-edit 2) ----------------------------------------------

/// The share button copied the post link.
pub const LINK_COPIED: &str = "Link copied.";
/// The share button copied the file path of a loose picture.
pub const PATH_COPIED: &str = "Path copied.";
/// "Copy link" of the action sheet succeeded (ActionSheet.tsx).
pub const LINK_COPIED_TO_CLIPBOARD: &str = "Link copied to clipboard.";
/// "Copy link" of the action sheet failed.
pub const LINK_NOT_COPIED: &str = "Could not copy the link.";

// --- Auto view (spec-viewer 4.3, 4.4, 4.7) ----------------------------------------------------

/// The lead of the setup dialog (AutoView.tsx:51-177), by the number of posts.
pub fn auto_view_lead(posts: usize) -> String {
    match posts {
        0 => "There are no posts to show yet.".to_owned(),
        1 => "Shows the post one after another and moves on by itself.".to_owned(),
        _ => format!(
            "Shows the {} posts one after another and moves on by itself.",
            format_count_exact(posts as u64)
        ),
    }
}

/// The hint under the time field of the setup dialog.
pub const AUTO_VIEW_HINT: &str =
    "Between 2 and 3600 seconds. A carousel shares the time between its slides (at least 2 seconds each).";

/// The validation error of the setup dialog.
pub const AUTO_VIEW_INTERVAL_ERROR: &str = "Enter a whole number of seconds from 2 to 3600.";

/// The title of the end screen.
pub const AUTO_VIEW_END_TITLE: &str = "End of posts";

/// The player's text when there is nothing to show (also the status text of that state).
pub const AUTO_VIEW_NO_POSTS: &str = "There are no posts to show.";

/// The text of the end screen: "You have seen all {N} posts." ("post" for exactly one).
pub fn auto_view_end_text(posts: usize) -> String {
    format!("You have seen all {}.", count_label(posts as u64, "post"))
}

/// The counter pill of the player: "{number} / {total}" with spaces (`number` is 1-based).
pub fn auto_view_counter(number: usize, total: usize) -> String {
    format!("{number} / {total}")
}

/// The clone's `announcement` (autoView.ts:336-343): the status line of the player, in this
/// priority: no posts, finished, paused, then "Post {number} of {total}, by {username}." (or
/// without the author when there is none). `number` is 1-based.
pub fn auto_view_announcement(
    number: usize,
    total: usize,
    finished: bool,
    paused: bool,
    username: Option<&str>,
) -> String {
    if total == 0 {
        return AUTO_VIEW_NO_POSTS.to_owned();
    }
    if finished {
        return "End of posts.".to_owned();
    }
    if paused {
        return "Auto view paused.".to_owned();
    }
    match username {
        Some(name) if !name.is_empty() => format!("Post {number} of {total}, by {name}."),
        _ => format!("Post {number} of {total}."),
    }
}

// --- Browser pages: settings (web brief 4.11; settings.py `_validate_ports`, `_host_list`) ------

/// NEW (settings.py style). `web_server` is not true or false.
pub fn settings_bad_web_server(path: &str) -> String {
    format!("Invalid settings at {path}: web_server must be true or false")
}

/// settings.py `_validate_ports`, with this app's file named.
pub fn settings_bad_port(path: &str, key: &str) -> String {
    format!("Invalid settings at {path}: {key} must be an integer from 1 to 65535")
}

/// settings.py `_validate_ports`.
pub fn settings_same_ports(path: &str) -> String {
    format!("Invalid settings at {path}: frontend_port and backend_port must be different")
}

/// NEW (settings.py style). `web_host` is not an IPv4 address.
pub fn settings_bad_web_host(path: &str) -> String {
    format!("Invalid settings at {path}: web_host must be an IPv4 address such as 127.0.0.1 or 0.0.0.0")
}

/// settings.py `_host_list`.
pub fn settings_hosts_not_a_list(path: &str) -> String {
    format!("Invalid allowed_hosts in settings at {path}: expected a list of host names")
}

/// settings.py `_host_list`.
pub fn settings_host_not_a_string(path: &str) -> String {
    format!("Invalid allowed_hosts in settings at {path}: every host name must be a string")
}

/// Like `settings_bad_data_directory`, for the browser imports folder.
pub fn settings_bad_library_directory(path: &str) -> String {
    format!("Invalid settings at {path}: library_directory must be a non-empty folder path")
}

/// Like `settings_data_directory_error`, for the browser imports folder.
pub fn settings_library_directory_error(path: &str, error: &str) -> String {
    format!("Invalid library_directory in settings at {path}: {error}")
}

/// NEW. The reason Save of the Browser view sheet could not write settings.json: not UTF-8.
pub fn settings_file_not_utf8(error: &str) -> String {
    format!("it is not UTF-8 text ({error})")
}

/// NEW. The same reason: not JSON.
pub fn settings_file_not_json(error: &str) -> String {
    format!("it is not valid JSON ({error})")
}

/// NEW. The same reason: JSON, but not an object.
pub const SETTINGS_FILE_NOT_AN_OBJECT: &str = "it does not hold a JSON object";

// --- Browser pages: the server (web brief 4.3-4.6) ----------------------------------------------

/// NEW. A typed port another program holds (the Browser view sheet refuses it).
pub fn port_in_use(port: u16) -> String {
    format!("Port {port} is used by another program. Choose another port.")
}

/// NEW. Every port of the search was taken.
pub fn no_free_port(first: u16, last: u16) -> String {
    format!("No free port was found from {first} to {last}.")
}

/// NEW. The address cannot be listened on (it is not an address of this computer).
pub fn address_not_usable(address: &str, error: &str) -> String {
    format!("The address {address} cannot be used: {error}")
}

/// NEW. The toast at start, and the reason in About, when the pages could not be started.
pub fn browser_not_started(reason: &str) -> String {
    format!("The browser pages could not be started: {reason}")
}

/// NEW. The toast after an import from the browser was added to what is open.
pub fn browser_import_added(posts: usize) -> String {
    format!("{} added from the browser.", count_label(posts as u64, "post"))
}

/// guard.py, with this app's setting (no ALLOWED_HOSTS variable).
pub const HOST_NOT_ALLOWED: &str = "This host name is not allowed. Open the app by IP address, localhost or this computer's name, or add the name to \"allowed_hosts\" in settings.json.";
/// guard.py.
pub const WRITE_REFUSED: &str = "Changes are only accepted from the app's own pages.";
/// The clone's packaged server (packaged.py).
pub const MALFORMED_REQUEST: &str = "Malformed request";
/// The clone's packaged server (packaged.py `has_dot_segment`).
pub const INVALID_PATH: &str = "Invalid path";
/// Starlette.
pub const NOT_FOUND: &str = "Not Found";
/// Starlette.
pub const METHOD_NOT_ALLOWED: &str = "Method Not Allowed";
/// Starlette's CORS refusal, `what` = "origin", "method" or "origin, method".
pub fn cors_disallowed(what: &str) -> String {
    format!("Disallowed CORS {what}")
}
/// NEW. 431.
pub const HEADERS_TOO_LARGE: &str = "The request headers are too large.";
/// NEW. 414.
pub const TARGET_TOO_LONG: &str = "The request address is too long.";
/// NEW. 501.
pub const CHUNKED_NOT_SUPPORTED: &str = "Request bodies sent in chunks are not supported.";
/// NEW. 400.
pub const CONFLICTING_LENGTHS: &str = "The request has two different Content-Length headers.";
/// NEW. 411.
pub const LENGTH_REQUIRED: &str = "A Content-Length header is required.";
/// NEW. A body that ended before its Content-Length.
pub const BODY_CUT_OFF: &str = "The request body was cut off.";
/// NEW. The server stops while a body is being received (an internal error text).
pub const SERVER_STOPPING: &str = "The browser pages are stopping.";
/// NEW. A file that got shorter while it was sent (an internal error text).
pub const FILE_GOT_SHORTER: &str = "The file got shorter while it was sent.";
/// NEW. 503 when every connection is taken.
pub const TOO_MANY_CONNECTIONS: &str = "Too many connections. Try again in a moment.";
/// NEW. A client that sends a body or reads an answer slower than the server's minimum rate
/// (an internal error text; the connection is closed).
pub const CLIENT_TOO_SLOW: &str = "The client sent or read too slowly.";
/// NEW. 416.
pub const RANGE_NOT_SATISFIABLE: &str = "The requested range lies beyond the end of the file.";
/// library.py `read_body`: 413 for a body over a route's limit.
pub fn request_too_large(limit: u64) -> String {
    format!("The request is larger than {}.", format_bytes(limit))
}
/// NEW. The UI thread did not answer a change in time.
pub const APP_TIMEOUT: &str = "Instagram Windows App did not answer in time. Try again.";
/// NEW. The app is closing and takes no more changes.
pub const APP_CLOSING: &str = "Instagram Windows App is closing.";
/// library.py `media_file`.
pub const NO_SUCH_LIBRARY_FILE: &str = "No such library file.";
/// NEW. A HEIC picture that could not be converted for a browser.
pub fn picture_not_for_browser(reason: &str) -> String {
    format!("This picture cannot be shown in a browser: {reason}")
}
/// NEW. The reason when a conversion took longer than its limit.
pub const CONVERSION_TIMED_OUT: &str = "the conversion took too long";
/// NEW. DELETE /api/library while something else than the browser imports is open.
pub const CLEAR_REFUSED: &str = "The posts come from your own pictures, folders or archives, which the browser never deletes. Close them in Instagram Windows App instead.";
/// NEW. DELETE /api/library/posts/{id} of a post that is not a browser import.
pub const DELETE_POST_REFUSED: &str = "This post comes from your own pictures, folders or archives, which the browser never deletes. Close it in Instagram Windows App instead.";
/// NEW. An import from the browser while loose pictures are open.
pub const IMPORT_WHILE_LOOSE: &str = "Instagram Windows App is showing loose pictures. Close them there, or open an export or archive, to save imports from the browser.";
/// NEW. The `directory` of the library JSON for several places: "{first} and {n} more".
pub fn places_and_more(first: &str, more: usize) -> String {
    format!("{first} and {more} more")
}
/// The clone's `for_client`, with this app's name.
pub fn remote_directory(label: &str) -> String {
    format!("{label} on the computer running Instagram Windows App")
}
/// NEW. The title of /docs.
pub const DOCS_TITLE: &str = "Instagram Windows App API";
/// NEW. The lead of /docs.
pub const DOCS_LEAD: &str = "The browser pages of Instagram Windows App use these routes. They show the posts that are open in the app, and every change goes to the app.";
/// NEW. The error note of /docs.
pub const DOCS_ERRORS: &str = "Errors are JSON objects of the form {\"detail\": \"<sentence>\"}.";

// --- Browser pages: edits and the browser imports folder (library.py) --------------------------

/// library.py `parse_post_changes`.
pub fn request_not_json(error: &str) -> String {
    format!("The request body is not valid JSON: {error}")
}
/// library.py `parse_post_changes`.
pub const REQUEST_NOT_AN_OBJECT: &str = "The request body must be a JSON object.";
/// library.py `parse_post_changes`.
pub fn field_not_editable(key: &str) -> String {
    format!("\"{key}\" cannot be changed; only the caption can.")
}
/// library.py `parse_post_changes`.
pub const EDIT_CHANGES_NOTHING: &str = "The request changes nothing: send a caption.";
/// library.py `parse_post_changes`.
pub const CAPTION_NOT_A_STRING: &str = "The caption must be a string.";
/// library.py `parse_post_changes`.
pub fn caption_too_long(max_units: usize) -> String {
    format!("A caption can be at most {} characters long.", format_count_exact(max_units as u64))
}
/// library.py.
pub const LIBRARY_NO_SUCH_POST: &str = "The library has no post with this id.";
/// library.py `_CommitParser.staged`.
pub fn commit_not_a_file(place: &str, value: &str) -> String {
    format!("{place}: \"{value}\" is not a file of this upload.")
}
/// library.py `_CommitParser.media`.
pub fn commit_poster_not_an_image(place: &str, poster: &str) -> String {
    format!("{place}: the poster \"{poster}\" is not an image.")
}
/// library.py `_CommitParser.media`.
pub fn commit_no_media(place: &str) -> String {
    format!("{place} has no media.")
}
/// library.py `_CommitParser.media`.
pub fn commit_too_many_media(place: &str, count: usize, max: usize) -> String {
    format!("{place} has {count} media; at most {max} are allowed.")
}
/// library.py `_CommitParser.post`: "Post {n}".
pub fn commit_post_place(number: usize) -> String {
    format!("Post {number}")
}
/// library.py `_CommitParser.post`.
pub fn commit_post_not_an_object(place: &str) -> String {
    format!("{place} is not a JSON object.")
}
/// library.py `_CommitParser.post`.
pub fn commit_post_no_id(place: &str) -> String {
    format!("{place} has no usable id.")
}
/// library.py `_CommitParser.avatars`.
pub const COMMIT_AVATARS_NOT_A_MAP: &str = "\"avatars\" must map usernames to uploaded file names.";
/// library.py `_CommitParser.avatars`.
pub fn commit_avatar_entry_invalid(key: &str) -> String {
    format!("The avatar entry \"{key}\" is not valid.")
}
/// library.py `_CommitParser.avatars`.
pub fn commit_avatar_place(username: &str) -> String {
    format!("The avatar of \"{username}\"")
}
/// library.py `_CommitParser.avatars`.
pub fn commit_avatar_not_an_image(username: &str) -> String {
    format!("The avatar of \"{username}\" is not an image.")
}
/// library.py `_CommitParser.parse`.
pub fn commit_not_json(error: &str) -> String {
    format!("The commit body is not valid JSON: {error}")
}
/// library.py `_CommitParser.parse`.
pub const COMMIT_NOT_AN_OBJECT: &str = "The commit body must be a JSON object.";
/// library.py `_CommitParser.parse`.
pub const COMMIT_BAD_SOURCE_KIND: &str = "source_kind must be \"archive\", \"folder\" or \"multiple\".";
/// library.py `_CommitParser.parse`.
pub const COMMIT_NO_POSTS: &str = "The commit lists no posts.";
/// library.py `_CommitParser.parse`.
pub fn commit_too_many_posts(count: usize, max: usize) -> String {
    format!("The commit lists {count} posts; at most {max} can be saved at once.")
}
/// library.py: the source name of an import without one.
pub const IMPORT_DEFAULT_SOURCE_NAME: &str = "Import";
/// library.py `_ready_root`, without the path.
pub fn library_folder_unusable(error: &str) -> String {
    format!("The browser imports folder cannot be used: {error}")
}
/// library.py `_ready_root`, without the path.
pub fn library_folder_link(name: &str) -> String {
    format!("\"{name}\" in the browser imports folder is a link; the library only uses real folders.")
}
/// library.py `_claim`, without the path and with this app's setting.
pub fn library_folder_foreign(entry: &str) -> String {
    format!("The browser imports folder already holds files this app did not create (\"{entry}\", for example an extracted export). To keep them safe the library only uses an empty folder or one it created itself: choose another folder with \"library_directory\" in settings.json, or move those files elsewhere.")
}
/// library.py `_claim`, without the path and with this app's setting.
pub const LIBRARY_FOLDER_IN_USE: &str = "The browser imports folder is in use by another running copy of this app. Close that copy, or give this one its own folder with \"library_directory\" in settings.json.";
/// library.py `_load`, without the path.
pub fn library_file_damaged(error: &str) -> String {
    format!("The post.json of the browser imports folder is damaged ({error}). Clear the library to start again.")
}
/// library.py `_parse_document`, without the path.
pub const LIBRARY_FILE_DAMAGED_SHAPE: &str = "The post.json of the browser imports folder is damaged. Clear the library to start again.";
/// library.py `create_upload`, without the path.
pub fn upload_not_started(error: &str) -> String {
    format!("Cannot start an upload in the browser imports folder: {error}")
}
/// library.py `receive_file`.
pub fn upload_bad_file_name(name: &str) -> String {
    format!("\"{name}\" is not a valid upload file name.")
}
/// library.py `receive_file`.
pub fn upload_file_too_large(limit: u64) -> String {
    format!("A file can be at most {}.", format_bytes(limit))
}
/// NEW. A staged file could not be written (the clone answers 500).
pub fn upload_not_written(error: &str) -> String {
    format!("The upload could not be written to the browser imports folder: {error}")
}
/// library.py `_reserve` / `_keep`.
pub const UPLOAD_CANCELLED: &str = "This upload was cancelled.";
/// NEW. The connection ended before the whole file arrived.
pub const UPLOAD_CUT_OFF: &str = "The upload was cut off before all its bytes arrived.";
/// library.py `abort_upload`.
pub const UPLOAD_BEING_SAVED: &str = "The upload is being saved and can no longer be cancelled.";
/// library.py `commit`, without the path.
pub fn import_not_saved(error: &str) -> String {
    format!("Could not save the import to the browser imports folder: {error}")
}
/// library.py `delete_post`, without the path.
pub fn library_file_not_updated(error: &str) -> String {
    format!("Could not update the post.json of the browser imports folder: {error}")
}
/// library.py `clear`, without the path.
pub fn library_file_not_removed(error: &str) -> String {
    format!("Could not remove the post.json of the browser imports folder: {error}")
}
/// library.py `_session`.
pub const UPLOAD_UNKNOWN: &str = "No such upload; start a new one.";
/// library.py `_start_write`.
pub const UPLOAD_SAVING_NO_MORE_FILES: &str = "This upload is being saved and accepts no more files.";
/// library.py `_start_write`.
pub fn upload_already_being_sent(name: &str) -> String {
    format!("{name} is already being uploaded.")
}
/// library.py `_start_write`.
pub fn upload_too_many_files(max: usize) -> String {
    format!("An upload can hold at most {max} files.")
}
/// library.py `_make_room_for_a_session`.
pub const TOO_MANY_IMPORTS: &str = "Too many imports are being saved at the same time. Try again when one has finished.";
/// library.py `_check_free_space`.
pub fn library_no_space(free: u64, min: u64) -> String {
    format!("Not enough free disk space: {} is free on the library's drive and at least {} must stay free.", format_bytes(free), format_bytes(min))
}
/// library.py `_accept`.
pub const UPLOAD_TYPE_NOT_STORED: &str = "Only JPEG, PNG, GIF, WebP, AVIF and BMP images and MP4, MOV and WebM videos can be saved.";
/// library.py `_reserve`.
pub fn upload_single_too_large(kind: &str, limit: u64) -> String {
    format!("A single {kind} can be at most {}.", format_bytes(limit))
}
/// library.py `_reserve`.
pub fn upload_session_too_large(limit: u64) -> String {
    format!("One import can save at most {}.", format_bytes(limit))
}
/// library.py `_reserve`.
pub fn library_full(limit: u64) -> String {
    format!("The library is full ({}).", format_bytes(limit))
}
/// library.py `_begin_commit`.
pub const UPLOAD_ALREADY_BEING_SAVED: &str = "This upload is already being saved.";
/// library.py `_begin_commit`.
pub const UPLOAD_STILL_SENDING: &str = "Some files of this upload are still being uploaded.";
/// NEW. 503 for a commit that waited `LibraryLimits::commit_wait` while
/// `LibraryLimits::max_commits_at_once` others were being merged (the frontend shows the
/// sentence and the user tries again).
pub const TOO_MANY_COMMITS: &str = "Other imports are being saved right now. Try again in a moment.";

// --- Browser pages: the Browser view sheet (lead's addition to web brief 4.12) -----------------

/// NEW. A port field that is not a whole number from 1 to 65535; `field` is the label.
pub fn port_field_invalid(field: &str) -> String {
    format!("{field} must be a whole number from 1 to 65535.")
}

/// NEW. The two port fields hold the same number.
pub const PORTS_MUST_DIFFER: &str = "The page address and the API address need two different ports.";

/// NEW. Save could not write settings.json; the new values apply to this session only.
pub fn settings_not_saved(reason: &str) -> String {
    format!("settings.json could not be saved: {reason}. The new values apply until the app is closed.")
}

/// NEW. Save worked.
pub const BROWSER_SETTINGS_SAVED: &str = "Saved in settings.json.";

/// NEW. Save worked, but a value of the command line stays in effect for this run.
pub const BROWSER_SETTINGS_SAVED_FOR_LATER: &str = "Saved in settings.json for later runs; the command line decides for this run.";

/// NEW. The note under a port that runs on another port than the configured one.
pub fn port_moved(configured: u16, running: u16) -> String {
    format!("Port {configured} was taken when the server started, so it runs on {running}.")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn names_in_sentences_are_the_last_part_of_a_path() {
        assert_eq!(display_name(Path::new("C:/pictures/Holiday 2026/IMG_2.jpg")), "IMG_2.jpg");
        assert_eq!(display_name(Path::new("C:/pictures/Holiday 2026")), "Holiday 2026");
        assert_eq!(display_name(Path::new("IMG_2.jpg")), "IMG_2.jpg");
        // A path without a last part is shown whole.
        assert_eq!(display_name(Path::new("..")), "..");
        #[cfg(windows)]
        assert_eq!(display_name(Path::new(r"C:\")), r"C:\");
    }

    #[test]
    fn count_labels_follow_the_clone() {
        assert_eq!(count_label(1, "post"), "1 post");
        assert_eq!(count_label(0, "post"), "0 posts");
        assert_eq!(count_label(1200, "post"), "1,200 posts");
        assert_eq!(count_label(1, "more problem"), "1 more problem");
    }

    #[test]
    fn error_texts_lose_their_prefix() {
        assert_eq!(describe_error("Error: gone"), "gone");
        assert_eq!(describe_error("uncaught  error:  gone "), "gone");
        assert_eq!(describe_error("Uncaught exception"), "Uncaught exception");
        assert_eq!(describe_error("  "), "unknown error");
        assert_eq!(describe_error("Érror"), "Érror");
    }

    #[test]
    fn long_quoted_values_are_shortened() {
        let long = "a".repeat(81);
        assert_eq!(shorten(&long), format!("{}...", "a".repeat(77)));
        let exact = "b".repeat(80);
        assert_eq!(shorten(&exact), exact);
    }

    #[test]
    fn export_problems_match_the_clone_test_vectors() {
        // export.test.ts:219-225
        assert_eq!(
            problem_file_unreadable(&export_label(1, "C9aKq2LtXy1"), "posts/post1_1.jpg", "gone"),
            "Post 1 (C9aKq2LtXy1): \"posts/post1_1.jpg\" could not be read: gone."
        );
        assert_eq!(
            problem_post_left_out(&export_label(2, "C8ZmP4RvNd7")),
            "Post 2 (C8ZmP4RvNd7) was left out: none of its pictures or videos could be saved."
        );
        assert_eq!(
            problem_only_poster_saved(&export_label(3, "C7TtW0YhBs3"), "posts/reel.webm"),
            "Post 3 (C7TtW0YhBs3): only the poster of \"posts/reel.webm\" was saved."
        );
        // An operating system error already ends with a period: the sentence has exactly one.
        assert_eq!(
            problem_avatar_unreadable("Post 1 (a)", "Access is denied."),
            "Post 1 (a): The profile picture could not be read: Access is denied."
        );
        assert_eq!(problem_avatar_is_a_video("Post 1 (a)"), "Post 1 (a): the profile picture is not a picture.");
    }

    #[test]
    fn download_messages_match_the_clone() {
        let none: Vec<String> = Vec::new();
        assert_eq!(
            download_finished(1, &strings(&["instagram-export-FXTbasic005.zip"]), &none),
            "Downloaded 1 post as instagram-export-FXTbasic005.zip."
        );
        assert_eq!(
            download_finished(6, &strings(&["instagram-export.zip"]), &none),
            "Downloaded 6 posts as instagram-export.zip."
        );
        assert_eq!(
            download_finished(
                12_000,
                &strings(&["instagram-export-part1.zip", "instagram-export-part2.zip", "instagram-export-part3.zip"]),
                &none
            ),
            "Downloaded 12,000 posts in 3 archives (instagram-export-part1.zip …)."
        );
        assert_eq!(
            download_finished(2, &strings(&["instagram-export.zip"]), &strings(&["First.", "Second.", "Third."])),
            "Downloaded 2 posts as instagram-export.zip. Left out: First. (and 2 more problems)"
        );
        assert_eq!(
            download_finished(2, &strings(&["instagram-export.zip"]), &strings(&["First.", "Second."])),
            "Downloaded 2 posts as instagram-export.zip. Left out: First. (and 1 more problem)"
        );
        assert_eq!(download_finished(0, &none, &none), "Nothing could be downloaded.");
        assert_eq!(download_finished(0, &none, &strings(&["First."])), "Nothing could be downloaded. First.");
        assert_eq!(download_toast_ms(0), 4000);
        assert_eq!(download_toast_ms(2), 9000);
    }

    #[test]
    fn reader_sentences_match_the_clone() {
        assert_eq!(manifest_too_large(), "post.json is larger than 32 MB.");
        assert_eq!(source_too_many_files("trip"), "\"trip\" contains more than 100000 files.");
        assert_eq!(file_too_large("big.jpg", 64 * 1024 * 1024), "\"big.jpg\" is larger than 64 MB and was skipped.");
        assert_eq!(file_too_large("big.mp4", 1024 * 1024 * 1024), "\"big.mp4\" is larger than 1 GB and was skipped.");
        assert_eq!(
            too_many_media(3, 31),
            "Post 3 lists 31 pictures and videos; only the first 30 are imported."
        );
        assert_eq!(
            link_rejected(LinkSubject::Post(3), false),
            "The link of post 3 is not a valid http(s) link and was ignored."
        );
        assert_eq!(
            link_rejected(LinkSubject::PostProfile(4), true),
            "The profile link of post 4 is an instagram.com link, but not to an Instagram profile, and was ignored."
        );
        assert_eq!(
            link_rejected(LinkSubject::Source, true),
            "The source link of the export is an instagram.com link, but not to an Instagram profile, post or feed, and was ignored."
        );
        assert_eq!(
            media_is_a_url(7, "https://cdn.example/e.jpg", false),
            "Post 7: \"https://cdn.example/e.jpg\" is a URL. Images are not downloaded; the file must be inside the export."
        );
        assert_eq!(
            no_post_imported(1, Some("Post 1 has no image path and was skipped.")),
            "The only post in post.json could not be imported. First problem: Post 1 has no image path and was skipped."
        );
        assert_eq!(no_post_imported(7, None), "None of the 7 posts in post.json could be imported.");
    }

    #[test]
    fn editor_texts_match_the_clone() {
        // library.spec.ts:636-637
        assert_eq!(caption_counter("  Low tide again 🌊\nThe second line.  "), "38 / 10,000");
        assert_eq!(
            description_not_saved("The library folder cannot be used."),
            "The description could not be saved: The library folder cannot be used."
        );
        assert_eq!(description_not_saved(" .. "), "The description could not be saved.");
        assert_eq!(edit_note(&PostOrigin::File), "Kept on this computer with the picture.");
        let manifest = std::path::PathBuf::from("post.json");
        assert_eq!(edit_note(&PostOrigin::Folder { manifest }), "Kept on this computer. The export folder is not changed.");
        let archive = std::path::PathBuf::from("export.zip");
        assert_eq!(edit_note(&PostOrigin::Archive { archive }), "Kept on this computer. The archive is not changed.");
    }

    fn loose(name: &str, kind: MediaKind) -> Post {
        use crate::model::{MediaItem, MediaType, PostKey, User};
        let path = std::path::PathBuf::from(name);
        Post {
            key: PostKey::for_file(&path),
            id: name.to_owned(),
            permalink: None,
            media: vec![MediaItem { kind, path, manifest_path: name.to_owned(), poster: None, alt: None, bytes: 1 }],
            user: User::named("tester"),
            caption: String::new(),
            original_caption: String::new(),
            comment_count: 0,
            like_count: None,
            taken_at: None,
            media_type: if kind == MediaKind::Video { MediaType::Video } else { MediaType::Image },
            alt: None,
            location: None,
            context: None,
            comments: Vec::new(),
            origin: PostOrigin::File,
        }
    }

    #[test]
    fn video_texts_and_the_note_of_a_loose_video() {
        assert_eq!(edit_note_for(&loose("a.mp4", MediaKind::Video)), "Kept on this computer with the video.");
        assert_eq!(edit_note_for(&loose("a.jpg", MediaKind::Image)), "Kept on this computer with the picture.");
        let mut export_video = loose("a.mp4", MediaKind::Video);
        export_video.origin = PostOrigin::Folder { manifest: std::path::PathBuf::from("post.json") };
        assert_eq!(edit_note_for(&export_video), "Kept on this computer. The export folder is not changed.");
        assert_eq!(video_cannot_play("a.mp4", VIDEO_NO_PICTURE), "\"a.mp4\" can't be played: it has no picture.");
        assert_eq!(video_damaged("HRESULT text."), "the file is damaged or incomplete (HRESULT text).");
        assert_eq!(
            video_needs_codec("IMG_7.MOV", "HEVC", CODEC_REMEDY_HEVC),
            "\"IMG_7.MOV\" can't be played: Windows has no decoder for its HEVC video. HEVC videos need the HEVC Video Extensions from the Microsoft Store."
        );
    }

    #[test]
    fn the_banner_of_loose_files_counts_pictures_and_videos() {
        assert_eq!(showing_loose_banner(3, 0, "Holiday"), "Showing 3 pictures from Holiday");
        assert_eq!(showing_loose_banner(0, 1, "Holiday"), "Showing 1 video from Holiday");
        assert_eq!(showing_loose_banner(0, 2, "Holiday"), "Showing 2 videos from Holiday");
        assert_eq!(showing_loose_banner(2, 1, "Holiday"), "Showing 3 pictures and videos from Holiday");
        assert_eq!(showing_loose_banner(1000, 200, "Holiday"), "Showing 1,200 pictures and videos from Holiday");
        let collection = Collection {
            posts: vec![loose("a.jpg", MediaKind::Image), loose("b.mp4", MediaKind::Video)],
            source: crate::model::CollectionSource::Loose,
            label: "mixed".to_owned(),
            folder: None,
            warnings: Vec::new(),
            source_url: None,
            total_entries: 2,
        };
        assert_eq!(collection_banner(&collection), "Showing 2 pictures and videos from mixed");
    }

    #[test]
    fn archive_sentences_match_the_clone() {
        // archive.test.ts, zip.ts, tar.ts and multi.test.ts.
        assert_eq!(archive_kind_not_supported("RAR"), "RAR archives are not supported. Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.");
        assert_eq!(archive_type_not_supported("photos.7z"), "\"photos.7z\" is an archive type that is not supported. Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.");
        assert_eq!(not_a_supported_archive("notes.zip"), "\"notes.zip\" is not a supported archive. Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.");
        assert_eq!(zip_damaged("no end-of-archive record"), "The zip file is damaged or incomplete (no end-of-archive record).");
        assert_eq!(zip_more_files(100_000), "The zip file contains more than 100000 files.");
        assert_eq!(zip_method("posts/a.jpg", 12), "\"posts/a.jpg\" uses a zip compression method that is not supported (12). Re-create the zip with standard (deflate) compression.");
        assert_eq!(member_too_large("posts/a.jpg", 64 * 1024 * 1024), "\"posts/a.jpg\" is larger than 64 MB.");
        assert_eq!(archive_unpacks_too_large(4096 * 1024 * 1024 + 96 * 1024 * 1024), "The archive unpacks to more than 4.1 GB, which is too large to import.");
        assert_eq!(media_budget_exceeded(4096 * 1024 * 1024), "The pictures and videos of this import add up to more than 4 GB. Import the export in smaller parts.");
        assert_eq!(archive_cache_no_space(512 * 1024 * 1024, 1024 * 1024 * 1024), "Not enough free disk space: 512 MB is free on the drive of the archive cache and at least 1 GB must stay free.");
        assert_eq!(sources_truncated(51), "51 archives or folders were selected; only the first 50 were imported.");
        assert_eq!(source_failed("bad.zip", "post.json is not valid JSON."), "\"bad.zip\" could not be imported: post.json is not valid JSON.");
        let reasons = failure_reasons(&[("a.zip".to_owned(), "First.".to_owned()), ("b".to_owned(), "Second.".to_owned())]);
        assert_eq!(no_source_imported(2, &reasons), "None of the 2 selected archives or folders could be imported. a.zip: First. b: Second.");
        assert_eq!(merged_duplicate("P1", "two.zip", "one.zip"), "The post \"P1\" from two.zip replaces the one with the same id from one.zip.");
    }

    #[test]
    fn sources_are_counted_like_the_clone() {
        // merge.test.ts describeSources.
        assert_eq!(describe_source_counts(3, 0, 0), "3 archives");
        assert_eq!(describe_source_counts(2, 1, 0), "2 archives and 1 folder");
        assert_eq!(describe_source_counts(0, 1, 2), "1 folder and 2 imports");
        assert_eq!(describe_source_counts(1, 1, 1), "1 archive, 1 folder and 1 import");
        assert_eq!(describe_source_counts(1, 0, 0), "1 archive");
        assert_eq!(describe_source_counts(0, 0, 0), "Nothing");
    }

    #[test]
    fn banner_and_auto_view_texts() {
        assert_eq!(showing_banner(1, 1, false, "Holiday"), "Showing 1 picture from Holiday");
        assert_eq!(showing_banner(1200, 1200, false, "Holiday"), "Showing 1,200 pictures from Holiday");
        assert_eq!(showing_banner(6, 6, true, "export-basic"), "Showing 6 posts from export-basic");
        assert_eq!(showing_banner(5, 7, true, "export-problems"), "Showing 5 of 7 posts from export-problems");
        assert_eq!(warnings_label(1), "1 warning");
        assert_eq!(show_all_warnings(12), "Show all 12 warnings");
        assert_eq!(auto_view_lead(0), "There are no posts to show yet.");
        assert_eq!(auto_view_lead(1), "Shows the post one after another and moves on by itself.");
        assert_eq!(auto_view_lead(1500), "Shows the 1,500 posts one after another and moves on by itself.");
        assert_eq!(auto_view_end_text(1), "You have seen all 1 post.");
        assert_eq!(auto_view_end_text(12), "You have seen all 12 posts.");
        assert_eq!(auto_view_announcement(0, 0, false, false, None), "There are no posts to show.");
        assert_eq!(auto_view_announcement(3, 3, true, true, None), "End of posts.");
        assert_eq!(auto_view_announcement(2, 3, false, true, Some("a")), "Auto view paused.");
        assert_eq!(auto_view_announcement(2, 3, false, false, Some("a")), "Post 2 of 3, by a.");
        assert_eq!(auto_view_announcement(2, 3, false, false, None), "Post 2 of 3.");
        assert_eq!(window_title(Some("IMG_2.jpg")), "IMG_2.jpg - Instagram Windows App");
        assert_eq!(window_title(None), "Instagram Windows App");
    }
}
