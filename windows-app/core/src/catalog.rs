//! Opening what the user points at: picture and video files, a folder of them, exports and
//! archives.
//!
//! The reference web project can only open an export; this module is the native viewer's own
//! rule set for loose pictures (design brief "Opening pictures"; spec-viewer 14.2), built so
//! that a loose picture is a post like any other and exports like one, plus the clone's rules
//! for export folders and archives (web brief 3):
//!
//! ```text
//! one picture file         every supported picture directly in its folder; the viewer opens on it
//! several files            exactly those files
//! a folder                 every supported picture directly in it (no recursion)
//! a folder with post.json  (or that file itself) an export, read by `manifest`
//! a wrapping folder        an export one folder down, when the folder has no picture of its own
//! an archive               .zip, .tar, .tar.gz, .tgz: an export, read by `archive`
//! a folder of archives     no post.json and no picture-named file in it: its archives
//! several of the above     every export folder and archive, merged (`sources`)
//! ```
//!
//! A folder that holds supported pictures directly always opens as those pictures: a post.json
//! one level down (an export the user unpacked into a subfolder of the Pictures folder) only
//! makes a folder an export when the folder holds no supported picture of its own, and its
//! archives only count when no file in it is named like a picture.
//!
//! "Supported" is decided by the first bytes of a file (`sniff`), never by its name, and means a
//! picture or a video (MP4, MOV, WebM; video brief 5.1). Loose pictures and videos are ordered
//! like Explorer orders file names (`text::natural_cmp`). Each one is a post of its own, never a
//! carousel, because each file needs its own description.
//!
//! Opening never decodes a picture: of every file only the size, the modified time and the
//! first `sniff::SNIFF_BYTES` bytes are read, on a few threads, so a folder of several thousand
//! pictures opens at once.
//!
//! Descriptions typed earlier come from `store::DescriptionStore` and are laid over the freshly
//! opened posts here; nothing is ever written into the pictures' folder.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::path::{Path, PathBuf};

use crate::archive::cache::{ArchiveCache, ArchiveLease};
use crate::archive::{self, MediaBudget};
use crate::manifest::{self, IMAGE_CONCURRENCY, MAX_IMAGE_BYTES, MAX_SOURCES, MAX_SOURCE_ENTRIES, MAX_VIDEO_BYTES};
use crate::messages::{self, display_name};
use crate::model::{
    fold_path, Collection, CollectionSource, ExportKind, ExportSource, MediaItem, MediaKind, MediaType, Post, PostKey,
    PostOrigin, User, Warning, WarningCode,
};
use crate::sniff;
use crate::sources::{self, LoadedSource, MergeInput};
use crate::store::DescriptionStore;
use crate::text;

/// The file extensions offered by the "Open pictures" dialog filter, lower case, without the
/// dot. This is only a filter for the dialog: whether a file is a picture is decided by its
/// content, so a picture with any other extension still opens.
pub const PICTURE_EXTENSIONS: [&str; 9] = ["jpg", "jpeg", "png", "webp", "gif", "bmp", "heic", "heif", "avif"];

/// The file extensions of the videos the app plays, for the dialog filter and for the rule which
/// files found in a folder are reported when they cannot be shown. As for pictures, the content
/// decides: a video with any other name still opens.
pub const VIDEO_EXTENSIONS: [&str; 4] = ["mp4", "m4v", "mov", "webm"];

/// How many folder levels below an opened folder a post.json is looked for: directly in it, or
/// in one wrapping folder (the layouts instagram.md promises). Deeper manifests are not searched,
/// so a large picture folder is never walked recursively. A manifest one level down counts only
/// for a folder without a supported picture of its own (`manifest_of`).
pub const MANIFEST_SEARCH_DEPTH: usize = 1;

/// The UTC years a file's modified time must lie in to be shown as the time of a loose picture:
/// the range of `text::parse_timestamp_text`, so that what is exported can be read back.
const TAKEN_AT_YEARS: std::ops::RangeInclusive<i32> = 1971..=9999;

/// Below this many files they are looked at one after another; threads only pay off for more.
const MIN_FILES_PER_WORKER: usize = 32;

/// Options that shape the posts of loose pictures.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenOptions {
    /// The author of every loose picture (`settings::Settings::default_username`).
    pub default_username: String,
}

/// What to open: the paths from the command line, a file dialog, a folder dialog or a drop.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenRequest {
    /// Files and folders, in the order they were given; relative paths count from the current
    /// folder of the process.
    pub paths: Vec<PathBuf>,
    /// What shapes the posts of loose pictures.
    pub options: OpenOptions,
}

/// What opening needs besides the request: where archives are unpacked, and which folder is the
/// browser imports folder. Cheap to clone; the opener thread gets its own copy.
#[derive(Clone, Debug)]
pub struct OpenContext {
    /// The archive cache (`settings::AppPaths::archive_cache_dir`).
    pub archive_cache: ArchiveCache,
    /// The browser imports folder (`settings::AppPaths::library_dir`): an export folder there is
    /// recorded as `ExportKind::WebLibrary` and named `messages::BROWSER_IMPORTS_NAME`.
    pub library_dir: Option<PathBuf>,
}

impl OpenContext {
    /// A context with the archive cache in `archive_cache_dir` and no browser imports folder.
    pub fn new(archive_cache_dir: PathBuf) -> OpenContext {
        OpenContext { archive_cache: ArchiveCache::new(archive_cache_dir), library_dir: None }
    }
}

/// A freshly opened collection.
#[derive(Debug)]
pub struct Opened {
    /// The posts with the stored descriptions already laid over them, and what the banner says.
    pub collection: Collection,
    /// The index of the post the viewer opens on: the picked picture when exactly ONE picture
    /// file was opened; `None` otherwise (the feed is shown).
    pub focus: Option<usize>,
    /// A sentence to show at once, beside the warnings of the banner: why the ONE file that was
    /// picked is not among the posts (it is no picture, it cannot be read, ...), while the other
    /// pictures of its folder are shown. `None` otherwise.
    pub notice: Option<String>,
    /// The leases of the archive cache generations the posts of archives use. They travel with
    /// the result (the collection itself is plain data) and are kept while the posts are shown.
    pub leases: Vec<ArchiveLease>,
    /// True when an archive was unpacked into the cache (a clean-up of the cache is due).
    pub cache_written: bool,
}

/// Why nothing could be opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OpenErrorCode {
    /// The request holds no path.
    NothingToOpen,
    /// A path does not exist or cannot be read.
    ReadFailed,
    /// There is not a single supported picture.
    NoPictures,
    /// A folder holds more than `manifest::MAX_SOURCE_ENTRIES` entries.
    TooLarge,
    /// The export folders or archives could not be read; the message is that of the
    /// `manifest::ManifestError` or `archive::ArchiveError` (one source), or
    /// `messages::no_source_imported` (several).
    Export,
}

/// Raised when nothing could be opened. `message` is a complete sentence for the user; the
/// collection on screen stays as it is.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OpenError {
    /// The kind of failure, for tests and for the UI's choice of presentation.
    pub code: OpenErrorCode,
    /// The sentence to show.
    pub message: String,
    /// What was skipped before giving up (for example why each picked file is not a picture),
    /// so the UI can show the reasons under the message.
    pub warnings: Vec<crate::model::Warning>,
}

impl OpenError {
    /// An error without warnings.
    pub fn new(code: OpenErrorCode, message: impl Into<String>) -> OpenError {
        OpenError { code, message: message.into(), warnings: Vec::new() }
    }
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for OpenError {}

// --- Paths --------------------------------------------------------------------------------------

/// The path made absolute without asking the file system (the file may be gone). Absolute paths
/// are what the standard library needs to reach files beyond the classic 260-character limit.
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf())
}

/// The folder a file lies in.
fn folder_of(path: &Path) -> PathBuf {
    path.parent().map(Path::to_path_buf).unwrap_or_default()
}

/// The comparison key of a path: `PostKey` already folds letter case and slashes on Windows.
fn path_key(path: &Path) -> PostKey {
    PostKey::for_file(path)
}

/// The picture file of a loose post.
fn picture_path(post: &Post) -> &Path {
    post.media.first().map_or(Path::new(""), |media| media.path.as_path())
}

/// True when the extension of `path` is one of `PICTURE_EXTENSIONS`, in any letter case.
fn has_picture_extension(path: &Path) -> bool {
    path.extension().is_some_and(|extension| {
        let lower = extension.to_string_lossy().to_lowercase();
        PICTURE_EXTENSIONS.contains(&lower.as_str())
    })
}

/// True when the extension of `path` is one of `PICTURE_EXTENSIONS` or `VIDEO_EXTENSIONS`: the
/// name promises something this app shows.
fn has_media_extension(path: &Path) -> bool {
    has_picture_extension(path)
        || path.extension().is_some_and(|extension| {
            let lower = extension.to_string_lossy().to_lowercase();
            VIDEO_EXTENSIONS.contains(&lower.as_str())
        })
}

// --- Export or loose ----------------------------------------------------------------------------

/// One export source a request names.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SourceRequest {
    /// An export folder or a manifest file: the post.json to read.
    Folder {
        /// The post.json (or posts.json).
        manifest: PathBuf,
    },
    /// An archive candidate (`archive::is_archive_candidate`); its content decides the rest.
    Archive {
        /// The archive file (absolute).
        archive: PathBuf,
    },
}

impl SourceRequest {
    /// The path that identifies the source: the manifest of a folder, the archive file.
    fn identity(&self) -> &Path {
        match self {
            SourceRequest::Folder { manifest } => manifest,
            SourceRequest::Archive { archive } => archive,
        }
    }
}

/// What a set of paths would open as, decided without decoding any picture.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RequestKind {
    /// Every path is a picture file or a folder of pictures (or cannot be read).
    Loose,
    /// The export sources of the request, in request order (folders of archives expanded; a
    /// source named twice counts once), plus the positions of the request's other paths (they
    /// get `messages::not_opened_beside_export`).
    Exports {
        /// The export folders and archives.
        sources: Vec<SourceRequest>,
        /// The positions of the paths that are not export sources.
        others: Vec<usize>,
    },
}

