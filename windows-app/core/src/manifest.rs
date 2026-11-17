//! Reads an existing "instagram-export": finds post.json, parses it leniently, resolves the
//! paths it names against the files that are really in the folder, and assembles the posts with
//! one warning for everything that had to be skipped.
//!
//! This is the reader of the reference web project (frontend/src/import/manifest.ts, paths.ts,
//! source.ts, core.ts, limits.ts; spec-export C), for a folder on disk:
//!
//! ```text
//! find_manifest      the shallowest post.json / posts.json of a folder, noise ignored    (C.1)
//! FileIndex::scan    every file below the export root, by normalised relative path      (C.1)
//! read_manifest      bytes of post.json -> raw posts with relative paths + warnings     (C.2-C.4, C.7)
//! FileIndex::resolve a path written in post.json -> the real file                       (C.6)
//! assemble_export    raw posts + files -> model::Post, sniffed and size-checked         (C.5)
//! load_export        all of the above for one manifest path
//! ```
//!
//! The reader is lenient on purpose: exports are written by other tools, so keys are matched
//! through alias tables, counts and dates come in many shapes, and paths differ in letter case or
//! carry foreign leading folders. Nothing in a post.json can make the reader panic or reach a
//! file outside the export root.
//!
//! Differences from the clone, all because files stay on disk instead of being loaded:
//! `MAX_TOTAL_MEDIA_BYTES` is not enforced for a folder (nothing is copied or held in memory; it
//! is enforced for the members an archive copies into the archive cache, see `archive`); HEIC is
//! not converted; path folding is lower-casing without Unicode NFC (no normalisation tables);
//! symbolic links and junctions inside an export folder are not followed, so that nothing outside
//! the export root is ever read. Archives are read by `archive`, which hands this module the
//! manifest and the member list; several sources are merged by `sources`.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use crate::messages::{self, display_name, LinkSubject};
use crate::model::{Comment, MediaItem, MediaKind, MediaType, Post, PostKey, PostOrigin, Poster, User, Warning, WarningCode};
use crate::sniff;
use crate::text;
use crate::urls::{self, LinkRejection};

// --- Limits (frontend/src/import/limits.ts; spec-export C.8) ----------------------------------

const MIB: u64 = 1024 * 1024;

/// Entries of post.json beyond this are ignored with a "truncated" warning.
pub const MAX_POSTS: usize = 5000;
/// Hard limit for the size of post.json.
pub const MAX_MANIFEST_BYTES: u64 = 32 * MIB;
/// A single picture larger than this is skipped with a warning (the validator rejects it too).
pub const MAX_IMAGE_BYTES: u64 = 64 * MIB;
/// A single video file larger than this is skipped with a warning.
pub const MAX_VIDEO_BYTES: u64 = 1024 * MIB;
/// What the clone takes in one import: the pictures and videos the archives of one opening copy
/// into the archive cache may add up to this much (`archive`). Not enforced for folders, whose
/// files are not copied (see the module comment).
pub const MAX_TOTAL_MEDIA_BYTES: u64 = 4096 * MIB;
/// Archives and folders of one opening; more are left out with a warning (`catalog`).
pub const MAX_SOURCES: usize = 50;
/// Hard limit for the number of files below an export root, and for the entries of one folder
/// of loose pictures.
pub const MAX_SOURCE_ENTRIES: usize = 100_000;
/// Carousel slides (pictures and videos) beyond this are dropped with a "truncated" warning.
pub const MAX_IMAGES_PER_POST: usize = 30;
/// Sample comments beyond this are silently dropped.
pub const MAX_COMMENTS_PER_POST: usize = 200;

/// Text limits, in UTF-16 code units. Longer values are cut (`text::clean_text`), except a link,
/// which is rejected.
pub const MAX_CAPTION_CHARS: usize = 10_000;
/// Longest text of a sample comment, in UTF-16 code units.
pub const MAX_COMMENT_CHARS: usize = 2000;
/// Longest username, in UTF-16 code units.
pub const MAX_USERNAME_CHARS: usize = 100;
/// Longest full name of an author, in UTF-16 code units.
pub const MAX_NAME_CHARS: usize = 200;
/// Longest alt text of a post or a slide, in UTF-16 code units.
pub const MAX_ALT_CHARS: usize = 2000;
/// Longest location, in UTF-16 code units.
pub const MAX_LOCATION_CHARS: usize = 200;
/// Longest post id, in characters (`text::clean_id` cuts to it).
pub const MAX_ID_CHARS: usize = 100;
/// Longest link, in UTF-16 code units; a longer one is rejected, not cut.
pub const MAX_URL_CHARS: usize = 2048;

/// Pictures are read and decoded with at most this many parallel workers.
pub const IMAGE_CONCURRENCY: usize = 3;

/// A count written as text that is longer than this is not a count (manifest.ts).
pub const MAX_COUNT_CHARS: usize = 64;
/// How many trailing segments of a path with foreign leading folders are tried (source.ts).
pub const MAX_SUFFIX_SEGMENTS: usize = 8;
/// Raw media items collected for one entry before collection stops (`MAX_IMAGES_PER_POST * 4`).
pub const MAX_RAW_MEDIA_ITEMS: usize = 120;
/// How deep `FileIndex::scan` walks below the export root (the clone's folder walk).
pub const MAX_FOLDER_DEPTH: usize = 32;

/// The file names of a manifest, compared without regard to letter case, in rank order:
/// post.json wins over posts.json at the same depth.
pub const MANIFEST_NAMES: [&str; 2] = ["post.json", "posts.json"];

/// File names (lower case) that are never part of an export or a picture folder (paths.ts
/// `isNoisePath`). A name starting with "._" and any path with a `__MACOSX` segment are noise too.
pub const NOISE_FILE_NAMES: [&str; 3] = [".ds_store", "thumbs.db", "desktop.ini"];

/// The folder macOS adds to an archive for resource forks; nothing below it is ever read.
const NOISE_FOLDER_NAME: &str = "__MACOSX";

/// The largest whole number a JavaScript number holds exactly (`Number.MAX_SAFE_INTEGER`); the
/// clone caps every count at it.
const MAX_SAFE_COUNT: u64 = 9_007_199_254_740_991;

/// The `path` of a warning keeps at most this many UTF-16 code units of a value from post.json
/// (manifest.ts `warn`).
const MAX_WARNING_PATH_CHARS: usize = 500;

// --- Errors -----------------------------------------------------------------------------------

/// Why an export could not be read at all; the clone's `ImportErrorCode`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ManifestErrorCode {
    /// A folder or post.json cannot be read from disk.
    ReadFailed,
    /// There is no post.json / posts.json.
    NoManifest,
    /// post.json is not JSON.
    InvalidJson,
    /// post.json is JSON of the wrong shape.
    InvalidManifest,
    /// post.json lists no post, or none of them could be shown.
    NoPosts,
    /// post.json is larger than `MAX_MANIFEST_BYTES`, or the folder holds more than
    /// `MAX_SOURCE_ENTRIES` files.
    TooLarge,
}

/// Raised when nothing usable could be read from an export. `message` is a complete sentence
/// from `messages` that can be shown to the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestError {
    /// The kind of failure.
    pub code: ManifestErrorCode,
    /// The sentence to show.
    pub message: String,
}

impl ManifestError {
    /// An error of `code` with its sentence.
    pub fn new(code: ManifestErrorCode, message: impl Into<String>) -> ManifestError {
        ManifestError { code, message: message.into() }
    }
}

impl std::fmt::Display for ManifestError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ManifestError {}

// --- Paths (paths.ts; spec-export C.1, C.6) ---------------------------------------------------

/// Why a path written in post.json cannot be used.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PathIssue {
    /// Blank, or nothing left after normalising.
    Empty,
    /// A URL (`scheme://`, `data:`, `blob:`, `javascript:`, `file:`, `mailto:`, or `//host`).
    Remote,
    /// It climbs out of the export with "..".
    Escape,
}

/// The schemes that make a path a URL even without "//" after the colon (paths.ts `REMOTE_RE`).
const REMOTE_SCHEMES: [&str; 5] = ["data:", "blob:", "javascript:", "file:", "mailto:"];

/// `text` without `prefix`, compared without regard to ASCII letter case.
fn strip_prefix_ignore_case<'a>(text: &'a str, prefix: &str) -> Option<&'a str> {
    let head = text.get(..prefix.len())?;
    head.eq_ignore_ascii_case(prefix).then(|| &text[prefix.len()..])
}

/// True for a character that may follow the first letter of a URL scheme.
fn is_scheme_char(character: char) -> bool {
    character.is_ascii_alphanumeric() || matches!(character, '+' | '.' | '-')
}

/// paths.ts `REMOTE_RE` plus the "//host" form:
/// `^(?:[a-z][a-z0-9+.-]+://|(?:data|blob|javascript|file|mailto):)` in any letter case.
fn is_remote_path(trimmed: &str) -> bool {
    if trimmed.starts_with("//") {
        return true;
    }
    if REMOTE_SCHEMES.iter().any(|scheme| strip_prefix_ignore_case(trimmed, scheme).is_some()) {
        return true;
    }
    // A scheme of at least two characters followed by "://": "C:/Users/..." is a drive, not a URL.
    let mut characters = trimmed.chars();
    if !characters.next().is_some_and(|first| first.is_ascii_alphabetic()) {
        return false;
    }
    let rest = characters.as_str();
    let scheme_tail = rest.find(|character| !is_scheme_char(character)).unwrap_or(rest.len());
    scheme_tail >= 1 && rest[scheme_tail..].starts_with("://")
}

/// The clone's `normalizeRelativePath` for a path written in post.json (paths.ts:7-31):
///
/// 1. `js_trim`; empty -> `Empty`.
/// 2. matches `^(?:[a-z][a-z0-9+.-]+://|(?:data|blob|javascript|file|mailto):)` (any letter
///    case) or starts with "//" -> `Remote`. The scheme needs at least two characters before
///    "://", so `C:/Users/...` is not remote.
/// 3. "\" -> "/"; split on "/"; drop empty and "." segments; ".." removes the previous segment
///    and, with nothing to remove, is `Escape`.
/// 4. no segment left -> `Empty`; else the segments joined with "/".
///
/// Vectors (manifest.test.ts:379-406): `posts\sub\a.jpg` -> `posts/sub/a.jpg`; `./posts/b.jpg`
/// -> `posts/b.jpg`; `/posts/c.jpg` -> `posts/c.jpg`; `posts/x/../d.jpg` -> `posts/d.jpg`;
/// `../outside.jpg`, `posts/../../outside.jpg`, `..\..\windows\system.ini` -> `Escape`;
/// `https://cdn.example/e.jpg`, `//cdn.example/e.jpg`, `data:image/png;base64,AAAA` -> `Remote`.
pub fn normalize_relative_path(raw: &str) -> Result<String, PathIssue> {
    let trimmed = text::js_trim(raw);
    if trimmed.is_empty() {
        return Err(PathIssue::Empty);
    }
    if is_remote_path(trimmed) {
        return Err(PathIssue::Remote);
    }
    let mut segments: Vec<&str> = Vec::new();
    for segment in trimmed.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => {
                if segments.pop().is_none() {
                    return Err(PathIssue::Escape);
                }
            }
            _ => segments.push(segment),
        }
    }
    if segments.is_empty() {
        return Err(PathIssue::Empty);
    }
    Ok(segments.join("/"))
}

/// The clone's `normalizeEntryPath` for the path of a file found on disk, relative to the root
/// that is being listed: "\" -> "/", empty and "." segments dropped; `None` when a segment is
/// ".." (such a file is ignored).
pub fn normalize_entry_path(raw: &str) -> Option<String> {
    let mut segments: Vec<&str> = Vec::new();
    for segment in raw.split(['/', '\\']) {
        match segment {
            "" | "." => {}
            ".." => return None,
            _ => segments.push(segment),
        }
    }
    if segments.is_empty() {
        None
    } else {
        Some(segments.join("/"))
    }
}

/// The last segment of a normalised relative path.
fn base_name(relative: &str) -> &str {
    relative.rsplit('/').next().unwrap_or(relative)
}

/// The clone's `isNoisePath` for a normalised relative path: any segment equal to `__MACOSX`, a
/// file name starting with "._", or a file name that is (in lower case) one of
/// `NOISE_FILE_NAMES`.
pub fn is_noise_path(relative: &str) -> bool {
    if relative.split('/').any(|segment| segment == NOISE_FOLDER_NAME) {
        return true;
    }
    let name = base_name(relative);
    if name.starts_with("._") {
        return true;
    }
    let lower = name.to_lowercase();
    NOISE_FILE_NAMES.contains(&lower.as_str())
}

/// The rank of a manifest file name: 0 for post.json, 1 for posts.json, `None` for any other.
fn manifest_rank(file_name: &str) -> Option<usize> {
    let lower = file_name.to_lowercase();
    MANIFEST_NAMES.iter().position(|name| *name == lower)
}

/// True when `file_name` is "post.json" or "posts.json" in any letter case (`Post.JSON` counts).
pub fn is_manifest_name(file_name: &str) -> bool {
    manifest_rank(file_name).is_some()
}

/// False when a subfolder at the last level of `find_manifest` certainly holds no manifest, so
/// that it need not be listed: opening a picture folder must not read the whole listing of
/// every album folder inside it (twice, because the app first asks what the paths are).
///
/// On Windows the two manifest names are asked for directly; file names are compared without
/// regard to letter case there, so "Post.JSON" answers to "post.json". (A folder that was
/// switched to case-sensitive names is the exception: a manifest in other letter case is not
/// found one level down in it.) The folder is still listed after a hit, which gives the name
/// as it is on disk and keeps links out. Elsewhere names are case-sensitive and the folder is
/// always listed.
fn may_hold_manifest(directory: &Path) -> bool {
    if !cfg!(windows) {
        return true;
    }
    MANIFEST_NAMES
        .iter()
        .any(|name| fs::symlink_metadata(directory.join(name)).is_ok_and(|metadata| metadata.is_file()))
}

/// The manifest of a folder: among the files named like a manifest (`is_manifest_name`) at most
/// `max_depth` folder levels below `folder` (0 = directly in it), noise ignored
/// (`is_noise_path`), the one with the smallest tuple (number of path segments, rank in
/// `MANIFEST_NAMES`, relative path text): the shallowest wins, post.json before posts.json
/// (source.ts:85-102). `Ok(None)` when there is none.
///
/// The walk stops descending at `max_depth` and never follows a folder whose name is noise.
/// Asking a large picture folder costs the listing of that folder and, on Windows, two file
/// look-ups per subfolder of the last level (`may_hold_manifest`), not their listings. `Err` (code
/// `ReadFailed`, `messages::source_not_imported`) only when `folder` itself cannot be listed;
/// unreadable subfolders are skipped.
pub fn find_manifest(folder: &Path, max_depth: usize) -> Result<Option<PathBuf>, ManifestError> {
    // Level by level: a manifest found at one level beats everything deeper, so the folders of
    // the next level are only listed when this level has none.
    let mut level: Vec<(PathBuf, String)> = vec![(folder.to_path_buf(), String::new())];
    for depth in 0..=max_depth {
        let mut best: Option<((usize, String), PathBuf)> = None;
        let mut next_level: Vec<(PathBuf, String)> = Vec::new();
        for (directory, prefix) in level {
            if depth > 0 && depth == max_depth && !may_hold_manifest(&directory) {
                continue;
            }
            let listing = match fs::read_dir(&directory) {
                Ok(listing) => listing,
                Err(error) if depth == 0 => {
                    return Err(ManifestError::new(
                        ManifestErrorCode::ReadFailed,
                        messages::source_not_imported(&display_name(folder), &error.to_string()),
                    ));
                }
                Err(_) => continue,
            };
            for entry in listing.flatten() {
                let Ok(file_type) = entry.file_type() else { continue };
                let name = entry.file_name().to_string_lossy().into_owned();
                if file_type.is_dir() {
                    if depth < max_depth && name != NOISE_FOLDER_NAME {
                        next_level.push((entry.path(), format!("{prefix}{name}/")));
                    }
                } else if file_type.is_file() {
                    let Some(rank) = manifest_rank(&name) else { continue };
                    let relative = format!("{prefix}{name}");
                    if is_noise_path(&relative) {
                        continue;
                    }
                    let key = (rank, relative);
                    if best.as_ref().is_none_or(|(best_key, _)| key < *best_key) {
                        best = Some((key, entry.path()));
                    }
                }
            }
        }
        if let Some((_, path)) = best {
            return Ok(Some(path));
        }
        level = next_level;
    }
    Ok(None)
}

/// One file below an export root.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceFile {
    /// Path relative to the root, normalised (`normalize_entry_path`), in the letter case of
    /// the disk: `posts/post1.jpg`. Warnings quote it as `{file}`.
    pub relative: String,
    /// The file on disk. For a member of an archive: its copy in the archive cache.
    pub path: PathBuf,
    /// Its size in bytes (for a member of an archive: the size the archive declares).
    pub bytes: u64,
    /// Why the file cannot be read, when that is known before it is looked at: a member of an
    /// archive that could not be extracted (password, compression method, damage). `None` for
    /// every file of a folder.
    pub unreadable: Option<String>,
}

/// The files that share one folded file name, for the last step of `FileIndex::resolve`. Only
/// the count and the first file are kept, so a lookup costs the same however many files of one
/// name an export holds.
#[derive(Clone, Copy, Debug)]
struct SameName {
    /// How many files (manifests excluded) have this name, at any depth.
    anywhere: usize,
    /// The first of them, as an index into `FileIndex::files`.
    first_anywhere: usize,
    /// How many of them lie directly in the root.
    in_root: usize,
    /// The first of those.
    first_in_root: usize,
}

/// The files of an export root, for resolving the paths post.json names (source.ts
/// `EntryIndex`). The root is the folder of the manifest; nothing outside it is ever returned.
#[derive(Clone, Debug)]
pub struct FileIndex {
    root: PathBuf,
    files: Vec<SourceFile>,
    /// Relative path -> index into `files`. A path listed twice: the later file wins.
    exact: HashMap<String, usize>,
    /// Folded relative path -> index into `files`. Several files folding alike: the first wins.
    folded: HashMap<String, usize>,
    /// Folded file name -> the files of that name that are not manifests.
    by_name: HashMap<String, SameName>,
}

/// The comparison key of a path: lower case. The clone also applies Unicode NFC, which would
/// need normalisation tables; a path written in decomposed form (macOS) for a file named in
/// composed form is therefore "not in the export" here.
fn fold(path: &str) -> String {
    path.to_lowercase()
}

impl FileIndex {
    /// An index over a given list of files (tests build one without touching the disk). The
    /// list order is the listing order: where two files fold to the same key, the first wins.
    pub fn from_files(root: PathBuf, files: Vec<SourceFile>) -> FileIndex {
        let mut exact: HashMap<String, usize> = HashMap::with_capacity(files.len());
        for (position, file) in files.iter().enumerate() {
            exact.insert(file.relative.clone(), position);
        }
        let mut folded: HashMap<String, usize> = HashMap::with_capacity(files.len());
        let mut by_name: HashMap<String, SameName> = HashMap::new();
        for (position, file) in files.iter().enumerate() {
            if exact[&file.relative] != position {
                // Listed twice under one name: only the later file counts.
                continue;
            }
            let key = fold(&file.relative);
            let name = base_name(&key);
            if !is_manifest_name(name) {
                let in_root = !key.contains('/');
                let same = by_name.entry(name.to_owned()).or_insert(SameName {
                    anywhere: 0,
                    first_anywhere: position,
                    in_root: 0,
                    first_in_root: position,
                });
                same.anywhere += 1;
                if in_root {
                    if same.in_root == 0 {
                        same.first_in_root = position;
                    }
                    same.in_root += 1;
                }
            }
            folded.entry(key).or_insert(position);
        }
        FileIndex { root, files, exact, folded, by_name }
    }

    /// Lists every regular file below `root`, recursively (at most `MAX_FOLDER_DEPTH` levels),
    /// in a stable order (each folder's entries sorted by name). Noise paths are left out;
    /// unreadable entries are skipped. More than `MAX_SOURCE_ENTRIES` files -> `Err` with code
    /// `TooLarge` and `messages::source_too_many_files(<name of root>)`; a root that cannot be
    /// listed -> code `ReadFailed`, `messages::source_not_imported`.
    pub fn scan(root: &Path) -> Result<FileIndex, ManifestError> {
        let mut files: Vec<SourceFile> = Vec::new();
        // Folders still to list, the next one last: (folder, relative prefix, depth below root).
        let mut pending: Vec<(PathBuf, String, usize)> = vec![(root.to_path_buf(), String::new(), 0)];
        while let Some((directory, prefix, depth)) = pending.pop() {
            let listing = match fs::read_dir(&directory) {
                Ok(listing) => listing,
                Err(error) if depth == 0 => {
                    return Err(ManifestError::new(
                        ManifestErrorCode::ReadFailed,
                        messages::source_not_imported(&display_name(root), &error.to_string()),
                    ));
                }
                Err(_) => continue,
            };
            let mut found_files: Vec<SourceFile> = Vec::new();
            let mut found_folders: Vec<(PathBuf, String, usize)> = Vec::new();
            for entry in listing.flatten() {
                // Links and junctions are neither: they could lead out of the export root.
                let Ok(file_type) = entry.file_type() else { continue };
                let name = entry.file_name().to_string_lossy().into_owned();
                if file_type.is_dir() {
                    if depth < MAX_FOLDER_DEPTH && name != NOISE_FOLDER_NAME {
                        found_folders.push((entry.path(), format!("{prefix}{name}/"), depth + 1));
                    }
                } else if file_type.is_file() {
                    let Some(relative) = normalize_entry_path(&format!("{prefix}{name}")) else { continue };
                    if is_noise_path(&relative) {
                        continue;
                    }
                    let Ok(metadata) = entry.metadata() else { continue };
                    found_files.push(SourceFile { relative, path: entry.path(), bytes: metadata.len(), unreadable: None });
                }
            }
            found_files.sort_by(|left, right| left.relative.cmp(&right.relative));
            files.append(&mut found_files);
            if files.len() > MAX_SOURCE_ENTRIES {
                return Err(ManifestError::new(
                    ManifestErrorCode::TooLarge,
                    messages::source_too_many_files(&display_name(root)),
                ));
            }
            // Pushed in reverse so that the folders are listed in name order.
            found_folders.sort_by(|left, right| right.1.cmp(&left.1));
            pending.append(&mut found_folders);
        }
        Ok(FileIndex::from_files(root.to_path_buf(), files))
    }

