//! Opening pictures without ever blocking the UI thread.
//!
//! Three things take time: a native file dialog (it runs its own modal loop for as long as the
//! user looks at it), reading a folder (one open and 256 bytes per file, slow on a network
//! share) and unpacking an archive into the archive cache. All run on short-lived threads here;
//! the UI thread polls for what they produced once per frame. Only one open runs at a time: a
//! request that arrives meanwhile waits and replaces any earlier waiting one, because the user's
//! latest choice is the one that counts.
//!
//! What a request does to what is on screen (web brief 3.5) is decided on the opener thread,
//! which gets a copy of the collection on screen: loose picture files join loose pictures,
//! export folders and archives join exports, and everything else replaces the collection.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use instagram_core::archive;
use instagram_core::catalog::{self, AddOutcome, AddedSources, OpenContext, OpenError, OpenOptions, OpenRequest, Opened, RequestKind};
use instagram_core::model::Collection;
use instagram_core::store::DescriptionStore;

use crate::loader::Waker;
use crate::platform;
use crate::strings;

/// What an open does to the collection on screen (`Opener::decide`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenWay {
    /// Loose pictures are added to the loose pictures on screen.
    AddPictures,
    /// Export folders and archives join the exports on screen.
    AddExports,
    /// A new collection replaces the one on screen.
    Replace,
}

/// Which native dialog to show.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DialogKind {
    /// "Open pictures": several files.
    Pictures,
    /// "Open folder": one folder.
    Folder,
}

/// What an open produced, for the UI thread.
pub enum OpenEvent {
    /// The user chose these paths in a dialog.
    Picked(Vec<PathBuf>),
    /// A new collection replaces the one on screen.
    Opened(Result<Opened, OpenError>),
    /// Pictures were added to the loose collection on screen; the collection is the updated one.
    Added {
        /// `AppState::collection_serial` when the pictures were asked for: the result extends
        /// that collection, and only that one.
        base_serial: u64,
        /// The extended collection, or why nothing could be added.
        result: Result<(Collection, AddOutcome), OpenError>,
    },
    /// Export folders or archives joined the exports on screen; the collection is the merged one.
    Merged {
        /// `AppState::collection_serial` when they were asked for (see `Added`).
        base_serial: u64,
        /// The merged collection with the leases of its new archives, or why nothing could be
        /// added.
        result: Result<(Collection, AddedSources), OpenError>,
    },
}

/// Cleans up the archive cache on a thread of its own (`ArchiveCache::clean_up`): at the start of
/// the app and after an archive was unpacked. Nothing waits for it, neither a frame nor the exit;
/// a clean-up cut short by the end of the process leaves nothing half usable (entry.json goes
/// first).
pub fn clean_up_cache_in_background(cache: archive::cache::ArchiveCache) {
    let spawned = std::thread::Builder::new().name("archive-cache-clean-up".to_owned()).spawn(move || {
        let report = cache.clean_up(instagram_core::text::now_ms());
        log::info!(
            "Archive cache: {} folders removed, {} kept in {}.",
            report.removed.len(),
            instagram_core::text::format_bytes(report.kept_bytes),
            cache.root().display()
        );
    });
    if let Err(error) = spawned {
        log::warn!("The clean-up of the archive cache could not be started: {error}");
    }
}

/// Cuts the poster cache of loose videos back to `poster::POSTER_CACHE_MAX_BYTES`, oldest first,
/// on a thread of its own at the start of the app (video brief 5.2). Nothing waits for it.
pub fn clean_up_posters_in_background(cache: instagram_core::poster::PosterCache) {
    let spawned = std::thread::Builder::new().name("poster-cache-clean-up".to_owned()).spawn(move || {
        let removed = cache.clean_up(instagram_core::poster::POSTER_CACHE_MAX_BYTES);
        if removed > 0 {
            log::info!("Poster cache: {removed} files removed from {}.", cache.dir().display());
        }
    });
    if let Err(error) = spawned {
        log::warn!("The clean-up of the poster cache could not be started: {error}");
    }
}