/// The post.json a path stands for: the path itself when it is a manifest file, or the manifest
/// of the folder it names. `None` for everything else, also for a path that cannot be read.
///
/// A manifest directly in the folder always makes it an export. A manifest one level down does
/// so only when the folder is a mere wrapper, that is when it holds no supported picture of its
/// own (`holds_a_picture`): a Pictures folder with an unpacked export in a subfolder still opens
/// as its pictures.
fn manifest_of(path: &Path) -> Option<PathBuf> {
    let path = absolute(path);
    let metadata = fs::metadata(&path).ok()?;
    if metadata.is_dir() {
        let manifest = manifest::find_manifest(&path, MANIFEST_SEARCH_DEPTH).ok().flatten()?;
        let directly_in_it = manifest.parent().is_some_and(|parent| path_key(parent) == path_key(&path));
        (directly_in_it || !holds_a_picture(&path)).then_some(manifest)
    } else if metadata.is_file() && manifest::is_manifest_name(&display_name(&path)) {
        Some(path)
    } else {
        None
    }
}

/// True when `folder` holds at least one file directly in it that would be shown as a loose
/// picture or video (`loose_post`). Files whose name promises one are looked at first, so the
/// answer usually costs one file; the search stops at the first picture. A folder that cannot be
/// listed holds none; one that is too large to list counts as a picture folder, since it is
/// certainly not a mere wrapper around an export.
fn holds_a_picture(folder: &Path) -> bool {
    let files = match list_folder_files(folder) {
        Ok(files) => files,
        Err(error) => return error.code == OpenErrorCode::TooLarge,
    };
    let options = OpenOptions { default_username: String::new() };
    let (named_like_pictures, others): (Vec<&PathBuf>, Vec<&PathBuf>) =
        files.iter().partition(|file| has_media_extension(file));
    named_like_pictures
        .into_iter()
        .chain(others)
        .any(|file| matches!(loose_post(file, false, &options), LooseFile::Post(_)))
}

/// The archives a folder opens as (web brief 3.1, the clone's "Downloads folder" rule): the
/// archive candidates directly in it, in natural file-name order, when no file directly in it is
/// named like a picture or a video. Decided from the names of one listing; empty for a folder
/// that cannot be listed.
fn archives_in(folder: &Path) -> Vec<PathBuf> {
    let Ok(files) = list_folder_files(folder) else {
        return Vec::new();
    };
    if files.iter().any(|file| has_media_extension(file)) {
        return Vec::new();
    }
    files.into_iter().filter(|file| archive::is_archive_candidate(file)).collect()
}

/// The export sources a path stands for: an archive file, a manifest file or an export folder
/// (`manifest_of`), the archives of a folder without a manifest and without pictures
/// (`archives_in`); `None` for a picture, a folder of pictures and a path that cannot be read.
fn sources_of(path: &Path) -> Option<Vec<SourceRequest>> {
    let path = absolute(path);
    let metadata = fs::metadata(&path).ok()?;
    if metadata.is_file() && archive::is_archive_candidate(&path) {
        return Some(vec![SourceRequest::Archive { archive: path }]);
    }
    if let Some(manifest) = manifest_of(&path) {
        return Some(vec![SourceRequest::Folder { manifest }]);
    }
    if metadata.is_dir() {
        let archives = archives_in(&path);
        if !archives.is_empty() {
            return Some(archives.into_iter().map(|archive| SourceRequest::Archive { archive }).collect());
        }
    }
    None
}

/// Classifies the paths of a request (see `RequestKind`). A path that does not exist or cannot
/// be listed counts as loose; `open_paths` reports it. Only names, sizes and listings are read,
/// apart from the few files `manifest_of` sniffs to tell a wrapper folder from a picture folder.
///
/// The UI uses it for the rule "opening more while something is open": loose files join a loose
/// collection (`add_loose_files`), export sources join an export collection (`add_sources`),
/// anything else replaces the collection (`open_classified`).
pub fn classify_request(paths: &[PathBuf]) -> RequestKind {
    let mut sources: Vec<SourceRequest> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    let mut others: Vec<usize> = Vec::new();
    for (position, path) in paths.iter().enumerate() {
        match sources_of(path) {
            Some(found) => {
                for source in found {
                    // The same export named twice is opened once.
                    if seen.insert(fold_path(source.identity())) {
                        sources.push(source);
                    }
                }
            }
            None => others.push(position),
        }
    }
    if sources.is_empty() {
        RequestKind::Loose
    } else {
        RequestKind::Exports { sources, others }
    }
}

/// Opens the paths of a request as a new collection (it replaces whatever is on screen):
/// `classify_request`, then `open_classified`.
pub fn open_paths(request: &OpenRequest, store: &DescriptionStore, context: &OpenContext) -> Result<Opened, OpenError> {
    if request.paths.is_empty() {
        return Err(OpenError::new(OpenErrorCode::NothingToOpen, messages::NOTHING_TO_OPEN));
    }
    open_classified(request, classify_request(&request.paths), store, context)
}

/// Opens a request that `classify_request` has looked at already as `kind`.
///
/// No path -> `NothingToOpen` (`messages::NOTHING_TO_OPEN`).
///
/// EXPORTS (web brief 3.5): at most `manifest::MAX_SOURCES` sources are read, one after the
/// other in request order (an export folder with `manifest::load_export`, an archive with
/// `archive::open_archive`; the archives of one opening share one `MediaBudget`). Every other
/// path of the request is not opened and gets the warning
/// `messages::not_opened_beside_export(<file name>)`, placed first.
///
/// * A request with exactly ONE source: its error becomes `OpenError` with code `Export` and the
///   same message; its collection has `source = Exports([source])`, `label` = the source's name,
///   its warnings unprefixed and its posts in post.json order.
/// * Several sources: the sources that fail become warnings (`messages::source_failed`), after
///   the note `messages::sources_truncated` when more than `MAX_SOURCES` were named; when every
///   one fails -> `Export` with `messages::no_source_imported`. Two or more that are read are
///   merged (`sources::merge`); one that is read alone is shown as it is. The label is
///   `sources::describe_sources` over every source of the request, as the clone counts them.
///
/// `folder` is the root of the first export folder or the folder of the first archive;
/// descriptions are laid over the posts (`apply_descriptions`); `focus = None`.
///
/// LOOSE, by what the request names (a path that does not exist or cannot be read is skipped
/// with the warning `messages::path_not_opened`, warning code `MissingImage`):
///
/// LOOSE, by what the request names (a path that does not exist or cannot be read is skipped
/// with the warning `messages::path_not_opened`, warning code `MissingImage`):
///
/// * exactly ONE path and it is a file: the candidates are the regular files directly in its
///   folder (`list_folder_files`), plus the picked file itself should the listing miss it; the
///   picked file counts as explicitly named, the others as found. `focus` = the index of the
///   picked file's post, or `None` when it is not a supported picture.
/// * otherwise: every file path is explicitly named; every folder path contributes the regular
///   files directly in it as found files. `focus = None`.
///
/// Each candidate becomes at most one post (`loose_post`). Duplicates of the same file
/// (`PostKey::for_file`) are dropped. Files that are not shown:
///
/// | File | Explicitly named | Found in a folder |
/// |---|---|---|
/// | noise (`manifest::is_noise_path`) and manifest names | silently ignored | silently ignored |
/// | cannot be read | `file_unreadable` | `file_unreadable` only when its extension is in `PICTURE_EXTENSIONS` or `VIDEO_EXTENSIONS`, else silent |
/// | empty (0 bytes) | `file_empty` | `file_empty` only when its extension is in `PICTURE_EXTENSIONS` or `VIDEO_EXTENSIONS`, else silent |
/// | not supported | `file_known_unsupported` or `file_not_supported` | the same, only when its extension is in `PICTURE_EXTENSIONS` or `VIDEO_EXTENSIONS`, else silent |
/// | a picture larger than `manifest::MAX_IMAGE_BYTES` | `file_too_large` | `file_too_large` |
/// | a video larger than `manifest::MAX_VIDEO_BYTES` | `file_too_large` | `file_too_large` |
///
/// (sentences in `messages`, `{file}` = the file name; codes `UnsupportedImage`, or
/// `UnsupportedMedia` for the known-format sentence.) So a folder with documents next to its
/// pictures opens without a warning per document, while a `.jpg` that is not a picture is
/// always reported. A video (MP4, MOV, WebM by content) is a post like a picture (video brief
/// 5.1); a found `.mkv` stays silent like a document, a named one gets the known-format sentence.
///
/// The posts are sorted with `loose_order`, ids are made unique in that order
/// (`text::unique_id`), stored descriptions are applied. The collection: `source = Loose`,
/// `label = collection_label(..)`, `folder` = the folder of the first post, `total_entries =
/// posts.len()`, `source_url = None`, warnings in the order the files were looked at.
///
/// No post at all -> `NoPictures` with `messages::no_pictures_found(<folder name, or the file
/// name when one file was opened>)` and the collected warnings in `OpenError::warnings`; when
/// ONE file was picked and it has a warning of its own, that warning's sentence is the message
/// instead (it says why: a video, an empty file, not a supported image ...). When the picked
/// file is not shown but other pictures of its folder are, its sentence is `Opened::notice`.
/// A folder named by the request with more than `manifest::MAX_SOURCE_ENTRIES` entries ->
/// `TooLarge`, `messages::source_too_many_files(<folder name>)`.
///
/// Details the table leaves open: the warning codes have no "read failed", which is why a path
/// that cannot be opened carries `MissingImage`; when not a single path of the request can be
/// opened, the error is `ReadFailed` with the sentence of the first such path; for a request of
/// several paths without any picture, the name in `no_pictures_found` is the label of the
/// folders that were looked at (the folder name, or "{n} folders"). When the folder of ONE
/// picked file cannot be listed, the file opens on its own, without a warning about the folder;
/// when that folder is too large to list, the file opens on its own with the warning
/// `messages::neighbours_not_opened(<folder name>)`.
pub fn open_classified(request: &OpenRequest, kind: RequestKind, store: &DescriptionStore, context: &OpenContext) -> Result<Opened, OpenError> {
    if request.paths.is_empty() {
        return Err(OpenError::new(OpenErrorCode::NothingToOpen, messages::NOTHING_TO_OPEN));
    }
    match kind {
        RequestKind::Exports { sources, others } => open_exports(&request.paths, sources, &others, store, context),
        RequestKind::Loose => open_loose(request, store),
    }
}

