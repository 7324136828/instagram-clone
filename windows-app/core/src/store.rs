//! The two JSON files the app writes in its data folder, and the atomic write they share with
//! settings.json.
//!
//! ```text
//! descriptions.json   the descriptions the user typed, keyed by model::PostKey
//! state.json          interface state: theme, auto view settings, last folder
//! ```
//!
//! Descriptions are the only thing the user creates in this app, and they exist nowhere else:
//! the pictures' folder is never written to and an opened export is never modified (design
//! brief, "Edit description"). So descriptions.json is written atomically (a temporary file in
//! the same folder, flushed, then moved over the target, with retries while a virus scanner
//! holds the target; the pattern of the clone's backend/app/library.py and backend/settings.py),
//! and a damaged file is never overwritten silently: it is set aside under a new name first.
//!
//! state.json is the opposite: it only holds conveniences, so a missing or damaged file means
//! the defaults, silently (as the clone treats its localStorage keys).
//!
//! descriptions.json:
//!
//! ```json
//! {
//!   "format": "instagram-windows-app-descriptions",
//!   "version": 1,
//!   "descriptions": {
//!     "file|c:\\users\\me\\pictures\\img_2.jpg": {
//!       "caption": "Low tide again",
//!       "path": "C:\\Users\\Me\\Pictures\\IMG_2.jpg",
//!       "file_bytes": 26284,
//!       "file_modified": "2026-11-01T15:04:05Z",
//!       "edited_at": "2026-11-12T08:15:30.417Z"
//!     },
//!     "export|c:\\users\\me\\trip\\post.json|FXTbasic001": {
//!       "caption": "A new description",
//!       "path": "C:\\Users\\Me\\trip\\post.json",
//!       "post_id": "FXTbasic001",
//!       "file_bytes": 26284,
//!       "file_modified": "2026-11-01T15:04:05Z",
//!       "edited_at": "2026-11-12T08:15:30.417Z"
//!     },
//!     "archive|c:\\users\\me\\downloads\\export-basic.zip|FXTbasic002": {
//!       "caption": "Kept for the post of the archive",
//!       "path": "C:\\Users\\Me\\Downloads\\export-basic.zip",
//!       "post_id": "FXTbasic002",
//!       "file_bytes": 139899,
//!       "file_modified": "2026-11-10T10:34:00Z",
//!       "edited_at": "2026-11-12T08:16:02.104Z"
//!     }
//!   }
//! }
//! ```
//!
//! An entry of a post of an archive names the archive (its size and modified time are the
//! archive's): the copy of its media in the archive cache may be cleaned up and made again.
//!
//! Two-space indentation, a trailing newline, UTF-8 without BOM, non-ASCII written raw, keys of
//! "descriptions" in sorted order (so the file is stable between saves). Times are ISO 8601 UTC
//! in the form of `text::iso_time` (milliseconds only when they are not zero); `post_id`,
//! `file_bytes` and `file_modified` are left out when unknown.
//!
//! Reading is forgiving where nothing can be lost: a UTF-8 BOM is skipped, an empty file is an
//! empty store, a time may also be a number of epoch milliseconds, and a key written by hand
//! with other letter case or forward slashes is folded to the form `model::PostKey` builds (when
//! two keys then name the same post, the entry edited last wins).
//!
//! state.json: `{"theme": "light", "auto_view": {"interval_sec": 15, "wait_for_videos": true, "loop": true, "shuffle":
//! false}, "last_folder": "C:\\Users\\Me\\Pictures"}` in the same layout; `last_folder` is left
//! out when there is none.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::autoview;
use crate::manifest::MAX_CAPTION_CHARS;
use crate::messages;
use crate::model::{Post, PostKey, PostOrigin};
use crate::text::{self, CivilTime};

/// The value of "format" in descriptions.json.
pub const DESCRIPTIONS_FORMAT: &str = "instagram-windows-app-descriptions";
/// The value of "version" in descriptions.json. A file with a higher version is treated as
/// damaged (set aside, never overwritten in place).
pub const DESCRIPTIONS_VERSION: u64 = 1;

/// How often moving the temporary file over the target is tried when Windows answers "access
/// denied" or "sharing violation" (a reader such as a virus scanner holds the target).
pub const REPLACE_ATTEMPTS: u32 = 5;
/// The pause before retry `n` (1-based) is `n * REPLACE_RETRY_STEP_MS`: 50, 100, 150, 200 ms.
pub const REPLACE_RETRY_STEP_MS: u64 = 50;
/// Temporary files are named `.{target file name}-{unique}.tmp` and live beside the target.
pub const TEMP_SUFFIX: &str = ".tmp";

/// How many names are tried for a temporary file before giving up. A name is unique within this
/// process already (a counter); a clash needs another process with the same id and counter.
const TEMP_NAME_ATTEMPTS: u32 = 100;
/// How many names (`...`, `...-2`, `...-3`) are tried for a damaged file that is set aside.
const BACKUP_NAME_ATTEMPTS: u32 = 1000;

/// Windows error codes that mean "another program has this file open". The standard library
/// does not give them an `ErrorKind` of their own.
const ERROR_SHARING_VIOLATION: i32 = 32;
const ERROR_LOCK_VIOLATION: i32 = 33;

/// Raised when one of the app's own files cannot be read or written. `message` is a sentence
/// from `messages` (`file_not_updated`, `store_unreadable`, `store_damaged_not_set_aside`); the
/// editor shows it through `messages::description_not_saved`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StoreError {
    /// The sentence that says which file could not be read or written, and why.
    pub message: String,
}

impl StoreError {
    /// An error with its sentence.
    pub fn new(message: impl Into<String>) -> StoreError {
        StoreError { message: message.into() }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for StoreError {}

// --- Atomic writes ----------------------------------------------------------------------------

/// What happens when the target of an atomic write already exists.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum WriteMode {
    /// The target is replaced in one step (descriptions.json, state.json).
    Replace,
    /// The target is only created when it is absent; an existing file is left byte for byte as
    /// it is (first creation of settings.json, which the user owns).
    CreateNew,
}

/// True for the errors Windows gives while another program holds a file (it is being moved into
/// place, or a virus scanner or an indexer reads it): "access denied", "sharing violation" and
/// "lock violation". Such an operation is worth repeating a moment later.
pub(crate) fn is_file_in_use(error: &io::Error) -> bool {
    error.kind() == io::ErrorKind::PermissionDenied
        || (cfg!(windows)
            && matches!(error.raw_os_error(), Some(ERROR_SHARING_VIOLATION | ERROR_LOCK_VIOLATION)))
}

/// Runs `operation` up to `attempts` times while it fails with `is_file_in_use`, sleeping
/// `pause_ms(n)` milliseconds before retry `n` (1-based). Any other error, and the error of the
/// last attempt, is returned as it is.
pub(crate) fn retry_while_in_use<T>(
    attempts: u32,
    pause_ms: &dyn Fn(u32) -> u64,
    operation: &mut dyn FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let mut attempt = 1;
    loop {
        match operation() {
            Err(error) if attempt < attempts && is_file_in_use(&error) => {
                thread::sleep(Duration::from_millis(pause_ms(attempt)));
                attempt += 1;
            }
            result => return result,
        }
    }
}

/// The pauses of a replace: 50, 100, 150, 200 ms (library.py:776-802).
fn replace_pause_ms(retry: u32) -> u64 {
    u64::from(retry) * REPLACE_RETRY_STEP_MS
}

/// The temporary file of one atomic write. It is removed when this value is dropped, on every
/// path out of `atomic_write`, unless it was moved over the target.
struct TempFile {
    path: PathBuf,
    moved_away: bool,
}

impl Drop for TempFile {
    fn drop(&mut self) {
        if self.moved_away {
            return;
        }
        // A scanner may hold the new file for a moment; a leftover would only be clutter, so the
        // removal is retried briefly and its failure is not reported.
        let _ = retry_while_in_use(REPLACE_ATTEMPTS, &replace_pause_ms, &mut || {
            match fs::remove_file(&self.path) {
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                result => result,
            }
        });
    }
}

/// Counts the temporary files of this process, so that two writes never share a name.
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// A new name `.{file name}-{process}-{counter}-{clock}.tmp` in `folder`.
fn temp_path(folder: &Path, file_name: &std::ffi::OsStr) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let clock = SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.subsec_nanos()).unwrap_or(0);
    let mut name = OsString::from(".");
    name.push(file_name);
    name.push(format!("-{:x}-{counter:x}-{clock:08x}{TEMP_SUFFIX}", std::process::id()));
    folder.join(name)
}

/// Creates the temporary file beside the target; it never opens a file that already exists.
fn create_temp_file(folder: &Path, file_name: &std::ffi::OsStr) -> io::Result<(TempFile, fs::File)> {
    let mut attempt = 1;
    loop {
        let path = temp_path(folder, file_name);
        match OpenOptions::new().write(true).create_new(true).open(&path) {
            Ok(file) => return Ok((TempFile { path, moved_away: false }, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt < TEMP_NAME_ATTEMPTS => {
                attempt += 1;
            }
            Err(error) => return Err(error),
        }
    }
}

/// `CreateNew` on a file system without hard links (FAT, some network shares): the target is
/// created directly with "fail when it exists". A reader could see it half written for a
/// moment, but a file that is already there is still never touched.
fn create_in_place(path: &Path, bytes: &[u8]) -> io::Result<bool> {
    let mut file = match OpenOptions::new().write(true).create_new(true).open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => return Ok(false),
        Err(error) => return Err(error),
    };
    let written = file.write_all(bytes).and_then(|()| file.sync_all());
    drop(file);
    match written {
        Ok(()) => Ok(true),
        Err(error) => {
            // The file is ours (it was created a moment ago) and incomplete: it must not stay.
            let _ = fs::remove_file(path);
            Err(error)
        }
    }
}

/// Publishes `bytes` as the complete content of `path`, so that a reader sees either the old
/// file or the new one, never a partial file (library.py:776-802, settings.py:39-63):
///
/// 1. the parent folder is created when it is missing;
/// 2. the bytes are written to a new temporary file in the SAME folder
///    (`.{file name}-{unique}.tmp`), flushed and synced to disk;
/// 3. `Replace`: the temporary file is renamed over `path` (`std::fs::rename` replaces on
///    Windows), retried up to `REPLACE_ATTEMPTS` times with growing pauses while Windows answers
///    "access denied" or "sharing violation".
///    `CreateNew`: the temporary file is hard-linked to `path`, which fails when `path` exists;
///    that case is not an error (`Ok(false)`). Where the file system has no hard links, `path`
///    is created directly, again only when it is absent;
/// 4. the temporary file is removed in every case, also after a failure.
///
/// Returns `Ok(true)` when `path` now holds `bytes`, `Ok(false)` when `CreateNew` found an
/// existing file. The error of the last attempt is returned as it is; the old content of
/// `path` is then untouched.
pub fn atomic_write(path: &Path, bytes: &[u8], mode: WriteMode) -> io::Result<bool> {
    let file_name = path
        .file_name()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "the path does not name a file"))?;
    let folder = path.parent().unwrap_or_else(|| Path::new(""));
    fs::create_dir_all(folder)?;

    let (mut temp, mut file) = create_temp_file(folder, file_name)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    // The file is complete and closed before it gets its final name.
    drop(file);

    match mode {
        WriteMode::Replace => {
            retry_while_in_use(REPLACE_ATTEMPTS, &replace_pause_ms, &mut || fs::rename(&temp.path, path))?;
            temp.moved_away = true;
            Ok(true)
        }
        WriteMode::CreateNew => {
            let linked =
                retry_while_in_use(REPLACE_ATTEMPTS, &replace_pause_ms, &mut || fs::hard_link(&temp.path, path));
            match linked {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
                Err(_) => create_in_place(path, bytes),
            }
        }
    }
}

/// `atomic_write` in `Replace` mode of `value` as JSON in the app's layout:
/// `serde_json::to_string_pretty` (two-space indentation, non-ASCII raw) plus one "\n", UTF-8
/// without BOM. A value that cannot be serialised is an `io::ErrorKind::InvalidData` error.
pub fn atomic_write_json<T: Serialize + ?Sized>(path: &Path, value: &T) -> io::Result<()> {
    let mut text = serde_json::to_string_pretty(value)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    text.push('\n');
    atomic_write(path, text.as_bytes(), WriteMode::Replace).map(|_| ())
}

