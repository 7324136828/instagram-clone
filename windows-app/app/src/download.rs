//! Downloads (exports) without ever blocking the UI thread.
//!
//! "Download post" and "Download all" write what the reference web project writes: a zip archive
//! with `post.json` and the files of the posts (`instagram_core::export`). Two things take time
//! here, and both run on threads of their own: the native Save dialog, and the export itself,
//! which copies every picture into the archive. The UI thread polls once per frame for what they
//! produced, reads the progress from two counters, and can raise the cancel flag at any time.
//!
//! Only one download exists at a time; `state.rs` keeps what the interface shows of it
//! (`DownloadState`) and decides what happens with the chosen file. This module also answers the
//! questions about the target folder that decision needs: which part files of an earlier
//! download are there, and whether the new one may be split into parts at all.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use instagram_core::export::{self, ExportError, ExportOptions, ExportProgress, ExportResult};
use instagram_core::model::Post;
use instagram_core::poster::PosterCache;
use instagram_core::text;

use crate::loader::Waker;
use crate::strings;

/// The UI thread is woken for a new progress figure at most this often; the figures in between
/// are simply never painted.
const PROGRESS_WAKE_INTERVAL: Duration = Duration::from_millis(60);

/// What the Save dialog is asked for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SaveRequest {
    /// The title of the dialog.
    pub title: String,
    /// The file name it is filled with, e.g. "instagram-export.zip".
    pub file_name: String,
    /// The folder it starts in, when that folder still exists.
    pub folder: Option<PathBuf>,
}

/// What a download produced, for the UI thread.
#[derive(Debug)]
pub enum DownloadEvent {
    /// The Save dialog closed: the chosen file, or None when the user cancelled it.
    Chosen(Option<PathBuf>),
    /// The export ended: what was written, or why nothing was.
    Finished(Result<ExportResult, ExportError>),
}

/// The counters the export thread writes and the UI thread reads.
#[derive(Default)]
struct Progress {
    done: AtomicUsize,
    total: AtomicUsize,
}

/// Runs the Save dialog and the export on background threads and collects their results.
pub struct Downloader {
    sender: mpsc::Sender<DownloadEvent>,
    receiver: mpsc::Receiver<DownloadEvent>,
    waker: Waker,
    /// A Save dialog the state asked for; the window glue shows it, because only it has the
    /// window handle a dialog needs as its parent.
    dialog_request: Option<SaveRequest>,
    dialog_open: bool,
    /// The cancel flag of the running export.
    cancel: Option<Arc<AtomicBool>>,
    progress: Arc<Progress>,
    /// Where the posters of loose videos come from (video brief 5.8); without one they are
    /// exported without a poster.
    posters: Option<PosterCache>,
}

impl Downloader {
    /// A downloader that wakes the UI thread through `waker`.
    pub fn new(waker: Waker) -> Downloader {
        let (sender, receiver) = mpsc::channel();
        Downloader { sender, receiver, waker, dialog_request: None, dialog_open: false, cancel: None, progress: Arc::new(Progress::default()), posters: None }
    }

    /// Gives loose videos a poster from `cache` in every download.
    pub fn set_poster_cache(&mut self, cache: PosterCache) {
        self.posters = Some(cache);
    }

    /// Asks for the Save dialog.
    pub fn request_dialog(&mut self, request: SaveRequest) {
        self.dialog_request = Some(request);
    }

    /// The Save dialog that was asked for and not shown yet. The caller shows it with
    /// `show_dialog`.
    pub fn take_dialog_request(&mut self) -> Option<SaveRequest> {
        self.dialog_request.take()
    }

    /// The Save dialog that was asked for, without taking it (tests look at it).
    pub fn pending_dialog(&self) -> Option<&SaveRequest> {
        self.dialog_request.as_ref()
    }