    /// The export root.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// Every file of the index, in listing order.
    pub fn files(&self) -> &[SourceFile] {
        &self.files
    }

    /// Resolves a normalised path from post.json (`normalize_relative_path`) to a file
    /// (source.ts:110-141; spec-export C.6). "fold" is lower-casing. In this order:
    ///
    /// 1. the file whose relative path equals `path` exactly;
    /// 2. the first listed file whose folded relative path equals the folded `path`;
    /// 3. foreign leading folders: with the folded segments of `path`, for `drop` from
    ///    `max(1, segments - MAX_SUFFIX_SEGMENTS)` to `segments - 2`: the file whose folded
    ///    relative path equals `segments[drop..]` joined; the first hit that is not a manifest
    ///    wins (the longest tail of at least two segments);
    /// 4. by file name, only when exactly ONE file qualifies: files that are not manifests and
    ///    have the same folded file name, where for a bare file name (one segment) a file at any
    ///    depth counts, and for a path with folders only a file lying directly in the root counts.
    ///
    /// A path that resolves to a manifest file (`is_manifest_name` at the root), or to nothing,
    /// is `None`: "not in the export".
    pub fn resolve(&self, path: &str) -> Option<&SourceFile> {
        self.resolve_position(path).map(|position| &self.files[position])
    }

    /// `resolve`, as an index into `files`.
    fn resolve_position(&self, path: &str) -> Option<usize> {
        let folded_path = fold(path);
        if let Some(position) = self.exact.get(path).or_else(|| self.folded.get(&folded_path)).copied() {
            let relative = &self.files[position].relative;
            let is_root_manifest = !relative.contains('/') && is_manifest_name(relative);
            return (!is_root_manifest).then_some(position);
        }

        // "C:/Users/me/export/posts/a.jpg", "export/posts/a.jpg": the longest tail of at least
        // two segments that names a file of the export.
        let segments: Vec<&str> = folded_path.split('/').collect();
        let first_drop = segments.len().saturating_sub(MAX_SUFFIX_SEGMENTS).max(1);
        for drop in first_drop..segments.len().saturating_sub(1) {
            if let Some(position) = self.folded.get(&segments[drop..].join("/")).copied() {
                if !is_manifest_name(base_name(&self.files[position].relative)) {
                    return Some(position);
                }
            }
        }

        // By file name, but only where it cannot pick up an unrelated file: "posts/a.jpg" may be
        // found as "a.jpg" right next to post.json, and a bare "a.jpg" may be found in a
        // subfolder. "avatars/a.jpg" is never satisfied by "posts/a.jpg".
        let same = self.by_name.get(*segments.last()?)?;
        if segments.len() == 1 {
            (same.anywhere == 1).then_some(same.first_anywhere)
        } else {
            (same.in_root == 1).then_some(same.first_in_root)
        }
    }
}

// --- Values (manifest.ts; spec-export C.3, C.4) -----------------------------------------------

/// Key normalisation of the alias tables: lower-case, then every character that is not a
/// letter or a number removed (`comment_count`, `commentCount`, `Comment-Count` and
/// `comment count` all become `commentcount`).
pub fn normalize_key(key: &str) -> String {
    // `is_word_char` is the clone's `\p{L}` / `\p{N}` test plus "_", which is no key character.
    key.to_lowercase().chars().filter(|character| *character != '_' && text::is_word_char(*character)).collect()
}

/// UTF-16 text in the byte order `unit` reads; a lone surrogate and an odd last byte become
/// U+FFFD.
fn decode_utf16(bytes: &[u8], unit: fn([u8; 2]) -> u16) -> String {
    let (pairs, odd_byte) = bytes.as_chunks::<2>();
    let has_odd_byte = !odd_byte.is_empty();
    let units = pairs.iter().map(|pair| unit(*pair));
    let mut decoded: String =
        char::decode_utf16(units).map(|unit| unit.unwrap_or(char::REPLACEMENT_CHARACTER)).collect();
    if has_odd_byte {
        decoded.push(char::REPLACEMENT_CHARACTER);
    }
    decoded
}

/// Text decoding of post.json (util.ts:67-76): bytes starting `FF FE` are UTF-16LE, `FE FF`
/// UTF-16BE, everything else UTF-8 with a leading BOM (`EF BB BF`) stripped. Invalid sequences
/// become U+FFFD; decoding never fails.
pub fn decode_manifest_text(bytes: &[u8]) -> String {
    match bytes {
        [0xFF, 0xFE, rest @ ..] => decode_utf16(rest, u16::from_le_bytes),
        [0xFE, 0xFF, rest @ ..] => decode_utf16(rest, u16::from_be_bytes),
        _ => {
            let without_bom = bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(bytes);
            String::from_utf8_lossy(without_bom).into_owned()
        }
    }
}

/// A non-negative amount as a count: rounded half up and capped at 2^53 - 1.
fn count_from_amount(amount: f64) -> Option<u64> {
    if !amount.is_finite() || amount < 0.0 {
        return None;
    }
    // The cast cannot overflow: the value is capped first.
    Some(text::js_round(amount).min(MAX_SAFE_COUNT as f64) as u64)
}

/// JavaScript's `String(number)` for a number of post.json, so that an id or a name written as
/// a number reads as it does in the clone: whole numbers without a fraction (`12345`, also for
/// `12345.0`), an exponent only below 1e-6 and from 1e21 on (`1e+21`).
fn js_number_text(number: &serde_json::Number) -> String {
    if let Some(whole) = number.as_u64() {
        return whole.to_string();
    }
    if let Some(whole) = number.as_i64() {
        return whole.to_string();
    }
    let value = number.as_f64().unwrap_or(0.0);
    if value == 0.0 {
        return "0".to_owned();
    }
    if (1e-6..1e21).contains(&value.abs()) {
        return format!("{value}");
    }
    let scientific = format!("{value:e}");
    match scientific.split_once('e') {
        Some((mantissa, exponent)) if !exponent.starts_with('-') => format!("{mantissa}e+{exponent}"),
        _ => scientific,
    }
}

/// The text of a string or a number (`String(value)` in the clone); `None` for any other value.
fn js_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Number(number) => Some(js_number_text(number)),
        _ => None,
    }
}

/// The characters that may continue the digits of a count: digits and the separators people
/// write between them.
fn is_count_char(character: char) -> bool {
    character.is_ascii_digit() || matches!(character, '.' | ',' | '\'' | '\u{2019}' | '_')
}

/// The magnitude words of a count, in the order the clone's pattern tries them, with their
/// multipliers.
const COUNT_SUFFIXES: [(&str, f64); 6] =
    [("k", 1e3), ("m", 1e6), ("b", 1e9), ("thousand", 1e3), ("million", 1e6), ("billion", 1e9)];

/// True for `^\s+[a-z]+$`: the unit word after a count ("1,234 likes").
fn is_unit_word(rest: &str) -> bool {
    let word = text::js_trim_start(rest);
    word.len() < rest.len() && !word.is_empty() && word.bytes().all(|byte| byte.is_ascii_lowercase())
}

/// True for `^\d{1,3}(?:[.,]\d{3})+$`: digits in groups of three ("1,234", "1.234.567").
fn is_grouped_number(digits: &str) -> bool {
    let mut groups = digits.split(['.', ',']);
    let first_is_short = groups.next().is_some_and(|first| (1..=3).contains(&first.len()));
    let mut further_groups = 0;
    for group in groups {
        if group.len() != 3 {
            return false;
        }
        further_groups += 1;
    }
    first_is_short && further_groups > 0
}

/// A count written as text; see `parse_count`. One pass over the text, whatever it holds.
fn parse_count_text(value: &str) -> Option<u64> {
    let lowered = text::js_trim(value).to_lowercase();
    if text::utf16_len(&lowered) > MAX_COUNT_CHARS {
        return None;
    }
    let characters: Vec<(usize, char)> = lowered.char_indices().collect();
    if !characters.first().is_some_and(|(_, first)| first.is_ascii_digit()) {
        return None;
    }

    // Group 1 of the pattern: white space belongs to the number only when more of it follows.
    let mut position = 1;
    while position < characters.len() {
        let character = characters[position].1;
        if is_count_char(character) {
            position += 1;
        } else if text::is_js_space(character) {
            let mut after_space = position;
            while after_space < characters.len() && text::is_js_space(characters[after_space].1) {
                after_space += 1;
            }
            if after_space < characters.len() && is_count_char(characters[after_space].1) {
                position = after_space;
            } else {
                break;
            }
        } else {
            break;
        }
    }
    let number_end = characters.get(position).map_or(lowered.len(), |(offset, _)| *offset);
    let (number, rest) = lowered.split_at(number_end);

    // The magnitude, then an optional unit word; or a unit word alone ("12 km" is 12).
    let multiplier = if rest.is_empty() {
        None
    } else {
        let word = text::js_trim_start(rest);
        let magnitude = COUNT_SUFFIXES.iter().find_map(|(suffix, multiplier)| {
            let after = word.strip_prefix(suffix)?;
            (after.is_empty() || is_unit_word(after)).then_some(*multiplier)
        });
        if magnitude.is_none() && !is_unit_word(rest) {
            return None;
        }
        magnitude
    };

    let digits: String = number.chars().filter(|character| character.is_ascii_digit() || matches!(character, '.' | ',')).collect();
    let amount: f64 = if !digits.contains(['.', ',']) {
        digits.parse().ok()?
    } else if multiplier.is_none() && is_grouped_number(&digits) {
        digits.replace(['.', ','], "").parse().ok()?
    } else {
        // The last separator is the decimal point (12.3K, 12,3K, 1.234,5).
        let last = digits.rfind(['.', ','])?;
        let whole = digits[..last].replace(['.', ','], "");
        let fraction = &digits[last + 1..];
        format!("{whole}.{}", if fraction.is_empty() { "0" } else { fraction }).parse().ok()?
    };
    count_from_amount(amount * multiplier.unwrap_or(1.0))
}

/// The clone's `parseCount` (manifest.ts:250-307; spec-export C.4 "Counts"), a hand-written
/// scanner because the pattern needs a look-ahead:
///
/// * number: finite and `>= 0` -> rounded half up, capped at 2^53 - 1; else `None`.
/// * object: its own key `count` (exact) holding a number or string -> that value; else `None`.
/// * string: `js_trim`, lower-case; longer than `MAX_COUNT_CHARS` -> `None`; must match
///   `^(\d(?:[\d.,'’_]|\s+(?=[\d.,'’_]))*)(?:\s*(k|m|b|thousand|million|billion))?(?:\s+[a-z]+)?$`
///   (ASCII digits; `\s` is `text::is_js_space`). The digit group, without white space, `'`,
///   `’` and `_`: no "." or "," -> an integer; no suffix and `^\d{1,3}(?:[.,]\d{3})+$` ->
///   grouping separators are removed; otherwise the LAST "." or "," is the decimal point and
///   earlier ones are removed. Multiplier k / thousand 1e3, m / million 1e6, b / billion 1e9;
///   result rounded half up, capped at 2^53 - 1. A trailing word after white space is a unit.
/// * anything else (true, [], null) -> `None`.
///
/// Vectors: "1,234" 1234; "1 234 567" 1234567; "12.3K" 12300; "12,3k" 12300; "1.2M" 1200000;
/// "2m" 2000000; "1.5B" 1500000000; "3 thousand" 3000; "12 k" 12000; "1'234" 1234; "1_000" 1000;
/// 12.6 -> 13; {"count": 77} -> 77; "1,234 likes" 1234; "12 km" 12. `None` for -1, "-5", "",
/// "abc", "K", true, [], {}, "12 34 x!".
pub fn parse_count(value: &Value) -> Option<u64> {
    match value {
        Value::Number(number) => match number.as_u64() {
            Some(whole) => Some(whole.min(MAX_SAFE_COUNT)),
            None => count_from_amount(number.as_f64()?),
        },
        // Instagram's own JSON nests counts as { "count": 12 }.
        Value::Object(object) => match object.get("count") {
            Some(inner @ (Value::Number(_) | Value::String(_))) => parse_count(inner),
            _ => None,
        },
        Value::String(text) => parse_count_text(text),
        _ => None,
    }
}

/// The clone's `parseBoolean`: `true` / `false`; the number 1, "true", "1", "yes" -> true; the
/// number 0, "false", "0", "no" -> false (exact, case-sensitive); anything else `None`.
pub fn parse_boolean(value: &Value) -> Option<bool> {
    match value {
        Value::Bool(flag) => Some(*flag),
        Value::Number(number) => {
            let number = number.as_f64()?;
            if number == 1.0 {
                Some(true)
            } else if number == 0.0 {
                Some(false)
            } else {
                None
            }
        }
        Value::String(text) => match text.as_str() {
            "true" | "1" | "yes" => Some(true),
            "false" | "0" | "no" => Some(false),
            _ => None,
        },
        _ => None,
    }
}

const IMAGE_TYPE_NAMES: [&str; 5] = ["image", "photo", "picture", "graphimage", "xdtgraphimage"];
const CAROUSEL_TYPE_NAMES: [&str; 7] =
    ["carousel", "carouselalbum", "album", "sidecar", "graphsidecar", "xdtgraphsidecar", "gallery"];
const VIDEO_TYPE_NAMES: [&str; 8] = ["video", "reel", "reels", "clip", "clips", "igtv", "graphvideo", "xdtgraphvideo"];

/// The clone's `parseMediaType` (manifest.ts:342-354): the number 1 -> image, 2 -> video, 8 ->
/// carousel; a string, trimmed and key-normalised: {image, photo, picture, graphimage,
/// xdtgraphimage} -> image; {carousel, carouselalbum, album, sidecar, graphsidecar,
/// xdtgraphsidecar, gallery} -> carousel; {video, reel, reels, clip, clips, igtv, graphvideo,
/// xdtgraphvideo} -> video; anything else `None`.
pub fn parse_media_type(value: &Value) -> Option<MediaType> {
    match value {
        Value::Number(number) => {
            let code = number.as_f64()?;
            if code == 1.0 {
                Some(MediaType::Image)
            } else if code == 2.0 {
                Some(MediaType::Video)
            } else if code == 8.0 {
                Some(MediaType::Carousel)
            } else {
                None
            }
        }
        Value::String(text) => {
            let name = normalize_key(text::js_trim(text));
            if IMAGE_TYPE_NAMES.contains(&name.as_str()) {
                Some(MediaType::Image)
            } else if CAROUSEL_TYPE_NAMES.contains(&name.as_str()) {
                Some(MediaType::Carousel)
            } else if VIDEO_TYPE_NAMES.contains(&name.as_str()) {
                Some(MediaType::Video)
            } else {
                None
            }
        }
        _ => None,
    }
}

/// The clone's `cleanUsername` for a string or number: to text, characters U+0000-U+001F and
/// U+007F removed, `js_trim`, leading "@"s stripped, `js_trim`, the first `MAX_USERNAME_CHARS`
/// UTF-16 code units; `None` when nothing is left or the value is of another type.
/// `settings` uses the same rule for `default_username`.
pub fn clean_username(value: &Value) -> Option<String> {
    let raw = js_text(value)?;
    let without_controls: String =
        raw.chars().filter(|character| !matches!(character, '\u{0}'..='\u{1F}' | '\u{7F}')).collect();
    let name = text::js_trim(text::js_trim(&without_controls).trim_start_matches('@'));
    if name.is_empty() {
        None
    } else {
        Some(text::truncate_utf16(name, MAX_USERNAME_CHARS).to_owned())
    }
}

/// The clone's `cleanText` for a value of post.json: a string or number as text, cleaned and cut
/// (`text::clean_text`); "" for a missing value or one of another type.
fn clean_text_value(value: Option<&Value>, max_units: usize) -> String {
    value.and_then(js_text).map(|raw| text::clean_text(&raw, max_units)).unwrap_or_default()
}

/// `clean_text_value`, with `None` for an empty result.
fn optional_text(value: Option<&Value>, max_units: usize) -> Option<String> {
    Some(clean_text_value(value, max_units)).filter(|cleaned| !cleaned.is_empty())
}

/// The clone's `cleanId` for a value of post.json (a string or a number).
fn clean_id_value(value: Option<&Value>) -> Option<String> {
    text::clean_id(&js_text(value?)?)
}

// --- Alias tables (manifest.ts:146-219; spec-export C.3), already normalised -------------------

const POSTS_ALIASES: &[&str] = &["posts", "items", "entries", "data"];
const IMAGE_ALIASES: &[&str] = &[
    "image",
    "imagepath",
    "path",
    "file",
    "media",
    "src",
    "imagefile",
    "imagefilename",
    "filepath",
    "filename",
    "photo",
    "photopath",
    "picture",
    "img",
];
const IMAGES_ALIASES: &[&str] = &["images", "imagepaths", "mediapaths", "photos", "pictures"];
/// The v1 media list; only an array or an object counts (a string is an "image" alias).
const MEDIA_ALIASES: &[&str] = &["media", "mediaitems"];
const MEDIA_PATH_ALIASES: &[&str] = &["path", "file", "src", "url", "filepath", "filename"];
const MEDIA_KIND_ALIASES: &[&str] = &["type", "kind", "mediatype"];
const VIDEO_ALIASES: &[&str] = &["video", "videopath", "videofile"];
const VIDEOS_ALIASES: &[&str] = &["videos", "videopaths"];
const POSTER_ALIASES: &[&str] =
    &["poster", "thumbnail", "cover", "posterpath", "thumbnailpath", "coverpath", "coverimage"];
const USER_ALIASES: &[&str] = &["user", "from", "owner", "author", "username", "fromuser", "postedby"];
const USERNAME_ALIASES: &[&str] = &["username", "name", "handle"];
const FULL_NAME_ALIASES: &[&str] = &["fullname", "displayname"];
const VERIFIED_ALIASES: &[&str] = &["verified", "isverified"];
const AVATAR_ALIASES: &[&str] = &["avatar", "avatarpath", "profilepic"];
const PROFILE_URL_ALIASES: &[&str] = &["profileurl", "url"];
const ENTRY_PROFILE_URL_ALIASES: &[&str] = &["profileurl"];
const CAPTION_ALIASES: &[&str] = &[
    "caption",
    "description",
    "firstcomment",
    "text",
    "userfirstcomment",
    "usersfirstcomment",
    "veryfirstcomment",
    "userveryfirstcomment",
    "usersveryfirstcomment",
];
const CAPTION_TEXT_ALIASES: &[&str] = &["text", "caption", "content", "description"];
const COMMENT_COUNT_ALIASES: &[&str] = &[
    "commentcount",
    "commentscount",
    "commentstotal",
    "totalcomments",
    "commenttotal",
    "commenttotalcount",
    "commentstotalcount",
    "totalcommentcount",
    "totalcommentscount",
    "numberofcomments",
    "numcomments",
];
const LIKE_COUNT_ALIASES: &[&str] = &["likecount", "likes", "likescount", "likestotal", "totallikes", "numberoflikes"];
const URL_ALIASES: &[&str] = &["url", "permalink", "link", "posturl"];
const ID_ALIASES: &[&str] = &["id", "shortcode", "code"];
const SHORTCODE_ALIASES: &[&str] = &["shortcode", "code"];
const TAKEN_AT_ALIASES: &[&str] = &["takenat", "timestamp", "date", "postedat", "createdat"];
const MEDIA_TYPE_ALIASES: &[&str] = &["mediatype", "type"];
const ALT_ALIASES: &[&str] = &["alt", "alttext", "accessibilitycaption"];
const LOCATION_ALIASES: &[&str] = &["location", "place"];
const LOCATION_NAME_ALIASES: &[&str] = &["name", "title", "label"];
const COMMENTS_ALIASES: &[&str] = &["comments", "commentsample", "samplecomments"];
const COMMENT_TEXT_ALIASES: &[&str] = &["text", "comment", "content", "body"];
const EXPORTED_AT_ALIASES: &[&str] = &["exportedat", "createdat", "date"];
const SOURCE_ALIASES: &[&str] = &["source", "sourceurl"];
const SOURCE_URL_ALIASES: &[&str] = &["url", "href", "link"];

