//! The whole way through the crate, as the app goes it: open paths, lay the stored descriptions
//! over the posts, edit a description, download (export), and read the download back.
//!
//! The first three tests are the acceptance tests of the two features this app keeps from the
//! reference web project ("Edit description", "Download post" / "Download all"): they follow the
//! clone's e2e/tests/edit-download.spec.ts with the fixtures export-basic and loose. The others
//! are a robustness sweep: many files, unusual file names, a path beyond the classic 260
//! characters, files that vanish, two windows sharing one descriptions.json, and folders that
//! cannot be written to.
//!
//! The fixtures are only read (each test that opens them checks afterwards that not one byte
//! and not one file name changed). Everything that is written goes into a folder of the test
//! below the system's temporary folder; the user profile is never touched, because the
//! settings are located through an environment the test makes up.
//!
//! The clone's validator runs under Node.js. Without `node` on PATH those steps print a note
//! and are skipped; the project itself does not need Node.js.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs::{self, File};
use std::io::{BufReader, Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Instant;

use image::{DynamicImage, ImageFormat, Rgb, RgbImage};
use instagram_core::autoview::{self, PlayerAction, PlayerState};
use instagram_core::catalog::{self, OpenContext, OpenErrorCode, OpenOptions, OpenRequest, Opened, RequestKind, SourceRequest};
use instagram_core::decode::{self, DecodeErrorKind};
use instagram_core::export::{
    self, ExportError, ExportErrorCode, ExportOptions, ExportProgress, ExportResult, DEFAULT_EXPORT_NAME,
};
use instagram_core::manifest;
use instagram_core::messages;
use instagram_core::model::{MediaKind, MediaType, Post, PostKey};
use instagram_core::settings;
use instagram_core::store::{self, DescriptionEntry, DescriptionStore, SaveOutcome};
use instagram_core::text;
use instagram_core::urls;
use serde_json::Value;
use zip::ZipArchive;

/// 2026-11-11T10:00:00Z: "now" of every test, so that nothing depends on the clock.
const NOW_MS: i64 = 1_794_391_200_000;

/// What the clone's e2e test types into the editor of the first post, as it is stored.
const EDITED_CAPTION: &str = "Croissants 🥐\nBaked at dawn.";

// --- Folders and files --------------------------------------------------------------------------

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

/// A folder of its own below the system's temporary folder, removed when the test ends.
struct TempFolder(PathBuf);

impl TempFolder {
    fn new(tag: &str) -> TempFolder {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = format!("instagram-core-e2e-{tag}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let path = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("a test folder can be created");
        TempFolder(path)
    }

    fn path(&self) -> &Path {
        &self.0
    }

    /// A folder below this one, created.
    fn folder(&self, name: &str) -> PathBuf {
        let path = self.0.join(name);
        fs::create_dir_all(&path).expect("a folder can be created");
        path
    }
}

impl Drop for TempFolder {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// The names directly in a folder, sorted.
fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .expect("the folder can be listed")
        .map(|entry| entry.expect("a folder entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// FNV-1a over the bytes of a file: enough to notice any change.
fn hash_of(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| (hash ^ u64::from(*byte)).wrapping_mul(0x0100_0000_01b3))
}

/// Every folder and file below `root` with the size and the hash of its content: two snapshots
/// are equal only when no file was added, removed, renamed or changed.
fn snapshot(root: &Path) -> BTreeMap<String, (u64, u64)> {
    fn walk(folder: &Path, prefix: &str, found: &mut BTreeMap<String, (u64, u64)>) {
        for entry in fs::read_dir(folder).expect("a fixture folder can be listed") {
            let entry = entry.expect("a folder entry");
            let name = format!("{prefix}{}", entry.file_name().to_string_lossy());
            if entry.file_type().expect("a file type").is_dir() {
                found.insert(format!("{name}/"), (0, 0));
                walk(&entry.path(), &format!("{name}/"), found);
            } else {
                let bytes = fs::read(entry.path()).expect("a fixture file can be read");
                found.insert(name, (bytes.len() as u64, hash_of(&bytes)));
            }
        }
    }
    let mut found = BTreeMap::new();
    walk(root, "", &mut found);
    found
}

/// A real, tiny PNG picture; `shade` makes the files differ.
fn tiny_png(shade: u8) -> Vec<u8> {
    let picture = RgbImage::from_pixel(4, 3, Rgb([shade, 120, 255 - shade]));
    let mut bytes = Vec::new();
    DynamicImage::ImageRgb8(picture)
        .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
        .expect("a picture can be encoded");
    bytes
}

// --- Opening ------------------------------------------------------------------------------------

/// A request to open one file or folder, with the author of loose pictures.
fn request(path: &Path, username: &str) -> OpenRequest {
    OpenRequest { paths: vec![path.to_path_buf()], options: OpenOptions { default_username: username.to_owned() } }
}

/// Where these tests would unpack an archive.
fn context() -> OpenContext {
    OpenContext::new(std::env::temp_dir().join(format!("instagram-core-end-to-end-cache-{}", std::process::id())))
}

/// Opens one file or folder, which must work.
fn open(path: &Path, username: &str, store: &DescriptionStore) -> Opened {
    catalog::open_paths(&request(path, username), store, &context())
        .unwrap_or_else(|error| panic!("{} did not open: {error}", path.display()))
}

fn ids(posts: &[Post]) -> Vec<&str> {
    posts.iter().map(|post| post.id.as_str()).collect()
}

fn captions(posts: &[Post]) -> Vec<&str> {
    posts.iter().map(|post| post.caption.as_str()).collect()
}

fn file_names(posts: &[Post]) -> Vec<String> {
    posts.iter().map(|post| post.file_name().unwrap_or_default()).collect()
}

/// A store that is read from disk as the app does at start; nothing may be wrong with the file.
fn load_store(path: &Path) -> DescriptionStore {
    let loaded = DescriptionStore::load(path, NOW_MS).expect("descriptions.json can be read");
    assert_eq!(loaded.notice, None);
    loaded.store
}

// --- Downloading --------------------------------------------------------------------------------

/// Runs an export that nobody cancels, with the progress reports it made.
fn try_download(posts: &[Post], target: &Path, name: &str) -> (Result<ExportResult, ExportError>, Vec<(usize, usize)>) {
    let mut reports = Vec::new();
    let result = export::export_posts(
        posts,
        target,
        &ExportOptions::new(name, NOW_MS, 0),
        &mut |progress: ExportProgress| reports.push((progress.done, progress.total)),
        &AtomicBool::new(false),
    );
    (result, reports)
}

/// An export that must succeed; its progress runs from "0 of N" to "N of N".
fn download(posts: &[Post], target: &Path, name: &str) -> ExportResult {
    let (result, reports) = try_download(posts, target, name);
    let result = result.unwrap_or_else(|error| panic!("the export failed: {error}"));
    assert_eq!(reports.first(), Some(&(0, posts.len())));
    assert_eq!(reports.last(), Some(&(posts.len(), posts.len())));
    assert_eq!(reports.len(), posts.len() + 1);
    result
}

/// The entries of an archive in directory order: name and content. Reading checks every size
/// and CRC-32.
fn read_archive(path: &Path) -> Vec<(String, Vec<u8>)> {
    let file = File::open(path).expect("the archive exists");
    let mut archive = ZipArchive::new(BufReader::new(file)).expect("the archive has a directory");
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).expect("an entry can be opened");
        let name = String::from_utf8(entry.name_raw().to_vec()).expect("entry names are UTF-8");
        let mut bytes = Vec::new();
        entry.read_to_end(&mut bytes).expect("an entry can be read and its CRC-32 is right");
        entries.push((name, bytes));
    }
    entries
}

fn sorted_names(entries: &[(String, Vec<u8>)]) -> Vec<&str> {
    let mut names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
    names.sort_unstable();
    names
}

fn bytes_of<'a>(entries: &'a [(String, Vec<u8>)], name: &str) -> &'a [u8] {
    match entries.iter().find(|(entry, _)| entry == name) {
        Some((_, bytes)) => bytes,
        None => panic!("the archive has no entry \"{name}\""),
    }
}