/// The warnings of the paths of a request that are not opened beside its exports.
fn beside_exports(paths: &[PathBuf], others: &[usize]) -> Vec<Warning> {
    others
        .iter()
        .filter_map(|position| paths.get(*position))
        .map(|path| {
            let path = absolute(path);
            Warning::new(WarningCode::InvalidEntry, messages::not_opened_beside_export(&display_name(&path))).with_path(path.display().to_string())
        })
        .collect()
}

/// What reading the export sources of one request gave.
struct ReadSources {
    /// The sources that were read, in request order.
    loaded: Vec<LoadedSource>,
    /// The kinds of every source of the request that was tried (for the label).
    kinds: Vec<ExportKind>,
    /// Name and sentence of each source that failed.
    failures: Vec<(String, String)>,
    /// How many sources the request named before `MAX_SOURCES` cut it.
    named: usize,
    leases: Vec<ArchiveLease>,
    cache_written: bool,
}

impl ReadSources {
    /// The notes of a request of several sources (multi.ts:145-153): the cut at `MAX_SOURCES`,
    /// then one per failed source.
    fn notes(&self) -> Vec<Warning> {
        let mut notes = Vec::new();
        if self.named > MAX_SOURCES {
            notes.push(Warning::new(WarningCode::Truncated, messages::sources_truncated(self.named)));
        }
        for (name, message) in &self.failures {
            notes.push(Warning::new(WarningCode::InvalidEntry, messages::source_failed(name, message)));
        }
        notes
    }
}

/// Reads the sources of a request, one after the other (multi.ts `importSources`).
fn read_sources(sources: Vec<SourceRequest>, context: &OpenContext) -> ReadSources {
    let named = sources.len();
    let mut read = ReadSources { loaded: Vec::new(), kinds: Vec::new(), failures: Vec::new(), named, leases: Vec::new(), cache_written: false };
    let mut budget = MediaBudget::default();
    for source in sources.into_iter().take(MAX_SOURCES) {
        let (kind, name) = source_kind_and_name(&source, context);
        read.kinds.push(kind);
        match load_source(&source, kind, name.clone(), context, &mut budget) {
            Ok((loaded, lease, extracted)) => {
                read.loaded.push(loaded);
                read.leases.extend(lease);
                read.cache_written |= extracted;
            }
            Err(message) => {
                log::warn!("{name}: {message}");
                read.failures.push((name, message));
            }
        }
    }
    read
}

/// The kind and the name of a source: the browser imports folder is `WebLibrary` with
/// `messages::BROWSER_IMPORTS_NAME`; another folder is named after its root folder, an archive
/// after its file.
fn source_kind_and_name(source: &SourceRequest, context: &OpenContext) -> (ExportKind, String) {
    match source {
        SourceRequest::Archive { archive } => (ExportKind::Archive, display_name(archive)),
        SourceRequest::Folder { manifest } => {
            let root = folder_of(manifest);
            if context.library_dir.as_deref().is_some_and(|library| fold_path(library) == fold_path(&root)) {
                (ExportKind::WebLibrary, messages::BROWSER_IMPORTS_NAME.to_owned())
            } else {
                (ExportKind::Folder, display_name(&root))
            }
        }
    }
}

/// Reads one export source. `Err` is the sentence of its failure.
fn load_source(
    source: &SourceRequest,
    kind: ExportKind,
    name: String,
    context: &OpenContext,
    budget: &mut MediaBudget,
) -> Result<(LoadedSource, Option<ArchiveLease>, bool), String> {
    let opened_at = text::now_ms();
    let describe = |path: PathBuf, manifest: PathBuf, loaded: &manifest::LoadedExport| ExportSource {
        kind,
        path,
        manifest,
        name: name.clone(),
        opened_at,
        exported_at: loaded.exported_at,
        source_url: loaded.source_url.clone(),
        total_entries: loaded.total_entries,
        shown: loaded.posts.len(),
        replaced: 0,
    };
    match source {
        SourceRequest::Folder { manifest } => {
            let loaded = manifest::load_export(manifest).map_err(|error| error.message)?;
            let source = describe(folder_of(manifest), manifest.clone(), &loaded);
            Ok((LoadedSource { source, loaded }, None, false))
        }
        SourceRequest::Archive { archive } => {
            let opened = archive::open_archive(archive, &context.archive_cache, budget).map_err(|error| error.message)?;
            let source = describe(absolute(archive), opened.manifest.clone(), &opened.loaded);
            Ok((LoadedSource { source, loaded: opened.loaded }, Some(opened.lease), opened.extracted))
        }
    }
}

/// The folder to offer in the next dialog for a source: the root of a folder, the folder of an
/// archive.
fn folder_of_source(source: &ExportSource) -> PathBuf {
    match source.kind {
        ExportKind::Archive => folder_of(&source.path),
        ExportKind::Folder | ExportKind::WebLibrary => source.path.clone(),
    }
}

/// A collection of exactly one source, as it was read: no prefix, post.json order.
fn single_source_collection(loaded: LoadedSource, mut warnings: Vec<Warning>) -> Collection {
    let LoadedSource { source, loaded } = loaded;
    warnings.extend(loaded.warnings);
    Collection {
        posts: loaded.posts,
        label: source.name.clone(),
        folder: Some(folder_of_source(&source)),
        warnings,
        source_url: loaded.source_url,
        total_entries: loaded.total_entries,
        source: CollectionSource::Exports(vec![source]),
    }
}

/// A collection of what `sources::merge` produced.
fn merged_collection(merged: sources::Merged, label: String, mut warnings: Vec<Warning>) -> Collection {
    warnings.extend(merged.warnings);
    Collection {
        posts: merged.posts,
        label,
        folder: merged.sources.first().map(folder_of_source),
        warnings,
        source_url: merged.source_url,
        total_entries: merged.total_entries,
        source: CollectionSource::Exports(merged.sources),
    }
}

/// The EXPORTS case of `open_classified`.
fn open_exports(paths: &[PathBuf], sources: Vec<SourceRequest>, others: &[usize], store: &DescriptionStore, context: &OpenContext) -> Result<Opened, OpenError> {
    let mut warnings = beside_exports(paths, others);
    let single_request = sources.len() == 1;
    let mut read = read_sources(sources, context);
    if read.loaded.is_empty() {
        let message = if single_request {
            read.failures.first().map(|(_, message)| message.clone()).unwrap_or_default()
        } else {
            messages::no_source_imported(read.kinds.len(), &messages::failure_reasons(&read.failures))
        };
        return Err(OpenError { code: OpenErrorCode::Export, message, warnings });
    }
    // The banner names what it shows: the archives and folders whose posts were read ("Showing
    // 4 of 7 posts from export-problems.zip" when two of three could not be imported), never the
    // ones the warnings say could not be imported.
    let label = if read.loaded.len() == 1 { None } else { Some(sources::describe_sources(read.loaded.iter().map(|loaded| loaded.source.kind))) };
    warnings.extend(read.notes());
    let mut loaded = std::mem::take(&mut read.loaded);
    let mut collection = if loaded.len() == 1 {
        let mut collection = single_source_collection(loaded.remove(0), warnings);
        if let Some(label) = label {
            collection.label = label;
        }
        collection
    } else {
        let merged = sources::merge(loaded.into_iter().map(MergeInput::from_loaded).collect());
        merged_collection(merged, label.unwrap_or_default(), warnings)
    };
    apply_descriptions(&mut collection, store);
    Ok(Opened { collection, focus: None, notice: None, leases: read.leases, cache_written: read.cache_written })
}

/// What `add_sources` did.
#[derive(Debug)]
pub struct AddedSources {
    /// How many posts of the new sources are shown.
    pub added: usize,
    /// The leases of the archives that were read.
    pub leases: Vec<ArchiveLease>,
    /// True when an archive was unpacked into the cache.
    pub cache_written: bool,
}

/// Adds export folders and archives to the export collection on screen (web brief 3.5; the
/// clone's "add to the import on screen", App.tsx:483-486). The collection counts as ONE earlier
/// result (`sources::MergeInput` with its label and, when it was merged before, its prefixed
/// warnings), the new sources follow in request order, and `sources::merge` runs over them;
/// nothing on screen is read again. A new source that is already open first takes its posts and
/// warnings out of the collection and is then merged at the end, so that it wins (this is how
/// the browser imports folder is refreshed). `others` are paths of the request that are not
/// export sources (`not_opened_beside_export`).
///
/// `Err` leaves the collection as it was: it is not an export (`CANNOT_ADD_TO_EXPORT`'s
/// counterpart for loose pictures is `add_loose_files`), or every new source failed (one source:
/// its own sentence; several: `messages::no_source_imported`).
pub fn add_sources(
    collection: &mut Collection,
    paths: &[PathBuf],
    sources: Vec<SourceRequest>,
    others: &[usize],
    store: &DescriptionStore,
    context: &OpenContext,
) -> Result<AddedSources, OpenError> {
    if !collection.is_export() {
        return Err(OpenError::new(OpenErrorCode::Export, messages::CANNOT_ADD_EXPORTS_TO_PICTURES));
    }
    let single_request = sources.len() == 1;
    let mut read = read_sources(sources, context);
    if read.loaded.is_empty() {
        let message = if single_request {
            read.failures.first().map(|(_, message)| message.clone()).unwrap_or_default()
        } else {
            messages::no_source_imported(read.kinds.len(), &messages::failure_reasons(&read.failures))
        };
        return Err(OpenError { code: OpenErrorCode::Export, message, warnings: beside_exports(paths, others) });
    }
    let notes: Vec<Warning> = beside_exports(paths, others).into_iter().chain(read.notes()).collect();
    let fresh = std::mem::take(&mut read.loaded);
    let added = merge_into(collection, fresh, notes, read.kinds.clone(), single_request, store);
    Ok(AddedSources { added, leases: read.leases, cache_written: read.cache_written })
}

/// A collection of exactly one source that was read already (the browser imports folder after a
/// commit, when nothing else is open), with the stored descriptions laid over it.
pub fn collection_of_source(loaded: LoadedSource, store: &DescriptionStore) -> Collection {
    let mut collection = single_source_collection(loaded, Vec::new());
    apply_descriptions(&mut collection, store);
    collection
}

/// Adds (or, when it is open already, refreshes) one source that was read already to the export
/// collection on screen, exactly as `add_sources` does for the sources it reads (web brief 4.10:
/// the browser imports folder after a commit). Returns how many of its posts are shown; nothing
/// happens to a collection of loose pictures (0).
pub fn add_loaded_source(collection: &mut Collection, loaded: LoadedSource, store: &DescriptionStore) -> usize {
    if !collection.is_export() {
        return 0;
    }
    let kind = loaded.source.kind;
    merge_into(collection, vec![loaded], Vec::new(), vec![kind], true, store)
}

