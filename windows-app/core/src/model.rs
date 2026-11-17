//! The post model every other module shares: what one post is, what a collection of posts on
//! screen is, and the key under which a description is remembered.
//!
//! It is the `ImportedPost` of the reference web project (frontend/src/import/types.ts) with two
//! differences that a native viewer needs: media are files on disk (a path, not a URL), and a post
//! keeps the caption it was opened with (`original_caption`) next to the current one, because
//! edits are stored by the app and never written into the pictures' folder or an opened export.
//!
//! The module holds types and small accessors only; the rules that fill them are in `manifest`
//! (exports) and `catalog` (loose pictures).

use std::path::{Path, PathBuf};

use crate::messages;

/// What a media file is, by its content (see `sniff`), never by its name.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaKind {
    /// A picture (JPEG, PNG, GIF, WebP, AVIF, BMP, HEIC / HEIF).
    Image,
    /// A video (MP4, MOV, WebM): kept and exported, shown as its poster.
    Video,
}

impl MediaKind {
    /// The word post.json uses in `media[].type`: "image" or "video".
    pub fn as_str(self) -> &'static str {
        match self {
            MediaKind::Image => "image",
            MediaKind::Video => "video",
        }
    }
}

/// The declared type of a post (`media_type` in post.json).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaType {
    /// One picture.
    Image,
    /// Several pictures or videos to page through.
    Carousel,
    /// A video or reel, also when the export holds only its cover picture.
    Video,
}

impl MediaType {
    /// The word post.json uses in `media_type`: "image", "carousel" or "video".
    pub fn as_str(self) -> &'static str {
        match self {
            MediaType::Image => "image",
            MediaType::Carousel => "carousel",
            MediaType::Video => "video",
        }
    }
}

/// The poster image of a video.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Poster {
    /// The poster file on disk.
    pub path: PathBuf,
    /// Its path as written in post.json, normalised to forward slashes.
    pub manifest_path: String,
}

/// One picture or video of a post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MediaItem {
    /// The sniffed kind of the file. A video is shown as its poster; it is exported byte for byte.
    pub kind: MediaKind,
    /// The file on disk (absolute). These bytes are what an export contains.
    pub path: PathBuf,
    /// The path as written in post.json, normalised to forward slashes (for example
    /// `posts/post5_1.jpg`); for a loose picture, its file name. Export problem sentences quote it.
    pub manifest_path: String,
    /// Video only: the poster image, when the export ships one that is a picture.
    pub poster: Option<Poster>,
    /// Accessibility text of this one picture or video, when the export gives one.
    pub alt: Option<String>,
    /// Size of the file in bytes when the collection was opened.
    pub bytes: u64,
}

impl MediaItem {
    /// The picture that stands for this item on screen: the file itself for a picture, the poster
    /// for a video, `None` for a video without a poster (a neutral placeholder is shown).
    pub fn display_path(&self) -> Option<&Path> {
        match self.kind {
            MediaKind::Image => Some(&self.path),
            MediaKind::Video => self.poster.as_ref().map(|poster| poster.path.as_path()),
        }
    }
}

/// The author of a post.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct User {
    /// The name shown in bold: the export's username, or the default author of loose pictures.
    pub username: String,
    /// The display name, when the export gives one.
    pub full_name: Option<String>,
    /// `None` when the export does not say; then `verified` is not written back either.
    pub verified: Option<bool>,
    /// The profile picture on disk, when the export ships one that is a picture.
    pub avatar: Option<PathBuf>,
    /// Sanitised link to the author's profile (see `urls`). Never set for a loose picture.
    pub profile_url: Option<String>,
}

impl User {
    /// A user that is only a name: what a loose picture and an entry without an author get.
    pub fn named(username: impl Into<String>) -> User {
        User { username: username.into(), full_name: None, verified: None, avatar: None, profile_url: None }
    }
}

/// One of the sample comments an export may list for a post. Comments are shown, exported
/// unchanged, and never edited.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Comment {
    /// Internal id, `{post id}-c{1-based position}`; not written to post.json.
    pub id: String,
    /// Who wrote the comment.
    pub username: String,
    /// The comment.
    pub text: String,
    /// Likes of the comment (`like_count` in post.json).
    pub likes: u64,
    /// Epoch milliseconds (UTC), when known.
    pub taken_at: Option<i64>,
}

