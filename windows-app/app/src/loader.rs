//! Pictures for the screen: background decoding, a request queue with priorities, and a texture
//! cache with a byte budget.
//!
//! Decoding blocks for tens to hundreds of milliseconds and the Windows Imaging Component path
//! enters COM as a multithreaded apartment, so `instagram_core::decode` is only ever called on
//! the worker threads here, never on the UI thread. Every frame the views ask for what they show
//! (`size_of`, `image`); a request that is not repeated for a few frames is dropped before a
//! worker starts it, so scrolling through thousands of pictures never decodes what has already
//! left the screen. Finished pictures become textures; when their total size exceeds the budget
//! the least recently used ones are released. A picture that cannot be decoded is remembered with
//! its reason, a complete sentence that names the file.
//!
//! A loose video has no poster of its own: `poster_of` has a worker make one from its first frame
//! (`instagram_core::poster::PosterCache`, video brief 5.2) and hands back the JPEG, which is
//! then decoded like any picture.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Condvar, Mutex, MutexGuard, OnceLock};
use std::thread::JoinHandle;

use egui::{ColorImage, TextureHandle, TextureOptions};
use instagram_core::decode;
use instagram_core::manifest::IMAGE_CONCURRENCY;
use instagram_core::poster::PosterCache;

/// The textures of decoded pictures may take this much memory before the least recently used are
/// released. A single picture larger than this is still shown.
pub const TEXTURE_BUDGET_BYTES: usize = 256 * 1024 * 1024;
/// A decode request that was not repeated for this many frames is dropped: its picture has left
/// the screen.
const STALE_AFTER_FRAMES: u64 = 4;
/// At most this many bytes of pixels are uploaded to the GPU per frame, so that many pictures
/// arriving at once do not stall a frame. One picture is always uploaded.
const UPLOAD_BYTES_PER_FRAME: usize = 12 * 1024 * 1024;

/// How urgent a request is; lower values are served first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct Priority(pub u32);

impl Priority {
    /// The one picture the user is looking at: the picture in the viewer. It goes before the
    /// cards of the feed that are still painted, dimmed, behind the viewer.
    pub fn focus() -> Priority {
        Priority(0)
    }

    /// On screen right now. `rank` orders pictures of the same class (0 first).
    pub fn visible(rank: usize) -> Priority {
        Priority(1 + rank.min(9_998) as u32)
    }

    /// Just outside the view: decoded ahead so that scrolling does not show placeholders.
    pub fn near(rank: usize) -> Priority {
        Priority(10_000 + rank.min(9_999) as u32)
    }

    /// The size of every picture of a collection, read in the background in feed order.
    pub fn background(rank: usize) -> Priority {
        Priority(1_000_000 + rank.min(100_000_000) as u32)
    }
}

/// Which rendition of a picture is wanted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Variant {
    /// For a cover-cropped frame (feed card, grid tile, avatar): the shorter side has this many
    /// pixels (`decode::decode_thumbnail`).
    Cover { min_side_px: u32 },
    /// For a contained picture (viewer, auto view): the whole picture inside a square of this
    /// many pixels (`decode::decode_picture`).
    Contain { max_side_px: u32 },
}

/// A decoded picture on the GPU.
#[derive(Clone)]
pub struct Picture {
    /// The texture; cheap to clone.
    pub texture: TextureHandle,
    /// The size of the texture in pixels.
    pub size_px: [u32; 2],
    /// The oriented size of the file: the 100 % size and the aspect ratio.
    pub source_size: (u32, u32),
}

/// What is known about a requested picture.
#[derive(Clone)]
pub enum ImageState {
    /// Decoded and uploaded.
    Ready(Picture),
    /// Waiting for a worker or being decoded.
    Loading,
    /// It cannot be shown; the sentence says why and names the file.
    Failed(String),
}

/// What is known about the size of a picture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SizeState {
    /// The oriented size in pixels.
    Known((u32, u32)),
    /// Not read yet.
    Pending,
    /// The header could not be read; the sentence says why.
    Failed(String),
}

/// Counters for the log and the tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoaderStats {
    /// Textures currently kept.
    pub textures: usize,
    /// Their size in bytes.
    pub texture_bytes: usize,
    /// Requests that are queued, running or waiting to be uploaded.
    pub pending: usize,
    /// Pictures decoded since the start.
    pub decoded: u64,
    /// Decode requests dropped because their picture left the screen first.
    pub dropped: u64,
    /// Textures released to stay inside the budget.
    pub evicted: u64,
}