/// Takes the source with `identity` (`ExportSource::identity`) out of the export collection on
/// screen: its posts and its warnings go. Returns false when it is not a source of it, or when
/// it is the only one (the caller closes the collection then).
pub fn remove_source(collection: &mut Collection, identity: &Path, store: &DescriptionStore) -> bool {
    let folded = fold_path(identity);
    let sources = collection.sources().to_vec();
    let Some(position) = sources.iter().position(|source| fold_path(source.identity()) == folded) else {
        return false;
    };
    if sources.len() == 1 {
        return false;
    }
    let mut remaining = sources;
    let gone = remaining.remove(position);
    collection.posts.retain(|post| !gone.owns(post));
    collection.warnings.retain(|warning| warning.source.as_deref() != Some(folded.as_str()));
    collection.total_entries = collection.total_entries.saturating_sub(gone.total_entries.saturating_sub(gone.replaced));
    collection.label = if remaining.len() == 1 { remaining[0].name.clone() } else { sources::describe_sources(remaining.iter().map(|source| source.kind)) };
    collection.folder = remaining.first().map(folder_of_source);
    collection.source = CollectionSource::Exports(remaining);
    apply_descriptions(collection, store);
    true
}

/// The merge of `add_sources` and `add_loaded_source`: the collection on screen counts as one
/// earlier result, the new sources follow; a new source that is open already first takes its
/// posts and warnings out. Returns how many posts of the new sources are shown.
fn merge_into(collection: &mut Collection, fresh: Vec<LoadedSource>, notes: Vec<Warning>, new_kinds: Vec<ExportKind>, single_request: bool, store: &DescriptionStore) -> usize {
    // A source that is open already is read again: its old posts and warnings go first.
    let already_prefixed = collection.sources().len() > 1;
    let mut current_sources: Vec<ExportSource> = collection.sources().to_vec();
    let mut current_posts = std::mem::take(&mut collection.posts);
    let mut current_warnings = std::mem::take(&mut collection.warnings);
    for loaded in &fresh {
        let identity = fold_path(loaded.source.identity());
        if let Some(position) = current_sources.iter().position(|source| fold_path(source.identity()) == identity) {
            let gone = current_sources.remove(position);
            current_posts.retain(|post| !gone.owns(post));
            if already_prefixed {
                current_warnings.retain(|warning| warning.source.as_deref() != Some(identity.as_str()));
            } else {
                current_warnings.clear();
            }
        }
    }

    let new_keys: HashSet<PostKey> = fresh.iter().flat_map(|loaded| loaded.loaded.posts.iter().map(|post| post.key.clone())).collect();
    let mut fresh = fresh;

    let mut updated = if current_sources.is_empty() && fresh.len() == 1 {
        // Only the new source is left: it is shown as it would be opened on its own.
        let mut collection = single_source_collection(fresh.remove(0), notes);
        if !single_request {
            collection.label = sources::describe_sources(new_kinds);
        }
        collection
    } else {
        let current_count = if current_sources.is_empty() { 0 } else { current_warnings.len() };
        let mut inputs: Vec<MergeInput> = Vec::new();
        if !current_sources.is_empty() {
            inputs.push(MergeInput { name: collection.label.clone(), posts: current_posts, warnings: current_warnings, prefixed: already_prefixed, sources: current_sources });
        }
        inputs.extend(fresh.into_iter().map(MergeInput::from_loaded));
        let merged = sources::merge(inputs);
        let label = sources::describe_sources(merged.sources.iter().map(|source| source.kind));
        let mut collection = merged_collection(merged, label, Vec::new());
        // The notes of the new request come right after what was shown already (the clone's
        // merge of the import on screen with the new import, whose notes lead its warnings).
        let tail = collection.warnings.split_off(current_count.min(collection.warnings.len()));
        collection.warnings.extend(notes);
        collection.warnings.extend(tail);
        collection
    };
    apply_descriptions(&mut updated, store);
    let added = updated.posts.iter().filter(|post| new_keys.contains(&post.key)).count();
    *collection = updated;
    added
}

// --- Loose pictures -----------------------------------------------------------------------------

/// One file that may become a post.
struct Candidate {
    path: PathBuf,
    key: PostKey,
    /// True when the user named the file; false when it was merely found in a folder.
    explicit: bool,
}

/// The files a request names or contains, each once, in the order they will be looked at.
#[derive(Default)]
struct Gathered {
    candidates: Vec<Candidate>,
    positions: HashMap<PostKey, usize>,
    /// Warnings about paths of the request that could not be opened.
    warnings: Vec<Warning>,
    /// How many paths of the request could not be opened.
    failed_paths: usize,
    /// The keys of the files the request names (not those found in its folders).
    named_files: Vec<PostKey>,
}

impl Gathered {
    /// Adds a file unless it is already there; a file both named and found counts as named.
    fn add(&mut self, path: PathBuf, explicit: bool) {
        let key = path_key(&path);
        if explicit {
            self.named_files.push(key.clone());
        }
        match self.positions.get(&key) {
            Some(position) => self.candidates[*position].explicit |= explicit,
            None => {
                self.positions.insert(key.clone(), self.candidates.len());
                self.candidates.push(Candidate { path, key, explicit });
            }
        }
    }

    /// Notes a path of the request that does not exist or cannot be listed.
    fn fail(&mut self, path: &Path, message: String) {
        self.failed_paths += 1;
        self.warnings.push(Warning::new(WarningCode::MissingImage, message).with_path(path.display().to_string()));
    }

    /// The files directly in `folder`, as found files. A folder that is too large is an error;
    /// one that cannot be listed is a warning.
    fn add_folder(&mut self, folder: &Path) -> Result<(), OpenError> {
        match list_folder_files(folder) {
            Ok(files) => {
                for file in files {
                    self.add(file, false);
                }
                Ok(())
            }
            Err(error) if error.code == OpenErrorCode::TooLarge => Err(error),
            Err(error) => {
                self.fail(folder, error.message);
                Ok(())
            }
        }
    }
}

/// The one file a request picks, when it names exactly one path and that is a file.
fn single_picked_file(paths: &[PathBuf]) -> Option<PathBuf> {
    let [only] = paths else { return None };
    let path = absolute(only);
    fs::metadata(&path).is_ok_and(|metadata| metadata.is_file()).then_some(path)
}

/// One picked picture: its siblings are shown with it. The picked picture opens even when its
/// folder cannot be listed (no warning: the user asked for the file, not for the folder) or
/// holds more than `max_entries` entries (a warning says why the neighbours are missing).
fn gather_around(picked: &Path, max_entries: usize) -> Gathered {
    let mut gathered = Gathered::default();
    let folder = folder_of(picked);
    match list_folder_files_up_to(&folder, max_entries) {
        Ok(files) => {
            for file in files {
                gathered.add(file, false);
            }
        }
        Err(error) if error.code == OpenErrorCode::TooLarge => {
            let folder = absolute(&folder);
            gathered.warnings.push(
                Warning::new(WarningCode::Truncated, messages::neighbours_not_opened(&display_name(&folder)))
                    .with_path(folder.display().to_string()),
            );
        }
        Err(_) => {}
    }
    gathered.add(picked.to_path_buf(), true);
    gathered
}

/// The files and folders of a request: files are named, folders contribute what lies in them.
fn gather_named(paths: &[PathBuf]) -> Result<Gathered, OpenError> {
    let mut gathered = Gathered::default();
    for path in paths {
        let path = absolute(path);
        match fs::metadata(&path) {
            Ok(metadata) if metadata.is_dir() => {
                gathered.add_folder(&path)?;
            }
            Ok(_) => gathered.add(path, true),
            Err(error) => {
                let message = messages::path_not_opened(&path.display().to_string(), &error.to_string());
                gathered.fail(&path, message);
            }
        }
    }
    Ok(gathered)
}

/// Looks at every candidate (`loose_post`), a few at a time, and returns the pictures and the
/// warnings in candidate order.
fn inspect(candidates: &[Candidate], options: &OpenOptions) -> (Vec<Post>, Vec<Warning>) {
    let look = |part: &[Candidate]| -> Vec<LooseFile> {
        part.iter().map(|candidate| loose_post(&candidate.path, candidate.explicit, options)).collect()
    };
    let workers = (candidates.len() / MIN_FILES_PER_WORKER).clamp(1, IMAGE_CONCURRENCY);
    let looked: Vec<LooseFile> = if workers == 1 {
        look(candidates)
    } else {
        // Opening a file is mostly waiting for the disk (and for a virus scanner), so a few
        // threads shorten the wait even on a small machine.
        let part_size = candidates.len().div_ceil(workers);
        std::thread::scope(|scope| {
            let running: Vec<_> = candidates.chunks(part_size).map(|part| scope.spawn(move || look(part))).collect();
            running.into_iter().flat_map(|worker| worker.join().unwrap_or_default()).collect()
        })
    };

    let mut posts: Vec<Post> = Vec::new();
    let mut warnings: Vec<Warning> = Vec::new();
    for file in looked {
        match file {
            LooseFile::Post(post) => posts.push(*post),
            LooseFile::Skipped(warning) => warnings.push(warning),
            LooseFile::Ignored => {}
        }
    }
    (posts, warnings)
}

/// Sorts loose posts into display order.
fn sort_loose(posts: &mut [Post]) {
    posts.sort_by(|left, right| loose_order(picture_path(left), picture_path(right)));
}

/// Lays the remembered descriptions over posts: a post with an entry in the store shows the
/// stored caption, every other post its original one (`original_caption`). An entry of a loose
/// picture that belongs to an earlier file of the same name (`belongs_to_earlier_file`) is not
/// applied; a warning per such post is returned and the entry stays in the store.
fn describe(posts: &mut [Post], store: &DescriptionStore) -> Vec<Warning> {
    let mut warnings = Vec::new();
    for post in posts {
        match store.get(&post.key) {
            Some(entry) if belongs_to_earlier_file(entry, post) => {
                post.caption = post.original_caption.clone();
                let file = post.cover().map(|cover| cover.path.clone()).unwrap_or_default();
                warnings.push(
                    Warning::new(WarningCode::InvalidEntry, messages::description_of_earlier_file(&display_name(&file)))
                        .with_path(file.display().to_string()),
                );
            }
            Some(entry) => post.caption = entry.caption.clone(),
            None => post.caption = post.original_caption.clone(),
        }
    }
    warnings
}

