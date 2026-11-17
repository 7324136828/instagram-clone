//! Files over HTTP: byte ranges, ETags and HTTP dates of the file route, and the JPEG copies of
//! HEIC pictures for browsers (web brief 4.6).
//!
//! A browser asks for a video in ranges and for pictures again when it revalidates; the route
//! answers like Starlette's `FileResponse`, which the clone uses: `Accept-Ranges: bytes`, an
//! `ETag` made of the size and the modified time, `Last-Modified`, `304` for a matching
//! `If-None-Match`, `206` for one satisfiable range, `416` for an unsatisfiable one, the whole
//! file for several ranges, a range that cannot be read, or an `If-Range` that does not match.
//!
//! HEIC: no browser but Safari shows HEIC, and the clone converts HEIC to JPEG when it imports.
//! So a HEIC picture is listed with a `.jpg` path and served as a JPEG made from it (orientation
//! applied, at most `WEB_HEIC_MAX_SIDE` pixels per side, quality `HEIC_JPEG_QUALITY`), kept in
//! the app's web cache folder under a name made of the picture's path, size and time. At most
//! `HEIC_CONCURRENCY` conversions run at once; one that takes longer than `HEIC_TIMEOUT_MS` is
//! answered with an error (it finishes on its own thread and is used next time).
//!
//! Posters of loose videos (video brief 5.9): made from the video's first frame by the app's
//! poster cache on the HTTP thread, at most `POSTER_CONCURRENCY` at once; a request waits at
//! most `POSTER_TIMEOUT_MS` for its turn. `GET /api/library` first makes the posters that are
//! not made yet (`PosterService::prepare`, at most `POSTER_LISTING_WAIT_MS`), so that the JSON
//! leaves out the posters of videos no frame can be read from.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant, SystemTime};

use crate::decode;
use crate::messages;
use crate::model::fold_path;
use crate::poster::PosterCache;
use crate::text;

use super::http::{Request, Response, ResponseBody};
use super::snapshot::FormatCache;

/// The longest side of the JPEG made of a HEIC picture.
pub const WEB_HEIC_MAX_SIDE: u32 = 4096;
/// JPEG quality of those copies (the clone's `HEIC_JPEG_QUALITY`).
pub const HEIC_JPEG_QUALITY: u8 = 90;
/// Conversions running at the same time.
pub const HEIC_CONCURRENCY: usize = 2;
/// The longest a request waits for one conversion.
pub const HEIC_TIMEOUT_MS: u64 = 120_000;
/// The web cache folder is cut back to this size at start, oldest files first.
pub const WEB_CACHE_MAX_BYTES: u64 = 1024 * 1024 * 1024;
/// Posters of loose videos made at the same time.
pub const POSTER_CONCURRENCY: usize = 2;
/// The longest a request waits for its turn to have a poster made.
pub const POSTER_TIMEOUT_MS: u64 = 30_000;
/// The longest `GET /api/library` waits for the posters of loose videos that were not made yet
/// (`PosterService::prepare`; about a quarter of a second each, two at a time).
pub const POSTER_LISTING_WAIT_MS: u64 = 5_000;

/// What a `Range` header asks for, for a file of a known size.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RangeRequest {
    /// The whole file (no header, several ranges, or one that cannot be read).
    Whole,
    /// Bytes `start..=end`.
    Part {
        /// The first byte.
        start: u64,
        /// The last byte (inclusive).
        end: u64,
    },
    /// A range that lies beyond the file: 416.
    Unsatisfiable,
}