    /// Builds the native Save dialog for `request`: a .zip filter (which also makes Windows add
    /// the extension to a name typed without one) and the pre-filled name.
    pub fn build_dialog(request: &SaveRequest) -> rfd::FileDialog {
        let mut dialog = rfd::FileDialog::new().set_title(&request.title).add_filter(strings::FILTER_ZIP, &["zip"]).set_file_name(&request.file_name);
        if let Some(folder) = request.folder.as_deref().filter(|folder| folder.is_dir()) {
            dialog = dialog.set_directory(folder);
        }
        dialog
    }

    /// Shows `dialog` on a thread of its own. The choice arrives as `DownloadEvent::Chosen`.
    pub fn show_dialog(&mut self, dialog: rfd::FileDialog) {
        let sender = self.sender.clone();
        let waker = self.waker.clone();
        let spawned = std::thread::Builder::new().name("save-dialog".to_owned()).spawn(move || {
            let _ = sender.send(DownloadEvent::Chosen(dialog.save_file()));
            waker.wake();
        });
        match spawned {
            Ok(_) => self.dialog_open = true,
            Err(error) => {
                log::error!("The dialog thread could not be started: {error}");
                let _ = self.sender.send(DownloadEvent::Chosen(None));
            }
        }
    }

    /// Answers the Save dialog that was asked for without showing one: `choice` is what the
    /// user would have picked. For tests and the screenshot harness, which have no desktop to
    /// show a native dialog on. Returns false when no dialog was asked for.
    pub fn answer_dialog(&mut self, choice: Option<PathBuf>) -> bool {
        if self.dialog_request.take().is_none() {
            return false;
        }
        let _ = self.sender.send(DownloadEvent::Chosen(choice));
        true
    }

    /// True while the Save dialog is on screen or waits to be shown.
    pub fn is_choosing(&self) -> bool {
        self.dialog_open || self.dialog_request.is_some()
    }

    /// True while an export is running.
    pub fn is_running(&self) -> bool {
        self.cancel.is_some()
    }

    /// True while anything of a download is going on in the background.
    pub fn is_busy(&self) -> bool {
        self.is_choosing() || self.is_running()
    }

    /// Starts the export of `posts` into `target_dir` as `{name}.zip` (or its parts). The result
    /// arrives as `DownloadEvent::Finished`.
    pub fn start(&mut self, posts: Vec<Post>, target_dir: PathBuf, name: String) {
        let cancel = Arc::new(AtomicBool::new(false));
        let progress = Arc::new(Progress::default());
        progress.total.store(posts.len(), Ordering::Relaxed);
        self.progress = progress.clone();
        let sender = self.sender.clone();
        let waker = self.waker.clone();
        let flag = cancel.clone();
        let posters = self.posters.clone();
        let spawned = std::thread::Builder::new().name("export".to_owned()).spawn(move || {
            // Copying must never make the interface wait for a core.
            crate::platform::run_below_normal_priority();
            let now_ms = text::now_ms();
            let options = ExportOptions::new(name, now_ms, text::local_offset_minutes(now_ms));
            let last_wake = Mutex::new(Instant::now());
            let mut report = |figures: ExportProgress| {
                progress.done.store(figures.done, Ordering::Relaxed);
                progress.total.store(figures.total, Ordering::Relaxed);
                if let Ok(mut last) = last_wake.lock() {
                    if last.elapsed() >= PROGRESS_WAKE_INTERVAL {
                        *last = Instant::now();
                        waker.wake();
                    }
                }
            };
            let result = match &posters {
                Some(posters) => export::export_posts_with_posters(&posts, &target_dir, &options, posters, &mut report, &flag),
                None => export::export_posts(&posts, &target_dir, &options, &mut report, &flag),
            };
            let _ = sender.send(DownloadEvent::Finished(result));
            waker.wake();
        });
        match spawned {
            Ok(_) => self.cancel = Some(cancel),
            Err(error) => {
                log::error!("The export thread could not be started: {error}");
                let failure = ExportError::new(export::ExportErrorCode::WriteFailed, instagram_core::messages::DOWNLOAD_NOT_PREPARED.to_owned());
                let _ = self.sender.send(DownloadEvent::Finished(Err(failure)));
            }
        }
    }

