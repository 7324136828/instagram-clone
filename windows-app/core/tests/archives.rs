//! Export archives through the public interface: the clone's fixture archives (copied from its
//! e2e/fixtures into tests/fixtures/archives and only read), archives built here (stored and
//! deflated zips, tar, tar.gz, a wrapping folder, parts, duplicates, damage and hostile names,
//! size bombs, ten thousand entries), the archive cache, merging, descriptions, and the round
//! trip through "Download all".
//!
//! Every test unpacks into an archive cache of its own below the system's temporary folder; the
//! real `%LOCALAPPDATA%` is never touched.

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use flate2::write::GzEncoder;
use flate2::Compression;
use instagram_core::archive::cache::{ArchiveCache, CacheLimits, ENTRY_FILE_NAME, LOCK_FILE_NAME, MANIFEST_COPY_NAME};
use instagram_core::archive::{self, ArchiveErrorCode, MediaBudget, ReadLimits};
use instagram_core::catalog::{self, OpenContext, OpenErrorCode, OpenOptions, OpenRequest, Opened, RequestKind, SourceRequest};
use instagram_core::export::{self, ExportOptions};
use instagram_core::messages;
use instagram_core::model::{fold_path, ExportKind, MediaKind, Post, PostKey, PostOrigin, WarningCode};
use instagram_core::store::{self, DescriptionStore, SaveOutcome};
use zip::write::SimpleFileOptions;

// --- Helpers ------------------------------------------------------------------------------------

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

fn archive_fixture(name: &str) -> PathBuf {
    fixtures().join("archives").join(name)
}

/// A folder of its own below the system's temporary folder, removed when dropped.
struct TempFolder(PathBuf);

impl TempFolder {
    fn new(label: &str) -> TempFolder {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let folder = std::env::temp_dir().join(format!("instagram-core-archives-{label}-{}-{unique}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).expect("a temporary folder can be created");
        TempFolder(folder)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    fn join(&self, relative: &str) -> PathBuf {
        self.0.join(relative)
    }

    fn write(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.0.join(relative);
        fs::create_dir_all(path.parent().expect("a file has a folder")).expect("folders can be created");
        fs::write(&path, bytes).expect("a file can be written");
        path
    }

    /// The archive cache of this test.
    fn cache(&self) -> ArchiveCache {
        ArchiveCache::new(self.join("cache"))
    }

    fn context(&self) -> OpenContext {
        OpenContext { archive_cache: self.cache(), library_dir: None }
    }
}

impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn options() -> OpenOptions {
    OpenOptions { default_username: "tester".to_owned() }
}

fn empty_store(temp: &TempFolder) -> DescriptionStore {
    DescriptionStore::new(temp.join("data").join("descriptions.json"))
}

fn open_in(temp: &TempFolder, paths: &[PathBuf], store: &DescriptionStore) -> Result<Opened, catalog::OpenError> {
    catalog::open_paths(&OpenRequest { paths: paths.to_vec(), options: options() }, store, &temp.context())
}

fn open(temp: &TempFolder, paths: &[PathBuf]) -> Opened {
    open_in(temp, paths, &empty_store(temp)).unwrap_or_else(|error| panic!("{paths:?} did not open: {error}"))
}

fn ids(posts: &[Post]) -> Vec<&str> {
    posts.iter().map(|post| post.id.as_str()).collect()
}

fn messages_of(opened: &Opened) -> Vec<&str> {
    opened.collection.warnings.iter().map(|warning| warning.message.as_str()).collect()
}

/// Every file below `folder`, relative, with its bytes.
fn tree(folder: &Path) -> BTreeMap<String, Vec<u8>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![folder.to_path_buf()];
    while let Some(next) = pending.pop() {
        for entry in fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(folder).expect("below the folder").to_string_lossy().replace('\\', "/");
                found.insert(relative, fs::read(&path).unwrap_or_default());
            }
        }
    }
    found
}

/// The names of the entries of a folder, sorted.
fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder).expect("a folder").flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    names.sort();
    names
}

/// Writes a zip with the `zip` crate: (name, bytes, deflated).
fn write_zip(path: &Path, members: &[(&str, &[u8], bool)]) {
    let file = File::create(path).expect("the archive can be created");
    let mut writer = zip::ZipWriter::new(file);
    for (name, bytes, deflated) in members {
        let method = if *deflated { zip::CompressionMethod::Deflated } else { zip::CompressionMethod::Stored };
        writer.start_file(*name, SimpleFileOptions::default().compression_method(method)).expect("a member can be started");
        writer.write_all(bytes).expect("a member can be written");
    }
    writer.finish().expect("the archive can be finished");
}

/// One member of a zip written by hand, where the `zip` crate would not write what a test needs.
struct Raw<'a> {
    name: &'a str,
    data: Vec<u8>,
    method: u16,
    flags: u16,
    /// The size the directory declares, when it lies.
    declared: Option<u32>,
}

fn raw<'a>(name: &'a str, data: &[u8]) -> Raw<'a> {
    Raw { name, data: data.to_vec(), method: 0, flags: 0, declared: None }
}

fn deflate(data: &[u8]) -> Vec<u8> {
    let mut encoder = flate2::write::DeflateEncoder::new(Vec::new(), Compression::best());
    encoder.write_all(data).expect("deflate");
    encoder.finish().expect("deflate")
}

/// Writes a zip by hand: local headers, central directory, end record. Sizes are what `declared`
/// says (else the data length); names are written as given.
fn write_raw_zip(path: &Path, members: &[Raw]) {
    let mut out: Vec<u8> = Vec::new();
    let mut central: Vec<u8> = Vec::new();
    for member in members {
        let offset = out.len() as u32;
        let size = member.declared.unwrap_or(member.data.len() as u32);
        let compressed = member.data.len() as u32;
        let header = |signature: u32, central_record: bool| {
            let mut bytes = signature.to_le_bytes().to_vec();
            if central_record {
                bytes.extend_from_slice(&[20, 0]);
            }
            bytes.extend_from_slice(&[20, 0]);
            bytes.extend_from_slice(&member.flags.to_le_bytes());
            bytes.extend_from_slice(&member.method.to_le_bytes());
            bytes.extend_from_slice(&[0, 0, 0x21, 0]);
            bytes.extend_from_slice(&[0, 0, 0, 0]);
            bytes.extend_from_slice(&compressed.to_le_bytes());
            bytes.extend_from_slice(&size.to_le_bytes());
            bytes.extend_from_slice(&(member.name.len() as u16).to_le_bytes());
            bytes.extend_from_slice(&[0, 0]);
            bytes
        };
        out.extend(header(0x0403_4b50, false));
        out.extend_from_slice(member.name.as_bytes());
        out.extend_from_slice(&member.data);
        central.extend(header(0x0201_4b50, true));
        central.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend_from_slice(&offset.to_le_bytes());
        central.extend_from_slice(member.name.as_bytes());
    }
    let directory = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4b50_u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(members.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&directory.to_le_bytes());
    out.extend_from_slice(&[0, 0]);
    fs::write(path, out).expect("the archive can be written");
}