/// The characters at which JavaScript's "." stops matching: a key holding one of them has no
/// "post" spelling.
const LINE_TERMINATORS: [char; 4] = ['\n', '\r', '\u{2028}', '\u{2029}'];

/// The clone's `withoutPostPrefix`: "post_image_path", "post caption", "postFrom" name the field
/// without the "post" in front. A separator (`^post[^a-z0-9]+(.+)$`, any letter case) or a
/// camelCase hump (`^[Pp]ost(?=[A-Z][a-z])(.+)$`) is required, so that "postcode" and "poster"
/// stay whole. The result is the normalised remainder, `None` when it is empty.
fn without_post_prefix(key: &str) -> Option<String> {
    let rest = strip_prefix_ignore_case(key, "post")?;
    let normalized_remainder = |remainder: &str| {
        if remainder.contains(LINE_TERMINATORS) {
            return None;
        }
        Some(normalize_key(remainder)).filter(|short| !short.is_empty())
    };

    let separators = rest.find(|character: char| character.is_ascii_alphanumeric()).unwrap_or(rest.len());
    if separators > 0 {
        // When the separators run to the end of the key, the pattern gives the last one back
        // as the remainder (which then normalises to nothing unless it is a letter).
        let start = if separators < rest.len() {
            separators
        } else {
            rest.char_indices().next_back().map(|(offset, _)| offset).filter(|offset| *offset > 0)?
        };
        return normalized_remainder(&rest[start..]);
    }

    let mut characters = rest.chars();
    let has_hump = characters.next().is_some_and(|first| first.is_ascii_uppercase())
        && characters.next().is_some_and(|second| second.is_ascii_lowercase());
    if has_hump && (key.starts_with("post") || key.starts_with("Post")) {
        return normalized_remainder(rest);
    }
    None
}

/// The clone's `Fields`: an alias-aware view of a JSON object that ignores letter case and
/// separators in its keys.
struct Fields<'a> {
    values: HashMap<String, &'a Value>,
}

impl<'a> Fields<'a> {
    fn new(source: &'a Map<String, Value>) -> Fields<'a> {
        let mut values: HashMap<String, &'a Value> = HashMap::with_capacity(source.len());
        // Two keys that normalise alike: the first in file order wins.
        for (key, value) in source {
            values.entry(normalize_key(key)).or_insert(value);
        }
        // Second, so that a field under its plain name always wins over a "post ..." spelling.
        for (key, value) in source {
            if let Some(short) = without_post_prefix(key) {
                values.entry(short).or_insert(value);
            }
        }
        Fields { values }
    }

    /// The value of the first alias that is present and not `null`.
    fn pick(&self, aliases: &[&str]) -> Option<&'a Value> {
        aliases.iter().find_map(|alias| self.values.get(*alias).copied().filter(|value| !value.is_null()))
    }

    /// `pick` over two alias lists, the first list first.
    fn pick_either(&self, first: &[&str], second: &[&str]) -> Option<&'a Value> {
        self.pick(first).or_else(|| self.pick(second))
    }
}

// --- Parsing post.json ------------------------------------------------------------------------

/// One picture or video named by an entry, before any file was looked at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawMedia {
    /// Normalised relative path (`normalize_relative_path`), unique within the post.
    pub path: String,
    /// What post.json says the item is; it only words the warnings, the file decides.
    pub declared: Option<MediaKind>,
    /// Normalised relative path of the poster, when one is named and usable.
    pub poster: Option<String>,
    /// The item's own alt text (at most `MAX_ALT_CHARS`).
    pub alt: Option<String>,
}

/// The author of an entry, before any file was looked at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawUser {
    /// Cleaned username; "unknown" when neither the entry nor the export names one.
    pub username: String,
    /// The display name (at most `MAX_NAME_CHARS`), when given.
    pub full_name: Option<String>,
    /// The verified badge, when the export says either way (`parse_boolean`).
    pub verified: Option<bool>,
    /// Normalised relative path of the profile picture, when it is a clean local path.
    pub avatar: Option<String>,
    /// Sanitised (`urls::sanitize_profile_url`) or guessed (`urls::guessed_profile_url`) link.
    pub profile_url: Option<String>,
}

/// One entry of post.json that names at least one usable media path.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RawPost {
    /// Zero-based index of the entry in post.json (warnings say `index + 1`).
    pub index: usize,
    /// Unique within the manifest: the given id (id / shortcode / code, or the shortcode in an
    /// Instagram permalink), else `text::derived_post_id`; made unique with `text::unique_id`.
    pub id: String,
    /// Sanitised post link, or `https://www.instagram.com/p/{shortcode}/` built from an explicit
    /// shortcode.
    pub permalink: Option<String>,
    /// 1 to `MAX_IMAGES_PER_POST` items, the cover first.
    pub media: Vec<RawMedia>,
    /// The author: the entry's own, else the export-level user, else "unknown".
    pub user: RawUser,
    /// Cleaned caption (`text::clean_text`, `MAX_CAPTION_CHARS`), "" when the entry has none.
    pub caption: String,
    /// Total comments of the original post (`parse_count`): the count field, else the number
    /// of listed comments (or a "comments" value that is itself a count); 0 when unreadable.
    pub comment_count: u64,
    /// Likes (`parse_count`); `None` when absent, `null` or unreadable.
    pub like_count: Option<u64>,
    /// Epoch milliseconds (`text::parse_timestamp`).
    pub taken_at: Option<i64>,
    /// The manifest-level type: a declared video or carousel is kept; otherwise carousel when
    /// there is more than one media item, else image.
    pub media_type: MediaType,
    /// Alt text of the post (at most `MAX_ALT_CHARS`), when given.
    pub alt: Option<String>,
    /// The place (a string, or the name of a location object; at most `MAX_LOCATION_CHARS`).
    pub location: Option<String>,
    /// At most `MAX_COMMENTS_PER_POST`; ids are `{post id}-c{1-based position}`.
    pub comments: Vec<Comment>,
}

/// What post.json says, before any file was looked at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManifestResult {
    /// The usable entries, in post.json order, with paths relative to the export root.
    pub posts: Vec<RawPost>,
    /// The sanitised source link of the export (`source` / `source_url`; `urls::sanitize_source_url`).
    pub source_url: Option<String>,
    /// `exported_at` (aliases `created_at`, `date`) in epoch milliseconds.
    pub exported_at: Option<i64>,
    /// Export-level warnings first, then per entry in post.json order.
    pub warnings: Vec<Warning>,
    /// How many entries post.json lists, including skipped ones and those beyond `MAX_POSTS`.
    pub total_entries: usize,
}

/// A media item as an entry writes it, before its path is normalised (manifest.ts `RawMedia`).
struct MediaCandidate {
    value: String,
    declared: Option<MediaKind>,
    poster: Option<String>,
    alt: Option<String>,
    /// The "image" field next to an "images" list: it goes first.
    is_cover: bool,
}

impl MediaCandidate {
    fn new(value: &str, declared: Option<MediaKind>) -> MediaCandidate {
        MediaCandidate { value: value.to_owned(), declared, poster: None, alt: None, is_cover: false }
    }
}

/// A string that is not blank.
fn non_blank(value: Option<&Value>) -> Option<&str> {
    match value {
        Some(Value::String(text)) if !text::js_trim(text).is_empty() => Some(text),
        _ => None,
    }
}

/// "image" / "video" of a media object; anything else (also "carousel") says nothing.
fn media_kind(value: Option<&Value>) -> Option<MediaKind> {
    match parse_media_type(value?)? {
        MediaType::Image => Some(MediaKind::Image),
        MediaType::Video => Some(MediaKind::Video),
        MediaType::Carousel => None,
    }
}

/// One item of a v1 "media" list: a path, or `{type?, path|file|src|url, poster?, alt?}`.
fn media_list_item(item: &Value) -> Option<MediaCandidate> {
    let object = match item {
        Value::String(_) => return non_blank(Some(item)).map(|path| MediaCandidate::new(path, None)),
        Value::Object(object) => object,
        _ => return None,
    };
    let fields = Fields::new(object);
    let mut value = fields.pick_either(MEDIA_PATH_ALIASES, IMAGE_ALIASES);
    let mut declared = media_kind(fields.pick(MEDIA_KIND_ALIASES));
    if !value.is_some_and(Value::is_string) {
        value = fields.pick(VIDEO_ALIASES);
        declared = declared.or(Some(MediaKind::Video));
    }
    let mut candidate = MediaCandidate::new(non_blank(value)?, declared);
    if let Some(Value::String(poster)) = fields.pick(POSTER_ALIASES) {
        candidate.poster = Some(poster.clone());
    }
    candidate.alt = optional_text(fields.pick(ALT_ALIASES), MAX_ALT_CHARS);
    Some(candidate)
}

/// True when collection stops: an entry cannot make the reader gather paths without end.
fn has_enough(items: &[MediaCandidate]) -> bool {
    items.len() > MAX_RAW_MEDIA_ITEMS
}

/// Gathers the paths of one of the older fields: a string, a list (nested once), or an object
/// that names its path under an "image" alias or "url".
fn collect_legacy(value: Option<&Value>, depth: usize, declared: Option<MediaKind>, items: &mut Vec<MediaCandidate>) {
    if has_enough(items) {
        return;
    }
    match value {
        Some(Value::String(_)) => {
            if let Some(path) = non_blank(value) {
                items.push(MediaCandidate::new(path, declared));
            }
        }
        Some(Value::Array(list)) if depth < 2 => {
            for item in list {
                collect_legacy(Some(item), depth + 1, declared, items);
            }
        }
        Some(Value::Object(object)) if depth < 3 => {
            if let Some(path) = non_blank(Fields::new(object).pick_either(IMAGE_ALIASES, &["url"])) {
                items.push(MediaCandidate::new(path, declared));
            }
        }
        _ => {}
    }
}

/// Raw media of an entry, before path normalisation: the v1 "media" list or the older fields
/// (manifest.ts `rawMedia`).
fn media_candidates(entry: &Fields) -> Vec<MediaCandidate> {
    let mut items: Vec<MediaCandidate> = Vec::new();
    match entry.pick(MEDIA_ALIASES) {
        Some(Value::Array(list)) => {
            for item in list {
                if has_enough(&items) {
                    break;
                }
                items.extend(media_list_item(item));
            }
            return items;
        }
        // A single object is a list of one.
        Some(single @ Value::Object(_)) => {
            items.extend(media_list_item(single));
            return items;
        }
        _ => {}
    }

    collect_legacy(entry.pick(IMAGES_ALIASES), 0, None, &mut items);
    let listed = items.len();
    collect_legacy(entry.pick(IMAGE_ALIASES), 0, None, &mut items);
    // "image" is the cover: when it is not already part of "images" it goes first.
    if listed > 0 {
        for item in &mut items[listed..] {
            item.is_cover = true;
        }
    }
    let first_video = items.len();
    collect_legacy(entry.pick(VIDEOS_ALIASES), 0, Some(MediaKind::Video), &mut items);
    collect_legacy(entry.pick(VIDEO_ALIASES), 0, Some(MediaKind::Video), &mut items);
    if let Some(Value::String(poster)) = entry.pick(POSTER_ALIASES) {
        if let Some(video) = items.get_mut(first_video) {
            video.poster = Some(poster.clone());
        }
    }
    items
}

/// What a link of post.json is rendered as; each kind only accepts the pages that fit its label.
#[derive(Clone, Copy)]
enum LinkKind {
    Post,
    Profile,
    Source,
}

/// The clone's `Parser`: turns entries into raw posts and collects the warnings.
struct Parser {
    warnings: Vec<Warning>,
    used_ids: HashSet<String>,
    default_user: Option<RawUser>,
}

impl Parser {
    fn new(default_user: Option<RawUser>) -> Parser {
        Parser { warnings: Vec::new(), used_ids: HashSet::new(), default_user }
    }

    fn warn(&mut self, code: WarningCode, message: String, index: Option<usize>, path: Option<&str>) {
        let mut warning = match index {
            Some(index) => Warning::for_post(code, message, index),
            None => Warning::new(code, message),
        };
        if let Some(path) = path {
            warning = warning.with_path(text::truncate_utf16(path, MAX_WARNING_PATH_CHARS));
        }
        self.warnings.push(warning);
    }

    /// A sanitised link; a value that is present but unacceptable gives an "unsafe-url" warning
    /// and is dropped. A missing value, `null` and "" say nothing.
    fn link(&mut self, value: Option<&Value>, kind: LinkKind, subject: LinkSubject, index: Option<usize>) -> Option<String> {
        let sanitized = match value? {
            Value::String(text) if text.is_empty() => return None,
            Value::String(text) => match kind {
                LinkKind::Post => urls::sanitize_post_url(text),
                LinkKind::Profile => urls::sanitize_profile_url(text),
                LinkKind::Source => urls::sanitize_source_url(text),
            },
            _ => Err(LinkRejection::NotAnHttpLink),
        };
        match sanitized {
            Ok(link) => Some(link),
            Err(rejection) => {
                let wrong_page = rejection == LinkRejection::WrongInstagramPage;
                self.warn(WarningCode::UnsafeUrl, messages::link_rejected(subject, wrong_page), index, None);
                None
            }
        }
    }

    /// The author named by `value` (a name, or an object with details). `entry` supplies details
    /// written next to a user given as a plain string. `permalink` is the post's link: the
    /// author of a post from another site is not given an instagram.com profile of the same name.
    fn user(&mut self, value: Option<&Value>, entry: Option<&Fields>, index: Option<usize>, permalink: Option<&str>) -> Option<RawUser> {
        let details = match value {
            Some(Value::Object(object)) => Some(Fields::new(object)),
            _ => None,
        };
        let username = match &details {
            Some(details) => details.pick(USERNAME_ALIASES).and_then(clean_username),
            None => value.and_then(clean_username),
        }?;
        let pick = |from_details: &[&str], from_entry: &[&str]| {
            details
                .as_ref()
                .and_then(|details| details.pick(from_details))
                .or_else(|| entry.and_then(|entry| entry.pick(from_entry)))
        };

        let full_name = optional_text(pick(FULL_NAME_ALIASES, FULL_NAME_ALIASES), MAX_NAME_CHARS);
        let verified = pick(VERIFIED_ALIASES, VERIFIED_ALIASES).and_then(parse_boolean);
        // Remote avatar URLs are never fetched; the avatar is optional, so they are just ignored.
        let avatar = match pick(AVATAR_ALIASES, AVATAR_ALIASES) {
            Some(Value::String(path)) => normalize_relative_path(path).ok(),
            _ => None,
        };
        let subject = match index {
            Some(index) => LinkSubject::PostProfile(index + 1),
            None => LinkSubject::Profile,
        };
        let profile_url = self
            .link(pick(PROFILE_URL_ALIASES, ENTRY_PROFILE_URL_ALIASES), LinkKind::Profile, subject, index)
            .or_else(|| urls::guessed_profile_url(&username, permalink));
        Some(RawUser { username, full_name, verified, avatar, profile_url })
    }

    /// The media of an entry with normalised, de-duplicated paths (manifest.ts `media`).
    fn media(&mut self, entry: &Fields, index: usize) -> Vec<RawMedia> {
        let number = index + 1;
        let mut media: Vec<RawMedia> = Vec::new();
        for candidate in media_candidates(entry) {
            let is_video = candidate.declared == Some(MediaKind::Video);
            match normalize_relative_path(&candidate.value) {
                Err(PathIssue::Remote) => self.warn(
                    WarningCode::MissingImage,
                    messages::media_is_a_url(number, &candidate.value, is_video),
                    Some(index),
                    Some(&candidate.value),
                ),
                Err(PathIssue::Escape) => self.warn(
                    WarningCode::InvalidEntry,
                    messages::media_path_outside(number, &candidate.value, is_video),
                    Some(index),
                    Some(&candidate.value),
                ),
                Err(PathIssue::Empty) => {}
                Ok(path) => {
                    if media.iter().any(|item| item.path == path) {
                        continue;
                    }
                    // Posters are optional: one that is remote or outside the export is not used.
                    let poster = candidate.poster.as_deref().and_then(|poster| normalize_relative_path(poster).ok());
                    let item = RawMedia { path, declared: candidate.declared, poster, alt: candidate.alt };
                    if candidate.is_cover {
                        media.insert(0, item);
                    } else {
                        media.push(item);
                    }
                }
            }
        }
        if media.len() > MAX_IMAGES_PER_POST {
            self.warn(WarningCode::Truncated, messages::too_many_media(number, media.len()), Some(index), None);
            media.truncate(MAX_IMAGES_PER_POST);
        }
        media
    }

    /// Makes an id unique. A given id that repeats is reported; a derived one that repeats (the
    /// same files listed twice) is renamed without a warning.
    fn unique_id(&mut self, candidate: &str, index: usize, derived: bool) -> String {
        let id = text::unique_id(candidate, &mut self.used_ids);
        if id != candidate && !derived {
            self.warn(WarningCode::DuplicateId, messages::duplicate_id(index + 1, candidate, &id), Some(index), None);
        }
        id
    }

    /// The username an entry names (or the export's default user), without any warnings.
    fn username_of(&self, entry: &Fields) -> Option<String> {
        let named = match entry.pick(USER_ALIASES) {
            Some(Value::Object(object)) => Fields::new(object).pick(USERNAME_ALIASES).and_then(clean_username),
            Some(value) => clean_username(value),
            None => None,
        };
        named.or_else(|| self.default_user.as_ref().map(|user| user.username.clone()))
    }

    fn post(&mut self, raw: &Value, index: usize) -> Option<RawPost> {
        let number = index + 1;
        let Value::Object(object) = raw else {
            self.warn(WarningCode::InvalidEntry, messages::entry_not_an_object(number), Some(index), None);
            return None;
        };
        let entry = Fields::new(object);

        let warnings_before = self.warnings.len();
        let media = self.media(&entry, index);
        if media.is_empty() {
            // Path problems were already reported; only say something when there was nothing at all.
            if self.warnings.len() == warnings_before {
                self.warn(WarningCode::InvalidEntry, messages::entry_without_media(number), Some(index), None);
            }
            return None;
        }

        let explicit_shortcode = clean_id_value(entry.pick(SHORTCODE_ALIASES));
        let permalink = self
            .link(entry.pick(URL_ALIASES), LinkKind::Post, LinkSubject::Post(number), Some(index))
            .or_else(|| explicit_shortcode.as_deref().and_then(urls::post_url_for));

        // Only an Instagram link carries a shortcode; the path of another site's link says nothing.
        let from_instagram = urls::is_from_instagram(permalink.as_deref());
        let given_id = clean_id_value(entry.pick(ID_ALIASES)).or_else(|| {
            permalink.as_deref().filter(|_| from_instagram).and_then(urls::shortcode_from_permalink)
        });
        let taken_at = entry.pick(TAKEN_AT_ALIASES).and_then(text::parse_timestamp);
        // Without an id or a link, the id is derived from what the post is, not from where it is
        // listed: opening the same export again then finds the descriptions typed for its posts.
        let id = match &given_id {
            Some(given) => self.unique_id(given, index, false),
            None => {
                let paths: Vec<String> = media.iter().map(|item| item.path.clone()).collect();
                let username = self.username_of(&entry).unwrap_or_default();
                self.unique_id(&text::derived_post_id(&username, taken_at, &paths), index, true)
            }
        };

        let user = self.user(entry.pick(USER_ALIASES), Some(&entry), Some(index), permalink.as_deref());
        let user = match user.or_else(|| self.default_user.clone()) {
            Some(user) => user,
            None => {
                self.warn(WarningCode::InvalidEntry, messages::entry_without_user(number), Some(index), None);
                RawUser {
                    username: messages::UNKNOWN_USERNAME.to_owned(),
                    full_name: None,
                    verified: None,
                    avatar: None,
                    profile_url: None,
                }
            }
        };

        let caption = match entry.pick(CAPTION_ALIASES) {
            Some(Value::Object(nested)) => clean_text_value(Fields::new(nested).pick(CAPTION_TEXT_ALIASES), MAX_CAPTION_CHARS),
            value => clean_text_value(value, MAX_CAPTION_CHARS),
        };

        let comments_value = entry.pick(COMMENTS_ALIASES);
        let comments = parse_comments(comments_value, &id);
        let comment_count = match (entry.pick(COMMENT_COUNT_ALIASES), comments_value) {
            (Some(count), _) => parse_count(count).unwrap_or(0),
            (None, Some(Value::Array(_))) => comments.len() as u64,
            // "comments": 42 is a count.
            (None, Some(count)) => parse_count(count).unwrap_or(0),
            (None, None) => 0,
        };

        let media_type = match entry.pick(MEDIA_TYPE_ALIASES).and_then(parse_media_type) {
            Some(declared @ (MediaType::Video | MediaType::Carousel)) => declared,
            _ if media.len() > 1 => MediaType::Carousel,
            _ => MediaType::Image,
        };

        let location = match entry.pick(LOCATION_ALIASES) {
            Some(Value::Object(nested)) => optional_text(Fields::new(nested).pick(LOCATION_NAME_ALIASES), MAX_LOCATION_CHARS),
            value => optional_text(value, MAX_LOCATION_CHARS),
        };

        Some(RawPost {
            index,
            id,
            permalink,
            media,
            user,
            caption,
            comment_count,
            like_count: entry.pick(LIKE_COUNT_ALIASES).and_then(parse_count),
            taken_at,
            media_type,
            alt: optional_text(entry.pick(ALT_ALIASES), MAX_ALT_CHARS),
            location,
            comments,
        })
    }
}