/// Reads a `Range` header for a file of `size` bytes: `bytes=a-b`, `bytes=a-` or `bytes=-n`.
pub fn parse_range(header: Option<&str>, size: u64) -> RangeRequest {
    let Some(header) = header else {
        return RangeRequest::Whole;
    };
    let Some(spec) = header.trim().strip_prefix("bytes=") else {
        return RangeRequest::Whole;
    };
    if spec.contains(',') {
        return RangeRequest::Whole;
    }
    let Some((first, last)) = spec.trim().split_once('-') else {
        return RangeRequest::Whole;
    };
    let number = |text: &str| -> Option<u64> { (!text.is_empty() && text.chars().all(|character| character.is_ascii_digit())).then(|| text.parse().ok()).flatten() };
    match (first.trim(), last.trim()) {
        ("", suffix) => match number(suffix) {
            None => RangeRequest::Whole,
            Some(0) => RangeRequest::Unsatisfiable,
            Some(_) if size == 0 => RangeRequest::Unsatisfiable,
            Some(length) => RangeRequest::Part { start: size.saturating_sub(length), end: size - 1 },
        },
        (start, "") => match number(start) {
            None => RangeRequest::Whole,
            Some(start) if start >= size => RangeRequest::Unsatisfiable,
            Some(start) => RangeRequest::Part { start, end: size - 1 },
        },
        (start, end) => match (number(start), number(end)) {
            (Some(start), Some(end)) if start <= end => {
                if start >= size {
                    RangeRequest::Unsatisfiable
                } else {
                    RangeRequest::Part { start, end: end.min(size - 1) }
                }
            }
            _ => RangeRequest::Whole,
        },
    }
}

/// The day of the week of a day since 1970-01-01 (a Thursday), as HTTP writes it.
fn weekday(days: i64) -> &'static str {
    ["Thu", "Fri", "Sat", "Sun", "Mon", "Tue", "Wed"][days.rem_euclid(7) as usize]
}

/// An HTTP date (`Sun, 06 Nov 1994 08:49:37 GMT`) of an instant in epoch milliseconds.
pub fn http_date(epoch_ms: i64) -> String {
    const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
    let civil = text::civil_from_epoch_ms(epoch_ms);
    let days = epoch_ms.div_euclid(86_400_000);
    let month = MONTHS.get(civil.month.saturating_sub(1) as usize).copied().unwrap_or("Jan");
    format!("{}, {:02} {month} {:04} {:02}:{:02}:{:02} GMT", weekday(days), civil.day, civil.year, civil.hour, civil.minute, civil.second)
}

/// The ETag of a file: its size and its modified time in milliseconds.
pub fn file_etag(size: u64, modified_ms: i64) -> String {
    format!("\"{size}-{modified_ms}\"")
}

/// The modified time of `metadata` in epoch milliseconds (0 when unknown).
fn modified_ms(metadata: &fs::Metadata) -> i64 {
    metadata.modified().map(text::epoch_ms_from_system_time).unwrap_or(0)
}

/// The answer of the file route for the real file `path` with the content type `mime`: the
/// file must still be a regular file (a link is never followed), else 404 `not_found`.
pub fn serve_file(path: &Path, mime: &str, request: &Request, not_found: &str) -> Response {
    let missing = || Response::detail(404, not_found);
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_file() => {}
        _ => return missing(),
    }
    let Ok(file) = File::open(path) else {
        return missing();
    };
    let Ok(metadata) = file.metadata() else {
        return missing();
    };
    let size = metadata.len();
    let modified = modified_ms(&metadata);
    let etag = file_etag(size, modified);
    // Cross-Origin-Resource-Policy: another web site cannot embed the user's pictures and videos
    // (an <img> on evil.example would load them otherwise). Edge treats another port of the same
    // address as another site here (it refuses http://127.0.0.1:8317/... in a page of
    // http://127.0.0.1:3127/ with ERR_BLOCKED_BY_RESPONSE.NotSameSite), so in practice only a
    // page of the address the file is fetched from may embed it: the vendored pages fetch
    // `/api/library/files/...` from their own page address, and a page served by the API address
    // (only /docs, which embeds nothing) could not embed media of the page address either.
    let common = |response: Response| {
        response
            .with_header("Content-Type", mime)
            .with_header("Accept-Ranges", "bytes")
            .with_header("ETag", etag.clone())
            .with_header("Last-Modified", http_date(modified))
            .with_header("Cache-Control", "no-cache")
            .with_header("Cross-Origin-Resource-Policy", "same-site")
    };
    if request.header("if-none-match").is_some_and(|value| value.split(',').any(|tag| tag.trim() == etag || tag.trim() == "*")) {
        return common(Response::empty(304));
    }
    let range = match request.header("if-range") {
        Some(condition) if condition.trim() != etag => RangeRequest::Whole,
        _ => parse_range(request.header("range"), size),
    };
    match range {
        RangeRequest::Whole => common(Response::empty(200)).with_body(ResponseBody::File { file, offset: 0, length: size }),
        RangeRequest::Part { start, end } => common(Response::empty(206))
            .with_header("Content-Range", format!("bytes {start}-{end}/{size}"))
            .with_body(ResponseBody::File { file, offset: start, length: end - start + 1 }),
        RangeRequest::Unsatisfiable => Response::detail(416, messages::RANGE_NOT_SATISFIABLE).with_header("Content-Range", format!("bytes */{size}")),
    }
}