/// A tar header block.
fn tar_header(name: &str, kind: u8, size: u64, link: &str) -> Vec<u8> {
    let mut block = vec![0_u8; 512];
    block[..name.len()].copy_from_slice(name.as_bytes());
    block[100..107].copy_from_slice(b"0000644");
    block[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
    block[156] = kind;
    block[157..157 + link.len()].copy_from_slice(link.as_bytes());
    block[257..263].copy_from_slice(b"ustar\0");
    block[263..265].copy_from_slice(b"00");
    block[148..156].copy_from_slice(b"        ");
    let sum: u32 = block.iter().map(|byte| u32::from(*byte)).sum();
    block[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    block
}

/// A tar of regular files, with an end marker.
fn tar_of(members: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    for (name, data) in members {
        out.extend(tar_header(name, b'0', data.len() as u64, ""));
        out.extend_from_slice(data);
        out.resize(out.len().div_ceil(512) * 512, 0);
    }
    out.extend(vec![0_u8; 1024]);
    out
}

fn gzip(bytes: &[u8]) -> Vec<u8> {
    let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
    encoder.write_all(bytes).expect("gzip");
    encoder.finish().expect("gzip")
}

/// A small post.json: posts named by (id, media path, taken_at).
fn manifest(posts: &[(&str, &str, &str)]) -> Vec<u8> {
    let entries: Vec<String> = posts
        .iter()
        .map(|(id, media, taken_at)| format!("{{\"id\": \"{id}\", \"user\": {{\"username\": \"tester\"}}, \"caption\": \"Caption of {id}\", \"taken_at\": \"{taken_at}\", \"media\": [{{\"type\": \"image\", \"path\": \"{media}\"}}]}}"))
        .collect();
    format!("{{\"format\": \"instagram-export\", \"version\": 1, \"posts\": [{}]}}", entries.join(", ")).into_bytes()
}

/// The first bytes of pictures: enough for the sniffer, which is all opening looks at.
const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0, 1, 2, 3];
const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0, 0, 0, 13, b'I', b'H', b'D', b'R'];

// --- The clone's fixtures -----------------------------------------------------------------------

#[test]
fn export_basic_zip_opens_like_the_folder_export() {
    let temp = TempFolder::new("basic-zip");
    let archive = archive_fixture("export-basic.zip");
    let before = tree(&fixtures().join("archives"));
    let opened = open(&temp, std::slice::from_ref(&archive));
    let folder = open(&temp, &[fixtures().join("export-basic")]);
    let collection = &opened.collection;

    assert_eq!(collection.label, "export-basic.zip");
    assert_eq!(messages::showing_banner(collection.posts.len(), collection.total_entries, collection.is_export(), &collection.label), "Showing 6 posts from export-basic.zip");
    assert_eq!(ids(&collection.posts), ids(&folder.collection.posts));
    assert_eq!(collection.warnings, folder.collection.warnings);
    assert_eq!(collection.folder.as_deref(), archive.parent());
    let [source] = collection.sources() else { panic!("one source") };
    assert_eq!((source.kind, source.name.as_str(), source.total_entries, source.shown), (ExportKind::Archive, "export-basic.zip", 6, 6));
    assert_eq!(fold_path(&source.path), fold_path(&archive));
    assert_eq!(source.manifest.file_name().and_then(|name| name.to_str()), Some(MANIFEST_COPY_NAME));
    assert!(source.manifest.starts_with(temp.join("cache")));

    for (from_zip, from_folder) in collection.posts.iter().zip(&folder.collection.posts) {
        assert_eq!(from_zip.key, PostKey::for_archive_post(&archive, &from_zip.id));
        assert_eq!(from_zip.origin, PostOrigin::Archive { archive: archive.clone() });
        assert_eq!((&from_zip.caption, &from_zip.user.username, from_zip.taken_at), (&from_folder.caption, &from_folder.user.username, from_folder.taken_at));
        // The window title names the file as post.json does, never the cache copy.
        assert_eq!(from_zip.file_name(), from_folder.file_name());
        for (slide, item) in from_zip.media.iter().enumerate() {
            assert!(item.path.starts_with(temp.join("cache")), "the media are copies in the cache");
            assert_eq!(fs::read(&item.path).unwrap(), fs::read(&from_folder.media[slide].path).unwrap(), "the bytes of the archive member");
            assert_eq!(item.manifest_path, from_folder.media[slide].manifest_path);
        }
        assert_eq!(from_zip.user.avatar.is_some(), from_folder.user.avatar.is_some());
    }
    // Nothing is written next to the archive, and the archive is unchanged.
    assert_eq!(tree(&fixtures().join("archives")), before);
}

#[test]
fn export_basic_tar_gz_and_the_wrapped_zip_give_the_same_posts() {
    let temp = TempFolder::new("basic-tar-gz");
    let zip = open(&temp, &[archive_fixture("export-basic.zip")]);
    let tar_gz = open(&temp, &[archive_fixture("export-basic.tar.gz")]);
    let wrapped = open(&temp, &[archive_fixture("export-wrapped.zip")]);
    for other in [&tar_gz, &wrapped] {
        assert_eq!(ids(&other.collection.posts), ids(&zip.collection.posts));
        assert_eq!(other.collection.warnings, vec![], "the __MACOSX decoy and .DS_Store are noise");
        for (left, right) in other.collection.posts.iter().zip(&zip.collection.posts) {
            assert_eq!(left.caption, right.caption);
            assert_eq!(left.media.len(), right.media.len());
            for (one, two) in left.media.iter().zip(&right.media) {
                assert_eq!(fs::read(&one.path).unwrap(), fs::read(&two.path).unwrap());
            }
        }
    }
    assert_eq!(tar_gz.collection.label, "export-basic.tar.gz");
    // A tar.gz keeps only what post.json references (here: every file).
    let generation = tar_gz.leases[0].generation().to_path_buf();
    let kept: Vec<String> = names_in(&generation).into_iter().filter(|name| name.starts_with('m')).collect();
    assert_eq!(kept.len(), 9, "{kept:?}");
}

