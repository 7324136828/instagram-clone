//! The exporter ("Download post" / "Download all"), through the public API only.
//!
//! These are the tests of the reference web project's frontend/src/import/export.test.ts (the
//! line numbers in the comments are those of that file), plus what only a native exporter has:
//! archives are files in a folder the user chose, so temporary files, renaming, replacing and
//! cleaning up are tested too. Two tests extract an archive and run the clone's own validator
//! on it with Node.js; they are skipped with a note when `node` is not installed.
//!
//! The posts are built by hand from the model types: the files of the clone's test (tiny byte
//! sequences that start like real pictures and videos) are written into a folder of the test,
//! and the validator tests use the pictures in core/tests/fixtures. Nothing here touches the
//! real user profile: every test has its own folder under the system's temporary folder.

use std::fs::{self, File};
use std::io::{BufReader, Read};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use instagram_core::export::{
    download_message, download_toast_ms, export_posts, ExportError, ExportErrorCode, ExportOptions, ExportProgress,
    ExportResult, DEFAULT_EXPORT_NAME, READ_CHUNK_BYTES,
};
use instagram_core::messages;
use instagram_core::model::{Comment, MediaItem, MediaKind, MediaType, Post, PostKey, PostOrigin, Poster, User};
use serde_json::{json, Value};
use zip::{CompressionMethod, ZipArchive};

/// 2026-11-11T10:00:00Z, the `NOW` of export.test.ts:72.
const NOW_MS: i64 = 1_794_391_200_000;
/// 2026-11-01T15:04:05Z, 2026-11-01T16:00:00Z, 2026-10-28T08:12:40Z and 2026-10-20T19:45:00Z.
const RIDGE_MS: i64 = 1_793_545_445_000;
const RIDGE_COMMENT_MS: i64 = 1_793_548_800_000;
const SECOND_MS: i64 = 1_793_175_160_000;
const REEL_MS: i64 = 1_792_525_500_000;

// --- A folder per test ------------------------------------------------------------------------

/// `{root}\source` holds the files of the posts, `{root}\target` is where archives are written.
struct Workspace {
    root: PathBuf,
    source: PathBuf,
    target: PathBuf,
}

impl Workspace {
    fn new(tag: &str) -> Workspace {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique =
            format!("instagram-core-export-{tag}-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed));
        let root = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&root);
        let source = root.join("source");
        let target = root.join("target");
        fs::create_dir_all(&source).expect("a test folder can be created");
        fs::create_dir_all(&target).expect("a test folder can be created");
        Workspace { root, source, target }
    }

    /// Writes a source file and returns its path. `relative` uses forward slashes.
    fn write(&self, relative: &str, bytes: &[u8]) -> PathBuf {
        let path = self.source.join(relative);
        if let Some(folder) = path.parent() {
            fs::create_dir_all(folder).expect("a source folder can be created");
        }
        fs::write(&path, bytes).expect("a source file can be written");
        path
    }

    /// Where a source file is, or would be.
    fn path(&self, relative: &str) -> PathBuf {
        self.source.join(relative)
    }

    /// The names in the target folder, sorted.
    fn target_names(&self) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(&self.target)
            .expect("the target folder can be listed")
            .map(|entry| entry.expect("a folder entry").file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }
}

impl Drop for Workspace {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

// --- Tiny files that start like real pictures and videos (the clone's testkit.ts) -------------

fn padded(head: &[u8], fill: u8) -> Vec<u8> {
    let mut bytes = vec![fill; head.len().max(64)];
    bytes[..head.len()].copy_from_slice(head);
    bytes
}

fn jpeg(fill: u8) -> Vec<u8> {
    padded(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0x00], fill)
}

fn png(fill: u8) -> Vec<u8> {
    padded(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A], fill)
}

fn webp(fill: u8) -> Vec<u8> {
    padded(b"RIFF\x24\x00\x00\x00WEBPVP8 ", fill)
}

fn mp4(fill: u8) -> Vec<u8> {
    padded(b"\x00\x00\x00\x20ftypisom\x00\x00\x00\x00isomiso2avc1mp41", fill)
}

/// An EBML header with the DocType "webm".
fn webm(fill: u8) -> Vec<u8> {
    padded(b"\x1A\x45\xDF\xA3\x8B\x42\x86\x81\x01\x42\x82\x84webm", fill)
}

// --- Posts built by hand ----------------------------------------------------------------------

fn media(kind: MediaKind, path: &Path, manifest_path: &str) -> MediaItem {
    MediaItem {
        kind,
        path: path.to_path_buf(),
        manifest_path: manifest_path.to_owned(),
        poster: None,
        alt: None,
        bytes: fs::metadata(path).map(|metadata| metadata.len()).unwrap_or(0),
    }
}

fn picture(path: &Path, manifest_path: &str) -> MediaItem {
    media(MediaKind::Image, path, manifest_path)
}

fn video(path: &Path, manifest_path: &str, poster: Option<(&Path, &str)>) -> MediaItem {
    let mut item = media(MediaKind::Video, path, manifest_path);
    item.poster = poster.map(|(path, manifest_path)| Poster {
        path: path.to_path_buf(),
        manifest_path: manifest_path.to_owned(),
    });
    item
}

/// A post with the defaults of a loose picture: no link, no counts, no time.
fn post(id: &str, username: &str, media: Vec<MediaItem>) -> Post {
    let key = match media.first() {
        Some(cover) => PostKey::for_file(&cover.path),
        None => PostKey::from_stored(format!("file|{id}")),
    };
    Post {
        key,
        id: id.to_owned(),
        permalink: None,
        media,
        user: User::named(username),
        caption: String::new(),
        original_caption: String::new(),
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

/// The files of export.test.ts:9-17, 63-69.
struct SourceFiles {
    cover: Vec<u8>,
    clip: Vec<u8>,
    clip_poster: Vec<u8>,
    second: Vec<u8>,
    reel: Vec<u8>,
    reel_poster: Vec<u8>,
    avatar: Vec<u8>,
}

fn source_files() -> SourceFiles {
    SourceFiles {
        cover: jpeg(11),
        clip: mp4(12),
        clip_poster: png(13),
        second: png(14),
        reel: webm(15),
        reel_poster: webp(16),
        avatar: jpeg(17),
    }
}

/// The three posts of export.test.ts:19-62 as the app holds them after opening that export: a
/// carousel of a picture and a video with poster, a single picture without a link, and a reel
/// by another author. The first two share one profile picture.
fn source_posts(workspace: &Workspace) -> Vec<Post> {
    let files = source_files();
    let cover = workspace.write("posts/post1_1.jpg", &files.cover);
    let clip = workspace.write("posts/post1_2.mp4", &files.clip);
    let clip_poster = workspace.write("posts/post1_2_poster.png", &files.clip_poster);
    let second = workspace.write("posts/second.png", &files.second);
    let reel = workspace.write("posts/reel.webm", &files.reel);
    let reel_poster = workspace.write("posts/reel.webp", &files.reel_poster);
    let avatar = workspace.write("avatars/lumen.atlas.jpg", &files.avatar);

    let mut clip_item = video(&clip, "posts/post1_2.mp4", Some((&clip_poster, "posts/post1_2_poster.png")));
    clip_item.alt = Some("The climb".to_owned());
    let mut first = post("C9aKq2LtXy1", "lumen.atlas", vec![picture(&cover, "posts/post1_1.jpg"), clip_item]);
    first.permalink = Some("https://www.instagram.com/p/C9aKq2LtXy1/".to_owned());
    first.user.full_name = Some("Lumen Atlas".to_owned());
    first.user.verified = Some(true);
    first.user.avatar = Some(avatar.clone());
    first.caption = "Two mornings on the ridge 🌄\n\n#sunrise".to_owned();
    first.original_caption = first.caption.clone();
    first.comment_count = 12;
    first.like_count = Some(340);
    first.taken_at = Some(RIDGE_MS);
    first.media_type = MediaType::Carousel;
    first.alt = Some("Cover alt".to_owned());
    first.location = Some("Chamonix, France".to_owned());
    first.comments = vec![Comment {
        id: "C9aKq2LtXy1-c1".to_owned(),
        username: "someone".to_owned(),
        text: "Wow".to_owned(),
        likes: 3,
        taken_at: Some(RIDGE_COMMENT_MS),
    }];

    let mut middle = post("C8ZmP4RvNd7", "lumen.atlas", vec![picture(&second, "posts/second.png")]);
    middle.user.avatar = Some(avatar);
    middle.taken_at = Some(SECOND_MS);

    let reel_item = video(&reel, "posts/reel.webm", Some((&reel_poster, "posts/reel.webp")));
    let mut last = post("C7TtW0YhBs3", "mira.films", vec![reel_item]);
    last.permalink = Some("https://www.instagram.com/reel/C7TtW0YhBs3/".to_owned());
    last.caption = "Behind the scenes, day 3.".to_owned();
    last.original_caption = last.caption.clone();
    last.comment_count = 4;
    last.like_count = Some(2100);
    last.taken_at = Some(REEL_MS);
    last.media_type = MediaType::Video;

    vec![first, middle, last]
}

// --- Running an export and reading archives back ----------------------------------------------

fn options() -> ExportOptions {
    ExportOptions::new(DEFAULT_EXPORT_NAME, NOW_MS, 0)
}

/// Runs an export that nobody cancels; returns the result and the progress reports.
fn run(
    posts: &[Post],
    target: &Path,
    options: &ExportOptions,
) -> (Result<ExportResult, ExportError>, Vec<(usize, usize)>) {
    let mut reports = Vec::new();
    let result = export_posts(
        posts,
        target,
        options,
        &mut |progress: ExportProgress| reports.push((progress.done, progress.total)),
        &AtomicBool::new(false),
    );
    (result, reports)
}

fn exported(posts: &[Post], target: &Path, options: &ExportOptions) -> ExportResult {
    run(posts, target, options).0.expect("the export succeeds")
}

/// The file name and the number of posts of every archive of a result.
fn archive_summary(result: &ExportResult) -> Vec<(&str, usize)> {
    result.archives.iter().map(|archive| (archive.name.as_str(), archive.posts)).collect()
}

/// The id of the first post of an archive.
fn first_id(archive: &Path) -> String {
    manifest_of(&read_archive(archive))["posts"][0]["id"].as_str().unwrap_or_default().to_owned()
}

/// One entry of an archive, in the order of the archive's directory.
struct Entry {
    name: String,
    bytes: Vec<u8>,
    method: CompressionMethod,
    compressed_size: u64,
    /// Year, month, day, hour, minute, second.
    modified: (u16, u8, u8, u8, u8, u8),
    is_dir: bool,
}

/// Reads an archive completely; the reader checks every entry's size and CRC-32 on the way.
fn read_archive(path: &Path) -> Vec<Entry> {
    let file = File::open(path).expect("the archive exists");
    let mut archive = ZipArchive::new(BufReader::new(file)).expect("the archive has a directory");
    let mut entries = Vec::new();
    for index in 0..archive.len() {
        let mut file = archive.by_index(index).expect("an entry can be opened");
        let name = String::from_utf8(file.name_raw().to_vec()).expect("entry names are UTF-8");
        let modified = file.last_modified().expect("every entry has a time");
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).expect("an entry can be read and its CRC-32 is right");
        assert_eq!(bytes.len() as u64, file.size(), "{name}");
        entries.push(Entry {
            name,
            bytes,
            method: file.compression(),
            compressed_size: file.compressed_size(),
            modified: (
                modified.year(),
                modified.month(),
                modified.day(),
                modified.hour(),
                modified.minute(),
                modified.second(),
            ),
            is_dir: file.is_dir(),
        });
    }
    entries
}

fn names(entries: &[Entry]) -> Vec<&str> {
    entries.iter().map(|entry| entry.name.as_str()).collect()
}

fn sorted_names(entries: &[Entry]) -> Vec<&str> {
    let mut sorted = names(entries);
    sorted.sort_unstable();
    sorted
}

fn bytes_of<'a>(entries: &'a [Entry], name: &str) -> &'a [u8] {
    match entries.iter().find(|entry| entry.name == name) {
        Some(entry) => &entry.bytes,
        None => panic!("the archive has no entry \"{name}\""),
    }
}

