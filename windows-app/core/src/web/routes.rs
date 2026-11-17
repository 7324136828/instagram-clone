//! The route table of both listeners (web brief 4.6): the clone's API over what the app has open,
//! the browser imports, the vendored pages on the page listener and a small `/docs` page on the
//! API listener.
//!
//! Every request first passes the guard (`guard.rs`): the Host check, then, for a request that
//! changes something, the own-pages check. On the API listener an allowed cross-origin request
//! gets the CORS headers, and a preflight is answered before the routes.

use std::sync::Arc;

use serde_json::{Map, Value};

use crate::library::{self, Library, LibraryError};
use crate::manifest;
use crate::messages;
use crate::model::{ExportKind, ExportSource};
use crate::sources::LoadedSource;
use crate::text;

use super::files::{self, HeicCache, PosterService};
use super::guard::{self, Guard, CORS_MAX_AGE_SECONDS, CORS_METHODS};
use super::http::{self, BodyReader, Handler, Request, Response, ResponseBody};
use super::snapshot::{Snapshot, SnapshotKind};
use super::{ApiError, WebCommandKind, WebFile, WebHost};

/// The body of a post edit (PATCH) is at most this long (library.py `MAX_POST_EDIT_BYTES`).
pub const MAX_POST_EDIT_BYTES: u64 = 256 * 1024;
/// The longest caption an edit may send, in UTF-16 code units (refused, never cut).
pub const MAX_CAPTION_UNITS: usize = manifest::MAX_CAPTION_CHARS;

/// Which listener a request came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListenerKind {
    /// The page listener: the vendored pages and `/api`.
    Frontend,
    /// The API listener: `/api` and `/docs`, CORS for the page origins.
    Backend,
}

/// What both listeners share.
pub struct Shared {
    /// The app: the snapshot and the commands.
    pub host: Arc<dyn WebHost>,
    /// The browser imports folder.
    pub library: Arc<Library>,
    /// The vendored pages.
    pub assets: &'static [WebFile],
    /// The JPEG copies of HEIC pictures.
    pub heic: Arc<HeicCache>,
    /// The posters of loose videos; `None` when the app gave no poster cache (they are then
    /// answered with 404).
    pub posters: Option<Arc<PosterService>>,
}

/// The routes of one listener.
pub struct Routes {
    /// Which listener.
    pub kind: ListenerKind,
    /// Its checks.
    pub guard: Guard,
    /// What both share.
    pub shared: Arc<Shared>,
}

/// An `ApiError` as an answer.
fn api_error(error: ApiError) -> Response {
    Response::detail(error.status, &error.detail)
}

/// A `LibraryError` as an answer.
fn library_error(error: LibraryError) -> Response {
    Response::detail(error.status, &error.detail)
}

/// 405 with the methods the path knows.
fn method_not_allowed(allowed: &str) -> Response {
    Response::detail(405, messages::METHOD_NOT_ALLOWED).with_header("Allow", allowed)
}

/// The library JSON of a snapshot for the client of `request`.
fn library_answer(status: u16, snapshot: &Snapshot, request: &Request) -> Response {
    Response::json_bytes(status, snapshot.body(guard::is_local_peer(request.peer.ip())))
}

/// The headers every HTML page gets: it must never be framed by another site, and a link it
/// follows (to instagram.com) must not tell that site the address of this computer.
fn html_headers(response: Response) -> Response {
    response.with_header("X-Frame-Options", "DENY").with_header("Content-Security-Policy", "frame-ancestors 'none'").with_header("Referrer-Policy", "no-referrer")
}

