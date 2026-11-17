//! Export archives: .zip, .tar, .tar.gz and .tgz files that hold an "instagram-export", opened
//! the way the clone's import opens them (frontend/src/import/archive.ts, core.ts; web brief 3).
//!
//! ```text
//! is_archive_candidate   a name the archive rules apply to (also .rar, .7z, ... for their sentence)
//! detect_archive_kind    the container, from the first 512 bytes, else from the extension
//! open_archive           the archive cache's generation of the archive (made when there is none),
//!                        then post.json read and the posts assembled like an export folder
//! zip / tar              ports of the clone's readers
//! cache                  the extraction cache in %LOCALAPPDATA%
//! ```
//!
//! The manifest may lie at any depth (the shallowest post.json or posts.json wins, as in the
//! clone); its folder is the export root, and every path of post.json is resolved against the
//! members below it with `manifest::FileIndex`, exactly as for a folder. Only the members post.json
//! references are copied into the cache, each up to what its use allows (a picture-only use 64 MB,
//! other media 1 GB, the manifest 32 MB), and nothing of the archive is ever written anywhere else.

pub mod cache;
pub mod tar;
pub mod zip;

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use self::cache::{ArchiveCache, ArchiveLease, ArchiveVersion, CacheEntry, CachedMember, Generation, GenerationWriter, MANIFEST_COPY_NAME};
use self::tar::{TarInput, TarLimits};
use self::zip::ZipReader;
use crate::manifest::{
    self, FileIndex, LoadedExport, ManifestError, SourceFile, MAX_IMAGE_BYTES, MAX_MANIFEST_BYTES, MAX_SOURCE_ENTRIES,
    MAX_TOTAL_MEDIA_BYTES, MAX_VIDEO_BYTES,
};
use crate::messages::{self, display_name};
use crate::model::{PostKey, PostOrigin};
use crate::text;

const MIB: u64 = 1024 * 1024;

/// The file name endings of the archives that are opened (archive.ts:11), lower case, without
/// the dot. The Open dialog offers them in its filters.
pub const ARCHIVE_EXTENSIONS: [&str; 4] = ["zip", "tar", "tgz", "tar.gz"];
/// The endings of archive types that are recognised only to say that they are not read
/// (archive.ts:67).
pub const UNSUPPORTED_ARCHIVE_EXTENSIONS: [&str; 9] = ["rar", "7z", "xz", "bz2", "zst", "lz", "lzma", "cab", "iso"];
/// How much of an archive is read to tell its container.
pub const DETECT_BYTES: usize = 512;
/// What a .tar.gz may unpack to in total, skipped members included (archive.ts:95).
pub const MAX_GZIP_OUTPUT_BYTES: u64 = MAX_TOTAL_MEDIA_BYTES + MAX_MANIFEST_BYTES + 64 * MIB;
/// The largest member of a .tar.gz whose content is kept while it streams by (archive.ts:88).
pub const MAX_STREAMED_MEMBER_BYTES: u64 = max_u64(max_u64(MAX_IMAGE_BYTES, MAX_VIDEO_BYTES), MAX_MANIFEST_BYTES);

const fn max_u64(left: u64, right: u64) -> u64 {
    if left > right {
        left
    } else {
        right
    }
}

/// Why an archive could not be opened (the clone's `ImportErrorCode` of these failures).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveErrorCode {
    /// The archive cannot be read, or is damaged.
    ReadFailed,
    /// Not an archive this app reads.
    Unsupported,
    /// Over a limit: entries, sizes, the unpacked size, the media of one opening.
    TooLarge,
    /// No post.json in the archive.
    NoManifest,
    /// post.json was found but could not be used; the message is the reader's.
    Export,
    /// The archive cache would leave too little free space.
    NoSpace,
    /// The archive cache could not be written.
    CacheFailed,
}

/// Raised when an archive cannot be opened. `message` is a complete sentence for the user.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ArchiveError {
    /// The kind of failure.
    pub code: ArchiveErrorCode,
    /// The sentence to show.
    pub message: String,
}

impl ArchiveError {
    /// An error of `code` with its sentence.
    pub fn new(code: ArchiveErrorCode, message: impl Into<String>) -> ArchiveError {
        ArchiveError { code, message: message.into() }
    }
}