fn post_json(entries: &[(String, Vec<u8>)]) -> Value {
    assert_eq!(entries.last().map(|(name, _)| name.as_str()), Some("post.json"), "post.json is the last entry");
    serde_json::from_slice(bytes_of(entries, "post.json")).expect("post.json is JSON")
}

/// Extracts an archive into `folder`, as a user would before opening or validating it.
fn extract(archive: &Path, folder: &Path) {
    for (name, bytes) in read_archive(archive) {
        let path = folder.join(&name);
        fs::create_dir_all(path.parent().expect("an entry has a folder")).expect("a folder can be created");
        fs::write(&path, &bytes).expect("an entry can be extracted");
    }
}

// --- The clone's validator ----------------------------------------------------------------------

fn node_is_installed() -> bool {
    Command::new("node").arg("--version").output().map(|output| output.status.success()).unwrap_or(false)
}

/// Runs `node validate-export.mjs <folder>` and checks the acceptance rule of the design brief:
/// exit code 0, no ERROR, and exactly `url_warnings` warnings, each the one about a missing
/// "url" (0 for an export whose posts have links, one per post for loose pictures). Returns
/// false, after printing a note, when Node.js is not installed.
fn validator_accepts(folder: &Path, url_warnings: usize) -> bool {
    if !node_is_installed() {
        println!("note: node is not on PATH; the validator of the clone was NOT run on {}", folder.display());
        return false;
    }
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("validator").join("validate-export.mjs");
    let output = Command::new("node").arg(&script).arg(folder).output().expect("node can be started");
    let mut lines: Vec<String> = String::from_utf8_lossy(&output.stdout).lines().map(str::to_owned).collect();
    lines.extend(String::from_utf8_lossy(&output.stderr).lines().map(str::to_owned));

    assert_eq!(output.status.code(), Some(0), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.contains("ERROR")), "{lines:#?}");
    let warnings: Vec<&String> = lines.iter().filter(|line| line.starts_with("WARN")).collect();
    assert_eq!(warnings.len(), url_warnings, "{lines:#?}");
    for warning in warnings {
        assert!(warning.ends_with("no \"url\"; the clone cannot link back to Instagram"), "{lines:#?}");
    }
    let verdict = lines.last().cloned().unwrap_or_default();
    assert!(verdict.starts_with("OK: "), "{lines:#?}");
    println!("validator ran on {}: exit 0, {verdict}", folder.display());
    true
}

// --- Comparing posts ----------------------------------------------------------------------------

/// What an export must carry over, of one post: everything the format has a field for. Paths
/// are left out (an export numbers its files anew), the bytes of the files stand in for them.
#[derive(Debug, PartialEq)]
struct Carried {
    id: String,
    permalink: Option<String>,
    caption: String,
    username: String,
    full_name: Option<String>,
    verified: Option<bool>,
    profile_url: Option<String>,
    avatar: Option<u64>,
    comment_count: u64,
    like_count: Option<u64>,
    taken_at: Option<i64>,
    media_type: MediaType,
    media: Vec<(MediaKind, u64, u64)>,
    alt: Option<String>,
    location: Option<String>,
    comments: Vec<(String, String, u64, Option<i64>)>,
}

fn carried(post: &Post) -> Carried {
    let file_hash = |path: &Path| hash_of(&fs::read(path).unwrap_or_else(|error| panic!("{}: {error}", path.display())));
    Carried {
        id: post.id.clone(),
        permalink: post.permalink.clone(),
        caption: post.caption.clone(),
        username: post.user.username.clone(),
        full_name: post.user.full_name.clone(),
        verified: post.user.verified,
        profile_url: post.user.profile_url.clone(),
        avatar: post.user.avatar.as_deref().map(file_hash),
        comment_count: post.comment_count,
        like_count: post.like_count,
        taken_at: post.taken_at,
        media_type: post.media_type,
        media: post.media.iter().map(|media| (media.kind, media.bytes, file_hash(&media.path))).collect(),
        alt: post.alt.clone(),
        location: post.location.clone(),
        comments: post
            .comments
            .iter()
            .map(|comment| (comment.username.clone(), comment.text.clone(), comment.likes, comment.taken_at))
            .collect(),
    }
}

fn all_carried(posts: &[Post]) -> Vec<Carried> {
    posts.iter().map(carried).collect()
}

/// `all_carried` for loose pictures that were exported and read back. The one difference the
/// format makes: an export never says "this author has no Instagram profile", so the reader
/// guesses the profile link from the username, as the clone's reader does for every entry
/// without a link of its own. The guess is checked and then left out of the comparison.
fn all_carried_from_loose_export(posts: &[Post]) -> Vec<Carried> {
    posts
        .iter()
        .map(|post| {
            let mut carried = carried(post);
            assert_eq!(carried.profile_url, urls::profile_url_for(&post.user.username), "{}", post.id);
            carried.profile_url = None;
            carried
        })
        .collect()
}

// --- (a) An export: edit a description, download all, download one post --------------------------