/// The stable key of a post in the descriptions store (`store::DescriptionStore`).
///
/// A loose picture is identified by its absolute path, an export post by the path of its post.json
/// plus the post id, a post of an export archive by the path of the archive plus the post id.
/// Paths are compared without regard to letter case on Windows, so the key holds the lower-cased
/// path there. The text form is what descriptions.json uses as the object key:
///
/// ```text
/// file|c:\users\me\pictures\img_2.jpg
/// export|c:\users\me\exports\trip\post.json|FXTbasic001
/// archive|c:\users\me\downloads\instagram-export.zip|FXTbasic001
/// ```
///
/// `|` cannot occur in a Windows path and not in a post id (`text::clean_id`), so the form is
/// unambiguous. The id part keeps its letter case: ids differing in case are different posts.
/// An archive post is keyed by the archive, not by the copy of its post.json in the archive
/// cache, so that a description survives the cache being cleaned up and rebuilt.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PostKey(String);

impl PostKey {
    /// The key of a loose picture file.
    pub fn for_file(path: &Path) -> PostKey {
        PostKey(format!("file|{}", fold_path(path)))
    }

    /// The key of a post of an opened export: the manifest (post.json) path plus the post id.
    pub fn for_export_post(manifest: &Path, id: &str) -> PostKey {
        PostKey(format!("export|{}|{}", fold_path(manifest), id))
    }

    /// The key of a post of an opened export archive: the archive file plus the post id.
    pub fn for_archive_post(archive: &Path, id: &str) -> PostKey {
        PostKey(format!("archive|{}|{}", fold_path(archive), id))
    }

    /// A key as it was read from descriptions.json. The text is taken as it is.
    pub fn from_stored(text: impl Into<String>) -> PostKey {
        PostKey(text.into())
    }

    /// The text form, used as the object key in descriptions.json.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PostKey {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// A path as text for a key. A file name need not be valid Unicode (Windows allows half a
/// surrogate pair in one). Showing every such part as U+FFFD, as a lossy conversion does, would
/// give two files that differ only there the same key; so the part is written as U+FFFD
/// followed by its bytes in hexadecimal, which keeps the names apart.
fn path_text(path: &Path) -> String {
    let mut text = String::new();
    for chunk in path.as_os_str().as_encoded_bytes().utf8_chunks() {
        text.push_str(chunk.valid());
        if !chunk.invalid().is_empty() {
            text.push(char::REPLACEMENT_CHARACTER);
            text.extend(chunk.invalid().iter().map(|byte| format!("{byte:02x}")));
        }
    }
    text
}

/// The comparable form of a path: absolute, without the `\\?\` prefix, and on Windows with
/// backslashes and in lower case. The file system is not consulted (the file may be gone). Two
/// paths that name the same file or folder fold to the same text; the archive cache, the merge
/// of several sources and the keys of descriptions all compare paths this way.
pub fn fold_path(path: &Path) -> String {
    let absolute = std::path::absolute(path).unwrap_or_else(|_| path.to_path_buf());
    let text = path_text(&absolute);
    let text = match text.strip_prefix(r"\\?\UNC\") {
        Some(rest) => format!(r"\\{rest}"),
        None => text.strip_prefix(r"\\?\").map(str::to_owned).unwrap_or(text),
    };
    if cfg!(windows) {
        text.replace('/', "\\").to_lowercase()
    } else {
        text
    }
}

/// Where a post comes from. It decides where its description is remembered, what the editor's
/// note says, what "Show in folder" selects and which file name the window title shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PostOrigin {
    /// A loose picture: the file is `media[0].path`.
    File,
    /// A post of an export folder (also the browser imports and the web app's library).
    Folder {
        /// The post.json that was read.
        manifest: PathBuf,
    },
    /// A post of an export archive; its media are copies in the archive cache.
    Archive {
        /// The archive file (absolute).
        archive: PathBuf,
    },
}

impl PostOrigin {
    /// The file or folder the post comes from, as `CollectionSource::Exports` names its sources:
    /// the post.json of a folder, the archive file of an archive; `None` for a loose picture.
    pub fn source_path(&self) -> Option<&Path> {
        match self {
            PostOrigin::File => None,
            PostOrigin::Folder { manifest } => Some(manifest),
            PostOrigin::Archive { archive } => Some(archive),
        }
    }
}

/// One post: a loose picture, or an entry of an opened export.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Post {
    /// Where the description of this post is remembered. Also the identity of the post in the UI
    /// (selection, auto view order): it is unique within a collection.
    pub key: PostKey,
    /// Unique within the collection: the export's id or shortcode, or one derived from the file
    /// name (see `catalog`). It names a single-post download (`export::single_post_export_name`).
    pub id: String,
    /// Sanitised link to the post; `None` when absent or unsafe, and always for a loose picture.
    pub permalink: Option<String>,
    /// At least one picture or video; the first one is the cover. A loose picture has exactly one.
    pub media: Vec<MediaItem>,
    /// The author.
    pub user: User,
    /// The description as it is now: the stored edit when there is one, else `original_caption`.
    pub caption: String,
    /// The description the post was opened with: the caption in post.json, or "" for a loose
    /// picture. An edit equal to it removes the stored entry.
    pub original_caption: String,
    /// Total number of comments on the original post (0 for a loose picture).
    pub comment_count: u64,
    /// `None` when unknown (always for a loose picture); then no likes line is shown and
    /// `like_count` is exported as `null`.
    pub like_count: Option<u64>,
    /// When the post was made, epoch milliseconds in UTC: `taken_at` of the export, or the
    /// modified time of a loose file. `None`: no time label and no date line.
    pub taken_at: Option<i64>,
    /// The declared type. `export` recomputes what it writes from the media (spec-export A.5).
    pub media_type: MediaType,
    /// Accessibility text of the post (the cover), when the export gives one.
    pub alt: Option<String>,
    /// The place named by the export. Exported as `location`.
    pub location: Option<String>,
    /// The header sub-line that is NOT exported: the file name of a loose picture. `None` for an
    /// export post, where the sub-line is `location`.
    pub context: Option<String>,
    /// Sample comments of an export (empty for a loose picture).
    pub comments: Vec<Comment>,
    /// Where the post comes from (a picture file, an export folder or an archive).
    pub origin: PostOrigin,
}

