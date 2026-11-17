//! The browser pages over real sockets (web brief 4.15): every route, the guard, CORS, the HTTP
//! limits, uploads into the browser imports folder, the static pages, stopping and moving the
//! server. Each test starts its own server on `127.0.0.1:0` (never a fixed port) with a fake app
//! (`FakeHost`) that holds a collection opened from the fixtures and answers the changes the
//! way the app's UI thread does. Every file a test writes lies below the system's temporary
//! folder.

use std::collections::HashMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{IpAddr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use instagram_core::catalog::{self, OpenContext, OpenOptions, OpenRequest};
use instagram_core::library::{Library, LibraryLimits};
use instagram_core::manifest;
use instagram_core::model::Collection;
use instagram_core::store::{self, DescriptionStore};
use instagram_core::test_folder::TestFolder;
use instagram_core::web::{self, ApiError, FormatCache, PortSearch, Server, ServerConfig, Snapshot, WebCommandKind, WebFile, WebHost};
use serde_json::Value;

// --- The fake app ---------------------------------------------------------------------------------

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

/// A folder of its own below the system's temporary folder, removed when dropped
/// (`test_folder`).
struct TempFolder(TestFolder);

impl TempFolder {
    fn new(name: &str) -> TempFolder {
        TempFolder(TestFolder::new(&format!("web-{name}")).expect("a test folder"))
    }
}

/// What the UI thread of the app does with the browser's changes, without a window.
struct FakeHost {
    collection: Mutex<Option<Collection>>,
    store: Mutex<DescriptionStore>,
    snapshot: Mutex<Arc<Snapshot>>,
    revision: AtomicU64,
    commands: Mutex<Vec<&'static str>>,
    library_dir: PathBuf,
    formats: Arc<FormatCache>,
}

impl FakeHost {
    fn new(collection: Option<Collection>, folder: &Path) -> Arc<FakeHost> {
        let library_dir = folder.join("library");
        let formats = Arc::new(FormatCache::default());
        let first = Arc::new(Snapshot::new(1, None, collection.as_ref(), &library_dir, Arc::clone(&formats)));
        Arc::new(FakeHost {
            collection: Mutex::new(collection),
            store: Mutex::new(DescriptionStore::new(folder.join("descriptions.json"))),
            snapshot: Mutex::new(first),
            revision: AtomicU64::new(1),
            commands: Mutex::new(Vec::new()),
            library_dir,
            formats,
        })
    }

    fn republish(&self) -> Arc<Snapshot> {
        let revision = self.revision.fetch_add(1, Ordering::SeqCst) + 1;
        let collection = self.collection.lock().expect("collection");
        let snapshot = Arc::new(Snapshot::new(revision, Some(1_800_000_000_000), collection.as_ref(), &self.library_dir, Arc::clone(&self.formats)));
        *self.snapshot.lock().expect("snapshot") = Arc::clone(&snapshot);
        snapshot
    }

    fn commands(&self) -> Vec<&'static str> {
        self.commands.lock().expect("commands").clone()
    }
}

impl WebHost for FakeHost {
    fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.snapshot.lock().expect("snapshot"))
    }

    fn send(&self, kind: WebCommandKind) -> Result<Arc<Snapshot>, ApiError> {
        match kind {
            WebCommandKind::SetCaption { key, caption } => {
                self.commands.lock().expect("commands").push("SetCaption");
                let mut collection = self.collection.lock().expect("collection");
                let post = collection.as_mut().and_then(|collection| collection.post_mut(&key)).ok_or_else(|| ApiError::new(404, "gone"))?;
                store::save_clean_caption(&mut self.store.lock().expect("store"), post, caption, 1_800_000_000_000).map_err(|error| ApiError::new(503, error.message))?;
            }
            WebCommandKind::ShowWebLibrary { loaded, .. } => {
                self.commands.lock().expect("commands").push("ShowWebLibrary");
                let mut collection = self.collection.lock().expect("collection");
                let store = self.store.lock().expect("store");
                match (collection.as_mut(), loaded) {
                    (None, Some(loaded)) => *collection = Some(catalog::collection_of_source(*loaded, &store)),
                    (Some(open), Some(loaded)) => {
                        catalog::add_loaded_source(open, *loaded, &store);
                    }
                    (_, None) => *collection = None,
                }
            }
            WebCommandKind::CloseWebLibrary => {
                self.commands.lock().expect("commands").push("CloseWebLibrary");
                *self.collection.lock().expect("collection") = None;
            }
        }
        Ok(self.republish())
    }
}

/// The pages a test server serves.
static ASSETS: &[WebFile] = &[
    WebFile { path: "index.html", content_type: "text/html; charset=utf-8", etag: "\"00000000000000a1\"", immutable: false, bytes: b"<!doctype html><title>Instagram</title><script src=\"/assets/app-1.js\"></script>" },
    WebFile { path: "assets/app-1.js", content_type: "text/javascript; charset=utf-8", etag: "\"00000000000000b2\"", immutable: true, bytes: b"console.log('app');" },
    WebFile { path: "licenses/README.txt", content_type: "text/plain; charset=utf-8", etag: "\"00000000000000c3\"", immutable: false, bytes: b"Third-party software" },
];

/// A collection opened from `paths` of the fixtures.
fn open(paths: &[PathBuf], folder: &Path) -> Collection {
    let request = OpenRequest { paths: paths.to_vec(), options: OpenOptions { default_username: "tester".to_owned() } };
    let context = OpenContext { archive_cache: instagram_core::archive::cache::ArchiveCache::new(folder.join("archives")), library_dir: Some(folder.join("library")) };
    let opened = catalog::open_paths(&request, &DescriptionStore::new(folder.join("unused.json")), &context).expect("the fixture opens");
    // The leases must outlive the test: the cache is the test's own folder.
    std::mem::forget(opened.leases);
    opened.collection
}

/// A running test server.
struct Running {
    server: Option<Server>,
    host: Arc<FakeHost>,
    library: Arc<Library>,
    frontend: SocketAddr,
    backend: SocketAddr,
}

impl Running {
    fn start(collection: Option<Collection>, folder: &Path) -> Running {
        Running::start_with(collection, folder, |_| {}, LibraryLimits::default())
    }

    fn start_with(collection: Option<Collection>, folder: &Path, tune: impl FnOnce(&mut ServerConfig), limits: LibraryLimits) -> Running {
        let host = FakeHost::new(collection, folder);
        let library = Arc::new(Library::with_limits(folder.join("library"), limits, |_| Some(500 * 1024 * 1024 * 1024)));
        let mut config = ServerConfig::new("127.0.0.1".parse().expect("address"), 0, 0, ASSETS, folder.join("web-cache"));
        config.machine_names = vec!["desk-01".to_owned()];
        tune(&mut config);
        let server = Server::start(&config, Arc::clone(&host) as Arc<dyn WebHost>, Arc::clone(&library)).expect("the server starts");
        let (frontend, backend) = (server.frontend_address(), server.backend_address());
        Running { server: Some(server), host, library, frontend, backend }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(server) = self.server.take() {
            server.stop(Duration::from_millis(web::WEB_STOP_WAIT_MS));
        }
        self.library.close();
    }
}

/// One answer as the client sees it.
#[derive(Debug)]
struct Answer {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl Answer {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.get(name).map(String::as_str)
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.body).unwrap_or_else(|error| panic!("not JSON ({error}): {}", String::from_utf8_lossy(&self.body)))
    }

    fn detail(&self) -> String {
        self.json().get("detail").and_then(Value::as_str).unwrap_or_default().to_owned()
    }
}

/// Reads one answer from `stream` (the body by its Content-Length; none for HEAD).
fn read_answer(stream: &mut TcpStream, head_request: bool) -> Answer {
    let mut bytes = Vec::new();
    let mut byte = [0_u8; 1];
    while !bytes.ends_with(b"\r\n\r\n") {
        match stream.read(&mut byte) {
            Ok(1) => bytes.push(byte[0]),
            _ => panic!("the connection ended inside the head: {}", String::from_utf8_lossy(&bytes)),
        }
    }
    let head = String::from_utf8_lossy(&bytes).into_owned();
    let mut lines = head.split("\r\n");
    let status = lines.next().and_then(|line| line.split(' ').nth(1)).and_then(|code| code.parse().ok()).expect("a status line");
    let headers: HashMap<String, String> = lines.filter_map(|line| line.split_once(':')).map(|(name, value)| (name.trim().to_ascii_lowercase(), value.trim().to_owned())).collect();
    let length: usize = headers.get("content-length").and_then(|value| value.parse().ok()).unwrap_or(0);
    let mut body = vec![0_u8; if head_request { 0 } else { length }];
    stream.read_exact(&mut body).expect("the body");
    Answer { status, headers, body }
}

/// Sends `head` (with `Connection: close` added) and `body`, and reads the answer.
fn send(address: SocketAddr, head: &str, body: &[u8]) -> Answer {
    let mut stream = TcpStream::connect(address).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(20))).expect("timeout");
    let request = format!("{head}\r\nConnection: close\r\n\r\n");
    stream.write_all(request.as_bytes()).expect("send the head");
    stream.write_all(body).expect("send the body");
    read_answer(&mut stream, head.starts_with("HEAD "))
}