#[test]
fn an_opened_export_keeps_an_edited_description_and_downloads_like_the_clone() {
    let work = TempFolder::new("basic");
    let fixture = fixtures().join("export-basic");
    let before = snapshot(&fixture);

    // The app's files, located as the app locates them, in a made-up profile.
    let env: HashMap<String, String> = [
        ("APPDATA".to_owned(), work.path().join("Roaming").display().to_string()),
        ("USERNAME".to_owned(), "Jane Doe".to_owned()),
    ]
    .into_iter()
    .collect();
    let settings = settings::load_or_create(&env).expect("the first start creates the settings");
    assert_eq!(settings.default_username, "JaneDoe");
    assert_eq!(settings.paths.data_dir, work.path().join("Roaming").join("instagram-windows-app").join("data"));
    let mut store = load_store(&settings.paths.descriptions_file);
    assert!(store.is_empty());

    // Open the folder: six posts, in the order of post.json.
    let RequestKind::Exports { sources, .. } = catalog::classify_request(std::slice::from_ref(&fixture)) else {
        panic!("a folder with post.json is an export");
    };
    let [SourceRequest::Folder { manifest: found }] = sources.as_slice() else { panic!("one export folder") };
    assert_eq!(PostKey::for_file(found), PostKey::for_file(&fixture.join("post.json")));
    let Opened { mut collection, focus, .. } = open(&fixture, &settings.default_username, &store);
    assert_eq!(focus, None);
    assert!(collection.is_export());
    assert_eq!(
        ids(&collection.posts),
        ["FXTbasic001", "FXTbasic002", "FXTbasic003", "FXTbasic004", "FXTbasic005", "FXTbasic006"]
    );
    assert_eq!(collection.warnings, Vec::new());
    assert_eq!((collection.label.as_str(), collection.total_entries), ("export-basic", 6));
    assert_eq!(collection.folder.as_deref(), Some(fixture.as_path()));
    assert_eq!(collection.source_url.as_deref(), Some("https://www.instagram.com/fixture.sunrise.bakery/"));
    assert_eq!(
        messages::showing_banner(collection.posts.len(), collection.total_entries, true, &collection.label),
        "Showing 6 posts from export-basic"
    );
    let [source] = collection.sources() else {
        panic!("the collection is an export");
    };
    assert_eq!(source.path, fixture);
    assert_eq!(source.manifest, fixture.join("post.json"));
    // The authors are those of the export, not the default author of loose pictures.
    assert_eq!(collection.posts[0].user.username, "fixture.sunrise.bakery");

    // Edit the description of the first post, as the editor hands the text over.
    let original = collection.posts[0].caption.clone();
    assert!(original.starts_with("Fresh out of the oven 🥐☀️\n"), "{original}");
    let typed = "  Croissants 🥐\r\nBaked at dawn.  ";
    assert!(text::caption_changed(typed, &original));
    let outcome = store::save_description(&mut store, &mut collection.posts[0], typed, NOW_MS);
    assert_eq!(outcome, Ok(SaveOutcome::Saved));
    assert_eq!(collection.posts[0].caption, EDITED_CAPTION);
    assert_eq!(collection.posts[0].original_caption, original);
    assert!(collection.posts[0].is_edited());
    // Saving the same text again writes nothing.
    let again = store::save_description(&mut store, &mut collection.posts[0], typed, NOW_MS + 5);
    assert_eq!(again, Ok(SaveOutcome::Unchanged));
    assert_eq!(names_in(&settings.paths.data_dir), ["descriptions.json"]);

    // A new start: the store comes from disk and is laid over the freshly opened export.
    let reread = load_store(&settings.paths.descriptions_file);
    assert_eq!(reread, store);
    let entry = reread.get(&collection.posts[0].key).expect("the description is remembered");
    assert_eq!(entry.caption, EDITED_CAPTION);
    assert_eq!(entry.post_id.as_deref(), Some("FXTbasic001"));
    assert_eq!(entry.path, fixture.join("post.json"));
    assert_eq!((entry.file_bytes, entry.edited_at), (Some(26_284), NOW_MS));
    let reopened = open(&fixture, &settings.default_username, &reread).collection;
    assert_eq!(reopened.posts[0].caption, EDITED_CAPTION);
    assert_eq!(reopened.posts[0].original_caption, original);
    assert!(reopened.posts[1..].iter().all(|post| !post.is_edited()));
    assert_eq!(reopened.posts, collection.posts);
    // The manifest file itself opens the same export; without the store the original is back.
    let by_file = open(&fixture.join("post.json"), "someone", &reread).collection;
    assert_eq!(by_file.posts, collection.posts);
    let mut plain = open(&fixture, "someone", &DescriptionStore::new(PathBuf::from("unused.json"))).collection;
    assert_eq!(plain.posts[0].caption, original);
    catalog::apply_descriptions(&mut plain, &reread);
    assert_eq!(plain.posts, collection.posts);

    // Download all: the entry list of the clone's e2e test, except that the HEIC picture stays
    // the HEIC file it is (the clone exports the JPEG its browser converted it to).
    let downloads = work.folder("downloads");
    let all = download(&reopened.posts, &downloads, DEFAULT_EXPORT_NAME);
    assert_eq!(all.problems, Vec::<String>::new());
    assert_eq!((all.posts, all.archives.len()), (6, 1));
    assert_eq!(all.archives[0].name, "instagram-export.zip");
    assert_eq!(all.archives[0].path, downloads.join("instagram-export.zip"));
    assert_eq!(export::download_message(&all), "Downloaded 6 posts as instagram-export.zip.");
    assert_eq!(export::download_toast_ms(&all), 4000);
    let entries = read_archive(&all.archives[0].path);
    assert_eq!(
        sorted_names(&entries),
        [
            "avatars/fixture.sunrise.bakery.jpg",
            "avatars/fixture_tidewater.jpg",
            "post.json",
            "posts/post1.jpg",
            "posts/post2.png",
            "posts/post3.webp",
            "posts/post4.heic",
            "posts/post5_1.jpg",
            "posts/post5_2.jpg",
            "posts/post6.jpg",
        ]
    );
    // The bytes on disk, never decoded pixels.
    for name in sorted_names(&entries) {
        if name != "post.json" {
            assert_eq!(bytes_of(&entries, name), fs::read(fixture.join(name)).expect("a fixture file"), "{name}");
        }
    }
    let written = post_json(&entries);
    let written_posts = written["posts"].as_array().expect("a posts list");
    let written_ids: Vec<&str> = written_posts.iter().filter_map(|post| post["id"].as_str()).collect();
    assert_eq!(written_ids, ids(&collection.posts));
    assert_eq!(written_posts[0]["caption"], EDITED_CAPTION);
    assert_eq!(written_posts[0]["media"][0]["path"], "posts/post1.jpg");
    assert_eq!(written_posts[0]["user"]["avatar"], "avatars/fixture.sunrise.bakery.jpg");
    assert_eq!(written_posts[2]["caption"], "Quiet morning in the green room 🌿");
    assert_eq!(written_posts[3]["media"][0]["path"], "posts/post4.heic");
    assert_eq!(written["source"], serde_json::json!({ "type": "posts" }));

    // Download one post: the HEIC one.
    let fourth = reopened.posts.iter().find(|post| post.id == "FXTbasic004").expect("the fourth post");
    let single_name = export::single_post_export_name(&fourth.id);
    let one = download(std::slice::from_ref(fourth), &downloads, &single_name);
    assert_eq!(one.archives[0].name, "instagram-export-FXTbasic004.zip");
    assert_eq!(export::download_message(&one), "Downloaded 1 post as instagram-export-FXTbasic004.zip.");
    let single = read_archive(&one.archives[0].path);
    assert_eq!(sorted_names(&single), ["post.json", "posts/post1.heic"]);
    assert_eq!(bytes_of(&single, "posts/post1.heic"), fs::read(fixture.join("posts/post4.heic")).expect("the HEIC"));
    assert_eq!(names_in(&downloads), ["instagram-export-FXTbasic004.zip", "instagram-export.zip"]);

    // Both downloads pass the clone's validator without a single warning.
    let all_folder = work.folder("extracted-all");
    let one_folder = work.folder("extracted-one");
    extract(&all.archives[0].path, &all_folder);
    extract(&one.archives[0].path, &one_folder);
    validator_accepts(&all_folder, 0);
    validator_accepts(&one_folder, 0);

    // Auto view runs over the same posts, identified by their keys.
    let keys: Vec<PostKey> = reopened.posts.iter().map(|post| post.key.clone()).collect();
    let mut player = PlayerState::new(autoview::start_cursor(&keys, 4, false, &mut || 0.0));
    assert_eq!(player.current(), Some(&keys[4]));
    assert!(player.apply(PlayerAction::Advance { slide_count: reopened.posts[4].media.len(), loop_posts: true }));
    assert_eq!((player.current(), player.slide), (Some(&keys[4]), 1));
    assert!(player.apply(PlayerAction::Advance { slide_count: 2, loop_posts: true }));
    assert_eq!(player.current(), Some(&keys[5]));

    // Typing the original caption again forgets the entry.
    let back = store::save_description(&mut store, &mut collection.posts[0], &original, NOW_MS + 9);
    assert_eq!(back, Ok(SaveOutcome::Removed));
    assert!(load_store(&settings.paths.descriptions_file).is_empty());

    // The export folder is exactly as it was.
    assert_eq!(snapshot(&fixture), before);
}