impl std::fmt::Display for ArchiveError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ArchiveError {}

impl From<ManifestError> for ArchiveError {
    fn from(error: ManifestError) -> ArchiveError {
        let code = match error.code {
            manifest::ManifestErrorCode::TooLarge => ArchiveErrorCode::TooLarge,
            manifest::ManifestErrorCode::NoManifest => ArchiveErrorCode::NoManifest,
            _ => ArchiveErrorCode::Export,
        };
        ArchiveError::new(code, error.message)
    }
}

/// The container of an archive (archive.ts `ArchiveKind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ArchiveKind {
    /// A zip file.
    Zip,
    /// A plain tar file.
    Tar,
    /// A gzip-compressed tar file (.tar.gz, .tgz).
    Gzip,
}

impl ArchiveKind {
    /// The word entry.json of the archive cache uses.
    pub fn as_str(self) -> &'static str {
        match self {
            ArchiveKind::Zip => "zip",
            ArchiveKind::Tar => "tar",
            ArchiveKind::Gzip => "gzip",
        }
    }
}

// --- Names ----------------------------------------------------------------------------------

/// util.ts `decodeName`: UTF-8 when the bytes are valid UTF-8, otherwise Latin-1, so nothing is lost.
pub fn decode_name(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes.iter().map(|byte| char::from(*byte)).collect(),
    }
}

/// Code page 437, bytes 0x80 to 0xFF (util.ts:90-92).
const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";

/// util.ts `decodeZipName`: the name of a zip member. Modern tools write UTF-8 (with or without
/// saying so); names that are not valid UTF-8 are in the DOS code page, which is what the zip
/// format prescribes and what 7-Zip and older Windows tools write ("é" as the single byte 0x82).
pub fn decode_zip_name(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => text.to_owned(),
        Err(_) => bytes
            .iter()
            .map(|byte| if *byte < 0x80 { char::from(*byte) } else { CP437_HIGH.chars().nth(usize::from(byte - 0x80)).unwrap_or('\u{fffd}') })
            .collect(),
    }
}

/// True when `name` ends (in any letter case) with one of `ARCHIVE_EXTENSIONS`.
pub fn is_archive_name(name: &str) -> bool {
    let lower = name.to_lowercase();
    ARCHIVE_EXTENSIONS.iter().any(|extension| lower.ends_with(&format!(".{extension}")))
}

/// True when the file name of `path` makes it an archive to open: an archive this app reads, or
/// one of the types it only recognises to say that they are not read (web brief 3.1). Decided by
/// the name alone: a picture dropped on the window costs no extra read.
pub fn is_archive_candidate(path: &Path) -> bool {
    let name = display_name(path);
    let lower = name.to_lowercase();
    is_archive_name(&name) || UNSUPPORTED_ARCHIVE_EXTENSIONS.iter().any(|extension| lower.ends_with(&format!(".{extension}")))
}

/// The signatures of archive types that are not read (archive.ts:39-45).
const UNSUPPORTED_SIGNATURES: [(&str, &[u8]); 5] = [
    ("RAR", &[0x52, 0x61, 0x72, 0x21, 0x1a, 0x07]),
    ("7z", &[0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c]),
    ("xz", &[0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00]),
    ("bzip2", &[0x42, 0x5a, 0x68]),
    ("Zstandard", &[0x28, 0xb5, 0x2f, 0xfd]),
];