fn manifest_of(entries: &[Entry]) -> Value {
    serde_json::from_slice(bytes_of(entries, "post.json")).expect("post.json is JSON")
}

fn keys(value: &Value) -> Vec<&str> {
    value.as_object().map(|object| object.keys().map(String::as_str).collect()).unwrap_or_default()
}

/// What every archive of this app must be, whatever its posts: media stored, post.json deflated
/// and last, forward-slash file entries only, one time on every entry, canonical top level.
fn assert_well_formed(entries: &[Entry], modified: (u16, u8, u8, u8, u8, u8)) {
    assert_eq!(entries.last().map(|entry| entry.name.as_str()), Some("post.json"));
    for entry in entries {
        let name = entry.name.as_str();
        assert!(!entry.is_dir && !name.ends_with('/'), "{name} is a directory entry");
        assert!(!name.starts_with('/') && !name.contains('\\') && !name.contains(".."), "{name}");
        assert_eq!(entry.modified, modified, "{name}");
        if name == "post.json" {
            assert_eq!(entry.method, CompressionMethod::Deflated, "{name}");
        } else {
            assert!(name.starts_with("posts/") || name.starts_with("avatars/"), "{name}");
            assert_eq!(entry.method, CompressionMethod::Stored, "{name}");
            assert_eq!(entry.compressed_size, entry.bytes.len() as u64, "{name}");
        }
    }
    let mut unique = names(entries);
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(unique.len(), entries.len(), "an entry name occurs twice");

    let text = std::str::from_utf8(bytes_of(entries, "post.json")).expect("post.json is UTF-8");
    assert!(!text.starts_with('\u{FEFF}'));
    assert!(text.ends_with("}\n") && !text.ends_with("\n\n"));
    let manifest = manifest_of(entries);
    assert_eq!(keys(&manifest), ["format", "version", "exported_at", "source", "posts"]);
    assert_eq!(manifest["format"], "instagram-export");
    assert_eq!(manifest["version"], 1);
    assert_eq!(manifest["source"], json!({ "type": "posts" }));
}

/// 10:00:00 on 2026-11-11, the entry time of `options()`.
const NOW_ENTRY_TIME: (u16, u8, u8, u8, u8, u8) = (2026, 11, 11, 10, 0, 0);

// --- export.test.ts, "exporting posts" --------------------------------------------------------

/// export.test.ts:105-177.
#[test]
fn writes_post_json_posts_and_avatars_in_the_instagram_md_layout_captions_included() {
    let workspace = Workspace::new("layout");
    let files = source_files();
    let posts = source_posts(&workspace);
    let (result, progress) = run(&posts, &workspace.target, &options());
    let result = result.expect("the export succeeds");

    assert_eq!(result.problems, Vec::<String>::new());
    assert_eq!(result.posts, 3);
    assert_eq!(result.archives.len(), 1);
    assert_eq!(result.archives[0].name, "instagram-export.zip");
    assert_eq!(result.archives[0].posts, 3);
    assert_eq!(result.archives[0].path, workspace.target.join("instagram-export.zip"));
    assert_eq!(progress, [(0, 3), (1, 3), (2, 3), (3, 3)]);
    // Nothing but the archive: no temporary file is left.
    assert_eq!(workspace.target_names(), ["instagram-export.zip"]);
    assert_eq!(download_message(&result), "Downloaded 3 posts as instagram-export.zip.");
    assert_eq!(download_toast_ms(&result), 4000);

    let entries = read_archive(&result.archives[0].path);
    assert_well_formed(&entries, NOW_ENTRY_TIME);
    assert_eq!(
        sorted_names(&entries),
        [
            "avatars/lumen.atlas.jpg",
            "post.json",
            "posts/post1_1.jpg",
            "posts/post1_2.mp4",
            "posts/post1_2_poster.png",
            "posts/post2.png",
            "posts/post3.webm",
            "posts/post3_poster.webp",
        ]
    );
    // Per post its slides, each poster directly after its slide, then the avatar when it is new
    // in the archive; post.json last.
    assert_eq!(
        names(&entries),
        [
            "posts/post1_1.jpg",
            "posts/post1_2.mp4",
            "posts/post1_2_poster.png",
            "avatars/lumen.atlas.jpg",
            "posts/post2.png",
            "posts/post3.webm",
            "posts/post3_poster.webp",
            "post.json",
        ]
    );
    // The original bytes, never re-encoded.
    assert_eq!(bytes_of(&entries, "posts/post1_1.jpg"), files.cover);
    assert_eq!(bytes_of(&entries, "posts/post1_2.mp4"), files.clip);
    assert_eq!(bytes_of(&entries, "posts/post1_2_poster.png"), files.clip_poster);
    assert_eq!(bytes_of(&entries, "posts/post2.png"), files.second);
    assert_eq!(bytes_of(&entries, "posts/post3.webm"), files.reel);
    assert_eq!(bytes_of(&entries, "posts/post3_poster.webp"), files.reel_poster);
    assert_eq!(bytes_of(&entries, "avatars/lumen.atlas.jpg"), files.avatar);

    let manifest = manifest_of(&entries);
    assert_eq!(manifest["exported_at"], "2026-11-11T10:00:00Z");
    let posts = manifest["posts"].as_array().expect("posts is a list");
    assert_eq!(posts.len(), 3);
    assert_eq!(
        keys(&posts[0]),
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
    assert_eq!(
        posts[0],
        json!({
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
                "avatar": "avatars/lumen.atlas.jpg"
            },
            "caption": "Two mornings on the ridge 🌄\n\n#sunrise",
            "comment_count": 12,
            "like_count": 340,
            "taken_at": "2026-11-01T15:04:05Z",
            "media_type": "carousel",
            "alt": "Cover alt",
            "location": "Chamonix, France",
            "comments": [{ "username": "someone", "text": "Wow", "like_count": 3, "taken_at": "2026-11-01T16:00:00Z" }]
        })
    );
    // No "url", "alt", "location" or "comments"; like_count is written as null.
    assert_eq!(
        keys(&posts[1]),
        ["id", "media", "user", "caption", "comment_count", "like_count", "taken_at", "media_type"]
    );
    assert_eq!(
        posts[1],
        json!({
            "id": "C8ZmP4RvNd7",
            "media": [{ "type": "image", "path": "posts/post2.png" }],
            "user": { "username": "lumen.atlas", "avatar": "avatars/lumen.atlas.jpg" },
            "caption": "",
            "comment_count": 0,
            "like_count": null,
            "taken_at": "2026-10-28T08:12:40Z",
            "media_type": "image"
        })
    );
    assert_eq!(
        posts[2],
        json!({
            "id": "C7TtW0YhBs3",
            "url": "https://www.instagram.com/reel/C7TtW0YhBs3/",
            "media": [{ "type": "video", "path": "posts/post3.webm", "poster": "posts/post3_poster.webp" }],
            "user": { "username": "mira.films" },
            "caption": "Behind the scenes, day 3.",
            "comment_count": 4,
            "like_count": 2100,
            "taken_at": "2026-10-20T19:45:00Z",
            "media_type": "video"
        })
    );
}