/// The filters of the "Open pictures" dialog, in their order: name and extensions. The archive
/// endings come from the core; rfd writes `tar.gz` as the pattern `*.tar.gz`.
pub fn picture_dialog_filters() -> Vec<(&'static str, Vec<&'static str>)> {
    let pictures: Vec<&'static str> = catalog::PICTURE_EXTENSIONS.iter().chain(&catalog::VIDEO_EXTENSIONS).copied().collect();
    let archives: Vec<&'static str> = archive::ARCHIVE_EXTENSIONS.to_vec();
    vec![
        (strings::FILTER_PICTURES_AND_EXPORTS, pictures.iter().chain(&archives).copied().collect()),
        (strings::FILTER_PICTURES, pictures),
        (strings::FILTER_EXPORT_ARCHIVES, archives),
        (strings::FILTER_ALL_FILES, vec!["*"]),
    ]
}

/// What a background thread sends back. A finished open carries a whole collection, so it is
/// boxed to keep the message small.
enum Message {
    DialogClosed(Option<Vec<PathBuf>>),
    Finished(Box<OpenEvent>),
}

/// Starts dialogs and opens on background threads and collects their results.
pub struct Opener {
    sender: mpsc::Sender<Message>,
    receiver: mpsc::Receiver<Message>,
    waker: Waker,
    /// A dialog the views asked for; the window glue shows it, because only it has the window
    /// handle a dialog needs as its parent.
    dialog_request: Option<DialogKind>,
    dialog_open: bool,
    running: bool,
    /// Paths that arrived while an open was running.
    waiting: Option<Vec<PathBuf>>,
}

impl Opener {
    /// An opener that wakes the UI thread through `waker`.
    pub fn new(waker: Waker) -> Opener {
        let (sender, receiver) = mpsc::channel();
        Opener { sender, receiver, waker, dialog_request: None, dialog_open: false, running: false, waiting: None }
    }

    /// Asks for a native dialog. Ignored while one is open: a second dialog over the first would
    /// only confuse.
    pub fn request_dialog(&mut self, kind: DialogKind) {
        if !self.dialog_open {
            self.dialog_request = Some(kind);
        }
    }

    /// The dialog that was asked for and not shown yet. The caller shows it with `show_dialog`.
    pub fn take_dialog_request(&mut self) -> Option<DialogKind> {
        self.dialog_request.take()
    }

    /// The dialog that was asked for, without taking it (tests look at it).
    pub fn pending_dialog(&self) -> Option<DialogKind> {
        self.dialog_request
    }

    /// Builds the native dialog for `kind`. `folder` is the folder opened last; see
    /// `start_folder` for where the dialog starts.
    pub fn build_dialog(kind: DialogKind, folder: Option<&Path>) -> rfd::FileDialog {
        let mut dialog = match kind {
            DialogKind::Pictures => picture_dialog_filters()
                .into_iter()
                .fold(rfd::FileDialog::new().set_title(strings::DIALOG_OPEN_PICTURES), |dialog, (name, extensions)| dialog.add_filter(name, &extensions)),
            DialogKind::Folder => rfd::FileDialog::new().set_title(strings::DIALOG_OPEN_FOLDER),
        };
        if let Some(folder) = Opener::start_folder(folder, platform::pictures_folder()) {
            dialog = dialog.set_directory(folder);
        }
        dialog
    }

    /// Where an Open dialog starts: the folder opened last while it exists, else the user's
    /// Pictures folder (`pictures`), as in any picture viewer. `None` leaves it to Windows.
    pub fn start_folder(last: Option<&Path>, pictures: Option<PathBuf>) -> Option<PathBuf> {
        last.filter(|folder| folder.is_dir()).map(Path::to_path_buf).or(pictures)
    }