#[test]
fn the_two_part_archives_merge_into_one_collection() {
    let temp = TempFolder::new("parts");
    let opened = open(&temp, &[archive_fixture("export-part1.zip"), archive_fixture("export-part2.zip")]);
    let collection = &opened.collection;
    assert_eq!(collection.label, "2 archives");
    assert_eq!(ids(&collection.posts), ["FXTpart0003", "FXTpart0002", "FXTpart0001"], "newest first");
    assert_eq!(collection.total_entries, 3, "four entries, one of them replaced");
    assert_eq!(messages::showing_banner(collection.posts.len(), collection.total_entries, true, &collection.label), "Showing 3 posts from 2 archives");
    assert_eq!(messages_of(&opened), ["The post \"FXTpart0002\" from export-part2.zip replaces the one with the same id from export-part1.zip."]);
    assert_eq!(collection.warnings[0].code, WarningCode::DuplicateId);
    assert_eq!(collection.posts[1].caption, "Part 2: second version of the shared post");
    let sources: Vec<(&str, usize, usize)> = collection.sources().iter().map(|source| (source.name.as_str(), source.shown, source.replaced)).collect();
    assert_eq!(sources, [("export-part1.zip", 1, 0), ("export-part2.zip", 2, 1)]);
    assert_eq!(opened.leases.len(), 2);
    // The same archives in a folder of their own: the "Downloads folder" rule.
    let downloads = temp.join("Downloads");
    fs::create_dir_all(&downloads).unwrap();
    for name in ["export-part2.zip", "export-part1.zip"] {
        fs::copy(archive_fixture(name), downloads.join(name)).unwrap();
    }
    fs::write(downloads.join("readme.txt"), b"not a picture").unwrap();
    let from_folder = open(&temp, std::slice::from_ref(&downloads));
    assert_eq!(ids(&from_folder.collection.posts), ids(&collection.posts));
    assert_eq!(from_folder.collection.sources()[0].name, "export-part1.zip", "natural file-name order");
}

#[test]
fn videos_lenient_and_problem_archives_read_like_their_folders() {
    let temp = TempFolder::new("fixtures");
    let videos = open(&temp, &[archive_fixture("export-videos.zip")]);
    assert_eq!(videos.collection.posts.len(), 5);
    assert!(videos.collection.posts.iter().any(|post| post.media.iter().any(|item| item.kind == MediaKind::Video)), "videos are kept");
    let reel = videos.collection.posts.iter().flat_map(|post| &post.media).find(|item| item.manifest_path == "posts/reel1.webm").expect("the reel");
    assert_eq!(reel.kind, MediaKind::Video);
    assert!(reel.poster.as_ref().is_some_and(|poster| poster.path.starts_with(temp.join("cache"))));

    let lenient = open(&temp, &[archive_fixture("export-lenient.zip")]);
    let lenient_folder = open(&temp, &[fixtures().join("export-lenient")]);
    assert_eq!(ids(&lenient.collection.posts), ids(&lenient_folder.collection.posts));

    let problems = open(&temp, &[archive_fixture("export-problems.zip")]);
    let problems_folder = open(&temp, &[fixtures().join("export-problems")]);
    assert_eq!(ids(&problems.collection.posts), ids(&problems_folder.collection.posts));
    assert_eq!(messages_of(&problems), messages_of(&problems_folder));
    assert_eq!(problems.collection.total_entries, problems_folder.collection.total_entries);
}

#[test]
fn garbage_and_an_archive_without_post_json_say_why() {
    let temp = TempFolder::new("refused");
    let store = empty_store(&temp);
    let garbage = open_in(&temp, &[archive_fixture("garbage.zip")], &store).unwrap_err();
    assert_eq!(garbage.code, OpenErrorCode::Export);
    assert!(garbage.message.starts_with("The zip file is damaged or incomplete ("), "{}", garbage.message);
    let none = open_in(&temp, &[archive_fixture("not-an-export.zip")], &store).unwrap_err();
    assert_eq!(none.message, "No post.json was found in \"not-an-export.zip\". An export contains post.json next to a posts folder.");
    // Both together: every source failed, and each reason is named.
    let both = open_in(&temp, &[archive_fixture("garbage.zip"), archive_fixture("not-an-export.zip")], &store).unwrap_err();
    assert!(both.message.starts_with("None of the 2 selected archives or folders could be imported. garbage.zip: The zip file is damaged or incomplete ("), "{}", both.message);
    assert!(both.message.ends_with(" not-an-export.zip: No post.json was found in \"not-an-export.zip\". An export contains post.json next to a posts folder."), "{}", both.message);
    // One good source among them: the others become warnings, in front.
    let mixed = open(&temp, &[archive_fixture("garbage.zip"), archive_fixture("export-part2.zip")]);
    assert_eq!(mixed.collection.label, "export-part2.zip", "the banner names what it shows, not what could not be imported");
    assert_eq!(mixed.collection.posts.len(), 2);
    // Two good ones and two bad ones: the two that were read are counted.
    let two_of_four = open(&temp, &[archive_fixture("garbage.zip"), archive_fixture("export-part1.zip"), archive_fixture("not-an-export.zip"), archive_fixture("export-part2.zip")]);
    assert_eq!(two_of_four.collection.label, "2 archives");
    let first = &mixed.collection.warnings[0];
    assert!(first.message.starts_with("\"garbage.zip\" could not be imported: The zip file is damaged or incomplete ("), "{}", first.message);
    assert_eq!(first.code, WarningCode::InvalidEntry);
    // Empty files and other archive types.
    let empty = temp.write("empty.zip", b"");
    assert_eq!(open_in(&temp, &[empty], &store).unwrap_err().message, "\"empty.zip\" is empty.");
    let rar = temp.write("photos.rar", b"Rar!\x1a\x07\x00 and more");
    assert_eq!(open_in(&temp, &[rar], &store).unwrap_err().message, "RAR archives are not supported. Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.");
    let seven = temp.write("photos.7z", b"not really");
    assert_eq!(open_in(&temp, &[seven], &store).unwrap_err().message, "\"photos.7z\" is an archive type that is not supported. Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.");
}

