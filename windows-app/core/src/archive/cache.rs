//! The archive cache: where the members of an opened archive live as plain files.
//!
//! Decoders, Windows Imaging Component, the exporter and sniffing all take a path, and a gzip
//! stream must be unpacked anyway, so an opened archive is extracted rather than read in place
//! (web brief 3.3). Only the members post.json references are kept. The layout:
//!
//! ```text
//! %LOCALAPPDATA%\instagram-windows-app\archives\        settings::AppPaths::archive_cache_dir
//!   <key>\                                               16 hex of FNV-1a-64 of the folded archive path
//!     <size>-<mtime_ms>-<rand8>\                         one generation per version of the archive
//!       entry.json      what the generation holds (format, version, archive, size, modified, kind,
//!                       manifest, root, created_at, members)
//!       post.json       a copy of the archive's manifest
//!       m000001.jpg     one file per kept member, named by its position in the listing and its
//!                       lower-case extension when that is [a-z0-9]{1,5}
//!       in-use.lock     shared-locked by every window that shows the generation
//!   .tmp-<rand8>\                                        a generation being written
//! ```
//!
//! * Safety by construction: every file name in the cache is made here, so a member called
//!   `../x`, `C:\x`, `a:b` or `con.jpg` can never name a file on disk.
//! * Reuse: a generation is used again, without reading the archive at all, while the archive's
//!   size and modified time are those recorded and every kept file is there with its size.
//! * Leases: a window that shows a generation holds a shared lock on its `in-use.lock`
//!   (`ArchiveLease`). Clean-up deletes only what it can lock exclusively, so a generation in
//!   use by this or another window of the app is never touched.
//! * Clean-up (`ArchiveCache::clean_up`): older generations of an archive, generations unused for
//!   `ARCHIVE_CACHE_MAX_AGE_DAYS`, then the least recently used ones while the cache holds more
//!   than `ARCHIVE_CACHE_MAX_BYTES`, and temporary folders older than an hour.
//! * Free space: extracting never leaves less than `ARCHIVE_CACHE_MIN_FREE_BYTES` free.

use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value};

use super::{ArchiveError, ArchiveErrorCode, ArchiveKind};
use crate::messages;
use crate::model::fold_path;
use crate::text;

const GIB: u64 = 1024 * 1024 * 1024;

/// The value of "format" in entry.json.
pub const ARCHIVE_CACHE_FORMAT: &str = "instagram-windows-app-archive-cache";
/// The value of "version" in entry.json; a generation of another version is not used.
pub const ARCHIVE_CACHE_VERSION: u64 = 1;
/// Clean-up removes the least recently used generations while the cache holds more than this.
pub const ARCHIVE_CACHE_MAX_BYTES: u64 = 8 * GIB;
/// Clean-up removes generations that were not used for this many days.
pub const ARCHIVE_CACHE_MAX_AGE_DAYS: u64 = 30;
/// Extracting never leaves less than this free on the drive of the cache.
pub const ARCHIVE_CACHE_MIN_FREE_BYTES: u64 = GIB;
/// A temporary folder older than this whose lock is free is left over from a crash.
pub const STALE_TEMP_MS: i64 = 60 * 60 * 1000;

/// What a generation records about itself.
pub const ENTRY_FILE_NAME: &str = "entry.json";
/// The lock every window that shows a generation holds shared.
pub const LOCK_FILE_NAME: &str = "in-use.lock";
/// The copy of the archive's manifest in a generation.
pub const MANIFEST_COPY_NAME: &str = "post.json";
/// The prefix of a generation that is being written.
pub const TEMP_PREFIX: &str = ".tmp-";

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// How the free space of a drive is asked for: the folder of the cache in, the bytes free for
/// this user out, `None` when that cannot be told. Tests pass a function of their own.
pub type FreeSpaceFn = fn(&Path) -> Option<u64>;