/// The text of a JSON file of this app for a value built in memory: serde_json's pretty printer
/// (what `{:#}` selects; the same output as `to_string_pretty`) plus one "\n". Writing a `Value`
/// into a string cannot fail.
fn json_file_text(value: &Value) -> String {
    format!("{value:#}\n")
}

/// The text without a leading byte order mark (Notepad writes one).
fn without_bom(text: &str) -> &str {
    text.strip_prefix('\u{feff}').unwrap_or(text)
}

// --- Times in descriptions.json ---------------------------------------------------------------

/// An instant as descriptions.json writes it: `text::iso_time`, that is `YYYY-MM-DDTHH:MM:SSZ`
/// with `.mmm` before the "Z" when the milliseconds are not zero. `None` when the UTC year is
/// outside 0001..=9999; the member is then left out.
fn instant_text(epoch_ms: i64) -> Option<String> {
    text::iso_time(epoch_ms)
}

/// Reads what `instant_text` writes, and nothing else. The lenient reader of post.json
/// (`text::parse_timestamp`) is not used on purpose: it refuses years before 1971, and the
/// modified time of a file may well be older.
fn parse_instant_text(text: &str) -> Option<i64> {
    const SHORT: usize = "YYYY-MM-DDTHH:MM:SSZ".len();
    const LONG: usize = "YYYY-MM-DDTHH:MM:SS.mmmZ".len();
    let bytes = text.as_bytes();
    if bytes.len() != SHORT && bytes.len() != LONG {
        return None;
    }
    let number = |from: usize, to: usize| -> Option<u32> {
        let digits = bytes.get(from..to)?;
        if !digits.iter().all(u8::is_ascii_digit) {
            return None;
        }
        Some(digits.iter().fold(0, |value, digit| value * 10 + u32::from(digit - b'0')))
    };
    let punctuation = [(4, b'-'), (7, b'-'), (10, b'T'), (13, b':'), (16, b':'), (bytes.len() - 1, b'Z')];
    if punctuation.iter().any(|&(index, expected)| bytes[index] != expected) {
        return None;
    }
    let millisecond = if bytes.len() == LONG {
        if bytes[19] != b'.' {
            return None;
        }
        number(20, 23)?
    } else {
        0
    };
    text::epoch_ms_from_civil(&CivilTime {
        year: i32::try_from(number(0, 4)?).ok()?,
        month: number(5, 7)?,
        day: number(8, 10)?,
        hour: number(11, 13)?,
        minute: number(14, 16)?,
        second: number(17, 19)?,
        millisecond,
    })
}

/// A time member of descriptions.json: the text form, or a whole number of epoch milliseconds.
fn instant_from_json(value: &Value) -> Option<i64> {
    match value {
        Value::String(text) => parse_instant_text(text),
        Value::Number(number) => number.as_i64(),
        _ => None,
    }
}

// --- Keys -------------------------------------------------------------------------------------

/// The path part of a key as `model::PostKey` folds it on Windows: backslashes, lower case.
fn fold_key_path(path: &str) -> String {
    path.replace('/', "\\").to_lowercase()
}

/// A key of descriptions.json in the form `model::PostKey` builds. On Windows the path part of
/// `file|{path}`, `export|{path}|{id}` and `archive|{path}|{id}` is compared without regard to
/// letter case and slash direction, so a key that differs only in those is folded to the same
/// text; the id of a post keeps its letter case. Elsewhere, and for a text of another shape, the
/// key is taken as it is.
fn normalized_key(key: &str) -> Cow<'_, str> {
    if !cfg!(windows) {
        return Cow::Borrowed(key);
    }
    let folded = if let Some(path) = key.strip_prefix("file|") {
        format!("file|{}", fold_key_path(path))
    } else if let Some((path, id)) = key.strip_prefix("export|").and_then(|rest| rest.rsplit_once('|')) {
        format!("export|{}|{id}", fold_key_path(path))
    } else if let Some((path, id)) = key.strip_prefix("archive|").and_then(|rest| rest.rsplit_once('|')) {
        format!("archive|{}|{id}", fold_key_path(path))
    } else {
        return Cow::Borrowed(key);
    };
    if folded == key {
        Cow::Borrowed(key)
    } else {
        Cow::Owned(folded)
    }
}

/// The path a key names, for an entry of a hand-written file that has no "path" member.
fn path_of_key(key: &str) -> PathBuf {
    if let Some(path) = key.strip_prefix("file|") {
        PathBuf::from(path)
    } else if let Some((path, _id)) = key.strip_prefix("export|").or_else(|| key.strip_prefix("archive|")).and_then(|rest| rest.rsplit_once('|')) {
        PathBuf::from(path)
    } else {
        PathBuf::new()
    }
}

// --- Descriptions -----------------------------------------------------------------------------

/// One remembered description.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescriptionEntry {
    /// The description as saved: trimmed, line breaks as "\n", at most 10,000 UTF-16 code units.
    pub caption: String,
    /// For a loose picture the picture file, for an export post the manifest (post.json), for a
    /// post of an archive the archive, as the user's path (original letter case).
    /// Informational: the key decides.
    pub path: PathBuf,
    /// The post id inside the export or archive; `None` for a loose picture.
    pub post_id: Option<String>,
    /// Size of the post's cover file when the description was saved, so that a replaced file
    /// can be noticed. Informational: a description is applied even when the file has changed.
    pub file_bytes: Option<u64>,
    /// Modified time of the cover file when the description was saved, epoch milliseconds.
    pub file_modified: Option<i64>,
    /// When the description was saved, epoch milliseconds.
    pub edited_at: i64,
}

impl DescriptionEntry {
    /// One member of "descriptions". `None` (the entry is dropped) when it is not an object with
    /// a string "caption"; every other member is optional and an unknown one is ignored.
    fn from_json(key: &str, value: &Value) -> Option<DescriptionEntry> {
        let object = value.as_object()?;
        let caption = object.get("caption")?.as_str()?;
        let path = object.get("path").and_then(Value::as_str).filter(|path| !path.is_empty());
        Some(DescriptionEntry {
            // A file edited by hand may hold more than the editor allows; the limit holds anyway.
            caption: text::truncate_utf16(caption, MAX_CAPTION_CHARS).to_owned(),
            path: path.map(PathBuf::from).unwrap_or_else(|| path_of_key(key)),
            post_id: object.get("post_id").and_then(Value::as_str).map(str::to_owned),
            file_bytes: object.get("file_bytes").and_then(Value::as_u64),
            file_modified: object.get("file_modified").and_then(instant_from_json),
            // Without a readable time the entry counts as the oldest possible edit.
            edited_at: object.get("edited_at").and_then(instant_from_json).unwrap_or(0),
        })
    }

    /// The member of "descriptions" for this entry, in the member order of the module comment.
    fn to_json(&self) -> Value {
        let mut object = Map::new();
        object.insert("caption".to_owned(), Value::from(self.caption.as_str()));
        object.insert("path".to_owned(), Value::from(self.path.to_string_lossy().into_owned()));
        if let Some(post_id) = &self.post_id {
            object.insert("post_id".to_owned(), Value::from(post_id.as_str()));
        }
        if let Some(file_bytes) = self.file_bytes {
            object.insert("file_bytes".to_owned(), Value::from(file_bytes));
        }
        if let Some(file_modified) = self.file_modified.and_then(instant_text) {
            object.insert("file_modified".to_owned(), Value::from(file_modified));
        }
        if let Some(edited_at) = instant_text(self.edited_at) {
            object.insert("edited_at".to_owned(), Value::from(edited_at));
        }
        Value::Object(object)
    }
}

/// The descriptions the user typed, in memory, bound to the file they are saved in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DescriptionStore {
    path: PathBuf,
    entries: BTreeMap<String, DescriptionEntry>,
}

/// What `DescriptionStore::load` found.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LoadedStore {
    /// The descriptions of the file; empty when there was no file or it was set aside.
    pub store: DescriptionStore,
    /// A sentence for the user when a damaged file was set aside
    /// (`messages::store_damaged_notice`); `None` normally.
    pub notice: Option<String>,
}

impl DescriptionStore {
    /// An empty store that will be saved to `path`.
    pub fn new(path: PathBuf) -> DescriptionStore {
        DescriptionStore { path, entries: BTreeMap::new() }
    }

    /// Reads descriptions.json.
    ///
    /// * The file does not exist -> an empty store, no notice (nothing is created until the
    ///   first save). An empty file (no bytes, or white space only) is the same: there is
    ///   nothing in it to keep.
    /// * The file is valid -> its entries. Unknown members of an entry are ignored; an entry
    ///   without a string "caption" is dropped.
    /// * The file exists but is damaged (not UTF-8, not JSON, not an object, another "format",
    ///   no "descriptions" object, a "version" above `DESCRIPTIONS_VERSION`) -> backup, then
    ///   reset: the file is renamed to `descriptions.damaged-{YYYYMMDD-HHMMSS}.json` beside it
    ///   (UTC time of `now_ms`; a free name is found by appending `-2`, `-3`, ...), an empty
    ///   store is returned, and `notice` tells the user where the old file is. Nothing of the
    ///   user's is ever deleted.
    /// * The file cannot be read (any I/O error but "not found"; "access denied" is retried
    ///   like a replace), or the damaged file cannot be renamed -> `Err`
    ///   (`messages::store_unreadable` / `messages::store_damaged_not_set_aside`). The caller
    ///   then works with `DescriptionStore::new(path)` but must show the error; a later save
    ///   will fail or succeed on its own merits.
    pub fn load(path: &Path, now_ms: i64) -> Result<LoadedStore, StoreError> {
        let empty = || DescriptionStore::new(path.to_path_buf());
        Ok(match read_from_disk(path, now_ms)? {
            OnDisk::Missing => LoadedStore { store: empty(), notice: None },
            OnDisk::Valid(store) => LoadedStore { store, notice: None },
            OnDisk::SetAside(backup_name) => LoadedStore {
                store: empty(),
                notice: Some(messages::store_damaged_notice(&path.display().to_string(), &backup_name)),
            },
        })
    }

    /// Parses the text of descriptions.json into a store bound to `path`. `Err` holds a short
    /// reason when the text is damaged in the sense of `load` (for the log). Pure.
    pub fn from_json_text(path: PathBuf, text: &str) -> Result<DescriptionStore, String> {
        let mut store = DescriptionStore::new(path);
        let text = without_bom(text);
        if text.trim().is_empty() {
            return Ok(store);
        }
        let document: Value =
            serde_json::from_str(text).map_err(|error| format!("it is not valid JSON ({error})"))?;
        let Value::Object(document) = document else {
            return Err("it does not hold a JSON object".to_owned());
        };
        if let Some(format) = document.get("format") {
            if format.as_str() != Some(DESCRIPTIONS_FORMAT) {
                return Err(format!("its \"format\" is not \"{DESCRIPTIONS_FORMAT}\""));
            }
        }
        match document.get("version").map(Value::as_u64) {
            None => {}
            Some(Some(version)) if version <= DESCRIPTIONS_VERSION => {}
            Some(Some(version)) => {
                return Err(format!(
                    "it has version {version}, and this app reads version {DESCRIPTIONS_VERSION}"
                ));
            }
            Some(None) => return Err("its \"version\" is not a whole number".to_owned()),
        }
        let Some(Value::Object(descriptions)) = document.get("descriptions") else {
            return Err("it has no \"descriptions\" object".to_owned());
        };
        for (key, value) in descriptions {
            if let Some(entry) = DescriptionEntry::from_json(key, value) {
                store.insert_newest(key, entry);
            }
        }
        Ok(store)
    }

    /// Takes an entry read from the file. When two keys of the file name the same post (they
    /// differ only in letter case or slash direction), the entry edited last is kept.
    fn insert_newest(&mut self, key: &str, entry: DescriptionEntry) {
        let key = normalized_key(key).into_owned();
        let is_older = self.entries.get(&key).is_some_and(|kept| kept.edited_at > entry.edited_at);
        if !is_older {
            self.entries.insert(key, entry);
        }
    }

    /// The text of descriptions.json for this store, in the layout of the module comment,
    /// ending with one "\n". Pure.
    pub fn to_json_text(&self) -> String {
        let descriptions: Map<String, Value> =
            self.entries.iter().map(|(key, entry)| (key.clone(), entry.to_json())).collect();
        let mut document = Map::new();
        document.insert("format".to_owned(), Value::from(DESCRIPTIONS_FORMAT));
        document.insert("version".to_owned(), Value::from(DESCRIPTIONS_VERSION));
        document.insert("descriptions".to_owned(), Value::Object(descriptions));
        json_file_text(&Value::Object(document))
    }