#[test]
fn more_than_fifty_sources_are_cut_with_a_note() {
    let temp = TempFolder::new("fifty");
    let mut paths: Vec<PathBuf> = Vec::new();
    for index in 0..51 {
        let path = temp.join(&format!("part{index:02}.zip"));
        let id = format!("P{index:02}");
        let posts = manifest(&[(id.as_str(), "a.jpg", "2026-01-01T00:00:00Z")]);
        write_zip(&path, &[("post.json", &posts, true), ("a.jpg", JPEG, false)]);
        paths.push(path);
    }
    let opened = open(&temp, &paths);
    assert_eq!(opened.collection.posts.len(), 50);
    assert_eq!(opened.collection.label, "50 archives");
    assert_eq!(messages_of(&opened)[0], "51 archives or folders were selected; only the first 50 were imported.");
    assert_eq!(opened.collection.warnings[0].code, WarningCode::Truncated);
}

// --- Archives built here ------------------------------------------------------------------------

#[test]
fn stored_and_deflated_zips_tar_and_tar_gz_open_alike() {
    let temp = TempFolder::new("kinds");
    let posts = manifest(&[("A1", "posts/a.jpg", "2026-03-01T00:00:00Z"), ("A2", "posts/b.png", "2026-02-01T00:00:00Z")]);
    let stored = temp.join("stored.zip");
    write_zip(&stored, &[("post.json", &posts, false), ("posts/a.jpg", JPEG, false), ("posts/b.png", PNG, false)]);
    let deflated = temp.join("deflated.zip");
    write_zip(&deflated, &[("post.json", &posts, true), ("posts/a.jpg", JPEG, true), ("posts/b.png", PNG, true)]);
    let tar_bytes = tar_of(&[("export/post.json", &posts), ("export/posts/a.jpg", JPEG), ("export/posts/b.png", PNG)]);
    let tar = temp.write("plain.tar", &tar_bytes);
    let tgz = temp.write("compressed.tgz", &gzip(&tar_bytes));
    for archive in [&stored, &deflated, &tar, &tgz] {
        let opened = open(&temp, std::slice::from_ref(archive));
        assert_eq!(ids(&opened.collection.posts), ["A1", "A2"], "{}", archive.display());
        assert_eq!(fs::read(&opened.collection.posts[0].media[0].path).unwrap(), JPEG);
        assert_eq!(fs::read(&opened.collection.posts[1].media[0].path).unwrap(), PNG);
        assert_eq!(opened.collection.posts[1].file_name().as_deref(), Some("b.png"));
    }
}

#[test]
fn a_tar_gz_keeps_only_the_members_post_json_references() {
    let temp = TempFolder::new("tgz-unreferenced");
    let posts = manifest(&[("A1", "posts/a.jpg", "2026-03-01T00:00:00Z")]);
    let tar_bytes = tar_of(&[("post.json", &posts), ("posts/a.jpg", JPEG), ("posts/unused.jpg", JPEG), ("notes.txt", b"hello")]);
    let archive = temp.write("export.tar.gz", &gzip(&tar_bytes));
    let opened = open(&temp, &[archive]);
    let generation = opened.leases[0].generation().to_path_buf();
    assert_eq!(names_in(&generation), [ENTRY_FILE_NAME, LOCK_FILE_NAME, "m000002.jpg", MANIFEST_COPY_NAME]);
}

#[test]
fn the_unpacked_size_of_a_tar_gz_is_capped() {
    let temp = TempFolder::new("tgz-cap");
    let posts = manifest(&[("A1", "posts/a.jpg", "2026-03-01T00:00:00Z")]);
    let tar_bytes = tar_of(&[("post.json", &posts), ("posts/a.jpg", &[7_u8; 8192])]);
    let archive = temp.write("export.tar.gz", &gzip(&tar_bytes));
    let limits = ReadLimits { max_gzip_output: 4096, ..ReadLimits::default() };
    let error = archive::open_archive_with(&archive, &temp.cache(), &mut MediaBudget::default(), &limits).unwrap_err();
    assert_eq!(error.message, "The archive unpacks to more than 4 KB, which is too large to import.");
    // Nothing is left in the cache of a failed extraction.
    assert!(fs::read_dir(temp.join("cache")).map(|entries| entries.count()).unwrap_or(0) == 0);
    // A damaged stream.
    let mut damaged = gzip(&tar_of(&[("post.json", &posts), ("posts/a.jpg", &[9_u8; 50_000])]));
    // The compression method of the gzip header is not deflate (8): the stream cannot be decoded.
    damaged[2] = 7;
    let broken = temp.write("broken.tar.gz", &damaged);
    let error = open_in(&temp, &[broken], &empty_store(&temp)).unwrap_err();
    assert!(error.message.starts_with("The archive is damaged or incomplete ("), "{}", error.message);
    // Damage inside the stream: either the decoder or the tar walk notices it.
    let mut garbled = gzip(&tar_of(&[("post.json", &posts), ("posts/a.jpg", &[9_u8; 50_000])]));
    let middle = garbled.len() / 2;
    for byte in &mut garbled[middle..middle + 20] {
        *byte ^= 0xa5;
    }
    let broken = temp.write("garbled.tar.gz", &garbled);
    let error = open_in(&temp, &[broken], &empty_store(&temp)).unwrap_err();
    assert!(error.message.starts_with("The archive is damaged or incomplete (") || error.message == messages::TAR_BAD_CHECKSUM, "{}", error.message);
}