/// True when the stored description of a loose picture was saved for another file of the same
/// path: both the size and the modified time recorded with it differ from the file now. (Cameras
/// count from IMG_0001 again, so a name alone says little.) An entry without these values, an
/// export post, and a file of which only one of the two changed keep their description.
fn belongs_to_earlier_file(entry: &crate::store::DescriptionEntry, post: &Post) -> bool {
    if entry.post_id.is_some() {
        return false;
    }
    let (Some(cover), Some(saved_bytes), Some(saved_modified), Some(modified)) =
        (post.cover(), entry.file_bytes, entry.file_modified, post.taken_at)
    else {
        return false;
    };
    saved_bytes != cover.bytes && saved_modified != modified
}

/// The banner name of a set of folders: the name of the only one, else "{n} folders".
fn folders_label<'a>(folders: impl Iterator<Item = &'a Path>) -> String {
    let mut distinct: HashSet<PostKey> = HashSet::new();
    let mut first: Option<&Path> = None;
    for folder in folders {
        if distinct.insert(path_key(folder)) && first.is_none() {
            first = Some(folder);
        }
    }
    match (first, distinct.len()) {
        (None, _) => String::new(),
        (Some(only), 1) => display_name(&absolute(only)),
        (Some(_), count) => format!("{count} folders"),
    }
}

fn open_loose(request: &OpenRequest, store: &DescriptionStore) -> Result<Opened, OpenError> {
    open_loose_up_to(request, store, MAX_SOURCE_ENTRIES)
}

/// `open_loose` with the largest folder it lists as a parameter, so that the tests can reach
/// the limit without a hundred thousand files.
fn open_loose_up_to(request: &OpenRequest, store: &DescriptionStore, max_entries: usize) -> Result<Opened, OpenError> {
    let picked = single_picked_file(&request.paths);
    let gathered = match &picked {
        Some(picked) => gather_around(picked, max_entries),
        None => gather_named(&request.paths)?,
    };
    let (mut posts, file_warnings) = inspect(&gathered.candidates, &request.options);
    let mut warnings = gathered.warnings;
    warnings.extend(file_warnings);
    // The sentence of the one picked file, when it is not shown: the user asked for that file,
    // so the reason is said at once and not only behind the warnings button.
    let picked_warning = picked.as_ref().and_then(|picked| {
        let shown = picked.display().to_string();
        warnings.iter().find(|warning| warning.path.as_deref() == Some(shown.as_str())).map(|warning| warning.message.clone())
    });

    if posts.is_empty() {
        if gathered.failed_paths == request.paths.len() {
            // Nothing of the request could even be opened: say that, not "no pictures".
            let message = warnings.first().map(|warning| warning.message.clone()).unwrap_or_default();
            return Err(OpenError { code: OpenErrorCode::ReadFailed, message, warnings });
        }
        let name = match request.paths.as_slice() {
            [only] => display_name(&absolute(only)),
            several => {
                let looked_at: Vec<PathBuf> = several
                    .iter()
                    .map(|path| absolute(path))
                    .map(|path| if path.is_dir() { path } else { folder_of(&path) })
                    .collect();
                folders_label(looked_at.iter().map(PathBuf::as_path))
            }
        };
        let message = picked_warning.unwrap_or_else(|| messages::no_pictures_found(&name));
        return Err(OpenError { code: OpenErrorCode::NoPictures, message, warnings });
    }

    sort_loose(&mut posts);
    let mut used_ids: HashSet<String> = HashSet::with_capacity(posts.len());
    for post in &mut posts {
        post.id = text::unique_id(&post.id, &mut used_ids);
    }
    warnings.extend(describe(&mut posts, store));

    let focus = picked.and_then(|picked| {
        let key = path_key(&picked);
        posts.iter().position(|post| post.key == key)
    });
    let notice = if focus.is_none() { picked_warning } else { None };
    let pictures: Vec<&Path> = posts.iter().map(picture_path).collect();
    let label = collection_label(&pictures);
    let folder = pictures.first().map(|first| folder_of(first));
    let total_entries = posts.len();
    let collection =
        Collection { posts, source: CollectionSource::Loose, label, folder, warnings, source_url: None, total_entries };
    Ok(Opened { collection, focus, notice, leases: Vec::new(), cache_written: false })
}

/// What `add_loose_files` did.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AddOutcome {
    /// How many posts were added.
    pub added: usize,
    /// The index (after re-sorting) of the first added post in display order; when nothing was
    /// added but exactly one file was named and it is already shown, the index of that post.
    pub focus: Option<usize>,
    /// True when the request named exactly one file: the picture the user picked, which the
    /// viewer shows (design brief: one picture file opens the viewer on it).
    pub one_file: bool,
}

/// Adds further loose pictures to a loose collection (design brief: "dropping or opening further
/// loose files while a loose collection is open ADDS them (no duplicates by path)").
///
/// `paths` are handled as in the "otherwise" case of `open_paths`: files are explicitly named,
/// folders contribute their direct files. A file that is already in the collection (same
/// `PostKey`) is not added again. New posts get ids that are unique among all posts (existing
/// ids are never changed), stored descriptions are applied to them, and the whole collection is
/// sorted again with `loose_order`; `label` and `total_entries` are recomputed and new warnings
/// are appended to `collection.warnings`.
///
/// `Err` (the collection is untouched): the collection is an export
/// (`messages::CANNOT_ADD_TO_EXPORT`, code `Export`), or a folder is too large. Adding nothing
/// is not an error: `added` is 0 and the warnings say why.
pub fn add_loose_files(
    collection: &mut Collection,
    paths: &[PathBuf],
    options: &OpenOptions,
    store: &DescriptionStore,
) -> Result<AddOutcome, OpenError> {
    if collection.is_export() {
        return Err(OpenError::new(OpenErrorCode::Export, messages::CANNOT_ADD_TO_EXPORT));
    }
    let mut gathered = gather_named(paths)?;
    let shown: HashSet<&PostKey> = collection.posts.iter().map(|post| &post.key).collect();
    gathered.candidates.retain(|candidate| !shown.contains(&candidate.key));

    let (mut new_posts, file_warnings) = inspect(&gathered.candidates, options);
    sort_loose(&mut new_posts);
    let mut used_ids: HashSet<String> = collection.posts.iter().map(|post| post.id.clone()).collect();
    for post in &mut new_posts {
        post.id = text::unique_id(&post.id, &mut used_ids);
    }
    let description_warnings = describe(&mut new_posts, store);

    let added_keys: HashSet<PostKey> = new_posts.iter().map(|post| post.key.clone()).collect();
    collection.posts.append(&mut new_posts);
    sort_loose(&mut collection.posts);
    collection.warnings.extend(gathered.warnings);
    collection.warnings.extend(file_warnings);
    collection.warnings.extend(description_warnings);
    collection.total_entries = collection.posts.len();
    let pictures: Vec<&Path> = collection.posts.iter().map(picture_path).collect();
    collection.label = collection_label(&pictures);
    if collection.folder.is_none() {
        collection.folder = pictures.first().map(|first| folder_of(first));
    }

    let one_file = paths.len() == 1 && gathered.named_files.len() == 1;
    let focus = if added_keys.is_empty() {
        match gathered.named_files.as_slice() {
            [only] => collection.index_of(only),
            _ => None,
        }
    } else {
        collection.posts.iter().position(|post| added_keys.contains(&post.key))
    };
    Ok(AddOutcome { added: added_keys.len(), focus, one_file })
}

/// The regular files directly in `folder` (no recursion, folders and noise names left out), in
/// `text::natural_cmp` order of their file names. More than `manifest::MAX_SOURCE_ENTRIES`
/// directory entries -> `TooLarge`; a folder that cannot be listed -> `ReadFailed` with
/// `messages::path_not_opened`.
pub fn list_folder_files(folder: &Path) -> Result<Vec<PathBuf>, OpenError> {
    list_folder_files_up_to(folder, MAX_SOURCE_ENTRIES)
}

/// `list_folder_files` with the largest number of entries as a parameter.
fn list_folder_files_up_to(folder: &Path, max_entries: usize) -> Result<Vec<PathBuf>, OpenError> {
    let folder = absolute(folder);
    let listing = fs::read_dir(&folder).map_err(|error| {
        OpenError::new(
            OpenErrorCode::ReadFailed,
            messages::path_not_opened(&folder.display().to_string(), &error.to_string()),
        )
    })?;
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    for (position, entry) in listing.enumerate() {
        if position >= max_entries {
            return Err(OpenError::new(OpenErrorCode::TooLarge, messages::source_too_many_files(&display_name(&folder))));
        }
        let Ok(entry) = entry else { continue };
        let Ok(file_type) = entry.file_type() else { continue };
        // A link to a picture is a picture of this folder; the user put it there.
        let is_file = file_type.is_file()
            || (file_type.is_symlink() && fs::metadata(entry.path()).is_ok_and(|target| target.is_file()));
        if !is_file {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if manifest::is_noise_path(&name) {
            continue;
        }
        files.push((name, entry.path()));
    }
    files.sort_by(|left, right| text::natural_cmp(&left.0, &right.0));
    Ok(files.into_iter().map(|(_, path)| path).collect())
}

/// What looking at one loose file gave.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LooseFile {
    /// A supported picture or video, as a post (boxed: a post is much larger than the other
    /// variants).
    Post(Box<crate::model::Post>),
    /// Not shown, and the user is told why.
    Skipped(crate::model::Warning),
    /// Not shown, silently (see the table of `open_paths`).
    Ignored,
}