    /// Writes the store to its file with `atomic_write` (`Replace`). The data folder is created
    /// when missing. `Err` carries `messages::file_not_updated(path, os error)`; the file then
    /// still holds what it held before.
    pub fn save(&self) -> Result<(), StoreError> {
        atomic_write(&self.path, self.to_json_text().as_bytes(), WriteMode::Replace).map(|_| ()).map_err(
            |error| {
                StoreError::new(messages::file_not_updated(&self.path.display().to_string(), &error.to_string()))
            },
        )
    }

    /// The file this store is saved in.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// The remembered description of a post.
    pub fn get(&self, key: &PostKey) -> Option<&DescriptionEntry> {
        self.entries.get(&*normalized_key(key.as_str()))
    }

    /// Remembers a description (in memory; `save` writes it). Returns the entry it replaced.
    pub fn set(&mut self, key: &PostKey, entry: DescriptionEntry) -> Option<DescriptionEntry> {
        self.entries.insert(normalized_key(key.as_str()).into_owned(), entry)
    }

    /// Forgets a description (in memory; `save` writes it). Returns the entry that was removed.
    pub fn remove(&mut self, key: &PostKey) -> Option<DescriptionEntry> {
        self.entries.remove(&*normalized_key(key.as_str()))
    }

    /// Number of remembered descriptions.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True when nothing is remembered.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Every remembered description with its key, in key order.
    pub fn entries(&self) -> impl Iterator<Item = (PostKey, &DescriptionEntry)> + '_ {
        self.entries.iter().map(|(key, entry)| (PostKey::from_stored(key.clone()), entry))
    }
}

/// Reads descriptions.json again and takes its entries over when the file is valid, so that what
/// another window of the app saved is known here as well. Returns true when that changed the
/// store. A missing, unreadable or damaged file leaves both the store and the file as they are:
/// `save_description` deals with such a file, with a message, when the user saves.
pub fn refresh_from_disk(store: &mut DescriptionStore) -> bool {
    let Ok(bytes) = fs::read(store.path()) else {
        return false;
    };
    let Ok(text) = std::str::from_utf8(&bytes) else {
        return false;
    };
    match DescriptionStore::from_json_text(store.path().to_path_buf(), text) {
        Ok(fresh) if fresh != *store => {
            *store = fresh;
            true
        }
        _ => false,
    }
}

/// What `read_from_disk` found at the path of descriptions.json.
enum OnDisk {
    /// There is no such file (or not even its folder).
    Missing,
    /// The entries of a valid file; an empty file counts as valid and empty.
    Valid(DescriptionStore),
    /// The file was damaged and has been renamed to this file name beside it.
    SetAside(String),
}

/// Reads descriptions.json and sets a damaged file aside: the file-system half of
/// `DescriptionStore::load`, which documents the rules.
fn read_from_disk(path: &Path, now_ms: i64) -> Result<OnDisk, StoreError> {
    // "Not a directory": a file has the name of the folder, so the store's file cannot exist.
    let is_missing =
        |error: &io::Error| matches!(error.kind(), io::ErrorKind::NotFound | io::ErrorKind::NotADirectory);
    let shown = path.display().to_string();

    let bytes = match retry_while_in_use(REPLACE_ATTEMPTS, &replace_pause_ms, &mut || fs::read(path)) {
        Ok(bytes) => bytes,
        Err(error) if is_missing(&error) => return Ok(OnDisk::Missing),
        Err(error) => return Err(StoreError::new(messages::store_unreadable(&shown, &error.to_string()))),
    };
    let damage = match std::str::from_utf8(&bytes) {
        Ok(text) => match DescriptionStore::from_json_text(path.to_path_buf(), text) {
            Ok(store) => return Ok(OnDisk::Valid(store)),
            Err(reason) => reason,
        },
        Err(error) => format!("it is not UTF-8 text ({error})"),
    };
    log::warn!("{shown} is set aside: {damage}");

    match set_aside(path, now_ms) {
        Ok(backup_name) => Ok(OnDisk::SetAside(backup_name)),
        // Another window of the app set the file aside in the meantime: nothing is left to do.
        Err(error) if is_missing(&error) => Ok(OnDisk::Missing),
        Err(error) => Err(StoreError::new(messages::store_damaged_not_set_aside(&shown, &error.to_string()))),
    }
}

/// Renames a damaged file to `{stem}.damaged-{YYYYMMDD-HHMMSS}{.extension}` beside it and
/// returns the new file name. A name that is taken is never replaced: `-2`, `-3`, ... is
/// appended until one is free.
fn set_aside(path: &Path, now_ms: i64) -> io::Result<String> {
    let folder = path.parent().unwrap_or_else(|| Path::new(""));
    let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let extension =
        path.extension().map(|extension| format!(".{}", extension.to_string_lossy())).unwrap_or_default();
    let CivilTime { year, month, day, hour, minute, second, .. } = text::civil_from_epoch_ms(now_ms);
    let stamp = format!("{year:04}{month:02}{day:02}-{hour:02}{minute:02}{second:02}");

    for number in 1..=BACKUP_NAME_ATTEMPTS {
        let counter = if number == 1 { String::new() } else { format!("-{number}") };
        let name = format!("{stem}.damaged-{stamp}{counter}{extension}");
        let target = folder.join(&name);
        // `rename` would replace an older backup of the same second, so a taken name is skipped.
        if target.try_exists()? {
            continue;
        }
        retry_while_in_use(REPLACE_ATTEMPTS, &replace_pause_ms, &mut || fs::rename(path, &target))?;
        return Ok(name);
    }
    Err(io::Error::new(io::ErrorKind::AlreadyExists, "no free name is left for the damaged file"))
}

/// What saving a description did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SaveOutcome {
    /// The saved text equals the current description: nothing was written.
    Unchanged,
    /// The description was stored and the file written.
    Saved,
    /// The text equals the description the post was opened with (the caption in post.json, or
    /// "" for a loose picture): the stored entry was removed and the file written.
    Removed,
}

/// "Save" of the editor (design brief "Edit description"; spec-edit 3.4, 4, 13.2).
///
/// 1. `caption = text::saved_caption(field_text)`, cut to `MAX_CAPTION_CHARS` UTF-16 code units
///    (the editor already enforces the limit while typing).
/// 2. `caption == post.caption` -> `Unchanged`, nothing is written.
/// 3. `caption == post.original_caption` -> the entry of `post.key` is removed; otherwise it is
///    set to `{caption, path, post_id, file_bytes, file_modified, edited_at: now_ms}` where, by
///    `post.origin`, `path` is the picture file (loose), the manifest (export folder) or the
///    archive file (archive), `post_id` is `Some(post.id)` for a post of an export folder or an
///    archive, and size and modified time are those of the cover file on disk now (of the
///    archive file for a post of an archive; left `None` when they cannot be asked).
/// 4. `store.save()`. On failure the store is put back exactly as it was, `post` is not
///    touched, and the error is returned: the dialog stays open with the text and shows
///    `messages::description_not_saved(&error.message)`.
/// 5. On success `post.caption = caption`. The UI shows the toast `messages::DESCRIPTION_UPDATED`.
///
/// Several windows of the app share one descriptions.json, and each holds its own store in
/// memory. So that the window that saves last does not wipe out what another window saved,
/// steps 3 and 4 start from the file as it is on disk now, not from the memory of this window:
/// the file is read again, the one entry of `post.key` is changed, and the result is written
/// and becomes `store`. A file that has gone missing is written again from memory; a file that
/// has been damaged since is set aside first, as `DescriptionStore::load` does; a file that
/// cannot be read fails the save (`messages::store_unreadable`) and nothing is written.
pub fn save_description(store: &mut DescriptionStore, post: &mut Post, field_text: &str, now_ms: i64) -> Result<SaveOutcome, StoreError> {
    let caption = limit_caption(&text::saved_caption(field_text));
    save_clean_caption(store, post, caption, now_ms)
}

/// Step 1 of `save_description` after the trim: the first `MAX_CAPTION_CHARS` UTF-16 code units,
/// never half of a surrogate pair, and trimmed at the end again, because the cut can expose
/// white space and a saved description never ends with any.
fn limit_caption(saved: &str) -> String {
    text::js_trim_end(text::truncate_utf16(saved, MAX_CAPTION_CHARS)).to_owned()
}

/// Steps 2 to 5 of `save_description` for a caption that is already trimmed and within the
/// limit: the same path for the editor's Save and for an edit that arrives another way (the
/// browser pages of stage W of the web brief), so both store and remove alike.
pub fn save_clean_caption(store: &mut DescriptionStore, post: &mut Post, caption: String, now_ms: i64) -> Result<SaveOutcome, StoreError> {
    if caption == post.caption {
        return Ok(SaveOutcome::Unchanged);
    }
    // The change is made on a copy that starts from the file, and the copy replaces `store` only
    // once it is on disk. A failed save therefore leaves `store` exactly as it was: the file
    // still holds the old state, and a later save of another post must not write this change
    // after all.
    let mut next = match read_from_disk(store.path(), now_ms)? {
        OnDisk::Valid(on_disk) => on_disk,
        OnDisk::Missing | OnDisk::SetAside(_) => store.clone(),
    };
    let outcome = if caption == post.original_caption {
        next.remove(&post.key);
        SaveOutcome::Removed
    } else {
        next.set(&post.key, new_entry(post, &caption, now_ms));
        SaveOutcome::Saved
    };
    next.save()?;
    *store = next;
    post.caption = caption;
    Ok(outcome)
}

/// The entry that remembers `caption` for `post` (step 3 of `save_description`).
fn new_entry(post: &Post, caption: &str, now_ms: i64) -> DescriptionEntry {
    let cover = post.cover().map(|cover| cover.path.clone()).unwrap_or_default();
    let (path, measured, post_id) = match &post.origin {
        PostOrigin::File => (cover.clone(), cover, None),
        PostOrigin::Folder { manifest } => (manifest.clone(), cover, Some(post.id.clone())),
        // The copy in the archive cache is the app's own: the archive is what the user has.
        PostOrigin::Archive { archive } => (archive.clone(), archive.clone(), Some(post.id.clone())),
    };
    let metadata = fs::metadata(&measured).ok();
    DescriptionEntry {
        caption: caption.to_owned(),
        path,
        post_id,
        file_bytes: metadata.as_ref().map(fs::Metadata::len),
        file_modified: metadata
            .and_then(|metadata| metadata.modified().ok())
            .map(text::epoch_ms_from_system_time),
        edited_at: now_ms,
    }
}

// --- Interface state --------------------------------------------------------------------------

/// The appearance. The clone's default is light; the system setting is not consulted.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Theme {
    /// The clone's light appearance.
    #[default]
    Light,
    /// The clone's dark appearance.
    Dark,
}

impl Theme {
    /// The word state.json uses: "light" or "dark".
    pub fn as_str(self) -> &'static str {
        match self {
            Theme::Light => "light",
            Theme::Dark => "dark",
        }
    }

    /// The clone's rule for its stored value: exactly "dark" means dark, anything else light.
    pub fn from_stored(word: &str) -> Theme {
        if word == "dark" {
            Theme::Dark
        } else {
            Theme::Light
        }
    }

    /// The other appearance ("Switch appearance").
    pub fn toggled(self) -> Theme {
        match self {
            Theme::Light => Theme::Dark,
            Theme::Dark => Theme::Light,
        }
    }
}

/// What the app remembers about its interface between runs (state.json).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UiState {
    /// The appearance chosen with "Switch appearance".
    pub theme: Theme,
    /// The auto view settings used last time ("Start" saves all of them; changing the time
    /// while running saves only the interval; command-line options are never saved).
    pub auto_view: autoview::Settings,
    /// The folder of the pictures or export opened last, offered by the next Open dialog.
    pub last_folder: Option<PathBuf>,
    /// The folder the last download (export) was saved to, offered by the next Save dialog.
    pub last_export_folder: Option<PathBuf>,
}