/// What a worker is asked to do.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
enum Job {
    /// Read the size from the header.
    Size(PathBuf),
    /// Decode a rendition.
    Decode(PathBuf, Variant),
    /// Make (or find) the poster of a loose video.
    Poster(PathBuf),
}

/// What is known about the poster of a loose video.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PosterState {
    /// The poster file: a JPEG to decode like a picture.
    Ready(PathBuf),
    /// Being made.
    Pending,
    /// It cannot be made; the sentence says why.
    Failed(String),
}

/// What a worker reports back.
enum Done {
    Size(PathBuf, Result<(u32, u32), String>),
    Decoded(PathBuf, Variant, Result<(ColorImage, (u32, u32)), String>),
    /// The video, and its poster with the poster's size.
    Poster(PathBuf, Result<(PathBuf, (u32, u32)), String>),
    /// The request went stale before a worker got to it.
    Dropped(Job),
}

/// A queued job: when it was last asked for and whether it may go stale.
struct Queued {
    order: (Priority, u64),
    wanted_frame: u64,
    keep: bool,
}

/// The queue shared with the workers, ordered by priority and then by arrival.
#[derive(Default)]
struct Queue {
    by_order: BTreeMap<(Priority, u64), Job>,
    jobs: HashMap<Job, Queued>,
    next_serial: u64,
}

impl Queue {
    /// Adds a job, or refreshes one that is already queued (and raises its priority).
    fn request(&mut self, job: Job, priority: Priority, frame: u64, keep: bool) {
        if self.refresh(&job, priority, frame, keep) {
            return;
        }
        let order = (priority, self.next_serial);
        self.next_serial += 1;
        self.by_order.insert(order, job.clone());
        self.jobs.insert(job, Queued { order, wanted_frame: frame, keep });
    }

    /// Refreshes a job that is still queued: it was asked for in `frame`, and its priority rises
    /// to `priority` if that is more urgent. Returns false when the job is not queued (a worker
    /// has taken it, or it was never added).
    fn refresh(&mut self, job: &Job, priority: Priority, frame: u64, keep: bool) -> bool {
        let Some(queued) = self.jobs.get_mut(job) else {
            return false;
        };
        queued.wanted_frame = frame;
        queued.keep |= keep;
        if priority < queued.order.0 {
            self.by_order.remove(&queued.order);
            queued.order = (priority, queued.order.1);
            self.by_order.insert(queued.order, job.clone());
        }
        true
    }

    /// Takes the most urgent job. Stale jobs met on the way are returned in `dropped`.
    fn take(&mut self, frame: u64, dropped: &mut Vec<Job>) -> Option<Job> {
        while let Some((_, job)) = self.by_order.pop_first() {
            let Some(queued) = self.jobs.remove(&job) else {
                continue;
            };
            if !queued.keep && queued.wanted_frame + STALE_AFTER_FRAMES < frame {
                dropped.push(job);
                continue;
            }
            return Some(job);
        }
        None
    }

    /// Removes every queued job and returns them.
    fn clear(&mut self) -> Vec<Job> {
        self.by_order.clear();
        self.jobs.drain().map(|(job, _)| job).collect()
    }
}

/// Wakes the UI thread when a worker has something for it. The egui context is attached on the
/// first frame, so the loader can be built before a window exists.
#[derive(Clone, Default)]
pub struct Waker(Arc<OnceLock<egui::Context>>);

impl Waker {
    /// Remembers the context to wake.
    pub fn attach(&self, ctx: &egui::Context) {
        let _ = self.0.set(ctx.clone());
    }

    /// Asks for a repaint; a no-op before a context is attached.
    pub fn wake(&self) {
        if let Some(ctx) = self.0.get() {
            ctx.request_repaint();
        }
    }
}

/// What the workers share with the UI thread.
struct Shared {
    queue: Mutex<Queue>,
    available: Condvar,
    frame: AtomicU64,
    shutdown: AtomicBool,
    waker: Waker,
    /// Where the posters of loose videos go; `None`: they are not made.
    posters: OnceLock<PosterCache>,
}