/// The limits of the cache (tests make them small).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CacheLimits {
    /// `ARCHIVE_CACHE_MAX_BYTES`.
    pub max_bytes: u64,
    /// `ARCHIVE_CACHE_MAX_AGE_DAYS`, in milliseconds.
    pub max_age_ms: i64,
    /// `ARCHIVE_CACHE_MIN_FREE_BYTES`.
    pub min_free_bytes: u64,
    /// `STALE_TEMP_MS`.
    pub stale_temp_ms: i64,
}

impl Default for CacheLimits {
    fn default() -> CacheLimits {
        CacheLimits {
            max_bytes: ARCHIVE_CACHE_MAX_BYTES,
            max_age_ms: ARCHIVE_CACHE_MAX_AGE_DAYS as i64 * DAY_MS,
            min_free_bytes: ARCHIVE_CACHE_MIN_FREE_BYTES,
            stale_temp_ms: STALE_TEMP_MS,
        }
    }
}

/// The archive cache in one folder. Cheap to clone: it is a path and a few numbers.
#[derive(Clone, Debug)]
pub struct ArchiveCache {
    root: PathBuf,
    limits: CacheLimits,
    free_space: FreeSpaceFn,
}

/// The version of an archive file: what must be unchanged for a generation to be used again.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ArchiveVersion {
    /// The size in bytes.
    pub size: u64,
    /// The modified time, epoch milliseconds.
    pub modified_ms: i64,
}

impl ArchiveVersion {
    /// The version of the file `metadata` describes.
    pub fn of(metadata: &fs::Metadata) -> ArchiveVersion {
        ArchiveVersion { size: metadata.len(), modified_ms: metadata.modified().map(text::epoch_ms_from_system_time).unwrap_or(0) }
    }
}

/// One member below the export root of an archive, as entry.json records it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CachedMember {
    /// Its path relative to the export root (`manifest::SourceFile::relative`).
    pub path: String,
    /// The size the archive declares.
    pub size: u64,
    /// The name of its copy in the generation; `None` when it was not kept (post.json does not
    /// reference it, it is too large for its use, or it could not be extracted).
    pub file: Option<String>,
    /// Why it could not be extracted, when it was referenced and that failed.
    pub unreadable: Option<String>,
}

/// What entry.json of a generation says.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CacheEntry {
    /// The archive (absolute).
    pub archive: PathBuf,
    /// The version of the archive the generation was made from.
    pub version: ArchiveVersion,
    /// The container.
    pub kind: ArchiveKind,
    /// The path of the manifest inside the archive.
    pub manifest: String,
    /// The export root inside the archive (the folder of the manifest; "" at the top).
    pub root: String,
    /// When the generation was written, epoch milliseconds.
    pub created_at: i64,
    /// The members below the root, in the order of the archive.
    pub members: Vec<CachedMember>,
}

impl CacheEntry {
    /// The text of entry.json.
    pub fn to_json_text(&self) -> String {
        let members: Vec<Value> = self
            .members
            .iter()
            .map(|member| {
                let mut object = Map::new();
                object.insert("path".to_owned(), Value::from(member.path.as_str()));
                object.insert("size".to_owned(), Value::from(member.size));
                object.insert("file".to_owned(), member.file.as_deref().map_or(Value::Null, Value::from));
                if let Some(reason) = &member.unreadable {
                    object.insert("unreadable".to_owned(), Value::from(reason.as_str()));
                }
                Value::Object(object)
            })
            .collect();
        let mut document = Map::new();
        document.insert("format".to_owned(), Value::from(ARCHIVE_CACHE_FORMAT));
        document.insert("version".to_owned(), Value::from(ARCHIVE_CACHE_VERSION));
        document.insert("archive".to_owned(), Value::from(self.archive.to_string_lossy().into_owned()));
        document.insert("size".to_owned(), Value::from(self.version.size));
        document.insert("modified".to_owned(), Value::from(self.version.modified_ms));
        document.insert("kind".to_owned(), Value::from(self.kind.as_str()));
        document.insert("manifest".to_owned(), Value::from(self.manifest.as_str()));
        document.insert("root".to_owned(), Value::from(self.root.as_str()));
        document.insert("created_at".to_owned(), text::iso_time_millis(self.created_at).map_or(Value::Null, Value::from));
        document.insert("members".to_owned(), Value::Array(members));
        format!("{:#}\n", Value::Object(document))
    }