/// The sample comments of an entry (manifest.ts `comments`): only a list counts; each item must
/// be an object with a non-empty text.
fn parse_comments(value: Option<&Value>, post_id: &str) -> Vec<Comment> {
    let Some(Value::Array(list)) = value else {
        return Vec::new();
    };
    let mut comments: Vec<Comment> = Vec::new();
    for item in list {
        if comments.len() >= MAX_COMMENTS_PER_POST {
            break;
        }
        let Value::Object(object) = item else { continue };
        let fields = Fields::new(object);
        let text = clean_text_value(fields.pick(COMMENT_TEXT_ALIASES), MAX_COMMENT_CHARS);
        if text.is_empty() {
            continue;
        }
        let username = match fields.pick(USER_ALIASES) {
            Some(Value::Object(author)) => Fields::new(author).pick(USERNAME_ALIASES).and_then(clean_username),
            Some(author) => clean_username(author),
            None => None,
        };
        comments.push(Comment {
            id: format!("{post_id}-c{}", comments.len() + 1),
            username: username.unwrap_or_else(|| messages::UNKNOWN_USERNAME.to_owned()),
            text,
            likes: fields.pick(LIKE_COUNT_ALIASES).and_then(parse_count).unwrap_or(0),
            taken_at: fields.pick(TAKEN_AT_ALIASES).and_then(text::parse_timestamp),
        });
    }
    comments
}

/// `^(?:[a-z][a-z0-9+.-]*:|//)` in any letter case: a source that is written like a link. A bare
/// word such as "feed" is a label, not a link.
fn looks_like_link(text: &str) -> bool {
    if text.starts_with("//") {
        return true;
    }
    let mut characters = text.chars();
    if !characters.next().is_some_and(|first| first.is_ascii_alphabetic()) {
        return false;
    }
    characters.find(|character| !is_scheme_char(*character)) == Some(':')
}

/// The clone's `parseManifest`: the parsed JSON of post.json as raw posts. `max_posts` is
/// `MAX_POSTS` everywhere but in tests.
fn parse_manifest(json: &Value, max_posts: usize) -> Result<ManifestResult, ManifestError> {
    let (entries, top) = match json {
        Value::Array(entries) => (entries, None),
        Value::Object(object) => {
            let top = Fields::new(object);
            match top.pick(POSTS_ALIASES) {
                Some(Value::Array(entries)) => (entries, Some(top)),
                _ => {
                    return Err(ManifestError::new(ManifestErrorCode::InvalidManifest, messages::MANIFEST_NO_POSTS_LIST));
                }
            }
        }
        _ => return Err(ManifestError::new(ManifestErrorCode::InvalidManifest, messages::MANIFEST_WRONG_SHAPE)),
    };
    if entries.is_empty() {
        return Err(ManifestError::new(ManifestErrorCode::NoPosts, messages::MANIFEST_LISTS_NO_POSTS));
    }

    // Export-level metadata, parsed first so that it can supply a default user.
    let mut meta = Parser::new(None);
    let mut exported_at = None;
    let mut source_url = None;
    let mut default_user = None;
    if let Some(top) = &top {
        exported_at = top.pick(EXPORTED_AT_ALIASES).and_then(text::parse_timestamp);
        let source_link = match top.pick(SOURCE_ALIASES) {
            Some(Value::Object(source)) => Fields::new(source).pick(SOURCE_URL_ALIASES),
            other => other,
        };
        let is_judged = match source_link {
            Some(Value::String(text)) => looks_like_link(text::js_trim(text)),
            _ => true,
        };
        if is_judged {
            source_url = meta.link(source_link, LinkKind::Source, LinkSubject::Source, None);
        }
        default_user = meta.user(top.pick(USER_ALIASES), None, None, None);
        if default_user.is_none() {
            // An export of one profile: its posts are that profile's unless they say otherwise.
            if let Some(username) = source_url.as_deref().and_then(urls::username_from_profile_url) {
                default_user = meta.user(Some(&Value::String(username)), None, None, None);
            }
        }
    }

    let mut parser = Parser::new(default_user);
    parser.warnings.append(&mut meta.warnings);
    let total_entries = entries.len();
    if total_entries > max_posts {
        parser.warn(WarningCode::Truncated, messages::manifest_truncated(total_entries, max_posts), None, None);
    }
    let posts: Vec<RawPost> = entries
        .iter()
        .take(max_posts)
        .enumerate()
        .filter_map(|(index, entry)| parser.post(entry, index))
        .collect();

    Ok(ManifestResult { posts, source_url, exported_at, warnings: parser.warnings, total_entries })
}

/// Parses the bytes of post.json (manifest.ts `parseManifest` and `Parser`; spec-export C.1
/// decoding, C.2 shapes, C.3 alias tables, C.4 per-entry rules, C.7 links). Pure: no file is
/// touched, so every rule can be tested with a JSON literal.
///
/// Errors, each with its sentence from `messages`: larger than `MAX_MANIFEST_BYTES` ->
/// `TooLarge` / `manifest_too_large`; not JSON -> `InvalidJson` / `manifest_invalid_json` (with
/// serde_json's message); an object without a posts list -> `InvalidManifest` /
/// `MANIFEST_NO_POSTS_LIST`; neither object nor array -> `InvalidManifest` /
/// `MANIFEST_WRONG_SHAPE`; an empty list -> `NoPosts` / `MANIFEST_LISTS_NO_POSTS`.
///
/// Everything else is a warning (codes in `model::WarningCode`, sentences in `messages`): more
/// than `MAX_POSTS` entries (`manifest_truncated`, only the first are parsed), an entry that is
/// not an object, URL or escaping media paths, more than `MAX_IMAGES_PER_POST` media, an entry
/// without media, a rejected link (`link_rejected`), a reused given id (`duplicate_id`), an
/// entry without an author (`entry_without_user`). An entry without any usable media is not in
/// `posts`. `format` and `version` are not checked.
///
/// The JSON engine is serde_json, which is stricter than a browser's `JSON.parse`. Two things a
/// browser reads are repaired before the text is given up (`repair_for_serde`): half of a
/// surrogate pair written as a `\u` escape (what a tool writes that cut a caption in the middle
/// of an emoji) becomes U+FFFD, the character a browser shows for it, and a number too large for
/// a 64-bit float (a browser reads Infinity) becomes null. Nesting deeper than 128 levels stays
/// "not valid JSON" here.
///
/// The fixture core/tests/fixtures/export-lenient/Post.JSON exercises the alias tables; the
/// clone's manifest.test.ts holds the vectors to port.
pub fn read_manifest(bytes: &[u8]) -> Result<ManifestResult, ManifestError> {
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_MANIFEST_BYTES {
        return Err(ManifestError::new(ManifestErrorCode::TooLarge, messages::manifest_too_large()));
    }
    let text = decode_manifest_text(bytes);
    let json: Value = match serde_json::from_str(&text) {
        Ok(json) => json,
        // Repairing costs a pass over the text, so only a text serde_json refused pays for it.
        Err(error) => match repair_for_serde(&text).and_then(|repaired| serde_json::from_str(&repaired).ok()) {
            Some(json) => json,
            None => {
                return Err(ManifestError::new(
                    ManifestErrorCode::InvalidJson,
                    messages::manifest_invalid_json(&error.to_string()),
                ))
            }
        },
    };
    parse_manifest(&json, MAX_POSTS)
}

/// The four hexadecimal digits of a `\u` escape that start at `start`, as a UTF-16 code unit.
fn escaped_unit(bytes: &[u8], start: usize) -> Option<u16> {
    let digits = std::str::from_utf8(bytes.get(start..start + 4)?).ok()?;
    u16::from_str_radix(digits, 16).ok()
}

/// The JSON text with what a browser accepts and serde_json refuses made acceptable (see
/// `read_manifest`): a `\u` escape of a lone high or low surrogate becomes `�`, a number
/// literal that overflows a 64-bit float becomes `null`. `None` when nothing needed a repair.
/// Only ASCII sequences are replaced, so the result is valid UTF-8 wherever the text was.
fn repair_for_serde(text: &str) -> Option<String> {
    const REPLACEMENT_ESCAPE: &str = "\\ufffd";
    let is_high = |unit: u16| (0xD800..=0xDBFF).contains(&unit);
    let is_low = |unit: u16| (0xDC00..=0xDFFF).contains(&unit);
    let bytes = text.as_bytes();
    let mut repaired = String::new();
    let mut copied_until = 0;
    let mut replace = |repaired: &mut String, range: std::ops::Range<usize>, with: &str| {
        repaired.push_str(&text[copied_until..range.start]);
        repaired.push_str(with);
        copied_until = range.end;
    };
    let mut in_string = false;
    let mut position = 0;
    while position < bytes.len() {
        match (in_string, bytes[position]) {
            (true, b'\\') => {
                let unit = if bytes.get(position + 1) == Some(&b'u') { escaped_unit(bytes, position + 2) } else { None };
                match unit {
                    Some(high) if is_high(high) => {
                        let pair = bytes.get(position + 6..position + 8) == Some(b"\\u")
                            && escaped_unit(bytes, position + 8).is_some_and(is_low);
                        if pair {
                            position += 12;
                        } else {
                            replace(&mut repaired, position..position + 6, REPLACEMENT_ESCAPE);
                            position += 6;
                        }
                    }
                    Some(low) if is_low(low) => {
                        replace(&mut repaired, position..position + 6, REPLACEMENT_ESCAPE);
                        position += 6;
                    }
                    Some(_) => position += 6,
                    // Any other escape is two characters long.
                    None => position += 2,
                }
            }
            (true, b'"') => {
                in_string = false;
                position += 1;
            }
            (false, b'"') => {
                in_string = true;
                position += 1;
            }
            (false, b'-' | b'0'..=b'9') => {
                let start = position;
                while position < bytes.len() && matches!(bytes[position], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E') {
                    position += 1;
                }
                if text[start..position].parse::<f64>().is_ok_and(f64::is_infinite) {
                    replace(&mut repaired, start..position, "null");
                }
            }
            _ => position += 1,
        }
    }
    if copied_until == 0 {
        return None;
    }
    repaired.push_str(&text[copied_until..]);
    Some(repaired)
}

// --- Assembling the posts ---------------------------------------------------------------------

/// An export read from disk.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedExport {
    /// The posts that have at least one usable media file, in post.json order.
    pub posts: Vec<Post>,
    /// Export-level warnings first, then by entry index (stable).
    pub warnings: Vec<Warning>,
    /// The sanitised source link of the export, when present (`ManifestResult::source_url`).
    pub source_url: Option<String>,
    /// When the export was written, epoch milliseconds, when post.json says.
    pub exported_at: Option<i64>,
    /// How many entries post.json lists; `posts.len()` is lower when entries were skipped.
    pub total_entries: usize,
}

/// What a path of post.json wants its file for (core.ts `jobFor`).
#[derive(Clone, Copy, PartialEq, Eq)]
enum FileUse {
    /// A media item that post.json does not call a video.
    Media,
    /// A media item that post.json calls a video.
    Video,
    /// A poster or a profile picture: only a picture will do.
    Picture,
}

/// One distinct file that post.json references, however often (core.ts `MediaJob`).
struct FileJob {
    /// Index into `FileIndex::files`.
    file: usize,
    /// True while the file is only wanted as a picture (poster, avatar).
    image_only: bool,
    /// True when post.json calls the file a video somewhere; it only words the warnings.
    declared_video: bool,
}

/// What looking at one file gave (core.ts `Loaded`).
enum FileOutcome {
    Usable(MediaKind),
    Unusable { code: WarningCode, message: String },
}

/// The distinct files post.json references, each looked at once.
struct FilePlan<'a> {
    index: &'a FileIndex,
    jobs: Vec<FileJob>,
    job_of_file: HashMap<usize, usize>,
}

impl<'a> FilePlan<'a> {
    fn new(index: &'a FileIndex) -> FilePlan<'a> {
        FilePlan { index, jobs: Vec::new(), job_of_file: HashMap::new() }
    }

    /// The job for a path of post.json; `None` when the path is not in the export.
    fn job_for(&mut self, path: &str, wanted: FileUse) -> Option<usize> {
        let file = self.index.resolve_position(path)?;
        let jobs = &mut self.jobs;
        let job = *self.job_of_file.entry(file).or_insert_with(|| {
            jobs.push(FileJob { file, image_only: true, declared_video: false });
            jobs.len() - 1
        });
        if wanted != FileUse::Picture {
            self.jobs[job].image_only = false;
        }
        if wanted == FileUse::Video {
            self.jobs[job].declared_video = true;
        }
        Some(job)
    }

    fn file(&self, job: usize) -> &'a SourceFile {
        &self.index.files[self.jobs[job].file]
    }

    /// Plans every file `parsed` references, in the order `assemble_export` does: the media of
    /// every post, then the posters, then the avatars (a file used both ways counts as media).
    /// Returns the jobs of the media and of the posters, per post and slot, and of the avatars.
    fn plan_all(&mut self, raw_posts: &'a [RawPost]) -> PlannedJobs<'a> {
        let media: Vec<Vec<Option<usize>>> = raw_posts
            .iter()
            .map(|post| {
                let wanted = |item: &RawMedia| if item.declared == Some(MediaKind::Video) { FileUse::Video } else { FileUse::Media };
                post.media.iter().map(|item| self.job_for(&item.path, wanted(item))).collect()
            })
            .collect();
        let posters: Vec<Vec<Option<usize>>> = raw_posts
            .iter()
            .map(|post| {
                post.media
                    .iter()
                    .map(|item| item.poster.as_deref().and_then(|poster| self.job_for(poster, FileUse::Picture)))
                    .collect()
            })
            .collect();
        let mut avatars: HashMap<&'a str, Option<usize>> = HashMap::new();
        for post in raw_posts {
            if let Some(avatar) = post.user.avatar.as_deref() {
                if !avatars.contains_key(avatar) {
                    avatars.insert(avatar, self.job_for(avatar, FileUse::Picture));
                }
            }
        }
        PlannedJobs { media, posters, avatars }
    }
}

/// The jobs `FilePlan::plan_all` made for the paths of post.json.
struct PlannedJobs<'a> {
    media: Vec<Vec<Option<usize>>>,
    posters: Vec<Vec<Option<usize>>>,
    avatars: HashMap<&'a str, Option<usize>>,
}

/// The largest file a reference may name: a picture-only use (poster, avatar) takes up to
/// `MAX_IMAGE_BYTES`, a media item the larger of the picture and the video limit (core.ts:151).
pub fn max_bytes_for(image_only: bool) -> u64 {
    if image_only {
        MAX_IMAGE_BYTES
    } else {
        MAX_IMAGE_BYTES.max(MAX_VIDEO_BYTES)
    }
}

/// One file of an index that post.json references (`referenced_files`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReferencedFile {
    /// Its position in `FileIndex::files`.
    pub file: usize,
    /// True when it is only wanted as a picture (a poster or an avatar): then at most
    /// `MAX_IMAGE_BYTES` of it are of use (`max_bytes_for`).
    pub image_only: bool,
}

/// The distinct files of `index` that the posts of `parsed` reference (media, posters, avatars),
/// each once, in the order `assemble_export` looks at them. An archive extracts exactly these
/// members, so that `assemble_export` finds every file it will look at.
pub fn referenced_files(parsed: &ManifestResult, index: &FileIndex) -> Vec<ReferencedFile> {
    let mut plan = FilePlan::new(index);
    let _ = plan.plan_all(&parsed.posts);
    plan.jobs.iter().map(|job| ReferencedFile { file: job.file, image_only: job.image_only }).collect()
}

/// The shallowest manifest among the normalised relative paths of an archive, as the clone's
/// `EntryIndex.findManifest` picks it (source.ts:85-102): of the paths whose file name is a
/// manifest name (`is_manifest_name`), the one with the smallest (number of segments, rank in
/// `MANIFEST_NAMES`, path text). A path listed twice counts once. `None` when there is none.
/// Unlike `find_manifest` for a folder there is no depth limit: the whole listing is at hand.
pub fn find_manifest_entry<'a>(paths: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let mut best: Option<((usize, usize, &'a str), &'a str)> = None;
    for path in paths {
        let Some(rank) = manifest_rank(base_name(path)) else { continue };
        let key = (path.split('/').count(), rank, path);
        if best.as_ref().is_none_or(|(best_key, _)| key < *best_key) {
            best = Some((key, path));
        }
    }
    best.map(|(_, path)| path)
}

/// Looks at one referenced file: its size and its first bytes (core.ts `loadMedia`, without
/// loading the content).
fn inspect_file(file: &SourceFile, job: &FileJob) -> FileOutcome {
    let by_declaration = if job.declared_video { WarningCode::UnsupportedMedia } else { WarningCode::UnsupportedImage };
    let unusable = |code: WarningCode, message: String| FileOutcome::Unusable { code, message };
    let name = file.relative.as_str();

    let max_bytes = max_bytes_for(job.image_only);
    if file.bytes > max_bytes {
        return unusable(by_declaration, messages::file_too_large(name, max_bytes));
    }
    // A member of an archive that could not be extracted: the clone's open() failed the same way.
    if let Some(reason) = &file.unreadable {
        return unusable(by_declaration, messages::file_unreadable(name, reason));
    }
    if file.bytes == 0 {
        return unusable(by_declaration, messages::file_empty(name));
    }
    let head = match sniff::read_head(&file.path) {
        Ok(head) => head,
        Err(error) => return unusable(by_declaration, messages::file_unreadable(name, &error.to_string())),
    };
    if head.is_empty() {
        // Emptied since the folder was listed.
        return unusable(by_declaration, messages::file_empty(name));
    }
    let Some(sniffed) = sniff::sniff_media(&head) else {
        return match sniff::describe_unsupported_media(&head) {
            Some(known) => unusable(WarningCode::UnsupportedMedia, messages::file_known_unsupported(name, known)),
            None if job.declared_video => unusable(by_declaration, messages::file_not_a_video(name)),
            None => unusable(by_declaration, messages::file_not_supported(name)),
        };
    };
    match sniffed.kind {
        MediaKind::Video if job.image_only => unusable(WarningCode::UnsupportedImage, messages::file_is_a_video(name)),
        MediaKind::Video if file.bytes > MAX_VIDEO_BYTES => {
            unusable(by_declaration, messages::file_too_large(name, MAX_VIDEO_BYTES))
        }
        MediaKind::Image if file.bytes > MAX_IMAGE_BYTES => {
            unusable(WarningCode::UnsupportedImage, messages::file_too_large(name, MAX_IMAGE_BYTES))
        }
        kind => FileOutcome::Usable(kind),
    }
}

/// The post-level type once the files are known (core.ts `finalMediaType`): an undeclared
/// single file may be a video.
fn final_media_type(declared: MediaType, media: &[MediaItem]) -> MediaType {
    if declared != MediaType::Image {
        return declared;
    }
    if media.len() > 1 {
        return MediaType::Carousel;
    }
    match media.first().map(|item| item.kind) {
        Some(MediaKind::Video) => MediaType::Video,
        _ => MediaType::Image,
    }
}