#[test]
fn hostile_member_names_never_reach_the_file_system() {
    let temp = TempFolder::new("hostile");
    let posts = br#"{"posts": [
        {"id": "P1", "user": "tester", "taken_at": "2026-01-03T00:00:00Z", "media": ["../evil.jpg"]},
        {"id": "P2", "user": "tester", "taken_at": "2026-01-02T00:00:00Z", "media": ["C:/Windows/x/abs.jpg"]},
        {"id": "P3", "user": "tester", "taken_at": "2026-01-01T00:00:00Z", "media": ["con.jpg", "a:b.jpg", "/rooted.jpg"]}
    ]}"#;
    let archive = temp.join("hostile.zip");
    write_raw_zip(
        &archive,
        &[raw("post.json", posts), raw("../evil.jpg", JPEG), raw("C:/Windows/x/abs.jpg", JPEG), raw("con.jpg", JPEG), raw("a:b.jpg", JPEG), raw("/rooted.jpg", JPEG)],
    );
    let outside = temp.path().parent().expect("the temporary folder").join("evil.jpg");
    let existed = outside.exists();
    let opened = open(&temp, std::slice::from_ref(&archive));
    // "../evil.jpg" climbs out of the export: the post is skipped with the clone's warning.
    assert_eq!(ids(&opened.collection.posts), ["P2", "P3"]);
    assert!(messages_of(&opened).iter().any(|message| message.contains("points outside the export")), "{:?}", messages_of(&opened));
    assert_eq!(opened.collection.posts[1].media.len(), 3);
    // Every file the cache holds has a name the app made, inside the generation.
    let generation = opened.leases[0].generation().to_path_buf();
    for name in names_in(&generation) {
        assert!(name.starts_with('m') || [ENTRY_FILE_NAME, LOCK_FILE_NAME, MANIFEST_COPY_NAME].contains(&name.as_str()), "{name}");
    }
    for post in &opened.collection.posts {
        for item in &post.media {
            assert_eq!(item.path.parent(), Some(generation.as_path()));
        }
    }
    assert_eq!(outside.exists(), existed, "nothing was written beside the temporary folder");
    assert_eq!(names_in(temp.path()), ["cache", "hostile.zip"], "nothing was written next to the archive");
}

#[test]
fn symbolic_links_and_devices_in_a_tar_are_not_files() {
    let temp = TempFolder::new("tar-links");
    let posts = manifest(&[("A1", "posts/a.jpg", "2026-03-01T00:00:00Z"), ("A2", "posts/link.jpg", "2026-02-01T00:00:00Z")]);
    let mut bytes = Vec::new();
    for (name, data) in [("post.json", posts.as_slice()), ("posts/a.jpg", JPEG)] {
        bytes.extend(tar_header(name, b'0', data.len() as u64, ""));
        bytes.extend_from_slice(data);
        bytes.resize(bytes.len().div_ceil(512) * 512, 0);
    }
    bytes.extend(tar_header("posts/link.jpg", b'2', 0, "../../../../Windows/win.ini"));
    bytes.extend(tar_header("posts/hard.jpg", b'1', 0, "posts/a.jpg"));
    bytes.extend(tar_header("posts/device", b'3', 0, ""));
    bytes.extend(vec![0_u8; 1024]);
    let archive = temp.write("links.tar", &bytes);
    let opened = open(&temp, &[archive]);
    assert_eq!(ids(&opened.collection.posts), ["A1"]);
    assert_eq!(messages_of(&opened), ["Post 2: the image \"posts/link.jpg\" is not in the export."]);
}

#[test]
fn members_that_cannot_be_extracted_become_warnings_of_their_post() {
    let temp = TempFolder::new("member-problems");
    let posts = br#"{"posts": [
        {"id": "P1", "user": "tester", "taken_at": "2026-01-04T00:00:00Z", "media": ["posts/secret.jpg"]},
        {"id": "P2", "user": "tester", "taken_at": "2026-01-03T00:00:00Z", "media": ["posts/packed.jpg"]},
        {"id": "P3", "user": "tester", "taken_at": "2026-01-02T00:00:00Z", "media": ["posts/bomb.jpg"]},
        {"id": "P4", "user": "tester", "taken_at": "2026-01-01T00:00:00Z", "media": ["posts/huge.jpg", "posts/fine.jpg"]}
    ]}"#;
    let bomb_data = vec![0_u8; 2 * 1024 * 1024];
    let archive = temp.join("problems.zip");
    write_raw_zip(
        &archive,
        &[
            raw("post.json", posts),
            Raw { flags: 1, ..raw("posts/secret.jpg", JPEG) },
            Raw { method: 12, ..raw("posts/packed.jpg", JPEG) },
            // A size bomb: two megabytes of zeros that claim to be 100 bytes.
            Raw { method: 8, declared: Some(100), ..raw("posts/bomb.jpg", &deflate(&bomb_data)) },
            // Declares 2 GB: more than a picture or video may be; it is never unpacked.
            Raw { method: 8, declared: Some(2_000_000_000), ..raw("posts/huge.jpg", &deflate(&[1, 2, 3])) },
            raw("posts/fine.jpg", JPEG),
        ],
    );
    let opened = open(&temp, std::slice::from_ref(&archive));
    assert_eq!(ids(&opened.collection.posts), ["P4"]);
    assert_eq!(
        messages_of(&opened),
        [
            "Post 1: \"posts/secret.jpg\" could not be read: \"posts/secret.jpg\" is password protected and cannot be read.",
            "Post 2: \"posts/packed.jpg\" could not be read: \"posts/packed.jpg\" uses a zip compression method that is not supported (12). Re-create the zip with standard (deflate) compression.",
            "Post 3: \"posts/bomb.jpg\" could not be read: The zip file is damaged or incomplete (\"posts/bomb.jpg\" is larger than declared).",
            "Post 4: \"posts/huge.jpg\" is larger than 1 GB and was skipped.",
        ]
    );
    // Nothing of the bomb is on disk.
    let generation = opened.leases[0].generation().to_path_buf();
    let total: u64 = fs::read_dir(&generation).unwrap().flatten().map(|entry| entry.metadata().unwrap().len()).sum();
    assert!(total < 64 * 1024, "{total} bytes in the cache");
    // The same reasons come back when the cache is used again.
    let again = open(&temp, &[archive]);
    assert!(!again.cache_written);
    assert_eq!(messages_of(&again), messages_of(&opened));
}