impl Shared {
    /// The queue; a poisoned lock is still usable, the queue holds no invariants a panic breaks.
    fn lock(&self) -> MutexGuard<'_, Queue> {
        match self.queue.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }
}

/// A cached texture.
struct Entry {
    picture: Picture,
    bytes: usize,
    last_used_frame: u64,
}

/// The picture loader. It belongs to the UI thread; only the queue is shared with the workers.
pub struct Loader {
    shared: Arc<Shared>,
    sender: mpsc::Sender<Done>,
    receiver: mpsc::Receiver<Done>,
    workers: Vec<JoinHandle<()>>,
    /// Requests that are queued, running, or finished but not yet taken over.
    pending: HashSet<Job>,
    /// Jobs a worker was already running when the cache was reset: their results may show a
    /// file as it was before, so they are thrown away when they arrive.
    discarded: HashSet<Job>,
    /// Decoded pictures waiting for their turn to be uploaded.
    uploads: VecDeque<(PathBuf, Variant, ColorImage, (u32, u32))>,
    textures: HashMap<(PathBuf, Variant), Entry>,
    texture_bytes: usize,
    budget_bytes: usize,
    sizes: HashMap<PathBuf, (u32, u32)>,
    failures: HashMap<PathBuf, String>,
    /// The posters of loose videos, by video.
    posters: HashMap<PathBuf, Result<PathBuf, String>>,
    /// The files whose size (or failure) became known in the current frame.
    size_news: Vec<PathBuf>,
    /// How many such files there have been since the start.
    size_revision: u64,
    frame: u64,
    max_texture_side: u32,
    stats: LoaderStats,
}

impl Default for Loader {
    fn default() -> Self {
        Loader::new()
    }
}

impl Loader {
    /// A loader with the default texture budget. No thread is started before the first request.
    pub fn new() -> Loader {
        Loader::with_budget(TEXTURE_BUDGET_BYTES)
    }

    /// A loader with a texture budget of `budget_bytes`.
    pub fn with_budget(budget_bytes: usize) -> Loader {
        let (sender, receiver) = mpsc::channel();
        Loader {
            shared: Arc::new(Shared {
                queue: Mutex::new(Queue::default()),
                available: Condvar::new(),
                frame: AtomicU64::new(0),
                shutdown: AtomicBool::new(false),
                waker: Waker::default(),
                posters: OnceLock::new(),
            }),
            sender,
            receiver,
            workers: Vec::new(),
            pending: HashSet::new(),
            discarded: HashSet::new(),
            uploads: VecDeque::new(),
            textures: HashMap::new(),
            texture_bytes: 0,
            budget_bytes,
            sizes: HashMap::new(),
            failures: HashMap::new(),
            posters: HashMap::new(),
            size_news: Vec::new(),
            size_revision: 0,
            frame: 0,
            max_texture_side: decode::MAX_TEXTURE_SIDE,
            stats: LoaderStats::default(),
        }
    }

    /// The handle that wakes the UI thread, for other background work of the app.
    pub fn waker(&self) -> Waker {
        self.shared.waker.clone()
    }

    /// Where the posters of loose videos are made (`settings::AppPaths::poster_cache_dir`). Set
    /// once; without it loose videos show the placeholder.
    pub fn set_poster_cache(&mut self, cache: PosterCache) {
        let _ = self.shared.posters.set(cache);
    }