/// export.test.ts:198-207.
#[test]
fn writes_an_edited_description() {
    let workspace = Workspace::new("edited");
    let mut edited = source_posts(&workspace).remove(1);
    edited.caption = "A new description\nwith two lines ✨".to_owned();
    let name = instagram_core::export::single_post_export_name(&edited.id);
    let result = exported(&[edited], &workspace.target, &ExportOptions::new(name, NOW_MS, 0));
    assert_eq!(result.archives[0].name, "instagram-export-C8ZmP4RvNd7.zip");
    assert_eq!(workspace.target_names(), ["instagram-export-C8ZmP4RvNd7.zip"]);
    assert_eq!(download_message(&result), "Downloaded 1 post as instagram-export-C8ZmP4RvNd7.zip.");

    let entries = read_archive(&result.archives[0].path);
    assert_well_formed(&entries, NOW_ENTRY_TIME);
    assert_eq!(sorted_names(&entries), ["avatars/lumen.atlas.jpg", "post.json", "posts/post1.png"]);
    let manifest = manifest_of(&entries);
    assert_eq!(manifest["posts"].as_array().map(Vec::len), Some(1));
    assert_eq!(manifest["posts"][0]["caption"], "A new description\nwith two lines ✨");
    // The text in the file, as the example of spec-export A.5 shows it.
    let text = String::from_utf8(bytes_of(&entries, "post.json").to_vec()).expect("post.json is UTF-8");
    assert!(text.contains("      \"caption\": \"A new description\\nwith two lines ✨\",\n"));
}

/// export.test.ts:209-234. The files are deleted instead of failing to load, so the reason is
/// the operating system's.
#[test]
fn leaves_out_files_it_cannot_read_falls_back_to_a_poster_and_numbers_the_posts_it_writes() {
    let workspace = Workspace::new("unreadable");
    let files = source_files();
    let posts = source_posts(&workspace);
    for gone in ["posts/post1_1.jpg", "posts/second.png", "posts/reel.webm"] {
        fs::remove_file(workspace.path(gone)).expect("a source file can be removed");
    }
    let reason = File::open(workspace.path("posts/second.png")).expect_err("the file is gone").to_string();

    let (result, progress) = run(&posts, &workspace.target, &options());
    let result = result.expect("the export succeeds");
    assert_eq!(result.posts, 2);
    assert_eq!(progress, [(0, 3), (1, 3), (2, 3), (3, 3)]);
    assert_eq!(
        result.problems,
        [
            format!("Post 1 (C9aKq2LtXy1): \"posts/post1_1.jpg\" could not be read: {reason}."),
            format!("Post 2 (C8ZmP4RvNd7): \"posts/second.png\" could not be read: {reason}."),
            "Post 2 (C8ZmP4RvNd7) was left out: none of its pictures or videos could be saved.".to_owned(),
            format!("Post 3 (C7TtW0YhBs3): \"posts/reel.webm\" could not be read: {reason}."),
            "Post 3 (C7TtW0YhBs3): only the poster of \"posts/reel.webm\" was saved.".to_owned(),
        ]
    );
    assert_eq!(
        download_message(&result),
        format!(
            "Downloaded 2 posts as instagram-export.zip. Left out: {} (and 4 more problems)",
            result.problems[0]
        )
    );
    assert_eq!(download_toast_ms(&result), 9000);

    let entries = read_archive(&result.archives[0].path);
    assert_well_formed(&entries, NOW_ENTRY_TIME);
    assert_eq!(
        sorted_names(&entries),
        ["avatars/lumen.atlas.jpg", "post.json", "posts/post1.mp4", "posts/post1_poster.png", "posts/post2.webp"]
    );
    assert_eq!(bytes_of(&entries, "posts/post2.webp"), files.reel_poster);
    let manifest = manifest_of(&entries);
    let summary: Vec<Value> = manifest["posts"]
        .as_array()
        .expect("posts is a list")
        .iter()
        .map(|post| json!([post["id"], post["media_type"], post["media"]]))
        .collect();
    assert_eq!(
        summary,
        [
            json!([
                "C9aKq2LtXy1",
                "video",
                [{ "type": "video", "path": "posts/post1.mp4", "poster": "posts/post1_poster.png", "alt": "The climb" }]
            ]),
            // The reel is declared a video and stays one, although only its poster was saved.
            json!(["C7TtW0YhBs3", "video", [{ "type": "image", "path": "posts/post2.webp" }]]),
        ]
    );
}

/// export.test.ts:236-242.
#[test]
fn returns_no_archive_when_nothing_could_be_read() {
    let workspace = Workspace::new("nothing");
    let posts = source_posts(&workspace);
    fs::remove_file(workspace.path("posts/second.png")).expect("a source file can be removed");
    let (result, progress) = run(&posts[1..2], &workspace.target, &options());
    let result = result.expect("an export of nothing is not an error");
    assert!(result.archives.is_empty());
    assert_eq!(result.posts, 0);
    assert_eq!(
        result.problems.last().map(String::as_str),
        Some("Post 1 (C8ZmP4RvNd7) was left out: none of its pictures or videos could be saved.")
    );
    assert_eq!(progress, [(0, 1), (1, 1)]);
    // Not even a temporary file was created.
    assert_eq!(workspace.target_names(), Vec::<String>::new());
    let message = download_message(&result);
    let start = "Nothing could be downloaded. Post 1 (C8ZmP4RvNd7): \"posts/second.png\" could not be read: ";
    assert!(message.starts_with(start), "{message}");

    // No posts at all: the same, without a problem.
    let (empty, progress) = run(&[], &workspace.target, &options());
    let empty = empty.expect("an export of nothing is not an error");
    assert_eq!((empty.archives.len(), empty.posts, empty.problems.len()), (0, 0, 0));
    assert_eq!(progress, [(0, 0)]);
    assert_eq!(download_message(&empty), "Nothing could be downloaded.");
    assert_eq!(workspace.target_names(), Vec::<String>::new());
}