#[test]
fn duplicate_member_names_take_the_later_member() {
    let temp = TempFolder::new("duplicates");
    let posts = manifest(&[("A1", "posts/a.jpg", "2026-03-01T00:00:00Z")]);
    let archive = temp.join("dupes.zip");
    write_raw_zip(&archive, &[raw("post.json", &posts), raw("posts/a.jpg", PNG), raw("posts/a.jpg", JPEG)]);
    let opened = open(&temp, &[archive]);
    assert_eq!(fs::read(&opened.collection.posts[0].media[0].path).unwrap(), JPEG);
}

#[test]
fn ten_thousand_entries_open_and_the_entry_cap_holds() {
    let temp = TempFolder::new("ten-thousand");
    let posts = manifest(&[("A1", "posts/a.jpg", "2026-03-01T00:00:00Z")]);
    let archive = temp.join("many.zip");
    let names: Vec<String> = (0..10_000).map(|index| format!("extra/{index:05}.txt")).collect();
    let mut members: Vec<(&str, &[u8], bool)> = vec![("post.json", &posts, true), ("posts/a.jpg", JPEG, false)];
    members.extend(names.iter().map(|name| (name.as_str(), b"x".as_slice(), false)));
    write_zip(&archive, &members);
    let started = std::time::Instant::now();
    let opened = open(&temp, std::slice::from_ref(&archive));
    assert_eq!(ids(&opened.collection.posts), ["A1"]);
    assert!(started.elapsed().as_secs() < 30, "{:?}", started.elapsed());
    let generation = opened.leases[0].generation().to_path_buf();
    assert_eq!(names_in(&generation).iter().filter(|name| name.starts_with('m')).count(), 1, "only the referenced member is copied");
    let limits = ReadLimits { max_entries: 5_000, ..ReadLimits::default() };
    let fresh = TempFolder::new("ten-thousand-cap");
    let error = archive::open_archive_with(&archive, &fresh.cache(), &mut MediaBudget::default(), &limits).unwrap_err();
    assert_eq!(error.message, "The zip file contains more than 5000 files.");
    assert_eq!(error.code, ArchiveErrorCode::TooLarge);
}

#[test]
fn the_media_of_one_opening_share_one_budget() {
    let temp = TempFolder::new("budget");
    let mut budget = MediaBudget { used: 0, limit: 60_000 };
    let first = archive::open_archive(&archive_fixture("export-part1.zip"), &temp.cache(), &mut budget).expect("the first archive fits");
    assert!(budget.used > 0 && budget.used < 60_000, "{}", budget.used);
    let used = budget.used;
    let error = archive::open_archive(&archive_fixture("export-basic.zip"), &temp.cache(), &mut budget).unwrap_err();
    assert_eq!(error.message, "The pictures and videos of this import add up to more than 59 KB. Import the export in smaller parts.");
    assert_eq!(budget.used, used, "a failed archive charges nothing");
    drop(first);
}

#[test]
fn free_space_is_checked_before_unpacking() {
    let temp = TempFolder::new("free-space");
    let cache = temp.cache().with_free_space(|_| Some(100));
    let error = archive::open_archive(&archive_fixture("export-basic.zip"), &cache, &mut MediaBudget::default()).unwrap_err();
    assert_eq!(error.message, "Not enough free disk space: 100 bytes is free on the drive of the archive cache and at least 1 GB must stay free.");
    assert_eq!(error.code, ArchiveErrorCode::NoSpace);
    let roomy = temp.cache().with_free_space(|_| Some(u64::MAX));
    assert!(archive::open_archive(&archive_fixture("export-basic.zip"), &roomy, &mut MediaBudget::default()).is_ok());
}

// --- The cache ------------------------------------------------------------------------------------

/// Sets the modified time of a file.
fn set_modified(path: &Path, time: std::time::SystemTime) {
    File::options().write(true).open(path).expect("the file").set_modified(time).expect("the time can be set");
}

#[test]
fn the_cache_is_used_again_without_reading_the_archive() {
    let temp = TempFolder::new("reuse");
    let copy = temp.join("export-basic.zip");
    fs::copy(archive_fixture("export-basic.zip"), &copy).unwrap();
    let first = archive::open_archive(&copy, &temp.cache(), &mut MediaBudget::default()).expect("first open");
    assert!(first.extracted);
    let generation = first.lease.generation().to_path_buf();

    // Spoil every byte of the archive while keeping its size and time: the cache must answer.
    let modified = fs::metadata(&copy).unwrap().modified().unwrap();
    let size = fs::metadata(&copy).unwrap().len();
    fs::write(&copy, vec![0_u8; size as usize]).unwrap();
    set_modified(&copy, modified);
    let second = archive::open_archive(&copy, &temp.cache(), &mut MediaBudget::default()).expect("the cache is used");
    assert!(!second.extracted);
    assert_eq!(second.lease.generation(), generation);
    assert_eq!(ids(&second.loaded.posts), ids(&first.loaded.posts));

    // Another version of the archive: a new generation.
    fs::copy(archive_fixture("export-basic.zip"), &copy).unwrap();
    set_modified(&copy, modified + std::time::Duration::from_secs(60));
    let third = archive::open_archive(&copy, &temp.cache(), &mut MediaBudget::default()).expect("a new generation");
    assert!(third.extracted);
    assert_ne!(third.lease.generation(), generation);

    // A kept file that went missing makes a generation unusable.
    let media = third.loaded.posts[0].media[0].path.clone();
    drop((first, second, third));
    fs::remove_file(&media).unwrap();
    let fourth = archive::open_archive(&copy, &temp.cache(), &mut MediaBudget::default()).expect("made again");
    assert!(fourth.extracted);
}