    /// Shows `dialog` on a thread of its own. The choice arrives as `OpenEvent::Picked`; a
    /// cancelled dialog produces nothing.
    pub fn show_dialog(&mut self, kind: DialogKind, dialog: rfd::FileDialog) {
        let sender = self.sender.clone();
        let waker = self.waker.clone();
        let spawned = std::thread::Builder::new().name("file-dialog".to_owned()).spawn(move || {
            let picked = match kind {
                DialogKind::Pictures => dialog.pick_files(),
                DialogKind::Folder => dialog.pick_folder().map(|folder| vec![folder]),
            };
            let _ = sender.send(Message::DialogClosed(picked));
            waker.wake();
        });
        match spawned {
            Ok(_) => self.dialog_open = true,
            Err(error) => log::error!("The dialog thread could not be started: {error}"),
        }
    }

    /// True while a native dialog is on screen.
    pub fn is_dialog_open(&self) -> bool {
        self.dialog_open
    }

    /// True while a folder is being read.
    pub fn is_running(&self) -> bool {
        self.running
    }

    /// What opening `paths` (classified as `kind`) does while `current` is on screen (design
    /// brief "Opening pictures", web brief 3.5): loose picture files join loose pictures, export
    /// folders and archives join exports, anything else replaces what is shown. It reads
    /// nothing: `kind` is `catalog::classify_request` of the paths, and `all_files` says whether
    /// every path is a file.
    pub fn decide(kind: &RequestKind, all_files: bool, current: Option<&Collection>) -> OpenWay {
        match (current, kind) {
            (Some(collection), RequestKind::Loose) if !collection.is_export() && all_files => OpenWay::AddPictures,
            (Some(collection), RequestKind::Exports { .. }) if collection.is_export() => OpenWay::AddExports,
            _ => OpenWay::Replace,
        }
    }

    /// Opens `paths` in the background. `current` is the collection on screen with its
    /// `collection_serial`; a copy of it goes to the opener thread, which decides (`decide`)
    /// whether the paths join it or replace it. While another open is running the paths wait
    /// for it to finish.
    pub fn open(&mut self, paths: Vec<PathBuf>, current: Option<(&Collection, u64)>, options: &OpenOptions, store: &DescriptionStore, context: &OpenContext) {
        if paths.is_empty() {
            return;
        }
        if self.running {
            self.waiting = Some(paths);
            return;
        }
        let sender = self.sender.clone();
        let waker = self.waker.clone();
        let options = options.clone();
        let store = store.clone();
        let context = context.clone();
        let current = current.map(|(collection, serial)| (collection.clone(), serial));
        let spawned = std::thread::Builder::new().name("open-pictures".to_owned()).spawn(move || {
            let kind = catalog::classify_request(&paths);
            let all_files = paths.iter().all(|path| path.is_file());
            let way = Opener::decide(&kind, all_files, current.as_ref().map(|(collection, _)| collection));
            let event = match (way, current, kind) {
                (OpenWay::AddPictures, Some((mut collection, base_serial)), _) => {
                    let outcome = catalog::add_loose_files(&mut collection, &paths, &options, &store);
                    OpenEvent::Added { base_serial, result: outcome.map(|outcome| (collection, outcome)) }
                }
                (OpenWay::AddExports, Some((mut collection, base_serial)), RequestKind::Exports { sources, others }) => {
                    let added = catalog::add_sources(&mut collection, &paths, sources, &others, &store, &context);
                    OpenEvent::Merged { base_serial, result: added.map(|added| (collection, added)) }
                }
                (_, _, kind) => OpenEvent::Opened(catalog::open_classified(&OpenRequest { paths, options }, kind, &store, &context)),
            };
            let _ = sender.send(Message::Finished(Box::new(event)));
            waker.wake();
        });
        match spawned {
            Ok(_) => self.running = true,
            Err(error) => log::error!("The thread that opens pictures could not be started: {error}"),
        }
    }

    /// The next finished piece of work, if any. Call until it returns None, once per frame.
    pub fn poll(&mut self) -> Option<OpenEvent> {
        loop {
            match self.receiver.try_recv().ok()? {
                Message::DialogClosed(picked) => {
                    self.dialog_open = false;
                    if let Some(paths) = picked.filter(|paths| !paths.is_empty()) {
                        return Some(OpenEvent::Picked(paths));
                    }
                }
                Message::Finished(event) => {
                    self.running = false;
                    return Some(*event);
                }
            }
        }
    }