/// export.test.ts:244-261.
#[test]
fn splits_a_large_export_into_complete_parts_numbered_across_parts() {
    let workspace = Workspace::new("parts");
    let files = source_files();
    let posts = source_posts(&workspace);
    // Post 1 has 256 bytes of files (with the avatar), posts 2 and 3 have 128 each.
    let by_bytes = ExportOptions { max_part_bytes: 200, ..options() };
    let result = exported(&posts, &workspace.target, &by_bytes);
    assert_eq!(
        archive_summary(&result),
        [("instagram-export-part1.zip", 1), ("instagram-export-part2.zip", 1), ("instagram-export-part3.zip", 1)]
    );
    assert_eq!(result.posts, 3);
    assert_eq!(
        workspace.target_names(),
        ["instagram-export-part1.zip", "instagram-export-part2.zip", "instagram-export-part3.zip"]
    );
    assert_eq!(download_message(&result), "Downloaded 3 posts in 3 archives (instagram-export-part1.zip …).");

    let part1 = read_archive(&result.archives[0].path);
    let part2 = read_archive(&result.archives[1].path);
    let part3 = read_archive(&result.archives[2].path);
    for part in [&part1, &part2, &part3] {
        // Each part is a complete export, and all carry the same instant.
        assert_well_formed(part, NOW_ENTRY_TIME);
        assert_eq!(manifest_of(part)["exported_at"], "2026-11-11T10:00:00Z");
        assert_eq!(manifest_of(part)["posts"].as_array().map(Vec::len), Some(1));
    }
    // The second part has its own copy of the avatar the first part already holds.
    assert_eq!(sorted_names(&part2), ["avatars/lumen.atlas.jpg", "post.json", "posts/post2.png"]);
    assert_eq!(bytes_of(&part2, "avatars/lumen.atlas.jpg"), files.avatar);
    assert_eq!(bytes_of(&part1, "avatars/lumen.atlas.jpg"), files.avatar);
    let second = &manifest_of(&part2)["posts"][0];
    assert_eq!(second["id"], "C8ZmP4RvNd7");
    assert_eq!(second["user"]["avatar"], "avatars/lumen.atlas.jpg");
    // The numbering goes on across the parts.
    assert_eq!(manifest_of(&part3)["posts"][0]["media"][0]["path"], "posts/post3.webm");
    assert_eq!(sorted_names(&part3), ["post.json", "posts/post3.webm", "posts/post3_poster.webp"]);

    // By the number of posts.
    let by_posts = ExportOptions { max_part_posts: 2, name: "by-posts".to_owned(), ..options() };
    let result = exported(&posts, &workspace.target, &by_posts);
    assert_eq!(archive_summary(&result), [("by-posts-part1.zip", 2), ("by-posts-part2.zip", 1)]);
    let first = read_archive(&result.archives[0].path);
    let last = read_archive(&result.archives[1].path);
    // Two posts of one author in one part share one avatar file.
    assert_eq!(
        names(&first),
        [
            "posts/post1_1.jpg",
            "posts/post1_2.mp4",
            "posts/post1_2_poster.png",
            "avatars/lumen.atlas.jpg",
            "posts/post2.png",
            "post.json"
        ]
    );
    assert_eq!(names(&last), ["posts/post3.webm", "posts/post3_poster.webp", "post.json"]);

    // A part is never empty: with room for nothing, every post gets a part of its own.
    let tiny = ExportOptions { max_part_bytes: 0, max_part_posts: 0, name: "tiny".to_owned(), ..options() };
    let result = exported(&posts, &workspace.target, &tiny);
    assert_eq!(result.archives.iter().map(|archive| archive.posts).collect::<Vec<_>>(), [1, 1, 1]);
}

/// export.test.ts:263-271, with more usernames that are not file names.
#[test]
fn keeps_ids_unique_and_avatar_file_names_safe() {
    let workspace = Workspace::new("ids");
    let files = source_files();
    let original = source_posts(&workspace).remove(1);
    let twin_of = |username: &str| {
        let mut twin = original.clone();
        twin.user.username = username.to_owned();
        twin
    };
    let posts = vec![
        original.clone(),
        twin_of("../CON"),
        // The same name in another letter case is another author, but not another file name.
        twin_of("Lumen.Atlas"),
        // A Windows device name, a name of dots, and one without any character a stem may have.
        twin_of("con"),
        twin_of("..."),
        twin_of("名前"),
        // The author of the first post again: no second file.
        twin_of("lumen.atlas"),
    ];
    let result = exported(&posts, &workspace.target, &options());
    let entries = read_archive(&result.archives[0].path);
    assert_well_formed(&entries, NOW_ENTRY_TIME);
    let manifest = manifest_of(&entries);
    let posts = manifest["posts"].as_array().expect("posts is a list");
    let ids: Vec<&str> = posts.iter().filter_map(|post| post["id"].as_str()).collect();
    assert_eq!(
        ids,
        [
            "C8ZmP4RvNd7",
            "C8ZmP4RvNd7-2",
            "C8ZmP4RvNd7-3",
            "C8ZmP4RvNd7-4",
            "C8ZmP4RvNd7-5",
            "C8ZmP4RvNd7-6",
            "C8ZmP4RvNd7-7"
        ]
    );
    let avatars: Vec<&str> = posts.iter().filter_map(|post| post["user"]["avatar"].as_str()).collect();
    assert_eq!(
        avatars,
        [
            "avatars/lumen.atlas.jpg",
            "avatars/_CON.jpg",
            "avatars/Lumen.Atlas-2.jpg",
            "avatars/con_.jpg",
            "avatars/user.jpg",
            "avatars/_.jpg",
            "avatars/lumen.atlas.jpg"
        ]
    );
    // The username itself is written as it is.
    assert_eq!(posts[1]["user"]["username"], "../CON");
    assert_eq!(bytes_of(&entries, "avatars/_CON.jpg"), files.avatar);
    let avatar_files: Vec<&str> = names(&entries).into_iter().filter(|name| name.starts_with("avatars/")).collect();
    assert_eq!(avatar_files.len(), 6);

    // Ids stay unique across parts as well, and an id that looks de-duplicated is respected.
    let mut taken = original.clone();
    taken.id = "C8ZmP4RvNd7-2".to_owned();
    let split = ExportOptions { max_part_posts: 1, name: "split".to_owned(), ..options() };
    let result = exported(&[original.clone(), taken, original], &workspace.target, &split);
    let ids: Vec<String> = result.archives.iter().map(|archive| first_id(&archive.path)).collect();
    assert_eq!(ids, ["C8ZmP4RvNd7", "C8ZmP4RvNd7-2", "C8ZmP4RvNd7-3"]);
}