impl UiState {
    /// Reads state.json. A missing, unreadable or damaged file, a missing member or a member
    /// of the wrong type gives the default for that part, silently: `theme` through
    /// `Theme::from_stored`, `auto_view` through `autoview::normalize_settings`, `last_folder`
    /// and `last_export_folder` only from a non-empty string.
    pub fn load(path: &Path) -> UiState {
        // A file that a scanner holds for a moment is waited for: falling back to the defaults
        // at once would make the next save forget the user's theme.
        match retry_while_in_use(REPLACE_ATTEMPTS, &replace_pause_ms, &mut || fs::read(path)) {
            Ok(bytes) => std::str::from_utf8(&bytes).map(UiState::from_json_text).unwrap_or_default(),
            Err(_) => UiState::default(),
        }
    }

    /// The state in `text` (the content of state.json), with the same tolerance as `load`. Pure.
    pub fn from_json_text(text: &str) -> UiState {
        let Ok(Value::Object(document)) = serde_json::from_str::<Value>(without_bom(text)) else {
            return UiState::default();
        };
        let folder = |member: &str| {
            document.get(member).and_then(Value::as_str).filter(|folder| !folder.trim().is_empty()).map(PathBuf::from)
        };
        UiState {
            theme: document.get("theme").and_then(Value::as_str).map(Theme::from_stored).unwrap_or_default(),
            auto_view: match document.get("auto_view") {
                Some(settings @ Value::Object(_)) => autoview::normalize_settings(settings),
                // Anything but an object means the defaults (the rule of `normalize_settings`).
                _ => autoview::Settings::default(),
            },
            last_folder: folder("last_folder"),
            last_export_folder: folder("last_export_folder"),
        }
    }

    /// The text of state.json for this state, ending with one "\n". Pure.
    pub fn to_json_text(&self) -> String {
        let mut document = Map::new();
        document.insert("theme".to_owned(), Value::from(self.theme.as_str()));
        document.insert("auto_view".to_owned(), autoview::settings_to_json(&self.auto_view));
        if let Some(folder) = &self.last_folder {
            document.insert("last_folder".to_owned(), Value::from(folder.to_string_lossy().into_owned()));
        }
        if let Some(folder) = &self.last_export_folder {
            document.insert("last_export_folder".to_owned(), Value::from(folder.to_string_lossy().into_owned()));
        }
        json_file_text(&Value::Object(document))
    }