impl Post {
    /// The cover: the first media item. `None` only for a post built by hand without media.
    pub fn cover(&self) -> Option<&MediaItem> {
        self.media.first()
    }

    /// True when the post has more than one picture or video.
    pub fn is_carousel(&self) -> bool {
        self.media.len() > 1
    }

    /// True when the cover is a video or the post is declared a video (the grid then shows the
    /// video badge; a cover-only video post has an image as its only media).
    pub fn is_video(&self) -> bool {
        self.media_type == MediaType::Video
            || self.cover().is_some_and(|media| media.kind == MediaKind::Video)
    }

    /// The muted line under the username: the file name of a loose picture, else the location.
    pub fn header_subline(&self) -> Option<&str> {
        self.context.as_deref().or(self.location.as_deref())
    }

    /// True when the current description differs from the one the post was opened with.
    pub fn is_edited(&self) -> bool {
        self.caption != self.original_caption
    }

    /// The file name of the cover file, for the window title and "Show in folder".
    pub fn file_name(&self) -> Option<String> {
        self.media_file_name(0)
    }

    /// The file name of slide `slide` (0-based) as the user knows it: the name of the file on
    /// disk, or for a post of an archive the last part of its path in post.json (the copy in the
    /// archive cache has a name the app made up).
    pub fn media_file_name(&self, slide: usize) -> Option<String> {
        let media = self.media.get(slide)?;
        match self.origin {
            PostOrigin::Archive { .. } => media.manifest_path.rsplit('/').next().map(str::to_owned),
            PostOrigin::File | PostOrigin::Folder { .. } => {
                media.path.file_name().map(|name| name.to_string_lossy().into_owned())
            }
        }
    }

    /// The accessible name of slide `slide` (0-based), as the clone builds it
    /// (importedPosts.ts `toPost`): the slide's own alt, else for the cover the post's alt or
    /// "Photo by {username}", else "Photo {n} of {total} by {username}" ("Video ..." for a video).
    pub fn alt_text(&self, slide: usize) -> String {
        let media = self.media.get(slide);
        if let Some(alt) = media.and_then(|media| media.alt.as_deref()).map(str::trim) {
            if !alt.is_empty() {
                return alt.to_owned();
            }
        }
        let is_video = media.is_some_and(|media| media.kind == MediaKind::Video);
        if slide == 0 {
            if let Some(alt) = self.alt.as_deref().map(str::trim) {
                if !alt.is_empty() {
                    return alt.to_owned();
                }
            }
            return messages::cover_alt(is_video, &self.user.username);
        }
        messages::slide_alt(is_video, slide + 1, self.media.len(), &self.user.username)
    }
}