    /// Paths that arrived while an open was running, once that open has finished.
    pub fn take_waiting(&mut self) -> Option<Vec<PathBuf>> {
        if self.running {
            None
        } else {
            self.waiting.take()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use instagram_core::catalog::SourceRequest;
    use instagram_core::model::{CollectionSource, ExportKind, ExportSource};

    fn collection(source: CollectionSource) -> Collection {
        Collection { posts: Vec::new(), source, label: String::new(), folder: None, warnings: Vec::new(), source_url: None, total_entries: 0 }
    }

    #[test]
    fn loose_files_join_pictures_exports_join_exports_and_the_rest_replaces() {
        let loose = collection(CollectionSource::Loose);
        let export_source = ExportSource {
            kind: ExportKind::Archive,
            path: PathBuf::from("a.zip"),
            manifest: PathBuf::from("post.json"),
            name: "a.zip".to_owned(),
            opened_at: 0,
            exported_at: None,
            source_url: None,
            total_entries: 1,
            shown: 1,
            replaced: 0,
        };
        let exports = collection(CollectionSource::Exports(vec![export_source]));
        let archive = RequestKind::Exports { sources: vec![SourceRequest::Archive { archive: PathBuf::from("b.zip") }], others: Vec::new() };
        assert_eq!(Opener::decide(&RequestKind::Loose, true, Some(&loose)), OpenWay::AddPictures);
        assert_eq!(Opener::decide(&RequestKind::Loose, false, Some(&loose)), OpenWay::Replace, "a folder replaces");
        assert_eq!(Opener::decide(&archive, true, Some(&exports)), OpenWay::AddExports);
        assert_eq!(Opener::decide(&archive, true, Some(&loose)), OpenWay::Replace, "an archive replaces loose pictures");
        assert_eq!(Opener::decide(&RequestKind::Loose, true, Some(&exports)), OpenWay::Replace, "pictures replace exports");
        assert_eq!(Opener::decide(&archive, true, None), OpenWay::Replace);
        assert_eq!(Opener::decide(&RequestKind::Loose, true, None), OpenWay::Replace);
    }

    #[test]
    fn the_open_dialog_offers_pictures_and_exports_first() {
        let filters = picture_dialog_filters();
        let names: Vec<&str> = filters.iter().map(|(name, _)| *name).collect();
        assert_eq!(names, ["Pictures, videos and exports", "Pictures and videos", "Export archives", "All files"]);
        assert_eq!(filters[0].1.len(), catalog::PICTURE_EXTENSIONS.len() + catalog::VIDEO_EXTENSIONS.len() + 4);
        assert!(filters[0].1.contains(&"tar.gz") && filters[0].1.contains(&"jpg") && filters[0].1.contains(&"mp4"));
        assert_eq!(filters[1].1.len(), catalog::PICTURE_EXTENSIONS.len() + catalog::VIDEO_EXTENSIONS.len());
        assert!(filters[1].1.contains(&"webm") && filters[1].1.contains(&"m4v") && filters[1].1.contains(&"mov"));
        assert_eq!(filters[2].1, ["zip", "tar", "tgz", "tar.gz"]);
        assert_eq!(filters[3].1, ["*"]);
    }

    #[test]
    fn the_first_open_dialog_starts_in_the_pictures_folder() {
        let folder = instagram_core::test_folder::TestFolder::new("opening").expect("a test folder");
        let home = folder.to_path_buf();
        let pictures = home.join("Pictures");
        std::fs::create_dir_all(&pictures).expect("a Pictures folder");
        let found = platform::first_folder_in(&home, &["Pictures"]);
        assert_eq!(found.as_deref(), Some(pictures.as_path()));
        // Nothing opened yet: Pictures. The folder opened last wins while it exists.
        assert_eq!(Opener::start_folder(None, found.clone()), Some(pictures.clone()));
        assert_eq!(Opener::start_folder(Some(&home), found.clone()), Some(home.clone()));
        assert_eq!(Opener::start_folder(Some(&home.join("gone")), found), Some(pictures));
        assert_eq!(Opener::start_folder(None, None), None);
    }
}
