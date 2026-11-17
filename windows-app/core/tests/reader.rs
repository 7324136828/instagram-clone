//! Reading, driven by the fixtures: the lenient post.json reader (`manifest`) and the catalog
//! that opens paths (`catalog`), through the public interface of the crate.
//!
//! The fixtures in tests/fixtures are those of the reference web project (e2e/fixtures; see
//! spec-export E): export-basic, export-lenient, export-problems and export-wrapped are exports,
//! loose/ is a folder of pictures without a post.json. They are only read. Tests that need
//! other files build them in a folder of their own below the system's temporary folder and
//! remove it again; nothing here touches the user profile.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use instagram_core::catalog::{
    self, AddOutcome, OpenContext, OpenErrorCode, OpenOptions, OpenRequest, Opened, RequestKind, SourceRequest,
    MANIFEST_SEARCH_DEPTH,
};
use instagram_core::manifest::{self, ManifestErrorCode};
use instagram_core::messages;
use instagram_core::model::{CollectionSource, ExportKind, MediaKind, MediaType, Post, PostKey, WarningCode};
use instagram_core::store::{DescriptionEntry, DescriptionStore};
use instagram_core::text;

// --- Helpers ------------------------------------------------------------------------------------

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

fn fixture(name: &str) -> PathBuf {
    fixtures().join(name)
}

fn options() -> OpenOptions {
    OpenOptions { default_username: "tester".to_owned() }
}

/// A store without any description; it is never saved, so its path is never written.
fn empty_store() -> DescriptionStore {
    DescriptionStore::new(PathBuf::from("descriptions-of-a-test.json"))
}

fn open(paths: &[PathBuf]) -> Opened {
    open_with(paths, &empty_store())
}

fn open_with(paths: &[PathBuf], store: &DescriptionStore) -> Opened {
    catalog::open_paths(&OpenRequest { paths: paths.to_vec(), options: options() }, store, &context()).expect("the paths open")
}

/// Where these tests would unpack an archive (they open none).
fn context() -> OpenContext {
    OpenContext::new(std::env::temp_dir().join(format!("instagram-core-reader-cache-{}", std::process::id())))
}

/// What a request of the one export folder of `manifest` classifies as.
fn one_folder(manifest: PathBuf) -> RequestKind {
    RequestKind::Exports { sources: vec![SourceRequest::Folder { manifest }], others: vec![] }
}

fn ids(posts: &[Post]) -> Vec<&str> {
    posts.iter().map(|post| post.id.as_str()).collect()
}

fn file_names(posts: &[Post]) -> Vec<String> {
    posts.iter().map(|post| post.file_name().unwrap_or_default()).collect()
}

fn messages_of(opened: &Opened) -> Vec<&str> {
    opened.collection.warnings.iter().map(|warning| warning.message.as_str()).collect()
}

fn description(caption: &str, path: &Path) -> DescriptionEntry {
    DescriptionEntry {
        caption: caption.to_owned(),
        path: path.to_path_buf(),
        post_id: None,
        file_bytes: None,
        file_modified: None,
        edited_at: 1_793_545_445_000,
    }
}

/// A folder of its own below the system's temporary folder, removed when dropped.
struct TempFolder(PathBuf);