/// Machine-readable reason of a warning, the clone's `ImportWarningCode`. The UI shows only the
/// message; tests and the "first problem" rule of `manifest` use the code.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum WarningCode {
    /// A file named by post.json is not in the export, or is a URL.
    MissingImage,
    /// A file is not a supported picture (or is too large, empty, unreadable).
    UnsupportedImage,
    /// A file declared as a video is not a supported video.
    UnsupportedMedia,
    /// An entry or a path of post.json is unusable.
    InvalidEntry,
    /// A link was present but rejected.
    UnsafeUrl,
    /// A given id was used twice.
    DuplicateId,
    /// A list was cut at a limit.
    Truncated,
}

/// Something that was skipped or changed while opening. `message` is a complete sentence.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Warning {
    /// The machine-readable reason.
    pub code: WarningCode,
    /// The sentence to show.
    pub message: String,
    /// Zero-based index of the entry in post.json, when the warning is about one entry.
    pub post_index: Option<usize>,
    /// The file or path the warning is about, when there is one.
    pub path: Option<String>,
    /// In a collection merged from several sources: the source the warning is about, as
    /// `fold_path` of `ExportSource::path`. When that source is opened again, its old warnings
    /// go with its old posts. `None` outside a merge.
    pub source: Option<String>,
}

impl Warning {
    /// A warning that is not about one entry of post.json.
    pub fn new(code: WarningCode, message: impl Into<String>) -> Warning {
        Warning { code, message: message.into(), post_index: None, path: None, source: None }
    }

    /// A warning about entry `post_index` (zero-based) of post.json.
    pub fn for_post(code: WarningCode, message: impl Into<String>, post_index: usize) -> Warning {
        Warning { code, message: message.into(), post_index: Some(post_index), path: None, source: None }
    }

    /// The same warning, naming the file or path it is about.
    pub fn with_path(mut self, path: impl Into<String>) -> Warning {
        self.path = Some(path.into());
        self
    }
}

/// What kind of place an export source is.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum ExportKind {
    /// An export folder (a folder with post.json, or that file), also the web app's library.
    Folder,
    /// An export archive (.zip, .tar, .tar.gz, .tgz).
    Archive,
    /// The browser imports folder of this app (`settings::AppPaths::library_dir`).
    WebLibrary,
}

/// One export folder or archive whose posts are on screen (the clone's `ImportSourceSummary`).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExportSource {
    /// What kind of place it is.
    pub kind: ExportKind,
    /// The export root folder, or the archive file (absolute).
    pub path: PathBuf,
    /// The post.json that was read: in the folder, or the copy in the archive cache.
    pub manifest: PathBuf,
    /// What sentences and the banner call it: the root folder name, the archive file name, or
    /// "Browser imports".
    pub name: String,
    /// When it was opened, epoch milliseconds.
    pub opened_at: i64,
    /// `exported_at` of its post.json, epoch milliseconds, when it says.
    pub exported_at: Option<i64>,
    /// The sanitised source link of its post.json, when present.
    pub source_url: Option<String>,
    /// How many entries its post.json lists.
    pub total_entries: usize,
    /// How many of its posts are on screen after the merge.
    pub shown: usize,
    /// How many posts of earlier sources its posts replaced (same post id, read last wins).
    pub replaced: usize,
}

impl ExportSource {
    /// The path that identifies this source among the others: the post.json of a folder (the
    /// path of `PostOrigin::Folder`), the archive file of an archive.
    pub fn identity(&self) -> &Path {
        match self.kind {
            ExportKind::Archive => &self.path,
            ExportKind::Folder | ExportKind::WebLibrary => &self.manifest,
        }
    }

    /// True when `post` comes from this source.
    pub fn owns(&self, post: &Post) -> bool {
        post.origin.source_path().is_some_and(|path| fold_path(path) == fold_path(self.identity()))
    }
}

/// Where the posts on screen come from.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CollectionSource {
    /// Picture files opened directly: one post per file. More files can be added.
    Loose,
    /// One or several export folders and archives, merged (`sources`). They are never modified;
    /// more export folders and archives can be added.
    Exports(Vec<ExportSource>),
}