/// export.test.ts:273-282.
#[test]
fn stops_when_cancelled() {
    let workspace = Workspace::new("cancel");
    let posts = source_posts(&workspace);
    let cancel = AtomicBool::new(false);
    let mut reports = Vec::new();
    let error = export_posts(
        &posts,
        &workspace.target,
        &options(),
        &mut |progress: ExportProgress| {
            reports.push(progress.done);
            if progress.done == 1 {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        &cancel,
    )
    .expect_err("a cancelled export has no result");
    assert_eq!(error.code, ExportErrorCode::Cancelled);
    assert_eq!(error.message, "The download was cancelled.");
    assert_eq!(error.to_string(), messages::DOWNLOAD_WAS_CANCELLED);
    assert_eq!(reports, [0, 1]);
    // The part that held the first post is gone.
    assert_eq!(workspace.target_names(), Vec::<String>::new());

    // Cancelled before it starts.
    let (reports, error) = cancelled_at(&workspace, &posts, None);
    assert_eq!(error.code, ExportErrorCode::Cancelled);
    assert_eq!(reports, [0]);

    // Cancelled during the last report: the archive is complete, but it is not handed over.
    let (reports, error) = cancelled_at(&workspace, &posts, Some(3));
    assert_eq!(error.code, ExportErrorCode::Cancelled);
    assert_eq!(reports, [0, 1, 2, 3]);
    assert_eq!(workspace.target_names(), Vec::<String>::new());

    // The same with several parts.
    let split = ExportOptions { max_part_posts: 1, ..options() };
    let cancel = AtomicBool::new(false);
    let error = export_posts(
        &posts,
        &workspace.target,
        &split,
        &mut |progress: ExportProgress| cancel.store(progress.done == 2, Ordering::Relaxed),
        &cancel,
    )
    .expect_err("a cancelled export has no result");
    assert_eq!(error.code, ExportErrorCode::Cancelled);
    assert_eq!(workspace.target_names(), Vec::<String>::new());
}

/// Runs an export that is cancelled when `done` posts are reported (`None`: from the start).
fn cancelled_at(workspace: &Workspace, posts: &[Post], done: Option<usize>) -> (Vec<usize>, ExportError) {
    let cancel = AtomicBool::new(done.is_none());
    let mut reports = Vec::new();
    let error = export_posts(
        posts,
        &workspace.target,
        &options(),
        &mut |progress: ExportProgress| {
            reports.push(progress.done);
            if Some(progress.done) == done {
                cancel.store(true, Ordering::Relaxed);
            }
        },
        &cancel,
    )
    .expect_err("a cancelled export has no result");
    (reports, error)
}

// --- What export.test.ts does not reach: the other problem sentences --------------------------

#[test]
fn says_what_was_left_out_and_why() {
    let workspace = Workspace::new("problems");
    let clip = workspace.write("posts/clip.mp4", &mp4(1));
    let clip_poster = workspace.write("posts/clip-poster.webm", &webm(2));
    let notes = workspace.write("posts/notes.txt", b"These are notes, not a picture.");
    let notes_poster = workspace.write("posts/notes-poster.txt", b"<svg xmlns=\"http://www.w3.org/2000/svg\"></svg>");
    let empty = workspace.write("posts/empty.jpg", b"");
    let good = workspace.write("posts/good.jpg", &jpeg(3));
    let text_avatar = workspace.write("avatars/text.jpg", b"not a picture either");
    let video_avatar = workspace.write("avatars/video.jpg", &mp4(4));
    let missing = workspace.path("posts/missing.mp4");
    let missing_poster = workspace.path("posts/missing-poster.jpg");
    let missing_avatar = workspace.path("avatars/missing.jpg");
    let reason = File::open(&missing).expect_err("the file does not exist").to_string();

    // Post 1: a video whose poster is a video, a video and a poster that are gone, a text file
    // with a text file as poster, an empty file; the profile picture is gone.
    let mut first = post(
        "A",
        "ada",
        vec![
            video(&clip, "posts/clip.mp4", Some((&clip_poster, "posts/clip-poster.webm"))),
            video(&missing, "posts/missing.mp4", Some((&missing_poster, "posts/missing-poster.jpg"))),
            video(&notes, "posts/notes.txt", Some((&notes_poster, "posts/notes-poster.txt"))),
            picture(&empty, "posts/empty.jpg"),
        ],
    );
    first.user.avatar = Some(missing_avatar.clone());
    first.media_type = MediaType::Carousel;
    // Post 2: the same author and profile picture (reported once); its picture has a poster,
    // which is only looked at for a video.
    let mut good_item = picture(&good, "posts/good.jpg");
    good_item.poster =
        Some(Poster { path: missing_poster.clone(), manifest_path: "posts/missing-poster.jpg".to_owned() });
    let mut second = post("B", "ada", vec![good_item]);
    second.user.avatar = Some(missing_avatar);
    // Posts 3 and 4: profile pictures that are a text file and a video.
    let mut third = post("C", "cleo", vec![picture(&good, "posts/good.jpg")]);
    third.user.avatar = Some(text_avatar);
    let mut fourth = post("D", "dana", vec![picture(&good, "posts/good.jpg")]);
    fourth.user.avatar = Some(video_avatar);
    // Post 5: nothing is left of it, so its profile picture is never looked at.
    let mut fifth = post("E", "eve", vec![picture(&missing, "posts/gone.jpg")]);
    fifth.user.avatar = Some(workspace.path("avatars/never-read.jpg"));
    // Post 6: built without any media.
    let sixth = post("F", "fay", Vec::new());

    let (result, progress) = run(&[first, second, third, fourth, fifth, sixth], &workspace.target, &options());
    let result = result.expect("the export succeeds");
    assert_eq!(
        result.problems,
        [
            "Post 1 (A): The poster of \"posts/clip.mp4\" is not a picture.".to_owned(),
            format!("Post 1 (A): \"posts/missing.mp4\" could not be read: {reason}."),
            format!("Post 1 (A): The poster of \"posts/missing.mp4\" could not be read: {reason}."),
            "Post 1 (A): \"posts/notes.txt\" is not a picture or video.".to_owned(),
            "Post 1 (A): The poster of \"posts/notes.txt\" is not a picture or video.".to_owned(),
            "Post 1 (A): \"posts/empty.jpg\" is not a picture or video.".to_owned(),
            format!("Post 1 (A): The profile picture could not be read: {reason}."),
            "Post 3 (C): The profile picture is not a picture or video.".to_owned(),
            "Post 4 (D): the profile picture is not a picture.".to_owned(),
            format!("Post 5 (E): \"posts/gone.jpg\" could not be read: {reason}."),
            "Post 5 (E) was left out: none of its pictures or videos could be saved.".to_owned(),
            "Post 6 (F) was left out: none of its pictures or videos could be saved.".to_owned(),
        ]
    );
    // The reason is the operating system's sentence, and the problem ends with one full stop.
    assert!(reason.contains("os error"), "{reason}");
    assert!(!result.problems[1].ends_with(".."));
    assert_eq!(result.posts, 4);
    assert_eq!(progress.len(), 7);
    assert_eq!(progress.last(), Some(&(6, 6)));

    let entries = read_archive(&result.archives[0].path);
    assert_well_formed(&entries, NOW_ENTRY_TIME);
    assert_eq!(
        names(&entries),
        ["posts/post1.mp4", "posts/post2.jpg", "posts/post3.jpg", "posts/post4.jpg", "post.json"]
    );
    let manifest = manifest_of(&entries);
    // The carousel lost its slides down to one video, without a poster.
    assert_eq!(manifest["posts"][0]["media"], json!([{ "type": "video", "path": "posts/post1.mp4" }]));
    assert_eq!(manifest["posts"][0]["media_type"], "video");
    assert_eq!(manifest["posts"][1]["media"], json!([{ "type": "image", "path": "posts/post2.jpg" }]));
    for post in manifest["posts"].as_array().expect("posts is a list") {
        assert_eq!(keys(&post["user"]), ["username"]);
    }
}

#[test]
fn writes_the_alt_of_a_slide_only_when_it_differs_from_the_alt_of_the_post() {
    let workspace = Workspace::new("alt");
    let mut slides = Vec::new();
    for (index, alt) in [Some("Same"), Some(""), Some("Other"), None].into_iter().enumerate() {
        let relative = format!("posts/slide{index}.jpg");
        let path = workspace.write(&relative, &jpeg(index as u8));
        let mut slide = picture(&path, &relative);
        slide.alt = alt.map(str::to_owned);
        slides.push(slide);
    }
    let mut carousel = post("alts", "lumen.atlas", slides.clone());
    carousel.alt = Some("Same".to_owned());
    carousel.media_type = MediaType::Carousel;
    // Without an alt of the post, every non-empty slide alt is written.
    let mut plain = post("plain", "lumen.atlas", slides);
    plain.media_type = MediaType::Carousel;

    let result = exported(&[carousel, plain], &workspace.target, &options());
    let entries = read_archive(&result.archives[0].path);
    let manifest = manifest_of(&entries);
    assert_eq!(
        manifest["posts"][0]["media"],
        json!([
            { "type": "image", "path": "posts/post1_1.jpg" },
            { "type": "image", "path": "posts/post1_2.jpg" },
            { "type": "image", "path": "posts/post1_3.jpg", "alt": "Other" },
            { "type": "image", "path": "posts/post1_4.jpg" }
        ])
    );
    assert_eq!(manifest["posts"][0]["alt"], "Same");
    assert_eq!(
        manifest["posts"][1]["media"],
        json!([
            { "type": "image", "path": "posts/post2_1.jpg", "alt": "Same" },
            { "type": "image", "path": "posts/post2_2.jpg" },
            { "type": "image", "path": "posts/post2_3.jpg", "alt": "Other" },
            { "type": "image", "path": "posts/post2_4.jpg" }
        ])
    );
}

#[test]
fn an_author_whose_first_avatar_failed_gets_the_next_one_that_works() {
    let workspace = Workspace::new("avatars");
    let picture_path = workspace.write("posts/a.jpg", &jpeg(1));
    let working = workspace.write("avatars/working.png", &png(2));
    let other = workspace.write("avatars/other.jpg", &jpeg(3));
    let with_avatar = |id: &str, avatar: PathBuf| {
        let mut post = post(id, "ada", vec![picture(&picture_path, "posts/a.jpg")]);
        post.user.avatar = Some(avatar);
        post
    };
    let posts = [
        with_avatar("one", workspace.path("avatars/gone.jpg")),
        with_avatar("two", working),
        // Within one archive the first avatar of a username wins.
        with_avatar("three", other),
    ];
    let result = exported(&posts, &workspace.target, &options());
    assert_eq!(result.problems.len(), 1);
    assert!(result.problems[0].starts_with("Post 1 (one): The profile picture could not be read: "));
    let entries = read_archive(&result.archives[0].path);
    assert_eq!(
        names(&entries),
        ["posts/post1.jpg", "posts/post2.jpg", "avatars/ada.png", "posts/post3.jpg", "post.json"]
    );
    assert_eq!(bytes_of(&entries, "avatars/ada.png"), png(2));
    let manifest = manifest_of(&entries);
    assert_eq!(manifest["posts"][0]["user"], json!({ "username": "ada" }));
    assert_eq!(manifest["posts"][1]["user"], json!({ "username": "ada", "avatar": "avatars/ada.png" }));
    assert_eq!(manifest["posts"][2]["user"], json!({ "username": "ada", "avatar": "avatars/ada.png" }));
}

// --- Files: chunks, times, replacing, cleaning up ---------------------------------------------

#[test]
fn copies_a_file_larger_than_one_chunk_byte_for_byte() {
    let workspace = Workspace::new("chunks");
    // Two full chunks and a rest, with content that differs from chunk to chunk.
    let mut large = jpeg(0);
    large.resize(2 * READ_CHUNK_BYTES + 12_345, 0);
    for (index, byte) in large.iter_mut().enumerate().skip(64) {
        *byte = (index % 251) as u8 ^ (index >> 16) as u8;
    }
    let path = workspace.write("large.jpg", &large);
    let exactly_one_chunk = workspace.write("chunk.png", &{
        let mut bytes = png(7);
        bytes.resize(READ_CHUNK_BYTES, 7);
        bytes
    });
    let posts = [
        post("large", "unknown", vec![picture(&path, "large.jpg")]),
        post("chunk", "unknown", vec![picture(&exactly_one_chunk, "chunk.png")]),
    ];
    let result = exported(&posts, &workspace.target, &options());
    assert_eq!(result.problems, Vec::<String>::new());
    let entries = read_archive(&result.archives[0].path);
    assert_well_formed(&entries, NOW_ENTRY_TIME);
    assert_eq!(names(&entries), ["posts/post1.jpg", "posts/post2.png", "post.json"]);
    assert!(bytes_of(&entries, "posts/post1.jpg") == large.as_slice(), "the large file was changed on the way");
    assert_eq!(bytes_of(&entries, "posts/post2.png").len(), READ_CHUNK_BYTES);
}

#[test]
fn every_entry_carries_the_local_time_of_the_export() {
    let workspace = Workspace::new("times");
    let posts = source_posts(&workspace);
    // 10:00:05.417 UTC, in a zone ninety minutes east: 11:30:04 with the two-second steps of ZIP.
    let later = ExportOptions::new(DEFAULT_EXPORT_NAME, NOW_MS + 5_417, 90);
    let result = exported(&posts, &workspace.target, &later);
    let entries = read_archive(&result.archives[0].path);
    assert_well_formed(&entries, (2026, 11, 11, 11, 30, 4));
    // exported_at is the instant itself, in UTC, with its milliseconds.
    assert_eq!(manifest_of(&entries)["exported_at"], "2026-11-11T10:00:05.417Z");

    // West of UTC, across midnight.
    let west = ExportOptions::new("west", NOW_MS, -11 * 60);
    let result = exported(&posts, &workspace.target, &west);
    assert_well_formed(&read_archive(&result.archives[0].path), (2026, 11, 10, 23, 0, 0));
}

#[test]
fn replaces_the_files_whose_names_the_user_chose() {
    let workspace = Workspace::new("replace");
    let posts = source_posts(&workspace);
    fs::write(workspace.target.join("instagram-export.zip"), b"an older download").expect("a file can be written");
    fs::write(workspace.target.join("unrelated.txt"), b"keep me").expect("a file can be written");
    let result = exported(&posts, &workspace.target, &options());
    assert_eq!(workspace.target_names(), ["instagram-export.zip", "unrelated.txt"]);
    assert_eq!(read_archive(&result.archives[0].path).len(), 8);
    assert_eq!(fs::read(workspace.target.join("unrelated.txt")).expect("the file is still there"), b"keep me");

    // Parts replace parts of the same names; the single archive of the earlier download is not
    // one of them and stays.
    fs::write(workspace.target.join("instagram-export-part2.zip"), b"an older part").expect("a file can be written");
    let split = ExportOptions { max_part_posts: 1, ..options() };
    let result = exported(&posts, &workspace.target, &split);
    assert_eq!(
        workspace.target_names(),
        [
            "instagram-export-part1.zip",
            "instagram-export-part2.zip",
            "instagram-export-part3.zip",
            "instagram-export.zip",
            "unrelated.txt"
        ]
    );
    let second = read_archive(&result.archives[1].path);
    assert_eq!(names(&second), ["posts/post2.png", "avatars/lumen.atlas.jpg", "post.json"]);
}

#[test]
fn a_folder_that_does_not_exist_is_an_error_and_is_not_created() {
    let workspace = Workspace::new("no-folder");
    let posts = source_posts(&workspace);
    let missing = workspace.target.join("not").join("there");
    let (result, _) = run(&posts, &missing, &options());
    let error = result.expect_err("the archive cannot be written");
    assert_eq!(error.code, ExportErrorCode::WriteFailed);
    assert!(error.message.starts_with("The archive could not be written: "), "{}", error.message);
    assert!(error.message.contains("os error"), "{}", error.message);
    assert!(!missing.exists());
    assert_eq!(workspace.target_names(), Vec::<String>::new());
}

#[test]
fn a_part_that_cannot_get_its_name_takes_the_whole_export_with_it() {
    let workspace = Workspace::new("blocked");
    let posts = source_posts(&workspace);
    // A folder has the name the second part needs, so that part cannot be renamed.
    let blocker = workspace.target.join("instagram-export-part2.zip");
    fs::create_dir(&blocker).expect("a folder can be created");
    let split = ExportOptions { max_part_posts: 1, ..options() };
    let (result, progress) = run(&posts, &workspace.target, &split);
    let error = result.expect_err("the second part cannot be renamed");
    assert_eq!(error.code, ExportErrorCode::WriteFailed);
    // The sentence names the file that is in the way.
    assert!(
        error.message.starts_with("The archive could not be written to \"instagram-export-part2.zip\": "),
        "{}",
        error.message
    );
    assert_eq!(progress.last(), Some(&(3, 3)));
    // The first part had its name already; it is removed again, as are the temporary files.
    assert_eq!(workspace.target_names(), ["instagram-export-part2.zip"]);
    assert!(blocker.is_dir());
}

#[test]
fn a_file_that_disappears_while_the_export_runs_stops_it() {
    let workspace = Workspace::new("vanished");
    let posts = source_posts(&workspace);
    let avatar = workspace.path("avatars/lumen.atlas.jpg");
    // The avatar is looked at once, for the first post. With one post per part the second post
    // needs it again, and by then it is gone.
    let split = ExportOptions { max_part_posts: 1, ..options() };
    let error = export_posts(
        &posts,
        &workspace.target,
        &split,
        &mut |progress: ExportProgress| {
            if progress.done == 1 {
                fs::remove_file(&avatar).expect("the avatar can be removed");
            }
        },
        &AtomicBool::new(false),
    )
    .expect_err("the avatar cannot be copied");
    assert_eq!(error.code, ExportErrorCode::WriteFailed);
    assert!(
        error.message.starts_with("The archive could not be written: The profile picture could not be read: "),
        "{}",
        error.message
    );
    assert_eq!(workspace.target_names(), Vec::<String>::new());
}

// --- The clone's validator and JSON.stringify, through Node.js --------------------------------

/// True when Node.js can be started. The tests below print a note and pass without it: the
/// project itself does not need Node.js.
fn node_is_installed() -> bool {
    Command::new("node").arg("--version").output().map(|output| output.status.success()).unwrap_or(false)
}

/// Extracts an archive into `folder`, as a user would before importing or validating it.
fn extract(archive: &Path, folder: &Path) {
    for entry in read_archive(archive) {
        let path = folder.join(&entry.name);
        fs::create_dir_all(path.parent().expect("an entry has a folder")).expect("a folder can be created");
        fs::write(&path, &entry.bytes).expect("an entry can be extracted");
    }
}

/// The outcome of `node validate-export.mjs <folder>`: exit code and the printed lines.
fn validate(folder: &Path) -> (Option<i32>, Vec<String>) {
    let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("validator").join("validate-export.mjs");
    let output = Command::new("node").arg(&script).arg(folder).output().expect("node can be started");
    let mut lines: Vec<String> = String::from_utf8_lossy(&output.stdout).lines().map(str::to_owned).collect();
    lines.extend(String::from_utf8_lossy(&output.stderr).lines().map(str::to_owned));
    (output.status.code(), lines)
}

/// A loose picture as the app opens it: the file name as id and sub-line, the default user.
fn loose_picture(file_name: &str, id: &str, taken_at: i64) -> Post {
    let path = fixtures().join("loose").join(file_name);
    let mut post = post(id, "unknown", vec![picture(&path, file_name)]);
    post.taken_at = Some(taken_at);
    post.context = Some(file_name.to_owned());
    post
}

/// Acceptance for loose pictures (spec-export G.2): exit code 0, no ERROR, and the only WARN is
/// the one about the missing "url", once per post.
#[test]
fn an_export_of_loose_pictures_passes_the_validator_of_the_clone() {
    let workspace = Workspace::new("validator-loose");
    let mut posts = vec![
        loose_picture("IMG_2.jpg", "IMG_2", RIDGE_MS),
        loose_picture("IMG_10.jpg", "IMG_10", RIDGE_MS + 417),
        loose_picture("img_1.png", "img_1", SECOND_MS),
        loose_picture("Sunset at the pier (final).webp", "Sunset_at_the_pier_final", REEL_MS),
        loose_picture("portrait.heic", "portrait", REEL_MS),
        // The same id again, as two folders may give it.
        loose_picture("IMG_10.jpg", "IMG_2", RIDGE_MS),
    ];
    posts[0].caption = "Sunday at the pier 🌊\n\"Low tide\", they said \\ they were right.\n#朝ごはん".to_owned();
    posts[3].caption = "  kept as typed  ".to_owned();
    let result = exported(&posts, &workspace.target, &options());
    assert_eq!(result.problems, Vec::<String>::new());
    assert_eq!(result.posts, 6);

    let entries = read_archive(&result.archives[0].path);
    assert_well_formed(&entries, NOW_ENTRY_TIME);
    assert_eq!(
        names(&entries),
        [
            "posts/post1.jpg",
            "posts/post2.jpg",
            "posts/post3.png",
            "posts/post4.webp",
            "posts/post5.heic",
            "posts/post6.jpg",
            "post.json"
        ]
    );
    // The bytes on disk, also for the picture this app cannot decode by itself.
    let heic = fs::read(fixtures().join("loose").join("portrait.heic")).expect("the fixture exists");
    assert_eq!(bytes_of(&entries, "posts/post5.heic"), heic);
    let manifest = manifest_of(&entries);
    assert_eq!(
        manifest["posts"][0],
        json!({
            "id": "IMG_2",
            "media": [{ "type": "image", "path": "posts/post1.jpg" }],
            "user": { "username": "unknown" },
            "caption": "Sunday at the pier 🌊\n\"Low tide\", they said \\ they were right.\n#朝ごはん",
            "comment_count": 0,
            "like_count": null,
            "taken_at": "2026-11-01T15:04:05Z",
            "media_type": "image"
        })
    );
    assert_eq!(manifest["posts"][1]["taken_at"], "2026-11-01T15:04:05.417Z");
    assert_eq!(manifest["posts"][3]["caption"], "  kept as typed  ");
    assert_eq!(manifest["posts"][5]["id"], "IMG_2-2");

    if !node_is_installed() {
        println!("note: node is not on PATH; the validator of the clone was not run");
        return;
    }
    let folder = workspace.root.join("extracted");
    extract(&result.archives[0].path, &folder);
    let (code, lines) = validate(&folder);
    assert_eq!(code, Some(0), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.contains("ERROR")), "{lines:#?}");
    let warnings: Vec<&String> = lines.iter().filter(|line| line.starts_with("WARN")).collect();
    assert_eq!(warnings.len(), 6, "{lines:#?}");
    for (index, warning) in warnings.iter().enumerate() {
        assert_eq!(
            warning.as_str(),
            format!("WARN: posts[{index}]: no \"url\"; the clone cannot link back to Instagram"),
            "{lines:#?}"
        );
    }
    assert_eq!(
        lines.last().map(String::as_str),
        Some("OK: 6 post(s), 6 image file(s), 0 video file(s), 6 warning(s)"),
        "{lines:#?}"
    );
}

/// Acceptance for loose videos (video brief 5.8): a folder of a picture, an MP4 made by the test
/// and a WebM of the clone's fixture, opened as the app opens it and downloaded with the posters
/// of the poster cache. Exit code 0, no ERROR, the only WARN the missing "url"; the videos are
/// the bytes of the files and each has its first frame as its poster.
#[cfg(windows)]
#[test]
fn an_export_of_loose_videos_passes_the_validator_of_the_clone() {
    use instagram_core::catalog::{self, OpenContext, OpenOptions, OpenRequest};
    use instagram_core::poster::{fixture, PosterCache};
    use instagram_core::store::DescriptionStore;

    let workspace = Workspace::new("validator-videos");
    let folder = workspace.source.join("mixed");
    fs::create_dir_all(&folder).expect("a folder");
    fs::copy(fixtures().join("loose").join("IMG_2.jpg"), folder.join("a.jpg")).expect("a picture");
    fixture::write_mp4(&folder.join("b.m4v"), fixture::FixtureSpec::SHORT).expect("the sink writer makes a video");
    let mut zip = ZipArchive::new(File::open(fixtures().join("archives").join("export-videos.zip")).expect("the clone's fixture")).expect("a zip");
    let mut webm = Vec::new();
    zip.by_name("posts/reel1.webm").expect("the WebM").read_to_end(&mut webm).expect("bytes");
    fs::write(folder.join("c.webm"), &webm).expect("the WebM");
    let before: Vec<String> = fs::read_dir(&folder).expect("listing").flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();

    let store = DescriptionStore::new(workspace.root.join("descriptions.json"));
    let context = OpenContext::new(workspace.root.join("archives"));
    let request = OpenRequest { paths: vec![folder.clone()], options: OpenOptions { default_username: "tester".to_owned() } };
    let posts = catalog::open_paths(&request, &store, &context).expect("the folder opens").collection.posts;
    assert_eq!(posts.iter().filter(|post| post.is_video()).count(), 2);
    let posters = PosterCache::new(workspace.root.join("posters"));
    let result = instagram_core::export::export_posts_with_posters(&posts, &workspace.target, &options(), &posters, &mut |_| {}, &AtomicBool::new(false)).expect("exports");
    assert_eq!(result.problems, Vec::<String>::new());
    let entries = read_archive(&result.archives[0].path);
    assert_well_formed(&entries, NOW_ENTRY_TIME);
    assert_eq!(names(&entries), ["posts/post1.jpg", "posts/post2.mp4", "posts/post2_poster.jpg", "posts/post3.webm", "posts/post3_poster.jpg", "post.json"]);
    assert_eq!(bytes_of(&entries, "posts/post2.mp4"), fs::read(folder.join("b.m4v")).expect("the video"), "the video is copied byte for byte");
    assert_eq!(bytes_of(&entries, "posts/post3.webm"), webm.as_slice());
    let manifest = manifest_of(&entries);
    assert_eq!(manifest["posts"][1]["media_type"], "video");
    assert_eq!(manifest["posts"][1]["media"], json!([{ "type": "video", "path": "posts/post2.mp4", "poster": "posts/post2_poster.jpg" }]));
    // Nothing was written next to the videos.
    let after: Vec<String> = fs::read_dir(&folder).expect("listing").flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(before, after);

    if !node_is_installed() {
        println!("note: node is not on PATH; the validator of the clone was not run");
        return;
    }
    let extracted = workspace.root.join("extracted");
    extract(&result.archives[0].path, &extracted);
    let (code, lines) = validate(&extracted);
    assert_eq!(code, Some(0), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.contains("ERROR")), "{lines:#?}");
    let warnings: Vec<&String> = lines.iter().filter(|line| line.starts_with("WARN")).collect();
    assert_eq!(warnings.len(), 3, "{lines:#?}");
    assert!(warnings.iter().all(|warning| warning.contains("no \"url\"")), "{lines:#?}");
    assert_eq!(lines.last().map(String::as_str), Some("OK: 3 post(s), 3 image file(s), 2 video file(s), 3 warning(s)"), "{lines:#?}");
}