fn get(address: SocketAddr, path: &str) -> Answer {
    send(address, &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1:{}", address.port()), b"")
}

/// A write from the app's own page (`Sec-Fetch-Site: same-origin`).
fn write(address: SocketAddr, method: &str, path: &str, content_type: &str, body: &[u8]) -> Answer {
    send(address, &format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nSec-Fetch-Site: same-origin\r\nContent-Type: {content_type}\r\nContent-Length: {}", address.port(), body.len()), body)
}

fn export_basic() -> PathBuf {
    fixtures().join("export-basic")
}

// --- Reading ----------------------------------------------------------------------------------------

#[test]
fn health_and_the_library_json_of_an_export() {
    let folder = TempFolder::new("library-json");
    let running = Running::start(Some(open(&[export_basic()], &folder.0)), &folder.0);
    let health = get(running.frontend, "/api/health");
    assert_eq!((health.status, health.json()), (200, serde_json::json!({"status": "ok"})));
    assert_eq!(health.header("content-type"), Some("application/json"));
    assert_eq!(health.header("cache-control"), Some("no-store"));
    assert_eq!(health.header("x-content-type-options"), Some("nosniff"));
    assert!(health.header("server").is_some_and(|server| server.starts_with("instagram-windows-app/")));
    // HEAD of a GET route: the same head, no body.
    assert_eq!(send(running.backend, &format!("HEAD /api/health HTTP/1.1\r\nHost: 127.0.0.1:{}", running.backend.port()), b"").status, 200);

    let library = get(running.backend, "/api/library").json();
    let keys: Vec<&str> = library.as_object().expect("an object").keys().map(String::as_str).collect();
    assert_eq!(keys, ["directory", "revision", "updated_at", "imports", "posts"]);
    // The pages say that imports are kept and saved in `directory`, and Clear names it: that is
    // the browser imports folder, never the user's own export.
    assert_eq!(library["directory"], Value::from(folder.0.join("library").display().to_string()), "a local client sees where imports are saved");
    assert_eq!(library["revision"], Value::from(1));
    assert_eq!(library["imports"][0]["source_name"], Value::from("export-basic"));
    assert_eq!(library["imports"][0]["source_kind"], Value::from("folder"));
    assert_eq!(library["imports"][0]["posts"], Value::from(6));
    let posts = library["posts"].as_array().expect("posts");
    assert_eq!(posts.len(), 6);
    let first = posts[0].as_object().expect("a post");
    let keys: Vec<&str> = first.keys().map(String::as_str).collect();
    assert_eq!(keys, ["id", "url", "media_type", "media", "image", "images", "user", "caption", "comment_count", "like_count", "taken_at", "alt", "location", "comments"]);
    let path = first["media"][0]["path"].as_str().expect("a path");
    assert!(path.starts_with("posts/FXTbasic001-") && path.ends_with("_1.jpg"), "{path}");
    assert_eq!(first["media"][0]["mime"], Value::from("image/jpeg"));
    assert_eq!(first["media"][0]["bytes"], Value::from(26_284));
    assert!(first["user"]["avatar"].as_str().is_some_and(|avatar| avatar.starts_with("avatars/fixture_sunrise_bakery-") && avatar.ends_with(".jpg")));
    let carousel = posts.iter().find(|post| post["id"] == "FXTbasic005").expect("the carousel");
    assert_eq!(carousel["media_type"], Value::from("carousel"));
    assert_eq!(carousel["images"].as_array().map(Vec::len), Some(2));
    // The HEIC picture is listed as a JPEG without a size (it is converted when it is fetched).
    let heic = posts.iter().find(|post| post["id"] == "FXTbasic004").expect("the HEIC post");
    assert_eq!(heic["media"][0]["mime"], Value::from("image/jpeg"));
    assert!(heic["media"][0].get("bytes").is_none());
    assert!(heic["media"][0]["path"].as_str().is_some_and(|path| path.ends_with(".jpg")));
}

#[test]
fn virtual_paths_stay_the_same_across_snapshots() {
    let folder = TempFolder::new("stable-paths");
    let collection = open(&[export_basic()], &folder.0);
    let formats = Arc::new(FormatCache::default());
    let library_dir = folder.0.join("library");
    let paths = |snapshot: &Snapshot| -> Vec<String> {
        let json: Value = serde_json::from_slice(&snapshot.body(true)).expect("JSON");
        json["posts"].as_array().expect("posts").iter().flat_map(|post| post["media"].as_array().cloned().unwrap_or_default()).map(|media| media["path"].as_str().unwrap_or_default().to_owned()).collect()
    };
    let one = Snapshot::new(1, None, Some(&collection), &library_dir, Arc::clone(&formats));
    let mut reordered = collection.clone();
    reordered.posts.reverse();
    let two = Snapshot::new(2, None, Some(&reordered), &library_dir, formats);
    let mut first = paths(&one);
    let mut second = paths(&two);
    first.sort();
    second.sort();
    assert_eq!(first, second, "a post keeps its file names when the order changes");
    assert!(one.built().files.len() >= 9, "media and avatars are in the file map");
}

#[test]
fn other_computers_are_not_told_the_full_path() {
    let folder = TempFolder::new("remote");
    let collection = open(&[export_basic()], &folder.0);
    let snapshot = Snapshot::new(3, None, Some(&collection), &folder.0.join("library"), Arc::new(FormatCache::default()));
    let remote: Value = serde_json::from_slice(&snapshot.body(false)).expect("JSON");
    assert_eq!(remote["directory"], Value::from("library on the computer running Instagram Windows App"));
    let loose = Snapshot::new(3, None, Some(&open(&[fixtures().join("loose")], &folder.0)), &folder.0.join("library"), Arc::new(FormatCache::default()));
    let remote: Value = serde_json::from_slice(&loose.body(false)).expect("JSON");
    assert_eq!(remote["directory"], Value::from("loose on the computer running Instagram Windows App"), "the name of the open pictures' folder");
    let empty = Snapshot::new(1, None, None, &folder.0.join("library"), Arc::new(FormatCache::default()));
    let local: Value = serde_json::from_slice(&empty.body(true)).expect("JSON");
    assert_eq!(local["directory"], Value::from(folder.0.join("library").display().to_string()), "nothing open: where a browser import is saved");
    assert_eq!(local["updated_at"], Value::Null);
    assert_eq!(local["posts"], serde_json::json!([]));
    assert_eq!(local["imports"], serde_json::json!([]));
}

#[test]
fn loose_pictures_and_a_merged_collection_in_the_json() {
    let folder = TempFolder::new("loose-merged");
    let loose = open(&[fixtures().join("loose")], &folder.0);
    let snapshot = Snapshot::new(1, Some(1_800_000_000_000), Some(&loose), &folder.0.join("library"), Arc::new(FormatCache::default()));
    let json: Value = serde_json::from_slice(&snapshot.body(true)).expect("JSON");
    let post = &json["posts"][0];
    assert_eq!(post["user"], serde_json::json!({"username": "tester"}));
    assert_eq!(post["like_count"], Value::Null);
    assert_eq!(post["comment_count"], Value::from(0));
    assert!(post.get("url").is_none() && post.get("location").is_none() && post.get("comments").is_none());
    assert!(post.get("taken_at").is_some(), "the time of the file");
    assert_eq!(json["imports"][0]["source_kind"], Value::from("folder"));
    assert_eq!(json["imports"][0]["posts"], Value::from(json["posts"].as_array().map(Vec::len).unwrap_or(0)));
    assert_eq!(json["directory"], Value::from(fixtures().join("loose").display().to_string()), "the banner names the pictures' folder (imports are refused)");

    // The author is the Windows account name unless settings.json names one: other computers
    // are told "unknown" instead, with the same file names.
    let hiding = Snapshot::new(1, Some(1_800_000_000_000), Some(&loose), &folder.0.join("library"), Arc::new(FormatCache::default())).with_remote_loose_author("unknown");
    let local: Value = serde_json::from_slice(&hiding.body(true)).expect("JSON");
    let remote: Value = serde_json::from_slice(&hiding.body(false)).expect("JSON");
    let authors = |json: &Value| json["posts"].as_array().expect("posts").iter().map(|post| post["user"]["username"].as_str().unwrap_or_default().to_owned()).collect::<Vec<_>>();
    assert!(authors(&local).iter().all(|name| name == "tester") && !authors(&local).is_empty());
    assert!(authors(&remote).iter().all(|name| name == "unknown") && authors(&remote).len() == authors(&local).len());
    assert_eq!(local["posts"].as_array().map(|posts| posts.iter().map(|post| post["media"].clone()).collect::<Vec<_>>()), remote["posts"].as_array().map(|posts| posts.iter().map(|post| post["media"].clone()).collect::<Vec<_>>()));
    // Posts of an export keep their authors.
    let export = Snapshot::new(1, None, Some(&open(&[export_basic()], &folder.0)), &folder.0.join("library"), Arc::new(FormatCache::default())).with_remote_loose_author("unknown");
    let remote: Value = serde_json::from_slice(&export.body(false)).expect("JSON");
    assert!(authors(&remote).iter().all(|name| name != "unknown"));

    let merged = open(&[fixtures().join("archives").join("export-part1.zip"), fixtures().join("archives").join("export-part2.zip")], &folder.0);
    let snapshot = Snapshot::new(1, None, Some(&merged), &folder.0.join("library"), Arc::new(FormatCache::default()));
    let json: Value = serde_json::from_slice(&snapshot.body(true)).expect("JSON");
    let imports = json["imports"].as_array().expect("imports");
    assert_eq!(imports.len(), 2);
    assert!(imports.iter().all(|record| record["source_kind"] == "archive"));
    assert_eq!(json["posts"].as_array().map(Vec::len), Some(merged.posts.len()));
    assert_eq!(json["directory"], Value::from(folder.0.join("library").display().to_string()), "never one of the user's archives");
}

#[test]
fn a_video_with_its_poster_is_listed_with_both() {
    let folder = TempFolder::new("video");
    let collection = open(&[fixtures().join("archives").join("export-videos.zip")], &folder.0);
    let snapshot = Snapshot::new(1, None, Some(&collection), &folder.0.join("library"), Arc::new(FormatCache::default()));
    let json: Value = serde_json::from_slice(&snapshot.body(true)).expect("JSON");
    let with_poster = json["posts"].as_array().expect("posts").iter().flat_map(|post| post["media"].as_array().cloned().unwrap_or_default()).find(|media| media["type"] == "video" && media.get("poster").is_some()).expect("a video with a poster");
    let keys: Vec<&str> = with_poster.as_object().expect("an object").keys().map(String::as_str).collect();
    assert_eq!(keys[..3], ["type", "path", "poster"]);
    assert!(with_poster["poster"].as_str().is_some_and(|poster| poster.contains("_poster.")));
    assert!(with_poster["mime"].as_str().is_some_and(|mime| mime.starts_with("video/")));
}

#[test]
fn a_loose_video_is_listed_with_a_poster_made_on_request_and_an_export_video_without_one_has_none() {
    let folder = TempFolder::new("loose-video");
    let videos = folder.0.join("videos");
    fs::create_dir_all(&videos).expect("folder");
    fs::copy(fixtures().join("loose").join("IMG_2.jpg"), videos.join("a.jpg")).expect("a picture");
    let mp4 = b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom and the rest of a video";
    fs::write(videos.join("b.mp4"), mp4).expect("a video");
    let loose = open(std::slice::from_ref(&videos), &folder.0);
    let snapshot = Snapshot::new(1, None, Some(&loose), &folder.0.join("library"), Arc::new(FormatCache::default()));
    let json: Value = serde_json::from_slice(&snapshot.body(true)).expect("JSON");
    let post = json["posts"].as_array().expect("posts").iter().find(|post| post["media_type"] == "video").expect("the video post").clone();
    let media = &post["media"][0];
    let keys: Vec<&str> = media.as_object().expect("an object").keys().map(String::as_str).collect();
    assert_eq!(keys, ["type", "path", "poster", "mime", "bytes"]);
    let path = media["path"].as_str().expect("a path");
    let poster = media["poster"].as_str().expect("a poster");
    assert!(path.starts_with("posts/b-") && path.ends_with("_1.mp4"), "{path}");
    assert_eq!(poster, path.replace("_1.mp4", "_1_poster.jpg"));
    assert_eq!((media["mime"].as_str(), media["bytes"].as_u64()), (Some("video/mp4"), Some(mp4.len() as u64)));
    assert_eq!(post["image"].as_str(), Some(poster), "the poster is the picture of the grids");
    assert_eq!(post["images"], serde_json::json!([poster]));
    // The poster is a file of the snapshot: the video, served as the JPEG made from it.
    let file = snapshot.built().files.get(poster).cloned().expect("the poster is in the file map");
    assert!(file.video_poster && !file.heic);
    assert_eq!((file.path.as_path(), file.mime), (videos.join("b.mp4").as_path(), "image/jpeg"));

    // A video of an export without a poster gets none (as library.py), nor an image.
    let collection = open(&[fixtures().join("archives").join("export-videos.zip")], &folder.0);
    let snapshot = Snapshot::new(1, None, Some(&collection), &folder.0.join("library"), Arc::new(FormatCache::default()));
    let json: Value = serde_json::from_slice(&snapshot.body(true)).expect("JSON");
    let bare = json["posts"].as_array().expect("posts").iter().flat_map(|post| post["media"].as_array().cloned().unwrap_or_default()).find(|media| media["type"] == "video" && media.get("poster").is_none()).expect("a video without a poster");
    let keys: Vec<&str> = bare.as_object().expect("an object").keys().map(String::as_str).collect();
    assert_eq!(keys[..2], ["type", "path"]);
    assert!(!keys.contains(&"poster"));
}

#[cfg(windows)]
#[test]
fn the_poster_of_a_loose_video_is_made_when_a_browser_asks() {
    use instagram_core::poster::fixture::{self, FixtureSpec};
    let folder = TempFolder::new("poster-route");
    let videos = folder.0.join("videos");
    fs::create_dir_all(&videos).expect("folder");
    fixture::write_mp4(&videos.join("clip.mp4"), FixtureSpec::SHORT).expect("the sink writer makes a video");
    let posters = folder.0.join("posters");
    let running = Running::start_with(Some(open(std::slice::from_ref(&videos), &folder.0)), &folder.0, |config| config.poster_cache_dir = Some(posters.clone()), LibraryLimits::default());
    let library = get(running.frontend, "/api/library").json();
    let poster = library["posts"][0]["media"][0]["poster"].as_str().expect("a poster").to_owned();
    let answer = get(running.frontend, &format!("/api/library/files/{poster}"));
    assert_eq!((answer.status, answer.header("content-type")), (200, Some("image/jpeg")));
    assert!(answer.body.starts_with(&[0xFF, 0xD8, 0xFF]), "a JPEG");
    assert_eq!(fs::read_dir(folder.0.join("posters")).expect("the poster cache").count(), 1);
    assert_eq!(fs::read_dir(&videos).expect("listing").count(), 1, "nothing beside the video");
    // A video's own bytes come in ranges.
    let path = library["posts"][0]["media"][0]["path"].as_str().expect("a path").to_owned();
    let part = send(running.frontend, &format!("GET /api/library/files/{path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nRange: bytes=0-99", running.frontend.port()), b"");
    assert_eq!((part.status, part.body.len()), (206, 100));
    assert_eq!(part.header("content-type"), Some("video/mp4"));
}

#[cfg(windows)]
#[test]
fn a_loose_video_without_a_poster_is_listed_without_one_from_the_first_page_load() {
    use instagram_core::poster::fixture::{self, FixtureSpec};
    let folder = TempFolder::new("poster-fails");
    let videos = folder.0.join("videos");
    fs::create_dir_all(&videos).expect("folder");
    // Starts like an MP4, but no decoder makes a frame of it (a damaged download) ...
    fs::write(videos.join("damaged.mp4"), b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom and nothing of a video after it").expect("a video");
    // ... beside a good one.
    fixture::write_mp4(&videos.join("good.mp4"), FixtureSpec::SHORT).expect("the sink writer makes a video");
    let posters = folder.0.join("posters");
    let running = Running::start_with(Some(open(std::slice::from_ref(&videos), &folder.0)), &folder.0, |config| config.poster_cache_dir = Some(posters.clone()), LibraryLimits::default());
    // The first page load already asks for nothing that cannot come: the posters were made
    // before the list was answered.
    let first = get(running.frontend, "/api/library").json();
    let post = |name: &str| first["posts"].as_array().expect("posts").iter().find(|post| post["media"][0]["path"].as_str().is_some_and(|path| path.starts_with(&format!("posts/{name}-")))).cloned().expect("the post");
    let damaged = post("damaged");
    assert_eq!(damaged["media_type"], Value::from("video"));
    assert!(damaged["media"][0].get("poster").is_none() && damaged.get("image").is_none() && damaged.get("images").is_none(), "{damaged}");
    let good = post("good");
    let poster = good["media"][0]["poster"].as_str().expect("the good video keeps its poster").to_owned();
    assert_eq!(fs::read_dir(&posters).expect("the poster cache").count(), 1, "made ahead of the list");
    let served = get(running.frontend, &format!("/api/library/files/{poster}"));
    assert_eq!((served.status, served.header("content-type")), (200, Some("image/jpeg")));
    // Later loads list the same.
    let again = get(running.frontend, "/api/library").json();
    assert_eq!(again["posts"], first["posts"]);
    assert_eq!(again["revision"], first["revision"], "nothing that is open changed");
}

#[test]
fn files_are_served_with_ranges_etags_and_revalidation() {
    let folder = TempFolder::new("files");
    let running = Running::start(Some(open(&[export_basic()], &folder.0)), &folder.0);
    let library = get(running.frontend, "/api/library").json();
    let path = library["posts"][0]["media"][0]["path"].as_str().expect("a path").to_owned();
    let url = format!("/api/library/files/{path}?r=1");
    let host = format!("Host: 127.0.0.1:{}", running.frontend.port());
    let whole = get(running.frontend, &url);
    assert_eq!((whole.status, whole.body.len()), (200, 26_284));
    assert_eq!(whole.body, fs::read(export_basic().join("posts").join("post1.jpg")).expect("the fixture"));
    assert_eq!(whole.header("content-type"), Some("image/jpeg"));
    assert_eq!(whole.header("accept-ranges"), Some("bytes"));
    assert_eq!(whole.header("cache-control"), Some("no-cache"));
    assert_eq!(whole.header("cross-origin-resource-policy"), Some("same-site"), "another web site cannot embed the pictures");
    assert!(whole.header("last-modified").is_some_and(|date| date.ends_with(" GMT")));
    let etag = whole.header("etag").expect("an ETag").to_owned();
    let head = send(running.frontend, &format!("HEAD {url} HTTP/1.1\r\n{host}"), b"");
    assert_eq!((head.status, head.header("content-length")), (200, Some("26284")));
    let part = send(running.frontend, &format!("GET {url} HTTP/1.1\r\n{host}\r\nRange: bytes=10-19"), b"");
    assert_eq!((part.status, part.body.len()), (206, 10));
    assert_eq!(part.header("content-range"), Some("bytes 10-19/26284"));
    assert_eq!(part.body, whole.body[10..20]);
    let suffix = send(running.frontend, &format!("GET {url} HTTP/1.1\r\n{host}\r\nRange: bytes=-4"), b"");
    assert_eq!((suffix.status, suffix.body.as_slice()), (206, &whole.body[26_280..]));
    let beyond = send(running.frontend, &format!("GET {url} HTTP/1.1\r\n{host}\r\nRange: bytes=99999-"), b"");
    assert_eq!((beyond.status, beyond.header("content-range")), (416, Some("bytes */26284")));
    let several = send(running.frontend, &format!("GET {url} HTTP/1.1\r\n{host}\r\nRange: bytes=0-1,4-5"), b"");
    assert_eq!((several.status, several.body.len()), (200, 26_284));
    let stale = send(running.frontend, &format!("GET {url} HTTP/1.1\r\n{host}\r\nRange: bytes=0-1\r\nIf-Range: \"other\""), b"");
    assert_eq!(stale.status, 200, "If-Range that does not match gives the whole file");
    let cached = send(running.frontend, &format!("GET {url} HTTP/1.1\r\n{host}\r\nIf-None-Match: {etag}"), b"");
    assert_eq!((cached.status, cached.body.len()), (304, 0));
    // The HEIC picture comes as a JPEG made of it, or as 404 with the reason on a system
    // without a HEIF codec.
    let heic = library["posts"].as_array().expect("posts").iter().find(|post| post["id"] == "FXTbasic004").map(|post| post["media"][0]["path"].as_str().unwrap_or_default().to_owned()).expect("the HEIC post");
    let converted = get(running.frontend, &format!("/api/library/files/{heic}"));
    match converted.status {
        200 => assert!(converted.body.starts_with(&[0xFF, 0xD8, 0xFF]) && converted.header("content-type") == Some("image/jpeg")),
        404 => assert!(converted.detail().starts_with("This picture cannot be shown in a browser: ")),
        other => panic!("unexpected status {other} for the HEIC copy"),
    }
    assert_eq!(get(running.frontend, "/api/library/files/posts/not-there_1.jpg").detail(), "No such library file.");
}

#[test]
fn nothing_outside_the_file_map_is_ever_reachable() {
    let folder = TempFolder::new("traversal");
    let running = Running::start(Some(open(&[export_basic()], &folder.0)), &folder.0);
    for path in ["/api/library/files/posts/../../Windows/win.ini", "/api/library/files/%2e%2e/%2e%2e/Windows/win.ini", "/api/library/files/..%2f..%2fWindows%2fwin.ini", "/api/library/files/posts%5c..%5cpost.json", "/api/library/files/posts/a%00.jpg"] {
        let answer = get(running.frontend, path);
        assert_eq!((answer.status, answer.detail().as_str()), (400, "Invalid path"), "{path}");
    }
    for path in ["/api/library/files/C:/Windows/win.ini", "/api/library/files/posts/post1.jpg", "/api/library/files/post.json", &format!("/api/library/files/{}", export_basic().join("post.json").display().to_string().replace('\\', "/"))] {
        let answer = get(running.frontend, path);
        assert_eq!(answer.status, 404, "{path}");
    }
}

// --- The guard and CORS -----------------------------------------------------------------------------

#[test]
fn foreign_host_names_and_cross_site_writes_are_refused() {
    let folder = TempFolder::new("guard");
    let running = Running::start(Some(open(&[export_basic()], &folder.0)), &folder.0);
    let rebound = send(running.frontend, "GET /api/health HTTP/1.1\r\nHost: evil.example", b"");
    assert_eq!((rebound.status, rebound.detail().as_str()), (400, "This host name is not allowed. Open the app by IP address, localhost or this computer's name, or add the name to \"allowed_hosts\" in settings.json."));
    assert_eq!(send(running.backend, "GET / HTTP/1.1\r\nHost: evil.example:8317", b"").status, 400, "on both listeners");
    assert_eq!(send(running.frontend, "GET /api/health HTTP/1.1\r\nHost: desk-01:3127", b"").status, 200, "the computer's own name");
    assert_eq!(send(running.frontend, "GET /api/health HTTP/1.0", b"").status, 200, "HTTP/1.0 without Host");

    let body = br#"{"caption": "From another site"}"#;
    let cross = send(running.frontend, &format!("PATCH /api/library/posts/FXTbasic001 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: http://evil.example\r\nSec-Fetch-Site: cross-site\r\nContent-Type: application/json\r\nContent-Length: {}", running.frontend.port(), body.len()), body);
    assert_eq!((cross.status, cross.detail().as_str()), (403, "Changes are only accepted from the app's own pages."));
    let null_origin = send(running.backend, &format!("DELETE /api/library HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: null", running.backend.port()), b"");
    assert_eq!(null_origin.status, 403);
    assert!(running.host.commands().is_empty(), "the app was never asked");
}

#[test]
fn the_apps_own_pages_change_descriptions() {
    let folder = TempFolder::new("patch");
    let running = Running::start(Some(open(&[export_basic()], &folder.0)), &folder.0);
    let body = br#"{"caption": "  Edited in the browser\r\n  "}"#;
    let answer = write(running.frontend, "PATCH", "/api/library/posts/FXTbasic002", "application/json", body);
    assert_eq!(answer.status, 200);
    let library = answer.json();
    let edited = library["posts"].as_array().expect("posts").iter().find(|post| post["id"] == "FXTbasic002").expect("the post");
    assert_eq!(edited["caption"], Value::from("Edited in the browser"));
    assert_eq!(library["revision"], Value::from(2));
    // Origin equal to the address asked for (a LAN page has no Sec-Fetch-Site): accepted.
    let body = br#"{"caption": "Second edit"}"#;
    let own = send(running.frontend, &format!("PATCH /api/library/posts/FXTbasic002 HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nOrigin: http://127.0.0.1:{port}\r\nContent-Type: application/json\r\nContent-Length: {}", body.len(), port = running.frontend.port()), body);
    assert_eq!(own.status, 200);
    // A client that is not a browser sends neither header (curl): accepted.
    let curl = send(running.backend, &format!("PATCH /api/library/posts/FXTbasic003 HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nContent-Type: application/json\r\nContent-Length: 18", running.backend.port()), br#"{"caption": "cli"}"#);
    assert_eq!(curl.status, 200);
    assert_eq!(running.host.commands(), ["SetCaption", "SetCaption", "SetCaption"]);
    let saved = running.host.store.lock().expect("store").entries().map(|(_, entry)| entry.caption.clone()).collect::<Vec<_>>();
    assert!(saved.contains(&"Second edit".to_owned()) && saved.contains(&"cli".to_owned()), "{saved:?}");
    // The parser's refusals and an unknown id.
    assert_eq!(write(running.frontend, "PATCH", "/api/library/posts/FXTbasic002", "application/json", br#"{"alt": "x"}"#).detail(), "\"alt\" cannot be changed; only the caption can.");
    assert_eq!(write(running.frontend, "PATCH", "/api/library/posts/NOPE", "application/json", br#"{"caption": "x"}"#).detail(), "The library has no post with this id.");
}

#[test]
fn cors_is_answered_on_the_api_listener_for_the_page_origins_only() {
    let folder = TempFolder::new("cors");
    let running = Running::start(None, &folder.0);
    let page_origin = format!("http://127.0.0.1:{}", running.frontend.port());
    let preflight = send(running.backend, &format!("OPTIONS /api/library/posts/x HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: {page_origin}\r\nAccess-Control-Request-Method: PATCH\r\nAccess-Control-Request-Headers: content-type", running.backend.port()), b"");
    assert_eq!(preflight.status, 200);
    assert_eq!(preflight.header("access-control-allow-origin"), Some(page_origin.as_str()));
    assert_eq!(preflight.header("access-control-allow-methods"), Some("GET, POST, PUT, PATCH, DELETE"));
    assert_eq!(preflight.header("access-control-allow-headers"), Some("content-type"));
    assert_eq!(preflight.header("access-control-max-age"), Some("600"));
    let foreign = send(running.backend, &format!("OPTIONS /api/library HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: http://evil.example\r\nAccess-Control-Request-Method: DELETE", running.backend.port()), b"");
    assert_eq!((foreign.status, String::from_utf8_lossy(&foreign.body).into_owned()), (400, "Disallowed CORS origin".to_owned()));
    let method = send(running.backend, &format!("OPTIONS /api/library HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: {page_origin}\r\nAccess-Control-Request-Method: TRACE", running.backend.port()), b"");
    assert_eq!(String::from_utf8_lossy(&method.body), "Disallowed CORS method");
    let simple = send(running.backend, &format!("GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: {page_origin}", running.backend.port()), b"");
    assert_eq!((simple.header("access-control-allow-origin"), simple.header("vary")), (Some(page_origin.as_str()), Some("Origin")));
    let page = send(running.frontend, &format!("GET /api/health HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nOrigin: http://127.0.0.1:{}", running.frontend.port(), running.frontend.port()), b"");
    assert!(page.headers.keys().all(|name| !name.starts_with("access-control-")), "the page listener sends no CORS headers");
}

// --- HTTP limits -------------------------------------------------------------------------------------

#[test]
fn bodies_are_checked_before_they_are_read() {
    let folder = TempFolder::new("limits");
    let running = Running::start(Some(open(&[export_basic()], &folder.0)), &folder.0);
    // Only the head is sent: the answer comes without the 300,000 bytes.
    let mut stream = TcpStream::connect(running.frontend).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
    write!(stream, "PATCH /api/library/posts/FXTbasic001 HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nContent-Length: 300000\r\n\r\n").expect("the head");
    let answer = read_answer(&mut stream, false);
    assert_eq!((answer.status, answer.detail().as_str()), (413, "The request is larger than 256 KB."));
    assert_eq!(answer.header("connection"), Some("close"));
    let chunked = send(running.frontend, "POST /api/library/uploads HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nTransfer-Encoding: chunked", b"0\r\n\r\n");
    assert_eq!((chunked.status, chunked.detail().as_str()), (501, "Request bodies sent in chunks are not supported."));
    let upload = write(running.frontend, "POST", "/api/library/uploads", "application/json", b"");
    let id = upload.json()["upload_id"].as_str().expect("an id").to_owned();
    let no_length = send(running.frontend, &format!("PUT /api/library/uploads/{id}/files/m000001.jpg HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: same-origin"), b"");
    assert_eq!((no_length.status, no_length.detail().as_str()), (411, "A Content-Length header is required."));
    let huge = send(running.frontend, &format!("PUT /api/library/uploads/{id}/files/m000001.jpg HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nContent-Length: 5000000000"), b"");
    assert_eq!((huge.status, huge.detail().as_str()), (413, "A file can be at most 1 GB."));
    assert_eq!(get(running.frontend, "/api/nothing").detail(), "Not Found");
    let wrong = send(running.frontend, "PUT /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nContent-Length: 0", b"");
    assert_eq!((wrong.status, wrong.detail().as_str(), wrong.header("allow")), (405, "Method Not Allowed", Some("GET")));
    let malformed = send(running.frontend, "NOT HTTP", b"");
    assert_eq!((malformed.status, malformed.detail().as_str()), (400, "Malformed request"));
}

#[test]
fn a_slow_head_is_closed_after_its_timeout() {
    let folder = TempFolder::new("slow");
    let running = Running::start_with(None, &folder.0, |config| config.timeouts.head = Duration::from_millis(300), LibraryLimits::default());
    let mut stream = TcpStream::connect(running.frontend).expect("connect");
    stream.write_all(b"GET /api/health HT").expect("half a head");
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let started = Instant::now();
    let mut buffer = [0_u8; 16];
    let read = stream.read(&mut buffer).unwrap_or(0);
    assert_eq!(read, 0, "closed without an answer");
    assert!(started.elapsed() < Duration::from_secs(3), "closed after the head timeout, not the read timeout");
}

/// Opens a TCP connection to `to` from the loopback address `local` (127.0.0.2, ...): another
/// program on this computer. std::net cannot bind before it connects; WinSock can.
#[cfg(windows)]
fn connect_from(local: std::net::Ipv4Addr, to: SocketAddr) -> TcpStream {
    use std::os::windows::io::FromRawSocket;
    use windows::Win32::Networking::WinSock::{bind, closesocket, connect, socket, AF_INET, IN_ADDR, IN_ADDR_0, IPPROTO_TCP, SOCKADDR, SOCKADDR_IN, SOCK_STREAM};
    let SocketAddr::V4(to) = to else { panic!("the test servers listen on IPv4") };
    let address = |ip: std::net::Ipv4Addr, port: u16| SOCKADDR_IN { sin_family: AF_INET, sin_port: port.to_be(), sin_addr: IN_ADDR { S_un: IN_ADDR_0 { S_addr: u32::from_ne_bytes(ip.octets()) } }, sin_zero: [0; 8] };
    let length = std::mem::size_of::<SOCKADDR_IN>() as i32;
    // WinSock is started: the test server's own sockets came first.
    // SAFETY: plain WinSock calls on a socket this function owns; the addresses outlive the
    // calls; the socket is handed to TcpStream, which closes it, or closed here on failure.
    unsafe {
        let raw = socket(i32::from(AF_INET.0), SOCK_STREAM, IPPROTO_TCP.0).expect("a socket");
        let from = address(local, 0);
        let target = address(*to.ip(), to.port());
        if bind(raw, std::ptr::from_ref(&from).cast::<SOCKADDR>(), length) != 0 || connect(raw, std::ptr::from_ref(&target).cast::<SOCKADDR>(), length) != 0 {
            closesocket(raw);
            panic!("could not connect from {local} to {to}");
        }
        TcpStream::from_raw_socket(raw.0 as u64)
    }
}

/// A connection that works on a request: it sent the head of an edit of `length` bytes and holds
/// the body back (the server waits for it for the rate grace, 30 s by default).
fn busy(mut stream: TcpStream) -> TcpStream {
    write!(stream, "PATCH /api/library/posts/FXTbasic001 HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n").expect("the head");
    stream
}

/// Sends a GET on a connection that stays open; the answer.
fn get_on(stream: &mut TcpStream, path: &str) -> Answer {
    stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
    write!(stream, "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").expect("a request");
    read_answer(stream, false)
}

/// True when the server closed `stream` (an end of stream or a reset, no answer).
fn closed_by_server(stream: &mut TcpStream) -> bool {
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let mut byte = [0_u8; 1];
    match stream.read(&mut byte) {
        Ok(0) => true,
        Ok(_) => false,
        Err(error) => error.kind() != std::io::ErrorKind::WouldBlock && error.kind() != std::io::ErrorKind::TimedOut,
    }
}

#[test]
fn connections_beyond_the_limit_get_503_and_keep_alive_serves_several_requests() {
    let folder = TempFolder::new("connections");
    let running = Running::start_with(None, &folder.0, |config| config.max_connections = 2, LibraryLimits::default());
    // Two requests on one connection.
    let mut stream = TcpStream::connect(running.frontend).expect("connect");
    let first = get_on(&mut stream, "/api/health");
    assert_eq!((first.status, first.header("connection")), (200, Some("keep-alive")));
    assert_eq!(get_on(&mut stream, "/api/library").status, 200);
    // That connection and one more, both working on a request, are all the page listener
    // serves at once.
    let _first = busy(stream);
    let _second = busy(TcpStream::connect(running.frontend).expect("connect"));
    std::thread::sleep(Duration::from_millis(300));
    // The refused client has sent its request already: it still reads the 503 (the server
    // reads what came before it closes, else Windows would reset the connection).
    let mut third = TcpStream::connect(running.frontend).expect("connect");
    third.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
    third.write_all(b"GET /api/library HTTP/1.1\r\nHost: 127.0.0.1\r\nX-Padding: aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\r\n\r\n").expect("a request");
    std::thread::sleep(Duration::from_millis(50));
    let refused = read_answer(&mut third, false);
    assert_eq!((refused.status, refused.detail().as_str()), (503, "Too many connections. Try again in a moment."));
    // The API listener has places of its own: a full page listener does not take them.
    assert_eq!(get(running.backend, "/api/health").status, 200);
}

#[test]
fn one_client_gets_only_a_share_of_the_places() {
    let folder = TempFolder::new("per-peer");
    let running = Running::start_with(None, &folder.0, |config| config.max_connections_per_peer = 2, LibraryLimits::default());
    let _first = busy(TcpStream::connect(running.frontend).expect("connect"));
    let _second = busy(TcpStream::connect(running.frontend).expect("connect"));
    std::thread::sleep(Duration::from_millis(300));
    // The listener has 62 places left, but not for this client.
    assert_eq!(get(running.frontend, "/api/health").status, 503);
    assert_eq!(get(running.backend, "/api/health").status, 200, "each listener counts on its own");
}

#[cfg(windows)]
#[test]
fn every_loopback_address_is_one_client() {
    let folder = TempFolder::new("loopback-one-client");
    let running = Running::start_with(None, &folder.0, |config| config.max_connections_per_peer = 2, LibraryLimits::default());
    let _first = busy(connect_from("127.0.0.2".parse().expect("address"), running.frontend));
    let _second = busy(connect_from("127.0.0.3".parse().expect("address"), running.frontend));
    std::thread::sleep(Duration::from_millis(300));
    // Two more loopback addresses do not make two more clients: 127.0.0.1 has no place left.
    assert_eq!(get(running.frontend, "/api/health").status, 503);
    let mut fourth = connect_from("127.0.0.4".parse().expect("address"), running.frontend);
    fourth.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
    fourth.write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").expect("a request");
    assert_eq!(read_answer(&mut fourth, false).status, 503);
}

#[test]
fn a_new_request_takes_the_place_of_a_connection_that_only_waits() {
    let folder = TempFolder::new("reclaim");
    let running = Running::start_with(None, &folder.0, |config| config.max_connections_per_peer = 2, LibraryLimits::default());
    // A client holds its two places with keep-alive connections it does not use.
    let mut oldest = TcpStream::connect(running.frontend).expect("connect");
    assert_eq!(get_on(&mut oldest, "/api/health").status, 200);
    std::thread::sleep(Duration::from_millis(50));
    let mut newer = TcpStream::connect(running.frontend).expect("connect");
    assert_eq!(get_on(&mut newer, "/api/health").status, 200);
    // A browser of the same client gets in at once: the connection idle longest gives its place.
    assert_eq!(get(running.frontend, "/api/library").status, 200);
    assert!(closed_by_server(&mut oldest), "the connection idle longest was closed");
    assert_eq!(get_on(&mut newer, "/api/health").status, 200, "the other one still serves");
}

#[test]
fn requests_sent_right_behind_one_another_are_all_answered() {
    let folder = TempFolder::new("pipelined");
    let running = Running::start_with(None, &folder.0, |config| config.max_connections_per_peer = 1, LibraryLimits::default());
    let mut stream = TcpStream::connect(running.frontend).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
    stream.write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\nGET /api/library HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\nGET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").expect("three requests");
    let statuses: Vec<u16> = (0..3).map(|_| read_answer(&mut stream, false).status).collect();
    assert_eq!(statuses, [200, 200, 200]);
}

#[test]
fn connections_whose_request_has_not_arrived_take_no_place_and_are_not_reset() {
    let folder = TempFolder::new("reclaim-fresh");
    let running = Running::start_with(None, &folder.0, |config| config.max_connections_per_peer = 2, LibraryLimits::default());
    // Two connections that were just opened (their requests may be on their way) hold no place ...
    let mut first = TcpStream::connect(running.frontend).expect("connect");
    let mut second = TcpStream::connect(running.frontend).expect("connect");
    std::thread::sleep(Duration::from_millis(100));
    // ... so a request in the same moment is served, and the first two are not reset.
    assert_eq!(get(running.frontend, "/api/health").status, 200);
    assert_eq!(get_on(&mut first, "/api/health").status, 200, "the first one serves");
    assert_eq!(get_on(&mut second, "/api/health").status, 200, "the second one serves");
}

/// What a client of the attacks below sends: a GET of `/api/health` on `stream`; its status, or
/// `None` when the connection was closed (the client then opens a new one).
fn try_get_on(stream: &mut TcpStream) -> Option<u16> {
    stream.set_read_timeout(Some(Duration::from_secs(10))).ok()?;
    stream.write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").ok()?;
    let mut bytes = Vec::new();
    let mut chunk = [0_u8; 1024];
    while !bytes.ends_with(b"{\"status\":\"ok\"}") && !(bytes.starts_with(b"HTTP/1.1 503") && bytes.ends_with(b"}")) {
        let count = stream.read(&mut chunk).ok().filter(|count| *count > 0)?;
        bytes.extend_from_slice(&chunk[..count]);
    }
    String::from_utf8_lossy(&bytes).split(' ').nth(1).and_then(|code| code.parse().ok())
}

/// Requests of the user's browser (`get`, from 127.0.0.1) while an attack runs: their statuses.
fn probes(address: SocketAddr, count: usize, every: Duration) -> Vec<u16> {
    (0..count)
        .map(|_| {
            std::thread::sleep(every);
            get(address, "/api/health").status
        })
        .collect()
}

#[cfg(windows)]
#[test]
fn silent_connections_opened_again_and_again_never_keep_a_request_out() {
    // E2E3-1: another program on this computer (127.0.0.2: the same client as the user's
    // browser) keeps more connections open than the client has places, sends nothing on them,
    // and closes and opens each again faster than any time a waiting connection is kept for.
    // (Several hundred new connections a second can still close the browser's connections
    // before their requests arrive: README, Known limitations.)
    let folder = TempFolder::new("churn");
    let tune = |config: &mut ServerConfig| {
        config.max_connections_per_peer = 4;
        config.max_arriving_per_peer = 6;
    };
    let running = Running::start_with(None, &folder.0, tune, LibraryLimits::default());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let to = running.frontend;
    let attack = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            let mut open: std::collections::VecDeque<(Instant, TcpStream)> = std::collections::VecDeque::new();
            let mut opened = 0;
            while !stop.load(Ordering::SeqCst) {
                // Eight at a time, each closed after 300 ms (staggered) and opened again at once.
                while open.front().is_some_and(|(since, _)| since.elapsed() >= Duration::from_millis(300)) {
                    open.pop_front();
                }
                if open.len() < 8 {
                    open.push_back((Instant::now(), connect_from("127.0.0.2".parse().expect("address"), to)));
                    opened += 1;
                }
                std::thread::sleep(Duration::from_millis(30));
            }
            opened
        })
    };
    std::thread::sleep(Duration::from_millis(600));
    let statuses = probes(running.frontend, 12, Duration::from_millis(100));
    stop.store(true, Ordering::SeqCst);
    let opened = attack.join().expect("the attack");
    assert!(opened > 40, "the attack ran: {opened} connections");
    assert!(statuses.iter().all(|status| *status == 200), "the browser was served throughout: {statuses:?}");
}

#[test]
fn connections_used_more_often_than_they_wait_still_make_way_for_a_request() {
    // A client holds every place of its own with keep-alive connections it sends a small
    // request on every 150 ms, so none of them ever waits long: a request of another connection
    // still takes the place of one that waits.
    let folder = TempFolder::new("keep-alive-churn");
    let running = Running::start_with(None, &folder.0, |config| config.max_connections_per_peer = 4, LibraryLimits::default());
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let to = running.frontend;
    let holders: Vec<_> = (0..4)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut served = 0;
                let mut stream = TcpStream::connect(to).expect("connect");
                while !stop.load(Ordering::SeqCst) {
                    match try_get_on(&mut stream) {
                        Some(200) => served += 1,
                        // Closed for a request of the user, or refused: a new connection.
                        _ => stream = TcpStream::connect(to).expect("connect"),
                    }
                    std::thread::sleep(Duration::from_millis(150));
                }
                served
            })
        })
        .collect();
    std::thread::sleep(Duration::from_millis(500));
    let statuses = probes(running.frontend, 12, Duration::from_millis(100));
    stop.store(true, Ordering::SeqCst);
    let served: usize = holders.into_iter().map(|holder| holder.join().expect("a holder")).sum();
    assert!(served > 20, "the holders were served too: {served}");
    assert!(statuses.iter().all(|status| *status == 200), "the browser was served throughout: {statuses:?}");
}

#[test]
fn a_flood_of_silent_connections_closes_those_that_waited_longest() {
    let folder = TempFolder::new("arrival-flood");
    let running = Running::start_with(None, &folder.0, |config| config.max_arriving_per_peer = 4, LibraryLimits::default());
    // Six connections that send nothing; the client may have four that wait for their request.
    let mut silent: Vec<TcpStream> = (0..6)
        .map(|_| {
            let stream = TcpStream::connect(running.frontend).expect("connect");
            std::thread::sleep(Duration::from_millis(30));
            stream
        })
        .collect();
    std::thread::sleep(Duration::from_millis(100));
    let still_open = |stream: &mut TcpStream| {
        stream.set_read_timeout(Some(Duration::from_millis(300))).expect("timeout");
        let mut byte = [0_u8; 1];
        matches!(stream.read(&mut byte), Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
    };
    let open: Vec<bool> = silent.iter_mut().map(still_open).collect();
    assert_eq!(open, [false, false, true, true, true, true], "the two that waited longest were closed, at once");
    // A request still gets in: the oldest silent one makes way for it.
    assert_eq!(get(running.frontend, "/api/health").status, 200);
    assert!(!still_open(&mut silent[2]));
    // Their threads ended at once (a closed connection that kept waiting would still count).
    let deadline = Instant::now() + Duration::from_secs(2);
    while running.server.as_ref().is_some_and(|server| server.live_connections() != 3) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(running.server.as_ref().map(Server::live_connections), Some(3), "three wait, the others are gone");
}

#[test]
fn a_body_the_route_did_not_read_keeps_no_place() {
    // A client sends requests with a body announced and never sent: after the answer the server
    // waits for that body to throw it away, and meanwhile the place goes to a new request.
    let folder = TempFolder::new("drain-waits");
    let running = Running::start_with(None, &folder.0, |config| config.max_connections_per_peer = 2, LibraryLimits::default());
    let mut held: Vec<TcpStream> = (0..2)
        .map(|_| {
            let mut stream = TcpStream::connect(running.frontend).expect("connect");
            stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
            stream.write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 1000\r\n\r\n").expect("a request");
            assert_eq!(read_answer(&mut stream, false).status, 200);
            stream
        })
        .collect();
    assert_eq!(get(running.frontend, "/api/health").status, 200, "the place of one that waits for its body");
    assert!(held.iter_mut().any(closed_by_server), "that one was closed");
}

#[test]
fn a_connection_is_closed_after_its_life_and_its_answer() {
    let folder = TempFolder::new("life");
    let running = Running::start_with(None, &folder.0, |config| config.timeouts.connection_life = Duration::from_millis(300), LibraryLimits::default());
    let mut stream = TcpStream::connect(running.frontend).expect("connect");
    assert_eq!(get_on(&mut stream, "/api/health").header("connection"), Some("keep-alive"));
    std::thread::sleep(Duration::from_millis(400));
    let last = get_on(&mut stream, "/api/health");
    assert_eq!((last.status, last.header("connection")), (200, Some("close")), "served, then closed");
    assert!(closed_by_server(&mut stream));
}

#[test]
fn refused_clients_get_their_503_while_silent_ones_wait() {
    let folder = TempFolder::new("reject-storm");
    let running = Running::start_with(None, &folder.0, |config| config.max_connections = 1, LibraryLimits::default());
    let _working = busy(TcpStream::connect(running.frontend).expect("connect"));
    std::thread::sleep(Duration::from_millis(300));
    // Clients that open a connection and never send nor close (more than one client may have
    // waiting: the oldest are closed) ...
    let silent: Vec<TcpStream> = (0..40).map(|_| TcpStream::connect(running.frontend).expect("connect")).collect();
    // ... do not cost a real client its answer.
    let mut probe = TcpStream::connect(running.frontend).expect("connect");
    probe.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
    probe.write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").expect("a request");
    let refused = read_answer(&mut probe, false);
    assert_eq!((refused.status, refused.detail().as_str()), (503, "Too many connections. Try again in a moment."));
    drop(silent);
    // A burst of 60 requests at once (a page of many pictures): every one reads its 503, none is
    // reset unanswered.
    let burst: Vec<TcpStream> = (0..60)
        .map(|_| {
            let mut stream = TcpStream::connect(running.frontend).expect("connect");
            stream.write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").expect("a request");
            stream
        })
        .collect();
    for mut stream in burst {
        stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
        assert_eq!(read_answer(&mut stream, false).status, 503);
    }
}

#[test]
fn a_connection_that_sends_nothing_is_closed_soon() {
    let folder = TempFolder::new("silent");
    let running = Running::start_with(None, &folder.0, |config| config.timeouts.first_byte = Duration::from_millis(300), LibraryLimits::default());
    let mut stream = TcpStream::connect(running.frontend).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
    let started = Instant::now();
    let mut buffer = [0_u8; 16];
    assert_eq!(stream.read(&mut buffer).unwrap_or(0), 0, "closed without an answer");
    assert!(started.elapsed() < Duration::from_secs(5), "closed after the first-byte time, not the head time: {:?}", started.elapsed());
}

#[test]
fn a_body_that_trickles_is_cut_off() {
    let folder = TempFolder::new("trickle");
    let tune = |config: &mut ServerConfig| {
        config.timeouts.rate_grace = Duration::from_millis(500);
        config.timeouts.min_bytes_per_second = 1000;
    };
    let running = Running::start_with(Some(open(&[export_basic()], &folder.0)), &folder.0, tune, LibraryLimits::default());
    let mut stream = TcpStream::connect(running.frontend).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(20))).expect("timeout");
    write!(stream, "PATCH /api/library/posts/FXTbasic001 HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\nContent-Length: 200000\r\n\r\n").expect("the head");
    // One byte every 100 ms: each read comes in time, the body as a whole never would.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut writer = stream.try_clone().expect("a second handle");
    let trickle = {
        let stop = Arc::clone(&stop);
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) && writer.write_all(b" ").is_ok() {
                std::thread::sleep(Duration::from_millis(100));
            }
        })
    };
    let started = Instant::now();
    let answer = read_answer(&mut stream, false);
    let elapsed = started.elapsed();
    stop.store(true, Ordering::SeqCst);
    let _ = trickle.join();
    assert_eq!((answer.status, answer.detail().as_str()), (400, "The request body was cut off."));
    assert!(elapsed < Duration::from_secs(5), "cut off after the grace, not after 60 s per read: {elapsed:?}");
    assert!(running.host.commands().is_empty(), "the app was never asked");
}