/// What turns a HEIC picture (`source`) into its JPEG copy (`destination`, through a temporary
/// file in `folder`): `convert_to_jpeg`, or a test's stand-in.
pub type HeicConverter = dyn Fn(&Path, &Path, &Path) -> Result<(), String> + Send + Sync;

/// One conversion in progress, which every request for the same copy waits for.
#[derive(Debug, Default)]
struct Flight {
    result: Mutex<Option<Result<PathBuf, String>>>,
    done: Condvar,
}

/// The JPEG copies of HEIC pictures.
pub struct HeicCache {
    folder: PathBuf,
    running: Mutex<usize>,
    freed: Condvar,
    /// Conversions in progress, by the copy they make: a request for a copy that is being made
    /// waits for that conversion instead of starting its own (60 requests for one picture would
    /// otherwise convert it again and again).
    flights: Mutex<HashMap<PathBuf, Arc<Flight>>>,
    convert: Box<HeicConverter>,
}

impl std::fmt::Debug for HeicCache {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("HeicCache").field("folder", &self.folder).finish_non_exhaustive()
    }
}

/// Gives a conversion slot back when the conversion ends, however it ends.
struct Slot(Arc<HeicCache>);

impl Drop for Slot {
    fn drop(&mut self) {
        let mut running = self.0.running.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *running = running.saturating_sub(1);
        self.0.freed.notify_one();
    }
}

impl HeicCache {
    /// The cache in `folder` (created when the first copy is written).
    pub fn new(folder: PathBuf) -> Arc<HeicCache> {
        HeicCache::with_converter(folder, Box::new(convert_to_jpeg))
    }

    /// The cache with another converter (tests).
    pub fn with_converter(folder: PathBuf, convert: Box<HeicConverter>) -> Arc<HeicCache> {
        Arc::new(HeicCache { folder, running: Mutex::new(0), freed: Condvar::new(), flights: Mutex::new(HashMap::new()), convert })
    }

    /// Ends a flight with `result` for everyone who waits for it.
    fn land(&self, target: &Path, flight: &Arc<Flight>, result: Result<PathBuf, String>) {
        *flight.result.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(result);
        flight.done.notify_all();
        let mut flights = self.flights.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        if flights.get(target).is_some_and(|current| Arc::ptr_eq(current, flight)) {
            flights.remove(target);
        }
    }