/// The posts on screen and what the banner says about them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Collection {
    /// In display order: file-name order for loose pictures, post.json order for one export,
    /// newest first for several merged exports.
    pub posts: Vec<Post>,
    /// Loose pictures or exports; it decides the banner wording and what can be added.
    pub source: CollectionSource,
    /// The name the banner shows: the folder name of the pictures, the name of the one export
    /// source, or a count such as "2 archives and 1 folder" (see `catalog::collection_label`,
    /// `sources::describe_sources`).
    pub label: String,
    /// The folder to remember as "last folder" and to offer in the next Open dialog: the folder
    /// of the pictures (of the first one when they come from several), the root of the first
    /// export folder, or the folder of the first archive.
    pub folder: Option<PathBuf>,
    /// Everything that was skipped or changed while opening, in the clone's order: export-level
    /// warnings first, then by post.
    pub warnings: Vec<Warning>,
    /// Sanitised link of the page an export was taken from (`source.url`), when present.
    pub source_url: Option<String>,
    /// Number of entries post.json lists; `posts.len()` is lower when entries were skipped. For
    /// loose pictures it equals `posts.len()`.
    pub total_entries: usize,
}

impl Collection {
    /// True for opened exports, false for loose pictures.
    pub fn is_export(&self) -> bool {
        matches!(self.source, CollectionSource::Exports(_))
    }

    /// The export sources of the posts on screen, in the order they were read; empty for loose
    /// pictures.
    pub fn sources(&self) -> &[ExportSource] {
        match &self.source {
            CollectionSource::Exports(sources) => sources,
            CollectionSource::Loose => &[],
        }
    }

    /// The index of the post with this key.
    pub fn index_of(&self, key: &PostKey) -> Option<usize> {
        self.posts.iter().position(|post| &post.key == key)
    }

    /// The post with this key.
    pub fn post(&self, key: &PostKey) -> Option<&Post> {
        self.posts.iter().find(|post| &post.key == key)
    }