    /// Call once at the start of every frame: takes over what the workers finished, uploads
    /// textures within the per-frame budget and releases the least recently used ones.
    pub fn begin_frame(&mut self, ctx: &egui::Context) {
        self.shared.waker.attach(ctx);
        self.frame += 1;
        self.shared.frame.store(self.frame, Ordering::Relaxed);
        self.max_texture_side = (ctx.input(|input| input.max_texture_side) as u32).clamp(1, decode::MAX_TEXTURE_SIDE);
        self.size_news.clear();

        while let Ok(done) = self.receiver.try_recv() {
            let job = match &done {
                Done::Size(path, _) => Job::Size(path.clone()),
                Done::Decoded(path, variant, _) => Job::Decode(path.clone(), *variant),
                Done::Poster(video, _) => Job::Poster(video.clone()),
                Done::Dropped(job) => job.clone(),
            };
            if self.discarded.remove(&job) {
                // Started before the reset: the next request decodes the file as it is now.
                self.pending.remove(&job);
                continue;
            }
            match done {
                Done::Size(path, result) => {
                    self.pending.remove(&Job::Size(path.clone()));
                    self.size_news.push(path.clone());
                    match result {
                        Ok(size) => {
                            self.sizes.insert(path, size);
                        }
                        Err(message) => {
                            self.failures.entry(path).or_insert(message);
                        }
                    }
                }
                Done::Decoded(path, variant, result) => match result {
                    Ok((image, source_size)) => {
                        if !self.sizes.contains_key(&path) {
                            self.sizes.insert(path.clone(), source_size);
                            self.size_news.push(path.clone());
                        }
                        self.uploads.push_back((path, variant, image, source_size));
                    }
                    Err(message) => {
                        self.pending.remove(&Job::Decode(path.clone(), variant));
                        log::warn!("{message}");
                        self.size_news.push(path.clone());
                        self.failures.insert(path, message);
                    }
                },
                Done::Poster(video, result) => {
                    self.pending.remove(&Job::Poster(video.clone()));
                    // The rows waiting for the frame of this video learn its size now.
                    self.size_news.push(video.clone());
                    match result {
                        Ok((poster, size)) => {
                            self.sizes.insert(poster.clone(), size);
                            self.posters.insert(video, Ok(poster));
                        }
                        Err(message) => {
                            self.posters.insert(video, Err(message));
                        }
                    }
                }
                Done::Dropped(job) => {
                    self.pending.remove(&job);
                    self.stats.dropped += 1;
                }
            }
        }
        self.size_revision += self.size_news.len() as u64;

        let mut uploaded_bytes = 0;
        while let Some((_, _, image, _)) = self.uploads.front() {
            let bytes = image.width() * image.height() * 4;
            if uploaded_bytes > 0 && uploaded_bytes + bytes > UPLOAD_BYTES_PER_FRAME {
                // The rest waits for the next frame.
                ctx.request_repaint();
                break;
            }
            let Some((path, variant, image, source_size)) = self.uploads.pop_front() else {
                break;
            };
            uploaded_bytes += bytes;
            let size_px = [image.width() as u32, image.height() as u32];
            let texture = ctx.load_texture(path.display().to_string(), image, TextureOptions::LINEAR);
            self.pending.remove(&Job::Decode(path.clone(), variant));
            self.stats.decoded += 1;
            self.texture_bytes += bytes;
            let entry = Entry { picture: Picture { texture, size_px, source_size }, bytes, last_used_frame: self.frame };
            if let Some(previous) = self.textures.insert((path, variant), entry) {
                self.texture_bytes -= previous.bytes;
            }
        }
        self.evict();
    }

    /// Releases the least recently used textures until the cache fits its budget. Textures used
    /// in this frame or the previous one are never released.
    fn evict(&mut self) {
        if self.texture_bytes <= self.budget_bytes {
            return;
        }
        let mut candidates: Vec<(u64, (PathBuf, Variant))> =
            self.textures.iter().filter(|(_, entry)| entry.last_used_frame + 1 < self.frame).map(|(key, entry)| (entry.last_used_frame, key.clone())).collect();
        candidates.sort();
        for (_, key) in candidates {
            if self.texture_bytes <= self.budget_bytes {
                break;
            }
            if let Some(entry) = self.textures.remove(&key) {
                self.texture_bytes -= entry.bytes;
                self.stats.evicted += 1;
            }
        }
    }

    /// Starts the worker threads on the first request.
    fn ensure_workers(&mut self) {
        if !self.workers.is_empty() {
            return;
        }
        // One core is left to the UI thread; the core crate allows three decodes at once.
        let cores = std::thread::available_parallelism().map(|count| count.get()).unwrap_or(2);
        let count = cores.saturating_sub(1).clamp(1, IMAGE_CONCURRENCY);
        for index in 0..count {
            let shared = self.shared.clone();
            let sender = self.sender.clone();
            let spawned = std::thread::Builder::new().name(format!("picture-decoder-{index}")).spawn(move || worker(&shared, &sender));
            match spawned {
                Ok(handle) => self.workers.push(handle),
                Err(error) => log::error!("A decoder thread could not be started: {error}"),
            }
        }
    }