impl TempFolder {
    fn new(label: &str) -> TempFolder {
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let folder = std::env::temp_dir().join(format!("instagram-core-reader-{label}-{}-{unique}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).expect("a temporary folder can be created");
        TempFolder(folder)
    }

    fn path(&self) -> &Path {
        &self.0
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

/// The first bytes of a JPEG file: all that opening a picture ever looks at.
const JPEG: &[u8] = &[0xFF, 0xD8, 0xFF, 0xE0, 0, 16, b'J', b'F', b'I', b'F', 0];
const MP4: &[u8] = b"\x00\x00\x00\x14ftypmp42\x20\x20\x20\x20mp42";
/// The start of a Matroska (MKV) file: a video format the app does not play.
const MATROSKA: &[u8] = &[0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42, 0x86, 0x81, 0x01];

const LOOSE_NAMES: [&str; 5] =
    ["img_1.png", "IMG_2.jpg", "IMG_10.jpg", "portrait.heic", "Sunset at the pier (final).webp"];
const BROKEN_WARNING: &str = "\"broken.jpg\" is an empty file.";

// --- export-basic -------------------------------------------------------------------------------

fn assert_basic_export(opened: &Opened, root: &Path) {
    let collection = &opened.collection;
    assert_eq!(opened.focus, None);
    assert!(collection.is_export());
    let [source] = collection.sources() else { panic!("one export source") };
    assert_eq!((source.kind, source.path.as_path(), source.manifest.clone()), (ExportKind::Folder, root, root.join("post.json")));
    assert_eq!((source.shown, source.replaced, source.total_entries), (6, 0, 6));
    assert_eq!(collection.folder.as_deref(), Some(root));
    assert_eq!(collection.total_entries, 6);
    assert_eq!(collection.source_url.as_deref(), Some("https://www.instagram.com/fixture.sunrise.bakery/"));
    assert_eq!(collection.warnings, vec![]);
    assert_eq!(
        ids(&collection.posts),
        ["FXTbasic001", "FXTbasic002", "FXTbasic003", "FXTbasic004", "FXTbasic005", "FXTbasic006"]
    );

    let posts = &collection.posts;
    for post in posts {
        assert_eq!(post.key, PostKey::for_export_post(&root.join("post.json"), &post.id));
        assert_eq!(post.permalink.as_deref().map(|link| link.contains(&post.id)), Some(true));
        assert_eq!(post.caption, post.original_caption);
        assert!(!post.is_edited());
        assert_eq!(post.context, None);
        for media in &post.media {
            assert!(media.path.is_file(), "{}", media.path.display());
            assert_eq!(media.bytes, fs::metadata(&media.path).unwrap().len());
            assert_eq!(media.path, root.join(&media.manifest_path));
        }
    }

    // Post 1: every optional field.
    let one = &posts[0];
    assert_eq!(one.permalink.as_deref(), Some("https://www.instagram.com/p/FXTbasic001/"));
    assert_eq!(one.user.username, "fixture.sunrise.bakery");
    assert_eq!(one.user.full_name.as_deref(), Some("Sunrise Bakery (test fixture)"));
    assert_eq!(one.user.verified, Some(true));
    assert_eq!(one.user.avatar, Some(root.join("avatars").join("fixture.sunrise.bakery.jpg")));
    assert_eq!(one.user.profile_url.as_deref(), Some("https://www.instagram.com/fixture.sunrise.bakery/"));
    assert!(one.caption.starts_with("Fresh out of the oven 🥐☀️\nFirst batch of the season"));
    assert!(one.caption.ends_with("#sourdough #fixturedata #朝ごはん"));
    assert_eq!(one.comment_count, 1234);
    assert_eq!(one.like_count, Some(56789));
    assert_eq!(one.taken_at, Some(1_793_545_445_000));
    assert_eq!(one.taken_at.and_then(text::iso_time).as_deref(), Some("2026-11-01T15:04:05Z"));
    assert_eq!(one.media_type, MediaType::Image);
    assert_eq!(one.alt.as_deref(), Some("Orange test card labelled POST 1"));
    assert_eq!(one.alt_text(0), "Orange test card labelled POST 1");
    assert_eq!(one.location.as_deref(), Some("Fixtureville, Testland"));
    assert_eq!(one.header_subline(), Some("Fixtureville, Testland"));
    assert_eq!(one.file_name().as_deref(), Some("post1.jpg"));
    assert_eq!(one.comments.len(), 2);
    assert_eq!(one.comments[0].id, "FXTbasic001-c1");
    assert_eq!(one.comments[0].username, "fixture_tidewater");
    assert_eq!(one.comments[0].text, "Saving me two of those 🙌");
    assert_eq!(one.comments[0].likes, 3);
    assert_eq!(one.comments[0].taken_at.and_then(text::iso_time).as_deref(), Some("2026-11-01T16:00:00Z"));
    assert_eq!(one.comments[1].id, "FXTbasic001-c2");
    assert_eq!(one.comments[1].likes, 0);

    // Post 2: hidden likes, a caption in three scripts, verified = false is kept.
    let two = &posts[1];
    assert_eq!(two.like_count, None);
    assert_eq!(two.comment_count, 42);
    assert_eq!(two.user.verified, Some(false));
    assert_eq!(two.user.avatar, Some(root.join("avatars").join("fixture_tidewater.jpg")));
    assert_eq!(two.caption, "潮が引いたあとの静かな海 🌊\nВечерний отлив и тишина.\nمساء هادئ على الشاطئ\n#tidepool #lowtide");
    assert_eq!(two.location, None);
    assert!(two.comments.is_empty());

    // Post 3 shares the avatar file of post 1.
    assert_eq!(posts[2].user.avatar, one.user.avatar);
    assert_eq!(posts[2].comment_count, 0);
    assert_eq!(posts[2].media[0].path, root.join("posts").join("post3.webp"));

    // Post 4: a real HEIC file is a picture like any other; its author has no avatar.
    let four = &posts[3];
    assert_eq!(four.media.len(), 1);
    assert_eq!(four.media[0].kind, MediaKind::Image);
    assert_eq!(four.media[0].manifest_path, "posts/post4.heic");
    assert_eq!(four.media_type, MediaType::Image);
    assert_eq!(four.user.username, "pixel.orchard.demo");
    assert_eq!(four.user.avatar, None);
    assert_eq!(four.user.verified, None);

    // Post 5: a carousel written with "image" + "images".
    let five = &posts[4];
    assert!(five.is_carousel());
    assert_eq!(five.media_type, MediaType::Carousel);
    let slides: Vec<&str> = five.media.iter().map(|media| media.manifest_path.as_str()).collect();
    assert_eq!(slides, ["posts/post5_1.jpg", "posts/post5_2.jpg"]);
    assert!(five.media.iter().all(|media| media.kind == MediaKind::Image && media.poster.is_none()));
    assert_eq!(five.alt_text(1), "Photo 2 of 2 by fixture_tidewater");

    // Post 6: declared a video, but the export keeps the cover picture only.
    let six = &posts[5];
    assert_eq!(six.permalink.as_deref(), Some("https://www.instagram.com/reel/FXTbasic006/"));
    assert_eq!(six.media_type, MediaType::Video);
    assert!(six.is_video());
    assert_eq!(six.media[0].kind, MediaKind::Image);
    assert_eq!(six.media[0].display_path(), Some(root.join("posts").join("post6.jpg").as_path()));
}

#[test]
fn export_basic_opens_from_its_folder() {
    let root = fixture("export-basic");
    let opened = open(std::slice::from_ref(&root));
    assert_eq!(opened.collection.label, "export-basic");
    assert_basic_export(&opened, &root);
    assert_eq!(
        messages::showing_banner(opened.collection.posts.len(), opened.collection.total_entries, true, &opened.collection.label),
        "Showing 6 posts from export-basic"
    );
}

#[test]
fn export_basic_opens_from_its_post_json() {
    let root = fixture("export-basic");
    let manifest = root.join("post.json");
    assert_eq!(catalog::classify_request(std::slice::from_ref(&manifest)), one_folder(manifest.clone()));
    let opened = open(&[manifest]);
    assert_eq!(opened.collection.label, "export-basic");
    assert_basic_export(&opened, &root);
}

#[test]
fn the_reader_agrees_with_the_catalog_on_export_basic() {
    let root = fixture("export-basic");
    let manifest_path = manifest::find_manifest(&root, MANIFEST_SEARCH_DEPTH).unwrap().expect("the fixture has a post.json");
    assert_eq!(manifest_path, root.join("post.json"));

    let parsed = manifest::read_manifest(&fs::read(&manifest_path).unwrap()).unwrap();
    assert_eq!(parsed.total_entries, 6);
    assert_eq!(parsed.warnings, vec![]);
    assert_eq!(parsed.exported_at.and_then(text::iso_time).as_deref(), Some("2026-11-10T09:30:00Z"));
    assert_eq!(parsed.posts[4].media.len(), 2);
    assert_eq!(parsed.posts[5].media_type, MediaType::Video);

    let index = manifest::FileIndex::scan(&root).unwrap();
    let listed: Vec<&str> = index.files().iter().map(|file| file.relative.as_str()).collect();
    assert_eq!(
        listed,
        [
            "post.json",
            "avatars/fixture.sunrise.bakery.jpg",
            "avatars/fixture_tidewater.jpg",
            "posts/post1.jpg",
            "posts/post2.png",
            "posts/post3.webp",
            "posts/post4.heic",
            "posts/post5_1.jpg",
            "posts/post5_2.jpg",
            "posts/post6.jpg",
        ]
    );
    assert_eq!(index.resolve("POSTS/Post4.HEIC").map(|file| file.relative.as_str()), Some("posts/post4.heic"));
    assert_eq!(index.resolve("post.json"), None);

    let assembled = manifest::assemble_export(parsed, &index, &manifest_path).unwrap();
    let loaded = manifest::load_export(&manifest_path).unwrap();
    assert_eq!(assembled, loaded);
    assert_eq!(loaded.posts, open(&[root]).collection.posts);
}

// --- export-lenient -----------------------------------------------------------------------------

#[test]
fn export_lenient_is_read_through_its_aliases() {
    let root = fixture("export-lenient");
    let opened = open(std::slice::from_ref(&root));
    let collection = &opened.collection;
    // The manifest is called Post.JSON here.
    let [source] = collection.sources() else {
        panic!("the folder is an export");
    };
    let (manifest, export_root) = (&source.manifest, &source.path);
    assert_eq!(manifest.file_name().map(|name| name.to_string_lossy().to_lowercase()).as_deref(), Some("post.json"));
    assert_eq!(export_root, &root);
    assert_eq!(collection.label, "export-lenient");
    assert_eq!(collection.total_entries, 4);
    assert_eq!(collection.source_url, None);
    assert_eq!(collection.warnings, vec![]);
    assert_eq!(collection.posts.len(), 4);
    let posts = &collection.posts;

    // 1: shortcode, permalink, image_path with backslashes and the wrong letter case, from,
    // description, counts as text, timestamp in epoch seconds, type.
    let one = &posts[0];
    assert_eq!(one.id, "FXTlen001");
    assert_eq!(one.permalink.as_deref(), Some("https://www.instagram.com/p/FXTlen001/"));
    assert_eq!(one.media[0].manifest_path, "Posts/LENIENT1.JPG");
    assert_eq!(one.media[0].path, root.join("posts").join("lenient1.jpg"));
    assert_eq!(one.user.username, "lenient.lighthouse.demo");
    assert_eq!(one.user.profile_url.as_deref(), Some("https://www.instagram.com/lenient.lighthouse.demo/"));
    assert_eq!(one.user.avatar, None);
    assert_eq!(one.caption, "Alias field names all the way down 🗼\n#lenient");
    assert_eq!(one.comment_count, 1234);
    assert_eq!(one.like_count, Some(12_300));
    assert_eq!(one.taken_at, Some(1_793_361_600_000));
    assert_eq!(one.media_type, MediaType::Image);

    // 2: code, an http link, a path with ".\", owner as an object, first_comment, counts with
    // magnitudes, a date with an offset. The file says .png and holds WebP: it is a picture.
    let two = &posts[1];
    assert_eq!(two.id, "FXTlen002");
    assert_eq!(two.permalink.as_deref(), Some("https://instagram.com/p/FXTlen002/"));
    assert_eq!(two.media[0].manifest_path, "posts/lenient2.png");
    assert_eq!(two.media[0].kind, MediaKind::Image);
    assert_eq!(two.user.username, "lenient.lighthouse.demo");
    assert_eq!(two.user.full_name.as_deref(), Some("Lenient Lighthouse"));
    assert_eq!(two.user.verified, Some(true));
    assert_eq!(two.user.avatar, Some(root.join("avatars").join("lighthouse.png")));
    assert_eq!(two.user.profile_url.as_deref(), Some("https://www.instagram.com/lenient.lighthouse.demo/"));
    assert_eq!(two.caption, "The extension says PNG, the bytes say WebP.");
    assert_eq!(two.comment_count, 2500);
    assert_eq!(two.like_count, Some(1_200_000));
    assert_eq!(two.taken_at.and_then(text::iso_time).as_deref(), Some("2026-10-29T08:00:00Z"));

    // 3: id, post_url, media_paths with a leading slash, author with handle and display_name,
    // text, a comment count that is no number, a negative like count, epoch milliseconds.
    let three = &posts[2];
    assert_eq!(three.id, "FXTlen003");
    assert_eq!(three.permalink.as_deref(), Some("https://www.instagram.com/p/FXTlen003/"));
    let slides: Vec<&str> = three.media.iter().map(|media| media.manifest_path.as_str()).collect();
    assert_eq!(slides, ["posts/lenient3_1.jpeg", "posts/lenient3_2.jpeg"]);
    assert_eq!(three.media_type, MediaType::Carousel);
    assert_eq!(three.user.username, "lenient_gull_demo");
    assert_eq!(three.user.full_name.as_deref(), Some("Lenient Gull"));
    assert_eq!(three.caption, "Two slides listed under media_paths.");
    assert_eq!(three.comment_count, 0);
    assert_eq!(three.like_count, None);
    assert_eq!(three.taken_at, Some(1_793_275_200_000));

    // 4: file, username, an empty caption, a like count as text; no id and no link, so the id
    // is derived from the author and the file, and no link is made up.
    let four = &posts[3];
    assert_eq!(four.id, text::derived_post_id("lenient_gull_demo", None, &["posts/lenient4.jpg".to_owned()]));
    assert!(four.id.starts_with("post-"));
    assert_eq!(four.permalink, None);
    assert_eq!(four.media[0].kind, MediaKind::Image);
    assert_eq!(four.caption, "");
    assert_eq!(four.comment_count, 5);
    assert_eq!(four.like_count, Some(987));
    assert_eq!(four.taken_at, None);
    assert_eq!(four.user.profile_url.as_deref(), Some("https://www.instagram.com/lenient_gull_demo/"));
}

// --- export-problems ----------------------------------------------------------------------------

#[test]
fn export_problems_keeps_the_usable_posts_and_names_every_problem() {
    let root = fixture("export-problems");
    let opened = open(std::slice::from_ref(&root));
    let collection = &opened.collection;
    assert_eq!(collection.total_entries, 7);
    assert_eq!(ids(&collection.posts), ["FXTprob001", "FXTprob003", "FXTprob004", "FXTprob001-2"]);
    assert_eq!(
        messages::showing_banner(collection.posts.len(), collection.total_entries, true, &collection.label),
        "Showing 4 of 7 posts from export-problems"
    );

    let warnings: Vec<(WarningCode, Option<usize>, &str)> = collection
        .warnings
        .iter()
        .map(|warning| (warning.code, warning.post_index, warning.message.as_str()))
        .collect();
    assert_eq!(
        warnings,
        [
            (
                WarningCode::MissingImage,
                Some(1),
                "Post 2: the image \"posts/does-not-exist.jpg\" is not in the export."
            ),
            (WarningCode::UnsafeUrl, Some(2), "The link of post 3 is not a valid http(s) link and was ignored."),
            (
                WarningCode::DuplicateId,
                Some(4),
                "Post 5 reuses the id \"FXTprob001\"; it was imported as \"FXTprob001-2\"."
            ),
            (
                WarningCode::UnsupportedImage,
                Some(5),
                "Post 6: \"posts/problem6.svg\" is not a supported image (JPEG, PNG, GIF, WebP, AVIF, BMP or HEIC/HEIF) or video (MP4, MOV or WebM)."
            ),
            (
                WarningCode::MissingImage,
                Some(6),
                "Post 7: \"https://cdn.fixture.invalid/remote/problem7.jpg\" is a URL. Images are not downloaded; the file must be inside the export."
            ),
        ]
    );
    assert_eq!(messages::warnings_label(collection.warnings.len()), "5 warnings");

    // The valid post.
    let valid = &collection.posts[0];
    assert_eq!(valid.user.username, "problem.pelican.demo");
    assert_eq!(valid.permalink.as_deref(), Some("https://www.instagram.com/p/FXTprob001/"));
    assert_eq!(valid.caption, "The one valid post of this export ✅");
    assert_eq!(valid.media[0].path, root.join("posts").join("problem1.jpg"));

    // A javascript: link never becomes a permalink; the profile link is the guessed one.
    let scripted = &collection.posts[1];
    assert_eq!(scripted.permalink, None);
    assert_eq!(scripted.user.profile_url.as_deref(), Some("https://www.instagram.com/problem.pelican.demo/"));

    // The look-alike host is an ordinary other site (the clone's current rule, spec-export F.3):
    // the links are kept as that site's, nothing of Instagram's is derived from them, and
    // `export` leaves them out of a download because the validator would reject them.
    let lookalike = &collection.posts[2];
    assert_eq!(lookalike.id, "FXTprob004");
    assert_eq!(lookalike.permalink.as_deref(), Some("https://www.instagram.com.evil.example/p/FXTprob004/"));
    assert_eq!(
        lookalike.user.profile_url.as_deref(),
        Some("https://instagram.com.evil.example/problem.pelican.demo/")
    );

    // The post that reuses an id keeps its own key.
    let reused = &collection.posts[3];
    assert_eq!(reused.caption, "I reuse the id of the first post.");
    assert_ne!(reused.key, valid.key);
}

// --- export-wrapped -----------------------------------------------------------------------------

#[test]
fn export_wrapped_is_found_one_folder_down_and_the_decoy_is_ignored() {
    let outer = fixture("export-wrapped");
    let root = outer.join("my-instagram-export");
    assert_eq!(manifest::find_manifest(&outer, 0), Ok(None));
    assert_eq!(manifest::find_manifest(&outer, MANIFEST_SEARCH_DEPTH), Ok(Some(root.join("post.json"))));
    assert_eq!(
        catalog::classify_request(std::slice::from_ref(&outer)),
        one_folder(root.join("post.json"))
    );

    let opened = open(&[outer]);
    assert_eq!(opened.collection.label, "my-instagram-export");
    assert_basic_export(&opened, &root);

    // The decoy inside __MACOSX is a manifest only for someone who points straight at it.
    let decoy = fixture("export-wrapped").join("__MACOSX").join("my-instagram-export").join("post.json");
    let error = catalog::open_paths(&OpenRequest { paths: vec![decoy], options: options() }, &empty_store(), &context()).unwrap_err();
    assert_eq!(error.code, OpenErrorCode::Export);
    assert_eq!(error.message, messages::MANIFEST_NO_POSTS_LIST);
    // .DS_Store is noise, not a file of the export.
    let index = manifest::FileIndex::scan(&root).unwrap();
    assert_eq!(index.files().len(), 10);
    assert!(index.files().iter().all(|file| !file.relative.contains("DS_Store")));
}

// --- loose pictures -----------------------------------------------------------------------------

fn assert_loose_collection(opened: &Opened) {
    let folder = fixture("loose");
    let collection = &opened.collection;
    assert!(!collection.is_export());
    assert_eq!(collection.source, CollectionSource::Loose);
    assert_eq!(collection.label, "loose");
    assert_eq!(collection.folder.as_deref(), Some(folder.as_path()));
    assert_eq!(collection.source_url, None);
    assert_eq!(collection.total_entries, 5);
    assert_eq!(file_names(&collection.posts), LOOSE_NAMES);
    assert_eq!(ids(&collection.posts), ["img_1", "IMG_2", "IMG_10", "portrait", "Sunset_at_the_pier_final_"]);

    for (post, name) in collection.posts.iter().zip(LOOSE_NAMES) {
        let path = folder.join(name);
        assert_eq!(post.key, PostKey::for_file(&path));
        assert_eq!(post.media.len(), 1);
        assert_eq!(post.media[0].kind, MediaKind::Image);
        assert_eq!(post.media[0].path, path);
        assert_eq!(post.media[0].manifest_path, name);
        assert_eq!(post.media[0].bytes, fs::metadata(&path).unwrap().len());
        assert_eq!(post.media[0].poster, None);
        assert_eq!(post.media_type, MediaType::Image);
        assert!(!post.is_carousel());
        assert_eq!(post.user.username, "tester");
        assert_eq!(post.user.profile_url, None);
        assert_eq!(post.user.avatar, None);
        assert_eq!(post.user.full_name, None);
        assert_eq!(post.user.verified, None);
        assert_eq!(post.permalink, None);
        assert_eq!(post.caption, "");
        assert_eq!(post.original_caption, "");
        assert_eq!(post.comment_count, 0);
        assert_eq!(post.like_count, None);
        assert!(post.comments.is_empty());
        assert_eq!(post.alt, None);
        assert_eq!(post.location, None);
        assert_eq!(post.context.as_deref(), Some(name));
        assert_eq!(post.header_subline(), Some(name));
        // The time is the file's modified time.
        let modified = text::epoch_ms_from_system_time(fs::metadata(&path).unwrap().modified().unwrap());
        assert_eq!(post.taken_at, Some(modified));
    }
}

#[test]
fn a_folder_of_pictures_opens_in_natural_order() {
    let opened = open(&[fixture("loose")]);
    assert_loose_collection(&opened);
    assert_eq!(opened.focus, None);
    // notes.txt is no picture and does not claim to be one: silent. broken.jpg claims to be.
    assert_eq!(messages_of(&opened), [BROKEN_WARNING]);
    assert_eq!(opened.collection.warnings[0].code, WarningCode::UnsupportedImage);
    assert_eq!(opened.collection.warnings[0].post_index, None);
    assert_eq!(
        messages::showing_banner(opened.collection.posts.len(), opened.collection.total_entries, false, &opened.collection.label),
        "Showing 5 pictures from loose"
    );
    assert_eq!(catalog::classify_request(&[fixture("loose")]), RequestKind::Loose);
}

#[test]
fn one_picture_opens_its_folder_and_focuses_the_picture() {
    for (name, position) in [("IMG_2.jpg", 1), ("img_1.png", 0), ("Sunset at the pier (final).webp", 4), ("portrait.heic", 3)] {
        let opened = open(&[fixture("loose").join(name)]);
        assert_loose_collection(&opened);
        assert_eq!(opened.focus, Some(position), "{name}");
        assert_eq!(messages_of(&opened), [BROKEN_WARNING], "{name}");
    }
    // Letter case and slash direction of the picked path do not matter on Windows.
    #[cfg(windows)]
    {
        let shouted = PathBuf::from(fixture("loose").join("img_10.JPG").to_string_lossy().replace('\\', "/"));
        assert_eq!(open(&[shouted]).focus, Some(2));
    }
}

#[test]
fn one_file_that_is_no_picture_still_opens_its_folder() {
    // A named file is always answered for; the viewer then shows the feed.
    let notes = open(&[fixture("loose").join("notes.txt")]);
    assert_loose_collection(&notes);
    assert_eq!(notes.focus, None);
    assert_eq!(
        messages_of(&notes),
        [
            BROKEN_WARNING,
            "\"notes.txt\" is not a supported image (JPEG, PNG, GIF, WebP, AVIF, BMP or HEIC/HEIF) or video (MP4, MOV or WebM)."
        ]
    );

    let broken = open(&[fixture("loose").join("broken.jpg")]);
    assert_loose_collection(&broken);
    assert_eq!(broken.focus, None);
    assert_eq!(messages_of(&broken), [BROKEN_WARNING]);
}

#[test]
fn several_files_open_exactly_those_files() {
    let folder = fixture("loose");
    let picked = [folder.join("IMG_10.jpg"), folder.join("img_1.png"), folder.join("IMG_10.jpg"), folder.join("notes.txt")];
    let opened = open(&picked);
    assert_eq!(file_names(&opened.collection.posts), ["img_1.png", "IMG_10.jpg"]);
    assert_eq!(opened.focus, None);
    assert_eq!(opened.collection.label, "loose");
    assert_eq!(opened.collection.total_entries, 2);
    assert_eq!(
        messages_of(&opened),
        ["\"notes.txt\" is not a supported image (JPEG, PNG, GIF, WebP, AVIF, BMP or HEIC/HEIF) or video (MP4, MOV or WebM)."]
    );

    // A folder and a file of it: the file is not shown twice, and a named file is answered for
    // even though the folder would have passed over it in silence.
    let mixed = open(&[folder.clone(), folder.join("notes.txt"), folder.join("IMG_2.jpg")]);
    assert_eq!(file_names(&mixed.collection.posts), LOOSE_NAMES);
    assert_eq!(mixed.focus, None);
    assert_eq!(messages_of(&mixed).len(), 2);
}

#[test]
fn pictures_from_several_folders_share_one_collection() {
    let temp = TempFolder::new("several");
    let first = temp.write("first/IMG_2.jpg", JPEG);
    let second = temp.write("second/IMG_2.jpg", JPEG);
    let third = temp.write("second/IMG_1.jpg", JPEG);
    let clip = temp.write("second/clip.mp4", MP4);

    let opened = open(&[temp.path().join("second"), first.clone()]);
    let files: Vec<&Path> = opened.collection.posts.iter().map(|post| post.media[0].path.as_path()).collect();
    // Pictures and videos together, in file-name order.
    assert_eq!(files, [clip.as_path(), third.as_path(), first.as_path(), second.as_path()]);
    assert_eq!(opened.collection.label, "2 folders");
    assert_eq!(opened.collection.folder.as_deref(), Some(temp.path().join("second").as_path()));
    // The same stem twice: ids are made unique in display order.
    assert_eq!(ids(&opened.collection.posts), ["clip", "IMG_1", "IMG_2", "IMG_2-2"]);
    assert_eq!(opened.collection.warnings, vec![]);
    assert_eq!(messages::collection_banner(&opened.collection), "Showing 4 pictures and videos from 2 folders");
    // A video named among pictures is a post like them.
    let named = open(&[clip, first]);
    assert_eq!(messages_of(&named), Vec::<String>::new());
    assert_eq!(named.collection.posts.len(), 2);
    assert_eq!(named.collection.posts[0].media[0].kind, MediaKind::Video);
}

#[test]
fn nothing_to_show_is_an_error_with_the_reasons() {
    let store = empty_store();
    let request = |paths: Vec<PathBuf>| OpenRequest { paths, options: options() };

    let nothing = catalog::open_paths(&request(vec![]), &store, &context()).unwrap_err();
    assert_eq!(nothing.code, OpenErrorCode::NothingToOpen);
    assert_eq!(nothing.message, messages::NOTHING_TO_OPEN);

    let temp = TempFolder::new("empty");
    let notes = temp.write("docs/notes.txt", b"just some notes");
    temp.write("docs/film.mkv", MATROSKA);
    let fake = temp.write("docs/fake.jpg", b"not a picture");

    // A folder without pictures: the reasons are those of the files that claim to be pictures.
    let folder = catalog::open_paths(&request(vec![temp.path().join("docs")]), &store, &context()).unwrap_err();
    assert_eq!(folder.code, OpenErrorCode::NoPictures);
    assert_eq!(folder.message, "No pictures were found in \"docs\".");
    assert_eq!(folder.to_string(), folder.message);
    assert_eq!(folder.warnings.len(), 1);
    let fake_path = temp.path().join("docs").join("fake.jpg").display().to_string();
    assert_eq!(folder.warnings[0].path.as_deref(), Some(fake_path.as_str()));

    // One file that is no picture, in a folder without pictures: the message says why that
    // file is not shown, not merely that no pictures were found.
    let file = catalog::open_paths(&request(vec![notes.clone()]), &store, &context()).unwrap_err();
    assert_eq!(file.code, OpenErrorCode::NoPictures);
    assert_eq!(
        file.message,
        "\"notes.txt\" is not a supported image (JPEG, PNG, GIF, WebP, AVIF, BMP or HEIC/HEIF) or video (MP4, MOV or WebM)."
    );
    assert_eq!(file.warnings.len(), 2);

    // Several files, none a picture.
    let several = catalog::open_paths(&request(vec![notes, fake]), &store, &context()).unwrap_err();
    assert_eq!(several.message, "No pictures were found in \"docs\".");
    assert_eq!(several.warnings.len(), 2);

    // A path that does not exist.
    let missing = temp.path().join("gone");
    let gone = catalog::open_paths(&request(vec![missing.clone()]), &store, &context()).unwrap_err();
    assert_eq!(gone.code, OpenErrorCode::ReadFailed);
    assert!(gone.message.starts_with(&format!("\"{}\" could not be opened: ", missing.display())), "{}", gone.message);
    assert_eq!(gone.warnings.len(), 1);

    // ... next to something that opens is a warning.
    let picture = temp.write("pictures/a.jpg", JPEG);
    let partly = open(&[missing, picture]);
    assert_eq!(partly.collection.posts.len(), 1);
    assert_eq!(partly.collection.warnings.len(), 1);
    assert_eq!(partly.collection.warnings[0].code, WarningCode::MissingImage);
    assert!(partly.collection.warnings[0].message.contains("could not be opened: "));

    // An export that cannot be read keeps the reader's sentence.
    let broken = temp.write("broken-export/post.json", b"{ \"posts\": [ }");
    let export = catalog::open_paths(&request(vec![broken]), &store, &context()).unwrap_err();
    assert_eq!(export.code, OpenErrorCode::Export);
    assert!(export.message.starts_with("post.json is not valid JSON ("), "{}", export.message);
    let unusable = temp.write("no-media/post.json", b"[{\"user\": \"a\", \"image\": \"posts/gone.jpg\"}]");
    let export = catalog::open_paths(&request(vec![temp.path().join("no-media")]), &store, &context()).unwrap_err();
    assert_eq!(
        export.message,
        "The only post in post.json could not be imported. First problem: Post 1: the image \"posts/gone.jpg\" is not in the export."
    );
    assert_eq!(manifest::load_export(&unusable).unwrap_err().code, ManifestErrorCode::NoPosts);
}

// --- exports beside other things ----------------------------------------------------------------

#[test]
fn exports_are_shown_on_their_own_and_merged_with_each_other() {
    let loose = fixture("loose");
    let export = fixture("export-basic");
    let lenient = fixture("export-lenient");
    let paths = [loose.clone(), loose.join("IMG_2.jpg"), export.clone(), export.join("post.json"), lenient.clone()];
    let RequestKind::Exports { sources, others } = catalog::classify_request(&paths) else {
        panic!("the request names exports");
    };
    assert_eq!(others, [0, 1], "the pictures are not opened beside the exports");
    assert_eq!(sources.len(), 2, "export-basic named twice counts once");
    assert_eq!(sources[0], SourceRequest::Folder { manifest: export.join("post.json") });

    let opened = open(&paths);
    assert_eq!(opened.collection.label, "2 folders");
    assert_eq!(opened.collection.sources().iter().map(|source| source.name.as_str()).collect::<Vec<_>>(), ["export-basic", "export-lenient"]);
    assert_eq!(opened.collection.posts.len(), 10, "6 posts of export-basic and 4 of export-lenient");
    assert_eq!(opened.collection.total_entries, 10);
    assert_eq!(opened.collection.folder.as_deref(), Some(export.as_path()));
    assert_eq!(
        messages_of(&opened),
        [
            "\"loose\" was not opened: an export is shown on its own.",
            "\"IMG_2.jpg\" was not opened: an export is shown on its own.",
        ]
    );
    // Merged: newest first, posts without a time after them.
    let times: Vec<Option<i64>> = opened.collection.posts.iter().map(|post| post.taken_at).collect();
    let dated: Vec<i64> = times.iter().flatten().copied().collect();
    assert!(dated.windows(2).all(|pair| pair[0] >= pair[1]), "{times:?}");
    assert!(times.iter().skip_while(|time| time.is_some()).all(Option::is_none), "{times:?}");

    // An export one folder down is found (post.json of the first folder by name wins); exports
    // two levels down are not looked for, so that folder is a folder of pictures.
    assert_eq!(
        catalog::classify_request(&[fixtures()]),
        one_folder(fixtures().join("export-basic").join("post.json"))
    );
    let tests_folder = fixtures().parent().unwrap().to_path_buf();
    assert_eq!(catalog::classify_request(&[tests_folder]), RequestKind::Loose);
    // A picture inside an export opens as a picture with its siblings.
    let inside = open(&[export.join("posts").join("post2.png")]);
    assert!(!inside.collection.is_export());
    assert_eq!(inside.collection.posts.len(), 7);
    assert_eq!(inside.focus, Some(1));
    assert_eq!(inside.collection.posts[1].user.username, "tester");
    // Missing paths and an empty request are loose; `open_paths` reports them.
    assert_eq!(catalog::classify_request(&[loose.join("nowhere.jpg")]), RequestKind::Loose);
    assert_eq!(catalog::classify_request(&[]), RequestKind::Loose);
}

// --- descriptions -------------------------------------------------------------------------------

#[test]
fn stored_descriptions_are_laid_over_the_posts() {
    let loose = fixture("loose");
    let export = fixture("export-basic");
    let manifest_path = export.join("post.json");
    let mut store = empty_store();
    let picture_key = PostKey::for_file(&loose.join("IMG_2.jpg"));
    let post_key = PostKey::for_export_post(&manifest_path, "FXTbasic002");
    store.set(&picture_key, description("Second of the roll.\nShot at noon.", &loose.join("IMG_2.jpg")));
    store.set(&post_key, description("Low tide, rewritten.", &manifest_path));
    // An entry for a file that is not opened changes nothing.
    store.set(&PostKey::for_file(&loose.join("elsewhere.jpg")), description("unused", &loose.join("elsewhere.jpg")));

    let pictures = open_with(std::slice::from_ref(&loose), &store);
    let edited = pictures.collection.post(&picture_key).expect("IMG_2.jpg is shown");
    assert_eq!(edited.caption, "Second of the roll.\nShot at noon.");
    assert_eq!(edited.original_caption, "");
    assert!(edited.is_edited());
    assert_eq!(pictures.collection.posts.iter().filter(|post| post.is_edited()).count(), 1);

    let posts = open_with(&[export], &store);
    let rewritten = posts.collection.post(&post_key).expect("FXTbasic002 is shown");
    assert_eq!(rewritten.caption, "Low tide, rewritten.");
    assert!(rewritten.original_caption.starts_with("潮が引いたあとの静かな海"));
    assert!(rewritten.is_edited());
    assert_eq!(posts.collection.posts.iter().filter(|post| post.is_edited()).count(), 1);

    // The same through the public function, on a collection opened without the store.
    let mut plain = open(&[loose]).collection;
    assert!(plain.posts.iter().all(|post| !post.is_edited()));
    catalog::apply_descriptions(&mut plain, &store);
    assert_eq!(plain, pictures.collection);
    // The store is keyed without regard to letter case on Windows.
    #[cfg(windows)]
    {
        let shouted = PathBuf::from(fixture("loose").join("IMG_2.jpg").to_string_lossy().to_uppercase());
        assert!(store.get(&PostKey::for_file(&shouted)).is_some());
    }
}

// --- adding to a loose collection ---------------------------------------------------------------

#[test]
fn further_pictures_are_added_without_duplicates() {
    let folder = fixture("loose");
    let mut store = empty_store();
    store.set(&PostKey::for_file(&folder.join("img_1.png")), description("The first one.", &folder.join("img_1.png")));

    let mut collection = open_with(&[folder.join("IMG_2.jpg"), folder.join("IMG_10.jpg")], &store).collection;
    assert_eq!(file_names(&collection.posts), ["IMG_2.jpg", "IMG_10.jpg"]);

    // One new file, one that is already shown.
    let outcome =
        catalog::add_loose_files(&mut collection, &[folder.join("img_1.png"), folder.join("IMG_2.jpg")], &options(), &store)
            .unwrap();
    assert_eq!(outcome, AddOutcome { added: 1, focus: Some(0), one_file: false });
    assert_eq!(file_names(&collection.posts), ["img_1.png", "IMG_2.jpg", "IMG_10.jpg"]);
    assert_eq!(collection.posts[0].caption, "The first one.");
    assert_eq!(collection.total_entries, 3);
    assert_eq!(collection.label, "loose");
    assert_eq!(collection.warnings, vec![]);

    // A file that is already shown: nothing is added, the viewer goes to it.
    let again = catalog::add_loose_files(&mut collection, &[folder.join("IMG_10.jpg")], &options(), &store).unwrap();
    assert_eq!(again, AddOutcome { added: 0, focus: Some(2), one_file: true });
    assert_eq!(collection.posts.len(), 3);

    // The whole folder: the rest is added, and what cannot be shown is reported once.
    let rest = catalog::add_loose_files(&mut collection, std::slice::from_ref(&folder), &options(), &store).unwrap();
    assert_eq!(rest, AddOutcome { added: 2, focus: Some(3), one_file: false });
    assert_eq!(file_names(&collection.posts), LOOSE_NAMES);
    assert_eq!(collection.total_entries, 5);
    let messages: Vec<&str> = collection.warnings.iter().map(|warning| warning.message.as_str()).collect();
    assert_eq!(messages, [BROKEN_WARNING]);
    assert_eq!(collection.posts, open_with(std::slice::from_ref(&folder), &store).collection.posts);

    // Nothing new and several names: no focus. A path that is not there: a warning, no error.
    let nothing = catalog::add_loose_files(
        &mut collection,
        &[folder.join("IMG_2.jpg"), folder.join("img_1.png"), folder.join("nowhere.jpg")],
        &options(),
        &store,
    )
    .unwrap();
    assert_eq!(nothing, AddOutcome { added: 0, focus: None, one_file: false });
    assert_eq!(collection.warnings.len(), 2);
    assert!(collection.warnings[1].message.contains("nowhere.jpg\" could not be opened: "));
}

#[test]
fn added_pictures_get_ids_that_are_unique_among_all_posts() {
    let temp = TempFolder::new("add");
    let first = temp.write("a/photo.jpg", JPEG);
    let second = temp.write("b/photo.jpg", JPEG);
    let third = temp.write("c/photo-2.jpg", JPEG);

    let mut collection = open(std::slice::from_ref(&second)).collection;
    assert_eq!(ids(&collection.posts), ["photo"]);
    assert_eq!(collection.label, "b");

    let outcome = catalog::add_loose_files(&mut collection, &[first.clone(), third.clone()], &options(), &empty_store()).unwrap();
    assert_eq!(outcome.added, 2);
    // Display order: "photo-2.jpg" before "photo.jpg" ("-" sorts before "."), a before b.
    let pictures: Vec<&Path> = collection.posts.iter().map(|post| post.media[0].path.as_path()).collect();
    assert_eq!(pictures, [third.as_path(), first.as_path(), second.as_path()]);
    assert_eq!(outcome.focus, Some(0));
    // The post that was there keeps its id; the new ones avoid it and each other.
    assert_eq!(ids(&collection.posts), ["photo-2", "photo-3", "photo"]);
    assert_eq!(collection.label, "3 folders");
    assert_eq!(collection.folder.as_deref(), Some(temp.path().join("b").as_path()));
}

#[test]
fn nothing_is_added_to_an_export() {
    let mut collection = open(&[fixture("export-basic")]).collection;
    let before = collection.clone();
    let error = catalog::add_loose_files(&mut collection, &[fixture("loose")], &options(), &empty_store()).unwrap_err();
    assert_eq!(error.code, OpenErrorCode::Export);
    assert_eq!(error.message, messages::CANNOT_ADD_TO_EXPORT);
    assert_eq!(collection, before);
}

// --- robustness ---------------------------------------------------------------------------------

#[cfg(windows)]
#[test]
fn a_file_name_that_is_not_valid_unicode_opens() {
    use std::ffi::OsString;
    use std::os::windows::ffi::OsStringExt;

    let temp = TempFolder::new("unicode");
    // "img" + an unpaired surrogate + ".jpg": a name Windows allows and UTF-8 cannot express.
    let name = OsString::from_wide(&[0x69, 0x6D, 0x67, 0xD800, 0x2E, 0x6A, 0x70, 0x67]);
    let path = temp.path().join(&name);
    fs::write(&path, JPEG).expect("Windows accepts the name");
    temp.write("plain.jpg", JPEG);

    let opened = open(&[temp.path().to_path_buf()]);
    assert_eq!(opened.collection.posts.len(), 2);
    assert_eq!(opened.collection.warnings, vec![]);
    let odd = opened.collection.posts.iter().find(|post| post.media[0].path == path).expect("the file is shown");
    // The path stays exact; the name on screen shows U+FFFD where it cannot be written.
    assert!(odd.media[0].path.is_file());
    assert_eq!(odd.context.as_deref(), Some("img\u{FFFD}.jpg"));
    assert_eq!(odd.id, "img_");

    // Picking it directly focuses it.
    let picked = open(std::slice::from_ref(&path));
    assert_eq!(picked.focus.map(|focus| picked.collection.posts[focus].media[0].path.clone()), Some(path));

    // The same in an export: the file is listed and simply never matches a path of post.json.
    temp.write("post.json", b"[{\"user\": \"a\", \"image\": \"plain.jpg\"}]");
    let export = open(&[temp.path().to_path_buf()]);
    assert!(export.collection.is_export());
    assert_eq!(export.collection.posts.len(), 1);
}

#[test]
fn a_path_longer_than_260_characters_opens() {
    let temp = TempFolder::new("long");
    let mut folder = temp.path().to_path_buf();
    for letter in ["a", "b", "c"] {
        folder = folder.join(letter.repeat(110));
    }
    fs::create_dir_all(&folder).expect("the standard library reaches long paths");
    let picture = folder.join("IMG_1.jpg");
    fs::write(&picture, JPEG).unwrap();
    fs::write(folder.join("IMG_2.jpg"), JPEG).unwrap();
    assert!(picture.to_string_lossy().len() > 330);

    let by_folder = open(std::slice::from_ref(&folder));
    assert_eq!(file_names(&by_folder.collection.posts), ["IMG_1.jpg", "IMG_2.jpg"]);
    assert_eq!(by_folder.collection.label, "c".repeat(110));
    let by_file = open(&[folder.join("IMG_2.jpg")]);
    assert_eq!(by_file.focus, Some(1));
    assert_eq!(by_file.collection.posts[0].media[0].path, picture);

    // And an export down there.
    fs::write(folder.join("post.json"), b"[{\"user\": \"a\", \"images\": [\"IMG_1.jpg\", \"IMG_2.jpg\"]}]").unwrap();
    let export = open(&[folder]);
    assert!(export.collection.is_export());
    assert_eq!(export.collection.posts[0].media.len(), 2);
    assert_eq!(export.collection.posts[0].media[0].path, picture);
}

// Clearing the read-only flag is what Windows needs before a file can be removed.
#[allow(clippy::permissions_set_readonly_false)]
#[test]
fn read_only_files_open_and_nothing_is_written_beside_them() {
    let temp = TempFolder::new("readonly");
    let folder = temp.path().join("pictures");
    fs::create_dir_all(&folder).unwrap();
    for name in ["IMG_2.jpg", "img_1.png"] {
        let copy = folder.join(name);
        fs::copy(fixture("loose").join(name), &copy).unwrap();
        let mut permissions = fs::metadata(&copy).unwrap().permissions();
        permissions.set_readonly(true);
        fs::set_permissions(&copy, permissions).unwrap();
    }
    let listing = |folder: &Path| {
        let mut names: Vec<String> =
            fs::read_dir(folder).unwrap().map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned()).collect();
        names.sort();
        names
    };
    let before = listing(&folder);

    let opened = open(std::slice::from_ref(&folder));
    assert_eq!(file_names(&opened.collection.posts), ["img_1.png", "IMG_2.jpg"]);
    assert_eq!(opened.collection.warnings, vec![]);
    assert_eq!(listing(&folder), before);

    // The fixtures themselves are left exactly as they were, too.
    let loose_before = listing(&fixture("loose"));
    open(&[fixture("loose")]);
    open(&[fixture("export-basic")]);
    assert_eq!(listing(&fixture("loose")), loose_before);
    assert_eq!(listing(&fixture("export-basic")), ["avatars", "post.json", "posts"]);

    // Writable again, so that the folder can be removed.
    for name in ["IMG_2.jpg", "img_1.png"] {
        let copy = folder.join(name);
        let mut permissions = fs::metadata(&copy).unwrap().permissions();
        permissions.set_readonly(false);
        fs::set_permissions(&copy, permissions).unwrap();
    }
}

#[test]
fn a_folder_of_several_thousand_pictures_opens_quickly() {
    const PICTURES: usize = 5200;
    let temp = TempFolder::new("many");
    let folder = temp.path().join("roll");
    fs::create_dir_all(&folder).unwrap();
    // Each file holds a JPEG header and then nothing a decoder could use: opening must not care.
    let mut content = JPEG.to_vec();
    content.resize(600, 0xAB);
    for number in 1..=PICTURES {
        fs::write(folder.join(format!("IMG_{number}.jpg")), &content).unwrap();
    }
    for number in 1..=40 {
        fs::write(folder.join(format!("notes {number}.txt")), b"not a picture").unwrap();
    }

    let started = std::time::Instant::now();
    let opened = open(std::slice::from_ref(&folder));
    let folder_time = started.elapsed();
    assert_eq!(opened.collection.posts.len(), PICTURES);
    assert_eq!(opened.collection.total_entries, PICTURES);
    assert_eq!(opened.collection.warnings, vec![]);
    assert_eq!(opened.focus, None);
    // Natural order: 1, 2, ... 10, 11, ... and not 1, 10, 100.
    for (position, post) in opened.collection.posts.iter().enumerate() {
        assert_eq!(post.id, format!("IMG_{}", position + 1));
    }

    // Picking one picture of the folder shows them all and finds the picked one.
    let started = std::time::Instant::now();
    let picked = open(&[folder.join("IMG_4321.jpg")]);
    let file_time = started.elapsed();
    assert_eq!(picked.collection.posts.len(), PICTURES);
    assert_eq!(picked.focus, Some(4320));

    println!("opened {PICTURES} pictures in {folder_time:?} (folder) and {file_time:?} (one picked file)");
    // Generous: a slow disk with a virus scanner must still pass, a decoder would not.
    let limit = std::time::Duration::from_secs(30);
    assert!(folder_time < limit && file_time < limit, "{folder_time:?} / {file_time:?}");
}