    /// Waits until `flight` has landed or `deadline` passed.
    fn wait_for(flight: &Flight, deadline: Instant) -> Result<PathBuf, String> {
        let mut result = flight.result.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        loop {
            if let Some(result) = result.as_ref() {
                return result.clone();
            }
            let wait = deadline.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                return Err(messages::CONVERSION_TIMED_OUT.to_owned());
            }
            result = flight.done.wait_timeout(result, wait).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
        }
    }

    /// The file of the copy of `source`, by its folded path, size and modified time.
    fn copy_path(&self, source: &Path) -> io::Result<PathBuf> {
        let metadata = fs::metadata(source)?;
        let key = format!("{}|{}|{}", fold_path(source), metadata.len(), modified_ms(&metadata));
        Ok(self.folder.join(format!("heic-{:016x}.jpg", text::fnv1a_64(key.as_bytes()))))
    }

    /// The JPEG copy of the HEIC picture `source`, made now if there is none. `Err` holds the
    /// reason for the user (`messages::picture_not_for_browser` wraps it).
    pub fn converted(self: &Arc<Self>, source: &Path) -> Result<PathBuf, String> {
        self.converted_within(source, Duration::from_millis(HEIC_TIMEOUT_MS))
    }

    /// `converted` with the time limit as a parameter. One conversion runs per copy: the first
    /// request starts it (when one of `HEIC_CONCURRENCY` slots is free), later ones wait for it.
    /// A conversion that outlasts a request's limit finishes on its own thread for the next one.
    pub fn converted_within(self: &Arc<Self>, source: &Path, limit: Duration) -> Result<PathBuf, String> {
        let target = self.copy_path(source).map_err(|error| error.to_string())?;
        if target.is_file() {
            return Ok(target);
        }
        let deadline = Instant::now() + limit;
        let (flight, leader) = {
            let mut flights = self.flights.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            match flights.get(&target) {
                Some(flight) => (Arc::clone(flight), false),
                None => {
                    let flight = Arc::new(Flight::default());
                    flights.insert(target.clone(), Arc::clone(&flight));
                    (flight, true)
                }
            }
        };
        if !leader {
            return Self::wait_for(&flight, deadline);
        }
        {
            let mut running = self.running.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            while *running >= HEIC_CONCURRENCY {
                let wait = deadline.saturating_duration_since(Instant::now());
                if wait.is_zero() {
                    drop(running);
                    let timed_out = Err(messages::CONVERSION_TIMED_OUT.to_owned());
                    self.land(&target, &flight, timed_out.clone());
                    return timed_out;
                }
                running = self.freed.wait_timeout(running, wait).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
            }
            *running += 1;
        }
        let slot = Slot(Arc::clone(self));
        // A copy finished by a flight that landed just before this one started is used as it is.
        if target.is_file() {
            drop(slot);
            self.land(&target, &flight, Ok(target.clone()));
            return Ok(target);
        }
        let cache = Arc::clone(self);
        let source = source.to_path_buf();
        let destination = target.clone();
        let carried = Arc::clone(&flight);
        let spawned = std::thread::Builder::new().name("heic-to-jpeg".to_owned()).spawn(move || {
            let result = (cache.convert)(&source, &cache.folder, &destination).map(|()| destination.clone());
            drop(slot);
            cache.land(&destination, &carried, result);
        });
        if let Err(error) = spawned {
            let failed = Err(error.to_string());
            self.land(&target, &flight, failed.clone());
            return failed;
        }
        Self::wait_for(&flight, deadline)
    }

    /// Cuts the folder back to `max_bytes`, oldest copies first, and removes copies that were
    /// never finished. Run once at start, on a thread of its own.
    pub fn clean_up(&self, max_bytes: u64) {
        let Ok(entries) = fs::read_dir(&self.folder) else {
            return;
        };
        let mut copies: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(metadata) = entry.metadata() else { continue };
            if !metadata.is_file() || !name.starts_with("heic-") {
                continue;
            }
            if name.contains(".tmp-") {
                let _ = fs::remove_file(entry.path());
                continue;
            }
            copies.push((metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH), metadata.len(), entry.path()));
        }
        copies.sort();
        let mut total: u64 = copies.iter().map(|(_, size, _)| size).sum();
        for (_, size, path) in copies {
            if total <= max_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= size;
            }
        }
    }
}

/// The posters of loose videos for browsers: the app's poster cache with a limit on how many are
/// made at once.
#[derive(Debug)]
pub struct PosterService {
    cache: PosterCache,
    running: Mutex<usize>,
    freed: Condvar,
    /// Posters made ahead of a listing (`prepare`).
    ahead: Mutex<Ahead>,
    /// Told when a poster made ahead is done.
    made: Condvar,
}

/// The posters `PosterService::prepare` was asked for and has not made yet.
#[derive(Debug, Default)]
struct Ahead {
    queue: VecDeque<(PathBuf, Arc<FormatCache>)>,
    /// Folded paths of the videos queued or being made.
    pending: HashSet<String>,
    workers: usize,
}

/// Why a poster could not be served.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PosterFailure {
    /// The reason (`messages::picture_not_for_browser` wraps it).
    pub reason: String,
    /// True when the video itself gave no poster (it will not give one next time either);
    /// false when the request only waited too long for its turn.
    pub lasting: bool,
}

impl PosterService {
    /// The service over `cache`.
    pub fn new(cache: PosterCache) -> Arc<PosterService> {
        Arc::new(PosterService { cache, running: Mutex::new(0), freed: Condvar::new(), ahead: Mutex::new(Ahead::default()), made: Condvar::new() })
    }