    /// Queues `job` unless it is already on its way; refreshes it otherwise.
    fn request(&mut self, job: Job, priority: Priority, keep: bool) {
        self.ensure_workers();
        if self.pending.insert(job.clone()) {
            self.shared.lock().request(job, priority, self.frame, keep);
            self.shared.available.notify_one();
        } else {
            // Already on its way: keep it alive if it is still waiting for a worker. A job a
            // worker has taken must not be queued a second time.
            self.shared.lock().refresh(&job, priority, self.frame, keep);
        }
    }

    /// The oriented size of the picture at `path`, read from its header in the background.
    pub fn size_of(&mut self, path: &Path, priority: Priority) -> SizeState {
        if let Some(size) = self.sizes.get(path) {
            return SizeState::Known(*size);
        }
        if let Some(message) = self.failures.get(path) {
            return SizeState::Failed(message.clone());
        }
        // A size request never goes stale: the layout of the whole feed depends on it.
        self.request(Job::Size(path.to_path_buf()), priority, true);
        SizeState::Pending
    }

    /// The poster of the loose video at `video`, made by a worker on the first request. The
    /// request never goes stale: the layout of the feed depends on the poster's size.
    pub fn poster_of(&mut self, video: &Path, priority: Priority) -> PosterState {
        match self.posters.get(video) {
            Some(Ok(poster)) => return PosterState::Ready(poster.clone()),
            Some(Err(message)) => return PosterState::Failed(message.clone()),
            None => {}
        }
        if self.shared.posters.get().is_none() {
            return PosterState::Failed(String::new());
        }
        self.request(Job::Poster(video.to_path_buf()), priority, true);
        PosterState::Pending
    }

    /// What is known about the poster of the loose video at `video`; never starts work.
    pub fn peek_poster(&self, video: &Path) -> PosterState {
        match self.posters.get(video) {
            Some(Ok(poster)) => PosterState::Ready(poster.clone()),
            Some(Err(message)) => PosterState::Failed(message.clone()),
            None => PosterState::Pending,
        }
    }

    /// The files whose size (or the fact that it cannot be read) became known in this frame.
    /// A view with thousands of rows updates only these instead of asking for every row.
    pub fn size_news(&self) -> &[PathBuf] {
        &self.size_news
    }

    /// How many sizes have become known since the start. A view that remembers the value can
    /// tell whether `size_news` of this frame is everything it has missed.
    pub fn size_revision(&self) -> u64 {
        self.size_revision
    }

    /// What is known about the size of the picture at `path` right now; never starts work.
    pub fn peek_size(&self, path: &Path) -> SizeState {
        if let Some(size) = self.sizes.get(path) {
            SizeState::Known(*size)
        } else if let Some(message) = self.failures.get(path) {
            SizeState::Failed(message.clone())
        } else {
            SizeState::Pending
        }
    }

    /// The picture at `path` in the wanted rendition. Call it every frame while the picture is on
    /// screen: that keeps the request alive and the texture in the cache.
    pub fn image(&mut self, path: &Path, variant: Variant, priority: Priority) -> ImageState {
        let variant = self.limited(variant);
        let frame = self.frame;
        if let Some(entry) = self.textures.get_mut(&(path.to_path_buf(), variant)) {
            entry.last_used_frame = frame;
            return ImageState::Ready(entry.picture.clone());
        }
        if let Some(message) = self.failures.get(path) {
            return ImageState::Failed(message.clone());
        }
        self.request(Job::Decode(path.to_path_buf(), variant), priority, false);
        ImageState::Loading
    }

    /// Any rendition of `path` that is already a texture, the largest first: a stand-in while the
    /// wanted rendition is being decoded.
    pub fn any_rendition(&mut self, path: &Path) -> Option<Picture> {
        let frame = self.frame;
        let best = self.textures.iter_mut().filter(|((entry_path, _), _)| entry_path == path).max_by_key(|(_, entry)| entry.bytes)?;
        best.1.last_used_frame = frame;
        Some(best.1.picture.clone())
    }

    /// Keeps a rendition within what the GPU accepts as a texture.
    fn limited(&self, variant: Variant) -> Variant {
        match variant {
            Variant::Cover { min_side_px } => Variant::Cover { min_side_px: min_side_px.clamp(1, self.max_texture_side) },
            Variant::Contain { max_side_px } => Variant::Contain { max_side_px: max_side_px.clamp(1, self.max_texture_side) },
        }
    }