/// An answer for a client on another computer: an error whose detail names a full path (a
/// store error names descriptions.json with its folder, which holds the Windows account name)
/// gets the file names only (web brief 4.5: other computers are never told a full path).
pub fn for_remote_client(response: Response) -> Response {
    if response.status < 400 {
        return response;
    }
    let ResponseBody::Bytes(bytes) = &response.body else {
        return response;
    };
    let Ok(Value::Object(mut object)) = serde_json::from_slice::<Value>(bytes) else {
        return response;
    };
    let Some(Value::String(detail)) = object.get("detail") else {
        return response;
    };
    let cleaned = guard::without_full_paths(detail);
    if &cleaned == detail {
        return response;
    }
    object.insert("detail".to_owned(), Value::from(cleaned));
    let mut response = response;
    response.body = ResponseBody::Bytes(Value::Object(object).to_string().into_bytes());
    response
}

/// library.py `parse_post_changes`: the caption of an edit, cleaned (NUL removed, line ends
/// unified, trimmed). Too long is refused, never cut.
pub fn parse_post_changes(body: &[u8]) -> Result<String, ApiError> {
    let raw: Value = serde_json::from_slice(body).map_err(|error| ApiError::new(422, messages::request_not_json(&error.to_string())))?;
    let Value::Object(raw) = raw else {
        return Err(ApiError::new(422, messages::REQUEST_NOT_AN_OBJECT));
    };
    let mut unknown: Vec<&String> = raw.keys().filter(|key| key.as_str() != "caption").collect();
    unknown.sort();
    if let Some(first) = unknown.first() {
        let shown = if first.chars().count() <= 80 { (*first).clone() } else { format!("{}...", first.chars().take(77).collect::<String>()) };
        return Err(ApiError::new(422, messages::field_not_editable(&shown)));
    }
    let Some(caption) = raw.get("caption") else {
        return Err(ApiError::new(422, messages::EDIT_CHANGES_NOTHING));
    };
    let Value::String(caption) = caption else {
        return Err(ApiError::new(422, messages::CAPTION_NOT_A_STRING));
    };
    if text::utf16_len(caption) > MAX_CAPTION_UNITS {
        return Err(ApiError::new(422, messages::caption_too_long(MAX_CAPTION_UNITS)));
    }
    Ok(text::saved_caption(&caption.replace('\0', "")))
}

impl Routes {
    /// The browser imports folder read back as an export source, after a change of it; `None`
    /// when it holds no post any more (it is then taken out of what is open).
    fn web_library_source(&self) -> Result<Option<LoadedSource>, ApiError> {
        let manifest_path = self.shared.library.manifest_path();
        match manifest::load_export(&manifest_path) {
            Ok(loaded) if loaded.posts.is_empty() => Ok(None),
            Ok(loaded) => {
                let source = ExportSource {
                    kind: ExportKind::WebLibrary,
                    path: self.shared.library.folder().to_path_buf(),
                    manifest: manifest_path,
                    name: messages::BROWSER_IMPORTS_NAME.to_owned(),
                    opened_at: text::now_ms(),
                    exported_at: loaded.exported_at,
                    source_url: loaded.source_url.clone(),
                    total_entries: loaded.total_entries,
                    shown: loaded.posts.len(),
                    replaced: 0,
                };
                Ok(Some(LoadedSource { source, loaded }))
            }
            Err(error) if error.code == manifest::ManifestErrorCode::NoPosts => Ok(None),
            Err(error) => Err(ApiError::new(503, error.message)),
        }
    }

    /// Makes the posters of the loose videos `snapshot` lists that are not made yet, so that the
    /// JSON answered next leaves out those that cannot be made.
    fn prepare_posters(&self, snapshot: &Snapshot) {
        let Some(posters) = &self.shared.posters else {
            return;
        };
        let videos = snapshot.listed_poster_videos();
        if !videos.is_empty() {
            posters.prepare(&videos, snapshot.formats(), std::time::Duration::from_millis(files::POSTER_LISTING_WAIT_MS));
        }
    }