// --- (c) What was downloaded reads back as the same posts ---------------------------------------

#[test]
fn a_downloaded_export_reads_back_as_the_same_posts() {
    let work = TempFolder::new("parity");
    let fixture = fixtures().join("export-basic");
    let before = snapshot(&fixture);
    let mut store = DescriptionStore::new(work.path().join("descriptions.json"));

    let mut collection = open(&fixture, "nobody", &store).collection;
    assert!(collection.is_export());
    for (index, typed) in [(0, EDITED_CAPTION), (4, "Two tides 🌊🌊\n\n#carousel")] {
        let saved = store::save_description(&mut store, &mut collection.posts[index], typed, NOW_MS);
        assert_eq!(saved, Ok(SaveOutcome::Saved));
    }

    let downloads = work.folder("downloads");
    let result = download(&collection.posts, &downloads, DEFAULT_EXPORT_NAME);
    assert_eq!(result.problems, Vec::<String>::new());
    let extracted = work.folder("extracted");
    extract(&result.archives[0].path, &extracted);

    // Through the manifest reader directly ...
    let reread = manifest::load_export(&extracted.join("post.json")).expect("the download is an export");
    assert_eq!(reread.warnings, Vec::new());
    assert_eq!(reread.total_entries, 6);
    assert_eq!(reread.exported_at, Some(NOW_MS));
    assert_eq!(reread.source_url, None);
    assert_eq!(all_carried(&reread.posts), all_carried(&collection.posts));
    // ... where an edited description has become the caption of the export.
    assert_eq!(reread.posts[0].original_caption, EDITED_CAPTION);
    assert!(reread.posts.iter().all(|post| !post.is_edited()));
    // The media types survive: the carousel, and the reel that is exported as its cover.
    let types: Vec<MediaType> = reread.posts.iter().map(|post| post.media_type).collect();
    assert_eq!(
        types,
        [MediaType::Image, MediaType::Image, MediaType::Image, MediaType::Image, MediaType::Carousel, MediaType::Video]
    );
    assert_eq!(reread.posts[4].media.len(), 2);
    assert_eq!(reread.posts[0].comments.len(), 2);

    // ... and through the catalog, as the user opens the extracted folder.
    let opened = open(&extracted, "nobody", &DescriptionStore::new(work.path().join("other.json")));
    assert!(opened.collection.is_export());
    assert_eq!(opened.collection.posts, reread.posts);

    // A second generation is the same again: exporting the re-read posts changes nothing.
    let second = download(&opened.collection.posts, &work.folder("second"), DEFAULT_EXPORT_NAME);
    let first_entries = read_archive(&result.archives[0].path);
    let second_entries = read_archive(&second.archives[0].path);
    assert_eq!(first_entries, second_entries);

    validator_accepts(&extracted, 0);
    assert_eq!(snapshot(&fixture), before);
}

// --- (b) Loose pictures: open one file, edit, download, open the download ------------------------

#[test]
fn loose_pictures_round_trip_through_a_download() {
    let work = TempFolder::new("loose");
    let fixture = fixtures().join("loose");
    let before = snapshot(&fixture);
    let descriptions = work.path().join("data").join("descriptions.json");
    let mut store = load_store(&descriptions);

    // One picked file: its siblings come with it, in Explorer's order, and the viewer opens on it.
    let picked = fixture.join("IMG_2.jpg");
    assert_eq!(catalog::classify_request(std::slice::from_ref(&picked)), RequestKind::Loose);
    let Opened { mut collection, focus, .. } = open(&picked, "jane.doe", &store);
    assert!(!collection.is_export());
    assert_eq!(
        file_names(&collection.posts),
        ["img_1.png", "IMG_2.jpg", "IMG_10.jpg", "portrait.heic", "Sunset at the pier (final).webp"]
    );
    assert_eq!(focus, Some(1));
    assert_eq!(ids(&collection.posts), ["img_1", "IMG_2", "IMG_10", "portrait", "Sunset_at_the_pier_final_"]);
    assert_eq!((collection.label.as_str(), collection.total_entries), ("loose", 5));
    assert_eq!(collection.folder.as_deref(), Some(fixture.as_path()));
    // The empty picture is reported; the text file beside the pictures is nobody's business.
    let warnings: Vec<&str> = collection.warnings.iter().map(|warning| warning.message.as_str()).collect();
    assert_eq!(warnings, ["\"broken.jpg\" is an empty file."]);
    assert_eq!(messages::showing_banner(5, 5, false, &collection.label), "Showing 5 pictures from loose");
    for post in &collection.posts {
        assert_eq!(post.user.username, "jane.doe");
        assert_eq!((post.caption.as_str(), post.comment_count, post.like_count), ("", 0, None));
        assert_eq!(post.permalink, None);
        assert_eq!(post.context, post.file_name());
        assert_eq!(post.media.len(), 1);
        assert!(post.taken_at.is_some());
    }

    // Two descriptions.
    let first_text = "Low tide again 🌊\n朝ごはん at the pier";
    let last_text = "It's 100% \"final\" — really.\n\nThird line \\ with a backslash.";
    for (index, typed, at) in [(0, first_text, NOW_MS), (4, last_text, NOW_MS + 1)] {
        let saved = store::save_description(&mut store, &mut collection.posts[index], typed, at);
        assert_eq!(saved, Ok(SaveOutcome::Saved));
    }
    assert_eq!(captions(&collection.posts), [first_text, "", "", "", last_text]);
    assert!(collection.posts[1..4].iter().all(|post| !post.is_edited()));

    // Another instance of the store reads what this one wrote; opening the folder (not one
    // file) gives the same posts with the descriptions on them and no focus.
    let second_store = load_store(&descriptions);
    assert_eq!(second_store, store);
    let entry = second_store.get(&PostKey::for_file(&fixture.join("img_1.png"))).expect("a description");
    assert_eq!((entry.caption.as_str(), entry.post_id.as_deref()), (first_text, None));
    assert_eq!(entry.path, fixture.join("img_1.png"));
    let by_folder = open(&fixture, "jane.doe", &second_store);
    assert_eq!(by_folder.focus, None);
    assert_eq!(by_folder.collection.posts, collection.posts);

    // Download all.
    let downloads = work.folder("downloads");
    let result = download(&collection.posts, &downloads, DEFAULT_EXPORT_NAME);
    assert_eq!(result.problems, Vec::<String>::new());
    assert_eq!(export::download_message(&result), "Downloaded 5 posts as instagram-export.zip.");
    let entries = read_archive(&result.archives[0].path);
    let names: Vec<&str> = entries.iter().map(|(name, _)| name.as_str()).collect();
    assert_eq!(
        names,
        ["posts/post1.png", "posts/post2.jpg", "posts/post3.jpg", "posts/post4.heic", "posts/post5.webp", "post.json"]
    );
    let written = post_json(&entries);
    assert_eq!(written["posts"][0]["caption"], first_text);
    assert_eq!(written["posts"][0]["like_count"], Value::Null);
    // The file name under the username is the app's own and is not exported.
    assert!(written["posts"][0].get("location").is_none() && written["posts"][0].get("url").is_none());

    // Open the EXTRACTED download: a full write -> read round trip.
    let extracted = work.folder("extracted");
    extract(&result.archives[0].path, &extracted);
    let reopened = open(&extracted, "someone.else", &DescriptionStore::new(work.path().join("none.json")));
    assert!(reopened.collection.is_export());
    assert_eq!(reopened.collection.warnings, Vec::new());
    assert_eq!(captions(&reopened.collection.posts), captions(&collection.posts));
    assert_eq!(all_carried_from_loose_export(&reopened.collection.posts), all_carried(&collection.posts));
    // The author travels with the export; the default author of the reader plays no part.
    assert!(reopened.collection.posts.iter().all(|post| post.user.username == "jane.doe"));
    assert!(reopened.collection.posts.iter().all(|post| post.context.is_none()));

    // The clone's validator: the only warning allowed is the missing "url", once per picture.
    validator_accepts(&extracted, 5);

    // Nothing was written beside the pictures.
    assert_eq!(snapshot(&fixture), before);
}