#[test]
fn a_client_that_reads_too_slowly_loses_its_connection() {
    let folder = TempFolder::new("slow-reader");
    let pictures = folder.0.join("pictures");
    fs::create_dir_all(&pictures).expect("a folder");
    // A large "picture": the head of a JPEG and 32 MB after it (only the head is sniffed).
    let size: usize = 32 * 1024 * 1024;
    let mut bytes = fs::read(export_basic().join("posts").join("post1.jpg")).expect("a JPEG");
    bytes.resize(size, 0);
    fs::write(pictures.join("big.jpg"), &bytes).expect("a large file");
    let tune = |config: &mut ServerConfig| {
        config.timeouts.rate_grace = Duration::from_millis(200);
        config.timeouts.min_bytes_per_second = 4 * 1024 * 1024;
    };
    let running = Running::start_with(Some(open(std::slice::from_ref(&pictures), &folder.0)), &folder.0, tune, LibraryLimits::default());
    let path = get(running.frontend, "/api/library").json()["posts"][0]["media"][0]["path"].as_str().expect("a path").to_owned();
    let mut stream = TcpStream::connect(running.frontend).expect("connect");
    write!(stream, "GET /api/library/files/{path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").expect("the request");
    // The client reads nothing for a while: the server gives up instead of waiting for it.
    std::thread::sleep(Duration::from_secs(6));
    stream.set_read_timeout(Some(Duration::from_secs(20))).expect("timeout");
    let mut received = Vec::new();
    let _ = stream.read_to_end(&mut received);
    assert!(received.len() < size, "the answer was cut off: {} of {size} bytes came", received.len());
    let deadline = Instant::now() + Duration::from_secs(10);
    while running.server.as_ref().is_some_and(|server| server.live_connections() > 0) && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(running.server.as_ref().map(Server::live_connections), Some(0), "the connection is gone");
}