    /// GET / HEAD of a file of the snapshot.
    fn library_file(&self, request: &Request, path: &str) -> Response {
        let snapshot = self.shared.host.snapshot();
        let Some(file) = snapshot.built().files.get(path).cloned() else {
            return Response::detail(404, messages::NO_SUCH_LIBRARY_FILE);
        };
        if file.heic {
            return match self.shared.heic.converted(&file.path) {
                Ok(copy) => files::serve_file(&copy, file.mime, request, messages::NO_SUCH_LIBRARY_FILE),
                Err(reason) => Response::detail(404, &messages::picture_not_for_browser(&reason)),
            };
        }
        if file.video_poster {
            let Some(posters) = &self.shared.posters else {
                return Response::detail(404, messages::NO_SUCH_LIBRARY_FILE);
            };
            return match posters.poster(&file.path) {
                Ok(poster) => files::serve_file(&poster, file.mime, request, messages::NO_SUCH_LIBRARY_FILE),
                Err(failure) => {
                    if failure.lasting {
                        // The next /api/library lists this video without a poster, so pages
                        // loaded from now on ask for nothing that cannot come.
                        snapshot.record_poster_failure(&file.path);
                    }
                    Response::detail(404, &messages::picture_not_for_browser(&failure.reason))
                }
            };
        }
        files::serve_file(&file.path, file.mime, request, messages::NO_SUCH_LIBRARY_FILE)
    }

    /// DELETE /api/library: only when nothing but the browser imports folder is open.
    fn clear_library(&self) -> Response {
        let snapshot = self.shared.host.snapshot();
        match snapshot.kind {
            SnapshotKind::Empty => {}
            SnapshotKind::Exports { only_web_library: true } => {
                if let Err(error) = self.shared.host.send(WebCommandKind::CloseWebLibrary) {
                    return api_error(error);
                }
            }
            SnapshotKind::Loose | SnapshotKind::Exports { only_web_library: false } => return Response::detail(403, messages::CLEAR_REFUSED),
        }
        match self.shared.library.clear() {
            Ok(()) => Response::empty(204),
            Err(error) => library_error(error),
        }
    }

    /// POST /api/library/uploads/{id}/commit. Everything that needs no body is checked before a
    /// byte of it is read (a commit may be 32 MiB long), and the commit is reserved: one body per
    /// session. The body goes to the session's folder as it arrives; only then does the commit
    /// wait for its turn to be merged (`LibraryLimits::max_commits_at_once` bodies in memory at
    /// once), so a client that sends its body slowly keeps nobody else from saving an import.
    fn commit(&self, request: &Request, body: &mut BodyReader<'_>, upload_id: &str) -> Response {
        if self.shared.host.snapshot().kind == SnapshotKind::Loose {
            return Response::detail(409, messages::IMPORT_WHILE_LOOSE);
        }
        let limit = self.shared.library.limits().max_commit_bytes;
        let length = request.content_length.unwrap_or(0);
        if length > limit {
            return Response::detail(413, &messages::request_too_large(limit)).closing();
        }
        let mut reservation = match self.shared.library.reserve_commit(upload_id) {
            Ok(reservation) => reservation,
            // The body is not read: the connection is closed after the answer.
            Err(error) => return library_error(error).closing(),
        };
        if let Err(error) = reservation.receive(body, length) {
            return library_error(error).closing();
        }
        let document = match reservation.commit() {
            Ok(document) => document,
            Err(error) => return library_error(error),
        };
        let added = document.imports.last().and_then(|record| record.get("posts")).and_then(Value::as_u64).unwrap_or(0) as usize;
        let loaded = match self.web_library_source() {
            Ok(loaded) => loaded,
            Err(error) => return api_error(error),
        };
        match self.shared.host.send(WebCommandKind::ShowWebLibrary { loaded: loaded.map(Box::new), added }) {
            Ok(snapshot) => library_answer(200, &snapshot, request),
            Err(error) => api_error(error),
        }
    }