/// The six posts of the fixture export-basic as the app holds them, plus two videos: every post
/// has an Instagram permalink.
fn posts_with_permalinks(workspace: &Workspace) -> Vec<Post> {
    let basic = fixtures().join("export-basic");
    let bakery = "fixture.sunrise.bakery";
    let tidewater = "fixture_tidewater";
    let entry = |number: usize, username: &str, files: &[&str]| {
        let slides = files.iter().map(|file| picture(&basic.join(file), file)).collect();
        let mut post = post(&format!("FXTbasic00{number}"), username, slides);
        post.permalink = Some(format!("https://www.instagram.com/p/FXTbasic00{number}/"));
        post.user.avatar = Some(basic.join("avatars").join(format!("{username}.jpg")));
        post.user.profile_url = Some(format!("https://www.instagram.com/{username}/"));
        post.caption = format!("Post {number} of the fixture.");
        post.original_caption = post.caption.clone();
        post.comment_count = 40 + number as u64;
        post.like_count = Some(1000 * number as u64);
        post.taken_at = Some(RIDGE_MS - 86_400_000 * number as i64);
        post
    };

    let mut first = entry(1, bakery, &["posts/post1.jpg"]);
    first.user.full_name = Some("Sunrise Bakery (test fixture)".to_owned());
    first.user.verified = Some(true);
    first.caption = "Fresh out of the oven 🥐☀️\nBaked together with @fixture_tidewater.\n#sourdough #朝ごはん".to_owned();
    first.alt = Some("Orange test card labelled POST 1".to_owned());
    first.location = Some("Fixtureville, Testland".to_owned());
    first.comments = vec![
        Comment {
            id: "FXTbasic001-c1".to_owned(),
            username: tidewater.to_owned(),
            text: "Saving me two of those 🙌".to_owned(),
            likes: 3,
            taken_at: Some(RIDGE_COMMENT_MS),
        },
        Comment {
            id: "FXTbasic001-c2".to_owned(),
            username: "pixel.orchard.demo".to_owned(),
            text: "The crumb on that loaf! #goals".to_owned(),
            likes: 0,
            taken_at: None,
        },
    ];
    let mut second = entry(2, tidewater, &["posts/post2.png"]);
    second.user.verified = Some(false);
    second.like_count = None;
    second.caption = "潮が引いたあとの静かな海 🌊\nВечерний отлив и тишина.\nمساء هادئ على الشاطئ".to_owned();
    let third = entry(3, bakery, &["posts/post3.webp"]);
    let fourth = entry(4, tidewater, &["posts/post4.heic"]);
    let mut fifth = entry(5, bakery, &["posts/post5_1.jpg", "posts/post5_2.jpg"]);
    fifth.media_type = MediaType::Carousel;
    fifth.media[1].alt = Some("The second slide".to_owned());
    // A video post of which the export held only the cover.
    let mut sixth = entry(6, tidewater, &["posts/post6.jpg"]);
    sixth.media_type = MediaType::Video;

    // Two videos with posters; the fixtures have none, so these are the clone's test bytes.
    let clip = workspace.write("posts/clip.mp4", &mp4(21));
    let clip_poster = workspace.write("posts/clip.png", &png(22));
    let reel = workspace.write("posts/reel.webm", &webm(23));
    let reel_poster = workspace.write("posts/reel.webp", &webp(24));
    let mut seventh = entry(7, bakery, &[]);
    seventh.media = vec![video(&clip, "posts/clip.mp4", Some((&clip_poster, "posts/clip.png")))];
    seventh.permalink = Some("https://www.instagram.com/reel/FXTbasic007/".to_owned());
    seventh.media_type = MediaType::Video;
    let mut eighth = entry(8, "mira.films", &["posts/post1.jpg"]);
    eighth.media.push(video(&reel, "posts/reel.webm", Some((&reel_poster, "posts/reel.webp"))));
    eighth.user.avatar = None;
    eighth.media_type = MediaType::Carousel;

    vec![first, second, third, fourth, fifth, sixth, seventh, eighth]
}