#[test]
fn clean_up_never_deletes_a_generation_in_use() {
    let temp = TempFolder::new("clean-up");
    let tiny = CacheLimits { max_bytes: 1, ..CacheLimits::default() };
    let cache = temp.cache().with_limits(tiny);
    let held = archive::open_archive(&archive_fixture("export-part1.zip"), &cache, &mut MediaBudget::default()).unwrap();
    let also_held = archive::open_archive(&archive_fixture("export-part1.zip"), &cache, &mut MediaBudget::default()).unwrap();
    assert_eq!(held.lease.generation(), also_held.lease.generation(), "two leases share one generation");
    let other = archive::open_archive(&archive_fixture("export-part2.zip"), &cache, &mut MediaBudget::default()).unwrap();
    let other_generation = other.lease.generation().to_path_buf();
    drop(other);

    // Over the size cap: the unused generation goes, the leased one stays.
    let report = cache.clean_up(instagram_core::text::now_ms());
    assert_eq!(report.removed, std::slice::from_ref(&other_generation));
    assert!(held.lease.generation().join(ENTRY_FILE_NAME).is_file());
    assert!(!other_generation.exists());
    drop(held);
    assert!(also_held.lease.generation().exists(), "one lease is still held");
    let generation = also_held.lease.generation().to_path_buf();
    drop(also_held);
    let report = cache.clean_up(instagram_core::text::now_ms());
    assert_eq!(report.removed, std::slice::from_ref(&generation));
    assert_eq!(fs::read_dir(temp.join("cache")).unwrap().count(), 0, "empty key folders go too");
}

#[test]
fn above_its_size_the_cache_lets_the_least_recently_used_go_first() {
    let temp = TempFolder::new("clean-up-order");
    let cache = temp.cache();
    let mut generations: Vec<(PathBuf, u64)> = Vec::new();
    for name in ["export-part1.zip", "export-part2.zip", "export-videos.zip"] {
        let opened = archive::open_archive(&archive_fixture(name), &cache, &mut MediaBudget::default()).unwrap();
        let generation = opened.lease.generation().to_path_buf();
        let kept: u64 = fs::read_dir(&generation).unwrap().flatten().filter(|entry| entry.file_name().to_string_lossy().starts_with('m')).map(|entry| entry.metadata().unwrap().len()).sum();
        generations.push((generation, kept));
    }
    // Used last: part2 an hour ago, videos a day ago, part1 a week ago.
    let now = std::time::SystemTime::now();
    let hours = |count: u64| now - std::time::Duration::from_secs(count * 3600);
    set_modified(&generations[1].0.join(LOCK_FILE_NAME), hours(1));
    set_modified(&generations[2].0.join(LOCK_FILE_NAME), hours(24));
    set_modified(&generations[0].0.join(LOCK_FILE_NAME), hours(24 * 7));
    // A cap that the newest two fit into: only the least recently used one goes.
    let cap = generations[1].1 + generations[2].1;
    let report = cache.with_limits(CacheLimits { max_bytes: cap, ..CacheLimits::default() }).clean_up(instagram_core::text::now_ms());
    assert_eq!(report.removed, std::slice::from_ref(&generations[0].0));
    assert_eq!(report.kept_bytes, cap);
    assert!(generations[1].0.exists() && generations[2].0.exists());
}

#[test]
fn clean_up_removes_old_versions_unused_generations_and_stale_temporary_folders() {
    let temp = TempFolder::new("clean-up-rules");
    let cache = temp.cache();
    let copy = temp.join("export-part2.zip");
    fs::copy(archive_fixture("export-part2.zip"), &copy).unwrap();
    let old = archive::open_archive(&copy, &cache, &mut MediaBudget::default()).unwrap();
    let old_generation = old.lease.generation().to_path_buf();
    drop(old);
    set_modified(&copy, std::time::SystemTime::now() + std::time::Duration::from_secs(120));
    let new = archive::open_archive(&copy, &cache, &mut MediaBudget::default()).unwrap();
    let new_generation = new.lease.generation().to_path_buf();
    drop(new);
    // A generation of another archive that nobody used for 31 days.
    let unused = archive::open_archive(&archive_fixture("export-part1.zip"), &cache, &mut MediaBudget::default()).unwrap();
    let unused_generation = unused.lease.generation().to_path_buf();
    drop(unused);
    set_modified(&unused_generation.join(LOCK_FILE_NAME), std::time::SystemTime::now() - std::time::Duration::from_secs(31 * 24 * 3600));
    // A temporary folder left by a crash two hours ago, and one being written now.
    let stale = temp.join("cache/.tmp-0badf00d");
    fs::create_dir_all(&stale).unwrap();
    fs::write(stale.join(LOCK_FILE_NAME), b"").unwrap();
    set_modified(&stale.join(LOCK_FILE_NAME), std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600));
    let fresh = temp.join("cache/.tmp-0fresh00");
    fs::create_dir_all(&fresh).unwrap();
    fs::write(fresh.join(LOCK_FILE_NAME), b"").unwrap();

    let mut removed = cache.clean_up(instagram_core::text::now_ms()).removed;
    removed.sort();
    let mut expected = vec![old_generation, unused_generation, stale];
    expected.sort();
    assert_eq!(removed, expected);
    assert!(new_generation.join(ENTRY_FILE_NAME).is_file(), "the newest generation stays");
    assert!(fresh.exists(), "a folder that is being written stays");
}

// --- Opening more, descriptions, the round trip ---------------------------------------------------