    /// PATCH /api/library/posts/{id}: the caption, saved by the app like its own editor does.
    fn edit_post(&self, request: &Request, body: &mut BodyReader<'_>, post_id: &str) -> Response {
        if request.content_length.unwrap_or(0) > MAX_POST_EDIT_BYTES {
            return Response::detail(413, &messages::request_too_large(MAX_POST_EDIT_BYTES)).closing();
        }
        let Ok(bytes) = body.read_all(MAX_POST_EDIT_BYTES) else {
            return Response::detail(400, messages::BODY_CUT_OFF).closing();
        };
        let caption = match parse_post_changes(&bytes) {
            Ok(caption) => caption,
            Err(error) => return api_error(error),
        };
        let Some(key) = self.shared.host.snapshot().key_of(post_id).cloned() else {
            return Response::detail(404, messages::LIBRARY_NO_SUCH_POST);
        };
        match self.shared.host.send(WebCommandKind::SetCaption { key, caption }) {
            Ok(snapshot) => library_answer(200, &snapshot, request),
            Err(error) => api_error(error),
        }
    }

    /// DELETE /api/library/posts/{id}: only posts of the browser imports folder.
    fn delete_post(&self, request: &Request, post_id: &str) -> Response {
        let snapshot = self.shared.host.snapshot();
        if snapshot.key_of(post_id).is_none() {
            return Response::detail(404, messages::LIBRARY_NO_SUCH_POST);
        }
        if !snapshot.is_web_library_post(post_id) {
            return Response::detail(403, messages::DELETE_POST_REFUSED);
        }
        if let Err(error) = self.shared.library.delete_post(post_id) {
            return library_error(error);
        }
        let loaded = match self.web_library_source() {
            Ok(loaded) => loaded,
            Err(error) => return api_error(error),
        };
        match self.shared.host.send(WebCommandKind::ShowWebLibrary { loaded: loaded.map(Box::new), added: 0 }) {
            Ok(snapshot) => library_answer(200, &snapshot, request),
            Err(error) => api_error(error),
        }
    }

    /// Everything under `/api`.
    fn api(&self, request: &Request, body: &mut BodyReader<'_>, segments: &[String]) -> Response {
        let method = request.method.as_str();
        let get = method == "GET" || method == "HEAD";
        let parts: Vec<&str> = segments.iter().map(String::as_str).collect();
        match parts.as_slice() {
            ["health"] => {
                if !get {
                    return method_not_allowed("GET");
                }
                let mut status = Map::new();
                status.insert("status".to_owned(), Value::from("ok"));
                Response::json(200, &Value::Object(status))
            }
            ["library"] => match method {
                "GET" | "HEAD" => {
                    let snapshot = self.shared.host.snapshot();
                    self.prepare_posters(&snapshot);
                    library_answer(200, &snapshot, request)
                }
                "DELETE" => self.clear_library(),
                _ => method_not_allowed("GET, DELETE"),
            },
            ["library", "files", rest @ ..] if !rest.is_empty() => {
                if !get {
                    return method_not_allowed("GET, HEAD");
                }
                self.library_file(request, &rest.join("/"))
            }
            ["library", "uploads"] => {
                if method != "POST" {
                    return method_not_allowed("POST");
                }
                if self.shared.host.snapshot().kind == SnapshotKind::Loose {
                    return Response::detail(409, messages::IMPORT_WHILE_LOOSE);
                }
                match self.shared.library.create_upload_from(request.peer.ip()) {
                    Ok(upload_id) => {
                        let mut answer = Map::new();
                        answer.insert("upload_id".to_owned(), Value::from(upload_id));
                        Response::json(201, &Value::Object(answer))
                    }
                    Err(error) => library_error(error),
                }
            }
            ["library", "uploads", upload_id, "files", name] => {
                if method != "PUT" {
                    return method_not_allowed("PUT");
                }
                let Some(length) = request.content_length else {
                    return Response::detail(411, messages::LENGTH_REQUIRED).closing();
                };
                let largest = self.shared.library.limits().max_image_bytes.max(self.shared.library.limits().max_video_bytes);
                if length > largest {
                    return Response::detail(413, &messages::upload_file_too_large(largest)).closing();
                }
                match self.shared.library.receive_file(upload_id, name, body, Some(length)) {
                    Ok(stored) => Response::json(200, &library::stored_file_json(&stored)),
                    // The rest of the body is not read: the connection is closed after the answer.
                    Err(error) => library_error(error).closing(),
                }
            }
            ["library", "uploads", upload_id, "commit"] => {
                if method != "POST" {
                    return method_not_allowed("POST");
                }
                self.commit(request, body, upload_id)
            }
            ["library", "uploads", upload_id] => {
                if method != "DELETE" {
                    return method_not_allowed("DELETE");
                }
                match self.shared.library.abort_upload(upload_id) {
                    Ok(()) => Response::empty(204),
                    Err(error) => library_error(error),
                }
            }
            ["library", "posts", post_id] => match method {
                "PATCH" => self.edit_post(request, body, post_id),
                "DELETE" => self.delete_post(request, post_id),
                _ => method_not_allowed("PATCH, DELETE"),
            },
            _ => Response::detail(404, messages::NOT_FOUND),
        }
    }