    /// Forgets what is queued and everything known about pictures: sizes, failures, textures
    /// and decoded pictures waiting for upload. Called when another collection is opened and
    /// when the pictures are closed: a file may have been edited or replaced since it was read
    /// (another program, a new copy under the same name), and a picture that is opened again
    /// must show the file as it is now, in its present size. Nothing of the old collection is
    /// decoded any more; what a worker is decoding right now is thrown away when it arrives.
    pub fn reset_requests(&mut self) {
        let cleared = self.shared.lock().clear();
        for job in cleared {
            self.pending.remove(&job);
        }
        for (path, variant, _, _) in self.uploads.drain(..) {
            self.pending.remove(&Job::Decode(path, variant));
        }
        // What is left is with a worker (or on its way back). It stays pending, so that the
        // same picture is not queued twice; its result is dropped and asked for again.
        self.discarded.extend(self.pending.iter().cloned());
        self.failures.clear();
        self.sizes.clear();
        self.posters.clear();
        self.textures.clear();
        self.texture_bytes = 0;
    }

    /// True when nothing is queued, running or waiting to be uploaded.
    pub fn is_idle(&self) -> bool {
        self.pending.is_empty() && self.uploads.is_empty()
    }

    /// What is queued or running right now, in words for the log (a few entries at most).
    pub fn pending_summary(&self) -> String {
        const SHOWN: usize = 4;
        let mut jobs: Vec<String> = self.pending.iter().take(SHOWN).map(|job| format!("{job:?}")).collect();
        if self.pending.len() > SHOWN {
            jobs.push(format!("and {} more", self.pending.len() - SHOWN));
        }
        if !self.uploads.is_empty() {
            jobs.push(format!("{} waiting for upload", self.uploads.len()));
        }
        jobs.join("; ")
    }

    /// Counters for the log and the tests.
    pub fn stats(&self) -> LoaderStats {
        LoaderStats { textures: self.textures.len(), texture_bytes: self.texture_bytes, pending: self.pending.len() + self.uploads.len(), ..self.stats }
    }
}

impl Drop for Loader {
    fn drop(&mut self) {
        self.shared.shutdown.store(true, Ordering::SeqCst);
        self.shared.lock().clear();
        self.shared.available.notify_all();
        // The workers are not joined: one may be in the middle of a long decode, and closing the
        // window must not wait for it. They end when they see the flag.
    }
}