    /// True when the poster of `video` is in the cache.
    fn is_made(&self, video: &Path) -> bool {
        self.cache.poster_path(video).is_ok_and(|poster| poster.is_file())
    }

    /// Makes the posters of `videos` that are not made yet, on at most `POSTER_CONCURRENCY`
    /// threads of its own, and waits for them for at most `budget`. A video no poster can be
    /// made of is recorded in `formats`, so the library JSON built afterwards lists it without
    /// one: a page then never asks for a poster that cannot come (a damaged video would
    /// otherwise show a broken picture and log a 404 on the first page load). Posters still
    /// being made when the budget is up are listed, and finish for the requests that follow.
    pub fn prepare(self: &Arc<Self>, videos: &[PathBuf], formats: &Arc<FormatCache>, budget: Duration) {
        let deadline = Instant::now() + budget;
        let mut mine: Vec<String> = Vec::new();
        {
            let mut ahead = self.ahead.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            for video in videos {
                if formats.poster_failed(video) || self.is_made(video) {
                    continue;
                }
                let key = fold_path(video);
                if ahead.pending.insert(key.clone()) {
                    ahead.queue.push_back((video.clone(), Arc::clone(formats)));
                }
                mine.push(key);
            }
            if mine.is_empty() {
                return;
            }
            while ahead.workers < POSTER_CONCURRENCY && ahead.workers < ahead.queue.len() {
                let service = Arc::clone(self);
                if std::thread::Builder::new().name("poster-ahead".to_owned()).spawn(move || service.make_ahead()).is_err() {
                    break;
                }
                ahead.workers += 1;
            }
            if ahead.workers == 0 {
                // No thread could be started: the posters are made on request, as before.
                for key in &mine {
                    ahead.pending.remove(key);
                }
                ahead.queue.clear();
                return;
            }
        }
        let mut ahead = self.ahead.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        while mine.iter().any(|key| ahead.pending.contains(key)) {
            let wait = deadline.saturating_duration_since(Instant::now());
            if wait.is_zero() {
                return;
            }
            ahead = self.made.wait_timeout(ahead, wait).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
        }
    }

    /// A `poster-ahead` thread: makes queued posters until the queue is empty.
    fn make_ahead(&self) {
        loop {
            let next = {
                let mut ahead = self.ahead.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
                match ahead.queue.pop_front() {
                    Some(next) => next,
                    None => {
                        ahead.workers = ahead.workers.saturating_sub(1);
                        return;
                    }
                }
            };
            let (video, formats) = next;
            if let Err(failure) = self.poster(&video) {
                if failure.lasting {
                    formats.record_poster_failure(&video);
                }
            }
            self.ahead.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).pending.remove(&fold_path(&video));
            self.made.notify_all();
        }
    }

    /// The poster file of the loose video at `video`, made now if there is none.
    pub fn poster(&self, video: &Path) -> Result<PathBuf, PosterFailure> {
        if let Ok(existing) = self.cache.poster_path(video) {
            if existing.is_file() {
                return Ok(existing);
            }
        }
        let deadline = Instant::now() + Duration::from_millis(POSTER_TIMEOUT_MS);
        {
            let mut running = self.running.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            while *running >= POSTER_CONCURRENCY {
                let wait = deadline.saturating_duration_since(Instant::now());
                if wait.is_zero() {
                    return Err(PosterFailure { reason: messages::CONVERSION_TIMED_OUT.to_owned(), lasting: false });
                }
                running = self.freed.wait_timeout(running, wait).unwrap_or_else(|poisoned| poisoned.into_inner()).0;
            }
            *running += 1;
        }
        let result = self.cache.ensure_sniffed(video).map_err(|error| PosterFailure { reason: error.message, lasting: true });
        let mut running = self.running.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        *running = running.saturating_sub(1);
        self.freed.notify_one();
        result
    }
}