    /// The vendored pages (page listener): the file, or index.html for a path of the app
    /// (its last segment has no dot), else 404.
    fn static_file(&self, request: &Request, segments: &[String]) -> Response {
        if request.method != "GET" && request.method != "HEAD" {
            return Response::text(405, messages::METHOD_NOT_ALLOWED).with_header("Allow", "GET, HEAD");
        }
        let joined = segments.join("/");
        let wanted = if joined.is_empty() { "index.html" } else { joined.as_str() };
        let found = self.shared.assets.iter().find(|file| file.path == wanted).or_else(|| {
            let last = segments.last().map(String::as_str).unwrap_or_default();
            if last.contains('.') {
                None
            } else {
                self.shared.assets.iter().find(|file| file.path == "index.html")
            }
        });
        let Some(file) = found else {
            return Response::text(404, messages::NOT_FOUND);
        };
        let cache = if file.immutable { "public, max-age=31536000, immutable" } else { "no-cache" };
        let base = |response: Response| response.with_header("Content-Type", file.content_type).with_header("ETag", file.etag).with_header("Cache-Control", cache);
        let response = if request.header("if-none-match").is_some_and(|value| value.split(',').any(|tag| tag.trim() == file.etag)) {
            base(Response::empty(304))
        } else {
            base(Response::empty(200)).with_body(ResponseBody::Static(file.bytes))
        };
        if file.content_type.starts_with("text/html") {
            html_headers(response)
        } else {
            response
        }
    }

    /// The CORS preflight of the API listener (Starlette's answer).
    fn preflight(&self, request: &Request, origin: &str) -> Response {
        let method = request.header("access-control-request-method").unwrap_or_default();
        let origin_ok = self.guard.cors_allowed(origin);
        let method_ok = CORS_METHODS.contains(&method);
        if !origin_ok || !method_ok {
            let failures: Vec<&str> = [(!origin_ok).then_some("origin"), (!method_ok).then_some("method")].into_iter().flatten().collect();
            return Response::text(400, &messages::cors_disallowed(&failures.join(", ")));
        }
        let mut response = Response::text(200, "OK")
            .with_header("Access-Control-Allow-Origin", origin)
            .with_header("Vary", "Origin")
            .with_header("Access-Control-Allow-Methods", CORS_METHODS.join(", "))
            .with_header("Access-Control-Max-Age", CORS_MAX_AGE_SECONDS.to_string());
        if let Some(headers) = request.header("access-control-request-headers") {
            response = response.with_header("Access-Control-Allow-Headers", headers);
        }
        response
    }