/// Looks at one loose file and builds its post (design brief "Loose-file post defaults";
/// spec-viewer 14.2). `explicit` says whether the user named the file (see the table of
/// `open_paths`). The post:
///
/// * `key = PostKey::for_file(path)`; `media` = one `MediaItem` with the sniffed kind (`Image`
///   or `Video`), the absolute `path`, `manifest_path` = the file name, `bytes` = the file size,
///   no poster (the poster of a loose video is made for the screen and the downloads by
///   `poster::PosterCache`, never kept in the model);
/// * `id` = `text::clean_id(file stem)`, else `text::derived_file_id(file name)`; the caller
///   makes it unique;
/// * `user = User::named(options.default_username)`; no permalink, profile link or avatar;
/// * `caption` and `original_caption` are "" (the caller applies stored descriptions);
/// * `comment_count = 0`, `like_count = None`, no comments, `media_type = Image` (`Video` for a
///   video), no alt, no location;
/// * `taken_at` = the file's modified time when its UTC year is 1971..=9999 (the range of
///   `text::parse_timestamp_text`), else `None`;
/// * `context = Some(file name)`: the header sub-line, which is not exported.
///
/// The file is opened once: its size and modified time come from the open handle, and only its
/// first `sniff::SNIFF_BYTES` bytes are read.
pub fn loose_post(path: &Path, explicit: bool, options: &OpenOptions) -> LooseFile {
    let path = absolute(path);
    let name = display_name(&path);
    if manifest::is_noise_path(&name) || manifest::is_manifest_name(&name) {
        return LooseFile::Ignored;
    }
    // A file the user named is always answered for; one that was merely found only when its
    // name says it should be a picture or a video.
    let is_expected = explicit || has_media_extension(&path);
    let skipped = |code: WarningCode, message: String| {
        LooseFile::Skipped(Warning::new(code, message).with_path(path.display().to_string()))
    };
    // A locked database or pagefile.sys beside the pictures is none of the user's concern.
    let unreadable = |error: std::io::Error| {
        if is_expected {
            skipped(WarningCode::UnsupportedImage, messages::file_unreadable(&name, &error.to_string()))
        } else {
            LooseFile::Ignored
        }
    };

    let file = match File::open(&path) {
        Ok(file) => file,
        Err(error) => return unreadable(error),
    };
    let metadata = match file.metadata() {
        Ok(metadata) => metadata,
        Err(error) => return unreadable(error),
    };
    let head = match sniff::read_head_of(&file) {
        Ok(head) => head,
        Err(error) => return unreadable(error),
    };
    if head.is_empty() {
        return if is_expected {
            skipped(WarningCode::UnsupportedImage, messages::file_empty(&name))
        } else {
            LooseFile::Ignored
        };
    }

    let kind = match sniff::sniff_media(&head).map(|sniffed| sniffed.kind) {
        Some(kind) => kind,
        None if is_expected => {
            return match sniff::describe_unsupported_media(&head) {
                Some(known) => skipped(WarningCode::UnsupportedMedia, messages::file_known_unsupported(&name, known)),
                None => skipped(WarningCode::UnsupportedImage, messages::file_not_supported(&name)),
            };
        }
        None => return LooseFile::Ignored,
    };
    let limit = if kind == MediaKind::Video { MAX_VIDEO_BYTES } else { MAX_IMAGE_BYTES };
    if metadata.len() > limit {
        // The validator of the export format rejects such a file, so it never becomes a post.
        let code = if kind == MediaKind::Video { WarningCode::UnsupportedMedia } else { WarningCode::UnsupportedImage };
        return skipped(code, messages::file_too_large(&name, limit));
    }

    let stem = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
    let taken_at = metadata
        .modified()
        .ok()
        .map(text::epoch_ms_from_system_time)
        .filter(|modified| TAKEN_AT_YEARS.contains(&text::civil_from_epoch_ms(*modified).year));
    LooseFile::Post(Box::new(Post {
        key: PostKey::for_file(&path),
        id: text::clean_id(&stem).unwrap_or_else(|| text::derived_file_id(&name)),
        permalink: None,
        media: vec![MediaItem {
            kind,
            path,
            manifest_path: name.clone(),
            poster: None,
            alt: None,
            bytes: metadata.len(),
        }],
        user: User::named(options.default_username.as_str()),
        caption: String::new(),
        original_caption: String::new(),
        comment_count: 0,
        like_count: None,
        taken_at,
        media_type: if kind == MediaKind::Video { MediaType::Video } else { MediaType::Image },
        alt: None,
        location: None,
        context: Some(name),
        comments: Vec::new(),
        origin: PostOrigin::File,
    }))
}

/// The display order of two loose pictures: `text::natural_cmp` of the file names, then
/// `text::natural_cmp` of the full paths (pictures with the same name from different folders).
pub fn loose_order(left: &Path, right: &Path) -> std::cmp::Ordering {
    let name = |path: &Path| path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    text::natural_cmp(&name(left), &name(right))
        .then_with(|| text::natural_cmp(&left.to_string_lossy(), &right.to_string_lossy()))
}

/// The name the banner shows for loose pictures: the name of their folder when all lie in one
/// folder (the drive, such as "C:\", for a root folder), else "{n} folders" with the number of
/// distinct folders. An empty list gives "".
pub fn collection_label(picture_paths: &[&Path]) -> String {
    folders_label(picture_paths.iter().map(|path| path.parent().unwrap_or(Path::new(""))))
}