/// Waits up to `limit` for the server to have no connection left; how long that took.
fn connections_gone_within(running: &Running, limit: Duration) -> Option<Duration> {
    let started = Instant::now();
    while started.elapsed() < limit {
        if running.server.as_ref().is_some_and(|server| server.live_connections() == 0) {
            return Some(started.elapsed());
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

#[test]
fn bytes_taken_fast_do_not_buy_hours_for_an_answer_that_then_stalls() {
    let folder = TempFolder::new("front-loaded");
    let pictures = folder.0.join("pictures");
    fs::create_dir_all(&pictures).expect("a folder");
    let size: usize = 48 * 1024 * 1024;
    let mut bytes = fs::read(export_basic().join("posts").join("post1.jpg")).expect("a JPEG");
    bytes.resize(size, 0);
    fs::write(pictures.join("big.jpg"), &bytes).expect("a large file");
    let tune = |config: &mut ServerConfig| {
        config.timeouts.rate_grace = Duration::from_millis(500);
        config.timeouts.min_bytes_per_second = 1024 * 1024;
    };
    let running = Running::start_with(Some(open(std::slice::from_ref(&pictures), &folder.0)), &folder.0, tune, LibraryLimits::default());
    let path = get(running.frontend, "/api/library").json()["posts"][0]["media"][0]["path"].as_str().expect("a path").to_owned();
    let mut stream = TcpStream::connect(running.frontend).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
    write!(stream, "GET /api/library/files/{path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n").expect("the request");
    // 12 MB at once: a cumulative average would now allow 12.5 s of nothing at 1 MB a second.
    let mut taken = vec![0_u8; 12 * 1024 * 1024];
    stream.read_exact(&mut taken).expect("the first 12 MB");
    let gone = connections_gone_within(&running, Duration::from_secs(8));
    assert!(gone.is_some_and(|after| after < Duration::from_secs(5)), "cut off about the grace after it stalled, not 12 s later: {gone:?}");
    // The connection is reset: the client sees the end at once instead of being sent the rest
    // of what the system had accepted, for as long as it keeps the connection.
    stream.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
    let mut rest = Vec::new();
    let ended = stream.read_to_end(&mut rest);
    assert!(!matches!(&ended, Err(error) if matches!(error.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut)), "the client sees the end: {ended:?}");
    assert!(taken.len() + rest.len() < size, "the rest of the answer was not sent");
}

// --- Uploads and the browser imports folder ----------------------------------------------------------

/// Uploads two pictures and commits one post with each; the answer of the commit.
fn import_two_posts(address: SocketAddr) -> Answer {
    let created = write(address, "POST", "/api/library/uploads", "application/json", b"");
    assert_eq!(created.status, 201, "{}", String::from_utf8_lossy(&created.body));
    let id = created.json()["upload_id"].as_str().expect("an upload id").to_owned();
    assert!(id.len() == 32 && id.chars().all(|character| character.is_ascii_hexdigit()));
    let jpeg = fs::read(export_basic().join("posts").join("post1.jpg")).expect("a JPEG");
    let png = fs::read(export_basic().join("posts").join("post2.png")).expect("a PNG");
    let put = write(address, "PUT", &format!("/api/library/uploads/{id}/files/m000001.jpg"), "image/jpeg", &jpeg);
    assert_eq!(put.json(), serde_json::json!({"name": "m000001.jpg", "bytes": jpeg.len(), "mime": "image/jpeg", "kind": "image"}));
    let put = write(address, "PUT", &format!("/api/library/uploads/{id}/files/m000002.png"), "image/png", &png);
    assert_eq!(put.status, 200);
    let commit = serde_json::json!({
        "source_name": "export-basic.zip",
        "source_kind": "archive",
        "posts": [
            {"id": "WEB001", "media_type": "image", "media": [{"type": "image", "path": "m000001.jpg"}], "user": {"username": "lumen.atlas"}, "caption": "First import", "comment_count": 0, "like_count": 3, "taken_at": "2026-11-02T10:00:00.000Z", "comments": []},
            {"id": "WEB002", "media_type": "image", "media": [{"type": "image", "path": "m000002.png"}], "user": {"username": "lumen.atlas"}, "caption": "Second import", "comment_count": 0, "like_count": null, "taken_at": "2026-11-03T10:00:00.000Z", "comments": []}
        ]
    });
    write(address, "POST", &format!("/api/library/uploads/{id}/commit"), "application/json", commit.to_string().as_bytes())
}

#[test]
fn an_import_from_the_browser_becomes_an_export_folder_the_app_shows() {
    let folder = TempFolder::new("uploads");
    let running = Running::start(Some(open(&[export_basic()], &folder.0)), &folder.0);
    let committed = import_two_posts(running.frontend);
    assert_eq!(committed.status, 200, "{}", String::from_utf8_lossy(&committed.body));
    let library = committed.json();
    let ids: Vec<&str> = library["posts"].as_array().expect("posts").iter().filter_map(|post| post["id"].as_str()).collect();
    assert!(ids.contains(&"WEB001") && ids.contains(&"WEB002") && ids.contains(&"FXTbasic001"), "the import joined what is open: {ids:?}");
    assert_eq!(running.host.commands(), ["ShowWebLibrary"]);
    // The folder is an export the app's reader accepts, in the clone's library format.
    let manifest_path = folder.0.join("library").join("post.json");
    let loaded = manifest::load_export(&manifest_path).expect("the browser imports folder is a valid export");
    assert_eq!(loaded.posts.len(), 2);
    assert_eq!(loaded.posts[0].id, "WEB002", "newest first");
    let raw: Value = serde_json::from_slice(&fs::read(&manifest_path).expect("post.json")).expect("JSON");
    assert_eq!(raw["source"], serde_json::json!({"type": "library"}));
    assert_eq!(raw["library"]["imports"][0]["source_name"], Value::from("export-basic.zip"));
    assert!(folder.0.join("library").join(".instagram-library").is_file());
    assert!(fs::read_dir(folder.0.join("library").join("posts")).expect("posts").count() == 2);
    assert_eq!(library["imports"].as_array().map(Vec::len), Some(2), "the export and the browser imports' own history");
    // The posts of the browser imports may be deleted through the browser; others may not.
    let refused = write(running.frontend, "DELETE", "/api/library/posts/FXTbasic001", "application/json", b"");
    assert_eq!((refused.status, refused.detail().as_str()), (403, "This post comes from your own pictures, folders or archives, which the browser never deletes. Close it in Instagram Windows App instead."));
    let deleted = write(running.frontend, "DELETE", "/api/library/posts/WEB001", "application/json", b"");
    assert_eq!(deleted.status, 200);
    assert!(!deleted.json()["posts"].as_array().expect("posts").iter().any(|post| post["id"] == "WEB001"));
    // Clearing is refused while something else than the browser imports is open.
    let clear = write(running.frontend, "DELETE", "/api/library", "application/json", b"");
    assert_eq!((clear.status, clear.detail().as_str()), (403, "The posts come from your own pictures, folders or archives, which the browser never deletes. Close them in Instagram Windows App instead."));
    assert!(manifest_path.is_file());
}

#[test]
fn imports_are_refused_while_loose_pictures_are_open() {
    let folder = TempFolder::new("loose-refused");
    let running = Running::start(Some(open(&[fixtures().join("loose")], &folder.0)), &folder.0);
    let refused = write(running.frontend, "POST", "/api/library/uploads", "application/json", b"");
    assert_eq!((refused.status, refused.detail().as_str()), (409, "Instagram Windows App is showing loose pictures. Close them there, or open an export or archive, to save imports from the browser."));
    assert!(!folder.0.join("library").exists(), "nothing was created");
}

#[test]
fn the_browser_imports_alone_can_be_cleared() {
    let folder = TempFolder::new("clear");
    let running = Running::start(None, &folder.0);
    assert_eq!(import_two_posts(running.frontend).status, 200);
    assert_eq!(running.host.snapshot().kind, web::SnapshotKind::Exports { only_web_library: true });
    // A file of the user beside the library's own: it stays.
    fs::write(folder.0.join("library").join("notes.txt"), b"mine").expect("a file of the user");
    let cleared = write(running.frontend, "DELETE", "/api/library", "application/json", b"");
    assert_eq!(cleared.status, 204);
    assert_eq!(running.host.commands(), ["ShowWebLibrary", "CloseWebLibrary"]);
    assert!(!folder.0.join("library").join("post.json").exists());
    assert!(!folder.0.join("library").join("posts").exists());
    assert!(folder.0.join("library").join("notes.txt").is_file(), "only what the library wrote is deleted");
    // Nothing open: clearing is allowed (and finds nothing).
    assert_eq!(write(running.frontend, "DELETE", "/api/library", "application/json", b"").status, 204);
}

#[test]
fn upload_limits_types_and_cancelling() {
    let folder = TempFolder::new("upload-limits");
    let limits = LibraryLimits { max_image_bytes: 1000, ..LibraryLimits::default() };
    let running = Running::start_with(None, &folder.0, |_| {}, limits);
    let id = write(running.frontend, "POST", "/api/library/uploads", "application/json", b"").json()["upload_id"].as_str().expect("an id").to_owned();
    let jpeg = fs::read(export_basic().join("posts").join("post1.jpg")).expect("a JPEG");
    let large = write(running.frontend, "PUT", &format!("/api/library/uploads/{id}/files/m000001.jpg"), "image/jpeg", &jpeg);
    assert_eq!((large.status, large.detail().as_str()), (413, "A single image can be at most 1000 bytes."));
    // The content decides what a file is, not its name.
    let text = write(running.frontend, "PUT", &format!("/api/library/uploads/{id}/files/m000002.png"), "text/plain", b"hello world");
    assert_eq!((text.status, text.detail().as_str()), (415, "Only JPEG, PNG, GIF, WebP, AVIF and BMP images and MP4, MOV and WebM videos can be saved."));
    let heic = fs::read(export_basic().join("posts").join("post4.heic")).expect("a HEIC");
    assert_eq!(write(running.frontend, "PUT", &format!("/api/library/uploads/{id}/files/m000003.jpg"), "image/heic", &heic[..900.min(heic.len())]).status, 415, "HEIC is converted by the browser first");
    let named_heic = write(running.frontend, "PUT", &format!("/api/library/uploads/{id}/files/m000003.heic"), "image/heic", &heic[..900.min(heic.len())]);
    assert_eq!(named_heic.status, 422, "and the frontend never names an upload .heic");
    let name = write(running.frontend, "PUT", &format!("/api/library/uploads/{id}/files/evil.exe"), "application/octet-stream", b"MZ");
    assert_eq!((name.status, name.detail().as_str()), (422, "\"evil.exe\" is not a valid upload file name."));
    let staging = folder.0.join("library").join(".staging").join(&id);
    assert!(staging.is_dir());
    // A file that starts like a JPEG under a name Windows would run is refused before a byte is
    // written: only the extensions of pictures and videos are taken.
    let disguised = [&b"\xFF\xD8\xFF"[..], b"\r\necho this line would run if the file were opened\r\n"].concat();
    for script in ["m000004.bat", "m000005.hta", "m000006.cmd", "m000007.scr"] {
        let refused = write(running.frontend, "PUT", &format!("/api/library/uploads/{id}/files/{script}"), "image/jpeg", &disguised);
        assert_eq!((refused.status, refused.detail()), (422, format!("\"{script}\" is not a valid upload file name.")));
    }
    assert_eq!(fs::read_dir(&staging).expect("the staging folder").count(), 0, "nothing was written");
    assert_eq!(write(running.frontend, "DELETE", &format!("/api/library/uploads/{id}"), "application/json", b"").status, 204);
    assert!(!staging.exists(), "the staged files went with the session");
    let unknown = write(running.frontend, "POST", &format!("/api/library/uploads/{id}/commit"), "application/json", b"{}");
    assert_eq!((unknown.status, unknown.detail().as_str()), (404, "No such upload; start a new one."));
}

#[test]
fn a_commit_is_checked_before_its_body_is_read() {
    let folder = TempFolder::new("commit-early");
    let running = Running::start(None, &folder.0);
    // Only the head of a 32 MiB commit to an upload that does not exist: the answer comes at
    // once, before (and without) the body, and nothing was set aside for it.
    let mut stream = TcpStream::connect(running.frontend).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(20))).expect("timeout");
    write!(stream, "POST /api/library/uploads/0123456789abcdef0123456789abcdef/commit HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\nContent-Length: 33554432\r\n\r\n").expect("the head");
    let started = Instant::now();
    let answer = read_answer(&mut stream, false);
    assert_eq!((answer.status, answer.detail().as_str()), (404, "No such upload; start a new one."));
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    assert_eq!(answer.header("connection"), Some("close"), "the body that was not read closes the connection");
    let bogus = send(running.frontend, "POST /api/library/uploads/not-an-id/commit HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nContent-Length: 2", b"{}");
    assert_eq!(bogus.status, 404);
}

/// Sends only the head of a commit of `length` bytes for `id`.
fn commit_head(address: SocketAddr, id: &str, length: usize) -> TcpStream {
    let mut stream = TcpStream::connect(address).expect("connect");
    stream.set_read_timeout(Some(Duration::from_secs(20))).expect("timeout");
    write!(stream, "POST /api/library/uploads/{id}/commit HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nContent-Type: application/json\r\nContent-Length: {length}\r\n\r\n").expect("the head");
    stream
}

#[test]
fn a_commit_body_is_received_once_per_upload_and_slow_ones_keep_no_import_out() {
    // E2E3-2: a client that sends commit bodies slowly holds nothing the user's import needs.
    let folder = TempFolder::new("commit-reservations");
    let running = Running::start(None, &folder.0);
    let ids: Vec<String> = (0..3).map(|_| write(running.frontend, "POST", "/api/library/uploads", "application/json", b"").json()["upload_id"].as_str().expect("an id").to_owned()).collect();
    let empty = br#"{"source_kind": "archive", "posts": []}"#;
    let mut first = commit_head(running.frontend, &ids[0], empty.len());
    std::thread::sleep(Duration::from_millis(300));
    // A second commit of the same upload is refused before a byte of its 32 MB is read.
    let started = Instant::now();
    let again = read_answer(&mut commit_head(running.frontend, &ids[0], 32 * 1024 * 1024), false);
    assert_eq!((again.status, again.detail().as_str(), again.header("connection")), (409, "This upload is already being saved.", Some("close")));
    assert!(started.elapsed() < Duration::from_secs(5), "{:?}", started.elapsed());
    // Two more bodies arrive slowly, one of them 32 MB long: the body so far is on disk ...
    let mut second = commit_head(running.frontend, &ids[1], 32 * 1024 * 1024);
    second.write_all(&vec![b' '; 1024 * 1024]).expect("a part of the body");
    let third = commit_head(running.frontend, &ids[2], 1000);
    std::thread::sleep(Duration::from_millis(300));
    let body_on_disk = folder.0.join("library").join(".staging").join(&ids[1]).join(".commit.json");
    assert_eq!(fs::metadata(&body_on_disk).map(|metadata| metadata.len()).ok(), Some(1024 * 1024), "written as it arrives");
    // ... and the user's import is saved meanwhile.
    let committed = import_two_posts(running.frontend);
    assert_eq!(committed.status, 200, "{}", String::from_utf8_lossy(&committed.body));
    // The first body comes: it is read and checked (no posts), and its upload is free again.
    first.write_all(empty).expect("the body");
    let checked = read_answer(&mut first, false);
    assert_eq!((checked.status, checked.detail().as_str()), (422, "The commit lists no posts."));
    assert!(!folder.0.join("library").join(".staging").join(&ids[0]).join(".commit.json").exists(), "a body that was checked is not kept");
    let retried = write(running.frontend, "POST", &format!("/api/library/uploads/{}/commit", ids[0]), "application/json", empty);
    assert_eq!(retried.status, 422, "the failed commit gave its upload back: {}", retried.detail());
    // A body that is cut off is not kept either.
    drop((second, third));
    let deadline = Instant::now() + Duration::from_secs(5);
    while body_on_disk.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(!body_on_disk.exists());
}

#[test]
fn a_commit_waits_for_its_turn_and_is_refused_when_none_comes() {
    let folder = TempFolder::new("commit-turns");
    let limits = LibraryLimits { max_commits_at_once: 0, commit_wait: Duration::from_millis(300), ..LibraryLimits::default() };
    let running = Running::start_with(None, &folder.0, |_| {}, limits);
    let id = write(running.frontend, "POST", "/api/library/uploads", "application/json", b"").json()["upload_id"].as_str().expect("an id").to_owned();
    let started = Instant::now();
    let refused = write(running.frontend, "POST", &format!("/api/library/uploads/{id}/commit"), "application/json", br#"{"posts": []}"#);
    assert_eq!((refused.status, refused.detail().as_str()), (503, "Other imports are being saved right now. Try again in a moment."));
    assert!(started.elapsed() >= Duration::from_millis(300), "it waited for a turn first");
}

#[test]
fn bytes_sent_fast_do_not_buy_hours_for_a_body_that_then_stalls() {
    let folder = TempFolder::new("front-loaded-body");
    let tune = |config: &mut ServerConfig| {
        config.timeouts.rate_grace = Duration::from_millis(500);
        config.timeouts.min_bytes_per_second = 1024 * 1024;
    };
    let running = Running::start_with(None, &folder.0, tune, LibraryLimits::default());
    let id = write(running.frontend, "POST", "/api/library/uploads", "application/json", b"").json()["upload_id"].as_str().expect("an id").to_owned();
    // 12 MB of a 12 MB + 1 commit at once, then nothing: a cumulative average would wait 12.5 s.
    let fast = 12 * 1024 * 1024;
    let mut stream = commit_head(running.frontend, &id, fast + 1);
    stream.write_all(&vec![b' '; fast]).expect("the first 12 MB");
    let stalled = Instant::now();
    let answer = read_answer(&mut stream, false);
    assert_eq!((answer.status, answer.detail().as_str()), (400, "The request body was cut off."));
    assert!(stalled.elapsed() < Duration::from_secs(5), "cut off about the grace after it stalled: {:?}", stalled.elapsed());
}

#[test]
fn one_client_cannot_cancel_or_crowd_out_the_imports_of_another() {
    let folder = TempFolder::new("sessions-per-client");
    let limits = LibraryLimits { max_open_sessions: 3, max_sessions_per_client: 2, sessions_kept_for_this_computer: 0, stale_staging: Duration::from_secs(3600), ..LibraryLimits::default() };
    let library = Library::with_limits(folder.0.join("library"), limits, |_| Some(u64::MAX / 2));
    let staged = |id: &str| folder.0.join("library").join(".staging").join(id).is_dir();
    let (laptop, phone, tablet): (IpAddr, IpAddr, IpAddr) = ("192.168.1.31".parse().expect("address"), "192.168.1.30".parse().expect("address"), "192.168.1.32".parse().expect("address"));
    let theirs = library.create_upload_from(laptop).expect("the laptop's import");
    let first = library.create_upload_from(phone).expect("one");
    let second = library.create_upload_from(phone).expect("two");
    let third = library.create_upload_from(phone).expect("the phone's own idle import makes room");
    assert!(!staged(&first) && staged(&second) && staged(&third));
    assert!(staged(&theirs), "the laptop's import was not touched");
    // The folder is full (3): a third device is refused, and nobody's import is cancelled.
    let refused = library.create_upload_from(tablet).expect_err("full");
    assert_eq!((refused.status, refused.detail.as_str()), (429, "Too many imports are being saved at the same time. Try again when one has finished."));
    assert!(staged(&theirs) && staged(&second) && staged(&third));
    library.close();
}

#[test]
fn devices_on_the_network_never_take_the_imports_kept_for_this_computer() {
    let folder = TempFolder::new("sessions-kept");
    let library = Library::with_limits(folder.0.join("library"), LibraryLimits::default(), |_| Some(u64::MAX / 2));
    let staged = |id: &str| folder.0.join("library").join(".staging").join(id).is_dir();
    let address = |text: &str| -> IpAddr { text.parse().expect("an address") };
    // Two devices (or one with two addresses) start all the imports they may: 6 of the 8.
    let laptop: Vec<String> = (0..4).map(|_| library.create_upload_from(address("192.168.1.31")).expect("the laptop's import")).collect();
    let phone_first = library.create_upload_from(address("192.168.1.30")).expect("the phone's first");
    let phone_second = library.create_upload_from(address("192.168.1.30")).expect("the phone's second");
    let phone_third = library.create_upload_from(address("192.168.1.30")).expect("the phone's own idle first import makes room");
    assert!(!staged(&phone_first) && staged(&phone_second) && staged(&phone_third));
    let refused = library.create_upload_from(address("192.168.1.32")).expect_err("the network's share is taken");
    assert_eq!(refused.status, 429);
    // The user's browser still imports, and every program on this computer is that one client.
    let mine = library.create_upload_from(address("127.0.0.1")).expect("this computer's import");
    let other_program = library.create_upload_from(address("127.0.0.2")).expect("a place kept for this computer");
    let third_program = library.create_upload_from(address("127.0.0.3")).expect("this computer's oldest idle import makes room");
    assert!(!staged(&mine) && staged(&other_program) && staged(&third_program), "127.0.0.3 counts with 127.0.0.1");
    assert!(laptop.iter().all(|id| staged(id)) && staged(&phone_second) && staged(&phone_third), "nobody else's import was cancelled");
    library.close();
}

#[test]
fn an_import_that_never_received_a_file_is_dropped_soon() {
    let folder = TempFolder::new("sessions-empty");
    let limits = LibraryLimits { stale_empty_session: Duration::ZERO, stale_staging: Duration::from_secs(3600), ..LibraryLimits::default() };
    let library = Library::with_limits(folder.0.join("library"), limits, |_| Some(u64::MAX / 2));
    let staged = |id: &str| folder.0.join("library").join(".staging").join(id).is_dir();
    let empty = library.create_upload().expect("an import");
    let used = library.create_upload().expect("another import");
    let jpeg = fs::read(export_basic().join("posts").join("post1.jpg")).expect("a JPEG");
    library.receive_file(&used, "m000001.jpg", &mut jpeg.as_slice(), Some(jpeg.len() as u64)).expect("a file");
    let _next = library.create_upload().expect("a third import");
    assert!(!staged(&empty), "the import without a file went");
    assert!(staged(&used), "the one with a file stays for the stale time");
    library.close();
}

#[test]
fn clear_removes_what_is_named_like_the_librarys_files_and_nothing_else() {
    // README, Configuration: posts\ and avatars\ belong to the viewer, as the marker file says;
    // a file put there by hand under a name the library uses goes with Clear library (the
    // clone's rule), every other file stays.
    let folder = TempFolder::new("clear-names");
    let library_dir = folder.0.join("library");
    let library = Library::new(library_dir.clone());
    let id = library.create_upload().expect("the folder is claimed");
    library.abort_upload(&id).expect("cancelled");
    fs::create_dir_all(library_dir.join("posts").join("album")).expect("folders");
    fs::create_dir_all(library_dir.join("avatars")).expect("folders");
    for (name, bytes) in [("mine.txt", &b"notes"[..]), ("posts/holiday.jpg", b"\xFF\xD8\xFF"), ("posts/photo_1.jpg", b"\xFF\xD8\xFF"), ("posts/album/x_1.jpg", b"\xFF\xD8\xFF"), ("avatars/me.jpg", b"\xFF\xD8\xFF")] {
        fs::write(library_dir.join(name), bytes).expect("a file of the user");
    }
    library.clear().expect("cleared");
    for kept in ["mine.txt", "posts/holiday.jpg", "posts/album/x_1.jpg"] {
        assert!(library_dir.join(kept).is_file(), "{kept} stays");
    }
    for gone in ["posts/photo_1.jpg", "avatars/me.jpg"] {
        assert!(!library_dir.join(gone).exists(), "{gone} is named like a file of the library");
    }
    library.close();
}

// --- Pages, docs, stop and restart ---------------------------------------------------------------------

#[test]
fn the_pages_are_served_with_the_app_fallback_and_docs_only_on_the_api_listener() {
    let folder = TempFolder::new("static");
    let running = Running::start(None, &folder.0);
    let index = get(running.frontend, "/");
    assert_eq!((index.status, index.header("content-type")), (200, Some("text/html; charset=utf-8")));
    assert_eq!(index.header("x-frame-options"), Some("DENY"));
    assert_eq!(index.header("content-security-policy"), Some("frame-ancestors 'none'"));
    assert_eq!(index.header("referrer-policy"), Some("no-referrer"), "a link to instagram.com does not tell it this computer's address");
    assert_eq!(index.header("cache-control"), Some("no-cache"));
    let asset = get(running.frontend, "/assets/app-1.js");
    assert_eq!(asset.header("cache-control"), Some("public, max-age=31536000, immutable"));
    let revalidated = send(running.frontend, "GET /assets/app-1.js HTTP/1.1\r\nHost: 127.0.0.1\r\nIf-None-Match: \"00000000000000b2\"", b"");
    assert_eq!(revalidated.status, 304);
    let route = get(running.frontend, "/explore/some-post");
    assert_eq!(route.body, index.body, "a path of the app gets index.html");
    let missing = get(running.frontend, "/missing.png");
    assert_eq!((missing.status, missing.header("content-type"), missing.body.as_slice()), (404, Some("text/plain; charset=utf-8"), &b"Not Found"[..]));
    assert_eq!(get(running.frontend, "/licenses/README.txt").body, b"Third-party software");
    let docs = get(running.backend, "/docs");
    assert_eq!(docs.status, 200);
    assert_eq!(docs.header("referrer-policy"), Some("no-referrer"));
    assert!(String::from_utf8_lossy(&docs.body).contains("/api/library/uploads/{upload_id}/commit"));
    assert_eq!(get(running.backend, "/assets/app-1.js").status, 404, "no pages on the API listener");
}

#[test]
fn every_route_of_the_documented_table_is_routed_on_both_addresses_and_no_other_method_is() {
    // `API_ROUTES` is the table /docs shows and the README repeats (app/tests/docs.rs). Here it
    // is held against the router: every method it names for a path reaches a handler on both
    // listeners (an unknown route answers 404 "Not Found", a known one with another method
    // 405), and every method it does not name for that path is refused with 405. The ids are
    // made up, so most handlers answer that the thing does not exist; that is their own answer.
    let folder = TempFolder::new("route-table");
    let running = Running::start(Some(open(&[export_basic()], &folder.0)), &folder.0);
    let concrete = |path: &str| path.replace("{path}", "posts/none.jpg").replace("{upload_id}", "0123456789abcdef0123456789abcdef").replace("{name}", "a.jpg").replace("{post_id}", "nothing");
    let mut methods_of: Vec<(String, Vec<&str>)> = Vec::new();
    for (methods, path, _) in web::routes::API_ROUTES {
        let path = concrete(path);
        match methods_of.iter_mut().find(|(known, _)| *known == path) {
            Some((_, listed)) => listed.extend(methods.split(", ")),
            None => methods_of.push((path, methods.split(", ").collect())),
        }
    }
    assert_eq!(methods_of.len(), 8, "eight paths, some with several methods");
    for (path, listed) in &methods_of {
        for address in [running.frontend, running.backend] {
            for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE"] {
                let answer = match method {
                    "GET" => get(address, path),
                    "HEAD" => send(address, &format!("HEAD {path} HTTP/1.1\r\nHost: 127.0.0.1:{}", address.port()), b""),
                    _ => write(address, method, path, "application/json", b"{}"),
                };
                assert!(answer.status != 404 || method == "HEAD" || answer.detail() != instagram_core::messages::NOT_FOUND, "{method} {path} on {address} is not routed");
                // HEAD is GET without the body wherever GET is allowed.
                let documented = listed.contains(&method) || (method == "HEAD" && listed.contains(&"GET"));
                if documented {
                    assert_ne!(answer.status, 405, "{method} {path} on {address} is documented but refused");
                } else {
                    assert_eq!(answer.status, 405, "{method} {path} on {address} is not documented but answered {}", answer.status);
                }
            }
        }
    }
}

#[test]
fn stop_returns_quickly_with_a_connection_open_and_frees_the_ports() {
    let folder = TempFolder::new("stop");
    let mut running = Running::start(None, &folder.0);
    let _idle = TcpStream::connect(running.frontend).expect("an idle connection");
    let server = running.server.take().expect("the server");
    let started = Instant::now();
    assert!(server.stop(Duration::from_millis(web::WEB_STOP_WAIT_MS)), "both listeners closed");
    assert!(started.elapsed() <= Duration::from_millis(web::WEB_STOP_WAIT_MS + 200), "{:?}", started.elapsed());
    assert!(TcpStream::connect_timeout(&running.frontend, Duration::from_millis(500)).is_err(), "the page port is free");
    assert!(TcpStream::connect_timeout(&running.backend, Duration::from_millis(500)).is_err(), "the API port is free");
}

/// A port nobody listens on (bound once and released).
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").and_then(|listener| listener.local_addr()).map(|address| address.port()).expect("a free port")
}