    /// Reads entry.json; `None` for anything that is not a complete entry of this version.
    pub fn from_json_text(text: &str) -> Option<CacheEntry> {
        let document: Value = serde_json::from_str(text).ok()?;
        let object = document.as_object()?;
        if object.get("format")?.as_str()? != ARCHIVE_CACHE_FORMAT || object.get("version")?.as_u64()? != ARCHIVE_CACHE_VERSION {
            return None;
        }
        let kind = match object.get("kind")?.as_str()? {
            "zip" => ArchiveKind::Zip,
            "tar" => ArchiveKind::Tar,
            "gzip" => ArchiveKind::Gzip,
            _ => return None,
        };
        let members = object
            .get("members")?
            .as_array()?
            .iter()
            .map(|member| {
                let member = member.as_object()?;
                Some(CachedMember {
                    path: member.get("path")?.as_str()?.to_owned(),
                    size: member.get("size")?.as_u64()?,
                    file: member.get("file").and_then(Value::as_str).map(str::to_owned),
                    unreadable: member.get("unreadable").and_then(Value::as_str).map(str::to_owned),
                })
            })
            .collect::<Option<Vec<CachedMember>>>()?;
        Some(CacheEntry {
            archive: PathBuf::from(object.get("archive")?.as_str()?),
            version: ArchiveVersion { size: object.get("size")?.as_u64()?, modified_ms: object.get("modified")?.as_i64()? },
            kind,
            manifest: object.get("manifest")?.as_str()?.to_owned(),
            root: object.get("root")?.as_str()?.to_owned(),
            created_at: object.get("created_at").and_then(Value::as_str).and_then(text::parse_timestamp_text).unwrap_or(0),
            members,
        })
    }

    /// The bytes the generation holds: the kept members.
    fn kept_bytes(&self) -> u64 {
        self.members.iter().filter(|member| member.file.is_some()).map(|member| member.size).sum()
    }
}

/// A shared lock on a generation: while it is held, no clean-up of any window deletes the
/// generation. Dropping it releases the lock.
#[derive(Debug)]
pub struct ArchiveLease {
    archive: PathBuf,
    generation: PathBuf,
    // Held for its lock; closing the handle releases it.
    _lock: File,
}

impl ArchiveLease {
    /// The archive whose members the generation holds.
    pub fn archive(&self) -> &Path {
        &self.archive
    }

    /// The generation folder.
    pub fn generation(&self) -> &Path {
        &self.generation
    }
}

/// A generation that can be shown: its folder, what it holds, and the lease that keeps it.
#[derive(Debug)]
pub struct Generation {
    /// The generation folder.
    pub folder: PathBuf,
    /// Its entry.json.
    pub entry: CacheEntry,
    /// The shared lock on it.
    pub lease: ArchiveLease,
}

/// What a clean-up did.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CleanUpReport {
    /// The generation and temporary folders that were removed.
    pub removed: Vec<PathBuf>,
    /// The bytes the cache still holds.
    pub kept_bytes: u64,
}

/// A short random-looking suffix for a new folder name: unique within this process (a counter)
/// and very likely across processes (process id and clock).
fn random_suffix() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).map(|elapsed| elapsed.as_nanos()).unwrap_or(0);
    let seed = format!("{}-{counter}-{nanos}-{:?}", std::process::id(), std::thread::current().id());
    format!("{:08x}", text::fnv1a_64(seed.as_bytes()) as u32)
}

/// The modified time of a file or folder, epoch milliseconds.
fn modified_ms(path: &Path) -> Option<i64> {
    fs::metadata(path).ok()?.modified().ok().map(text::epoch_ms_from_system_time)
}

/// Opens the lock file of a generation for locking and touching. It is never created here: a
/// generation without one is not usable.
fn open_lock(folder: &Path) -> io::Result<File> {
    OpenOptions::new().read(true).write(true).open(folder.join(LOCK_FILE_NAME))
}