// --- Robustness: many pictures -------------------------------------------------------------------

#[test]
fn a_folder_of_two_thousand_pictures_opens_at_once() {
    const PICTURES: usize = 2000;
    let work = TempFolder::new("many");
    let folder = work.folder("Holiday 2026");
    let shades: Vec<Vec<u8>> = (0..8).map(|shade| tiny_png(shade * 30)).collect();
    for number in 1..=PICTURES {
        fs::write(folder.join(format!("IMG_{number}.png")), &shades[number % shades.len()]).expect("a picture");
    }
    for number in 1..=20 {
        fs::write(folder.join(format!("notes {number}.txt")), b"not a picture").expect("a text file");
    }
    let empty = DescriptionStore::new(work.path().join("descriptions.json"));

    let started = Instant::now();
    let opened = open(&folder, "tester", &empty);
    let folder_time = started.elapsed();
    assert_eq!(opened.collection.posts.len(), PICTURES);
    assert_eq!(opened.collection.warnings, Vec::new());
    assert_eq!(opened.collection.label, "Holiday 2026");
    // Explorer's order: 1, 2, 3 ... 10, 11 ... not 1, 10, 100.
    for (index, post) in opened.collection.posts.iter().enumerate() {
        assert_eq!(post.id, format!("IMG_{}", index + 1));
    }

    let started = Instant::now();
    let around = open(&folder.join("IMG_1500.png"), "tester", &empty);
    let file_time = started.elapsed();
    assert_eq!(around.focus, Some(1499));
    assert_eq!(around.collection.posts, opened.collection.posts);

    // A description for every one of them: one file, written once, read once.
    let mut store = DescriptionStore::new(work.path().join("descriptions.json"));
    for post in &opened.collection.posts {
        let entry = DescriptionEntry {
            caption: format!("Description of {}", post.id),
            path: post.media[0].path.clone(),
            post_id: None,
            file_bytes: Some(post.media[0].bytes),
            file_modified: post.taken_at,
            edited_at: NOW_MS,
        };
        store.set(&post.key, entry);
    }
    store.save().expect("the descriptions can be saved");
    let started = Instant::now();
    let reread = load_store(store.path());
    let described = open(&folder, "tester", &reread);
    let described_time = started.elapsed();
    assert_eq!(reread.len(), PICTURES);
    assert!(described.collection.posts.iter().all(|post| post.caption == format!("Description of {}", post.id)));

    let downloads = work.folder("downloads");
    let started = Instant::now();
    let result = download(&described.collection.posts, &downloads, DEFAULT_EXPORT_NAME);
    let export_time = started.elapsed();
    assert_eq!((result.posts, result.problems.len(), result.archives.len()), (PICTURES, 0, 1));
    let archive = ZipArchive::new(BufReader::new(File::open(&result.archives[0].path).expect("the archive")))
        .expect("the archive has a directory");
    assert_eq!(archive.len(), PICTURES + 1);

    println!(
        "{PICTURES} pictures: folder opened in {folder_time:?}, one file with its siblings in {file_time:?}, \
         with {PICTURES} descriptions (read + open) in {described_time:?}, exported in {export_time:?}"
    );
    // "At once" for a person; the limit is generous because the tests of this file run side by
    // side on a small machine.
    for (what, time) in [("folder", folder_time), ("file", file_time), ("described", described_time)] {
        assert!(time.as_secs_f64() < 5.0, "opening ({what}) took {time:?}");
    }
}

// --- Robustness: files above the size limits of an export ----------------------------------------

#[test]
fn files_of_an_export_above_the_size_limits_are_skipped_with_the_clones_sentence() {
    const MIB: u64 = 1024 * 1024;
    let work = TempFolder::new("limits");
    let root = work.folder("big export");
    let posts = root.join("posts");
    fs::create_dir(&posts).expect("a folder can be created");
    // Extending a file does not write its bytes, so files of this size cost nothing here.
    let sized = |name: &str, head: &[u8], bytes: u64| {
        let path = posts.join(name);
        fs::write(&path, head).expect("a file can be written");
        File::options().write(true).open(&path).and_then(|file| file.set_len(bytes)).expect("a file can be extended");
    };
    let mp4 = b"\x00\x00\x00\x14ftypmp42\x20\x20\x20\x20mp42";
    sized("ok.png", &tiny_png(10), tiny_png(10).len() as u64);
    sized("limit.png", &tiny_png(20), manifest::MAX_IMAGE_BYTES);
    sized("huge.png", &tiny_png(30), manifest::MAX_IMAGE_BYTES + 1);
    sized("clip.mp4", mp4, manifest::MAX_VIDEO_BYTES + 1);
    sized("short.mp4", mp4, 65 * MIB);
    let listed = serde_json::json!({
        "posts": [
            { "id": "ok", "user": "someone", "image": "posts/ok.png" },
            { "id": "limit", "user": "someone", "image": "posts/limit.png" },
            { "id": "huge", "user": "someone", "image": "posts/huge.png" },
            { "id": "clip", "user": "someone", "video": "posts/clip.mp4" },
            { "id": "short", "user": "someone", "video": "posts/short.mp4" },
        ]
    });
    fs::write(root.join("post.json"), listed.to_string()).expect("post.json can be written");

    let collection = open(&root, "tester", &DescriptionStore::new(work.path().join("none.json"))).collection;
    assert_eq!(ids(&collection.posts), ["ok", "limit", "short"]);
    assert_eq!(collection.total_entries, 5);
    let warnings: Vec<&str> = collection.warnings.iter().map(|warning| warning.message.as_str()).collect();
    assert_eq!(
        warnings,
        [
            "Post 3: \"posts/huge.png\" is larger than 64 MB and was skipped.",
            "Post 4: \"posts/clip.mp4\" is larger than 1 GB and was skipped.",
        ]
    );
    assert_eq!(
        messages::showing_banner(collection.posts.len(), collection.total_entries, true, &collection.label),
        "Showing 3 of 5 posts from big export"
    );
    // A picture exactly at the limit and a video above the picture limit are posts.
    assert_eq!(collection.posts[1].media[0].bytes, 64 * MIB);
    assert_eq!((collection.posts[2].media[0].kind, collection.posts[2].media_type), (MediaKind::Video, MediaType::Video));
    assert!(collection.posts[2].is_video());
    assert_eq!(collection.posts[2].media[0].display_path(), None);
}