/// Turns the raw posts into `model::Post`s by looking at the files (core.ts:143-478; spec-export
/// C.5). `manifest_path` is the post.json that was read; `index` lists the files of its folder.
///
/// Per media item: resolve the path (`FileIndex::resolve`); missing -> warning
/// `media_not_in_export`. Read the first bytes and sniff (`sniff::sniff_file`): unreadable ->
/// `file_unreadable`; empty -> `file_empty`; not supported -> `file_known_unsupported` (when
/// `sniff::describe_unsupported_media` knows the format), else `file_not_a_video` for an item
/// declared a video, else `file_not_supported`. A picture larger than `MAX_IMAGE_BYTES` or a
/// video larger than `MAX_VIDEO_BYTES` -> `file_too_large`. File-level messages are prefixed
/// with `for_post`. The warning code is `UnsupportedMedia` when post.json declared the item a
/// video, else `UnsupportedImage` (`MissingImage` for a missing file). The item's kind is the
/// SNIFFED kind.
///
/// A poster is attached only to an item that sniffed as a video and only when the poster file
/// sniffs as a picture of at most `MAX_IMAGE_BYTES`; otherwise the post is kept and a warning
/// says why (`poster_not_in_export`, `poster_is_a_video`, `poster_problem`). An avatar is kept
/// only when it is a picture; problems are reported once per avatar path (`avatar_not_in_export`,
/// `avatar_is_a_video`, `avatar_problem`).
///
/// A post keeps the media that loaded; a post without any is skipped (its warnings say why).
/// When no post is left: `Err` with code `NoPosts` and `messages::no_post_imported(total_entries,
/// <message of the earliest skipped entry's warning>)`.
///
/// The resulting `Post`: `key = PostKey::for_export_post(manifest_path, id)`; `caption` and
/// `original_caption` both the manifest caption (`catalog` applies stored descriptions);
/// `comment_count = max(raw count, comments.len())`; `media_type` after `finalMediaType`: when
/// the manifest-level type is image, more than one loaded media -> carousel, a single video ->
/// video; `context = None`; `MediaItem::manifest_path` the normalised path from post.json,
/// `MediaItem::path` and `User::avatar` absolute paths of the resolved files.
pub fn assemble_export(
    parsed: ManifestResult,
    index: &FileIndex,
    manifest_path: &Path,
) -> Result<LoadedExport, ManifestError> {
    let ManifestResult { posts: raw_posts, source_url, exported_at, mut warnings, total_entries } = parsed;

    // One job per distinct file, however many posts reference it. Posters and avatars are
    // planned after the media, so that a file used both ways counts as media.
    let mut plan = FilePlan::new(index);
    let PlannedJobs { media: media_jobs, posters: poster_jobs, avatars: avatar_jobs } = plan.plan_all(&raw_posts);
    let outcomes: Vec<FileOutcome> =
        plan.jobs.iter().map(|job| inspect_file(&index.files[job.file], job)).collect();

    // The warnings that say why a post is missing, as opposed to notes about a post that is
    // shown: those of entries the parser rejected and, added below, those about media that are
    // missing or unusable. Each is (entry index, message).
    let parsed_entries: HashSet<usize> = raw_posts.iter().map(|post| post.index).collect();
    let mut skip_reasons: Vec<(usize, String)> = warnings
        .iter()
        .filter_map(|warning| Some((warning.post_index?, warning.message.clone())))
        .filter(|(entry, _)| !parsed_entries.contains(entry))
        .collect();

    let mut posts: Vec<Post> = Vec::new();
    let mut reported_avatars: HashSet<&str> = HashSet::new();
    for (position, source) in raw_posts.iter().enumerate() {
        let number = source.index + 1;
        let mut media: Vec<MediaItem> = Vec::new();
        let mut poster_warnings: Vec<Warning> = Vec::new();
        for (slot, item) in source.media.iter().enumerate() {
            let declared_video = item.declared == Some(MediaKind::Video);
            let job = media_jobs[position][slot];
            let problem = match job.map(|job| &outcomes[job]) {
                None => Some((WarningCode::MissingImage, messages::media_not_in_export(number, &item.path, declared_video))),
                Some(FileOutcome::Unusable { code, message }) => Some((*code, messages::for_post(number, message))),
                Some(FileOutcome::Usable(_)) => None,
            };
            if let Some((code, message)) = problem {
                skip_reasons.push((source.index, message.clone()));
                warnings.push(Warning::for_post(code, message, source.index).with_path(item.path.as_str()));
                continue;
            }
            let (Some(job), Some(FileOutcome::Usable(kind))) = (job, job.map(|job| &outcomes[job])) else { continue };
            let file = plan.file(job);
            let mut loaded = MediaItem {
                kind: *kind,
                path: file.path.clone(),
                manifest_path: item.path.clone(),
                poster: None,
                alt: item.alt.clone(),
                bytes: file.bytes,
            };
            if let (MediaKind::Video, Some(poster_path)) = (*kind, item.poster.as_deref()) {
                let poster_job = poster_jobs[position][slot];
                match poster_job.map(|job| &outcomes[job]) {
                    Some(FileOutcome::Usable(MediaKind::Image)) => {
                        if let Some(poster_job) = poster_job {
                            loaded.poster =
                                Some(Poster { path: plan.file(poster_job).path.clone(), manifest_path: poster_path.to_owned() });
                        }
                    }
                    outcome => {
                        let (code, message) = match outcome {
                            None => (
                                WarningCode::MissingImage,
                                messages::poster_not_in_export(number, &item.path, poster_path),
                            ),
                            Some(FileOutcome::Usable(_)) => (
                                WarningCode::UnsupportedImage,
                                messages::poster_is_a_video(number, &item.path, poster_path),
                            ),
                            Some(FileOutcome::Unusable { code, message }) => {
                                (*code, messages::poster_problem(number, &item.path, message))
                            }
                        };
                        poster_warnings.push(Warning::for_post(code, message, source.index).with_path(poster_path));
                    }
                }
            }
            media.push(loaded);
        }
        if media.is_empty() {
            // Skipped; the warnings above say why.
            continue;
        }
        warnings.append(&mut poster_warnings);

        let mut user = User {
            username: source.user.username.clone(),
            full_name: source.user.full_name.clone(),
            verified: source.user.verified,
            avatar: None,
            profile_url: source.user.profile_url.clone(),
        };
        if let Some(avatar_path) = source.user.avatar.as_deref() {
            let avatar_job = avatar_jobs.get(avatar_path).copied().flatten();
            match avatar_job.map(|job| &outcomes[job]) {
                Some(FileOutcome::Usable(MediaKind::Image)) => {
                    user.avatar = avatar_job.map(|job| plan.file(job).path.clone());
                }
                // Once per avatar file, not once per post of that user.
                outcome if reported_avatars.insert(avatar_path) => {
                    let username = source.user.username.as_str();
                    let (code, message) = match outcome {
                        None => (WarningCode::MissingImage, messages::avatar_not_in_export(avatar_path, username)),
                        Some(FileOutcome::Usable(_)) => {
                            (WarningCode::UnsupportedImage, messages::avatar_is_a_video(username, avatar_path))
                        }
                        Some(FileOutcome::Unusable { code, message }) => (*code, messages::avatar_problem(username, message)),
                    };
                    warnings.push(Warning::new(code, message).with_path(avatar_path));
                }
                _ => {}
            }
        }

        posts.push(Post {
            key: PostKey::for_export_post(manifest_path, &source.id),
            id: source.id.clone(),
            permalink: source.permalink.clone(),
            media_type: final_media_type(source.media_type, &media),
            media,
            user,
            caption: source.caption.clone(),
            original_caption: source.caption.clone(),
            comment_count: source.comment_count.max(source.comments.len() as u64),
            like_count: source.like_count,
            taken_at: source.taken_at,
            alt: source.alt.clone(),
            location: source.location.clone(),
            context: None,
            comments: source.comments.clone(),
            origin: PostOrigin::Folder { manifest: manifest_path.to_path_buf() },
        });
    }

    if posts.is_empty() {
        // The earliest entry of post.json that failed, with the problem that made it fail.
        let reason = skip_reasons.iter().min_by_key(|(entry, _)| *entry).map(|(_, message)| message.as_str());
        return Err(ManifestError::new(ManifestErrorCode::NoPosts, messages::no_post_imported(total_entries, reason)));
    }

    // Export-level warnings first, then per post in manifest order. The sort is stable.
    warnings.sort_by_key(|warning| warning.post_index.map_or(0, |entry| entry + 1));

    Ok(LoadedExport { posts, warnings, source_url, exported_at, total_entries })
}