/// A failure to write the cache.
fn cache_failed(error: &io::Error) -> ArchiveError {
    ArchiveError::new(ArchiveErrorCode::CacheFailed, messages::archive_cache_not_written(&error.to_string()))
}

/// The free space for this user on the drive of `path` (its first existing ancestor).
#[cfg(windows)]
pub fn system_free_space(path: &Path) -> Option<u64> {
    use std::os::windows::ffi::OsStrExt;
    let existing = path.ancestors().find(|ancestor| ancestor.exists())?;
    let wide: Vec<u16> = existing.as_os_str().encode_wide().chain(std::iter::once(0)).collect();
    let mut free: u64 = 0;
    // SAFETY: `wide` is a NUL-terminated UTF-16 string that outlives the call, and `free` is a
    // valid place for the one number asked for; the other two are not asked for.
    let asked = unsafe {
        windows::Win32::Storage::FileSystem::GetDiskFreeSpaceExW(windows::core::PCWSTR(wide.as_ptr()), Some(&mut free), None, None)
    };
    asked.ok().map(|()| free)
}

/// The free space of a drive is not known on other platforms; the check is then skipped.
#[cfg(not(windows))]
pub fn system_free_space(_path: &Path) -> Option<u64> {
    None
}

impl ArchiveCache {
    /// The cache in `root` (`settings::AppPaths::archive_cache_dir`) with the default limits and
    /// the drive's real free space. Nothing is created until something is extracted.
    pub fn new(root: PathBuf) -> ArchiveCache {
        ArchiveCache { root, limits: CacheLimits::default(), free_space: system_free_space }
    }

    /// The same cache with other limits.
    pub fn with_limits(mut self, limits: CacheLimits) -> ArchiveCache {
        self.limits = limits;
        self
    }

    /// The same cache asking `free_space` for the free bytes of its drive.
    pub fn with_free_space(mut self, free_space: FreeSpaceFn) -> ArchiveCache {
        self.free_space = free_space;
        self
    }

    /// The folder of the cache.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The limits in effect.
    pub fn limits(&self) -> CacheLimits {
        self.limits
    }

    /// The folder of the generations of `archive`: 16 hex digits of FNV-1a-64 of its folded path.
    pub fn key_folder(&self, archive: &Path) -> PathBuf {
        self.root.join(format!("{:016x}", text::fnv1a_64(fold_path(archive).as_bytes())))
    }

    /// Fails with `archive_cache_no_space` when writing `needed` bytes would leave less than
    /// `min_free_bytes` free on the drive of the cache. An unknown free space passes.
    pub fn ensure_free(&self, needed: u64) -> Result<(), ArchiveError> {
        match (self.free_space)(&self.root) {
            Some(free) if free < needed.saturating_add(self.limits.min_free_bytes) => Err(ArchiveError::new(
                ArchiveErrorCode::NoSpace,
                messages::archive_cache_no_space(free, self.limits.min_free_bytes),
            )),
            _ => Ok(()),
        }
    }

    /// A usable generation of `archive` at `version`, leased: entry.json is complete, of this
    /// version, made from this archive at this size and time, and every kept file is there with
    /// its size. A generation that is being deleted (its lock cannot be shared) is passed over.
    /// The lock file's modified time is set to now: it is the "last used" of the clean-up.
    pub fn find(&self, archive: &Path, version: ArchiveVersion) -> Option<Generation> {
        let key_folder = self.key_folder(archive);
        let prefix = format!("{}-{}-", version.size, version.modified_ms);
        let mut candidates: Vec<PathBuf> = fs::read_dir(&key_folder)
            .ok()?
            .flatten()
            .filter(|entry| entry.file_name().to_string_lossy().starts_with(&prefix))
            .map(|entry| entry.path())
            .collect();
        candidates.sort();
        candidates.reverse();
        candidates.into_iter().find_map(|folder| self.lease(&folder, archive, Some(version)))
    }