/// Lays the remembered descriptions over the posts of a collection: for every post whose key
/// has an entry in the store, `caption` becomes the stored caption (`original_caption` is not
/// touched); every other post gets its `original_caption` back, so that a description removed
/// meanwhile is gone from the screen too. An entry of a loose picture that belongs to an earlier
/// file of the same name is not applied (see `open_paths`); the warning about it was given when
/// the picture was opened, so none is added here.
pub fn apply_descriptions(collection: &mut Collection, store: &DescriptionStore) {
    let _ = describe(&mut collection.posts, store);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cmp::Ordering;

    /// A folder of its own below the system's temporary folder, removed when dropped.
    struct TempFolder(PathBuf);

    impl TempFolder {
        fn new(label: &str) -> TempFolder {
            static COUNTER: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
            let unique = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let folder =
                std::env::temp_dir().join(format!("instagram-core-catalog-{label}-{}-{unique}", std::process::id()));
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

    fn options() -> OpenOptions {
        OpenOptions { default_username: "tester".to_owned() }
    }

    /// The warning sentence of a file that is not shown; `None` for a picture or a silent skip.
    fn skip_message(path: &Path, explicit: bool) -> Option<String> {
        match loose_post(path, explicit, &options()) {
            LooseFile::Skipped(warning) => Some(warning.message),
            LooseFile::Post(_) | LooseFile::Ignored => None,
        }
    }

    fn is_ignored(path: &Path, explicit: bool) -> bool {
        loose_post(path, explicit, &options()) == LooseFile::Ignored
    }

    #[test]
    fn a_loose_picture_becomes_a_post_with_the_defaults() {
        let folder = TempFolder::new("post");
        let path = folder.write("Sunset at the pier (final).JPG", PNG);
        let LooseFile::Post(post) = loose_post(&path, false, &options()) else {
            panic!("a picture is a post, whatever its extension says");
        };
        assert_eq!(post.key, PostKey::for_file(&path));
        assert_eq!(post.id, "Sunset_at_the_pier_final_");
        assert_eq!(post.permalink, None);
        assert_eq!(post.media.len(), 1);
        assert_eq!(post.media[0].kind, MediaKind::Image);
        assert_eq!(post.media[0].path, path);
        assert_eq!(post.media[0].manifest_path, "Sunset at the pier (final).JPG");
        assert_eq!(post.media[0].bytes, PNG.len() as u64);
        assert_eq!(post.media[0].poster, None);
        assert_eq!(post.user, User::named("tester"));
        assert_eq!(post.caption, "");
        assert_eq!(post.original_caption, "");
        assert_eq!(post.comment_count, 0);
        assert_eq!(post.like_count, None);
        assert_eq!(post.media_type, MediaType::Image);
        assert_eq!(post.context.as_deref(), Some("Sunset at the pier (final).JPG"));
        assert_eq!(post.header_subline(), Some("Sunset at the pier (final).JPG"));
        assert_eq!(post.file_name().as_deref(), Some("Sunset at the pier (final).JPG"));
        assert!(post.comments.is_empty());

        // The time is the file's modified time: just now.
        let taken_at = post.taken_at.expect("a fresh file has a usable modified time");
        assert!((text::now_ms() - taken_at).abs() < 10 * 60 * 1000);

        // A stem without a letter or digit gets a derived id.
        let symbols = folder.write("(!).png", PNG);
        let LooseFile::Post(derived) = loose_post(&symbols, true, &options()) else { panic!("a picture") };
        assert_eq!(derived.id, text::derived_file_id("(!).png"));
    }

    #[test]
    fn files_that_are_not_shown_follow_the_rule_table() {
        let folder = TempFolder::new("rules");

        // Noise and manifest names: silent either way.
        for name in ["desktop.ini", "Thumbs.db", ".DS_Store", "._IMG_1.jpg", "post.json", "Posts.JSON"] {
            let path = folder.write(name, JPEG);
            assert!(is_ignored(&path, true), "{name}");
            assert!(is_ignored(&path, false), "{name}");
        }

        // Cannot be read: reported either way.
        let missing = folder.0.join("gone.jpg");
        for explicit in [true, false] {
            let message = skip_message(&missing, explicit).expect("an unreadable file is reported");
            assert!(message.starts_with("\"gone.jpg\" could not be read: "), "{message}");
        }

        // Empty: reported when named, or when the name promises a picture.
        let empty_picture = folder.write("broken.jpg", b"");
        let empty_other = folder.write("empty.txt", b"");
        assert_eq!(skip_message(&empty_picture, true).as_deref(), Some("\"broken.jpg\" is an empty file."));
        assert_eq!(skip_message(&empty_picture, false).as_deref(), Some("\"broken.jpg\" is an empty file."));
        assert_eq!(skip_message(&empty_other, true).as_deref(), Some("\"empty.txt\" is an empty file."));
        assert!(is_ignored(&empty_other, false));

        // A video is a post like a picture, named or found, whatever its name says.
        let video = folder.write("clip.mp4", MP4);
        let disguised_video = folder.write("clip.jpg", MP4);
        for explicit in [true, false] {
            assert!(matches!(loose_post(&video, explicit, &options()), LooseFile::Post(_)));
            assert!(matches!(loose_post(&disguised_video, explicit, &options()), LooseFile::Post(_)));
        }
        // A video name that holds no video is reported like a picture name.
        let fake_video = folder.write("fake.MOV", b"just some notes");
        let empty_video = folder.write("empty.webm", b"");
        assert_eq!(skip_message(&fake_video, false).as_deref(), Some("\"fake.MOV\" is not a supported image (JPEG, PNG, GIF, WebP, AVIF, BMP or HEIC/HEIF) or video (MP4, MOV or WebM)."));
        assert_eq!(skip_message(&empty_video, false).as_deref(), Some("\"empty.webm\" is an empty file."));

        // Not supported: reported when named, or when the name promises a picture.
        let notes = folder.write("notes.txt", b"just some notes");
        let fake = folder.write("fake.HEIC", b"just some notes");
        let film = folder.write("film.mkv", MATROSKA);
        let disguised_film = folder.write("film.png", MATROSKA);
        let not_supported = |name: &str| {
            format!("\"{name}\" is not a supported image (JPEG, PNG, GIF, WebP, AVIF, BMP or HEIC/HEIF) or video (MP4, MOV or WebM).")
        };
        let matroska = |name: &str| {
            format!("\"{name}\" is a Matroska (MKV) video, which is not supported. Convert it to MP4 (H.264) or WebM.")
        };
        assert_eq!(skip_message(&notes, true), Some(not_supported("notes.txt")));
        assert!(is_ignored(&notes, false));
        assert_eq!(skip_message(&fake, false), Some(not_supported("fake.HEIC")));
        assert_eq!(skip_message(&film, true), Some(matroska("film.mkv")));
        assert!(is_ignored(&film, false));
        assert_eq!(skip_message(&disguised_film, false), Some(matroska("film.png")));

        // Codes: the known-format sentence is about media, the others about pictures.
        let LooseFile::Skipped(known) = loose_post(&film, true, &options()) else { panic!("skipped") };
        assert_eq!(known.code, WarningCode::UnsupportedMedia);
        assert_eq!(known.post_index, None);
        assert_eq!(known.path.as_deref(), Some(film.display().to_string().as_str()));
        let LooseFile::Skipped(unknown) = loose_post(&notes, true, &options()) else { panic!("skipped") };
        assert_eq!(unknown.code, WarningCode::UnsupportedImage);
    }

    #[test]
    fn a_picture_above_the_size_limit_is_skipped_either_way() {
        let folder = TempFolder::new("large");
        let path = folder.write("huge.png", PNG);
        // Extending the file does not write the bytes, so this is quick and needs no real data.
        File::options().write(true).open(&path).unwrap().set_len(MAX_IMAGE_BYTES + 1).unwrap();
        for explicit in [true, false] {
            assert_eq!(
                skip_message(&path, explicit).as_deref(),
                Some("\"huge.png\" is larger than 64 MB and was skipped."),
                "explicit: {explicit}"
            );
        }
        File::options().write(true).open(&path).unwrap().set_len(MAX_IMAGE_BYTES).unwrap();
        assert!(matches!(loose_post(&path, false, &options()), LooseFile::Post(_)));
        // A video may be larger than a picture, up to 1 GB.
        let video = folder.write("holiday.mp4", MP4);
        File::options().write(true).open(&video).unwrap().set_len(MAX_IMAGE_BYTES + 1).unwrap();
        assert!(matches!(loose_post(&video, false, &options()), LooseFile::Post(_)));
        File::options().write(true).open(&video).unwrap().set_len(MAX_VIDEO_BYTES + 1).unwrap();
        for explicit in [true, false] {
            assert_eq!(skip_message(&video, explicit).as_deref(), Some("\"holiday.mp4\" is larger than 1 GB and was skipped."), "explicit: {explicit}");
        }
    }

    #[test]
    fn a_loose_video_becomes_a_video_post_without_a_poster() {
        let folder = TempFolder::new("video-post");
        let path = folder.write("Beach day.m4v", MP4);
        let LooseFile::Post(post) = loose_post(&path, true, &options()) else { panic!("a video is a post") };
        assert_eq!(post.media[0].kind, MediaKind::Video);
        assert_eq!(post.media[0].poster, None);
        assert_eq!(post.media_type, MediaType::Video);
        assert!(post.is_video());
        assert_eq!(post.alt_text(0), "Video by tester");
        assert_eq!(post.id, "Beach_day");
        assert_eq!(post.key, PostKey::for_file(&path));
        assert_eq!(messages::edit_note_for(&post), messages::EDIT_NOTE_VIDEO);
    }

    #[test]
    fn pictures_and_videos_open_together_in_name_order() {
        let folder = TempFolder::new("mixed");
        folder.write("b_2.jpg", JPEG);
        let video = folder.write("b_10.mp4", MP4);
        folder.write("a.webm", b"\x1A\x45\xDF\xA3\x8B\x42\x86\x81\x01\x42\x82\x84webm\0\0\0\0");
        folder.write("film.mkv", MATROSKA);
        let store = empty_store(&folder);
        // One picked video opens with its pictures and videos, the viewer on it; the .mkv found
        // beside them stays silent.
        let opened = open_for_test(&request(std::slice::from_ref(&video)), &store).expect("opens");
        let names: Vec<String> = opened.collection.posts.iter().filter_map(Post::file_name).collect();
        assert_eq!(names, ["a.webm", "b_2.jpg", "b_10.mp4"]);
        assert_eq!(opened.focus, Some(2));
        assert!(opened.collection.warnings.is_empty(), "{:?}", opened.collection.warnings);
        assert_eq!(messages::collection_banner(&opened.collection), format!("Showing 3 pictures and videos from {}", opened.collection.label));
        // A named .mkv is answered for.
        let named = open_for_test(&request(&[folder.0.join("film.mkv"), video.clone()]), &store).expect("opens");
        assert_eq!(named.collection.posts.len(), 1);
        assert!(named.collection.warnings[0].message.contains("is a Matroska (MKV) video, which is not supported."));
        // Only videos.
        let only = TempFolder::new("only-videos");
        only.write("one.mp4", MP4);
        only.write("two.mov", b"\x00\x00\x00\x14ftypqt  \x20\x20\x20\x20qt  ");
        let opened = open_for_test(&request(std::slice::from_ref(&only.0)), &store).expect("opens");
        assert_eq!(messages::collection_banner(&opened.collection), format!("Showing 2 videos from {}", opened.collection.label));
    }

    #[test]
    fn a_folder_of_videos_is_no_wrapper_around_an_export() {
        let folder = TempFolder::new("videos-wrapper");
        folder.write("clip.mp4", MP4);
        folder.write("inner/post.json", br#"{"posts": []}"#);
        assert!(holds_a_picture(&folder.0));
        assert_eq!(manifest_of(&folder.0), None);
        // Nor a folder of archives: a video name keeps its archives from being opened.
        let downloads = TempFolder::new("videos-archives");
        downloads.write("clip.mp4", MP4);
        downloads.write("export.zip", b"PK\x05\x06");
        assert!(archives_in(&downloads.0).is_empty());
        assert!(has_media_extension(Path::new("a.M4V")) && has_media_extension(Path::new("b.webm")) && !has_media_extension(Path::new("c.mkv")));
    }

    #[test]
    fn a_folder_is_listed_without_folders_and_noise_in_natural_order() {
        let folder = TempFolder::new("list");
        for name in ["IMG_10.jpg", "img_1.png", "IMG_2.jpg", "notes.txt", "desktop.ini", "._IMG_2.jpg", "post.json"] {
            folder.write(name, JPEG);
        }
        folder.write("sub/inner.jpg", JPEG);
        let names: Vec<String> = list_folder_files(&folder.0).unwrap().iter().map(|path| display_name(path)).collect();
        assert_eq!(names, ["img_1.png", "IMG_2.jpg", "IMG_10.jpg", "notes.txt", "post.json"]);
        assert!(list_folder_files(&folder.0).unwrap().iter().all(|path| path.is_absolute()));

        let error = list_folder_files(&folder.0.join("nowhere")).unwrap_err();
        assert_eq!(error.code, OpenErrorCode::ReadFailed);
        assert!(error.message.contains("nowhere\" could not be opened: "), "{}", error.message);
    }

    #[test]
    fn loose_pictures_are_ordered_like_explorer_orders_names() {
        let order = |left: &str, right: &str| loose_order(Path::new(left), Path::new(right));
        assert_eq!(order("a/img_1.png", "a/IMG_2.jpg"), Ordering::Less);
        assert_eq!(order("a/IMG_2.jpg", "a/IMG_10.jpg"), Ordering::Less);
        assert_eq!(order("a/IMG_10.jpg", "a/img_9.jpg"), Ordering::Greater);
        // The name decides before the folder does.
        assert_eq!(order("z/a.jpg", "a/b.jpg"), Ordering::Less);
        // The same name in two folders: the path decides, and the order is total.
        assert_eq!(order("a/x.jpg", "b/x.jpg"), Ordering::Less);
        assert_eq!(order("b/x.jpg", "a/x.jpg"), Ordering::Greater);
        assert_eq!(order("a/x.jpg", "a/x.jpg"), Ordering::Equal);
    }

    #[test]
    fn the_label_is_the_folder_or_a_count_of_folders() {
        let base = std::env::temp_dir();
        let one = base.join("Holiday 2026").join("a.jpg");
        let two = base.join("Holiday 2026").join("b.jpg");
        let other = base.join("Work").join("c.jpg");
        assert_eq!(collection_label(&[]), "");
        assert_eq!(collection_label(&[&one]), "Holiday 2026");
        assert_eq!(collection_label(&[&one, &two]), "Holiday 2026");
        assert_eq!(collection_label(&[&one, &other, &two]), "2 folders");
        #[cfg(windows)]
        {
            let shouted = PathBuf::from(one.to_string_lossy().to_uppercase());
            assert_eq!(collection_label(&[&one, &shouted]), "Holiday 2026");
            assert_eq!(collection_label(&[Path::new(r"C:\a.jpg")]), r"C:\");
        }
    }

    fn request(paths: &[PathBuf]) -> OpenRequest {
        OpenRequest { paths: paths.to_vec(), options: options() }
    }

    fn empty_store(folder: &TempFolder) -> DescriptionStore {
        DescriptionStore::new(folder.0.join("data").join("descriptions.json"))
    }

    /// `open_paths` with an archive cache of its own (these tests open no archive).
    fn open_for_test(request: &OpenRequest, store: &DescriptionStore) -> Result<Opened, OpenError> {
        let cache = std::env::temp_dir().join(format!("instagram-core-catalog-cache-{}", std::process::id()));
        open_paths(request, store, &OpenContext::new(cache))
    }

    /// The manifest of the one export folder `paths` stand for.
    fn only_folder_source(paths: &[PathBuf]) -> Option<PathBuf> {
        match classify_request(paths) {
            RequestKind::Exports { mut sources, others } if sources.len() == 1 && others.is_empty() => match sources.remove(0) {
                SourceRequest::Folder { manifest } => Some(manifest),
                SourceRequest::Archive { .. } => None,
            },
            _ => None,
        }
    }

    #[test]
    fn archives_and_folders_of_archives_are_export_sources() {
        let folder = TempFolder::new("archives");
        let zip = folder.write("downloads/b-export.zip", b"PK\x03\x04");
        let tgz = folder.write("downloads/a-export.TAR.GZ", &[0x1f, 0x8b]);
        let rar = folder.write("downloads/c.rar", b"Rar!");
        folder.write("downloads/notes.txt", b"notes");
        let downloads = folder.0.join("downloads");
        // A folder without post.json and without picture names: its archives, in name order.
        let RequestKind::Exports { sources, others } = classify_request(std::slice::from_ref(&downloads)) else {
            panic!("a folder of archives opens its archives");
        };
        assert!(others.is_empty());
        assert_eq!(sources, [SourceRequest::Archive { archive: tgz.clone() }, SourceRequest::Archive { archive: zip.clone() }, SourceRequest::Archive { archive: rar.clone() }]);
        // A picture folder with a stray zip stays a picture folder.
        folder.write("pictures/IMG_1.jpg", JPEG);
        folder.write("pictures/old.zip", b"PK\x03\x04");
        assert_eq!(classify_request(&[folder.0.join("pictures")]), RequestKind::Loose);
        // An archive named directly, twice, and a picture beside it.
        let picture = folder.write("pictures/IMG_2.jpg", JPEG);
        let RequestKind::Exports { sources, others } = classify_request(&[zip.clone(), picture, zip.clone()]) else {
            panic!("an archive is an export source");
        };
        assert_eq!(sources, [SourceRequest::Archive { archive: zip }]);
        assert_eq!(others, [1]);
        // An archive name that does not exist is a path that cannot be opened.
        assert_eq!(classify_request(&[folder.0.join("gone.zip")]), RequestKind::Loose);
    }

    #[test]
    fn a_folder_with_pictures_of_its_own_is_never_taken_for_the_export_below_it() {
        let folder = TempFolder::new("wrapper");
        let manifest = folder.write("mixed/old-instagram-export/post.json", b"[]");
        folder.write("mixed/holiday-1.png", PNG);
        folder.write("mixed/another-album/x.png", PNG);
        let mixed = folder.0.join("mixed");
        assert_eq!(classify_request(std::slice::from_ref(&mixed)), RequestKind::Loose);
        let opened = open_for_test(&request(std::slice::from_ref(&mixed)), &empty_store(&folder)).expect("the pictures open");
        assert!(!opened.collection.is_export());
        let names: Vec<String> = opened.collection.posts.iter().map(|post| post.file_name().unwrap_or_default()).collect();
        assert_eq!(names, ["holiday-1.png"]);
        assert_eq!(opened.collection.warnings, vec![]);

        // A mere wrapper (no picture of its own, other files do not count) is the export.
        folder.write("wrapper/instagram-export/post.json", b"[]");
        folder.write("wrapper/readme.txt", b"just some notes");
        folder.write("wrapper/broken.jpg", b"");
        let wrapper = folder.0.join("wrapper");
        let Some(found) = only_folder_source(std::slice::from_ref(&wrapper)) else {
            panic!("a folder without pictures is the export one level down");
        };
        assert_eq!(display_name(&found), "post.json");
        assert_eq!(display_name(found.parent().unwrap()), "instagram-export");

        // A post.json directly in the folder makes it an export, pictures or not.
        let direct = folder.0.join("mixed").join("old-instagram-export");
        folder.write("mixed/old-instagram-export/stray.png", PNG);
        assert_eq!(only_folder_source(&[direct]), Some(absolute(&manifest)));
    }

    #[test]
    fn one_picture_in_a_folder_too_large_to_list_opens_on_its_own() {
        let folder = TempFolder::new("huge");
        for name in ["a.png", "b.png", "c.png"] {
            folder.write(name, PNG);
        }
        folder.write("d.dat", b"not a picture");
        let picked = folder.0.join("c.png");
        let opened = open_loose_up_to(&request(std::slice::from_ref(&picked)), &empty_store(&folder), 3)
            .expect("the picked picture opens");
        assert_eq!(opened.collection.posts.len(), 1);
        assert_eq!(opened.collection.posts[0].key, PostKey::for_file(&picked));
        assert_eq!(opened.focus, Some(0));
        let messages: Vec<&str> = opened.collection.warnings.iter().map(|warning| warning.message.as_str()).collect();
        let folder_name = display_name(&folder.0);
        assert_eq!(messages, [format!("\"{folder_name}\" contains more than 100,000 files, so only the picked picture was opened.")]);
        // Below the limit the neighbours come along, as always.
        let opened = open_loose_up_to(&request(std::slice::from_ref(&picked)), &empty_store(&folder), 10).unwrap();
        assert_eq!(opened.collection.posts.len(), 3);
        assert_eq!(opened.collection.warnings, vec![]);
    }

    #[test]
    fn the_reason_why_the_picked_file_is_not_shown_is_said_at_once() {
        let folder = TempFolder::new("picked");
        folder.write("a.png", PNG);
        let film = folder.write("film.mkv", MATROSKA);
        let opened = open_for_test(&request(std::slice::from_ref(&film)), &empty_store(&folder)).expect("the folder opens");
        assert_eq!(opened.focus, None);
        assert_eq!(opened.notice.as_deref(), Some("\"film.mkv\" is a Matroska (MKV) video, which is not supported. Convert it to MP4 (H.264) or WebM."));
        assert_eq!(opened.collection.posts.len(), 1);

        // A picked picture needs no such sentence.
        let picture = open_for_test(&request(&[folder.0.join("a.png")]), &empty_store(&folder)).unwrap();
        assert_eq!((picture.focus, picture.notice), (Some(0), None));

        // Nothing else to show: the sentence of the file is the message, not "no pictures".
        let alone = TempFolder::new("picked-alone");
        let empty = alone.write("empty.jpg", b"");
        let error = open_for_test(&request(std::slice::from_ref(&empty)), &empty_store(&alone)).unwrap_err();
        assert_eq!(error.code, OpenErrorCode::NoPictures);
        assert_eq!(error.message, "\"empty.jpg\" is an empty file.");
        // Several paths keep the summary.
        let notes = alone.write("notes.txt", b"just some notes");
        let error = open_for_test(&request(&[empty, notes]), &empty_store(&alone)).unwrap_err();
        assert_eq!(error.message, format!("No pictures were found in \"{}\".", display_name(&alone.0)));
    }

    #[test]
    fn a_file_merely_found_beside_the_pictures_is_not_reported_when_it_cannot_be_read() {
        let folder = TempFolder::new("unreadable");
        let database = folder.0.join("locked-database.db");
        assert!(is_ignored(&database, false));
        let message = skip_message(&database, true).expect("a named file is answered for");
        assert!(message.starts_with("\"locked-database.db\" could not be read: "), "{message}");
    }

    #[test]
    fn a_description_is_not_given_to_another_file_of_the_same_name() {
        let folder = TempFolder::new("same-name");
        let path = folder.write("IMG_0001.png", PNG);
        let mut store = empty_store(&folder);
        let LooseFile::Post(post) = loose_post(&path, true, &options()) else { panic!("a picture") };
        let modified = post.taken_at.expect("a fresh file has a modified time");
        let entry = |bytes: u64, modified: i64| crate::store::DescriptionEntry {
            caption: "Low tide at the beach".to_owned(),
            path: path.clone(),
            post_id: None,
            file_bytes: Some(bytes),
            file_modified: Some(modified),
            edited_at: 1,
        };
        let opened_caption = |store: &DescriptionStore| {
            let opened = open_for_test(&request(std::slice::from_ref(&folder.0)), store).expect("the folder opens");
            let messages: Vec<String> = opened.collection.warnings.iter().map(|warning| warning.message.clone()).collect();
            (opened.collection.posts[0].caption.clone(), messages)
        };

        // The same file: the description is shown.
        store.set(&post.key, entry(PNG.len() as u64, modified));
        assert_eq!(opened_caption(&store), ("Low tide at the beach".to_owned(), vec![]));
        // Only one of the two values changed (the picture was touched, or edited to the same
        // size): still the same picture.
        store.set(&post.key, entry(PNG.len() as u64 + 1, modified));
        assert_eq!(opened_caption(&store).0, "Low tide at the beach");
        store.set(&post.key, entry(PNG.len() as u64, modified - 60_000));
        assert_eq!(opened_caption(&store).0, "Low tide at the beach");
        // Both differ: another picture got the name. Not shown, and the user is told.
        store.set(&post.key, entry(PNG.len() as u64 + 1, modified - 60_000));
        assert_eq!(
            opened_caption(&store),
            (String::new(), vec!["The description saved for \"IMG_0001.png\" belongs to an earlier file of that name and is not shown.".to_owned()])
        );
        assert!(store.get(&post.key).is_some(), "the entry is kept");
    }

    #[test]
    fn laying_descriptions_over_again_also_takes_away_a_removed_one() {
        let folder = TempFolder::new("reapply");
        folder.write("a.png", PNG);
        let mut store = empty_store(&folder);
        let mut opened = open_for_test(&request(std::slice::from_ref(&folder.0)), &store).unwrap();
        opened.collection.posts[0].caption = "typed in another window, then removed".to_owned();
        apply_descriptions(&mut opened.collection, &store);
        assert_eq!(opened.collection.posts[0].caption, "");
        let key = opened.collection.posts[0].key.clone();
        store.set(
            &key,
            crate::store::DescriptionEntry { caption: "kept".to_owned(), path: PathBuf::new(), post_id: None, file_bytes: None, file_modified: None, edited_at: 1 },
        );
        apply_descriptions(&mut opened.collection, &store);
        assert_eq!(opened.collection.posts[0].caption, "kept");
    }

    #[test]
    fn picture_extensions_are_matched_without_regard_to_case() {
        assert!(has_picture_extension(Path::new("a.JPG")));
        assert!(has_picture_extension(Path::new("b.heif")));
        assert!(has_picture_extension(Path::new("archive.tar.avif")));
        assert!(!has_picture_extension(Path::new("notes.txt")));
        assert!(!has_picture_extension(Path::new("jpg")));
        assert!(!has_picture_extension(Path::new("clip.mp4")));
    }
}
