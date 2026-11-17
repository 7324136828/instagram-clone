//! What the browser sees: the posts that are open in the app, as the clone's library JSON
//! (`GET /api/library`, library.py `Library._response`), with a virtual file map behind it
//! (web brief 4.7 / 4.8).
//!
//! The UI thread makes a `Snapshot` after every change of what is open (a cheap copy of the
//! posts and sources) and publishes it. Everything that needs the disk is done later, on the
//! first request that needs it, on an HTTP thread (`Snapshot::built`): the formats of the files
//! are sniffed (through a cache shared by all snapshots, keyed by the folded path and the size),
//! and the JSON and the file map are made once.
//!
//! * Post ids are the app's own (`Post::id`): unique within a collection and already in the
//!   clone's `cleanId` form, so the frontend's parser keeps them unchanged and a PATCH maps back
//!   through `Snapshot::key_of`.
//! * Virtual paths are stable across snapshots, so that a page loaded earlier never shows another
//!   post's file: `posts/<stem>_<k>.<ext>`, `posts/<stem>_<k>_poster.<ext>` with stem =
//!   `safe_stem(id)` (at most 48 characters) + "-" + 6 hex digits of FNV-1a of the post key, and
//!   `avatars/<safe_stem(username)>-<6 hex of the folded avatar path>.<ext>`. `<ext>` comes from
//!   the content (`sniff`); a HEIC picture is listed as `.jpg` and served converted
//!   (`files::HeicCache`). These shapes match library.py's `POST_FILE_RE` / `AVATAR_FILE_RE`
//!   and the frontend's `LIBRARY_FILE_RE`.
//! * A file that cannot be read or sniffed now is left out; a post left without media is left
//!   out.
//! * Videos are served as they are, in ranges (a browser seeks with Range requests). A video of
//!   an export lists its own poster, or none. A loose video lists a poster,
//!   `posts/<stem>_1_poster.jpg`, made from its first frame on the first request
//!   (`poster::PosterCache`, video brief 5.9), and that poster is its `image`. `GET /api/library`
//!   makes the posters first (`files::PosterService::prepare`); once making one failed
//!   (`Snapshot::record_poster_failure`), the JSON is made again without it, so pages never ask
//!   for a poster that cannot come.
//! * The author of loose pictures is the Windows account name unless settings.json names one;
//!   other computers are told `unknown` instead (`Snapshot::with_remote_loose_author`).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde_json::{Map, Value};

use crate::library;
use crate::messages;
use crate::model::{fold_path, Collection, CollectionSource, ExportKind, ExportSource, MediaKind, MediaType, Post, PostKey, PostOrigin};
use crate::sniff::{self, MediaFormat};
use crate::text;

/// Post id stems are cut to this many characters before the hash is appended.
pub const MAX_VIRTUAL_STEM_CHARS: usize = 48;

/// What is open, as far as the upload and clear rules care.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapshotKind {
    /// Nothing is open.
    Empty,
    /// Loose pictures: browser imports are refused (409).
    Loose,
    /// Export folders and archives.
    Exports {
        /// True when the browser imports folder is the only source (then the browser may clear it).
        only_web_library: bool,
    },
}

/// The MIME type of a format, as the clone's library reports it.
pub fn mime_of(format: MediaFormat) -> &'static str {
    match format {
        MediaFormat::Jpeg => "image/jpeg",
        MediaFormat::Png => "image/png",
        MediaFormat::Gif => "image/gif",
        MediaFormat::Webp => "image/webp",
        MediaFormat::Avif => "image/avif",
        MediaFormat::Heic => "image/heic",
        MediaFormat::Bmp => "image/bmp",
        MediaFormat::Mp4 => "video/mp4",
        MediaFormat::Mov => "video/quicktime",
        MediaFormat::Webm => "video/webm",
    }
}

/// What is known about files, shared by all snapshots of a run: their formats, sniffed once per
/// file version, and the loose videos no poster could be made of.
#[derive(Debug, Default)]
pub struct FormatCache {
    known: Mutex<HashMap<(String, u64), Option<MediaFormat>>>,
    poster_failures: Mutex<HashSet<(String, u64)>>,
}