/// archive.ts `detectArchiveKind` for a file called `name` of `size` bytes that starts with
/// `head` (its first `DETECT_BYTES`): empty -> `archive_empty`; `PK\3\4`, `PK\5\6`, `PK\7\8` ->
/// zip; `1F 8B` -> gzip; "ustar" at offset 257 -> tar; RAR, 7z, xz, bzip2, Zstandard ->
/// `archive_kind_not_supported`; otherwise by the extension: .zip -> zip, .tar -> tar, the
/// unsupported ones -> `archive_type_not_supported`, anything else -> `not_a_supported_archive`.
pub fn detect_kind(name: &str, head: &[u8], size: u64) -> Result<ArchiveKind, ArchiveError> {
    if size == 0 {
        return Err(ArchiveError::new(ArchiveErrorCode::ReadFailed, messages::archive_empty(name)));
    }
    let starts = |signature: &[u8]| head.starts_with(signature);
    if starts(b"PK\x03\x04") || starts(b"PK\x05\x06") || starts(b"PK\x07\x08") {
        return Ok(ArchiveKind::Zip);
    }
    if starts(&[0x1f, 0x8b]) {
        return Ok(ArchiveKind::Gzip);
    }
    if head.get(257..262) == Some(b"ustar".as_slice()) {
        return Ok(ArchiveKind::Tar);
    }
    if let Some((kind, _)) = UNSUPPORTED_SIGNATURES.iter().find(|(_, signature)| starts(signature)) {
        return Err(ArchiveError::new(ArchiveErrorCode::Unsupported, messages::archive_kind_not_supported(kind)));
    }
    // No known signature: the extension decides (a zip with a prefix, a pre-POSIX tar).
    let lower = name.to_lowercase();
    if lower.ends_with(".zip") {
        return Ok(ArchiveKind::Zip);
    }
    if lower.ends_with(".tar") {
        return Ok(ArchiveKind::Tar);
    }
    if UNSUPPORTED_ARCHIVE_EXTENSIONS.iter().any(|extension| lower.ends_with(&format!(".{extension}"))) {
        return Err(ArchiveError::new(ArchiveErrorCode::Unsupported, messages::archive_type_not_supported(name)));
    }
    Err(ArchiveError::new(ArchiveErrorCode::Unsupported, messages::not_a_supported_archive(name)))
}

/// `detect_kind` for a file on disk.
pub fn detect_archive_kind(path: &Path) -> Result<ArchiveKind, ArchiveError> {
    let name = display_name(path);
    let unreadable = |error: io::Error| ArchiveError::new(ArchiveErrorCode::ReadFailed, messages::source_not_imported(&name, &error.to_string()));
    let mut file = File::open(path).map_err(unreadable)?;
    let size = file.metadata().map_err(unreadable)?.len();
    let mut head = Vec::with_capacity(DETECT_BYTES);
    (&mut file).take(DETECT_BYTES as u64).read_to_end(&mut head).map_err(unreadable)?;
    detect_kind(&name, &head, size)
}

// --- Opening --------------------------------------------------------------------------------

/// The limits of reading an archive (tests make them small, like the clone's tests do).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReadLimits {
    /// More members than this are refused (`MAX_SOURCE_ENTRIES`).
    pub max_entries: usize,
    /// What a .tar.gz may unpack to (`MAX_GZIP_OUTPUT_BYTES`).
    pub max_gzip_output: u64,
    /// The largest member of a .tar.gz that is kept (`MAX_STREAMED_MEMBER_BYTES`).
    pub max_streamed_member: u64,
}

impl Default for ReadLimits {
    fn default() -> ReadLimits {
        ReadLimits { max_entries: MAX_SOURCE_ENTRIES, max_gzip_output: MAX_GZIP_OUTPUT_BYTES, max_streamed_member: MAX_STREAMED_MEMBER_BYTES }
    }
}

/// The pictures and videos the archives of one opening have copied so far, against
/// `MAX_TOTAL_MEDIA_BYTES` (the clone's `MediaBudget`, shared by every source of one import).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MediaBudget {
    /// Bytes charged so far.
    pub used: u64,
    /// The most that may be charged.
    pub limit: u64,
}

impl Default for MediaBudget {
    fn default() -> MediaBudget {
        MediaBudget { used: 0, limit: MAX_TOTAL_MEDIA_BYTES }
    }
}

/// An opened archive.
#[derive(Debug)]
pub struct OpenedArchive {
    /// The posts, keyed `PostKey::for_archive_post` and with `PostOrigin::Archive`; their media
    /// are the copies in the archive cache.
    pub loaded: LoadedExport,
    /// The copy of post.json in the archive cache.
    pub manifest: PathBuf,
    /// The lease that keeps the generation while the posts are shown.
    pub lease: ArchiveLease,
    /// True when the archive was unpacked now; false when the cache was used again.
    pub extracted: bool,
}