    /// Takes the shared lock of the generation in `folder` and checks it (see `find`); `version`
    /// `None` skips the comparison with the archive (a generation just written).
    fn lease(&self, folder: &Path, archive: &Path, version: Option<ArchiveVersion>) -> Option<Generation> {
        let lock = open_lock(folder).ok()?;
        lock.try_lock_shared().ok()?;
        let entry = CacheEntry::from_json_text(&fs::read_to_string(folder.join(ENTRY_FILE_NAME)).ok()?)?;
        let same_archive = fold_path(&entry.archive) == fold_path(archive);
        let same_version = version.is_none_or(|version| entry.version == version);
        let complete = fs::metadata(folder.join(MANIFEST_COPY_NAME)).is_ok_and(|metadata| metadata.is_file())
            && entry.members.iter().all(|member| match &member.file {
                Some(file) => fs::metadata(folder.join(file)).is_ok_and(|metadata| metadata.is_file() && metadata.len() == member.size),
                None => true,
            });
        if !(same_archive && same_version && complete) {
            return None;
        }
        if let Err(error) = lock.set_modified(SystemTime::now()) {
            log::debug!("The last use of {} could not be noted: {error}", folder.display());
        }
        Some(Generation {
            folder: folder.to_path_buf(),
            entry,
            lease: ArchiveLease { archive: archive.to_path_buf(), generation: folder.to_path_buf(), _lock: lock },
        })
    }

    /// Starts a new generation of `archive` in a temporary folder of the cache.
    pub fn begin(&self, archive: &Path, version: ArchiveVersion) -> Result<GenerationWriter, ArchiveError> {
        fs::create_dir_all(&self.root).map_err(|error| cache_failed(&error))?;
        let mut attempt = 0;
        let folder = loop {
            let folder = self.root.join(format!("{TEMP_PREFIX}{}", random_suffix()));
            match fs::create_dir(&folder) {
                Ok(()) => break folder,
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists && attempt < 100 => attempt += 1,
                Err(error) => return Err(cache_failed(&error)),
            }
        };
        let mut writer = GenerationWriter { cache: self.clone(), archive: archive.to_path_buf(), version, folder, lock: None, done: false };
        let lock = OpenOptions::new().read(true).write(true).create_new(true).open(writer.folder.join(LOCK_FILE_NAME)).map_err(|error| cache_failed(&error))?;
        // Held while the folder is written, so that a clean-up never takes it for a leftover.
        let _ = lock.try_lock_shared();
        writer.lock = Some(lock);
        Ok(writer)
    }