#[test]
fn the_server_moves_to_new_ports_and_refuses_a_taken_one() {
    let folder = TempFolder::new("restart");
    let mut running = Running::start(Some(open(&[export_basic()], &folder.0)), &folder.0);
    let old_config = ServerConfig::new("127.0.0.1".parse().expect("address"), running.frontend.port(), running.backend.port(), ASSETS, folder.0.join("web-cache"));
    // A port another program holds is refused, and the old server keeps serving.
    let taken = TcpListener::bind("127.0.0.1:0").expect("another program");
    let mut busy = old_config.clone();
    busy.frontend_port = taken.local_addr().expect("address").port();
    busy.backend_port = free_port();
    busy.search = PortSearch::Exact;
    let failure = web::restart(running.server.take(), &busy, Some(&old_config), Arc::clone(&running.host) as Arc<dyn WebHost>, Arc::clone(&running.library)).err().expect("refused");
    assert_eq!(failure.error.message(), format!("Port {} is used by another program. Choose another port.", busy.frontend_port));
    running.server = failure.server;
    assert_eq!(get(running.frontend, "/api/health").status, 200, "the old server still answers");

    // New ports: the old ones refuse, the new ones answer.
    let mut moved = old_config.clone();
    moved.frontend_port = free_port();
    moved.backend_port = free_port();
    moved.search = PortSearch::Exact;
    let server = web::restart(running.server.take(), &moved, Some(&old_config), Arc::clone(&running.host) as Arc<dyn WebHost>, Arc::clone(&running.library)).ok().expect("moved");
    assert_eq!((server.frontend_address().port(), server.backend_address().port()), (moved.frontend_port, moved.backend_port));
    assert!(TcpStream::connect_timeout(&running.frontend, Duration::from_millis(500)).is_err(), "the old page port refuses");
    assert!(TcpStream::connect_timeout(&running.backend, Duration::from_millis(500)).is_err(), "the old API port refuses");
    running.frontend = server.frontend_address();
    running.backend = server.backend_address();
    running.server = Some(server);
    assert_eq!(get(running.frontend, "/api/health").status, 200);
    assert_eq!(get(running.backend, "/api/library").json()["posts"].as_array().map(Vec::len), Some(6), "the same posts on the new ports");

    // An upload being received when the server moves is cancelled and its staging removed.
    let id = write(running.frontend, "POST", "/api/library/uploads", "application/json", b"").json()["upload_id"].as_str().expect("an id").to_owned();
    let staging = folder.0.join("library").join(".staging").join(&id);
    let mut slow = TcpStream::connect(running.frontend).expect("connect");
    slow.set_read_timeout(Some(Duration::from_secs(10))).expect("timeout");
    write!(slow, "PUT /api/library/uploads/{id}/files/m000001.jpg HTTP/1.1\r\nHost: 127.0.0.1\r\nSec-Fetch-Site: same-origin\r\nContent-Length: 100000\r\n\r\n").expect("the head");
    slow.write_all(&fs::read(export_basic().join("posts").join("post1.jpg")).expect("a JPEG")[..5000]).expect("the first part");
    std::thread::sleep(Duration::from_millis(300));
    let mut again = moved.clone();
    again.frontend_port = free_port();
    again.backend_port = free_port();
    let server = web::restart(running.server.take(), &again, Some(&moved), Arc::clone(&running.host) as Arc<dyn WebHost>, Arc::clone(&running.library)).ok().expect("moved again");
    running.frontend = server.frontend_address();
    running.backend = server.backend_address();
    running.server = Some(server);
    let _ = slow.write_all(&[0_u8; 4096]);
    let cancelled = read_answer(&mut slow, false);
    assert_eq!((cancelled.status, cancelled.detail().as_str()), (409, "This upload was cancelled."));
    let deadline = Instant::now() + Duration::from_secs(5);
    while staging.exists() && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(50));
    }
    assert!(!staging.exists(), "the staging of the cancelled upload is gone");
}