/// Opens the export archive `archive` (web brief 3.4): a usable generation of the archive cache,
/// or a new one (the archive is read, its manifest found, the referenced members copied); then
/// post.json is read and the posts are assembled exactly as for a folder, keyed by the archive.
/// The media the posts use are charged to `budget`; above its limit the archive fails with
/// `media_budget_exceeded` and nothing is charged.
pub fn open_archive(archive: &Path, cache: &ArchiveCache, budget: &mut MediaBudget) -> Result<OpenedArchive, ArchiveError> {
    open_archive_with(archive, cache, budget, &ReadLimits::default())
}

/// `open_archive` with other read limits.
pub fn open_archive_with(archive: &Path, cache: &ArchiveCache, budget: &mut MediaBudget, limits: &ReadLimits) -> Result<OpenedArchive, ArchiveError> {
    let archive = std::path::absolute(archive).unwrap_or_else(|_| archive.to_path_buf());
    let name = display_name(&archive);
    let unreadable = |error: io::Error| ArchiveError::new(ArchiveErrorCode::ReadFailed, messages::source_not_imported(&name, &error.to_string()));
    let metadata = fs::metadata(&archive).map_err(unreadable)?;
    let version = ArchiveVersion::of(&metadata);

    let (generation, extracted) = match cache.find(&archive, version) {
        Some(generation) => {
            log::info!("Archive cache: reused {} for {}.", generation.folder.display(), archive.display());
            (generation, false)
        }
        None => {
            let kind = detect_archive_kind(&archive)?;
            let generation = extract(&archive, kind, version, cache, limits)?;
            log::info!(
                "Archive cache: unpacked {} into {} ({} files kept).",
                archive.display(),
                generation.folder.display(),
                generation.entry.members.iter().filter(|member| member.file.is_some()).count()
            );
            (generation, true)
        }
    };
    assemble(&archive, generation, budget, extracted)
}

/// Builds the posts of a generation and charges their media to `budget`.
fn assemble(archive: &Path, generation: Generation, budget: &mut MediaBudget, extracted: bool) -> Result<OpenedArchive, ArchiveError> {
    let name = display_name(archive);
    let Generation { folder, entry, lease } = generation;
    let manifest_copy = folder.join(MANIFEST_COPY_NAME);
    let bytes = fs::read(&manifest_copy).map_err(|error| ArchiveError::new(ArchiveErrorCode::ReadFailed, messages::source_not_imported(&name, &error.to_string())))?;
    let parsed = manifest::read_manifest(&bytes)?;
    let files: Vec<SourceFile> = entry
        .members
        .iter()
        .enumerate()
        .map(|(position, member)| SourceFile {
            relative: member.path.clone(),
            // A member that was not kept has no file; its name is never created.
            path: folder.join(member.file.clone().unwrap_or_else(|| format!("absent-{position}"))),
            bytes: member.size,
            unreadable: member.unreadable.clone(),
        })
        .collect();
    let sizes: HashMap<PathBuf, u64> = files.iter().map(|file| (file.path.clone(), file.bytes)).collect();
    let index = FileIndex::from_files(folder.clone(), files);
    let mut loaded = manifest::assemble_export(parsed, &index, &manifest_copy)?;
    for post in &mut loaded.posts {
        post.key = PostKey::for_archive_post(archive, &post.id);
        post.origin = PostOrigin::Archive { archive: archive.to_path_buf() };
    }

    // The bytes this archive's posts use, each file once (core.ts charges every loaded file).
    let mut used: HashSet<&Path> = HashSet::new();
    for post in &loaded.posts {
        for item in &post.media {
            used.insert(&item.path);
            if let Some(poster) = &item.poster {
                used.insert(&poster.path);
            }
        }
        if let Some(avatar) = &post.user.avatar {
            used.insert(avatar);
        }
    }
    let charged: u64 = used.iter().map(|path| sizes.get(*path).copied().unwrap_or(0)).sum();
    if budget.used.saturating_add(charged) > budget.limit {
        return Err(ArchiveError::new(ArchiveErrorCode::TooLarge, messages::media_budget_exceeded(budget.limit)));
    }
    budget.used += charged;
    Ok(OpenedArchive { loaded, manifest: manifest_copy, lease, extracted })
}

/// One member of the listing of an archive.
struct Listed {
    path: String,
    size: u64,
}

/// Copies member `index` (of the listing) into a writer, at most so many bytes.
type CopyMember<'a> = dyn FnMut(usize, u64, &mut dyn Write) -> Result<u64, ArchiveError> + 'a;