/// Acceptance for posts of an export: exit code 0 and neither ERROR nor WARN, which is the bar
/// of the clone's own end-to-end tests (edit-download.spec.ts:86-88, 114-116).
#[test]
fn an_export_of_posts_with_permalinks_passes_the_validator_without_a_warning() {
    let workspace = Workspace::new("validator-links");
    let posts = posts_with_permalinks(&workspace);
    let result = exported(&posts, &workspace.target, &options());
    assert_eq!(result.problems, Vec::<String>::new());
    assert_eq!(result.posts, 8);
    let entries = read_archive(&result.archives[0].path);
    assert_well_formed(&entries, NOW_ENTRY_TIME);
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
            "posts/post7.mp4",
            "posts/post7_poster.png",
            "posts/post8_1.jpg",
            "posts/post8_2.webm",
            "posts/post8_2_poster.webp",
        ]
    );
    let manifest = manifest_of(&entries);
    assert_eq!(manifest["posts"][5]["media_type"], "video");
    assert_eq!(manifest["posts"][5]["media"][0]["type"], "image");
    assert_eq!(manifest["posts"][0]["user"]["profile_url"], "https://www.instagram.com/fixture.sunrise.bakery/");

    // The same posts in parts of three: every part is an export of its own.
    let split = ExportOptions { max_part_posts: 3, name: "in-parts".to_owned(), ..options() };
    let parts = exported(&posts, &workspace.target, &split);
    assert_eq!(parts.archives.iter().map(|archive| archive.posts).collect::<Vec<_>>(), [3, 3, 2]);

    if !node_is_installed() {
        println!("note: node is not on PATH; the validator of the clone was not run");
        return;
    }
    let folder = workspace.root.join("extracted");
    extract(&result.archives[0].path, &folder);
    let (code, lines) = validate(&folder);
    assert_eq!(code, Some(0), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.contains("ERROR") || line.contains("WARN")), "{lines:#?}");
    assert_eq!(
        lines.last().map(String::as_str),
        Some("OK: 8 post(s), 12 image file(s), 2 video file(s), 0 warning(s)"),
        "{lines:#?}"
    );

    for (index, archive) in parts.archives.iter().enumerate() {
        let folder = workspace.root.join(format!("part{}", index + 1));
        extract(&archive.path, &folder);
        let (code, lines) = validate(&folder);
        assert_eq!(code, Some(0), "{}: {lines:#?}", archive.name);
        assert!(
            !lines.iter().any(|line| line.contains("ERROR") || line.contains("WARN")),
            "{}: {lines:#?}",
            archive.name
        );
        let ok = format!("OK: {} post(s), ", archive.posts);
        assert!(lines.last().is_some_and(|line| line.starts_with(&ok)), "{}: {lines:#?}", archive.name);
    }
}