#[test]
fn a_second_copy_of_the_app_sees_the_browser_imports_in_use() {
    let folder = TempFolder::new("in-use");
    let first = Library::new(folder.0.join("library"));
    assert!(first.create_upload().is_ok(), "the first copy claims the folder");
    let second = Library::new(folder.0.join("library"));
    let refused = second.create_upload().expect_err("in use");
    assert_eq!((refused.status, refused.detail.as_str()), (503, "The browser imports folder is in use by another running copy of this app. Close that copy, or give this one its own folder with \"library_directory\" in settings.json."));
    first.close();
    assert!(second.create_upload().is_ok(), "free again once the first copy is closed");
    second.close();
}

#[test]
fn a_folder_that_holds_other_files_is_never_adopted() {
    let folder = TempFolder::new("foreign");
    let library_dir = folder.0.join("library");
    fs::create_dir_all(&library_dir).expect("a folder");
    fs::copy(export_basic().join("post.json"), library_dir.join("post.json")).expect("an extracted export");
    let library = Library::new(library_dir.clone());
    let refused = library.create_upload().expect_err("foreign");
    assert_eq!(refused.status, 503);
    assert!(refused.detail.starts_with("The browser imports folder already holds files this app did not create (\"post.json\""), "{}", refused.detail);
    assert!(library.clear().is_err(), "never cleared");
    assert!(library_dir.join("post.json").is_file() && !library_dir.join(".instagram-library").exists(), "untouched");
}