/// How the members of an archive reach the cache.
enum Members<'a> {
    /// Copied on request: member index, the most bytes to copy, where to.
    Copy(&'a mut CopyMember<'a>),
    /// Already written while the stream passed by (a .tar.gz): the indexes that have a file.
    Written(&'a HashSet<usize>),
}

/// Unpacks `archive` into a new generation of the cache.
fn extract(archive: &Path, kind: ArchiveKind, version: ArchiveVersion, cache: &ArchiveCache, limits: &ReadLimits) -> Result<Generation, ArchiveError> {
    let name = display_name(archive);
    let file = File::open(archive).map_err(|error| ArchiveError::new(ArchiveErrorCode::ReadFailed, messages::source_not_imported(&name, &error.to_string())))?;
    let writer = cache.begin(archive, version)?;
    match kind {
        ArchiveKind::Zip => {
            let mut reader = ZipReader::open(file, limits.max_entries)?;
            let listed: Vec<Listed> = reader.members().iter().map(|member| Listed { path: member.path.clone(), size: member.size }).collect();
            let mut copy = |index: usize, max: u64, out: &mut dyn Write| reader.copy_member(index, max, out);
            keep_referenced(writer, archive, kind, version, cache, &listed, Members::Copy(&mut copy))
        }
        ArchiveKind::Tar => {
            let len = version.size;
            let mut input = tar::PlainInput::new(file, len);
            let tar_limits = TarLimits { max_entries: limits.max_entries, max_member_bytes: limits.max_streamed_member };
            let entries = tar::list_plain(&mut input, &tar_limits)?;
            let listed: Vec<Listed> = entries.iter().map(|entry| Listed { path: entry.path.clone(), size: entry.size }).collect();
            let mut copy = |index: usize, max: u64, out: &mut dyn Write| input.copy_member(&entries[index], max, out);
            keep_referenced(writer, archive, kind, version, cache, &listed, Members::Copy(&mut copy))
        }
        ArchiveKind::Gzip => {
            cache.ensure_free(0)?;
            let mut input = tar::gz_input(file, limits.max_gzip_output);
            let tar_limits = TarLimits { max_entries: limits.max_entries, max_member_bytes: limits.max_streamed_member };
            let folder = writer.folder().to_path_buf();
            let mut written: Vec<(usize, PathBuf)> = Vec::new();
            let entries = tar::walk(&mut input, &tar_limits, &mut |input, index, path, size| {
                cache.ensure_free(size)?;
                let target = folder.join(cache::member_file_name(index, path));
                let mut out = File::create(&target).map_err(|error| cache_failed(&error))?;
                written.push((index, target));
                stream_member(input, size, &mut out)
            })?;
            // A member that was cut off is not listed: its partial copy goes.
            let mut kept: HashSet<usize> = HashSet::new();
            for (index, path) in written {
                if index < entries.len() {
                    kept.insert(index);
                } else {
                    let _ = fs::remove_file(path);
                }
            }
            let listed: Vec<Listed> = entries.iter().map(|entry| Listed { path: entry.path.clone(), size: entry.size }).collect();
            keep_referenced(writer, archive, kind, version, cache, &listed, Members::Written(&kept))
        }
    }
}

/// A failure to write the cache.
fn cache_failed(error: &io::Error) -> ArchiveError {
    ArchiveError::new(ArchiveErrorCode::CacheFailed, messages::archive_cache_not_written(&error.to_string()))
}

/// Copies the `size` bytes of a member from a streamed tar into `out`; returns the bytes read
/// (fewer when the stream ends).
fn stream_member<I: TarInput>(input: &mut I, size: u64, out: &mut File) -> Result<u64, ArchiveError> {
    let mut buffer = vec![0_u8; 64 * 1024];
    let mut copied: u64 = 0;
    while copied < size {
        let step = usize::try_from(size - copied).unwrap_or(buffer.len()).min(buffer.len());
        let read = input.read_full(&mut buffer[..step])?;
        if read == 0 {
            break;
        }
        out.write_all(&buffer[..read]).map_err(|error| cache_failed(&error))?;
        copied += read as u64;
    }
    Ok(copied)
}