/// Links that the validator would report as errors are left out (spec-export G.3), so such a
/// post gets the "no url" warning instead of making the export invalid.
#[test]
fn links_to_other_sites_are_left_out_so_the_export_stays_valid() {
    let workspace = Workspace::new("validator-other-links");
    let mut posts = posts_with_permalinks(&workspace);
    posts.truncate(2);
    posts[0].permalink = Some("https://example.com/p/FXTbasic001/".to_owned());
    posts[0].user.profile_url = Some("https://example.com/fixture.sunrise.bakery/".to_owned());
    posts[1].permalink = Some("http://www.instagram.com/p/FXTbasic002/".to_owned());
    let result = exported(&posts, &workspace.target, &options());
    let entries = read_archive(&result.archives[0].path);
    let manifest = manifest_of(&entries);
    assert!(manifest["posts"][0].get("url").is_none());
    assert!(manifest["posts"][0]["user"].get("profile_url").is_none());
    assert!(manifest["posts"][1].get("url").is_none());
    assert_eq!(manifest["posts"][1]["user"]["profile_url"], "https://www.instagram.com/fixture_tidewater/");

    if !node_is_installed() {
        println!("note: node is not on PATH; the validator of the clone was not run");
        return;
    }
    let folder = workspace.root.join("extracted");
    extract(&result.archives[0].path, &folder);
    let (code, lines) = validate(&folder);
    assert_eq!(code, Some(0), "{lines:#?}");
    assert!(!lines.iter().any(|line| line.contains("ERROR")), "{lines:#?}");
    assert_eq!(lines.iter().filter(|line| line.starts_with("WARN")).count(), 2, "{lines:#?}");
    assert!(lines.iter().filter(|line| line.starts_with("WARN")).all(|line| line.contains("no \"url\"")), "{lines:#?}");
}

/// post.json must be the bytes JavaScript writes for the same data. Node.js reads the file of an
/// archive and writes it again with `JSON.stringify(value, null, 2) + "\n"`; nothing may change.
#[test]
fn post_json_is_byte_for_byte_what_json_stringify_writes() {
    let workspace = Workspace::new("stringify");
    let mut posts = posts_with_permalinks(&workspace);
    // Everything a writer could escape differently: quotes, backslashes, slashes, line breaks of
    // every kind, a tab, control characters, DEL, the Unicode line separators, a byte order mark
    // inside the text, emoji (surrogate pairs in JavaScript), combining marks, right-to-left text.
    posts[0].caption = concat!(
        "\"quoted\" 'single' \\ back\\slash </script> a/b\r\nCRLF\rCR\nLF\ttab ",
        "\u{0}\u{1}\u{8}\u{b}\u{c}\u{e}\u{1f} \u{7f}\u{80}\u{9f} \u{2028}\u{2029}\u{feff} ",
        "🌄👩‍👩‍👧‍👦🇫🇷 e\u{301} 朝ごはん مساء \u{d7ff}\u{e000}\u{fffd}\u{ffff}\u{10ffff}"
    )
    .to_owned();
    posts[0].location = Some("Chamonix \"Mont-Blanc\", France".to_owned());
    posts[0].comments[0].text = "back\\slash and \"quotes\" 🙌\n".to_owned();
    posts[1].caption = String::new();
    posts[2].like_count = Some(9_007_199_254_740_991);
    posts[2].comment_count = 1_234_567;
    let result = exported(&posts, &workspace.target, &options());
    let entries = read_archive(&result.archives[0].path);
    let written = bytes_of(&entries, "post.json");
    // The text survives the trip through the file.
    assert_eq!(manifest_of(&entries)["posts"][0]["caption"].as_str(), Some(posts[0].caption.as_str()));

    if !node_is_installed() {
        println!("note: node is not on PATH; post.json was not compared with JSON.stringify");
        return;
    }
    let file = workspace.root.join("post.json");
    fs::write(&file, written).expect("post.json can be written");
    let script = "const fs = require('fs'); \
                  const text = fs.readFileSync(process.argv[1], 'utf8'); \
                  process.stdout.write(JSON.stringify(JSON.parse(text), null, 2) + '\\n');";
    let output = Command::new("node").arg("-e").arg(script).arg(&file).output().expect("node can be started");
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    assert!(
        output.stdout == written,
        "JSON.stringify writes post.json differently:\n{}\n--- instead of ---\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(written)
    );
}