    /// The post with this key, for an edit of its description.
    pub fn post_mut(&mut self, key: &PostKey) -> Option<&mut Post> {
        self.posts.iter_mut().find(|post| &post.key == key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn picture(name: &str) -> MediaItem {
        MediaItem {
            kind: MediaKind::Image,
            path: PathBuf::from(name),
            manifest_path: name.to_owned(),
            poster: None,
            alt: None,
            bytes: 1,
        }
    }

    fn post(media: Vec<MediaItem>) -> Post {
        Post {
            key: PostKey::from_stored("file|a"),
            id: "a".to_owned(),
            permalink: None,
            media,
            user: User::named("unknown"),
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

    #[test]
    fn archive_keys_name_the_archive_and_keep_the_letter_case_of_the_id() {
        let archive = Path::new("downloads").join("Export-Basic.ZIP");
        let key = PostKey::for_archive_post(&archive, "FXTbasic001");
        assert!(key.as_str().starts_with("archive|"));
        assert!(key.as_str().ends_with("|FXTbasic001"));
        assert_ne!(key, PostKey::for_archive_post(&archive, "fxtbasic001"));
        assert_ne!(key, PostKey::for_export_post(&archive, "FXTbasic001"));
        #[cfg(windows)]
        assert_eq!(
            PostKey::for_archive_post(Path::new(r"C:\Downloads\Export-Basic.ZIP"), "FXTbasic001").as_str(),
            r"archive|c:\downloads\export-basic.zip|FXTbasic001"
        );
    }

    #[test]
    fn an_archive_post_is_named_by_its_path_in_post_json() {
        let mut cached = post(vec![picture("m000001.jpg")]);
        cached.media[0].manifest_path = "posts/IMG_7.jpg".to_owned();
        assert_eq!(cached.file_name().as_deref(), Some("m000001.jpg"));
        cached.origin = PostOrigin::Archive { archive: PathBuf::from("export.zip") };
        assert_eq!(cached.file_name().as_deref(), Some("IMG_7.jpg"));
        assert_eq!(cached.media_file_name(1), None);
        assert_eq!(cached.origin.source_path(), Some(Path::new("export.zip")));
    }

    #[test]
    fn a_source_owns_the_posts_of_its_manifest_or_archive() {
        let manifest = std::env::temp_dir().join("trip").join("post.json");
        let source = ExportSource {
            kind: ExportKind::Folder,
            path: manifest.parent().map(Path::to_path_buf).unwrap_or_default(),
            manifest: manifest.clone(),
            name: "trip".to_owned(),
            opened_at: 0,
            exported_at: None,
            source_url: None,
            total_entries: 1,
            shown: 1,
            replaced: 0,
        };
        let mut entry = post(vec![picture("a.jpg")]);
        assert!(!source.owns(&entry));
        entry.origin = PostOrigin::Folder { manifest: manifest.clone() };
        assert!(source.owns(&entry));
        let archive = ExportSource { kind: ExportKind::Archive, path: manifest.with_extension("zip"), ..source };
        assert!(!archive.owns(&entry));
        entry.origin = PostOrigin::Archive { archive: manifest.with_extension("zip") };
        assert!(archive.owns(&entry));
    }

    #[test]
    fn export_keys_keep_the_letter_case_of_the_id() {
        let manifest = Path::new("exports").join("trip").join("post.json");
        let upper = PostKey::for_export_post(&manifest, "FXTbasic001");
        let lower = PostKey::for_export_post(&manifest, "fxtbasic001");
        assert_ne!(upper, lower);
        assert!(upper.as_str().starts_with("export|"));
        assert!(upper.as_str().ends_with("|FXTbasic001"));
    }

    #[cfg(windows)]
    #[test]
    fn file_keys_ignore_letter_case_and_slash_direction_on_windows() {
        let one = PostKey::for_file(Path::new(r"C:\Users\Me\Pictures\IMG_2.jpg"));
        let other = PostKey::for_file(Path::new("c:/users/me/pictures/img_2.JPG"));
        assert_eq!(one, other);
        assert_eq!(one.as_str(), r"file|c:\users\me\pictures\img_2.jpg");
        let verbatim = PostKey::for_file(Path::new(r"\\?\C:\Users\Me\Pictures\IMG_2.jpg"));
        assert_eq!(one, verbatim);
    }

    #[cfg(windows)]
    #[test]
    fn names_that_are_not_unicode_still_get_keys_of_their_own() {
        use std::ffi::OsString;
        use std::os::windows::ffi::OsStringExt;
        // "a", half a surrogate pair, ".jpg": two names that a lossy conversion cannot tell apart.
        let name = |half: u16| {
            let units: Vec<u16> = [u16::from(b'a'), half].into_iter().chain(".jpg".encode_utf16()).collect();
            Path::new(r"C:\Pictures").join(OsString::from_wide(&units))
        };
        let (one, other) = (name(0xD800), name(0xD801));
        assert_eq!(one.to_string_lossy(), other.to_string_lossy());
        assert_ne!(PostKey::for_file(&one), PostKey::for_file(&other));
        assert_eq!(PostKey::for_file(&one), PostKey::for_file(&name(0xD800)));
        assert!(PostKey::for_file(&one).as_str().starts_with("file|c:\\pictures\\a\u{FFFD}"));
        // Text that is valid Unicode is taken as it is, whatever the script.
        assert_eq!(PostKey::for_file(Path::new(r"C:\Bilder\Größe 朝.JPG")).as_str(), "file|c:\\bilder\\größe 朝.jpg");
    }

    #[test]
    fn alt_text_follows_the_clone() {
        let mut single = post(vec![picture("a.jpg")]);
        assert_eq!(single.alt_text(0), "Photo by unknown");
        single.alt = Some("  A test card  ".to_owned());
        assert_eq!(single.alt_text(0), "A test card");

        let mut carousel = post(vec![picture("a.jpg"), picture("b.jpg"), picture("c.jpg")]);
        carousel.media[2].alt = Some("Third".to_owned());
        carousel.media[1].kind = MediaKind::Video;
        assert_eq!(carousel.alt_text(1), "Video 2 of 3 by unknown");
        assert_eq!(carousel.alt_text(2), "Third");
    }

    #[test]
    fn the_sub_line_prefers_the_file_name() {
        let mut loose = post(vec![picture("a.jpg")]);
        assert_eq!(loose.header_subline(), None);
        loose.location = Some("Fixtureville".to_owned());
        assert_eq!(loose.header_subline(), Some("Fixtureville"));
        loose.context = Some("a.jpg".to_owned());
        assert_eq!(loose.header_subline(), Some("a.jpg"));
    }
}