#[test]
fn exports_opened_more_join_the_exports_on_screen() {
    let temp = TempFolder::new("add");
    let store = empty_store(&temp);
    let mut collection = open(&temp, &[fixtures().join("export-basic")]).collection;
    let part1 = archive_fixture("export-part1.zip");
    let RequestKind::Exports { sources, others } = catalog::classify_request(std::slice::from_ref(&part1)) else { panic!("an archive") };
    assert_eq!(sources, [SourceRequest::Archive { archive: part1.clone() }]);
    let added = catalog::add_sources(&mut collection, std::slice::from_ref(&part1), sources.clone(), &others, &store, &temp.context()).unwrap();
    assert_eq!(added.added, 2);
    assert_eq!(collection.posts.len(), 8);
    assert_eq!(collection.label, "1 archive and 1 folder");
    assert_eq!(collection.total_entries, 8);
    let warnings_before = collection.warnings.clone();

    // The same archive again: its posts are read anew and win; nothing is doubled.
    let again = catalog::add_sources(&mut collection, std::slice::from_ref(&part1), sources, &[], &store, &temp.context()).unwrap();
    assert_eq!(again.added, 2);
    assert_eq!(collection.posts.len(), 8);
    assert_eq!(collection.sources().len(), 2);
    assert_eq!(collection.sources()[1].name, "export-part1.zip");
    assert_eq!(collection.warnings, warnings_before);

    // Part 2 replaces the shared post and says so.
    let part2 = archive_fixture("export-part2.zip");
    let RequestKind::Exports { sources, others } = catalog::classify_request(std::slice::from_ref(&part2)) else { panic!("an archive") };
    catalog::add_sources(&mut collection, std::slice::from_ref(&part2), sources, &others, &store, &temp.context()).unwrap();
    assert_eq!(collection.posts.len(), 9);
    assert_eq!(collection.label, "2 archives and 1 folder");
    assert_eq!(collection.warnings.last().map(|warning| warning.message.as_str()), Some("The post \"FXTpart0002\" from export-part2.zip replaces the one with the same id from 1 archive and 1 folder."));

    // A failed archive leaves everything as it was.
    let before = collection.clone();
    let RequestKind::Exports { sources, others } = catalog::classify_request(&[archive_fixture("garbage.zip")]) else { panic!("an archive") };
    let error = catalog::add_sources(&mut collection, &[archive_fixture("garbage.zip")], sources, &others, &store, &temp.context()).unwrap_err();
    assert!(error.message.starts_with("The zip file is damaged"), "{}", error.message);
    assert_eq!(collection, before);

    // Loose pictures cannot take exports.
    let mut loose = open(&temp, &[fixtures().join("loose")]).collection;
    let RequestKind::Exports { sources, others } = catalog::classify_request(&[archive_fixture("export-part2.zip")]) else { panic!("an archive") };
    let refused = catalog::add_sources(&mut loose, &[archive_fixture("export-part2.zip")], sources, &others, &store, &temp.context()).unwrap_err();
    assert_eq!(refused.message, messages::CANNOT_ADD_EXPORTS_TO_PICTURES);
}

#[test]
fn descriptions_of_archive_posts_survive_reopening() {
    let temp = TempFolder::new("descriptions");
    let mut store = empty_store(&temp);
    let archive = archive_fixture("export-basic.zip");
    let mut collection = open_in(&temp, std::slice::from_ref(&archive), &store).unwrap().collection;
    let original = collection.posts[1].caption.clone();
    assert_eq!(store::save_description(&mut store, &mut collection.posts[1], "  Kept for the archive  ", 1_793_545_445_000), Ok(SaveOutcome::Saved));
    let entry = store.get(&collection.posts[1].key).expect("remembered").clone();
    assert_eq!(fold_path(&entry.path), fold_path(&archive));
    assert!(collection.posts[1].key.as_str().starts_with("archive|"));

    // Reopened, with the cache cleaned up in between: the description is back.
    let _ = fs::remove_dir_all(temp.join("cache"));
    let reloaded = DescriptionStore::load(store.path(), 1).unwrap().store;
    let reopened = open_in(&temp, std::slice::from_ref(&archive), &reloaded).unwrap();
    assert!(reopened.cache_written);
    assert_eq!(reopened.collection.posts[1].caption, "Kept for the archive");
    assert_eq!(reopened.collection.posts[1].original_caption, original);
    // The archive in another merge keeps the same keys, so the description follows it.
    let merged = open_in(&temp, &[archive_fixture("export-part1.zip"), archive.clone()], &reloaded).unwrap();
    let post = merged.collection.post(&collection.posts[1].key).expect("the same post");
    assert_eq!(post.caption, "Kept for the archive");
    // Typing the original caption removes the entry.
    let mut again = reopened.collection;
    let mut store = reloaded;
    assert_eq!(store::save_description(&mut store, &mut again.posts[1], &original, 1_793_545_446_000), Ok(SaveOutcome::Removed));
    assert!(store.is_empty());
}

fn node_is_installed() -> bool {
    Command::new("node").arg("--version").output().map(|output| output.status.success()).unwrap_or(false)
}

#[test]
fn download_all_of_archive_posts_opens_again_with_the_same_posts() {
    let temp = TempFolder::new("round-trip");
    let mut store = empty_store(&temp);
    let opened = open_in(&temp, &[archive_fixture("export-part1.zip"), archive_fixture("export-part2.zip")], &store).unwrap();
    let mut posts = opened.collection.posts.clone();
    store::save_description(&mut store, &mut posts[0], "Described in the app", 1_793_545_445_000).expect("saved");

    let target = temp.join("downloads");
    fs::create_dir_all(&target).unwrap();
    let result = export::export_posts(&posts, &target, &ExportOptions::new("instagram-export", 1_793_545_445_000, 0), &mut |_| {}, &AtomicBool::new(false)).expect("the export");
    assert_eq!(result.archives.len(), 1);
    assert!(result.problems.is_empty(), "{:?}", result.problems);
    let written = result.archives[0].path.clone();

    let reopened = open(&temp, std::slice::from_ref(&written));
    assert_eq!(ids(&reopened.collection.posts), ids(&posts));
    for (again, before) in reopened.collection.posts.iter().zip(&posts) {
        assert_eq!(again.caption, before.caption, "the description is in post.json of the download");
        assert_eq!(again.media.len(), before.media.len());
        for (one, two) in again.media.iter().zip(&before.media) {
            assert_eq!(fs::read(&one.path).unwrap(), fs::read(&two.path).unwrap(), "the bytes of the archive members");
            assert_eq!(one.kind, two.kind);
        }
    }
    assert_eq!(reopened.collection.posts[0].caption, "Described in the app");

    // The clone's validator accepts the download (when Node.js is there).
    if node_is_installed() {
        let extracted = temp.join("extracted");
        let mut reader = zip::ZipArchive::new(File::open(&written).unwrap()).unwrap();
        for index in 0..reader.len() {
            let mut member = reader.by_index(index).unwrap();
            let path = extracted.join(String::from_utf8(member.name_raw().to_vec()).expect("entry names are UTF-8"));
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            let mut bytes = Vec::new();
            member.read_to_end(&mut bytes).unwrap();
            fs::write(&path, bytes).unwrap();
        }
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("validator").join("validate-export.mjs");
        let output = Command::new("node").arg(&script).arg(&extracted).output().expect("node runs");
        let text = format!("{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        assert_eq!(output.status.code(), Some(0), "{text}");
        assert!(!text.contains("ERROR"), "{text}");
        println!("validator: {}", text.lines().last().unwrap_or_default());
    } else {
        println!("note: node is not on PATH; the validator of the clone was NOT run");
    }
}