/// The key of a file version: its folded path and its size; `None` when it is no regular file.
fn version_of(path: &Path) -> Option<(String, u64)> {
    let metadata = fs::metadata(path).ok().filter(fs::Metadata::is_file)?;
    Some((fold_path(path), metadata.len()))
}

impl FormatCache {
    /// The format and size of the file at `path`, `None` when it is no regular file or no
    /// supported picture or video.
    pub fn format_of(&self, path: &Path) -> Option<(MediaFormat, u64)> {
        let key = version_of(path)?;
        let size = key.1;
        if let Some(known) = self.known.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).get(&key) {
            return known.map(|format| (format, size));
        }
        let format = sniff::sniff_file(path).ok().flatten().map(|sniffed| sniffed.format);
        self.known.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(key, format);
        format.map(|format| (format, size))
    }

    /// Remembers that no poster could be made of the video at `path` (as it is now).
    pub fn record_poster_failure(&self, path: &Path) {
        if let Some(key) = version_of(path) {
            self.poster_failures.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).insert(key);
        }
    }

    /// True when no poster could be made of the video at `path` as it is now.
    pub fn poster_failed(&self, path: &Path) -> bool {
        version_of(path).is_some_and(|key| self.poster_failures.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).contains(&key))
    }

    /// How many posters failed so far: a snapshot built with fewer is built again.
    fn poster_failure_count(&self) -> usize {
        self.poster_failures.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).len()
    }
}

/// A file the browser may fetch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VirtualFile {
    /// The real file.
    pub path: PathBuf,
    /// The content type it is served with.
    pub mime: &'static str,
    /// True for a HEIC picture served as a JPEG copy.
    pub heic: bool,
    /// True for the poster of a loose video: `path` is the video, the poster is made from it.
    pub video_poster: bool,
}

/// What a snapshot becomes on the first request that needs the disk.
#[derive(Debug)]
pub struct Built {
    /// The library JSON for this computer.
    pub local_body: Vec<u8>,
    /// The library JSON for other computers (no full path in `directory`).
    pub remote_body: Vec<u8>,
    /// Virtual path -> real file.
    pub files: HashMap<String, VirtualFile>,
    /// How many posts are listed.
    pub listed_posts: usize,
}

/// The posts that are open, at one moment.
#[derive(Debug)]
pub struct Snapshot {
    /// Grows with every published change (1 for the first, empty snapshot).
    pub revision: u64,
    /// When this change was published (epoch ms); `None` before anything was open.
    pub updated_at: Option<i64>,
    /// What is open.
    pub kind: SnapshotKind,
    posts: Vec<Post>,
    sources: Vec<ExportSource>,
    label: String,
    library_dir: PathBuf,
    ids: HashMap<String, PostKey>,
    web_library_ids: HashSet<String>,
    formats: Arc<FormatCache>,
    remote_loose_author: Option<String>,
    /// The built JSON and file map with the number of poster failures known when it was made.
    built: Mutex<Option<(usize, Arc<Built>)>>,
}

impl Snapshot {
    /// The snapshot of `collection` (`None`: nothing is open). `library_dir` is the browser
    /// imports folder.
    pub fn new(revision: u64, updated_at: Option<i64>, collection: Option<&Collection>, library_dir: &Path, formats: Arc<FormatCache>) -> Snapshot {
        let posts: Vec<Post> = collection.map(|collection| collection.posts.clone()).unwrap_or_default();
        let sources: Vec<ExportSource> = collection.map(|collection| collection.sources().to_vec()).unwrap_or_default();
        let kind = match collection.map(|collection| &collection.source) {
            None => SnapshotKind::Empty,
            Some(CollectionSource::Loose) => SnapshotKind::Loose,
            Some(CollectionSource::Exports(sources)) => SnapshotKind::Exports { only_web_library: sources.len() == 1 && sources[0].kind == ExportKind::WebLibrary },
        };
        let ids = posts.iter().map(|post| (post.id.clone(), post.key.clone())).collect();
        let web_library_ids = sources
            .iter()
            .filter(|source| source.kind == ExportKind::WebLibrary)
            .flat_map(|source| posts.iter().filter(|post| source.owns(post)).map(|post| post.id.clone()))
            .collect();
        Snapshot {
            revision,
            updated_at,
            kind,
            posts,
            sources,
            label: collection.map(|collection| collection.label.clone()).unwrap_or_default(),
            library_dir: library_dir.to_path_buf(),
            ids,
            web_library_ids,
            formats,
            remote_loose_author: None,
            built: Mutex::new(None),
        }
    }