/// Decodes `source` and writes it as a JPEG to `destination` (through a temporary file in
/// `folder`, so a half-written copy is never served).
fn convert_to_jpeg(source: &Path, folder: &Path, destination: &Path) -> Result<(), String> {
    let decoded = decode::decode_picture(source, WEB_HEIC_MAX_SIDE).map_err(|error| error.message)?;
    // JPEG has no alpha channel; HEIC photos have none either.
    let rgb: Vec<u8> = decoded.rgba.as_chunks::<4>().0.iter().flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect();
    let mut jpeg: Vec<u8> = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, HEIC_JPEG_QUALITY)
        .encode(&rgb, decoded.width, decoded.height, image::ExtendedColorType::Rgb8)
        .map_err(|error| error.to_string())?;
    fs::create_dir_all(folder).map_err(|error| error.to_string())?;
    let name = destination.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let temporary = folder.join(format!("{name}.tmp-{}", super::random_hex(4)));
    fs::write(&temporary, &jpeg).map_err(|error| error.to_string())?;
    if let Err(error) = fs::rename(&temporary, destination) {
        let _ = fs::remove_file(&temporary);
        // Another request may have finished the same copy first.
        if !destination.is_file() {
            return Err(error.to_string());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ranges_follow_the_rules_of_a_file_response() {
        assert_eq!(parse_range(None, 100), RangeRequest::Whole);
        assert_eq!(parse_range(Some("bytes=0-9"), 100), RangeRequest::Part { start: 0, end: 9 });
        assert_eq!(parse_range(Some("bytes=90-"), 100), RangeRequest::Part { start: 90, end: 99 });
        assert_eq!(parse_range(Some("bytes=-10"), 100), RangeRequest::Part { start: 90, end: 99 });
        assert_eq!(parse_range(Some("bytes=-500"), 100), RangeRequest::Part { start: 0, end: 99 });
        assert_eq!(parse_range(Some("bytes=50-500"), 100), RangeRequest::Part { start: 50, end: 99 });
        assert_eq!(parse_range(Some("bytes=100-"), 100), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=200-300"), 100), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=-0"), 100), RangeRequest::Unsatisfiable);
        assert_eq!(parse_range(Some("bytes=0-1,5-6"), 100), RangeRequest::Whole);
        assert_eq!(parse_range(Some("bytes=9-0"), 100), RangeRequest::Whole);
        assert_eq!(parse_range(Some("items=0-9"), 100), RangeRequest::Whole);
        assert_eq!(parse_range(Some("bytes=a-b"), 100), RangeRequest::Whole);
        assert_eq!(parse_range(Some("bytes=0-0"), 0), RangeRequest::Unsatisfiable);
    }

    #[test]
    fn requests_for_one_picture_wait_for_one_conversion() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let folder = crate::test_folder::TestFolder::new("heic-flight").expect("a test folder");
        let source = folder.join("photo.heic");
        fs::write(&source, b"not really a HEIC").expect("a picture");
        let conversions = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&conversions);
        let cache = HeicCache::with_converter(
            folder.join("web-cache"),
            Box::new(move |_source: &Path, folder: &Path, destination: &Path| {
                counted.fetch_add(1, Ordering::SeqCst);
                std::thread::sleep(Duration::from_millis(200));
                fs::create_dir_all(folder).map_err(|error| error.to_string())?;
                fs::write(destination, b"\xFF\xD8\xFF").map_err(|error| error.to_string())
            }),
        );
        let requests: Vec<_> = (0..10)
            .map(|_| {
                let (cache, source) = (Arc::clone(&cache), source.clone());
                std::thread::spawn(move || cache.converted_within(&source, Duration::from_secs(20)))
            })
            .collect();
        let copies: Vec<PathBuf> = requests.into_iter().map(|request| request.join().expect("a request").expect("the copy")).collect();
        assert_eq!(conversions.load(Ordering::SeqCst), 1, "one conversion for ten requests");
        assert!(copies.iter().all(|copy| copy == &copies[0] && copy.is_file()));
        assert_eq!(cache.converted_within(&source, Duration::from_secs(1)), Ok(copies[0].clone()), "later requests use the copy");
        assert_eq!(conversions.load(Ordering::SeqCst), 1);
        assert!(cache.flights.lock().expect("flights").is_empty(), "nothing is left in flight");
    }

    #[test]
    fn http_dates_are_written_in_gmt() {
        assert_eq!(http_date(784_111_777_000), "Sun, 06 Nov 1994 08:49:37 GMT");
        assert_eq!(http_date(0), "Thu, 01 Jan 1970 00:00:00 GMT");
        assert_eq!(file_etag(12, 34), "\"12-34\"");
    }
}