/// Reads the export whose manifest is `manifest_path` (a post.json / posts.json found by
/// `find_manifest`, or picked by the user): reads the file (`ReadFailed` /
/// `messages::source_not_imported` when it cannot be read; `TooLarge` when it is larger than
/// `MAX_MANIFEST_BYTES`, checked before reading), `read_manifest`, `FileIndex::scan` of its
/// folder, `assemble_export`. The export folder is only read, never written.
pub fn load_export(manifest_path: &Path) -> Result<LoadedExport, ManifestError> {
    let manifest_path = std::path::absolute(manifest_path).unwrap_or_else(|_| manifest_path.to_path_buf());
    let root = manifest_path.parent().map(Path::to_path_buf).unwrap_or_default();
    let unreadable = |error: std::io::Error| {
        ManifestError::new(
            ManifestErrorCode::ReadFailed,
            messages::source_not_imported(&display_name(&root), &error.to_string()),
        )
    };

    let metadata = fs::metadata(&manifest_path).map_err(unreadable)?;
    if metadata.len() > MAX_MANIFEST_BYTES {
        return Err(ManifestError::new(ManifestErrorCode::TooLarge, messages::manifest_too_large()));
    }
    let bytes = fs::read(&manifest_path).map_err(unreadable)?;
    let parsed = read_manifest(&bytes)?;
    let index = FileIndex::scan(&root)?;
    assemble_export(parsed, &index, &manifest_path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn parse(json: Value) -> ManifestResult {
        parse_manifest(&json, MAX_POSTS).expect("the manifest parses")
    }

    fn error_code(json: Value) -> ManifestErrorCode {
        parse_manifest(&json, MAX_POSTS).expect_err("the manifest is rejected").code
    }

    /// The first post of a manifest that is a bare list of one entry.
    fn first(entry: Value) -> RawPost {
        parse(json!([entry])).posts.remove(0)
    }

    /// An entry by "alice" with one picture, plus the given fields.
    fn with_base(extra: Value) -> Value {
        let mut entry = json!({ "user": "alice", "image": "posts/a.jpg" });
        if let (Value::Object(entry), Value::Object(extra)) = (&mut entry, extra) {
            entry.extend(extra);
        }
        entry
    }

    fn paths(post: &RawPost) -> Vec<&str> {
        post.media.iter().map(|item| item.path.as_str()).collect()
    }

    fn codes(result: &ManifestResult) -> Vec<(WarningCode, Option<usize>)> {
        result.warnings.iter().map(|warning| (warning.code, warning.post_index)).collect()
    }

    fn plain(path: &str) -> RawMedia {
        RawMedia { path: path.to_owned(), declared: None, poster: None, alt: None }
    }

    fn video(path: &str, poster: Option<&str>) -> RawMedia {
        RawMedia { path: path.to_owned(), declared: Some(MediaKind::Video), poster: poster.map(str::to_owned), alt: None }
    }

    const NOVEMBER_FIRST_MS: i64 = 1_793_545_445_000;

    // --- parseCount (manifest.test.ts:15-75) --------------------------------------------------

    #[test]
    fn counts_are_parsed_like_the_clone() {
        let vectors: [(Value, u64); 18] = [
            (json!(0), 0),
            (json!(1234), 1234),
            (json!(12.6), 13),
            (json!("1234"), 1234),
            (json!("1,234"), 1234),
            (json!("1.234"), 1234),
            (json!("1 234"), 1234),
            (json!("1 234 567"), 1_234_567),
            (json!("1,234,567"), 1_234_567),
            (json!("12.3K"), 12_300),
            (json!("12,3k"), 12_300),
            (json!("1.2M"), 1_200_000),
            (json!("2m"), 2_000_000),
            (json!("1.5B"), 1_500_000_000),
            (json!("3 thousand"), 3000),
            (json!("1,234 likes"), 1234),
            (json!(" 56 "), 56),
            (json!({ "count": 77 }), 77),
        ];
        for (input, expected) in vectors {
            assert_eq!(parse_count(&input), Some(expected), "{input}");
        }
        for input in [
            json!(null),
            json!(-1),
            json!(-0.5),
            json!("-5"),
            json!(""),
            json!("abc"),
            json!("K"),
            json!(true),
            json!([]),
            json!({}),
            json!({ "count": {} }),
            json!("12kilo"),
            json!("1 2 3 !"),
        ] {
            assert_eq!(parse_count(&input), None, "{input}");
        }
    }

    #[test]
    fn counts_read_every_sensible_spelling() {
        assert_eq!(parse_count(&json!("1\u{a0}234\u{202f}567")), Some(1_234_567));
        assert_eq!(parse_count(&json!("1'234")), Some(1234));
        assert_eq!(parse_count(&json!("1\u{2019}234")), Some(1234));
        assert_eq!(parse_count(&json!("1_000")), Some(1000));
        assert_eq!(parse_count(&json!("12 k")), Some(12_000));
        assert_eq!(parse_count(&json!("3  million")), Some(3_000_000));
        assert_eq!(parse_count(&json!("2 billion")), Some(2_000_000_000));
        assert_eq!(parse_count(&json!("1 234 comments")), Some(1234));
        assert_eq!(parse_count(&json!("9,007,199,254,740,991 likes")), Some(MAX_SAFE_COUNT));
        assert_eq!(parse_count(&json!("12 34 x!")), None);
        // A unit word, not a magnitude.
        assert_eq!(parse_count(&json!("12 km")), Some(12));
        assert_eq!(parse_count(&json!("5 m likes")), Some(5_000_000));
        // The last separator is the decimal point.
        assert_eq!(parse_count(&json!("1.234,5")), Some(1235));
        assert_eq!(parse_count(&json!("12.")), Some(12));
        // Numbers beyond what JavaScript counts exactly are capped.
        assert_eq!(parse_count(&json!(u64::MAX)), Some(MAX_SAFE_COUNT));
        assert_eq!(parse_count(&json!(1e300)), Some(MAX_SAFE_COUNT));
        assert_eq!(parse_count(&json!("999999999999999999999999")), Some(MAX_SAFE_COUNT));
    }

    #[test]
    fn counts_answer_at_once_on_hostile_input() {
        const SPACES: usize = 100_000;
        let started = std::time::Instant::now();
        assert_eq!(parse_count(&json!(format!("1{}!", " ".repeat(SPACES)))), None);
        assert_eq!(parse_count(&json!(format!("1{}!", " 1".repeat(SPACES / 2)))), None);
        assert_eq!(parse_count(&json!(format!("1{}k{}!", "\u{a0}".repeat(SPACES), " ".repeat(SPACES)))), None);
        assert_eq!(parse_count(&json!("1".repeat(SPACES))), None);
        assert_eq!(parse_count(&json!({ "count": format!("1{}!", " ".repeat(SPACES)) })), None);
        // Surrounding white space is trimmed before the length is judged.
        assert_eq!(parse_count(&json!(format!("  1 234{}", " ".repeat(SPACES)))), Some(1234));
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
    }

    // --- Small value parsers -----------------------------------------------------------------

    #[test]
    fn booleans_media_types_and_usernames() {
        for truthy in [json!(true), json!(1), json!("true"), json!("1"), json!("yes")] {
            assert_eq!(parse_boolean(&truthy), Some(true), "{truthy}");
        }
        for falsy in [json!(false), json!(0), json!("false"), json!("0"), json!("no")] {
            assert_eq!(parse_boolean(&falsy), Some(false), "{falsy}");
        }
        for neither in [json!("TRUE"), json!("Yes"), json!(2), json!(null), json!([]), json!(" true")] {
            assert_eq!(parse_boolean(&neither), None, "{neither}");
        }

        assert_eq!(parse_media_type(&json!(1)), Some(MediaType::Image));
        assert_eq!(parse_media_type(&json!(2)), Some(MediaType::Video));
        assert_eq!(parse_media_type(&json!(8)), Some(MediaType::Carousel));
        assert_eq!(parse_media_type(&json!(3)), None);
        assert_eq!(parse_media_type(&json!("GraphSidecar")), Some(MediaType::Carousel));
        assert_eq!(parse_media_type(&json!(" Carousel_Album ")), Some(MediaType::Carousel));
        assert_eq!(parse_media_type(&json!("XDTGraphVideo")), Some(MediaType::Video));
        assert_eq!(parse_media_type(&json!("photo")), Some(MediaType::Image));
        assert_eq!(parse_media_type(&json!("something else")), None);
        assert_eq!(parse_media_type(&json!(true)), None);

        assert_eq!(clean_username(&json!("  @@alice \u{7}")).as_deref(), Some("alice"));
        assert_eq!(clean_username(&json!(12345)).as_deref(), Some("12345"));
        assert_eq!(clean_username(&json!("not a handle!")).as_deref(), Some("not a handle!"));
        assert_eq!(clean_username(&json!("@")), None);
        assert_eq!(clean_username(&json!("")), None);
        assert_eq!(clean_username(&json!(["alice"])), None);
        assert_eq!(clean_username(&json!("u".repeat(500))).map(|name| name.len()), Some(MAX_USERNAME_CHARS));
    }

    #[test]
    fn numbers_read_as_text_like_javascript() {
        let text = |value: Value| js_text(&value);
        assert_eq!(text(json!(12345)).as_deref(), Some("12345"));
        assert_eq!(text(json!(-7)).as_deref(), Some("-7"));
        assert_eq!(text(json!(12345.0)).as_deref(), Some("12345"));
        assert_eq!(text(json!(1.5)).as_deref(), Some("1.5"));
        assert_eq!(text(json!(1e21)).as_deref(), Some("1e+21"));
        assert_eq!(text(json!(1.5e-7)).as_deref(), Some("1.5e-7"));
        assert_eq!(text(json!(0.0)).as_deref(), Some("0"));
        assert_eq!(text(json!(true)), None);
    }

    #[test]
    fn keys_are_normalised_and_post_prefixes_are_understood() {
        for key in ["comment_count", "commentCount", "Comment-Count", "comment count"] {
            assert_eq!(normalize_key(key), "commentcount");
        }
        assert_eq!(normalize_key("Größe_1"), "größe1");
        assert_eq!(normalize_key("__"), "");

        assert_eq!(without_post_prefix("post_image_path").as_deref(), Some("imagepath"));
        assert_eq!(without_post_prefix("post image path").as_deref(), Some("imagepath"));
        assert_eq!(without_post_prefix("postImagePath").as_deref(), Some("imagepath"));
        assert_eq!(without_post_prefix("Post-Image").as_deref(), Some("image"));
        assert_eq!(without_post_prefix("POST_ID").as_deref(), Some("id"));
        assert_eq!(without_post_prefix("post from (user)").as_deref(), Some("fromuser"));
        assert_eq!(without_post_prefix("post user's very first comment").as_deref(), Some("usersveryfirstcomment"));
        for whole in ["postcode", "poster", "postscript", "post", "post_", "post__", "POSTImage", "postID", "image"] {
            assert_eq!(without_post_prefix(whole), None, "{whole}");
        }
    }

    #[test]
    fn manifest_text_is_decoded_leniently() {
        assert_eq!(decode_manifest_text(b"[1]"), "[1]");
        assert_eq!(decode_manifest_text(b"\xEF\xBB\xBF[1]"), "[1]");
        assert_eq!(decode_manifest_text(b"\xFF\xFE[\x001\x00]\x00"), "[1]");
        assert_eq!(decode_manifest_text(b"\xFE\xFF\x00[\x001\x00]"), "[1]");
        // Invalid sequences are replaced, never fatal.
        assert_eq!(decode_manifest_text(b"a\xFFb"), "a\u{FFFD}b");
        assert_eq!(decode_manifest_text(b"\xFF\xFEa\x00\x00\xD8b"), "a\u{FFFD}\u{FFFD}");
    }

    // --- Paths -------------------------------------------------------------------------------

    #[test]
    fn relative_paths_are_normalised() {
        assert_eq!(normalize_relative_path("posts\\sub\\a.jpg").as_deref(), Ok("posts/sub/a.jpg"));
        assert_eq!(normalize_relative_path("./posts/b.jpg").as_deref(), Ok("posts/b.jpg"));
        assert_eq!(normalize_relative_path("/posts/c.jpg").as_deref(), Ok("posts/c.jpg"));
        assert_eq!(normalize_relative_path("posts/x/../d.jpg").as_deref(), Ok("posts/d.jpg"));
        assert_eq!(normalize_relative_path("  posts//e.jpg ").as_deref(), Ok("posts/e.jpg"));
        // A drive letter is no URL scheme: the scheme needs two characters.
        assert_eq!(normalize_relative_path("C:/Users/me/export/posts/a.jpg").as_deref(), Ok("C:/Users/me/export/posts/a.jpg"));
        assert_eq!(normalize_relative_path("\\\\server\\share\\a.jpg").as_deref(), Ok("server/share/a.jpg"));
        for escaping in ["../outside.jpg", "posts/../../outside.jpg", "..\\..\\windows\\system.ini"] {
            assert_eq!(normalize_relative_path(escaping), Err(PathIssue::Escape), "{escaping}");
        }
        for remote in [
            "https://cdn.example/e.jpg",
            "//cdn.example/e.jpg",
            "data:image/png;base64,AAAA",
            "FILE:///c:/a.jpg",
            "JavaScript:alert(1)",
            "blob:abc",
            "mailto:a@b.c",
            "s3+x://bucket/a.jpg",
        ] {
            assert_eq!(normalize_relative_path(remote), Err(PathIssue::Remote), "{remote}");
        }
        for empty in ["", "   ", ".", "./", "/", "a/.."] {
            assert_eq!(normalize_relative_path(empty), Err(PathIssue::Empty), "{empty:?}");
        }
    }

    #[test]
    fn entry_paths_noise_and_manifest_names() {
        assert_eq!(normalize_entry_path("./posts\\a.jpg").as_deref(), Some("posts/a.jpg"));
        assert_eq!(normalize_entry_path("posts/../a.jpg"), None);
        assert_eq!(normalize_entry_path(""), None);

        for noise in ["__MACOSX/export/post.json", "posts/._post1.jpg", ".DS_Store", "posts/Thumbs.db", "a/Desktop.ini"] {
            assert!(is_noise_path(noise), "{noise}");
        }
        for wanted in ["posts/post1.jpg", "macosx/a.jpg", "posts/_a.jpg", "__macosx/a.jpg"] {
            assert!(!is_noise_path(wanted), "{wanted}");
        }

        assert!(is_manifest_name("post.json"));
        assert!(is_manifest_name("Post.JSON"));
        assert!(is_manifest_name("POSTS.json"));
        assert!(!is_manifest_name("post.json.bak"));
        assert!(!is_manifest_name("mypost.json"));
    }

    fn index_of(relatives: &[&str]) -> FileIndex {
        let root = PathBuf::from("export-root");
        let files = relatives
            .iter()
            .map(|relative| SourceFile { relative: (*relative).to_owned(), path: root.join(relative), bytes: 1, unreadable: None })
            .collect();
        FileIndex::from_files(root, files)
    }

    fn resolved<'a>(index: &'a FileIndex, path: &str) -> Option<&'a str> {
        index.resolve(path).map(|file| file.relative.as_str())
    }

    #[test]
    fn paths_resolve_exactly_then_without_case_then_by_tail_then_by_name() {
        let index = index_of(&[
            "post.json",
            "posts/a.jpg",
            "posts/A.JPG",
            "posts/sub/deep.jpg",
            "avatars/me.png",
            "loose.jpg",
            "posts/twin.jpg",
            "other/twin.jpg",
            "data/posts.json",
        ]);
        // Exact, then the first file that folds alike.
        assert_eq!(resolved(&index, "posts/A.JPG"), Some("posts/A.JPG"));
        assert_eq!(resolved(&index, "POSTS/a.JPG"), Some("posts/a.jpg"));
        assert_eq!(resolved(&index, "Avatars/ME.PNG"), Some("avatars/me.png"));
        // Foreign leading folders: the longest tail of at least two segments.
        assert_eq!(resolved(&index, "C:/Users/me/export/posts/sub/deep.jpg"), Some("posts/sub/deep.jpg"));
        assert_eq!(resolved(&index, "export/Posts/Sub/Deep.jpg"), Some("posts/sub/deep.jpg"));
        assert_eq!(resolved(&index, "export/avatars/me.png"), Some("avatars/me.png"));
        // By name: a bare name is found at any depth when it is unique ...
        assert_eq!(resolved(&index, "deep.jpg"), Some("posts/sub/deep.jpg"));
        assert_eq!(resolved(&index, "twin.jpg"), None);
        // ... and a path with folders only by a file directly in the root.
        assert_eq!(resolved(&index, "posts/loose.jpg"), Some("loose.jpg"));
        assert_eq!(resolved(&index, "avatars/deep.jpg"), None);
        assert_eq!(resolved(&index, "avatars/a.jpg"), None);
        // The manifest is never a media file, however it is written.
        assert_eq!(resolved(&index, "post.json"), None);
        assert_eq!(resolved(&index, "Post.JSON"), None);
        assert_eq!(resolved(&index, "export/post.json"), None);
        assert_eq!(resolved(&index, "x/data/posts.json"), None);
        assert_eq!(resolved(&index, "posts.json"), None);
        assert_eq!(resolved(&index, "missing.jpg"), None);
    }

    #[test]
    fn only_the_last_eight_segments_of_a_foreign_path_are_tried() {
        let index = index_of(&["a/b/c/d/e/f/g/h.jpg", "a/b/c/d/e/f/g/h/i.jpg"]);
        assert_eq!(resolved(&index, "x/a/b/c/d/e/f/g/h.jpg"), Some("a/b/c/d/e/f/g/h.jpg"));
        // Nine segments would be needed here; the name alone is not unique enough either,
        // because a path with folders is only satisfied by a file in the root.
        assert_eq!(resolved(&index, "x/a/b/c/d/e/f/g/h/i.jpg"), None);
    }

    // --- parseManifest (manifest.test.ts:106-672) ---------------------------------------------

    #[test]
    fn parses_the_canonical_format() {
        let manifest = parse(json!({
            "format": "instagram-export",
            "version": 1,
            "exported_at": "2026-11-10T09:30:00Z",
            "source": { "type": "profile", "url": "https://www.instagram.com/natgeo/" },
            "posts": [{
                "id": "DBx1abcDEF",
                "url": "https://www.instagram.com/p/DBx1abcDEF/",
                "image": "posts/post1.jpg",
                "images": ["posts/post1.jpg", "posts/post1_2.jpg"],
                "user": {
                    "username": "natgeo",
                    "full_name": "National Geographic",
                    "verified": true,
                    "avatar": "avatars/natgeo.jpg",
                    "profile_url": "https://www.instagram.com/natgeo/"
                },
                "caption": "A caption",
                "comment_count": 1234,
                "like_count": 56789,
                "taken_at": "2026-11-01T15:04:05Z",
                "media_type": "carousel",
                "alt": "accessibility text",
                "location": "Paris, France",
                "comments": [{ "username": "someone", "text": "Wow", "like_count": 3, "taken_at": "2026-11-01T16:00:00Z" }]
            }]
        }));
        assert_eq!(manifest.warnings, vec![]);
        assert_eq!(manifest.total_entries, 1);
        assert_eq!(manifest.exported_at.and_then(text::iso_time).as_deref(), Some("2026-11-10T09:30:00Z"));
        assert_eq!(manifest.source_url.as_deref(), Some("https://www.instagram.com/natgeo/"));
        assert_eq!(
            manifest.posts,
            vec![RawPost {
                index: 0,
                id: "DBx1abcDEF".to_owned(),
                permalink: Some("https://www.instagram.com/p/DBx1abcDEF/".to_owned()),
                media: vec![plain("posts/post1.jpg"), plain("posts/post1_2.jpg")],
                user: RawUser {
                    username: "natgeo".to_owned(),
                    full_name: Some("National Geographic".to_owned()),
                    verified: Some(true),
                    avatar: Some("avatars/natgeo.jpg".to_owned()),
                    profile_url: Some("https://www.instagram.com/natgeo/".to_owned()),
                },
                caption: "A caption".to_owned(),
                comment_count: 1234,
                like_count: Some(56789),
                taken_at: Some(NOVEMBER_FIRST_MS),
                media_type: MediaType::Carousel,
                alt: Some("accessibility text".to_owned()),
                location: Some("Paris, France".to_owned()),
                comments: vec![Comment {
                    id: "DBx1abcDEF-c1".to_owned(),
                    username: "someone".to_owned(),
                    text: "Wow".to_owned(),
                    likes: 3,
                    taken_at: Some(NOVEMBER_FIRST_MS + 55 * 60_000 + 55_000),
                }],
            }]
        );
    }

    #[test]
    fn accepts_a_bare_array_of_posts() {
        let manifest = parse(json!([{ "image": "posts/a.jpg", "user": "alice" }]));
        assert_eq!(manifest.posts.len(), 1);
        let post = &manifest.posts[0];
        assert_eq!(
            post.user,
            RawUser {
                username: "alice".to_owned(),
                full_name: None,
                verified: None,
                avatar: None,
                profile_url: Some("https://www.instagram.com/alice/".to_owned()),
            }
        );
        let hash = post.id.strip_prefix("post-").expect("a derived id");
        assert!(!hash.is_empty() && hash.bytes().all(|byte| byte.is_ascii_digit() || byte.is_ascii_lowercase()));
        assert_eq!(post.caption, "");
        assert_eq!(post.comment_count, 0);
        assert_eq!(post.like_count, None);
        assert_eq!(post.media_type, MediaType::Image);
        assert_eq!(manifest.source_url, None);
        assert_eq!(manifest.exported_at, None);
    }

    #[test]
    fn reads_what_a_browser_reads_lone_surrogates_and_huge_numbers() {
        let text = r#"[
            {"user": "a", "image": "1.jpg", "caption": "cut \ud83d"},
            {"user": "a", "image": "2.jpg", "caption": "\udc00 alone, reversed \udc00\ud83d"},
            {"user": "a", "image": "3.jpg", "caption": "whole 😀 and an escaped backslash \\ud83d"},
            {"user": "a", "image": "4.jpg", "id": 1e999, "like_count": -1E+999, "caption": "1e999 in text"}
        ]"#;
        let manifest = read_manifest(text.as_bytes()).expect("a browser reads it, so do we");
        let captions: Vec<&str> = manifest.posts.iter().map(|post| post.caption.as_str()).collect();
        assert_eq!(
            captions,
            ["cut \u{fffd}", "\u{fffd} alone, reversed \u{fffd}\u{fffd}", "whole \u{1f600} and an escaped backslash \\ud83d", "1e999 in text"]
        );
        // The number that a browser reads as Infinity is no usable id or count.
        assert!(manifest.posts[3].id.starts_with("post-"), "{}", manifest.posts[3].id);
        assert_eq!(manifest.posts[3].like_count, None);
        // Nothing to repair: the text is left alone, and a real error stays an error.
        assert_eq!(repair_for_serde(r#"{"a": "😀", "b": 12.5e3}"#), None);
        assert_eq!(read_manifest(b"[{\"user\": \"a\", \"image\": \"1.jpg\"").unwrap_err().code, ManifestErrorCode::InvalidJson);
        let unfinished_escape = read_manifest(br#"[{"caption": "\ud8"}]"#).unwrap_err();
        assert!(unfinished_escape.message.starts_with("post.json is not valid JSON ("), "{}", unfinished_escape.message);
    }

    #[test]
    fn rejects_json_that_is_not_an_export() {
        assert_eq!(error_code(json!("hello")), ManifestErrorCode::InvalidManifest);
        assert_eq!(error_code(json!(42)), ManifestErrorCode::InvalidManifest);
        assert_eq!(error_code(json!(null)), ManifestErrorCode::InvalidManifest);
        assert_eq!(error_code(json!({ "format": "instagram-export" })), ManifestErrorCode::InvalidManifest);
        assert_eq!(error_code(json!({ "posts": "nope" })), ManifestErrorCode::InvalidManifest);
        assert_eq!(error_code(json!({ "posts": [] })), ManifestErrorCode::NoPosts);
        assert_eq!(error_code(json!([])), ManifestErrorCode::NoPosts);

        let wrong_shape = parse_manifest(&json!(42), MAX_POSTS).unwrap_err();
        assert_eq!(wrong_shape.message, "post.json must be a JSON object with a \"posts\" list, or a JSON list of posts.");
        let no_list = parse_manifest(&json!({ "posts": "nope" }), MAX_POSTS).unwrap_err();
        assert_eq!(no_list.message, "post.json does not contain a \"posts\" list. See instagram.md for the expected format.");
        let empty = parse_manifest(&json!([]), MAX_POSTS).unwrap_err();
        assert_eq!(empty.to_string(), "post.json does not list any posts.");
    }

    #[test]
    fn reports_invalid_json_and_oversized_manifests() {
        for broken in ["{ \"posts\": [ }", ""] {
            let error = read_manifest(broken.as_bytes()).unwrap_err();
            assert_eq!(error.code, ManifestErrorCode::InvalidJson, "{broken:?}");
            assert!(error.message.starts_with("post.json is not valid JSON ("), "{}", error.message);
            assert!(error.message.ends_with(")."), "{}", error.message);
        }
        // Other top-level lists, a BOM and UTF-16 are all read.
        for key in ["posts", "items", "entries", "data", "Posts"] {
            let text = format!("\u{feff}{{\"{key}\": [{{\"image\": \"a.jpg\", \"user\": \"a\"}}]}}");
            assert_eq!(read_manifest(text.as_bytes()).map(|result| result.posts.len()), Ok(1), "{key}");
            let mut utf16 = vec![0xFF, 0xFE];
            utf16.extend(text.trim_start_matches('\u{feff}').encode_utf16().flat_map(u16::to_le_bytes));
            assert_eq!(read_manifest(&utf16).map(|result| result.posts.len()), Ok(1), "{key} as UTF-16");
        }

        // Hostile nesting is refused by the JSON engine instead of exhausting the stack.
        let nested = "[".repeat(200_000);
        assert_eq!(read_manifest(nested.as_bytes()).unwrap_err().code, ManifestErrorCode::InvalidJson);
        let nested_entry = format!("[{{\"user\": \"a\", \"image\": {}\"a.jpg\"{}}}]", "[".repeat(100), "]".repeat(100));
        let skipped = read_manifest(nested_entry.as_bytes()).unwrap();
        assert_eq!(skipped.posts.len(), 0);
        assert_eq!(skipped.warnings[0].message, "Post 1 has no image path and was skipped.");

        let oversized = vec![b' '; MAX_MANIFEST_BYTES as usize + 1];
        let error = read_manifest(&oversized).unwrap_err();
        assert_eq!(error.code, ManifestErrorCode::TooLarge);
        assert_eq!(error.message, "post.json is larger than 32 MB.");
    }

    #[test]
    fn field_aliases_for_media() {
        for key in ["image", "image_path", "path", "file", "media", "src"] {
            assert_eq!(paths(&first(json!({ "user": "alice", key: "posts/a.jpg" }))), ["posts/a.jpg"], "{key}");
        }
        for key in ["images", "image_paths", "media_paths"] {
            let post = first(json!({ "user": "alice", key: ["posts/a.jpg", "posts/b.jpg"] }));
            assert_eq!(paths(&post), ["posts/a.jpg", "posts/b.jpg"], "{key}");
            assert_eq!(post.media_type, MediaType::Carousel, "{key}");
        }
        // As people write it.
        for key in [
            "photo",
            "picture",
            "filename",
            "image_file",
            "file_path",
            "post_image_path",
            "post image path",
            "postImagePath",
            "Post-Image",
        ] {
            assert_eq!(paths(&first(json!({ "user": "alice", key: "posts/a.jpg" }))), ["posts/a.jpg"], "{key}");
        }
        for key in ["photos", "pictures", "post_images"] {
            let post = first(json!({ "user": "alice", key: ["posts/a.jpg", "posts/b.jpg"] }));
            assert_eq!(paths(&post), ["posts/a.jpg", "posts/b.jpg"], "{key}");
        }
    }

    #[test]
    fn field_aliases_for_the_user() {
        for key in ["user", "from", "owner", "author", "username"] {
            assert_eq!(first(json!({ "image": "posts/a.jpg", key: "@alice" })).user.username, "alice", "{key}");
        }
        for key in ["user", "from", "owner", "author"] {
            let post = first(json!({ "image": "posts/a.jpg", key: { "username": "alice" } }));
            assert_eq!(post.user.username, "alice", "{key}");
        }
        for key in ["username", "name", "handle"] {
            assert_eq!(first(json!({ "image": "posts/a.jpg", "user": { key: "alice" } })).user.username, "alice", "{key}");
        }
        for key in ["full_name", "fullName", "display_name"] {
            let post = first(json!({ "image": "posts/a.jpg", "user": { "username": "alice", key: "Alice A." } }));
            assert_eq!(post.user.full_name.as_deref(), Some("Alice A."), "{key}");
        }
        for key in ["verified", "is_verified"] {
            let post = first(json!({ "image": "posts/a.jpg", "user": { "username": "alice", key: true } }));
            assert_eq!(post.user.verified, Some(true), "{key}");
        }
        for key in ["avatar", "avatar_path", "profile_pic"] {
            let post = first(json!({ "image": "posts/a.jpg", "user": { "username": "alice", key: "avatars\\alice.jpg" } }));
            assert_eq!(post.user.avatar.as_deref(), Some("avatars/alice.jpg"), "{key}");
        }
        for key in ["profile_url", "url"] {
            let post =
                first(json!({ "image": "posts/a.jpg", "user": { "username": "alice", key: "http://instagram.com/alice.real/" } }));
            assert_eq!(post.user.profile_url.as_deref(), Some("https://instagram.com/alice.real/"), "{key}");
        }
        for key in ["from_user", "posted_by", "post_from", "post from (user)", "post_user", "postOwner"] {
            let manifest = parse(json!([{ "image": "posts/a.jpg", key: "alice" }]));
            assert_eq!(manifest.posts[0].user.username, "alice", "{key}");
            assert_eq!(manifest.warnings, vec![], "{key}");
        }
        // Details next to a user given as a plain string; remote avatars are ignored silently.
        let beside = parse(json!([{
            "image": "a.jpg",
            "user": "alice",
            "full_name": "Alice A.",
            "is_verified": "no",
            "avatar": "https://cdn.example/alice.jpg",
            "profile_url": "https://www.instagram.com/alice.a/"
        }]));
        assert_eq!(beside.warnings, vec![]);
        assert_eq!(
            beside.posts[0].user,
            RawUser {
                username: "alice".to_owned(),
                full_name: Some("Alice A.".to_owned()),
                verified: Some(false),
                avatar: None,
                profile_url: Some("https://www.instagram.com/alice.a/".to_owned()),
            }
        );
    }

    #[test]
    fn field_aliases_for_texts_counts_links_and_times() {
        for key in ["caption", "description", "first_comment", "text"] {
            assert_eq!(first(with_base(json!({ key: "Hello there" }))).caption, "Hello there", "{key}");
        }
        for key in [
            "user_first_comment",
            "post_user_first_comment",
            "post user's very first comment",
            "post_description",
            "post caption",
            "postCaption",
        ] {
            assert_eq!(first(with_base(json!({ key: "Hello there" }))).caption, "Hello there", "{key}");
        }
        for key in ["comment_count", "comments_count", "comments_total", "total_comments", "commentCount"] {
            assert_eq!(first(with_base(json!({ key: "1,234" }))).comment_count, 1234, "{key}");
        }
        for key in [
            "comments_total_count",
            "total_comment_count",
            "total_comments_count",
            "comment_total",
            "comment_total_count",
            "number_of_comments",
            "num_comments",
            "post_comments_total_count",
            "post comments total count",
            "postCommentCount",
            "post_comments",
        ] {
            assert_eq!(first(with_base(json!({ key: 1234 }))).comment_count, 1234, "{key}");
        }
        for key in ["like_count", "likes", "likes_count", "likeCount"] {
            assert_eq!(first(with_base(json!({ key: "12.3K" }))).like_count, Some(12_300), "{key}");
        }
        for key in ["total_likes", "likes_total", "number_of_likes", "post_likes", "post like count"] {
            assert_eq!(first(with_base(json!({ key: 77 }))).like_count, Some(77), "{key}");
        }
        for key in ["url", "permalink", "link", "post_url"] {
            let post = first(with_base(json!({ key: "https://www.instagram.com/p/Cabc123/" })));
            assert_eq!(post.permalink.as_deref(), Some("https://www.instagram.com/p/Cabc123/"), "{key}");
            assert_eq!(post.id, "Cabc123", "{key}");
        }
        for key in ["id", "shortcode", "code"] {
            assert_eq!(first(with_base(json!({ key: "Cabc123" }))).id, "Cabc123", "{key}");
        }
        for key in ["taken_at", "timestamp", "date", "posted_at", "created_at"] {
            assert_eq!(first(with_base(json!({ key: 1_793_545_445 }))).taken_at, Some(NOVEMBER_FIRST_MS), "{key}");
        }
        for key in ["media_type", "type"] {
            assert_eq!(first(with_base(json!({ key: "video" }))).media_type, MediaType::Video, "{key}");
        }
    }

    #[test]
    fn reads_an_entry_written_with_post_in_front_of_every_field() {
        let manifest = parse(json!([{
            "post_id": "Cabc123",
            "post_image_path": "posts\\post1.jpg",
            "post_from": "alice",
            "post_comments_total_count": 12,
            "post_user_first_comment": "Hello there",
            "post_link": "https://www.instagram.com/p/Cabc123/",
            "post_date": "2026-11-01T15:04:05Z"
        }]));
        assert_eq!(manifest.warnings, vec![]);
        let post = &manifest.posts[0];
        assert_eq!(post.id, "Cabc123");
        assert_eq!(paths(post), ["posts/post1.jpg"]);
        assert_eq!(post.user.username, "alice");
        assert_eq!(post.comment_count, 12);
        assert_eq!(post.caption, "Hello there");
        assert_eq!(post.permalink.as_deref(), Some("https://www.instagram.com/p/Cabc123/"));
        assert_eq!(post.taken_at, Some(NOVEMBER_FIRST_MS));
    }

    #[test]
    fn prefers_the_plain_field_name_and_leaves_other_words_alone() {
        let caption = first(with_base(json!({ "post_caption": "second choice", "caption": "first choice" })));
        assert_eq!(caption.caption, "first choice");
        assert_eq!(first(with_base(json!({ "post_id": "second", "id": "first" }))).id, "first");
        // "postcode" and "poster" are words of their own, not "post" + "code" / "er".
        let post = first(with_base(json!({ "postcode": "90210", "poster": "bob", "postscript": "x" })));
        assert_eq!(post.id, first(with_base(json!({}))).id);
        assert_eq!(post.permalink, None);
        assert_eq!(post.user.username, "alice");
        // Of two keys that normalise alike, the first in file order wins.
        let twice: Value = serde_json::from_str(r#"[{"user": "a", "image": "1.jpg", "like_count": 1, "likeCount": 2}]"#).unwrap();
        assert_eq!(parse(twice).posts[0].like_count, Some(1));
    }

    #[test]
    fn normalises_counts() {
        let manifest = parse(json!([
            { "user": "a", "image": "p/1.jpg", "comment_count": "1,234", "like_count": "1.2M" },
            { "user": "a", "image": "p/2.jpg", "comment_count": -3, "like_count": -1 },
            { "user": "a", "image": "p/3.jpg", "comment_count": "many", "like_count": null },
            { "user": "a", "image": "p/4.jpg", "comments": [{ "username": "b", "text": "x" }, { "username": "c", "text": "y" }] },
            { "user": "a", "image": "p/5.jpg", "comments": 42 }
        ]));
        let comment_counts: Vec<u64> = manifest.posts.iter().map(|post| post.comment_count).collect();
        assert_eq!(comment_counts, [1234, 0, 0, 2, 42]);
        let like_counts: Vec<Option<u64>> = manifest.posts.iter().map(|post| post.like_count).collect();
        assert_eq!(like_counts, [Some(1_200_000), None, None, None, None]);
    }

    #[test]
    fn normalises_image_paths_and_rejects_urls_and_escapes() {
        let manifest = parse(json!([
            { "user": "a", "image": "posts\\sub\\a.jpg" },
            { "user": "a", "image": "./posts/b.jpg" },
            { "user": "a", "image": "/posts/c.jpg" },
            { "user": "a", "image": "posts/x/../d.jpg" },
            { "user": "a", "image": "../outside.jpg" },
            { "user": "a", "image": "posts/../../outside.jpg" },
            { "user": "a", "image": "..\\..\\windows\\system.ini" },
            { "user": "a", "image": "https://cdn.example/e.jpg" },
            { "user": "a", "image": "//cdn.example/e.jpg" },
            { "user": "a", "image": "data:image/png;base64,AAAA" }
        ]));
        let first_paths: Vec<&str> = manifest.posts.iter().map(|post| post.media[0].path.as_str()).collect();
        assert_eq!(first_paths, ["posts/sub/a.jpg", "posts/b.jpg", "posts/c.jpg", "posts/d.jpg"]);
        assert_eq!(
            codes(&manifest),
            [
                (WarningCode::InvalidEntry, Some(4)),
                (WarningCode::InvalidEntry, Some(5)),
                (WarningCode::InvalidEntry, Some(6)),
                (WarningCode::MissingImage, Some(7)),
                (WarningCode::MissingImage, Some(8)),
                (WarningCode::MissingImage, Some(9)),
            ]
        );
        assert_eq!(manifest.warnings[0].message, "Post 5: the image path \"../outside.jpg\" points outside the export and was ignored.");
        assert_eq!(manifest.warnings[0].path.as_deref(), Some("../outside.jpg"));
        assert_eq!(
            manifest.warnings[3].message,
            "Post 8: \"https://cdn.example/e.jpg\" is a URL. Images are not downloaded; the file must be inside the export."
        );
    }

    #[test]
    fn puts_the_cover_first_when_image_is_not_part_of_images() {
        let post = first(json!({ "user": "a", "image": "posts/cover.jpg", "images": ["posts/2.jpg", "posts/3.jpg"] }));
        assert_eq!(paths(&post), ["posts/cover.jpg", "posts/2.jpg", "posts/3.jpg"]);
        // Lists nest once, and an object may name its path.
        let nested = first(json!({ "user": "a", "images": [["posts/1.jpg", "posts/2.jpg"], { "url": "posts/3.jpg" }, [["too/deep.jpg"]]] }));
        assert_eq!(paths(&nested), ["posts/1.jpg", "posts/2.jpg", "posts/3.jpg"]);
    }

    #[test]
    fn skips_entries_without_an_image_or_that_are_not_objects() {
        let manifest =
            parse(json!([{ "user": "a" }, "posts/a.jpg", null, { "user": "a", "image": "" }, { "user": "a", "image": "p.jpg" }]));
        assert_eq!(manifest.total_entries, 5);
        assert_eq!(manifest.posts.iter().map(|post| post.index).collect::<Vec<_>>(), [4]);
        let messages: Vec<&str> = manifest.warnings.iter().map(|warning| warning.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "Post 1 has no image path and was skipped.",
                "Post 2 is not a JSON object and was skipped.",
                "Post 3 is not a JSON object and was skipped.",
                "Post 4 has no image path and was skipped.",
            ]
        );
        assert!(manifest.warnings.iter().all(|warning| warning.code == WarningCode::InvalidEntry));
    }

    #[test]
    fn derives_ids_and_makes_duplicates_unique() {
        let manifest = parse(json!([
            { "user": "a", "image": "1.jpg", "id": "same" },
            { "user": "a", "image": "2.jpg", "id": "same" },
            { "user": "a", "image": "3.jpg", "shortcode": "same" },
            { "user": "a", "image": "4.jpg", "url": "https://www.instagram.com/reel/Rabc/" },
            { "user": "a", "image": "5.jpg" },
            { "user": "a", "image": "6.jpg", "id": "post-5" },
            { "user": "a", "image": "7.jpg", "id": 12345 },
            { "user": "a", "image": "8.jpg", "id": "weird id/with <chars>" }
        ]));
        let ids: Vec<&str> = manifest.posts.iter().map(|post| post.id.as_str()).collect();
        let derived = ids[4];
        assert!(derived.starts_with("post-") && derived.len() > 5);
        assert_eq!(ids, ["same", "same-2", "same-3", "Rabc", derived, "post-5", "12345", "weird_id_with_chars_"]);
        assert_eq!(ids.iter().collect::<HashSet<_>>().len(), 8);
        let duplicates: Vec<&Warning> =
            manifest.warnings.iter().filter(|warning| warning.code == WarningCode::DuplicateId).collect();
        assert_eq!(duplicates.iter().map(|warning| warning.post_index).collect::<Vec<_>>(), [Some(1), Some(2)]);
        assert_eq!(duplicates[0].message, "Post 2 reuses the id \"same\"; it was imported as \"same-2\".");
        assert_eq!(duplicates[1].message, "Post 3 reuses the id \"same\"; it was imported as \"same-3\".");
    }

    fn dated_entry() -> Value {
        json!({ "user": "alice", "images": ["posts/a.jpg", "posts/b.jpg"], "taken_at": "2026-11-01T15:04:05Z", "caption": "x" })
    }

    /// `dated_entry` with some fields replaced.
    fn dated_with(changes: Value) -> Value {
        let mut entry = dated_entry();
        if let (Value::Object(entry), Value::Object(changes)) = (&mut entry, changes) {
            entry.extend(changes);
        }
        entry
    }

    fn id_of(entry: Value) -> String {
        first(entry).id
    }

    #[test]
    fn derived_ids_depend_on_what_the_post_is() {
        let id = id_of(dated_entry());
        // Not on where post.json lists it.
        let later = parse(json!([{ "user": "bob", "image": "posts/z.jpg" }, { "user": "carol", "image": "posts/y.jpg" }, dated_entry()]));
        assert_eq!(later.posts[2].id, id);
        // The caption and the counts may be edited between two exports of the same post.
        assert_eq!(id_of(dated_with(json!({ "caption": "edited", "like_count": 5 }))), id);
        // The same moment written differently is the same moment.
        assert_eq!(id_of(dated_with(json!({ "taken_at": 1_793_545_445 }))), id);
        // A default user of the export counts like a user on the entry.
        let mut without_user = dated_entry();
        without_user.as_object_mut().unwrap().remove("user");
        assert_eq!(parse(json!({ "user": "alice", "posts": [without_user] })).posts[0].id, id);
        // The formula of the clone (text::derived_post_id).
        let paths = ["posts/a.jpg".to_owned(), "posts/b.jpg".to_owned()];
        assert_eq!(id, text::derived_post_id("alice", Some(NOVEMBER_FIRST_MS), &paths));

        let others = [
            id_of(dated_with(json!({ "user": "bob" }))),
            id_of(dated_with(json!({ "taken_at": "2026-11-02T15:04:05Z" }))),
            id_of(dated_with(json!({ "images": ["posts/a.jpg"] }))),
            id_of(dated_with(json!({ "images": ["posts/b.jpg", "posts/a.jpg"] }))),
            id_of(dated_with(json!({ "images": ["posts/c.jpg", "posts/b.jpg"] }))),
        ];
        let mut distinct: HashSet<&str> = others.iter().map(String::as_str).collect();
        distinct.insert(&id);
        assert_eq!(distinct.len(), 6);
    }

    #[test]
    fn repeated_derived_ids_are_renamed_without_a_warning() {
        let manifest = parse(json!([dated_entry(), dated_entry(), dated_with(json!({ "id": "given" }))]));
        assert_eq!(manifest.posts[1].id, format!("{}-2", manifest.posts[0].id));
        assert_eq!(manifest.warnings, vec![]);
        // A given id that collides with a derived one is still reported.
        let taken = manifest.posts[0].id.clone();
        let clash = parse(json!([dated_entry(), { "user": "a", "image": "posts/q.jpg", "id": taken }]));
        assert_eq!(clash.posts[1].id, format!("{taken}-2"));
        assert_eq!(codes(&clash), [(WarningCode::DuplicateId, Some(1))]);
    }

    #[test]
    fn derives_the_permalink_from_an_explicit_shortcode_only() {
        let manifest =
            parse(json!([{ "user": "a", "image": "1.jpg", "shortcode": "Cabc123" }, { "user": "a", "image": "2.jpg", "id": "my-own-id" }]));
        assert_eq!(manifest.posts[0].permalink.as_deref(), Some("https://www.instagram.com/p/Cabc123/"));
        assert_eq!(manifest.posts[1].permalink, None);
    }

    #[test]
    fn drops_unsafe_links_with_a_warning() {
        let manifest = parse(json!({
            "source": { "url": "javascript:alert(1)" },
            "posts": [
                { "image": "1.jpg", "url": "javascript:alert(1)", "user": { "username": "alice", "profile_url": "ftp://example.com/alice" } },
                { "image": "2.jpg", "url": "data:text/html,x", "user": "not a handle!" }
            ]
        }));
        assert_eq!(manifest.source_url, None);
        assert_eq!(manifest.posts[0].permalink, None);
        // The profile link falls back to the one derived from the username.
        assert_eq!(manifest.posts[0].user.profile_url.as_deref(), Some("https://www.instagram.com/alice/"));
        assert_eq!(manifest.posts[1].permalink, None);
        assert_eq!(manifest.posts[1].user.username, "not a handle!");
        assert_eq!(manifest.posts[1].user.profile_url, None);
        let messages: Vec<&str> = manifest.warnings.iter().map(|warning| warning.message.as_str()).collect();
        assert_eq!(
            messages,
            [
                "The source link of the export is not a valid http(s) link and was ignored.",
                "The link of post 1 is not a valid http(s) link and was ignored.",
                "The profile link of post 1 is not a valid http(s) link and was ignored.",
                "The link of post 2 is not a valid http(s) link and was ignored.",
            ]
        );
        assert_eq!(
            codes(&manifest),
            [
                (WarningCode::UnsafeUrl, None),
                (WarningCode::UnsafeUrl, Some(0)),
                (WarningCode::UnsafeUrl, Some(0)),
                (WarningCode::UnsafeUrl, Some(1)),
            ]
        );
    }

    #[test]
    fn keeps_only_instagram_links_that_fit_their_label() {
        let shim = "https://www.instagram.com/linkshim/?u=https%3A%2F%2Fevil.example%2Fphish";
        let manifest = parse(json!({
            "source": { "url": shim },
            "posts": [
                { "image": "1.jpg", "url": shim, "user": { "username": "alice", "profile_url": "https://www.instagram.com/accounts/logout/" } },
                { "image": "2.jpg", "url": "https://www.instagram.com/accounts/logout/", "user": "alice" },
                { "image": "3.jpg", "url": "https://www.instagram.com/oauth/authorize?client_id=1", "user": "alice" },
                { "image": "4.jpg", "url": "https://www.instagram.com/alice/", "user": { "username": "alice", "url": "https://www.instagram.com/p/Cabc123/" } },
                { "image": "5.jpg", "url": "https://www.instagram.com/alice/reel/Cabc123/?igsh=1", "user": "alice" }
            ]
        }));
        assert_eq!(manifest.source_url, None);
        let permalinks: Vec<Option<&str>> = manifest.posts.iter().map(|post| post.permalink.as_deref()).collect();
        assert_eq!(permalinks, [None, None, None, None, Some("https://www.instagram.com/alice/reel/Cabc123/?igsh=1")]);
        for post in &manifest.posts {
            assert_eq!(post.user.profile_url.as_deref(), Some("https://www.instagram.com/alice/"));
        }
        assert_eq!(
            codes(&manifest),
            [
                (WarningCode::UnsafeUrl, None),
                (WarningCode::UnsafeUrl, Some(0)),
                (WarningCode::UnsafeUrl, Some(0)),
                (WarningCode::UnsafeUrl, Some(1)),
                (WarningCode::UnsafeUrl, Some(2)),
                (WarningCode::UnsafeUrl, Some(3)),
                (WarningCode::UnsafeUrl, Some(3)),
            ]
        );
        let messages: Vec<&str> = manifest.warnings.iter().map(|warning| warning.message.as_str()).collect();
        assert_eq!(
            messages[..3],
            [
                "The source link of the export is an instagram.com link, but not to an Instagram profile, post or feed, and was ignored.",
                "The link of post 1 is an instagram.com link, but not to an Instagram post, and was ignored.",
                "The profile link of post 1 is an instagram.com link, but not to an Instagram profile, and was ignored.",
            ]
        );
    }

    #[test]
    fn keeps_links_to_other_sites_and_invents_no_instagram_links_for_them() {
        let manifest = parse(json!({
            "source": { "type": "feed", "url": "https://mastodon.social/@alice" },
            "posts": [
                { "image": "1.jpg", "url": "https://mastodon.social/@alice/1123", "user": { "username": "alice", "profile_url": "https://mastodon.social/@alice" } },
                { "image": "2.jpg", "url": "https://example.com/p/abc/", "user": "bob" },
                { "image": "3.jpg", "url": "https://www.instagram.com/p/Cabc123/", "user": "carol" }
            ]
        }));
        assert_eq!(manifest.warnings, vec![]);
        assert_eq!(manifest.source_url.as_deref(), Some("https://mastodon.social/@alice"));
        let permalinks: Vec<Option<&str>> = manifest.posts.iter().map(|post| post.permalink.as_deref()).collect();
        assert_eq!(
            permalinks,
            [Some("https://mastodon.social/@alice/1123"), Some("https://example.com/p/abc/"), Some("https://www.instagram.com/p/Cabc123/")]
        );
        let profiles: Vec<Option<&str>> = manifest.posts.iter().map(|post| post.user.profile_url.as_deref()).collect();
        assert_eq!(profiles, [Some("https://mastodon.social/@alice"), None, Some("https://www.instagram.com/carol/")]);
        assert_ne!(manifest.posts[1].id, "abc");
        assert_eq!(manifest.posts[2].id, "Cabc123");
    }

    #[test]
    fn the_source_gives_the_default_author_only_for_an_instagram_profile() {
        let other_site = parse(json!({ "source": { "url": "https://example.com/alice" }, "posts": [{ "image": "1.jpg" }] }));
        assert_eq!(other_site.posts[0].user.username, "unknown");
        assert_eq!(other_site.posts[0].user.profile_url, None);

        let from_source = parse(json!({
            "source": { "type": "profile", "url": "https://www.instagram.com/natgeo/" },
            "posts": [{ "image": "1.jpg" }]
        }));
        assert_eq!(from_source.posts[0].user.username, "natgeo");
        assert_eq!(from_source.posts[0].user.profile_url.as_deref(), Some("https://www.instagram.com/natgeo/"));
        assert_eq!(from_source.warnings, vec![]);

        let unknown = parse(json!([{ "image": "1.jpg" }]));
        assert_eq!(unknown.posts[0].user.username, "unknown");
        assert_eq!(unknown.warnings.len(), 1);
        assert_eq!(unknown.warnings[0].code, WarningCode::InvalidEntry);
        assert_eq!(unknown.warnings[0].message, "Post 1 does not say who it is from; it is shown as \"unknown\".");

        // An export-level user object, with its own details.
        let top_user = parse(json!({
            "user": { "username": "@bakery", "full_name": "Bakery", "profile_url": "javascript:alert(1)" },
            "posts": [{ "image": "1.jpg" }, { "image": "2.jpg", "user": "guest" }]
        }));
        assert_eq!(top_user.posts[0].user.username, "bakery");
        assert_eq!(top_user.posts[0].user.full_name.as_deref(), Some("Bakery"));
        assert_eq!(top_user.posts[0].user.profile_url.as_deref(), Some("https://www.instagram.com/bakery/"));
        assert_eq!(top_user.posts[1].user.username, "guest");
        assert_eq!(top_user.warnings.len(), 1);
        assert_eq!(top_user.warnings[0].message, "The profile link is not a valid http(s) link and was ignored.");
        assert_eq!(top_user.warnings[0].post_index, None);
    }

    #[test]
    fn accepts_the_pages_an_export_can_come_from() {
        for url in [
            "https://www.instagram.com/",
            "https://www.instagram.com/natgeo/",
            "https://www.instagram.com/natgeo/saved/",
            "https://www.instagram.com/natgeo/tagged/",
            "https://www.instagram.com/explore/tags/sourdough/",
            "https://www.instagram.com/p/Cabc123/",
        ] {
            let manifest = parse(json!({ "source": { "url": url }, "posts": [{ "image": "1.jpg", "user": "a" }] }));
            assert_eq!(manifest.source_url.as_deref(), Some(url));
            assert_eq!(manifest.warnings, vec![], "{url}");
        }
        // The plain-string form and the "source_url" alias; a bare word is a label, not a link.
        let plain_source = parse(json!({ "source": "https://www.instagram.com/natgeo/", "posts": [{ "image": "1.jpg", "user": "a" }] }));
        assert_eq!(plain_source.source_url.as_deref(), Some("https://www.instagram.com/natgeo/"));
        let alias = parse(json!({ "source_url": "//www.instagram.com/natgeo/", "posts": [{ "image": "1.jpg", "user": "a" }] }));
        assert_eq!(alias.source_url, None);
        assert_eq!(codes(&alias), [(WarningCode::UnsafeUrl, None)]);
        let label = parse(json!({ "source": "feed", "posts": [{ "image": "1.jpg", "user": "a" }] }));
        assert_eq!(label.source_url, None);
        assert_eq!(label.warnings, vec![]);
        let number = parse(json!({ "source": { "url": 7 }, "posts": [{ "image": "1.jpg", "user": "a" }] }));
        assert_eq!(codes(&number), [(WarningCode::UnsafeUrl, None)]);
    }

    #[test]
    fn caps_text_lengths_without_splitting_a_surrogate_pair() {
        let manifest = parse(json!([{
            "user": "u".repeat(500),
            "image": "1.jpg",
            "caption": "c".repeat(MAX_CAPTION_CHARS + 500),
            "comments": [{ "username": "v".repeat(500), "text": "t".repeat(MAX_COMMENT_CHARS + 500) }]
        }]));
        let post = &manifest.posts[0];
        assert_eq!(post.user.username.len(), MAX_USERNAME_CHARS);
        assert_eq!(post.caption.len(), MAX_CAPTION_CHARS);
        assert_eq!(post.comments[0].text.len(), MAX_COMMENT_CHARS);
        assert_eq!(post.comments[0].username.len(), MAX_USERNAME_CHARS);

        let caption = format!("{}😀", "a".repeat(MAX_CAPTION_CHARS - 1));
        let cut = first(json!({ "user": "a", "image": "1.jpg", "caption": caption }));
        assert_eq!(cut.caption, "a".repeat(MAX_CAPTION_CHARS - 1));
        // Captions are trimmed with the JavaScript white space set and lose NUL characters.
        let trimmed = first(json!({ "user": "a", "image": "1.jpg", "caption": "\u{feff} two\u{0} words \n" }));
        assert_eq!(trimmed.caption, "two words");
        assert_eq!(first(json!({ "user": "a", "image": "1.jpg", "caption": 2026 })).caption, "2026");
    }

    #[test]
    fn truncates_long_manifests_and_long_carousels_with_a_warning() {
        let entries: Vec<Value> = (0..7).map(|number| json!({ "user": "a", "image": format!("posts/{number}.jpg") })).collect();
        let manifest = parse_manifest(&Value::Array(entries), 5).unwrap();
        assert_eq!(manifest.total_entries, 7);
        assert_eq!(manifest.posts.len(), 5);
        assert_eq!(manifest.warnings.len(), 1);
        assert_eq!(manifest.warnings[0].code, WarningCode::Truncated);
        assert_eq!(manifest.warnings[0].message, "post.json lists 7 posts; only the first 5 are imported.");
        assert_eq!(manifest.warnings[0].post_index, None);

        let slides: Vec<String> = (0..MAX_IMAGES_PER_POST + 3).map(|number| format!("posts/s{number}.jpg")).collect();
        let carousel = parse(json!([{ "user": "a", "images": slides }]));
        assert_eq!(carousel.posts[0].media.len(), MAX_IMAGES_PER_POST);
        assert_eq!(codes(&carousel), [(WarningCode::Truncated, Some(0))]);
        assert_eq!(carousel.warnings[0].message, "Post 1 lists 33 pictures and videos; only the first 30 are imported.");

        // Collection stops once more than MAX_RAW_MEDIA_ITEMS paths were gathered.
        let flood: Vec<String> = (0..1000).map(|number| format!("posts/f{number}.jpg")).collect();
        let flooded = parse(json!([{ "user": "a", "images": flood }]));
        assert_eq!(flooded.posts[0].media.len(), MAX_IMAGES_PER_POST);
        assert_eq!(flooded.warnings[0].message, "Post 1 lists 121 pictures and videos; only the first 30 are imported.");
    }

    #[test]
    fn understands_instagram_type_names_and_nested_objects() {
        let manifest = parse(json!([
            { "user": "a", "image": "1.jpg", "media_type": "GraphSidecar", "caption": { "text": "nested" } },
            { "user": "a", "image": "2.jpg", "type": "reel" },
            { "user": "a", "image": "3.jpg", "media_type": "something else" },
            { "user": "a", "image": "4.jpg", "location": { "name": "Tokyo" } }
        ]));
        let types: Vec<MediaType> = manifest.posts.iter().map(|post| post.media_type).collect();
        assert_eq!(types, [MediaType::Carousel, MediaType::Video, MediaType::Image, MediaType::Image]);
        assert_eq!(manifest.posts[0].caption, "nested");
        assert_eq!(manifest.posts[3].location.as_deref(), Some("Tokyo"));
    }

    #[test]
    fn comments_are_read_leniently() {
        let mut many: Vec<Value> = (0..MAX_COMMENTS_PER_POST + 5).map(|number| json!({ "user": "c", "text": format!("#{number}") })).collect();
        many.insert(0, json!("not an object"));
        many.insert(1, json!({ "username": "silent", "text": "   " }));
        many.insert(2, json!({ "from": { "handle": "@nested" }, "comment": "  Hi  ", "likes": "1.5K", "date": 1_793_545_445 }));
        many.insert(3, json!({ "body": "anonymous" }));
        let post = first(json!({ "user": "a", "image": "1.jpg", "id": "P1", "comments": many }));
        assert_eq!(post.comments.len(), MAX_COMMENTS_PER_POST);
        assert_eq!(
            post.comments[0],
            Comment { id: "P1-c1".to_owned(), username: "nested".to_owned(), text: "Hi".to_owned(), likes: 1500, taken_at: Some(NOVEMBER_FIRST_MS) }
        );
        assert_eq!(post.comments[1].username, "unknown");
        assert_eq!(post.comments[1].id, "P1-c2");
        assert_eq!(post.comments[1].likes, 0);
        assert_eq!(post.comment_count, MAX_COMMENTS_PER_POST as u64);
    }

    // --- Videos and the "media" list (manifest.test.ts:674-803) -------------------------------

    #[test]
    fn reads_the_canonical_video_post() {
        let manifest = parse(json!([{
            "id": "C5xYz",
            "url": "https://www.instagram.com/reel/C5xYz/",
            "media_type": "video",
            "media": [{ "type": "video", "path": "posts/post5.mp4", "poster": "posts/post5_poster.jpg" }],
            "user": "someone",
            "caption": "...",
            "comment_count": 12,
            "like_count": 340,
            "taken_at": "2026-10-01T12:00:00Z"
        }]));
        assert_eq!(manifest.warnings, vec![]);
        let post = &manifest.posts[0];
        assert_eq!(post.id, "C5xYz");
        assert_eq!(post.permalink.as_deref(), Some("https://www.instagram.com/reel/C5xYz/"));
        assert_eq!(post.media_type, MediaType::Video);
        assert_eq!(post.media, [video("posts/post5.mp4", Some("posts/post5_poster.jpg"))]);
    }

    #[test]
    fn media_lists_accept_strings_and_objects_with_aliases() {
        let manifest = parse(json!([{
            "user": "alice",
            "media": [
                "posts/1.jpg",
                { "path": "posts/2.jpg", "alt": "second" },
                { "file": "posts\\3.mp4", "type": "video", "thumbnail": "./posts/3_poster.jpg" },
                { "src": "posts/4.webm", "kind": "reel", "cover": "posts/4.jpg" },
                { "url": "posts/5.png", "media_type": "photo" },
                { "video": "posts/6.mov" },
                { "image": "posts/7.jpg", "type": "carousel" }
            ]
        }]));
        assert_eq!(manifest.warnings, vec![]);
        assert_eq!(
            manifest.posts[0].media,
            [
                plain("posts/1.jpg"),
                RawMedia { alt: Some("second".to_owned()), ..plain("posts/2.jpg") },
                video("posts/3.mp4", Some("posts/3_poster.jpg")),
                video("posts/4.webm", Some("posts/4.jpg")),
                RawMedia { declared: Some(MediaKind::Image), ..plain("posts/5.png") },
                video("posts/6.mov", None),
                plain("posts/7.jpg"),
            ]
        );
        assert_eq!(manifest.posts[0].media_type, MediaType::Carousel);
    }

    #[test]
    fn media_wins_over_the_older_fields() {
        let post = first(json!({
            "user": "alice",
            "image": "posts/cover.jpg",
            "images": ["posts/a.jpg"],
            "video": "posts/v.mp4",
            "media": ["posts/m.jpg"]
        }));
        assert_eq!(post.media, [plain("posts/m.jpg")]);
        assert_eq!(post.media_type, MediaType::Image);
        // A single object is a list of one.
        let single = first(json!({ "user": "alice", "media": { "path": "posts/v.mp4", "type": "video" } }));
        assert_eq!(single.media, [video("posts/v.mp4", None)]);
    }

    #[test]
    fn reads_video_fields_next_to_the_older_image_fields() {
        assert_eq!(first(json!({ "user": "alice", "video": "posts/v.mp4" })).media, [video("posts/v.mp4", None)]);
        assert_eq!(
            first(json!({ "user": "alice", "video_path": "posts/v.mp4", "poster": "posts/p.jpg" })).media,
            [video("posts/v.mp4", Some("posts/p.jpg"))]
        );
        let mixed = first(json!({
            "user": "alice",
            "image": "posts/cover.jpg",
            "images": ["posts/a.jpg"],
            "videos": ["posts/v1.mp4", "posts/v2.webm"],
            "thumbnail": "posts/v1.jpg"
        }));
        assert_eq!(
            mixed.media,
            [plain("posts/cover.jpg"), plain("posts/a.jpg"), video("posts/v1.mp4", Some("posts/v1.jpg")), video("posts/v2.webm", None)]
        );
        assert_eq!(mixed.media_type, MediaType::Carousel);
        // A poster without any video is simply not used.
        assert_eq!(first(json!({ "user": "alice", "image": "posts/a.jpg", "cover": "posts/c.jpg" })).media, [plain("posts/a.jpg")]);
    }

    #[test]
    fn keeps_the_post_level_type_as_declared_or_derived_from_the_number_of_files() {
        assert_eq!(first(json!({ "user": "alice", "media": ["posts/a.mp4"] })).media_type, MediaType::Image);
        assert_eq!(first(json!({ "user": "alice", "media": ["posts/a.jpg"], "media_type": "video" })).media_type, MediaType::Video);
        assert_eq!(
            first(json!({ "user": "alice", "media": ["posts/a.jpg", "posts/b.mp4"], "media_type": "video" })).media_type,
            MediaType::Video
        );
    }

    #[test]
    fn warns_about_remote_and_escaping_media_in_words_that_fit() {
        let manifest = parse(json!([{
            "user": "alice",
            "media": [
                { "type": "video", "url": "https://cdn.example/v.mp4" },
                { "type": "video", "path": "../outside.mp4" },
                "https://cdn.example/a.jpg",
                { "type": "video", "path": "posts/ok.mp4", "poster": "https://cdn.example/p.jpg" },
                { "type": "video", "path": "posts/ok2.mp4", "poster": "../../p.jpg" }
            ]
        }]));
        assert_eq!(manifest.posts[0].media, [video("posts/ok.mp4", None), video("posts/ok2.mp4", None)]);
        let messages: Vec<(WarningCode, &str)> =
            manifest.warnings.iter().map(|warning| (warning.code, warning.message.as_str())).collect();
        assert_eq!(
            messages,
            [
                (
                    WarningCode::MissingImage,
                    "Post 1: \"https://cdn.example/v.mp4\" is a URL. Videos are not downloaded; the file must be inside the export."
                ),
                (WarningCode::InvalidEntry, "Post 1: the video path \"../outside.mp4\" points outside the export and was ignored."),
                (
                    WarningCode::MissingImage,
                    "Post 1: \"https://cdn.example/a.jpg\" is a URL. Images are not downloaded; the file must be inside the export."
                ),
            ]
        );
    }

    #[test]
    fn skips_media_lists_without_a_usable_path_and_removes_duplicates() {
        let empty = parse(json!([{ "user": "a", "media": [null, 3, { "alt": "no path" }, ""] }, { "user": "a", "image": "posts/ok.jpg" }]));
        assert_eq!(empty.posts.len(), 1);
        assert_eq!(codes(&empty), [(WarningCode::InvalidEntry, Some(0))]);
        let twice = first(json!({ "user": "alice", "media": ["posts/a.jpg", { "path": "./posts/a.jpg", "type": "image" }, "posts/b.jpg"] }));
        assert_eq!(twice.media, [plain("posts/a.jpg"), plain("posts/b.jpg")]);
    }

    #[test]
    fn truncates_long_media_lists_like_long_carousels() {
        let media: Vec<Value> =
            (0..MAX_IMAGES_PER_POST + 2).map(|number| json!({ "type": "video", "path": format!("posts/v{number}.mp4") })).collect();
        let manifest = parse(json!([{ "user": "alice", "media": media }]));
        assert_eq!(manifest.posts[0].media.len(), MAX_IMAGES_PER_POST);
        assert_eq!(manifest.warnings.len(), 1);
        assert_eq!(manifest.warnings[0].code, WarningCode::Truncated);
        assert!(manifest.warnings[0].message.contains("pictures and videos"));
    }

    // --- Files on disk -----------------------------------------------------------------------

    /// A folder of its own below the system's temporary folder, removed when dropped.
    struct TempFolder(PathBuf);

    impl TempFolder {
        fn new(label: &str) -> TempFolder {
            static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let folder = std::env::temp_dir()
                .join(format!("instagram-core-manifest-{label}-{}-{unique}", std::process::id()));
            let _ = fs::remove_dir_all(&folder);
            fs::create_dir_all(&folder).expect("a temporary folder can be created");
            TempFolder(folder)
        }

        fn write(&self, relative: &str, bytes: &[u8]) -> PathBuf {
            let path = self.0.join(relative);
            fs::create_dir_all(path.parent().expect("a file has a folder")).expect("folders can be created");
            fs::write(&path, bytes).expect("a file can be written");
            path
        }
    }

    impl Drop for TempFolder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0];
    const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13];
    const MP4: &[u8] = b"\x00\x00\x00\x14ftypmp42\x20\x20\x20\x20mp42";
    const MATROSKA: &[u8] = &[0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42, 0x86, 0x81, 0x01];

    #[test]
    fn the_shallowest_manifest_is_found_and_noise_is_ignored() {
        let folder = TempFolder::new("find");
        assert_eq!(find_manifest(&folder.0, 3), Ok(None));

        folder.write("__MACOSX/post.json", b"{}");
        folder.write("._post.json", b"{}");
        folder.write("wrapped/deeper/post.json", b"[]");
        assert_eq!(find_manifest(&folder.0, 0), Ok(None));
        assert_eq!(find_manifest(&folder.0, 1), Ok(None));
        assert_eq!(find_manifest(&folder.0, 2), Ok(Some(folder.0.join("wrapped").join("deeper").join("post.json"))));

        let b_posts = folder.write("b/posts.json", b"[]");
        assert_eq!(find_manifest(&folder.0, 2), Ok(Some(b_posts.clone())));
        let b_post = folder.write("b/Post.JSON", b"[]");
        assert_eq!(find_manifest(&folder.0, 1), Ok(Some(b_post)));
        let a_posts = folder.write("a/posts.json", b"[]");
        // post.json outranks posts.json at the same depth, whatever the folder is called.
        assert_eq!(find_manifest(&folder.0, 1).map(|found| found.map(|path| path.parent().map(Path::to_path_buf))), Ok(Some(Some(folder.0.join("b")))));
        fs::remove_file(folder.0.join("b").join("Post.JSON")).unwrap();
        assert_eq!(find_manifest(&folder.0, 1), Ok(Some(a_posts)));
        assert_ne!(find_manifest(&folder.0, 1), Ok(Some(b_posts)));

        let root = folder.write("posts.json", b"[]");
        assert_eq!(find_manifest(&folder.0, 2), Ok(Some(root)));

        let missing = find_manifest(&folder.0.join("does-not-exist"), 1).unwrap_err();
        assert_eq!(missing.code, ManifestErrorCode::ReadFailed);
        assert!(missing.message.starts_with("\"does-not-exist\" could not be imported: "), "{}", missing.message);
    }

    #[test]
    fn a_folder_is_listed_recursively_in_a_stable_order() {
        let folder = TempFolder::new("scan");
        folder.write("post.json", b"[]");
        folder.write("posts/b.jpg", JPEG);
        folder.write("posts/a.jpg", JPEG);
        folder.write("posts/sub/c.png", PNG);
        folder.write("avatars/me.png", PNG);
        folder.write(".DS_Store", b"x");
        folder.write("posts/._a.jpg", b"x");
        folder.write("posts/Thumbs.db", b"x");
        folder.write("__MACOSX/posts/a.jpg", JPEG);

        let index = FileIndex::scan(&folder.0).unwrap();
        assert_eq!(index.root(), folder.0);
        let listed: Vec<&str> = index.files().iter().map(|file| file.relative.as_str()).collect();
        assert_eq!(listed, ["post.json", "avatars/me.png", "posts/a.jpg", "posts/b.jpg", "posts/sub/c.png"]);
        let a = index.resolve("Posts/A.jpg").unwrap();
        assert_eq!(a.path, folder.0.join("posts").join("a.jpg"));
        assert_eq!(a.bytes, JPEG.len() as u64);

        let missing = FileIndex::scan(&folder.0.join("nowhere")).unwrap_err();
        assert_eq!(missing.code, ManifestErrorCode::ReadFailed);
    }

    #[test]
    fn files_decide_what_a_post_is() {
        let folder = TempFolder::new("assemble");
        let manifest_path = folder.write(
            "post.json",
            json!({
                "source": { "url": "https://www.instagram.com/alice/" },
                "posts": [
                    { "id": "one", "image": "Posts\\ONE.JPG", "caption": " First ", "user": { "username": "alice", "avatar": "avatars/alice.png" } },
                    { "id": "two", "media": [{ "type": "video", "path": "posts/clip.mp4", "poster": "posts/one.jpg" }], "comments": [{ "text": "a" }], "comment_count": 0 },
                    { "id": "three", "media": ["posts/clip.mp4", "posts/missing.jpg", "posts/empty.jpg", "posts/film.mkv", "posts/notes.txt"] },
                    { "id": "four", "media": [{ "type": "video", "path": "posts/notes.txt" }] },
                    { "id": "five", "media": [{ "path": "posts/clip2.mp4", "poster": "posts/clip.mp4" }, { "path": "posts/clip3.mp4", "poster": "posts/nowhere.jpg" }, { "path": "posts/clip4.mp4", "poster": "posts/empty.jpg" }], "user": { "username": "bob", "avatar": "posts/clip.mp4" } },
                    { "id": "six", "image": "posts/one.jpg", "user": { "username": "carol", "avatar": "avatars/gone.png" } },
                    { "id": "seven", "image": "posts/one.jpg", "user": { "username": "carol", "avatar": "avatars/gone.png" } }
                ]
            })
            .to_string()
            .as_bytes(),
        );
        folder.write("posts/one.jpg", JPEG);
        folder.write("posts/clip.mp4", MP4);
        folder.write("posts/clip2.mp4", MP4);
        folder.write("posts/clip3.mp4", MP4);
        folder.write("posts/clip4.mp4", MP4);
        folder.write("posts/empty.jpg", b"");
        folder.write("posts/film.mkv", MATROSKA);
        folder.write("posts/notes.txt", b"just some notes");
        folder.write("avatars/alice.png", PNG);

        let export = load_export(&manifest_path).unwrap();
        assert_eq!(export.total_entries, 7);
        assert_eq!(export.source_url.as_deref(), Some("https://www.instagram.com/alice/"));
        let ids: Vec<&str> = export.posts.iter().map(|post| post.id.as_str()).collect();
        assert_eq!(ids, ["one", "two", "three", "five", "six", "seven"]);

        let one = &export.posts[0];
        assert_eq!(one.key, PostKey::for_export_post(&manifest_path, "one"));
        assert_eq!(one.media.len(), 1);
        assert_eq!(one.media[0].kind, MediaKind::Image);
        assert_eq!(one.media[0].path, folder.0.join("posts").join("one.jpg"));
        assert_eq!(one.media[0].manifest_path, "Posts/ONE.JPG");
        assert_eq!(one.media[0].bytes, JPEG.len() as u64);
        assert_eq!(one.caption, "First");
        assert_eq!(one.original_caption, "First");
        assert_eq!(one.user.avatar, Some(folder.0.join("avatars").join("alice.png")));
        assert_eq!(one.media_type, MediaType::Image);
        assert_eq!(one.context, None);

        // A declared video with a picture as its poster; the default user comes from the source.
        let two = &export.posts[1];
        assert_eq!(two.user.username, "alice");
        assert_eq!(two.media[0].kind, MediaKind::Video);
        assert_eq!(
            two.media[0].poster,
            Some(Poster { path: folder.0.join("posts").join("one.jpg"), manifest_path: "posts/one.jpg".to_owned() })
        );
        assert_eq!(two.media_type, MediaType::Video);
        assert_eq!(two.comment_count, 1);
        assert!(two.is_video());

        // Five files were listed, so the entry stays a carousel although only one is left.
        let three = &export.posts[2];
        assert_eq!(three.media.len(), 1);
        assert_eq!(three.media[0].kind, MediaKind::Video);
        assert_eq!(three.media_type, MediaType::Carousel);
        // An undeclared single file that turns out to be a video makes the post one.
        assert_eq!(final_media_type(MediaType::Image, &three.media), MediaType::Video);

        let five = &export.posts[3];
        assert_eq!(five.media.len(), 3);
        assert!(five.media.iter().all(|item| item.poster.is_none()));
        assert_eq!(five.media_type, MediaType::Carousel);
        assert_eq!(five.user.avatar, None);

        let warnings: Vec<(WarningCode, Option<usize>, &str)> =
            export.warnings.iter().map(|warning| (warning.code, warning.post_index, warning.message.as_str())).collect();
        assert_eq!(
            warnings,
            [
                (WarningCode::UnsupportedImage, None, "Profile picture of bob: \"posts/clip.mp4\" is a video, not a picture."),
                (WarningCode::MissingImage, None, "The profile picture \"avatars/gone.png\" of carol is not in the export."),
                (WarningCode::MissingImage, Some(2), "Post 3: the image \"posts/missing.jpg\" is not in the export."),
                (WarningCode::UnsupportedImage, Some(2), "Post 3: \"posts/empty.jpg\" is an empty file."),
                (
                    WarningCode::UnsupportedMedia,
                    Some(2),
                    "Post 3: \"posts/film.mkv\" is a Matroska (MKV) video, which is not supported. Convert it to MP4 (H.264) or WebM."
                ),
                (
                    WarningCode::UnsupportedMedia,
                    Some(2),
                    "Post 3: \"posts/notes.txt\" is not a supported video (MP4, MOV or WebM)."
                ),
                (
                    WarningCode::UnsupportedMedia,
                    Some(3),
                    "Post 4: \"posts/notes.txt\" is not a supported video (MP4, MOV or WebM)."
                ),
                (
                    WarningCode::UnsupportedImage,
                    Some(4),
                    "Post 5: the video \"posts/clip2.mp4\" is shown without its poster: \"posts/clip.mp4\" is a video, not a picture."
                ),
                (
                    WarningCode::MissingImage,
                    Some(4),
                    "Post 5: the video \"posts/clip3.mp4\" is shown without its poster: \"posts/nowhere.jpg\" is not in the export."
                ),
                (
                    WarningCode::UnsupportedImage,
                    Some(4),
                    "Post 5: the video \"posts/clip4.mp4\" is shown without its poster: \"posts/empty.jpg\" is an empty file."
                ),
            ]
        );
        assert_eq!(export.warnings[2].path.as_deref(), Some("posts/missing.jpg"));
    }

    #[test]
    fn an_export_without_a_usable_post_names_the_first_problem() {
        let folder = TempFolder::new("none");
        let manifest_path = folder.write(
            "post.json",
            json!([{ "user": "a", "image": "posts/gone.jpg" }, "nonsense", { "user": "a", "image": "posts/text.jpg" }]).to_string().as_bytes(),
        );
        folder.write("posts/text.jpg", b"not a picture");
        let error = load_export(&manifest_path).unwrap_err();
        assert_eq!(error.code, ManifestErrorCode::NoPosts);
        assert_eq!(
            error.message,
            "None of the 3 posts in post.json could be imported. First problem: Post 1: the image \"posts/gone.jpg\" is not in the export."
        );

        let single = folder.write("single/post.json", json!([{ "user": "a", "image": "https://cdn.example/a.jpg" }]).to_string().as_bytes());
        let error = load_export(&single).unwrap_err();
        assert_eq!(
            error.message,
            "The only post in post.json could not be imported. First problem: Post 1: \"https://cdn.example/a.jpg\" is a URL. Images are not downloaded; the file must be inside the export."
        );

        let unreadable = load_export(&folder.0.join("absent").join("post.json")).unwrap_err();
        assert_eq!(unreadable.code, ManifestErrorCode::ReadFailed);
        assert!(unreadable.message.starts_with("\"absent\" could not be imported: "), "{}", unreadable.message);
    }
}