    /// The answer after the guard.
    fn route(&self, request: &Request, body: &mut BodyReader<'_>) -> Response {
        let segments = match http::path_segments(&request.path) {
            Ok(segments) => segments,
            Err(response) => return response,
        };
        if segments.first().map(String::as_str) == Some("api") {
            return self.api(request, body, &segments[1..]);
        }
        match self.kind {
            ListenerKind::Frontend => self.static_file(request, &segments),
            ListenerKind::Backend => match segments.iter().map(String::as_str).collect::<Vec<_>>().as_slice() {
                [] | ["docs"] => {
                    if request.method != "GET" && request.method != "HEAD" {
                        return method_not_allowed("GET");
                    }
                    html_headers(Response::empty(200).with_header("Content-Type", "text/html; charset=utf-8").with_header("Cache-Control", "no-cache").with_body(ResponseBody::Bytes(docs_page().into_bytes())))
                }
                _ => Response::detail(404, messages::NOT_FOUND),
            },
        }
    }
}

impl Handler for Routes {
    fn handle(&self, request: &Request, body: &mut BodyReader<'_>) -> Response {
        if !self.guard.host_ok(request.header("host")) {
            return Response::detail(400, messages::HOST_NOT_ALLOWED);
        }
        let origin = request.header("origin").map(str::to_owned);
        if self.kind == ListenerKind::Backend && request.method == "OPTIONS" {
            if let (Some(origin), Some(_)) = (&origin, request.header("access-control-request-method")) {
                return self.preflight(request, origin);
            }
        }
        if !guard::is_safe_method(&request.method) && !self.guard.write_ok(request.header("sec-fetch-site"), origin.as_deref(), request.header("host")) {
            return Response::detail(403, messages::WRITE_REFUSED);
        }
        let response = self.route(request, body);
        let response = if guard::is_local_peer(request.peer.ip()) { response } else { for_remote_client(response) };
        match (&origin, self.kind) {
            (Some(origin), ListenerKind::Backend) if self.guard.cors_allowed(origin) => response.with_header("Access-Control-Allow-Origin", origin.as_str()).with_header("Vary", "Origin"),
            _ => response,
        }
    }
}