#[test]
fn sessions_are_capped_and_stale_ones_expire() {
    let folder = TempFolder::new("sessions");
    let limits = LibraryLimits { max_open_sessions: 1, stale_staging: Duration::from_secs(3600), ..LibraryLimits::default() };
    let library = Library::with_limits(folder.0.join("library"), limits, |_| Some(u64::MAX / 2));
    let first = library.create_upload().expect("one");
    let second = library.create_upload().expect("the idle first one is dropped for it");
    assert!(!folder.0.join("library").join(".staging").join(&first).exists());
    assert!(folder.0.join("library").join(".staging").join(&second).exists());
    library.close();
    let expiring = Library::with_limits(folder.0.join("library"), LibraryLimits { stale_staging: Duration::ZERO, ..LibraryLimits::default() }, |_| Some(u64::MAX / 2));
    let old = expiring.create_upload().expect("one");
    let _new = expiring.create_upload().expect("another");
    assert!(!folder.0.join("library").join(".staging").join(&old).exists(), "a session nobody used for the stale time went");
    expiring.close();
    let full = Library::with_limits(folder.0.join("library"), LibraryLimits::default(), |_| Some(100));
    let id = full.create_upload().expect("a session");
    let refused = full.receive_file(&id, "m000001.jpg", &mut &b"\xFF\xD8\xFF\xE0"[..], Some(4)).expect_err("no space");
    assert_eq!((refused.status, refused.detail.as_str()), (507, "Not enough free disk space: 100 bytes is free on the library's drive and at least 1 GB must stay free."));
    full.close();
}