    /// How far the running export is.
    pub fn progress(&self) -> ExportProgress {
        ExportProgress { done: self.progress.done.load(Ordering::Relaxed), total: self.progress.total.load(Ordering::Relaxed) }
    }

    /// Asks the running export to stop. It ends with `ExportErrorCode::Cancelled` and leaves no
    /// file behind.
    pub fn cancel(&self) {
        if let Some(cancel) = &self.cancel {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// The next finished piece of work, if any. Call until it returns None, once per frame.
    pub fn poll(&mut self) -> Option<DownloadEvent> {
        let event = self.receiver.try_recv().ok()?;
        self.note(&event);
        Some(event)
    }

    fn note(&mut self, event: &DownloadEvent) {
        match event {
            DownloadEvent::Chosen(_) => self.dialog_open = false,
            DownloadEvent::Finished(_) => self.cancel = None,
        }
    }

    /// Cancels the running export and waits until it has removed its temporary files, at most
    /// `timeout`. The window is closing: nothing is shown any more, the result is returned for
    /// the log. None when no export was running or it did not end in time.
    pub fn cancel_and_wait(&mut self, timeout: Duration) -> Option<Result<ExportResult, ExportError>> {
        if !self.is_running() {
            return None;
        }
        self.cancel();
        let deadline = Instant::now() + timeout;
        while let Some(left) = deadline.checked_duration_since(Instant::now()) {
            match self.receiver.recv_timeout(left) {
                Ok(event) => {
                    self.note(&event);
                    if let DownloadEvent::Finished(result) = event {
                        return Some(result);
                    }
                }
                Err(_) => break,
            }
        }
        None
    }
}

/// True when the export of `posts` may be written as several archives: more posts than one
/// part holds, or more bytes. The sizes are those read when the pictures were opened and leave
/// out cover pictures and profile pictures, so an eighth is added: the estimate errs on the
/// side of "may", because whoever asks wants to know whether part files of an earlier download
/// could be replaced.
pub fn may_split(posts: &[Post]) -> bool {
    if posts.len() > export::EXPORT_PART_POSTS {
        return true;
    }
    let bytes: u64 = posts.iter().flat_map(|post| post.media.iter()).map(|media| media.bytes).fold(0, u64::saturating_add);
    bytes.saturating_add(bytes / 8) > export::EXPORT_PART_BYTES
}

/// The number `k` when `file_name` is `{name}-part{k}.zip` (in any letter case, as Windows
/// compares file names).
fn part_number(file_name: &str, name: &str) -> Option<usize> {
    let lower = file_name.to_lowercase();
    let rest = lower.strip_prefix(&name.to_lowercase())?.strip_prefix("-part")?.strip_suffix(".zip")?;
    if rest.is_empty() || !rest.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    rest.parse::<usize>().ok().filter(|number| *number >= 1)
}

/// The part files of an earlier download named `name` in `folder` (`{name}-part{k}.zip`), in
/// the order of their numbers. The Save dialog only asks about `{name}.zip`, so these are what a
/// split download would replace without a question.
pub fn existing_parts(folder: &Path, name: &str) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(folder) else {
        return Vec::new();
    };
    let mut parts: Vec<(usize, PathBuf)> = entries
        .flatten()
        .filter_map(|entry| {
            let file_name = entry.file_name();
            let number = part_number(file_name.to_str()?, name)?;
            entry.file_type().ok().filter(|kind| kind.is_file()).map(|_| (number, entry.path()))
        })
        .collect();
    parts.sort();
    parts.into_iter().map(|(_, path)| path).collect()
}

/// Removes what an earlier download of the same name left in the folder and the finished one
/// did not replace: the `stale` files that are not among the `written` archives. Called only
/// after the user agreed to replace them and the new download succeeded. Returns the files
/// that could not be removed.
pub fn remove_stale(stale: &[PathBuf], written: &[PathBuf]) -> Vec<PathBuf> {
    let same = |left: &Path, right: &Path| left.to_string_lossy().eq_ignore_ascii_case(&right.to_string_lossy());
    let mut kept = Vec::new();
    for file in stale {
        if written.iter().any(|archive| same(archive, file)) {
            continue;
        }
        if let Err(error) = std::fs::remove_file(file) {
            if error.kind() != std::io::ErrorKind::NotFound {
                log::warn!("{} of an earlier download could not be removed: {error}", file.display());
                kept.push(file.clone());
            }
        }
    }
    kept
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn part_files_are_recognised_by_their_number() {
        assert_eq!(part_number("instagram-export-part1.zip", "instagram-export"), Some(1));
        assert_eq!(part_number("Instagram-Export-PART12.ZIP", "instagram-export"), Some(12));
        assert_eq!(part_number("instagram-export.zip", "instagram-export"), None);
        assert_eq!(part_number("instagram-export-part.zip", "instagram-export"), None);
        assert_eq!(part_number("instagram-export-part0.zip", "instagram-export"), None);
        assert_eq!(part_number("instagram-export-part2a.zip", "instagram-export"), None);
        assert_eq!(part_number("instagram-export-part2.zip.tmp", "instagram-export"), None);
        assert_eq!(part_number("other-part2.zip", "instagram-export"), None);
    }

    #[test]
    fn the_parts_of_an_earlier_download_are_found_in_number_order() {
        let folder = instagram_core::test_folder::TestFolder::new("parts").expect("folder");
        for name in ["trip-part10.zip", "trip-part2.zip", "trip-part1.zip", "trip.zip", "trip-part3.txt", "other-part1.zip"] {
            std::fs::write(folder.join(name), b"x").expect("file");
        }
        std::fs::create_dir_all(folder.join("trip-part4.zip")).expect("a folder of that name");
        let names: Vec<String> = existing_parts(&folder, "trip").iter().map(|path| path.file_name().unwrap_or_default().to_string_lossy().into_owned()).collect();
        assert_eq!(names, ["trip-part1.zip", "trip-part2.zip", "trip-part10.zip"]);
        assert!(existing_parts(&folder.join("missing"), "trip").is_empty());

        // After a new download wrote two parts, the third of the earlier one is removed.
        let stale = existing_parts(&folder, "trip");
        let written = [folder.join("trip-part1.zip"), folder.join("TRIP-part2.zip")];
        assert!(remove_stale(&stale, &written).is_empty());
        assert!(folder.join("trip-part1.zip").exists());
        assert!(folder.join("trip-part2.zip").exists());
        assert!(!folder.join("trip-part10.zip").exists());
    }

    #[test]
    fn a_download_may_be_split_by_the_number_of_posts() {
        assert!(!may_split(&[]));
        let post = Post {
            key: instagram_core::model::PostKey::for_file(Path::new("C:\\pictures\\a.jpg")),
            id: "a".to_owned(),
            permalink: None,
            media: vec![instagram_core::model::MediaItem {
                kind: instagram_core::model::MediaKind::Image,
                path: PathBuf::from("C:\\pictures\\a.jpg"),
                manifest_path: "a.jpg".to_owned(),
                poster: None,
                alt: None,
                bytes: 10,
            }],
            user: instagram_core::model::User::named("tester"),
            caption: String::new(),
            original_caption: String::new(),
            comment_count: 0,
            like_count: None,
            taken_at: None,
            media_type: instagram_core::model::MediaType::Image,
            alt: None,
            location: None,
            context: None,
            comments: Vec::new(),
            origin: instagram_core::model::PostOrigin::File,
        };
        assert!(!may_split(std::slice::from_ref(&post)));
        assert!(!may_split(&vec![post.clone(); export::EXPORT_PART_POSTS]));
        assert!(may_split(&vec![post.clone(); export::EXPORT_PART_POSTS + 1]));
        let mut large = post;
        large.media[0].bytes = export::EXPORT_PART_BYTES;
        assert!(may_split(&[large]));
    }
}