    /// The same snapshot, telling other computers `author` as the author of loose pictures
    /// (used when the author is the Windows account name, which settings.json did not choose).
    pub fn with_remote_loose_author(mut self, author: &str) -> Snapshot {
        self.remote_loose_author = Some(author.to_owned());
        self
    }

    /// Remembers that no poster could be made of the loose video at `path`: the next JSON lists
    /// it without one.
    pub fn record_poster_failure(&self, path: &Path) {
        self.formats.record_poster_failure(path);
    }

    /// What is known about the files, shared by the snapshots of a run.
    pub fn formats(&self) -> &Arc<FormatCache> {
        &self.formats
    }

    /// The loose videos whose posters the JSON lists (made from their first frames).
    pub fn listed_poster_videos(&self) -> Vec<PathBuf> {
        let mut videos: Vec<PathBuf> = self.built().files.values().filter(|file| file.video_poster).map(|file| file.path.clone()).collect();
        videos.sort();
        videos
    }

    /// The key of the open post with this id.
    pub fn key_of(&self, id: &str) -> Option<&PostKey> {
        self.ids.get(id)
    }

    /// True when the post with this id comes from the browser imports folder.
    pub fn is_web_library_post(&self, id: &str) -> bool {
        self.web_library_ids.contains(id)
    }

    /// The posts of the snapshot, in display order.
    pub fn posts(&self) -> &[Post] {
        &self.posts
    }

    /// The JSON, file map and counts, made on first use, and again after a poster failed
    /// (concurrent requests wait for one build).
    pub fn built(&self) -> Arc<Built> {
        let mut built = self.built.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let failures = self.formats.poster_failure_count();
        if let Some((known, current)) = built.as_ref() {
            if *known == failures {
                return Arc::clone(current);
            }
        }
        let fresh = Arc::new(self.build());
        *built = Some((failures, Arc::clone(&fresh)));
        fresh
    }

    /// The library JSON for a client on this computer (`local`) or another one.
    pub fn body(&self, local: bool) -> Vec<u8> {
        let built = self.built();
        if local {
            built.local_body.clone()
        } else {
            built.remote_body.clone()
        }
    }

    /// The `directory` of the JSON for this computer. The clone's pages show it where they say
    /// that imports are kept ("Imports are added to your library, which this app's server keeps
    /// in ..."), where an import was saved ("Saved in your library: ..."), in the banner
    /// ("saved in ...") and in the Clear library question. While exports are open, or nothing,
    /// that is the browser imports folder: imports go there, and Clear acts on it alone; the
    /// user's own folders and archives are never named as the place an import is kept. With
    /// loose pictures open, imports are refused (409), and the banner names the folders of the
    /// pictures (the only one, or "{first} and {n} more").
    pub fn local_directory(&self) -> String {
        if self.kind != SnapshotKind::Loose {
            return self.library_dir.display().to_string();
        }
        let mut places: Vec<PathBuf> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for post in &self.posts {
            if let Some(parent) = post.cover().and_then(|cover| cover.path.parent()) {
                if seen.insert(fold_path(parent)) {
                    places.push(parent.to_path_buf());
                }
            }
        }
        match places.as_slice() {
            [] => self.library_dir.display().to_string(),
            [only] => only.display().to_string(),
            [first, rest @ ..] => messages::places_and_more(&first.display().to_string(), rest.len()),
        }
    }

    /// The `directory` for another computer (clone `for_client`): the name of the browser imports
    /// folder, or of the loose pictures that are open, never a full path.
    pub fn remote_directory(&self) -> String {
        let label = if self.kind == SnapshotKind::Loose { self.label.clone() } else { messages::display_name(&self.library_dir) };
        messages::remote_directory(&label)
    }