    /// Removes what the cache no longer needs (module comment), at `now_ms`. Only generations
    /// whose lock can be taken exclusively are touched: one shown by any window stays. Failures
    /// are logged and skipped; the clean-up never fails.
    pub fn clean_up(&self, now_ms: i64) -> CleanUpReport {
        let mut report = CleanUpReport::default();
        let Ok(listing) = fs::read_dir(&self.root) else {
            return report;
        };
        struct Found {
            folder: PathBuf,
            key: String,
            created_at: i64,
            last_used_ms: i64,
            bytes: u64,
        }
        let mut generations: Vec<Found> = Vec::new();
        for item in listing.flatten() {
            if !item.file_type().is_ok_and(|kind| kind.is_dir()) {
                continue;
            }
            let name = item.file_name().to_string_lossy().into_owned();
            let folder = item.path();
            if name.starts_with(TEMP_PREFIX) {
                let age = modified_ms(&folder.join(LOCK_FILE_NAME)).or_else(|| modified_ms(&folder)).map_or(i64::MAX, |modified| now_ms - modified);
                if age > self.limits.stale_temp_ms && remove_generation(&folder) {
                    report.removed.push(folder);
                }
                continue;
            }
            let Ok(inner) = fs::read_dir(&folder) else { continue };
            for generation in inner.flatten() {
                if !generation.file_type().is_ok_and(|kind| kind.is_dir()) {
                    continue;
                }
                let path = generation.path();
                let entry = fs::read_to_string(path.join(ENTRY_FILE_NAME)).ok().and_then(|text| CacheEntry::from_json_text(&text));
                let bytes = match &entry {
                    Some(entry) => entry.kept_bytes(),
                    None => fs::read_dir(&path).map(|files| files.flatten().filter_map(|file| file.metadata().ok()).map(|metadata| metadata.len()).sum()).unwrap_or(0),
                };
                let last_used_ms = modified_ms(&path.join(LOCK_FILE_NAME)).or_else(|| modified_ms(&path)).unwrap_or(0);
                generations.push(Found { folder: path, key: name.clone(), created_at: entry.map_or(0, |entry| entry.created_at), last_used_ms, bytes });
            }
        }

        // The newest generation of each archive; every older one is out of date.
        let mut newest: std::collections::HashMap<String, (i64, PathBuf)> = std::collections::HashMap::new();
        for found in &generations {
            let candidate = (found.created_at, found.folder.clone());
            match newest.get(&found.key) {
                Some(best) if *best >= candidate => {}
                _ => {
                    newest.insert(found.key.clone(), candidate);
                }
            }
        }
        let mut kept: Vec<Found> = Vec::new();
        for found in generations {
            let outdated = newest.get(&found.key).is_some_and(|(_, folder)| *folder != found.folder);
            let unused = now_ms - found.last_used_ms > self.limits.max_age_ms;
            if (outdated || unused) && remove_generation(&found.folder) {
                report.removed.push(found.folder);
            } else {
                kept.push(found);
            }
        }

        // Least recently used first while the cache is over its size.
        kept.sort_by_key(|found| found.last_used_ms);
        let mut total: u64 = kept.iter().map(|found| found.bytes).sum();
        for found in &kept {
            if total <= self.limits.max_bytes {
                break;
            }
            if remove_generation(&found.folder) {
                total = total.saturating_sub(found.bytes);
                report.removed.push(found.folder.clone());
            }
        }
        report.kept_bytes = total;

        // Folders of archives without any generation left.
        if let Ok(listing) = fs::read_dir(&self.root) {
            for item in listing.flatten() {
                let name = item.file_name().to_string_lossy().into_owned();
                if !name.starts_with(TEMP_PREFIX) && item.file_type().is_ok_and(|kind| kind.is_dir()) {
                    // Fails, as it should, while the folder holds anything.
                    let _ = fs::remove_dir(item.path());
                }
            }
        }
        for removed in &report.removed {
            log::info!("Archive cache: removed {}.", removed.display());
        }
        report
    }
}

/// Deletes a generation (or a temporary folder) when its lock can be taken exclusively: first
/// entry.json, so that it can no longer be taken for usable, then the files, then the lock and
/// the folder. Returns false and leaves everything when someone holds the lock.
fn remove_generation(folder: &Path) -> bool {
    let lock = match open_lock(folder) {
        Ok(lock) => {
            if lock.try_lock().is_err() {
                return false;
            }
            Some(lock)
        }
        // Without a lock file nobody can be using the folder.
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            log::debug!("{} is left in the archive cache: {error}", folder.display());
            return false;
        }
    };
    let _ = fs::remove_file(folder.join(ENTRY_FILE_NAME));
    if let Ok(files) = fs::read_dir(folder) {
        for file in files.flatten() {
            if file.file_name() != LOCK_FILE_NAME {
                let _ = if file.file_type().is_ok_and(|kind| kind.is_dir()) { fs::remove_dir_all(file.path()) } else { fs::remove_file(file.path()) };
            }
        }
    }
    drop(lock);
    match fs::remove_dir_all(folder) {
        Ok(()) => true,
        Err(error) => {
            log::debug!("{} could not be removed from the archive cache: {error}", folder.display());
            false
        }
    }
}

/// A generation being written in a temporary folder of the cache. `finish` publishes it under
/// its generation name; dropping it unfinished removes the folder.
#[derive(Debug)]
pub struct GenerationWriter {
    cache: ArchiveCache,
    archive: PathBuf,
    version: ArchiveVersion,
    folder: PathBuf,
    lock: Option<File>,
    done: bool,
}

impl GenerationWriter {
    /// The temporary folder files are written to.
    pub fn folder(&self) -> &Path {
        &self.folder
    }