    /// Writes state.json with `atomic_write` (`Replace`). The caller may ignore the error (not
    /// remembering a convenience is harmless) but should log it.
    pub fn save(&self, path: &Path) -> Result<(), StoreError> {
        atomic_write(path, self.to_json_text().as_bytes(), WriteMode::Replace).map(|_| ()).map_err(|error| {
            StoreError::new(messages::file_not_updated(&path.display().to_string(), &error.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaItem, MediaKind, MediaType, User};
    use std::sync::atomic::AtomicBool;
    use std::sync::Barrier;

    /// A folder of its own under the system's temporary folder, removed when the test ends.
    struct TempFolder(PathBuf);

    impl TempFolder {
        fn new(tag: &str) -> TempFolder {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let unique = format!(
                "instagram-core-store-{tag}-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            );
            let path = std::env::temp_dir().join(unique);
            let _ = fs::remove_dir_all(&path);
            fs::create_dir_all(&path).expect("a test folder can be created");
            TempFolder(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        /// The names of everything directly in the folder, sorted.
        fn names(&self) -> Vec<String> {
            names_in(&self.0)
        }
    }

    impl Drop for TempFolder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn names_in(folder: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(folder)
            .expect("the folder can be listed")
            .map(|entry| entry.expect("an entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    #[cfg(windows)]
    fn set_read_only(path: &Path, read_only: bool) {
        let mut permissions = fs::metadata(path).expect("metadata").permissions();
        permissions.set_readonly(read_only);
        fs::set_permissions(path, permissions).expect("permissions can be changed");
    }

    fn instant(year: i32, month: u32, day: u32, hour: u32, minute: u32, second: u32, millisecond: u32) -> i64 {
        text::epoch_ms_from_civil(&CivilTime { year, month, day, hour, minute, second, millisecond })
            .expect("a valid date")
    }

    /// 2026-11-12T08:15:30.417Z, the "now" of these tests.
    fn now() -> i64 {
        instant(2026, 11, 12, 8, 15, 30, 417)
    }

    fn entry(caption: &str, path: &str) -> DescriptionEntry {
        DescriptionEntry {
            caption: caption.to_owned(),
            path: PathBuf::from(path),
            post_id: None,
            file_bytes: None,
            file_modified: None,
            edited_at: now(),
        }
    }

    fn post(key: PostKey, id: &str, cover: &Path, caption: &str) -> Post {
        Post {
            key,
            id: id.to_owned(),
            permalink: None,
            media: vec![MediaItem {
                kind: MediaKind::Image,
                path: cover.to_path_buf(),
                manifest_path: cover.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default(),
                poster: None,
                alt: None,
                bytes: 0,
            }],
            user: User::named("unknown"),
            caption: caption.to_owned(),
            original_caption: caption.to_owned(),
            comment_count: 0,
            like_count: None,
            taken_at: None,
            media_type: MediaType::Image,
            alt: None,
            location: None,
            context: None,
            comments: Vec::new(),
            origin: PostOrigin::File,
        }
    }

    /// A loose picture that exists on disk, with 5 bytes of content.
    fn loose_post(folder: &Path, name: &str) -> Post {
        let picture = folder.join(name);
        fs::write(&picture, b"12345").expect("the picture can be written");
        post(PostKey::for_file(&picture), "img", &picture, "")
    }

    // --- Atomic writes ------------------------------------------------------------------------

    #[test]
    fn an_atomic_write_creates_the_folder_and_replaces_the_file() {
        let folder = TempFolder::new("replace");
        let target = folder.path().join("data").join("nested").join("state.json");

        assert!(atomic_write(&target, b"first", WriteMode::Replace).expect("the first write"));
        assert_eq!(fs::read(&target).expect("readable"), b"first");
        assert!(atomic_write(&target, b"the second, longer content", WriteMode::Replace).expect("the second write"));
        assert_eq!(fs::read(&target).expect("readable"), b"the second, longer content");
        assert!(atomic_write(&target, b"", WriteMode::Replace).expect("an empty write"));
        assert_eq!(fs::read(&target).expect("readable"), b"");
        // No temporary file is left behind.
        assert_eq!(names_in(target.parent().expect("a parent")), ["state.json"]);
    }

    #[test]
    fn create_new_never_touches_an_existing_file() {
        let folder = TempFolder::new("create-new");
        let target = folder.path().join("settings.json");

        assert!(atomic_write(&target, b"{}\n", WriteMode::CreateNew).expect("created"));
        assert_eq!(fs::read(&target).expect("readable"), b"{}\n");
        // settings.py: "a file another startup created in the meantime is never overwritten".
        let users = b"{ \"theme\": \"dark\" }\n";
        fs::write(&target, users).expect("the user's file");
        assert!(!atomic_write(&target, b"{}\n", WriteMode::CreateNew).expect("kept"));
        assert_eq!(fs::read(&target).expect("readable"), users);
        assert_eq!(folder.names(), ["settings.json"]);
    }

    #[test]
    fn simultaneous_creations_publish_exactly_one_complete_file() {
        let folder = TempFolder::new("create-race");
        let target = folder.path().join("settings.json");
        let workers = 8;
        let ready = Barrier::new(workers);
        let contents: Vec<Vec<u8>> =
            (0..workers).map(|worker| format!("{{\"worker\": {worker}}}\n").repeat(2000).into_bytes()).collect();

        let created: Vec<bool> = thread::scope(|scope| {
            let handles: Vec<_> = contents
                .iter()
                .map(|content| {
                    let (ready, target) = (&ready, &target);
                    scope.spawn(move || {
                        ready.wait();
                        atomic_write(target, content, WriteMode::CreateNew).expect("no error")
                    })
                })
                .collect();
            handles.into_iter().map(|handle| handle.join().expect("no panic")).collect()
        });

        let winners: Vec<usize> = (0..workers).filter(|&worker| created[worker]).collect();
        assert_eq!(winners.len(), 1, "exactly one start creates the file: {created:?}");
        assert_eq!(fs::read(&target).expect("readable"), contents[winners[0]]);
        assert_eq!(folder.names(), ["settings.json"]);
    }

    #[test]
    fn a_write_that_fails_reports_the_error_and_leaves_no_temporary_file() {
        let folder = TempFolder::new("write-fails");
        // The target is a folder: no file can be moved over it.
        let target = folder.path().join("descriptions.json");
        fs::create_dir(&target).expect("a folder in the way");
        fs::write(target.join("inside.txt"), b"kept").expect("content of the folder");

        assert!(atomic_write(&target, b"new", WriteMode::Replace).is_err());
        assert!(!atomic_write(&target, b"new", WriteMode::CreateNew).expect("it exists"));
        assert_eq!(folder.names(), ["descriptions.json"]);
        assert_eq!(fs::read(target.join("inside.txt")).expect("readable"), b"kept");

        // The parent is a file: the folder cannot be created.
        let blocker = folder.path().join("blocker");
        fs::write(&blocker, b"a file").expect("a file in the way");
        assert!(atomic_write(&blocker.join("state.json"), b"new", WriteMode::Replace).is_err());
        assert_eq!(fs::read(&blocker).expect("readable"), b"a file");
        // A path without a file name is refused before anything is written.
        let error = atomic_write(Path::new(""), b"new", WriteMode::Replace).expect_err("no file name");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[cfg(windows)]
    #[test]
    fn a_read_only_target_is_not_replaced() {
        let folder = TempFolder::new("read-only");
        let target = folder.path().join("descriptions.json");
        fs::write(&target, b"old").expect("the old file");
        set_read_only(&target, true);

        let error = atomic_write(&target, b"new", WriteMode::Replace).expect_err("a read-only file is not replaced");
        assert!(is_file_in_use(&error), "{error:?}");
        assert_eq!(fs::read(&target).expect("readable"), b"old");
        assert_eq!(folder.names(), ["descriptions.json"]);

        set_read_only(&target, false);
        assert!(atomic_write(&target, b"new", WriteMode::Replace).expect("writable again"));
        assert_eq!(fs::read(&target).expect("readable"), b"new");
    }

    #[cfg(windows)]
    #[test]
    fn a_target_held_by_another_program_is_retried() {
        use std::os::windows::fs::OpenOptionsExt;
        // Readers may share the file, but nobody may rename or delete it: what a scanner does.
        const FILE_SHARE_READ: u32 = 1;
        let folder = TempFolder::new("held");
        let target = folder.path().join("descriptions.json");
        fs::write(&target, b"old").expect("the old file");

        // Held for the whole write: every attempt fails, and the old file stays complete.
        let holder = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&target).expect("held");
        let error = atomic_write(&target, b"new", WriteMode::Replace).expect_err("the target is held");
        assert!(is_file_in_use(&error), "{error:?}");
        assert_eq!(fs::read(&target).expect("readable"), b"old");
        assert_eq!(folder.names(), ["descriptions.json"]);

        // Released after the first attempts: a later attempt succeeds.
        thread::scope(|scope| {
            scope.spawn(move || {
                thread::sleep(Duration::from_millis(120));
                drop(holder);
            });
            assert!(atomic_write(&target, b"new", WriteMode::Replace).expect("released in time"));
        });
        assert_eq!(fs::read(&target).expect("readable"), b"new");
        assert_eq!(folder.names(), ["descriptions.json"]);
    }

    #[test]
    fn only_file_in_use_errors_are_retried() {
        let no_pause = |_retry: u32| 0_u64;
        let denied = || io::Error::from(io::ErrorKind::PermissionDenied);

        // test_settings.py:81-95: two failures, then the read succeeds.
        let mut calls = 0;
        let result = retry_while_in_use(20, &no_pause, &mut || {
            calls += 1;
            if calls <= 2 {
                Err(denied())
            } else {
                Ok("content")
            }
        });
        assert_eq!((result.expect("the third attempt"), calls), ("content", 3));

        // test_settings.py:97-105: it stays unreadable; the last error is returned.
        let mut calls = 0;
        let result: io::Result<()> = retry_while_in_use(20, &no_pause, &mut || {
            calls += 1;
            Err(denied())
        });
        assert_eq!((result.expect_err("never readable").kind(), calls), (io::ErrorKind::PermissionDenied, 20));

        // Any other error is final at once.
        let mut calls = 0;
        let result: io::Result<()> = retry_while_in_use(20, &no_pause, &mut || {
            calls += 1;
            Err(io::Error::from(io::ErrorKind::NotFound))
        });
        assert_eq!((result.expect_err("not found").kind(), calls), (io::ErrorKind::NotFound, 1));

        assert!(is_file_in_use(&denied()));
        assert!(!is_file_in_use(&io::Error::from(io::ErrorKind::InvalidData)));
        assert_eq!(cfg!(windows), is_file_in_use(&io::Error::from_raw_os_error(ERROR_SHARING_VIOLATION)));
        assert_eq!((1..=4).map(replace_pause_ms).collect::<Vec<_>>(), [50, 100, 150, 200]);
    }

    #[test]
    fn json_files_use_two_spaces_raw_unicode_and_a_final_newline() {
        let folder = TempFolder::new("json");
        let target = folder.path().join("state.json");
        let value = serde_json::json!({"theme": "dark", "nested": {"name": "Zoë 🌊"}, "list": [1, 2]});
        atomic_write_json(&target, &value).expect("written");
        assert_eq!(
            fs::read_to_string(&target).expect("readable"),
            "{\n  \"theme\": \"dark\",\n  \"nested\": {\n    \"name\": \"Zoë 🌊\"\n  },\n  \"list\": [\n    1,\n    2\n  ]\n}\n"
        );
        // The store's own printer gives the same bytes.
        assert_eq!(json_file_text(&value), fs::read_to_string(&target).expect("readable"));

        // A value that cannot be JSON (a map whose keys are not strings) is an error, not a file.
        let other = folder.path().join("other.json");
        let unserialisable: BTreeMap<(u8, u8), u8> = BTreeMap::from([((1, 2), 3)]);
        let error = atomic_write_json(&other, &unserialisable).expect_err("not JSON");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(folder.names(), ["state.json"]);
    }

    // --- Times and keys -----------------------------------------------------------------------

    #[test]
    fn times_are_written_like_the_export_and_read_back() {
        assert_eq!(instant_text(1_793_545_445_000).as_deref(), Some("2026-11-01T15:04:05Z"));
        assert_eq!(instant_text(1_793_545_445_417).as_deref(), Some("2026-11-01T15:04:05.417Z"));
        assert_eq!(instant_text(0).as_deref(), Some("1970-01-01T00:00:00Z"));
        assert_eq!(instant_text(7).as_deref(), Some("1970-01-01T00:00:00.007Z"));
        assert_eq!(instant_text(-1).as_deref(), Some("1969-12-31T23:59:59.999Z"));
        assert_eq!(instant_text(i64::MAX), None);
        assert_eq!(instant_text(i64::MIN), None);

        for moment in [0, 7, -1, 1_793_545_445_000, 1_793_545_445_417, instant(1601, 1, 1, 0, 0, 0, 0)] {
            let text = instant_text(moment).expect("in range");
            assert_eq!(parse_instant_text(&text), Some(moment), "{text}");
        }
        assert_eq!(instant_from_json(&Value::from("2026-11-01T15:04:05Z")), Some(1_793_545_445_000));
        assert_eq!(instant_from_json(&Value::from(1_793_545_445_417_i64)), Some(1_793_545_445_417));
        for broken in [
            "",
            "2026-11-01",
            "2026-11-01T15:04:05",
            "2026-11-01 15:04:05Z",
            "2026-13-01T15:04:05Z",
            "2026-02-30T15:04:05Z",
            "2026-11-01T24:00:00Z",
            "2026-11-01T15:04:05.41Z",
            "2026-11-01T15:04:05,417Z",
            "2026-11-01T15:04:05+01:00",
            "+026-11-01T15:04:05Z",
            "２０２６-11-01T15:04:05Z",
            "yesterday at four o'clock",
        ] {
            assert_eq!(parse_instant_text(broken), None, "{broken:?}");
        }
        assert_eq!(instant_from_json(&Value::Null), None);
        assert_eq!(instant_from_json(&Value::from(1.5)), None);
    }

    #[cfg(windows)]
    #[test]
    fn keys_ignore_letter_case_and_slash_direction_of_the_path() {
        assert_eq!(normalized_key("file|C:/Users/Me/Pictures/IMG_2.jpg"), r"file|c:\users\me\pictures\img_2.jpg");
        assert_eq!(
            normalized_key("export|C:/Users/Me/Trip/Post.JSON|FXTbasic001"),
            r"export|c:\users\me\trip\post.json|FXTbasic001"
        );
        // Already in the stored form: nothing is copied.
        assert!(matches!(normalized_key(r"file|c:\users\me\a.jpg"), Cow::Borrowed(_)));
        // A text of another shape is taken as it is.
        assert_eq!(normalized_key("Something/Else"), "Something/Else");
        assert_eq!(normalized_key("export|NoId"), "export|NoId");

        let mut store = DescriptionStore::new(PathBuf::from("descriptions.json"));
        let written = PostKey::for_file(Path::new(r"C:\Users\Me\Pictures\IMG_2.jpg"));
        store.set(&written, entry("Low tide", r"C:\Users\Me\Pictures\IMG_2.jpg"));
        // The same file named with other letter case and slashes, also as a key typed by hand.
        assert!(store.get(&PostKey::for_file(Path::new("c:/users/me/pictures/img_2.JPG"))).is_some());
        assert!(store.get(&PostKey::from_stored("file|C:/USERS/ME/PICTURES/IMG_2.JPG")).is_some());
        store.set(&PostKey::from_stored("file|C:/users/me/pictures/img_2.jpg"), entry("High tide", "x"));
        assert_eq!(store.len(), 1);
        assert_eq!(store.get(&written).map(|entry| entry.caption.as_str()), Some("High tide"));
        // The kind before the first "|" is exact.
        assert!(store.remove(&PostKey::from_stored(r"FILE|C:\Users\Me\Pictures\IMG_2.jpg")).is_none());
        assert!(store.remove(&PostKey::from_stored(r"file|C:\Users\Me\Pictures\IMG_2.jpg")).is_some());
        assert!(store.is_empty());

        // The id of an export post keeps its letter case: "AbC" and "abc" are two posts.
        let manifest = Path::new(r"C:\Users\Me\Trip\post.json");
        store.set(&PostKey::for_export_post(manifest, "AbC"), entry("Upper", "x"));
        assert!(store.get(&PostKey::from_stored("export|c:/users/me/trip/POST.JSON|AbC")).is_some());
        assert!(store.get(&PostKey::for_export_post(manifest, "abc")).is_none());
    }

    #[cfg(windows)]
    #[test]
    fn keys_of_a_file_written_by_hand_are_folded_and_the_newest_entry_wins() {
        let text = r#"{
          "format": "instagram-windows-app-descriptions",
          "version": 1,
          "descriptions": {
            "file|C:/Users/Me/Pictures/IMG_2.jpg": {"caption": "older", "edited_at": "2026-11-01T15:04:05Z"},
            "file|c:\\users\\me\\pictures\\img_2.jpg": {"caption": "newest", "edited_at": "2026-11-12T08:15:30.417Z"},
            "file|C:\\USERS\\ME\\PICTURES\\IMG_2.JPG": {"caption": "oldest", "edited_at": "2020-01-01T00:00:00Z"},
            "export|C:/Users/Me/Trip/post.json|AbC": {"caption": "upper"},
            "export|c:\\users\\me\\trip\\post.json|abc": {"caption": "lower"}
          }
        }"#;
        let store = DescriptionStore::from_json_text(PathBuf::from("descriptions.json"), text).expect("valid");
        assert_eq!(store.len(), 3);
        let picture = PostKey::for_file(Path::new(r"C:\Users\Me\Pictures\IMG_2.jpg"));
        assert_eq!(store.get(&picture).map(|entry| entry.caption.as_str()), Some("newest"));
        let manifest = Path::new(r"C:\Users\Me\Trip\post.json");
        let caption_of =
            |id: &str| store.get(&PostKey::for_export_post(manifest, id)).map(|entry| entry.caption.clone());
        assert_eq!(caption_of("AbC").as_deref(), Some("upper"));
        assert_eq!(caption_of("abc").as_deref(), Some("lower"));
        assert_eq!(caption_of("ABC"), None);
        // The path of an entry without one is taken from its key.
        assert_eq!(
            store.get(&PostKey::for_export_post(manifest, "AbC")).map(|entry| entry.path.clone()),
            Some(PathBuf::from("C:/Users/Me/Trip/post.json"))
        );
    }

    // --- descriptions.json: text form ---------------------------------------------------------

    #[test]
    fn the_file_has_the_documented_layout_and_reads_back_equal() {
        let mut store = DescriptionStore::new(PathBuf::from("descriptions.json"));
        store.set(
            &PostKey::from_stored(r"file|c:\users\me\pictures\img_2.jpg"),
            DescriptionEntry {
                caption: "Low tide again 🌊\nThe \"second\" line: Zoë.".to_owned(),
                path: PathBuf::from(r"C:\Users\Me\Pictures\IMG_2.jpg"),
                post_id: None,
                file_bytes: Some(26_284),
                file_modified: Some(1_793_545_445_000),
                edited_at: now(),
            },
        );
        store.set(
            &PostKey::from_stored(r"export|c:\users\me\trip\post.json|FXTbasic001"),
            DescriptionEntry {
                caption: String::new(),
                path: PathBuf::from(r"C:\Users\Me\trip\post.json"),
                post_id: Some("FXTbasic001".to_owned()),
                file_bytes: None,
                file_modified: None,
                edited_at: instant(2026, 11, 12, 8, 15, 30, 0),
            },
        );

        let expected = r#"{
  "format": "instagram-windows-app-descriptions",
  "version": 1,
  "descriptions": {
    "export|c:\\users\\me\\trip\\post.json|FXTbasic001": {
      "caption": "",
      "path": "C:\\Users\\Me\\trip\\post.json",
      "post_id": "FXTbasic001",
      "edited_at": "2026-11-12T08:15:30Z"
    },
    "file|c:\\users\\me\\pictures\\img_2.jpg": {
      "caption": "Low tide again 🌊\nThe \"second\" line: Zoë.",
      "path": "C:\\Users\\Me\\Pictures\\IMG_2.jpg",
      "file_bytes": 26284,
      "file_modified": "2026-11-01T15:04:05Z",
      "edited_at": "2026-11-12T08:15:30.417Z"
    }
  }
}
"#;
        assert_eq!(store.to_json_text(), expected);
        let read = DescriptionStore::from_json_text(PathBuf::from("descriptions.json"), expected).expect("valid");
        assert_eq!(read, store);
        assert_eq!(read.to_json_text(), expected, "the file is stable between saves");
        // Keys come back in sorted order.
        let keys: Vec<String> = read.entries().map(|(key, _)| key.as_str().to_owned()).collect();
        assert_eq!(keys, [r"export|c:\users\me\trip\post.json|FXTbasic001", r"file|c:\users\me\pictures\img_2.jpg"]);

        let empty = DescriptionStore::new(PathBuf::from("descriptions.json"));
        assert_eq!(
            empty.to_json_text(),
            "{\n  \"format\": \"instagram-windows-app-descriptions\",\n  \"version\": 1,\n  \"descriptions\": {}\n}\n"
        );
    }

    #[test]
    fn reading_is_forgiving_where_nothing_can_be_lost() {
        let path = || PathBuf::from("descriptions.json");
        // An empty file, white space and a lone byte order mark are an empty store.
        for blank in ["", "  \r\n\t", "\u{feff}", "\u{feff}\n"] {
            let store = DescriptionStore::from_json_text(path(), blank).expect("blank is empty");
            assert!(store.is_empty(), "{blank:?}");
            assert_eq!(store.path(), Path::new("descriptions.json"));
        }

        // A byte order mark, CRLF, no "format" and no "version", unknown members everywhere.
        let text = "\u{feff}{\r\n \"written_by\": \"hand\",\r\n \"descriptions\": {\r\n\
            \"file|/pictures/a.jpg\": {\"caption\": \"kept\", \"mood\": \"calm\", \"file_bytes\": 12, \"file_modified\": 1793545445000},\r\n\
            \"file|/pictures/b.jpg\": {\"caption\": 42},\r\n\
            \"file|/pictures/c.jpg\": {\"path\": \"/pictures/c.jpg\"},\r\n\
            \"file|/pictures/d.jpg\": \"just text\",\r\n\
            \"file|/pictures/e.jpg\": null,\r\n\
            \"export|/trip/post.json|p1\": {\"caption\": \"\", \"path\": \"\", \"post_id\": 7, \"file_bytes\": -1, \"edited_at\": \"soon\"}\r\n\
            }\r\n}\r\n";
        let store = DescriptionStore::from_json_text(path(), text).expect("valid");
        assert_eq!(store.len(), 2, "entries without a string caption are dropped");
        let kept = store.get(&PostKey::from_stored("file|/pictures/a.jpg")).expect("kept");
        assert_eq!(
            kept,
            &DescriptionEntry {
                caption: "kept".to_owned(),
                path: PathBuf::from("/pictures/a.jpg"),
                post_id: None,
                file_bytes: Some(12),
                file_modified: Some(1_793_545_445_000),
                edited_at: 0,
            }
        );
        let cleared = store.get(&PostKey::from_stored("export|/trip/post.json|p1")).expect("an empty caption counts");
        assert_eq!(
            cleared,
            &DescriptionEntry {
                caption: String::new(),
                path: PathBuf::from("/trip/post.json"),
                post_id: None,
                file_bytes: None,
                file_modified: None,
                edited_at: 0,
            }
        );
        // A version that this app knows, also an older one.
        for version in ["0", "1"] {
            let text = format!("{{\"version\": {version}, \"descriptions\": {{}}}}");
            assert!(DescriptionStore::from_json_text(path(), &text).expect("known version").is_empty());
        }
    }

    #[test]
    fn damaged_text_is_refused_with_a_reason() {
        let reason = |text: &str| {
            DescriptionStore::from_json_text(PathBuf::from("descriptions.json"), text).expect_err("damaged")
        };
        assert!(reason("{broken").starts_with("it is not valid JSON ("));
        assert!(reason("{\"descriptions\": {\"a\": {\"caption\": \"cut of").starts_with("it is not valid JSON ("));
        assert!(reason("\0\0\0\0").starts_with("it is not valid JSON ("));
        for not_an_object in ["null", "[]", "\"text\"", "42", "true"] {
            assert_eq!(reason(not_an_object), "it does not hold a JSON object", "{not_an_object}");
        }
        assert_eq!(reason("{}"), "it has no \"descriptions\" object");
        assert_eq!(reason("{\"descriptions\": []}"), "it has no \"descriptions\" object");
        assert_eq!(reason("{\"descriptions\": null}"), "it has no \"descriptions\" object");
        assert_eq!(
            reason("{\"format\": \"instagram-export\", \"version\": 1, \"posts\": [], \"descriptions\": {}}"),
            "its \"format\" is not \"instagram-windows-app-descriptions\""
        );
        assert_eq!(
            reason("{\"format\": 1, \"descriptions\": {}}"),
            "its \"format\" is not \"instagram-windows-app-descriptions\""
        );
        assert_eq!(reason("{\"version\": 2, \"descriptions\": {}}"), "it has version 2, and this app reads version 1");
        for version in ["\"1\"", "1.5", "-1", "null"] {
            let text = format!("{{\"version\": {version}, \"descriptions\": {{}}}}");
            assert_eq!(reason(&text), "its \"version\" is not a whole number", "{version}");
        }
    }

    #[test]
    fn very_long_captions_survive_and_the_limit_never_splits_a_character() {
        let folder = TempFolder::new("long");
        let path = folder.path().join("descriptions.json");
        // Exactly 10,000 UTF-16 code units: 4,000 lemons (2 units each), line breaks, quotes,
        // backslashes and a tail of plain letters.
        let mut caption = "🍋".repeat(4000);
        caption.push_str(&"line \"one\"\\\n".repeat(100));
        caption.push_str(&"é".repeat(MAX_CAPTION_CHARS - text::utf16_len(&caption)));
        assert_eq!(text::utf16_len(&caption), MAX_CAPTION_CHARS);

        let key = PostKey::from_stored("file|/pictures/long.jpg");
        let mut store = DescriptionStore::new(path.clone());
        store.set(&key, entry(&caption, "/pictures/long.jpg"));
        store.save().expect("saved");
        let loaded = DescriptionStore::load(&path, now()).expect("loaded");
        assert_eq!(loaded.notice, None);
        assert_eq!(loaded.store, store);
        assert_eq!(loaded.store.get(&key).expect("kept").caption, caption);

        // What the editor hands over is cut to the limit, at a character boundary, and trimmed.
        assert_eq!(limit_caption(&caption), caption);
        let over = format!("{}🍋🍋", "a".repeat(MAX_CAPTION_CHARS - 1));
        assert_eq!(limit_caption(&over), "a".repeat(MAX_CAPTION_CHARS - 1), "half a lemon is never kept");
        let spaced = format!("{} \n\u{a0}tail", "b".repeat(MAX_CAPTION_CHARS - 3));
        assert_eq!(limit_caption(&spaced), "b".repeat(MAX_CAPTION_CHARS - 3), "the cut leaves no white space");
        assert_eq!(limit_caption(""), "");

        // A megabyte typed into the file by hand is cut the same way when it is read.
        let huge = format!("{}🍋{}", "c".repeat(MAX_CAPTION_CHARS - 1), "d".repeat(1 << 20));
        let text = format!("{{\"descriptions\": {{\"file|/pictures/huge.jpg\": {{\"caption\": \"{huge}\"}}}}}}");
        let read = DescriptionStore::from_json_text(path.clone(), &text).expect("valid");
        let cut = &read.get(&PostKey::from_stored("file|/pictures/huge.jpg")).expect("kept").caption;
        assert_eq!(cut, &"c".repeat(MAX_CAPTION_CHARS - 1));
    }

    // --- descriptions.json: files -------------------------------------------------------------

    #[test]
    fn a_missing_or_empty_file_is_an_empty_store_and_nothing_is_created() {
        let folder = TempFolder::new("missing");
        let path = folder.path().join("data").join("descriptions.json");

        // Neither the file nor its folder exists.
        let loaded = DescriptionStore::load(&path, now()).expect("missing is fine");
        assert_eq!(loaded, LoadedStore { store: DescriptionStore::new(path.clone()), notice: None });
        assert_eq!(folder.names(), Vec::<String>::new(), "nothing is created until the first save");

        // The first save creates the folder and the file.
        loaded.store.save().expect("saved");
        assert_eq!(names_in(path.parent().expect("a parent")), ["descriptions.json"]);

        // An empty file is not damaged: there is nothing in it to set aside.
        let blanks: [&[u8]; 3] = [b"", b" \r\n", b"\xEF\xBB\xBF"];
        for blank in blanks {
            fs::write(&path, blank).expect("the blank file");
            let loaded = DescriptionStore::load(&path, now()).expect("blank is fine");
            assert_eq!(loaded, LoadedStore { store: DescriptionStore::new(path.clone()), notice: None });
            assert_eq!(fs::read(&path).expect("readable"), blank, "reading never writes");
            assert_eq!(names_in(path.parent().expect("a parent")), ["descriptions.json"]);
        }
    }

    #[test]
    fn a_damaged_file_is_set_aside_and_a_new_one_is_started() {
        let damaged: [&[u8]; 9] = [
            b"{broken",
            b"null",
            b"[]",
            b"{}",
            b"{\"descriptions\": []}",
            b"{\"format\": \"instagram-windows-app-descriptions\", \"version\": 2, \"descriptions\": {\"k\": {\"caption\": \"from the future\"}}}",
            b"\xff\xfe{\x00}\x00",
            b"\0\0\0\0\0\0\0\0",
            b"{\"format\": \"instagram-windows-app-descriptions\", \"version\": 1, \"descriptions\": {\"file|a\": {\"caption\": \"cut of",
        ];
        for content in damaged {
            let folder = TempFolder::new("damaged");
            let path = folder.path().join("descriptions.json");
            fs::write(&path, content).expect("the damaged file");

            let loaded = DescriptionStore::load(&path, now()).expect("set aside");
            let backup = "descriptions.damaged-20261112-081530.json";
            assert!(loaded.store.is_empty());
            assert_eq!(loaded.store.path(), path);
            assert_eq!(
                loaded.notice.as_deref(),
                Some(messages::store_damaged_notice(&path.display().to_string(), backup).as_str())
            );
            // The user's bytes are kept under the new name, and nothing else was written.
            assert_eq!(folder.names(), [backup]);
            assert_eq!(fs::read(folder.path().join(backup)).expect("readable"), content);

            // The next start finds no file: the notice is not shown twice.
            assert_eq!(DescriptionStore::load(&path, now()).expect("missing").notice, None);
        }
    }

    #[test]
    fn a_backup_never_replaces_an_older_backup() {
        let folder = TempFolder::new("backups");
        let path = folder.path().join("descriptions.json");
        let backups = [
            "descriptions.damaged-20261112-081530.json",
            "descriptions.damaged-20261112-081530-2.json",
            "descriptions.damaged-20261112-081530-3.json",
        ];
        // Three damaged files within the same second.
        for (round, backup) in backups.iter().enumerate() {
            let content = format!("{{broken {round}");
            fs::write(&path, &content).expect("the damaged file");
            let loaded = DescriptionStore::load(&path, now()).expect("set aside");
            let notice = loaded.notice.expect("a notice");
            let ending = format!("The file was kept as \"{backup}\" and a new one was started.");
            assert!(notice.ends_with(&ending), "{notice}");
            assert_eq!(fs::read_to_string(folder.path().join(backup)).expect("readable"), content);
        }
        for (round, backup) in backups.iter().enumerate() {
            let kept = fs::read_to_string(folder.path().join(backup)).expect("readable");
            assert_eq!(kept, format!("{{broken {round}"));
        }
        // A later second gets its own name; a file without an extension keeps none.
        fs::write(&path, b"[]").expect("the damaged file");
        DescriptionStore::load(&path, now() + 1000).expect("set aside");
        assert!(folder.path().join("descriptions.damaged-20261112-081531.json").is_file());
        let bare = folder.path().join("descriptions");
        fs::write(&bare, b"[]").expect("the damaged file");
        DescriptionStore::load(&bare, 0).expect("set aside");
        assert!(folder.path().join("descriptions.damaged-19700101-000000").is_file());
    }

    #[test]
    fn a_file_that_cannot_be_read_is_reported_and_not_touched() {
        let folder = TempFolder::new("unreadable");
        // A folder where the file should be: it can be neither read nor replaced.
        let path = folder.path().join("descriptions.json");
        fs::create_dir(&path).expect("a folder in the way");

        let error = DescriptionStore::load(&path, now()).expect_err("a folder is not a file");
        let prefix = format!("The saved descriptions in \"{}\" could not be read: ", path.display());
        assert!(error.message.starts_with(&prefix), "{error}");
        assert!(path.is_dir());
        assert_eq!(folder.names(), ["descriptions.json"]);

        // The caller goes on with an empty store; a save then fails on its own merits.
        let mut store = DescriptionStore::new(path.clone());
        store.set(&PostKey::from_stored("file|/a.jpg"), entry("typed", "/a.jpg"));
        let error = store.save().expect_err("a folder cannot be replaced");
        assert!(error.message.starts_with(&format!("Could not update {}: ", path.display())), "{error}");
        assert_eq!(error.to_string(), error.message);
        assert_eq!(folder.names(), ["descriptions.json"]);
    }

    #[cfg(windows)]
    #[test]
    fn a_damaged_file_that_cannot_be_set_aside_is_reported_and_kept() {
        use std::os::windows::fs::OpenOptionsExt;
        const FILE_SHARE_READ: u32 = 1;
        let folder = TempFolder::new("not-set-aside");
        let path = folder.path().join("descriptions.json");
        fs::write(&path, b"{broken").expect("the damaged file");

        // Another program reads the file and does not allow renaming it.
        let holder = OpenOptions::new().read(true).share_mode(FILE_SHARE_READ).open(&path).expect("held");
        let error = DescriptionStore::load(&path, now()).expect_err("the file is held");
        let prefix =
            format!("The saved descriptions in \"{}\" are damaged and could not be set aside: ", path.display());
        assert!(error.message.starts_with(&prefix), "{error}");
        drop(holder);
        assert_eq!(fs::read(&path).expect("readable"), b"{broken");
        assert_eq!(folder.names(), ["descriptions.json"]);
    }

    #[test]
    fn readers_see_the_old_file_or_the_new_one_never_a_partial_one() {
        let folder = TempFolder::new("readers");
        let path = folder.path().join("descriptions.json");
        let filled = |count: usize, caption: &str| {
            let mut store = DescriptionStore::new(path.clone());
            for number in 0..count {
                let picture = format!("/pictures/img_{number}.jpg");
                store.set(&PostKey::from_stored(format!("file|{picture}")), entry(caption, &picture));
            }
            store
        };
        let small = filled(1, "short");
        let large = filled(200, &"a long caption 🌊 ".repeat(80));
        small.save().expect("the first save");

        let stop = AtomicBool::new(false);
        thread::scope(|scope| {
            let readers: Vec<_> = (0..3)
                .map(|_| {
                    scope.spawn(|| {
                        let mut reads = 0_u32;
                        while !stop.load(Ordering::Relaxed) {
                            let loaded = DescriptionStore::load(&path, now()).expect("always readable");
                            assert_eq!(loaded.notice, None, "a reader never sees a damaged file");
                            assert!(loaded.store == small || loaded.store == large, "a complete store");
                            reads += 1;
                        }
                        reads
                    })
                })
                .collect();
            let written = (0..120).try_for_each(|round| if round % 2 == 0 { large.save() } else { small.save() });
            stop.store(true, Ordering::Relaxed);
            for reader in readers {
                assert!(reader.join().expect("no reader failed") > 0);
            }
            written.expect("every save succeeds while the file is read");
        });
        assert_eq!(folder.names(), ["descriptions.json"]);
        assert_eq!(DescriptionStore::load(&path, now()).expect("loaded").store, small);
    }

    // --- Saving a description -----------------------------------------------------------------

    #[test]
    fn saving_stores_the_description_of_a_loose_picture() {
        let folder = TempFolder::new("save-loose");
        let path = folder.path().join("data").join("descriptions.json");
        let mut store = DescriptionStore::new(path.clone());
        let mut picture = loose_post(folder.path(), "IMG_2.jpg");
        let cover = picture.media[0].path.clone();

        // The text is the current one: nothing is written, not even the folder.
        assert_eq!(save_clean_caption(&mut store, &mut picture, String::new(), now()), Ok(SaveOutcome::Unchanged));
        assert!(store.is_empty());
        assert!(!path.exists() && !path.parent().expect("a parent").exists());

        let caption = "Low tide again 🌊\nThe second line.";
        assert_eq!(save_clean_caption(&mut store, &mut picture, caption.to_owned(), now()), Ok(SaveOutcome::Saved));
        assert_eq!(picture.caption, caption);
        assert!(picture.is_edited());
        let saved = store.get(&picture.key).expect("remembered").clone();
        assert_eq!(saved.caption, caption);
        assert_eq!(saved.path, cover, "the picture file, in the user's letter case");
        assert_eq!(saved.post_id, None);
        assert_eq!(saved.file_bytes, Some(5));
        let modified = fs::metadata(&cover).and_then(|metadata| metadata.modified()).expect("a modified time");
        assert_eq!(saved.file_modified, Some(text::epoch_ms_from_system_time(modified)));
        assert_eq!(saved.edited_at, now());
        // The file holds the same, and only the data folder was written to.
        assert_eq!(DescriptionStore::load(&path, now()).expect("loaded").store, store);
        assert_eq!(folder.names(), ["IMG_2.jpg", "data"]);
        assert_eq!(fs::read(&cover).expect("readable"), b"12345", "the picture is never written to");

        // The same text again changes nothing, and the file is not rewritten.
        let before = fs::read(&path).expect("readable");
        let again = save_clean_caption(&mut store, &mut picture, caption.to_owned(), now() + 5);
        assert_eq!(again, Ok(SaveOutcome::Unchanged));
        assert_eq!(fs::read(&path).expect("readable"), before);

        // Clearing the description of a loose picture removes the entry ("" is its original).
        assert_eq!(save_clean_caption(&mut store, &mut picture, String::new(), now() + 9), Ok(SaveOutcome::Removed));
        assert_eq!(picture.caption, "");
        assert!(store.is_empty());
        assert!(DescriptionStore::load(&path, now()).expect("loaded").store.is_empty());
    }

    #[test]
    fn saving_keeps_an_export_untouched_and_forgets_an_edit_that_is_undone() {
        let folder = TempFolder::new("save-export");
        let export = folder.path().join("Trip");
        fs::create_dir(&export).expect("the export folder");
        let manifest = export.join("post.json");
        fs::write(&manifest, b"{\"posts\": []}").expect("the manifest");
        let cover = export.join("post1.jpg");
        fs::write(&cover, b"1234567").expect("the cover");
        let path = folder.path().join("descriptions.json");
        let mut store = DescriptionStore::new(path.clone());
        let mut first = post(PostKey::for_export_post(&manifest, "FXTbasic001"), "FXTbasic001", &cover, "Morning light");
        let mut second = post(PostKey::for_export_post(&manifest, "fxtbasic001"), "fxtbasic001", &cover, "Another post");
        first.origin = PostOrigin::Folder { manifest: manifest.clone() };
        second.origin = PostOrigin::Folder { manifest: manifest.clone() };

        assert_eq!(
            save_clean_caption(&mut store, &mut first, "A new description".to_owned(), now()),
            Ok(SaveOutcome::Saved)
        );
        let saved = store.get(&first.key).expect("remembered");
        assert_eq!(saved.path, manifest, "an export post is found through its post.json");
        assert_eq!(saved.post_id.as_deref(), Some("FXTbasic001"));
        assert_eq!(saved.file_bytes, Some(7), "the size of the cover file");
        assert_eq!((first.caption.as_str(), first.original_caption.as_str()), ("A new description", "Morning light"));
        // Ids that differ in letter case are two posts.
        assert_eq!(store.get(&second.key), None);
        assert_eq!(
            save_clean_caption(&mut store, &mut second, String::new(), now()),
            Ok(SaveOutcome::Saved)
        );
        assert_eq!(store.len(), 2);
        assert_eq!(store.get(&second.key).map(|entry| entry.caption.as_str()), Some(""), "a cleared caption is an edit");

        // Typing the original caption again removes the entry of that post only.
        assert_eq!(
            save_clean_caption(&mut store, &mut first, "Morning light".to_owned(), now()),
            Ok(SaveOutcome::Removed)
        );
        assert!(!first.is_edited());
        assert_eq!(store.len(), 1);
        let loaded = DescriptionStore::load(&path, now()).expect("loaded").store;
        assert_eq!(loaded, store);
        assert!(loaded.get(&first.key).is_none() && loaded.get(&second.key).is_some());

        // The export folder is never modified.
        assert_eq!(names_in(&export), ["post.json", "post1.jpg"]);
        assert_eq!(fs::read(&manifest).expect("readable"), b"{\"posts\": []}");
    }

    #[test]
    fn a_post_of_an_archive_is_remembered_by_the_archive() {
        let folder = TempFolder::new("save-archive");
        let archive = folder.path().join("Export-Basic.zip");
        fs::write(&archive, b"PK\x05\x06 an archive of 26 bytes.").expect("the archive");
        let cache_copy = folder.path().join("cache").join("m000003.jpg");
        let path = folder.path().join("descriptions.json");
        let mut store = DescriptionStore::new(path.clone());
        let mut first = post(PostKey::for_archive_post(&archive, "FXTbasic001"), "FXTbasic001", &cache_copy, "Morning light");
        first.origin = PostOrigin::Archive { archive: archive.clone() };

        assert_eq!(save_clean_caption(&mut store, &mut first, "A new description".to_owned(), now()), Ok(SaveOutcome::Saved));
        let saved = store.get(&first.key).expect("remembered").clone();
        assert_eq!(saved.path, archive, "the entry names the archive, not the copy in the cache");
        assert_eq!(saved.post_id.as_deref(), Some("FXTbasic001"));
        assert_eq!(saved.file_bytes, fs::metadata(&archive).ok().map(|metadata| metadata.len()), "the size of the archive");
        let text = fs::read_to_string(&path).expect("written");
        assert!(text.contains("\"archive|"), "{text}");
        // Read back in another letter case of the path, the key is the same post.
        let loaded = DescriptionStore::load(&path, now()).expect("loaded").store;
        let shouted = PathBuf::from(archive.to_string_lossy().to_uppercase());
        #[cfg(windows)]
        assert_eq!(loaded.get(&PostKey::for_archive_post(&shouted, "FXTbasic001")), Some(&saved));
        let _ = shouted;
        // Typing the original caption again removes the entry.
        assert_eq!(save_clean_caption(&mut store, &mut first, "Morning light".to_owned(), now()), Ok(SaveOutcome::Removed));
        assert!(store.is_empty());
        assert_eq!(path_of_key("archive|c:\\a\\b.zip|id1"), PathBuf::from("c:\\a\\b.zip"));
    }

    #[test]
    fn a_missing_cover_file_does_not_prevent_saving() {
        let folder = TempFolder::new("save-gone");
        let mut store = DescriptionStore::new(folder.path().join("descriptions.json"));
        let gone = folder.path().join("gone.jpg");
        let mut picture = post(PostKey::for_file(&gone), "gone", &gone, "");
        let outcome = save_clean_caption(&mut store, &mut picture, "Still described".to_owned(), now());
        assert_eq!(outcome, Ok(SaveOutcome::Saved));
        let saved = store.get(&picture.key).expect("remembered");
        assert_eq!((saved.file_bytes, saved.file_modified), (None, None));
        assert!(!store.to_json_text().contains("file_bytes"));
    }

    #[cfg(windows)]
    #[test]
    fn a_failed_save_loses_nothing_in_memory_or_on_disk() {
        let folder = TempFolder::new("save-fails");
        let path = folder.path().join("descriptions.json");
        let mut store = DescriptionStore::new(path.clone());
        let mut first = loose_post(folder.path(), "first.jpg");
        let mut second = loose_post(folder.path(), "second.jpg");
        let mut third = loose_post(folder.path(), "third.jpg");
        save_clean_caption(&mut store, &mut first, "The first".to_owned(), now()).expect("saved");
        save_clean_caption(&mut store, &mut second, "The second".to_owned(), now()).expect("saved");
        let good_store = store.clone();
        let good_bytes = fs::read(&path).expect("readable");

        // The file can no longer be replaced.
        set_read_only(&path, true);
        let failing = [
            // A new entry, a changed entry and a removed entry: each is rolled back.
            save_clean_caption(&mut store, &mut third, "The third".to_owned(), now() + 1),
            save_clean_caption(&mut store, &mut first, "The first, changed".to_owned(), now() + 2),
            save_clean_caption(&mut store, &mut second, String::new(), now() + 3),
        ];
        for result in failing {
            let error = result.expect_err("the file is read-only");
            assert!(error.message.starts_with(&format!("Could not update {}: ", path.display())), "{error}");
            // The sentence the dialog shows keeps the reason.
            let shown = messages::description_not_saved(&error.message);
            assert!(shown.starts_with("The description could not be saved: Could not update "), "{shown}");
        }
        assert_eq!(store, good_store, "the store is exactly as before");
        assert_eq!(
            (third.caption.as_str(), first.caption.as_str(), second.caption.as_str()),
            ("", "The first", "The second"),
            "the posts are untouched"
        );
        assert_eq!(fs::read(&path).expect("readable"), good_bytes, "the old file is intact");
        assert_eq!(
            folder.names(),
            ["descriptions.json", "first.jpg", "second.jpg", "third.jpg"],
            "no temporary file is left behind"
        );

        // Once the file can be written again, the same edit goes through and nothing was lost.
        set_read_only(&path, false);
        let outcome = save_clean_caption(&mut store, &mut third, "The third".to_owned(), now() + 4);
        assert_eq!(outcome, Ok(SaveOutcome::Saved));
        let loaded = DescriptionStore::load(&path, now()).expect("loaded").store;
        assert_eq!(loaded, store);
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded.get(&first.key).map(|entry| entry.caption.as_str()), Some("The first"));
        assert_eq!(loaded.get(&second.key).map(|entry| entry.caption.as_str()), Some("The second"));
        assert_eq!(loaded.get(&third.key).map(|entry| entry.caption.as_str()), Some("The third"));
    }

    #[test]
    fn a_failed_first_save_leaves_the_store_empty_and_the_post_unchanged() {
        let folder = TempFolder::new("save-blocked");
        // The data folder cannot be created: a file has its name.
        let blocker = folder.path().join("data");
        fs::write(&blocker, b"not a folder").expect("a file in the way");
        let mut store = DescriptionStore::new(blocker.join("descriptions.json"));
        let mut picture = loose_post(folder.path(), "IMG_2.jpg");

        let error = save_clean_caption(&mut store, &mut picture, "Typed".to_owned(), now()).expect_err("no folder");
        assert!(error.message.starts_with("Could not update "), "{error}");
        assert!(store.is_empty());
        assert_eq!(picture.caption, "");
        assert_eq!(fs::read(&blocker).expect("readable"), b"not a folder");
        assert_eq!(folder.names(), ["IMG_2.jpg", "data"]);
    }

    #[test]
    fn the_text_of_the_editor_is_normalised_trimmed_and_limited_before_it_is_stored() {
        let folder = TempFolder::new("save-editor");
        let path = folder.path().join("descriptions.json");
        let mut store = DescriptionStore::new(path.clone());
        let mut picture = loose_post(folder.path(), "IMG_2.jpg");

        // spec-edit 3.4 and 13.1: Windows line breaks become "\n", both ends are trimmed with
        // JavaScript's white space (U+FEFF included), inner white space stays.
        let typed = " \t Low tide  again 🌊\r\nThe second line.\rThe third. \r\n\u{feff}";
        assert_eq!(save_description(&mut store, &mut picture, typed, now()), Ok(SaveOutcome::Saved));
        assert_eq!(picture.caption, "Low tide  again 🌊\nThe second line.\nThe third.");
        assert_eq!(store.get(&picture.key).map(|entry| entry.caption.as_str()), Some(picture.caption.as_str()));

        // Only white space was added around it: no change, and the file is not written again.
        let before = fs::read(&path).expect("readable");
        let padded = "\n\u{3000}Low tide  again 🌊\nThe second line.\nThe third.  ";
        assert_eq!(save_description(&mut store, &mut picture, padded, now() + 1), Ok(SaveOutcome::Unchanged));
        assert_eq!(fs::read(&path).expect("readable"), before);

        // More than the limit (the editor prevents it; a caller might not): cut, never refused.
        let long = format!("{}🍋🍋", "x".repeat(MAX_CAPTION_CHARS - 1));
        assert_eq!(save_description(&mut store, &mut picture, &long, now() + 2), Ok(SaveOutcome::Saved));
        assert_eq!(picture.caption, "x".repeat(MAX_CAPTION_CHARS - 1));
        assert_eq!(DescriptionStore::load(&path, now()).expect("loaded").store, store);

        // White space only is an empty description, which a loose picture starts with.
        assert_eq!(save_description(&mut store, &mut picture, " \r\n\t ", now() + 3), Ok(SaveOutcome::Removed));
        assert_eq!(picture.caption, "");
        assert!(DescriptionStore::load(&path, now()).expect("loaded").store.is_empty());
    }

    #[test]
    fn two_windows_never_wipe_out_each_others_descriptions() {
        let folder = TempFolder::new("two-windows");
        let path = folder.path().join("descriptions.json");
        let mut first = loose_post(folder.path(), "first.jpg");
        let mut second = loose_post(folder.path(), "second.jpg");
        let mut third = loose_post(folder.path(), "third.jpg");

        // Both windows start before anything is saved.
        let mut one = DescriptionStore::load(&path, now()).expect("loaded").store;
        let mut other = DescriptionStore::load(&path, now()).expect("loaded").store;
        save_clean_caption(&mut one, &mut first, "From window one".to_owned(), now()).expect("saved");
        save_clean_caption(&mut other, &mut second, "From the other window".to_owned(), now()).expect("saved");

        // The window that saved last has taken over what the file held.
        let on_disk = DescriptionStore::load(&path, now()).expect("loaded").store;
        assert_eq!(on_disk.len(), 2);
        assert_eq!(on_disk.get(&first.key).map(|entry| entry.caption.as_str()), Some("From window one"));
        assert_eq!(on_disk.get(&second.key).map(|entry| entry.caption.as_str()), Some("From the other window"));
        assert_eq!(other, on_disk);
        assert_eq!(one.len(), 1, "window one has not saved since");

        // The other window removes the first description; window one must not bring it back.
        first.caption = "From window one".to_owned();
        assert_eq!(save_clean_caption(&mut other, &mut first, String::new(), now()), Ok(SaveOutcome::Removed));
        assert_eq!(save_clean_caption(&mut one, &mut third, "A third".to_owned(), now()), Ok(SaveOutcome::Saved));
        let on_disk = DescriptionStore::load(&path, now()).expect("loaded").store;
        assert_eq!(one, on_disk);
        assert!(on_disk.get(&first.key).is_none());
        assert!(on_disk.get(&second.key).is_some() && on_disk.get(&third.key).is_some());
    }

    #[test]
    fn a_file_that_vanished_or_was_damaged_since_is_written_again_from_memory() {
        let folder = TempFolder::new("vanished");
        let path = folder.path().join("descriptions.json");
        let mut store = DescriptionStore::new(path.clone());
        let mut first = loose_post(folder.path(), "first.jpg");
        let mut second = loose_post(folder.path(), "second.jpg");
        let mut third = loose_post(folder.path(), "third.jpg");
        save_clean_caption(&mut store, &mut first, "The first".to_owned(), now()).expect("saved");

        // Somebody deletes the file while the app is open: what this window knows is kept.
        fs::remove_file(&path).expect("removed");
        save_clean_caption(&mut store, &mut second, "The second".to_owned(), now()).expect("saved");
        assert_eq!(store.len(), 2);
        assert_eq!(DescriptionStore::load(&path, now()).expect("loaded").store, store);

        // Somebody damages the file: it is set aside, never overwritten, and nothing is forgotten.
        fs::write(&path, b"{\"descriptions\": {\"file|x\": {\"caption\": \"cut of").expect("the damaged file");
        save_clean_caption(&mut store, &mut third, "The third".to_owned(), now()).expect("saved");
        assert_eq!(store.len(), 3);
        assert_eq!(DescriptionStore::load(&path, now()).expect("loaded"), LoadedStore { store, notice: None });
        let backup = folder.path().join("descriptions.damaged-20261112-081530.json");
        assert_eq!(fs::read(backup).expect("kept"), b"{\"descriptions\": {\"file|x\": {\"caption\": \"cut of");
    }

    #[test]
    fn a_file_that_cannot_be_read_fails_the_save_and_is_not_replaced() {
        let folder = TempFolder::new("save-unreadable");
        let path = folder.path().join("descriptions.json");
        fs::create_dir(&path).expect("a folder in the way");
        let mut store = DescriptionStore::new(path.clone());
        let mut picture = loose_post(folder.path(), "IMG_2.jpg");

        let error = save_clean_caption(&mut store, &mut picture, "Typed".to_owned(), now()).expect_err("unreadable");
        assert!(error.message.starts_with("The saved descriptions in "), "{error}");
        assert!(store.is_empty());
        assert_eq!(picture.caption, "");
        assert!(path.is_dir());
        assert_eq!(folder.names(), ["IMG_2.jpg", "descriptions.json"]);
    }

    // --- state.json ---------------------------------------------------------------------------

    #[test]
    fn a_missing_or_damaged_state_file_means_the_defaults() {
        let folder = TempFolder::new("state-defaults");
        let path = folder.path().join("state.json");
        let defaults = UiState::default();
        assert_eq!(defaults.theme, Theme::Light);
        assert_eq!(defaults.auto_view, autoview::Settings { interval_sec: 15, wait_for_videos: true, loop_posts: true, shuffle: false });
        assert_eq!(defaults.last_folder, None);

        assert_eq!(UiState::load(&path), defaults, "a missing file");
        let damaged: [&[u8]; 7] = [
            b"",
            b"{broken",
            b"null",
            b"[]",
            b"\"dark\"",
            b"\xff\xfe",
            b"{\"theme\": 1, \"auto_view\": \"fast\", \"last_folder\": 7}",
        ];
        for content in damaged {
            fs::write(&path, content).expect("the damaged file");
            assert_eq!(UiState::load(&path), defaults, "{content:?}");
            assert_eq!(fs::read(&path).expect("readable"), content, "reading never writes");
        }
        fs::remove_file(&path).expect("removed");
        fs::create_dir(&path).expect("a folder in the way");
        assert_eq!(UiState::load(&path), defaults, "a folder where the file should be");
    }

    #[test]
    fn theme_and_last_folder_are_read_member_by_member() {
        let dark = UiState::from_json_text(
            "\u{feff}{\"theme\": \"dark\", \"last_folder\": \"C:\\\\Users\\\\Me\\\\Pictures\", \"new\": 1}",
        );
        assert_eq!(dark.theme, Theme::Dark);
        assert_eq!(dark.last_folder, Some(PathBuf::from(r"C:\Users\Me\Pictures")));
        assert_eq!(dark.auto_view, autoview::Settings::default());

        // The clone's rule: exactly "dark" is dark.
        for light in ["\"light\"", "\"Dark\"", "\"\"", "null", "true"] {
            assert_eq!(UiState::from_json_text(&format!("{{\"theme\": {light}}}")).theme, Theme::Light, "{light}");
        }
        for no_folder in ["\"\"", "\"   \"", "null", "[\"C:\\\\\"]"] {
            let state = UiState::from_json_text(&format!("{{\"theme\": \"dark\", \"last_folder\": {no_folder}}}"));
            assert_eq!((state.theme, state.last_folder), (Theme::Dark, None), "{no_folder}");
        }
        assert_eq!(Theme::Dark.toggled().toggled().as_str(), "dark");
    }

    #[test]
    fn the_state_file_round_trips_in_the_documented_layout() {
        let folder = TempFolder::new("state-save");
        let path = folder.path().join("data").join("state.json");
        let state = UiState {
            theme: Theme::Dark,
            auto_view: autoview::Settings { interval_sec: 30, wait_for_videos: true, loop_posts: false, shuffle: true },
            last_folder: Some(PathBuf::from(r"C:\Users\Me\Pictures")),
            last_export_folder: Some(PathBuf::from(r"D:\Exports")),
        };
        let expected = "{\n  \"theme\": \"dark\",\n  \"auto_view\": {\n    \"interval_sec\": 30,\n    \"wait_for_videos\": true,\n    \"loop\": false,\n    \"shuffle\": true\n  },\n  \"last_folder\": \"C:\\\\Users\\\\Me\\\\Pictures\",\n  \"last_export_folder\": \"D:\\\\Exports\"\n}\n";
        assert_eq!(state.to_json_text(), expected);

        // Saving creates the data folder; reading gives the same state back.
        state.save(&path).expect("saved");
        assert_eq!(fs::read_to_string(&path).expect("readable"), expected);
        assert_eq!(UiState::load(&path), state);
        assert_eq!(names_in(path.parent().expect("a parent")), ["state.json"]);

        // Without a last folder the member is left out.
        let defaults = UiState::default();
        assert_eq!(
            defaults.to_json_text(),
            "{\n  \"theme\": \"light\",\n  \"auto_view\": {\n    \"interval_sec\": 15,\n    \"wait_for_videos\": true,\n    \"loop\": true,\n    \"shuffle\": false\n  }\n}\n"
        );
        assert_eq!(UiState::from_json_text(&defaults.to_json_text()), defaults);
        defaults.save(&path).expect("saved again");
        assert_eq!(UiState::load(&path), defaults);

        // Every member is read on its own: a broken part does not cost the others.
        let mixed = UiState::from_json_text(
            "{\"theme\": \"dark\", \"auto_view\": {\"interval_sec\": 1e9, \"loop\": \"yes\", \"shuffle\": true}, \"last_folder\": 7, \"last_export_folder\": \"  \"}",
        );
        assert_eq!(mixed.theme, Theme::Dark);
        assert_eq!(mixed.auto_view, autoview::Settings { interval_sec: 3600, wait_for_videos: true, loop_posts: true, shuffle: true });
        assert_eq!(mixed.last_folder, None);
        assert_eq!(mixed.last_export_folder, None);

        // A state that cannot be written is reported, for the log.
        let blocker = folder.path().join("blocker");
        fs::write(&blocker, b"a file").expect("a file in the way");
        let error = state.save(&blocker.join("state.json")).expect_err("no folder");
        assert!(error.message.starts_with("Could not update "), "{error}");
    }
}