    /// The JSON entries of the posts and the file map, with `author` as the author of loose
    /// pictures when it is given.
    fn posts_json(&self, author: Option<&str>) -> (Vec<Value>, HashMap<String, VirtualFile>) {
        let mut files: HashMap<String, VirtualFile> = HashMap::new();
        let mut taken_stems: HashSet<String> = HashSet::new();
        let mut avatar_paths: HashMap<String, Option<String>> = HashMap::new();
        let mut posts_json: Vec<Value> = Vec::new();
        for post in &self.posts {
            if let Some(entry) = self.post_json(post, author, &mut files, &mut taken_stems, &mut avatar_paths) {
                posts_json.push(Value::Object(entry));
            }
        }
        (posts_json, files)
    }

    fn build(&self) -> Built {
        let (posts_json, files) = self.posts_json(None);
        // The same posts with another author name, and the same file names (the stems do not
        // depend on the author).
        let remote_posts = match (&self.remote_loose_author, self.kind) {
            (Some(author), SnapshotKind::Loose) => self.posts_json(Some(author)).0,
            _ => posts_json.clone(),
        };
        let listed_posts = posts_json.len();
        let imports = self.imports_json();
        let updated_at = self.updated_at.and_then(text::iso_time_millis).map_or(Value::Null, Value::from);
        let body = |directory: String, posts: Vec<Value>| -> Vec<u8> {
            let mut object = Map::new();
            object.insert("directory".to_owned(), Value::from(directory));
            object.insert("revision".to_owned(), Value::from(self.revision));
            object.insert("updated_at".to_owned(), updated_at.clone());
            object.insert("imports".to_owned(), Value::Array(imports.clone()));
            object.insert("posts".to_owned(), Value::Array(posts));
            Value::Object(object).to_string().into_bytes()
        };
        Built { local_body: body(self.local_directory(), posts_json), remote_body: body(self.remote_directory(), remote_posts), files, listed_posts }
    }

    /// The stem of the virtual files of `post`, unique among the stems taken so far.
    fn post_stem(post: &Post, taken: &mut HashSet<String>) -> String {
        let safe: String = library::safe_stem(&post.id, "post").chars().take(MAX_VIRTUAL_STEM_CHARS).collect();
        let base = format!("{safe}-{:06x}", text::fnv1a_64(post.key.as_str().as_bytes()) & 0xFF_FFFF);
        let mut candidate = base.clone();
        let mut number = 2;
        while !taken.insert(candidate.to_lowercase()) {
            candidate = format!("{base}-{number}");
            number += 1;
        }
        candidate
    }

    /// The virtual path of a picture file at `path` (a poster or an avatar): its format must be a
    /// picture.
    fn picture_file(&self, path: &Path, virtual_base: &str, files: &mut HashMap<String, VirtualFile>) -> Option<(String, Option<u64>, &'static str)> {
        let (format, size) = self.formats.format_of(path)?;
        if format.kind() != MediaKind::Image {
            return None;
        }
        Some(Self::register(path, format, size, virtual_base, files))
    }

    /// Registers a file under `<virtual_base>.<ext>`; its virtual path, its size (none for a
    /// converted HEIC) and the MIME type it is served with.
    fn register(path: &Path, format: MediaFormat, size: u64, virtual_base: &str, files: &mut HashMap<String, VirtualFile>) -> (String, Option<u64>, &'static str) {
        let heic = format == MediaFormat::Heic;
        let (extension, mime) = if heic { ("jpg", "image/jpeg") } else { (format.extension(), mime_of(format)) };
        let virtual_path = format!("{virtual_base}.{extension}");
        files.insert(virtual_path.clone(), VirtualFile { path: path.to_path_buf(), mime, heic, video_poster: false });
        (virtual_path, (!heic).then_some(size), mime)
    }