/// The loop of one worker thread.
fn worker(shared: &Shared, sender: &mpsc::Sender<Done>) {
    // Decoding must never make the interface wait for a core.
    crate::platform::run_below_normal_priority();
    loop {
        let mut dropped = Vec::new();
        let job = {
            let mut queue = shared.lock();
            loop {
                if shared.shutdown.load(Ordering::SeqCst) {
                    return;
                }
                if let Some(job) = queue.take(shared.frame.load(Ordering::Relaxed), &mut dropped) {
                    break Some(job);
                }
                if !dropped.is_empty() {
                    break None;
                }
                queue = match shared.available.wait(queue) {
                    Ok(guard) => guard,
                    Err(poisoned) => poisoned.into_inner(),
                };
            }
        };
        let had_dropped = !dropped.is_empty();
        for stale in dropped {
            if sender.send(Done::Dropped(stale)).is_err() {
                return;
            }
        }
        if let Some(job) = job {
            let done = match job {
                Job::Size(path) => {
                    let result = decode::probe_dimensions(&path).map_err(|error| error.message);
                    Done::Size(path, result)
                }
                Job::Decode(path, variant) => {
                    let decoded = match variant {
                        Variant::Cover { min_side_px } => decode::decode_thumbnail(&path, min_side_px),
                        Variant::Contain { max_side_px } => decode::decode_picture(&path, max_side_px),
                    };
                    let result = decoded.map_err(|error| error.message).map(|picture| {
                        let size = [picture.width as usize, picture.height as usize];
                        (ColorImage::from_rgba_unmultiplied(size, &picture.rgba), (picture.source_width, picture.source_height))
                    });
                    Done::Decoded(path, variant, result)
                }
                Job::Poster(video) => {
                    let result = match shared.posters.get() {
                        Some(cache) => cache.ensure_sniffed(&video).map_err(|error| error.message).and_then(|poster| {
                            let size = decode::probe_dimensions(&poster).map_err(|error| error.message)?;
                            Ok((poster, size))
                        }),
                        None => Err(String::new()),
                    };
                    if let Err(message) = &result {
                        log::warn!("{message}");
                    }
                    Done::Poster(video, result)
                }
            };
            if sender.send(done).is_err() {
                return;
            }
            shared.waker.wake();
        } else if had_dropped {
            shared.waker.wake();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(name: &str) -> Job {
        Job::Decode(PathBuf::from(name), Variant::Cover { min_side_px: 100 })
    }

    #[test]
    fn the_most_urgent_job_is_taken_first_and_ties_keep_their_order() {
        let mut queue = Queue::default();
        queue.request(job("far"), Priority::near(3), 1, false);
        queue.request(job("first"), Priority::visible(0), 1, false);
        queue.request(job("second"), Priority::visible(0), 1, false);
        queue.request(Job::Size(PathBuf::from("size")), Priority::background(0), 1, true);
        let mut dropped = Vec::new();
        assert_eq!(queue.take(1, &mut dropped), Some(job("first")));
        assert_eq!(queue.take(1, &mut dropped), Some(job("second")));
        assert_eq!(queue.take(1, &mut dropped), Some(job("far")));
        assert_eq!(queue.take(1, &mut dropped), Some(Job::Size(PathBuf::from("size"))));
        assert_eq!(queue.take(1, &mut dropped), None);
        assert!(dropped.is_empty());
    }

    #[test]
    fn asking_again_raises_the_priority_and_never_lowers_it() {
        let mut queue = Queue::default();
        queue.request(job("a"), Priority::near(5), 1, false);
        queue.request(job("b"), Priority::near(1), 1, false);
        queue.request(job("a"), Priority::visible(0), 2, false);
        queue.request(job("b"), Priority::background(9), 2, false);
        let mut dropped = Vec::new();
        assert_eq!(queue.take(2, &mut dropped), Some(job("a")));
        assert_eq!(queue.take(2, &mut dropped), Some(job("b")));
        assert_eq!(queue.jobs.len(), 0);
        assert_eq!(queue.by_order.len(), 0);
    }

    #[test]
    fn a_request_that_is_not_repeated_goes_stale_but_a_kept_one_does_not() {
        let mut queue = Queue::default();
        queue.request(job("scrolled past"), Priority::visible(0), 1, false);
        queue.request(job("still on screen"), Priority::visible(1), 1, false);
        queue.request(Job::Size(PathBuf::from("size")), Priority::background(0), 1, true);
        // Ten frames later only one of the pictures was asked for again.
        queue.request(job("still on screen"), Priority::visible(1), 10, false);
        let mut dropped = Vec::new();
        assert_eq!(queue.take(10, &mut dropped), Some(job("still on screen")));
        assert_eq!(dropped, vec![job("scrolled past")]);
        assert_eq!(queue.take(10, &mut dropped), Some(Job::Size(PathBuf::from("size"))));
    }

    #[test]
    fn clearing_returns_what_was_queued() {
        let mut queue = Queue::default();
        queue.request(job("a"), Priority::visible(0), 1, false);
        queue.request(job("b"), Priority::visible(1), 1, false);
        let mut cleared = queue.clear();
        cleared.sort_by_key(|job| format!("{job:?}"));
        assert_eq!(cleared, vec![job("a"), job("b")]);
        let mut dropped = Vec::new();
        assert_eq!(queue.take(1, &mut dropped), None);
    }

    #[test]
    fn priorities_order_visible_before_near_before_background() {
        assert!(Priority::visible(9_999_999) < Priority::near(0));
        assert!(Priority::near(9_999_999) < Priority::background(0));
        assert!(Priority::visible(0) < Priority::visible(1));
        assert!(Priority::focus() < Priority::visible(0));
    }

    #[test]
    fn a_new_loader_is_idle_and_starts_no_thread() {
        let loader = Loader::new();
        assert!(loader.is_idle());
        assert!(loader.workers.is_empty());
        assert_eq!(loader.stats(), LoaderStats::default());
    }
}