/// Escapes text for HTML.
fn escape_html(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The routes of the API, as `/docs` lists them: method, path, what it does.
pub const API_ROUTES: [(&str, &str, &str); 10] = [
    ("GET", "/api/health", "{\"status\": \"ok\"}"),
    ("GET", "/api/library", "The posts that are open in Instagram Windows App, as the Instagram clone's library JSON."),
    ("DELETE", "/api/library", "Clears the browser imports folder when nothing else is open (204), else 403."),
    ("GET, HEAD", "/api/library/files/{path}", "A picture or video of an open post (Range requests, ETag); HEIC pictures as JPEG."),
    ("POST", "/api/library/uploads", "Starts saving an import from the browser: {\"upload_id\": \"...\"} (201)."),
    ("PUT", "/api/library/uploads/{upload_id}/files/{name}", "One file of that import (Content-Length required)."),
    ("POST", "/api/library/uploads/{upload_id}/commit", "Saves the import in the browser imports folder and shows it in the app."),
    ("DELETE", "/api/library/uploads/{upload_id}", "Cancels an import that is not committed (204)."),
    ("PATCH", "/api/library/posts/{post_id}", "{\"caption\": \"...\"}: changes the description, saved in descriptions.json of the app."),
    ("DELETE", "/api/library/posts/{post_id}", "Removes a post of the browser imports folder; 403 for every other post."),
];

/// The `/docs` page: the route table as a plain page without scripts.
pub fn docs_page() -> String {
    let rows: String = API_ROUTES
        .iter()
        .map(|(method, path, purpose)| format!("<tr><td><code>{}</code></td><td><code>{}</code></td><td>{}</td></tr>\n", escape_html(method), escape_html(path), escape_html(purpose)))
        .collect();
    format!(
        "<!doctype html>\n<html lang=\"en\">\n<head>\n<meta charset=\"utf-8\">\n<title>{title}</title>\n<style>body{{font:14px/1.5 system-ui,sans-serif;margin:32px;max-width:960px}}table{{border-collapse:collapse}}td,th{{border:1px solid #ccc;padding:6px 10px;text-align:left;vertical-align:top}}code{{font-family:Consolas,monospace}}</style>\n</head>\n<body>\n<h1>{title}</h1>\n<p>{lead}</p>\n<table>\n<tr><th>Method</th><th>Path</th><th>What it does</th></tr>\n{rows}</table>\n<p>{errors}</p>\n</body>\n</html>\n",
        title = escape_html(messages::DOCS_TITLE),
        lead = escape_html(messages::DOCS_LEAD),
        errors = escape_html(messages::DOCS_ERRORS),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn post_edits_are_parsed_like_library_py() {
        assert_eq!(parse_post_changes(br#"{"caption": "  Low tide\r\nagain \u0000 "}"#).ok().as_deref(), Some("Low tide\nagain"));
        let refused = |body: &[u8]| parse_post_changes(body).expect_err("refused");
        assert!(refused(b"not json").detail.starts_with("The request body is not valid JSON: "));
        assert_eq!(refused(b"[1]").detail, "The request body must be a JSON object.");
        assert_eq!(refused(br#"{"caption": "x", "id": "y", "alt": "z"}"#).detail, "\"alt\" cannot be changed; only the caption can.");
        assert_eq!(refused(b"{}").detail, "The request changes nothing: send a caption.");
        assert_eq!(refused(br#"{"caption": 12}"#).detail, "The caption must be a string.");
        let long = format!("{{\"caption\": \"{}\"}}", "a".repeat(10_001));
        assert_eq!(refused(long.as_bytes()).detail, "A caption can be at most 10,000 characters long.");
        assert!(refused(b"[1]").status == 422);
        let exactly = format!("{{\"caption\": \"{}\"}}", "\u{1F30A}".repeat(5_000));
        assert!(parse_post_changes(exactly.as_bytes()).is_ok(), "10,000 UTF-16 units are allowed");
    }

    #[test]
    fn an_error_for_another_computer_names_no_folder() {
        let detail = "The saved descriptions in \"C:\\Users\\Jane Doe\\AppData\\Roaming\\instagram-windows-app\\data\\descriptions.json\" could not be read: Access is denied. (os error 5)";
        let remote = for_remote_client(Response::detail(503, detail));
        let ResponseBody::Bytes(bytes) = &remote.body else { panic!("a JSON body") };
        let text = String::from_utf8_lossy(bytes).into_owned();
        assert!(!text.contains(":\\\\") && !text.contains("Jane"), "{text}");
        let json: Value = serde_json::from_slice(bytes).expect("JSON");
        assert_eq!(json["detail"], Value::from("The saved descriptions in \"descriptions.json\" could not be read: Access is denied. (os error 5)"));
        assert_eq!((remote.status, remote.header("content-type")), (503, Some("application/json")));
        // Answers without a path, and successful ones, are passed on as they are.
        let plain = for_remote_client(Response::detail(404, "The library has no post with this id."));
        assert!(matches!(&plain.body, ResponseBody::Bytes(bytes) if bytes.as_slice() == br#"{"detail":"The library has no post with this id."}"#));
        let ok = for_remote_client(Response::json_bytes(200, br#"{"directory":"C:\\x"}"#.to_vec()));
        assert!(matches!(&ok.body, ResponseBody::Bytes(bytes) if bytes.as_slice() == br#"{"directory":"C:\\x"}"#));
    }

    #[test]
    fn the_docs_page_has_no_scripts() {
        let page = docs_page();
        assert!(page.contains("/api/library/uploads/{upload_id}/commit"));
        assert!(!page.to_lowercase().contains("<script"));
    }
}