    /// The JSON entry of one post (key order as library.py writes it), or `None` when none of its
    /// files can be shown.
    fn post_json(&self, post: &Post, author: Option<&str>, files: &mut HashMap<String, VirtualFile>, taken: &mut HashSet<String>, avatars: &mut HashMap<String, Option<String>>) -> Option<Map<String, Value>> {
        let stem = Self::post_stem(post, taken);
        let mut media: Vec<Value> = Vec::new();
        let mut pictures: Vec<String> = Vec::new();
        let mut first_kind: Option<MediaKind> = None;
        for (index, item) in post.media.iter().enumerate() {
            let number = index + 1;
            let Some((format, size)) = self.formats.format_of(&item.path) else {
                continue;
            };
            let (path, bytes, mime) = Self::register(&item.path, format, size, &format!("posts/{stem}_{number}"), files);
            let kind = format.kind();
            first_kind.get_or_insert(kind);
            let mut entry = Map::new();
            entry.insert("type".to_owned(), Value::from(kind.as_str()));
            entry.insert("path".to_owned(), Value::from(path.clone()));
            let mut picture = (kind == MediaKind::Image).then(|| path.clone());
            if kind == MediaKind::Video {
                if let Some(poster) = &item.poster {
                    if let Some((poster_path, _, _)) = self.picture_file(&poster.path, &format!("posts/{stem}_{number}_poster"), files) {
                        entry.insert("poster".to_owned(), Value::from(poster_path.clone()));
                        picture = Some(poster_path);
                    }
                } else if post.origin == PostOrigin::File && !self.formats.poster_failed(&item.path) {
                    // A loose video: its poster is made from its first frame when it is asked for.
                    let poster_path = format!("posts/{stem}_{number}_poster.jpg");
                    files.insert(poster_path.clone(), VirtualFile { path: item.path.clone(), mime: "image/jpeg", heic: false, video_poster: true });
                    entry.insert("poster".to_owned(), Value::from(poster_path.clone()));
                    picture = Some(poster_path);
                }
            }
            if let Some(alt) = item.alt.as_deref().map(str::trim).filter(|alt| !alt.is_empty()) {
                entry.insert("alt".to_owned(), Value::from(alt));
            }
            entry.insert("mime".to_owned(), Value::from(mime));
            if let Some(bytes) = bytes {
                entry.insert("bytes".to_owned(), Value::from(bytes));
            }
            media.push(Value::Object(entry));
            pictures.extend(picture);
        }
        let first_kind = first_kind?;
        let media_type = match post.media_type {
            MediaType::Video => "video",
            MediaType::Carousel => "carousel",
            MediaType::Image if media.len() > 1 => "carousel",
            MediaType::Image => first_kind.as_str(),
        };

        let mut user = Map::new();
        let username = match author {
            Some(author) if post.origin == PostOrigin::File => author,
            _ => post.user.username.as_str(),
        };
        user.insert("username".to_owned(), Value::from(username));
        if let Some(full_name) = &post.user.full_name {
            user.insert("full_name".to_owned(), Value::from(full_name.clone()));
        }
        if let Some(verified) = post.user.verified {
            user.insert("verified".to_owned(), Value::from(verified));
        }
        if let Some(profile_url) = &post.user.profile_url {
            user.insert("profile_url".to_owned(), Value::from(profile_url.clone()));
        }
        if let Some(avatar) = &post.user.avatar {
            let folded = fold_path(avatar);
            let virtual_path = match avatars.get(&folded) {
                Some(known) => known.clone(),
                None => {
                    let safe: String = library::safe_stem(&post.user.username, "user").chars().take(MAX_VIRTUAL_STEM_CHARS).collect();
                    let base = format!("avatars/{safe}-{:06x}", text::fnv1a_64(folded.as_bytes()) & 0xFF_FFFF);
                    let found = self.picture_file(avatar, &base, files).map(|(path, _, _)| path);
                    avatars.insert(folded, found.clone());
                    found
                }
            };
            if let Some(path) = virtual_path {
                user.insert("avatar".to_owned(), Value::from(path));
            }
        }

        let mut entry = Map::new();
        entry.insert("id".to_owned(), Value::from(post.id.clone()));
        if let Some(url) = &post.permalink {
            entry.insert("url".to_owned(), Value::from(url.clone()));
        }
        entry.insert("media_type".to_owned(), Value::from(media_type));
        entry.insert("media".to_owned(), Value::Array(media));
        if let Some(first) = pictures.first() {
            entry.insert("image".to_owned(), Value::from(first.clone()));
            entry.insert("images".to_owned(), Value::Array(pictures.iter().cloned().map(Value::from).collect()));
        }
        entry.insert("user".to_owned(), Value::Object(user));
        entry.insert("caption".to_owned(), Value::from(post.caption.clone()));
        entry.insert("comment_count".to_owned(), Value::from(post.comment_count));
        entry.insert("like_count".to_owned(), post.like_count.map_or(Value::Null, Value::from));
        if let Some(taken_at) = post.taken_at.and_then(text::iso_time_millis) {
            entry.insert("taken_at".to_owned(), Value::from(taken_at));
        }
        if let Some(alt) = post.alt.as_deref().filter(|alt| !alt.is_empty()) {
            entry.insert("alt".to_owned(), Value::from(alt));
        }
        if let Some(location) = post.location.as_deref().filter(|location| !location.is_empty()) {
            entry.insert("location".to_owned(), Value::from(location));
        }
        if !post.comments.is_empty() {
            let comments = post
                .comments
                .iter()
                .map(|comment| {
                    let mut object = Map::new();
                    object.insert("username".to_owned(), Value::from(comment.username.clone()));
                    object.insert("text".to_owned(), Value::from(comment.text.clone()));
                    object.insert("like_count".to_owned(), Value::from(comment.likes));
                    if let Some(taken_at) = comment.taken_at.and_then(text::iso_time_millis) {
                        object.insert("taken_at".to_owned(), Value::from(taken_at));
                    }
                    Value::Object(object)
                })
                .collect();
            entry.insert("comments".to_owned(), Value::Array(comments));
        }
        Some(entry)
    }