// --- Robustness: unusual file names --------------------------------------------------------------

#[test]
fn unusual_file_names_open_keep_their_descriptions_and_export() {
    let work = TempFolder::new("names");
    let folder = work.folder("Bilder & Fotos (2026) 🥐");
    let long_name = format!("long {}.png", "x".repeat(230));
    let names: Vec<String> = [
        "🥐 croissant 👨‍👩‍👧.png",
        "朝ごはん.png",
        "한국어 사진.png",
        "тест файл.png",
        "عربى.png",
        "100% done.png",
        "%41%20encoded%.png",
        "it's mine.png",
        "'.png",
        "two  spaces  inside.png",
        "ünïcödé ÀÉÎ.PNG",
        "semi;colon, comma & amp.png",
        "#hash+plus=equals@at.png",
        "(1).png",
        "brackets [1] {2}.png",
        ".leading-dot.png",
        "dots.in.the.name.v1.2.png",
        "no extension",
        "IMG_2.PNG",
        "img_2.jpg.png",
    ]
    .into_iter()
    .map(str::to_owned)
    .chain([long_name.clone()])
    .collect();
    for (index, name) in names.iter().enumerate() {
        fs::write(folder.join(name), tiny_png(index as u8 * 10)).unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    let descriptions = work.path().join("data").join("descriptions.json");
    let mut store = load_store(&descriptions);

    let mut collection = open(&folder, "tester", &store).collection;
    assert_eq!(collection.warnings, Vec::new());
    assert_eq!(collection.posts.len(), names.len());
    assert_eq!(collection.label, "Bilder & Fotos (2026) 🥐");

    // Every file is there under its own name, in the order of text::natural_cmp.
    let mut expected = names.clone();
    expected.sort_by(|left, right| text::natural_cmp(left, right));
    assert_eq!(file_names(&collection.posts), expected);
    let mut seen_ids = HashSet::new();
    for post in &collection.posts {
        let name = post.file_name().expect("a file name");
        assert_eq!(post.context.as_deref(), Some(name.as_str()));
        assert_eq!(post.media[0].manifest_path, name);
        assert_eq!(post.key, PostKey::for_file(&folder.join(&name)));
        // Ids are what the export format allows, and unique.
        assert!(!post.id.is_empty() && post.id.len() <= 100, "{name}: {}", post.id);
        assert!(post.id.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')), "{name}: {}", post.id);
        assert!(post.id.chars().any(|c| c.is_ascii_alphanumeric()), "{name}: {}", post.id);
        assert!(seen_ids.insert(post.id.clone()), "{name}: the id {} is used twice", post.id);
        // Each one decodes, also the one whose path is longer than 260 characters.
        let decoded = decode::decode_picture(&post.media[0].path, 64).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!((decoded.width, decoded.height), (4, 3), "{name}");
    }

    // A description on every picture, its own file name among the text.
    for index in 0..collection.posts.len() {
        let typed = format!("About {} ✨", collection.posts[index].file_name().unwrap_or_default());
        let saved = store::save_description(&mut store, &mut collection.posts[index], &typed, NOW_MS);
        assert_eq!(saved, Ok(SaveOutcome::Saved), "{typed}");
    }
    let on_disk: Value = serde_json::from_str(&fs::read_to_string(&descriptions).expect("descriptions.json"))
        .expect("descriptions.json is JSON");
    assert_eq!(on_disk["descriptions"].as_object().map(|entries| entries.len()), Some(names.len()));
    let reopened = open(&folder, "tester", &load_store(&descriptions)).collection;
    assert_eq!(reopened.posts, collection.posts);
    for post in &reopened.posts {
        assert_eq!(post.caption, format!("About {} ✨", post.file_name().unwrap_or_default()));
    }
    // One picked file with such a name finds itself among its siblings.
    for name in ["🥐 croissant 👨‍👩‍👧.png", "100% done.png", "it's mine.png", long_name.as_str()] {
        let around = open(&folder.join(name), "tester", &store);
        let focus = around.focus.unwrap_or_else(|| panic!("{name} has no focus"));
        assert_eq!(around.collection.posts[focus].file_name().as_deref(), Some(name));
    }

    // Download all, and every post on its own (the id names the archive).
    let downloads = work.folder("downloads");
    let result = download(&collection.posts, &downloads, DEFAULT_EXPORT_NAME);
    assert_eq!(result.problems, Vec::<String>::new());
    assert_eq!(result.posts, names.len());
    for post in &collection.posts {
        let single = download(std::slice::from_ref(post), &downloads, &export::single_post_export_name(&post.id));
        assert_eq!((single.posts, single.problems.len()), (1, 0), "{}", post.id);
    }
    assert_eq!(names_in(&downloads).len(), names.len() + 1);

    let extracted = work.folder("extracted");
    extract(&result.archives[0].path, &extracted);
    let back = open(&extracted, "tester", &DescriptionStore::new(work.path().join("none.json"))).collection;
    assert_eq!(back.warnings, Vec::new());
    assert_eq!(all_carried_from_loose_export(&back.posts), all_carried(&collection.posts));
    validator_accepts(&extracted, names.len());
}

// --- Robustness: a path beyond 260 characters ----------------------------------------------------

#[test]
fn a_path_longer_than_260_characters_opens_decodes_and_exports() {
    let work = TempFolder::new("long");
    let mut folder = work.path().to_path_buf();
    for part in ["a".repeat(100), format!("{} deep folder", "b".repeat(100)), "c".repeat(100)] {
        folder.push(part);
    }
    fs::create_dir_all(&folder).expect("a deep folder can be created");
    assert!(folder.as_os_str().len() > 320, "{}", folder.display());
    for name in ["IMG_2.jpg", "img_1.png", "portrait.heic"] {
        fs::copy(fixtures().join("loose").join(name), folder.join(name)).unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    let descriptions = work.path().join("descriptions.json");
    let mut store = load_store(&descriptions);

    let mut collection = open(&folder, "tester", &store).collection;
    assert_eq!(file_names(&collection.posts), ["img_1.png", "IMG_2.jpg", "portrait.heic"]);
    assert_eq!(collection.warnings, Vec::new());
    assert_eq!(collection.label, "c".repeat(100));
    let around = open(&folder.join("portrait.heic"), "tester", &store);
    assert_eq!(around.focus, Some(2));

    // A description is found again, also when the same folder is named with the \\?\ prefix
    // (which is how some programs hand over a long path).
    let saved = store::save_description(&mut store, &mut collection.posts[1], "Far down the tree", NOW_MS);
    assert_eq!(saved, Ok(SaveOutcome::Saved));
    let reread = load_store(&descriptions);
    assert_eq!(open(&folder, "tester", &reread).collection.posts, collection.posts);
    if cfg!(windows) {
        let verbatim = PathBuf::from(format!(r"\\?\{}", folder.display()));
        let through_prefix = open(&verbatim, "tester", &reread).collection;
        assert_eq!(captions(&through_prefix.posts), ["", "Far down the tree", ""]);
        let keys = |posts: &[Post]| posts.iter().map(|post| post.key.clone()).collect::<Vec<PostKey>>();
        assert_eq!(keys(&through_prefix.posts), keys(&collection.posts));
        let picked = open(&verbatim.join("IMG_2.jpg"), "tester", &reread);
        assert_eq!(picked.focus, Some(1));
    }

    // Decoding: the built-in decoders, and Windows Imaging Component, which needs the prefix
    // added for it.
    // img_1.png is 720 x 900 pixels, IMG_2.jpg 720 x 720.
    for (post, size) in collection.posts[..2].iter().zip([(720, 900), (720, 720)]) {
        let path = &post.media[0].path;
        assert_eq!(decode::probe_dimensions(path), Ok(size));
        let shown = decode::decode_picture(path, 256).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!((shown.width, shown.height), decode::fit_within(size.0, size.1, 256));
        assert_eq!((shown.source_width, shown.source_height), size);
        let small = decode::decode_thumbnail(path, 100).unwrap_or_else(|error| panic!("{error}"));
        assert_eq!((small.width, small.height), decode::cover_within(size.0, size.1, 100));
    }
    if cfg!(windows) {
        let through_wic = decode::decode_with_wic(&collection.posts[1].media[0].path, 90);
        let through_wic = through_wic.unwrap_or_else(|error| panic!("WIC did not open the long path: {error}"));
        assert_eq!((through_wic.width, through_wic.height), (90, 90));
    }
    match decode::decode_picture(&collection.posts[2].media[0].path, 256) {
        Ok(shown) => assert_eq!((shown.width, shown.height), (256, 256)),
        // Only a computer without the HEIF codec may fail here, and then with that sentence.
        Err(error) => {
            println!("SKIPPED the HEIC part of the long path test: {error}");
            assert_eq!(error.kind, DecodeErrorKind::NoDecoder, "{error}");
        }
    }

    // Exporting from and into such a folder.
    let downloads = folder.join("downloads");
    fs::create_dir(&downloads).expect("a folder can be created");
    let result = download(&collection.posts, &downloads, DEFAULT_EXPORT_NAME);
    assert_eq!(result.problems, Vec::<String>::new());
    let entries = read_archive(&result.archives[0].path);
    assert_eq!(sorted_names(&entries), ["post.json", "posts/post1.png", "posts/post2.jpg", "posts/post3.heic"]);
    assert_eq!(post_json(&entries)["posts"][1]["caption"], "Far down the tree");
    assert_eq!(names_in(&downloads), ["instagram-export.zip"]);
}

// --- Robustness: files that vanish ---------------------------------------------------------------

#[test]
fn a_picture_deleted_after_opening_becomes_a_problem_sentence() {
    let work = TempFolder::new("vanished");
    let folder = work.folder("pictures");
    for (name, shade) in [("a.png", 10), ("b.png", 90), ("c.png", 200)] {
        fs::write(folder.join(name), tiny_png(shade)).expect("a picture");
    }
    let mut store = load_store(&work.path().join("descriptions.json"));
    let mut collection = open(&folder, "tester", &store).collection;
    assert_eq!(ids(&collection.posts), ["a", "b", "c"]);

    fs::remove_file(folder.join("b.png")).expect("the picture can be deleted");

    // Showing it: a sentence for the placeholder.
    let gone = &collection.posts[1].media[0].path;
    for error in [
        decode::decode_picture(gone, 64).expect_err("the file is gone"),
        decode::decode_thumbnail(gone, 64).expect_err("the file is gone"),
        decode::probe_dimensions(gone).map(|_| ()).expect_err("the file is gone"),
    ] {
        assert_eq!(error.kind, DecodeErrorKind::Unreadable);
        assert!(error.message.starts_with("\"b.png\" could not be read: "), "{error}");
    }
    // Describing it still works: the description is kept for when the picture is back.
    let saved = store::save_description(&mut store, &mut collection.posts[1], "Was here", NOW_MS);
    assert_eq!(saved, Ok(SaveOutcome::Saved));
    let entry = store.get(&collection.posts[1].key).expect("the description");
    assert_eq!((entry.file_bytes, entry.file_modified), (None, None));

    // Downloading: the others are saved, the missing one is named.
    let downloads = work.folder("downloads");
    let result = download(&collection.posts, &downloads, DEFAULT_EXPORT_NAME);
    assert_eq!(result.posts, 2);
    assert_eq!(result.problems.len(), 2, "{:#?}", result.problems);
    assert!(result.problems[0].starts_with("Post 2 (b): \"b.png\" could not be read: "), "{:#?}", result.problems);
    assert!(result.problems[0].ends_with('.') && !result.problems[0].ends_with(".."), "{:#?}", result.problems);
    assert_eq!(result.problems[1], "Post 2 (b) was left out: none of its pictures or videos could be saved.");
    let message = export::download_message(&result);
    assert!(
        message.starts_with("Downloaded 2 posts as instagram-export.zip. Left out: Post 2 (b): \"b.png\" could not be read: "),
        "{message}"
    );
    assert!(message.ends_with(" (and 1 more problem)"), "{message}");
    assert_eq!(export::download_toast_ms(&result), 9000);
    let entries = read_archive(&result.archives[0].path);
    assert_eq!(sorted_names(&entries), ["post.json", "posts/post1.png", "posts/post2.png"]);
    let written = post_json(&entries);
    let written_ids: Vec<&str> = written["posts"].as_array().expect("posts").iter().filter_map(|post| post["id"].as_str()).collect();
    assert_eq!(written_ids, ["a", "c"]);

    // Only that post: nothing to download, and no file appears.
    let nothing = download(&collection.posts[1..2], &work.folder("nothing"), "instagram-export-b");
    assert_eq!((nothing.posts, nothing.archives.len()), (0, 0));
    assert!(export::download_message(&nothing).starts_with("Nothing could be downloaded. Post 1 (b): \"b.png\" could not be read: "));
    assert_eq!(names_in(&work.path().join("nothing")), Vec::<String>::new());

    // Opening again: the folder without it; then without any picture; then without the folder.
    let fewer = open(&folder, "tester", &store).collection;
    assert_eq!(ids(&fewer.posts), ["a", "c"]);
    let missing = catalog::open_paths(&request(&folder.join("b.png"), "tester"), &store, &context()).expect_err("the file is gone");
    assert_eq!(missing.code, OpenErrorCode::ReadFailed);
    assert!(missing.message.contains("b.png\" could not be opened: "), "{}", missing.message);
    fs::remove_file(folder.join("a.png")).expect("deleted");
    fs::remove_file(folder.join("c.png")).expect("deleted");
    let empty = catalog::open_paths(&request(&folder, "tester"), &store, &context()).expect_err("no picture is left");
    assert_eq!(empty.code, OpenErrorCode::NoPictures);
    assert_eq!(empty.message, "No pictures were found in \"pictures\".");
    fs::remove_dir(&folder).expect("the folder can be removed");
    let no_folder = catalog::open_paths(&request(&folder, "tester"), &store, &context()).expect_err("no folder");
    assert_eq!(no_folder.code, OpenErrorCode::ReadFailed);
    assert!(no_folder.message.contains("pictures\" could not be opened: "), "{}", no_folder.message);
}

// --- Robustness: two windows, one descriptions.json ----------------------------------------------

#[test]
fn two_store_instances_share_one_descriptions_file() {
    let work = TempFolder::new("shared");
    let folder = work.folder("pictures");
    for (name, shade) in [("one.png", 10), ("two.png", 90), ("three.png", 200)] {
        fs::write(folder.join(name), tiny_png(shade)).expect("a picture");
    }
    let descriptions = work.path().join("data").join("descriptions.json");

    // Two windows start, each with its own store and its own copy of the posts.
    let mut first_store = load_store(&descriptions);
    let mut second_store = load_store(&descriptions);
    let mut first_window = open(&folder, "tester", &first_store).collection;
    let mut second_window = open(&folder, "tester", &second_store).collection;
    assert_eq!(ids(&first_window.posts), ["one", "three", "two"]);

    // Each window describes another picture; the second one knows nothing of the first save.
    let saved = store::save_description(&mut first_store, &mut first_window.posts[0], "From the first window", NOW_MS);
    assert_eq!(saved, Ok(SaveOutcome::Saved));
    let saved =
        store::save_description(&mut second_store, &mut second_window.posts[2], "From the second window", NOW_MS + 1);
    assert_eq!(saved, Ok(SaveOutcome::Saved));

    // Nothing was lost: a third start sees both, and the second window's store has caught up.
    let third_store = load_store(&descriptions);
    assert_eq!(third_store.len(), 2);
    assert_eq!(third_store, second_store);
    let third_window = open(&folder, "tester", &third_store).collection;
    assert_eq!(captions(&third_window.posts), ["From the first window", "", "From the second window"]);

    // The first window clears its description again; the other one stays.
    let cleared = store::save_description(&mut first_store, &mut first_window.posts[0], "   ", NOW_MS + 2);
    assert_eq!(cleared, Ok(SaveOutcome::Removed));
    let last_store = load_store(&descriptions);
    assert_eq!(last_store.len(), 1);
    assert_eq!(captions(&open(&folder, "tester", &last_store).collection.posts), ["", "", "From the second window"]);
    assert_eq!(names_in(descriptions.parent().expect("the data folder")), ["descriptions.json"]);
}

// --- Robustness: folders that cannot be written to -----------------------------------------------

/// Takes away the right to create files in a folder (Windows: a "deny" entry for everyone) and
/// gives it back when dropped. The read-only attribute would not do: Windows ignores it for
/// folders.
#[cfg(windows)]
struct WriteProtected(PathBuf);

#[cfg(windows)]
impl WriteProtected {
    /// `None` when icacls cannot be run; the caller then skips its check with a note.
    fn new(folder: &Path) -> Option<WriteProtected> {
        let denied = Command::new("icacls").arg(folder).args(["/deny", "*S-1-1-0:(WD,AD)"]).output().ok()?;
        denied.status.success().then(|| WriteProtected(folder.to_path_buf()))
    }
}

#[cfg(windows)]
impl Drop for WriteProtected {
    fn drop(&mut self) {
        let _ = Command::new("icacls").arg(&self.0).args(["/remove:d", "*S-1-1-0"]).output();
    }
}

#[test]
fn a_target_folder_that_is_missing_or_write_protected_is_an_error_sentence() {
    let work = TempFolder::new("target");
    let folder = work.folder("pictures");
    fs::write(folder.join("a.png"), tiny_png(40)).expect("a picture");
    let mut store = load_store(&work.path().join("descriptions.json"));
    let mut collection = open(&folder, "tester", &store).collection;
    let failed = |target: &Path, name: &str| -> ExportError {
        let (result, _) = try_download(&collection.posts, target, name);
        let error = result.expect_err("the export cannot be written");
        assert_eq!(error.code, ExportErrorCode::WriteFailed);
        assert!(error.message.starts_with("The archive could not be written: "), "{error}");
        assert!(error.message.len() > "The archive could not be written: ".len(), "{error}");
        error
    };

    // A folder that does not exist is not created.
    let missing = work.path().join("not").join("there");
    failed(&missing, DEFAULT_EXPORT_NAME);
    assert!(!missing.exists());

    // A file where the folder should be.
    let file = work.path().join("a-file.txt");
    fs::write(&file, b"text").expect("a file");
    failed(&file, DEFAULT_EXPORT_NAME);
    assert_eq!(fs::read(&file).expect("the file"), b"text");

    // An archive name that would leave the target folder.
    let downloads = work.folder("downloads");
    for name in ["../escaped", "sub/folder", r"C:\elsewhere", "", "what?"] {
        let error = failed(&downloads, name);
        assert_eq!(error.message, format!("The archive could not be written: \"{name}\" cannot be used as a file name"));
    }
    assert_eq!(names_in(&downloads), Vec::<String>::new());
    assert!(!work.path().join("escaped.zip").exists());

    // A folder that may not be written to: an error, nothing left behind, and the data folder
    // of the descriptions likewise.
    #[cfg(windows)]
    {
        let protected = work.folder("protected");
        match WriteProtected::new(&protected) {
            Some(guard) => {
                let error = failed(&protected, DEFAULT_EXPORT_NAME);
                println!("write-protected target: {error}");
                assert_eq!(names_in(&protected), Vec::<String>::new());

                let mut locked_store = DescriptionStore::new(protected.join("descriptions.json"));
                let unchanged = locked_store.clone();
                let error = store::save_description(&mut locked_store, &mut collection.posts[0], "Not kept", NOW_MS)
                    .expect_err("the data folder cannot be written to");
                let shown = messages::description_not_saved(&error.message);
                println!("write-protected data folder: {shown}");
                assert!(shown.starts_with("The description could not be saved: Could not update "), "{shown}");
                assert!(shown.ends_with('.') && !shown.ends_with(".."), "{shown}");
                assert_eq!(collection.posts[0].caption, "");
                assert_eq!(locked_store, unchanged);
                assert_eq!(names_in(&protected), Vec::<String>::new());

                // With the protection gone both work.
                drop(guard);
                let result = download(&collection.posts, &protected, DEFAULT_EXPORT_NAME);
                assert_eq!(result.posts, 1);
            }
            None => println!("note: icacls could not be run; the write-protected folder was NOT tested"),
        }
    }

    // A descriptions file whose folder is really a file: the same sentence, nothing changes.
    let mut blocked_store = DescriptionStore::new(file.join("descriptions.json"));
    let error = store::save_description(&mut blocked_store, &mut collection.posts[0], "Not kept", NOW_MS)
        .expect_err("the data folder is a file");
    assert!(error.message.starts_with("Could not update "), "{error}");
    assert_eq!(collection.posts[0].caption, "");
    assert!(blocked_store.is_empty());
    // The working store is untouched by all of this.
    let kept = store::save_description(&mut store, &mut collection.posts[0], "Kept", NOW_MS);
    assert_eq!(kept, Ok(SaveOutcome::Saved));
}