    /// Writes entry.json and publishes the generation: the temporary folder is renamed to
    /// `<key>\<size>-<mtime_ms>-<rand8>` and leased. (Windows does not rename a folder with an
    /// open file in it, so the writer's own lock is released just before.)
    pub fn finish(mut self, entry: &CacheEntry) -> Result<Generation, ArchiveError> {
        fs::write(self.folder.join(ENTRY_FILE_NAME), entry.to_json_text()).map_err(|error| cache_failed(&error))?;
        drop(self.lock.take());
        let key_folder = self.cache.key_folder(&self.archive);
        fs::create_dir_all(&key_folder).map_err(|error| cache_failed(&error))?;
        let target = key_folder.join(format!("{}-{}-{}", self.version.size, self.version.modified_ms, random_suffix()));
        fs::rename(&self.folder, &target).map_err(|error| cache_failed(&error))?;
        self.done = true;
        // A clean-up of another window could only have taken the generation between the rename
        // and the lock if it were not the newest of its archive, which it is; it is checked anyway.
        self.cache.lease(&target, &self.archive, None).ok_or_else(|| cache_failed(&io::Error::other("the new generation disappeared")))
    }
}

impl Drop for GenerationWriter {
    fn drop(&mut self) {
        if !self.done {
            drop(self.lock.take());
            let _ = fs::remove_dir_all(&self.folder);
        }
    }
}

/// The name of the copy of member `index` (0-based) with the archive path `path`: "m", six digits
/// of `index + 1`, and the lower-case extension of `path` when it is 1 to 5 ASCII letters or
/// digits. Nothing else of the archive's name is used.
pub fn member_file_name(index: usize, path: &str) -> String {
    let base = path.rsplit('/').next().unwrap_or(path);
    let extension = base.rsplit_once('.').map(|(_, extension)| extension.to_ascii_lowercase()).filter(|extension| {
        (1..=5).contains(&extension.len()) && extension.bytes().all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
    });
    match extension {
        Some(extension) => format!("m{:06}.{extension}", index + 1),
        None => format!("m{:06}", index + 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn member_files_are_named_by_position_never_by_the_archive() {
        assert_eq!(member_file_name(0, "posts/IMG_1.JPG"), "m000001.jpg");
        assert_eq!(member_file_name(41, "../../evil.jpg"), "m000042.jpg");
        assert_eq!(member_file_name(2, "C:/Windows/con.jpeg"), "m000003.jpeg");
        assert_eq!(member_file_name(3, "a:b.webm"), "m000004.webm");
        assert_eq!(member_file_name(4, "post.json"), "m000005.json");
        assert_eq!(member_file_name(5, "noextension"), "m000006");
        assert_eq!(member_file_name(6, "weird.ext:stream"), "m000007");
        assert_eq!(member_file_name(7, "long.extension"), "m000008");
        assert_eq!(member_file_name(8, "caf\u{e9}.j\u{e9}g"), "m000009");
    }

    #[test]
    fn entry_json_reads_back_what_was_written() {
        let entry = CacheEntry {
            archive: PathBuf::from("C:\\Downloads\\export.zip"),
            version: ArchiveVersion { size: 1234, modified_ms: 1_762_000_000_000 },
            kind: ArchiveKind::Gzip,
            manifest: "export/post.json".to_owned(),
            root: "export".to_owned(),
            created_at: 1_762_000_000_123,
            members: vec![
                CachedMember { path: "posts/a.jpg".to_owned(), size: 10, file: Some("m000002.jpg".to_owned()), unreadable: None },
                CachedMember { path: "posts/b.jpg".to_owned(), size: 20, file: None, unreadable: Some("broken".to_owned()) },
            ],
        };
        let text = entry.to_json_text();
        assert!(text.starts_with("{\n  \"format\": \"instagram-windows-app-archive-cache\",\n  \"version\": 1,"), "{text}");
        assert_eq!(CacheEntry::from_json_text(&text), Some(entry.clone()));
        assert_eq!(entry.kept_bytes(), 10);
        assert_eq!(CacheEntry::from_json_text(&text.replace("\"version\": 1", "\"version\": 2")), None);
        assert_eq!(CacheEntry::from_json_text("not json"), None);
    }
}