    /// The `imports` of the JSON: one record per source (the browser imports folder gives its
    /// own history), one for loose pictures, none when nothing is open.
    fn imports_json(&self) -> Vec<Value> {
        let record_id = |text: &str| format!("{:016x}{:016x}", text::fnv1a_64(text.as_bytes()), text::fnv1a_64(format!("#{text}").as_bytes()));
        match self.kind {
            SnapshotKind::Empty => Vec::new(),
            SnapshotKind::Loose => {
                let mut record = Map::new();
                record.insert("id".to_owned(), Value::from(record_id(&format!("loose|{}", self.label))));
                record.insert("source_name".to_owned(), Value::from(self.label.clone()));
                record.insert("source_kind".to_owned(), Value::from("folder"));
                if let Some(imported_at) = self.updated_at.and_then(text::iso_time_millis) {
                    record.insert("imported_at".to_owned(), Value::from(imported_at));
                }
                record.insert("posts".to_owned(), Value::from(self.posts.len()));
                record.insert("new".to_owned(), Value::from(self.posts.len()));
                record.insert("updated".to_owned(), Value::from(0));
                vec![Value::Object(record)]
            }
            SnapshotKind::Exports { .. } => {
                let mut records = Vec::new();
                for source in &self.sources {
                    if source.kind == ExportKind::WebLibrary {
                        records.extend(library::imports_of(&source.manifest).into_iter().map(Value::Object));
                        continue;
                    }
                    let mut record = Map::new();
                    record.insert("id".to_owned(), Value::from(record_id(&fold_path(&source.path))));
                    record.insert("source_name".to_owned(), Value::from(source.name.clone()));
                    record.insert("source_kind".to_owned(), Value::from(if source.kind == ExportKind::Archive { "archive" } else { "folder" }));
                    if let Some(imported_at) = text::iso_time_millis(source.opened_at) {
                        record.insert("imported_at".to_owned(), Value::from(imported_at));
                    }
                    record.insert("posts".to_owned(), Value::from(source.shown));
                    record.insert("new".to_owned(), Value::from(source.shown.saturating_sub(source.replaced)));
                    record.insert("updated".to_owned(), Value::from(source.replaced));
                    if let Some(exported_at) = source.exported_at.and_then(text::iso_time_millis) {
                        record.insert("exported_at".to_owned(), Value::from(exported_at));
                    }
                    if let Some(source_url) = &source.source_url {
                        record.insert("source_url".to_owned(), Value::from(source_url.clone()));
                    }
                    records.push(Value::Object(record));
                }
                records
            }
        }
    }
}