/// The rest of an extraction once the members are listed (web brief 3.4 steps 3 and 4): the
/// manifest, the export root, the members post.json references, entry.json, publication.
fn keep_referenced(
    writer: GenerationWriter,
    archive: &Path,
    kind: ArchiveKind,
    version: ArchiveVersion,
    cache: &ArchiveCache,
    listed: &[Listed],
    mut members: Members<'_>,
) -> Result<Generation, ArchiveError> {
    let name = display_name(archive);
    let folder = writer.folder().to_path_buf();
    let file_of = |index: usize| folder.join(cache::member_file_name(index, &listed[index].path));

    // The manifest: the shallowest post.json at any depth. A path listed twice: the later wins.
    let manifest_path = manifest::find_manifest_entry(listed.iter().map(|member| member.path.as_str()))
        .map(str::to_owned)
        .ok_or_else(|| ArchiveError::new(ArchiveErrorCode::NoManifest, messages::no_manifest(&name)))?;
    let manifest_index = listed.iter().rposition(|member| member.path == manifest_path).unwrap_or_default();
    if listed[manifest_index].size > MAX_MANIFEST_BYTES {
        return Err(ArchiveError::new(ArchiveErrorCode::TooLarge, messages::manifest_too_large()));
    }
    let manifest_bytes = match &mut members {
        Members::Copy(copy) => {
            let mut bytes: Vec<u8> = Vec::new();
            copy(manifest_index, MAX_MANIFEST_BYTES, &mut bytes).map_err(|error| match error.code {
                ArchiveErrorCode::TooLarge => ArchiveError::new(ArchiveErrorCode::TooLarge, messages::manifest_too_large()),
                _ => error,
            })?;
            bytes
        }
        Members::Written(written) if written.contains(&manifest_index) => {
            fs::read(file_of(manifest_index)).map_err(|error| cache_failed(&error))?
        }
        Members::Written(_) => return Err(ArchiveError::new(ArchiveErrorCode::TooLarge, messages::manifest_too_large())),
    };
    fs::write(folder.join(MANIFEST_COPY_NAME), &manifest_bytes).map_err(|error| cache_failed(&error))?;
    let parsed = manifest::read_manifest(&manifest_bytes)?;

    // The export root is the folder of the manifest; the members below it are the export.
    let root = manifest_path.rsplit_once('/').map(|(root, _)| root.to_owned()).unwrap_or_default();
    let below: Vec<(usize, String)> = listed
        .iter()
        .enumerate()
        .filter_map(|(index, member)| {
            let relative = if root.is_empty() { Some(member.path.as_str()) } else { member.path.strip_prefix(&root).and_then(|rest| rest.strip_prefix('/')) };
            relative.map(|relative| (index, relative.to_owned()))
        })
        .collect();
    let files: Vec<SourceFile> = below
        .iter()
        .map(|(index, relative)| SourceFile { relative: relative.clone(), path: file_of(*index), bytes: listed[*index].size, unreadable: None })
        .collect();
    let index = FileIndex::from_files(folder.clone(), files);
    let referenced = manifest::referenced_files(&parsed, &index);

    // What each referenced member may take: its use decides (core.ts:151).
    let mut caps: HashMap<usize, u64> = HashMap::new();
    for reference in &referenced {
        caps.insert(below[reference.file].0, manifest::max_bytes_for(reference.image_only));
    }
    let mut kept: HashSet<usize> = HashSet::new();
    let mut unreadable: HashMap<usize, String> = HashMap::new();
    match &mut members {
        Members::Copy(copy) => {
            let needed: u64 = caps.iter().filter(|(member, cap)| listed[**member].size <= **cap).map(|(member, _)| listed[*member].size).sum();
            cache.ensure_free(needed.saturating_add(manifest_bytes.len() as u64))?;
            let mut order: Vec<usize> = caps.keys().copied().collect();
            order.sort_unstable();
            for member in order {
                let cap = caps[&member];
                if listed[member].size > cap {
                    // The reader says "too large" from the declared size; nothing to copy.
                    continue;
                }
                let target = file_of(member);
                let mut out = File::create(&target).map_err(|error| cache_failed(&error))?;
                match copy(member, cap, &mut out) {
                    Ok(_) => {
                        kept.insert(member);
                    }
                    Err(error) if matches!(error.code, ArchiveErrorCode::CacheFailed | ArchiveErrorCode::NoSpace) => return Err(error),
                    Err(error) => {
                        drop(out);
                        let _ = fs::remove_file(&target);
                        unreadable.insert(member, error.message);
                    }
                }
            }
        }
        Members::Written(written) => {
            for &member in written.iter() {
                let referenced_within_cap = caps.get(&member).is_some_and(|cap| listed[member].size <= *cap);
                if referenced_within_cap {
                    kept.insert(member);
                } else {
                    let _ = fs::remove_file(file_of(member));
                }
            }
        }
    }

    let entry = CacheEntry {
        archive: archive.to_path_buf(),
        version,
        kind,
        manifest: manifest_path,
        root,
        created_at: text::now_ms(),
        members: below
            .iter()
            .map(|(index, relative)| CachedMember {
                path: relative.clone(),
                size: listed[*index].size,
                file: kept.contains(index).then(|| cache::member_file_name(*index, &listed[*index].path)),
                unreadable: unreadable.get(index).cloned(),
            })
            .collect(),
    };
    writer.finish(&entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dos_code_page_has_128_characters() {
        assert_eq!(CP437_HIGH.chars().count(), 128);
        assert_eq!(decode_zip_name(&[0x82, 0x9c, 0xff]), "é£\u{a0}");
        assert_eq!(decode_name(&[0x63, 0x61, 0x66, 0xe9]), "café");
        assert_eq!(decode_name("café".as_bytes()), "café");
    }

    #[test]
    fn archive_names_are_told_by_their_ending() {
        for name in ["a.zip", "B.ZIP", "c.tar", "d.tgz", "e.tar.gz", "F.TAR.GZ"] {
            assert!(is_archive_name(name), "{name}");
            assert!(is_archive_candidate(Path::new(name)), "{name}");
        }
        for name in ["a.rar", "b.7z", "c.tar.xz", "d.bz2", "e.zst", "f.lz", "g.lzma", "h.cab", "i.iso"] {
            assert!(!is_archive_name(name), "{name}");
            assert!(is_archive_candidate(Path::new(name)), "{name}");
        }
        for name in ["a.jpg", "zip", "a.gz", "a.zipx", "post.json"] {
            assert!(!is_archive_candidate(Path::new(name)), "{name}");
        }
    }

    #[test]
    fn the_container_is_told_by_its_content_first() {
        let mut tar_head = vec![0_u8; 512];
        tar_head[257..262].copy_from_slice(b"ustar");
        assert_eq!(detect_kind("x.bin", b"PK\x03\x04rest", 10), Ok(ArchiveKind::Zip));
        assert_eq!(detect_kind("x.bin", b"PK\x05\x06", 10), Ok(ArchiveKind::Zip));
        assert_eq!(detect_kind("x.tar", &[0x1f, 0x8b, 8], 10), Ok(ArchiveKind::Gzip));
        assert_eq!(detect_kind("x.zip", &tar_head, 512), Ok(ArchiveKind::Tar));
        assert_eq!(detect_kind("prefixed.zip", b"MZ....", 10), Ok(ArchiveKind::Zip));
        assert_eq!(detect_kind("old.tar", b"plain", 10), Ok(ArchiveKind::Tar));
        let message = |result: Result<ArchiveKind, ArchiveError>| result.unwrap_err().message;
        assert_eq!(message(detect_kind("empty.zip", b"", 0)), "\"empty.zip\" is empty.");
        assert_eq!(message(detect_kind("x.zip", b"Rar!\x1a\x07\x00", 10)), "RAR archives are not supported. Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.");
        assert_eq!(message(detect_kind("x.zip", b"BZh91", 10)), "bzip2 archives are not supported. Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.");
        assert_eq!(message(detect_kind("photos.7z", b"text", 10)), "\"photos.7z\" is an archive type that is not supported. Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.");
        assert_eq!(message(detect_kind("photos.tgz", b"text", 10)), "\"photos.tgz\" is not a supported archive. Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.");
    }

    #[test]
    fn the_streamed_member_cap_is_the_largest_use() {
        assert_eq!(MAX_STREAMED_MEMBER_BYTES, MAX_VIDEO_BYTES);
        assert_eq!(text::format_bytes(MAX_GZIP_OUTPUT_BYTES), "4.1 GB");
    }
}
