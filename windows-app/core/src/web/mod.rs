//! The browser pages: an HTTP server on two addresses that shows the posts open in the app with
//! the Instagram clone's own frontend, and the clone's API behind it (web brief section 4).
//!
//! ```text
//!  Browser ──HTTP──> page listener 127.0.0.1:3127 ─┐  vendored pages (embedded) + /api
//!  Browser/curl ───> API listener  127.0.0.1:8317 ─┤  /api + /docs, CORS for the page origins
//!                                                  │
//!                    accept thread per listener ───┤  one thread per connection; a place once its
//!                                                  │  first request has arrived (at most 64 per
//!                                                  │  listener, 16 per client, 8 kept for this
//!                                                  │  computer; one that waits gives it up)
//!                                                  │
//!                    routes ── reads ────> Arc<Snapshot> (the latest the app published)
//!                           ── changes ──> WebCommand ──channel──> the app's UI thread
//!                           <── reply ─── Result<Arc<Snapshot>, ApiError>   (10 s at most)
//!                           ── uploads ──> library::Library (the browser imports folder)
//! ```
//!
//! The server owns no state of what is open: the app publishes a `Snapshot` after every change
//! (`Bridge::publish`) and carries out every change a browser asks for on its own UI thread
//! (`WebCommand`), so the app stays the one writer of what is open and of descriptions.json.
//! Nothing here depends on the GUI: the real-socket tests run without a window.
//!
//! Modules: `http` (connections, limits), `guard` (Host, own-origin writes, CORS), `routes` (the
//! route table), `snapshot` (the library JSON and the virtual file map), `files` (ranges, HEIC
//! copies), `network` (ports, LAN addresses, machine names).

pub mod files;
pub mod guard;
pub mod http;
pub mod network;
pub mod routes;
pub mod snapshot;

use std::collections::{HashMap, VecDeque};
use std::io::{self as stdio, Read, Write};
use std::net::{IpAddr, Ipv4Addr, Shutdown, SocketAddr, SocketAddrV4, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::library::Library;
use crate::messages;
use crate::model::PostKey;
use crate::sources::LoadedSource;

use self::http::Admitted;
pub use self::http::Timeouts;
pub use self::network::{BindError, PortSearch};
pub use self::snapshot::{FormatCache, Snapshot, SnapshotKind};

/// The page address port of the clone (`port_mapping.json`).
pub const DEFAULT_FRONTEND_PORT: u16 = 3127;
/// The API address port of the clone (`port_mapping.json`).
pub const DEFAULT_BACKEND_PORT: u16 = 8317;
/// Each listener serves at most this many connections at once (a connection is served from the
/// moment its first request has fully arrived). The two listeners count apart, so that clients
/// of the API cannot crowd out the pages. A request that finds no place takes the place of a
/// connection that only waits (`Admission`), else it gets 503 at once.
pub const MAX_CONNECTIONS: usize = 64;
/// One client gets at most this many of them on one listener (a browser opens six per address),
/// so that one device cannot take every place. Every loopback address (all of 127.0.0.0/8) is
/// this computer and counts as one client: a program can bind any of them.
pub const MAX_CONNECTIONS_PER_PEER: usize = 16;
/// Places of each listener that only clients on this computer may take: devices on the network,
/// whatever number of addresses they use, never hold the last of them.
pub const RESERVED_FOR_THIS_COMPUTER: usize = 8;
/// Connections of each listener whose first request has not fully arrived yet. They hold no
/// place, however often a client opens them again: places go to requests. Beyond these numbers
/// the one that has waited longest is closed for the new one (only one of the same client while
/// that client has its share), unless its request is waiting in its socket; a request that has
/// arrived with its whole body is answered all the same, one whose body is still on its way gets
/// "Too many connections" (`Admission::admit`, `in_hand`). What this does not prevent: a
/// client that opens connections faster than its others send their requests closes those
/// others first. Every program on this computer is one client with the browser here, so a
/// program that opens several hundred connections a second makes some of the browser's
/// requests fail; no limit of a server keeps a program on the same computer from disturbing it.
pub const MAX_ARRIVING: usize = 128;
/// ... of them one client may have (a browser opens a few at once).
pub const MAX_ARRIVING_PER_PEER: usize = 32;
/// ... of them only clients on this computer may have.
pub const ARRIVING_KEPT_FOR_THIS_COMPUTER: usize = 32;
/// A refused connection is read from for at most this long after its 503 (a closed socket with
/// unread bytes resets the connection, and the client would never see the answer).
pub const REJECT_LINGER_MS: u64 = 200;
/// ... and at most this many bytes.
pub const REJECT_LINGER_BYTES: usize = 8 * 1024;
/// Refused connections handed to the `web-reject` thread that it has not taken yet; beyond, they
/// are closed at once (without their 503). A burst of refused connections (a page that opens
/// many at once while this computer's places are taken) arrives faster than the thread takes
/// them, so the queue holds as many as the thread reads from.
pub const REJECT_QUEUE: usize = 256;
/// Refused connections the `web-reject` thread reads from at the same time; beyond, the oldest
/// is closed (its 503 has been sent).
pub const REJECT_LINGERING_MAX: usize = 256;
/// How often the `web-reject` thread looks at the connections it reads from.
pub const REJECT_POLL_MS: u64 = 5;
/// An HTTP thread waits this long for the UI thread to carry out a change.
pub const UI_REPLY_TIMEOUT_MS: u64 = 10_000;
/// Stopping the server (both listeners) takes at most this long.
pub const WEB_STOP_WAIT_MS: u64 = 500;

/// A random value of `bytes` bytes as lower-case hexadecimal (upload ids, temporary names). The
/// system's generator is asked; should it fail, the time and a counter stand in, which is unique
/// within the process (what temporary names need) but not unguessable.
pub fn random_hex(bytes: usize) -> String {
    let mut buffer = vec![0_u8; bytes];
    if getrandom::fill(&mut buffer).is_err() {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let seed = format!("{}-{}-{:?}", std::process::id(), COUNTER.fetch_add(1, Ordering::Relaxed), Instant::now());
        for (index, byte) in buffer.iter_mut().enumerate() {
            *byte = (crate::text::fnv1a_64(format!("{seed}-{index}").as_bytes()) & 0xFF) as u8;
        }
    }
    buffer.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// One file of the vendored pages, embedded in the program (`app/build.rs` writes the table).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct WebFile {
    /// The path below the page root: "index.html", "assets/index-x.js", "licenses/README.txt".
    pub path: &'static str,
    /// The content type it is served with.
    pub content_type: &'static str,
    /// `"<16 hex of FNV-1a-64 of the bytes>"`, quotes included.
    pub etag: &'static str,
    /// True below `assets/`: the name holds a hash of the content, so it may be cached for good.
    pub immutable: bool,
    /// The bytes.
    pub bytes: &'static [u8],
}

/// A change the browser asked for that the server cannot carry out itself.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ApiError {
    /// The HTTP status of the answer.
    pub status: u16,
    /// The sentence (`{"detail": ...}`).
    pub detail: String,
}

impl ApiError {
    /// An error with its status and sentence.
    pub fn new(status: u16, detail: impl Into<String>) -> ApiError {
        ApiError { status, detail: detail.into() }
    }
}

/// What a browser asks the app to change.
#[derive(Debug)]
pub enum WebCommandKind {
    /// Save this caption for the open post (the editor's Save).
    SetCaption {
        /// The post.
        key: PostKey,
        /// The cleaned caption (`text::saved_caption`).
        caption: String,
    },
    /// The browser imports folder changed: add it to what is open or refresh it (`Some`), or
    /// take it out when it has no post any more (`None`). `added` counts the posts of a new
    /// import (0 after a deletion).
    ShowWebLibrary {
        /// The folder read back.
        loaded: Option<Box<LoadedSource>>,
        /// Posts of the import that was committed.
        added: usize,
    },
    /// Close what is open, which must be the browser imports folder alone (before it is cleared).
    CloseWebLibrary,
}

/// A change with the channel its answer goes back on.
#[derive(Debug)]
pub struct WebCommand {
    /// The change.
    pub kind: WebCommandKind,
    /// The new snapshot, or why the change was refused.
    pub reply: mpsc::Sender<Result<Arc<Snapshot>, ApiError>>,
}

/// The app as the server sees it.
pub trait WebHost: Send + Sync {
    /// The snapshot published last (never waits for the UI thread).
    fn snapshot(&self) -> Arc<Snapshot>;
    /// Hands a change to the UI thread and waits for its answer.
    fn send(&self, kind: WebCommandKind) -> Result<Arc<Snapshot>, ApiError>;
}

/// The link between the HTTP threads and the UI thread: the published snapshot, and a channel
/// for changes. The UI thread drains the receiver in its frame logic, which eframe runs on
/// `request_repaint` also while the window is hidden or minimised; `wake` asks for that frame.
pub struct Bridge {
    snapshot: Mutex<Arc<Snapshot>>,
    sender: Mutex<Option<mpsc::Sender<WebCommand>>>,
    wake: Box<dyn Fn() + Send + Sync>,
    reply_timeout: Duration,
}

impl Bridge {
    /// A bridge with a first snapshot; the receiver goes to the UI thread.
    pub fn new(first: Arc<Snapshot>, wake: Box<dyn Fn() + Send + Sync>, reply_timeout: Duration) -> (Arc<Bridge>, mpsc::Receiver<WebCommand>) {
        let (sender, receiver) = mpsc::channel();
        (Arc::new(Bridge { snapshot: Mutex::new(first), sender: Mutex::new(Some(sender)), wake, reply_timeout }), receiver)
    }

    /// Publishes a new snapshot.
    pub fn publish(&self, snapshot: Arc<Snapshot>) {
        *self.snapshot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()) = snapshot;
    }

    /// No more changes are taken: a waiting or new change is answered with `APP_CLOSING`.
    pub fn close(&self) {
        self.sender.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).take();
    }
}

impl WebHost for Bridge {
    fn snapshot(&self) -> Arc<Snapshot> {
        Arc::clone(&self.snapshot.lock().unwrap_or_else(|poisoned| poisoned.into_inner()))
    }

    fn send(&self, kind: WebCommandKind) -> Result<Arc<Snapshot>, ApiError> {
        let (reply, answer) = mpsc::channel();
        let sent = {
            let sender = self.sender.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
            match sender.as_ref() {
                Some(sender) => sender.send(WebCommand { kind, reply }).is_ok(),
                None => false,
            }
        };
        if !sent {
            return Err(ApiError::new(503, messages::APP_CLOSING));
        }
        (self.wake)();
        match answer.recv_timeout(self.reply_timeout) {
            Ok(result) => result,
            Err(mpsc::RecvTimeoutError::Timeout) => Err(ApiError::new(503, messages::APP_TIMEOUT)),
            Err(mpsc::RecvTimeoutError::Disconnected) => Err(ApiError::new(503, messages::APP_CLOSING)),
        }
    }
}

/// How the server is set up.
#[derive(Clone, Debug)]
pub struct ServerConfig {
    /// The address both listeners bind (`127.0.0.1`, `0.0.0.0` for every network adapter).
    pub host: Ipv4Addr,
    /// The page address port (0: any free port, for tests).
    pub frontend_port: u16,
    /// The API address port (0: any free port, for tests).
    pub backend_port: u16,
    /// Whether a taken port is replaced by the next free one.
    pub search: PortSearch,
    /// `allowed_hosts` of settings.json.
    pub allowed_hosts: Vec<String>,
    /// This computer's names (`network::machine_names`).
    pub machine_names: Vec<String>,
    /// This computer's LAN addresses (`network::discover_lan_ipv4`).
    pub discovered_lan: Vec<Ipv4Addr>,
    /// How long clients are waited for.
    pub timeouts: Timeouts,
    /// Connections each listener serves at once.
    pub max_connections: usize,
    /// Connections one client (`peer_group`) may have on one listener.
    pub max_connections_per_peer: usize,
    /// Places of each listener only clients on this computer may take.
    pub reserved_for_this_computer: usize,
    /// Connections of each listener whose first request has not fully arrived.
    pub max_arriving: usize,
    /// ... of one client.
    pub max_arriving_per_peer: usize,
    /// ... kept for clients on this computer.
    pub arriving_kept_for_this_computer: usize,
    /// The vendored pages.
    pub assets: &'static [WebFile],
    /// Where the JPEG copies of HEIC pictures are kept.
    pub web_cache_dir: PathBuf,
    /// Where the posters of loose videos are kept (`settings::AppPaths::poster_cache_dir`);
    /// `None`: they are not served.
    pub poster_cache_dir: Option<PathBuf>,
}

impl ServerConfig {
    /// A configuration for `host` and the two ports with the defaults of everything else.
    pub fn new(host: Ipv4Addr, frontend_port: u16, backend_port: u16, assets: &'static [WebFile], web_cache_dir: PathBuf) -> ServerConfig {
        ServerConfig {
            host,
            frontend_port,
            backend_port,
            search: PortSearch::Upward,
            allowed_hosts: Vec::new(),
            machine_names: Vec::new(),
            discovered_lan: Vec::new(),
            timeouts: Timeouts::default(),
            max_connections: MAX_CONNECTIONS,
            max_connections_per_peer: MAX_CONNECTIONS_PER_PEER,
            reserved_for_this_computer: RESERVED_FOR_THIS_COMPUTER,
            max_arriving: MAX_ARRIVING,
            max_arriving_per_peer: MAX_ARRIVING_PER_PEER,
            arriving_kept_for_this_computer: ARRIVING_KEPT_FOR_THIS_COMPUTER,
            assets,
            web_cache_dir,
            poster_cache_dir: None,
        }
    }

    /// The LAN addresses the listeners serve (none on loopback).
    pub fn lan_addresses(&self) -> Vec<Ipv4Addr> {
        network::lan_addresses_for(self.host, &self.discovered_lan)
    }
}

/// The two listeners, bound and not yet serving.
pub struct Listeners {
    /// The page listener.
    pub frontend: TcpListener,
    /// The API listener.
    pub backend: TcpListener,
}

impl Listeners {
    /// Binds both listeners as `config` says: the API listener first, then the page listener on
    /// another port (web brief 4.3).
    pub fn bind(config: &ServerConfig) -> Result<Listeners, BindError> {
        let backend = network::bind_listener(config.host, config.backend_port, Some(config.frontend_port).filter(|port| *port != 0 && config.search == PortSearch::Exact), config.search)?;
        let backend_port = backend.local_addr().map(|address| address.port()).ok();
        let frontend = network::bind_listener(config.host, config.frontend_port, backend_port, config.search)?;
        Ok(Listeners { frontend, backend })
    }
}

/// The client a connection is counted for: its address, except that every loopback address
/// (all of 127.0.0.0/8, ::1) is this computer and counts as one client. Any program here can
/// connect from 127.0.0.2, 127.0.0.3, ..., and must not get a share of its own for each.
pub fn peer_group(peer: IpAddr) -> IpAddr {
    if guard::is_local_peer(peer) {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        peer
    }
}

/// The limits of one share of a listener: its places, or its arriving connections.
#[derive(Clone, Copy, Debug)]
struct ShareLimits {
    /// Connections in all.
    max: usize,
    /// Connections of one client.
    per_peer: usize,
    /// Connections only this computer may have.
    reserved_local: usize,
}

/// How many connections each client has in one share, and how many all clients that are not on
/// this computer have.
#[derive(Debug, Default)]
struct Shares {
    groups: HashMap<IpAddr, usize>,
    remote: usize,
    total: usize,
}

impl Shares {
    fn of(&self, group: IpAddr) -> usize {
        self.groups.get(&group).copied().unwrap_or(0)
    }

    fn add(&mut self, group: IpAddr) {
        *self.groups.entry(group).or_insert(0) += 1;
        self.total += 1;
        if !guard::is_local_peer(group) {
            self.remote += 1;
        }
    }

    fn remove(&mut self, group: IpAddr) {
        if let Some(count) = self.groups.get_mut(&group) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                self.groups.remove(&group);
            }
        }
        self.total = self.total.saturating_sub(1);
        if !guard::is_local_peer(group) {
            self.remote = self.remote.saturating_sub(1);
        }
    }

    /// Whether one more connection of `group` fits without another one making way.
    fn fits(&self, group: IpAddr, limits: ShareLimits) -> bool {
        self.total < limits.max && self.of(group) < limits.per_peer && (guard::is_local_peer(group) || self.remote < limits.max.saturating_sub(limits.reserved_local))
    }

    /// Whether a connection of `holder` may make way for a new one of `group` that does not fit:
    /// only one of the same client when that client has its share already, only one of a client
    /// that is not on this computer when a device on the network finds only what is kept for
    /// this computer, else anyone's.
    fn may_make_way(&self, group: IpAddr, holder: IpAddr, limits: ShareLimits) -> bool {
        if self.of(group) >= limits.per_peer {
            holder == group
        } else if !guard::is_local_peer(group) && self.remote >= limits.max.saturating_sub(limits.reserved_local) {
            !guard::is_local_peer(holder)
        } else {
            true
        }
    }
}

/// What closes a connection from outside its thread: when it makes way for another one, and when
/// the server stops.
#[derive(Debug)]
struct Handle {
    /// A second handle of its socket (`TcpStream::try_clone`): it shows whether a request is on
    /// its way, and shuts the receiving side.
    stream: TcpStream,
    /// The socket the connection's thread reads and writes. A shutdown does not end a read that
    /// waits (on Windows it waits on until its timeout, up to 15 s), so that read is cancelled:
    /// the thread ends at once, and a client that opens connections faster than they time out
    /// cannot pile up threads.
    #[cfg(windows)]
    socket: u64,
}

impl Handle {
    /// The handle of `stream`; `None` when its socket could not be cloned (that connection then
    /// never makes way, and ends on its own).
    fn of(stream: &TcpStream) -> Option<Handle> {
        Some(Handle {
            stream: stream.try_clone().ok()?,
            #[cfg(windows)]
            socket: std::os::windows::io::AsRawSocket::as_raw_socket(stream),
        })
    }

    /// True when the client sent bytes that the connection's thread has not read yet: a request
    /// on its way to being served, which must not be thrown away to make way.
    fn has_unread_bytes(&self) -> bool {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawSocket;
            use windows::Win32::Networking::WinSock::{ioctlsocket, FIONREAD, SOCKET};
            let mut count: u32 = 0;
            // SAFETY: the socket is our own handle of it, open as long as `self`; `count` is a
            // live u32 that the call writes.
            let result = unsafe { ioctlsocket(SOCKET(self.stream.as_raw_socket() as usize), FIONREAD, &mut count) };
            // A count that cannot be read counts as bytes: a connection is never closed on a guess.
            result != 0 || count > 0
        }
        #[cfg(not(windows))]
        {
            false
        }
    }

    /// Ends the connection: a read its thread waits in returns at once, and every later read
    /// finds the end, so the thread closes the socket (the client then sees the end). Only the
    /// receiving side is shut: a thread that has just read a whole request still writes its
    /// answer ("Too many connections" when it has no place), so a request is never left without
    /// one. Only called with the `Admission` lock held while the connection is in it: its thread
    /// gives its entry back (`Held`) before it closes its socket, so the socket cancelled here is
    /// open, never another one that got its number since.
    fn close(&self) {
        let _ = self.stream.shutdown(Shutdown::Read);
        #[cfg(windows)]
        {
            use windows::Win32::Foundation::HANDLE;
            use windows::Win32::System::IO::CancelIoEx;
            // SAFETY: the socket is open (see above); cancelling its I/O touches nothing else.
            unsafe {
                let _ = CancelIoEx(HANDLE(self.socket as usize as *mut core::ffi::c_void), None);
            }
        }
    }
}

/// What a connection does.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Stage {
    /// Its first request has not fully arrived: it holds no place.
    Arriving,
    /// It holds a place and waits: for its next request, or while it reads what its client still
    /// sends after an answer (a body the route did not read, what comes after its last answer).
    Waiting,
    /// It holds a place and works on a request: reads its body, makes or sends its answer.
    Working,
}

/// One accepted connection.
#[derive(Debug)]
struct Entry {
    handle: Option<Handle>,
    /// The client (`peer_group`).
    group: IpAddr,
    stage: Stage,
    /// When it arrived, or last started to wait.
    since: Instant,
}

/// The connections of one listener: those whose first request is arriving, and those that hold a
/// place. A connection takes a place only once its first request has fully arrived (`admit`):
/// a client that opens connections and sends nothing, or half a request, takes no place however
/// often it opens them again, and the connections that wait longest make way for new ones. A
/// request that finds no place takes the place of a connection that waits (`Stage::Waiting`): a
/// browser sends its request as soon as it has opened a connection, and opens a new one when an
/// idle one was closed, so the connections that make way are those that hold a place without
/// using it. A connection whose client sent bytes its thread has not read never makes way; one
/// whose thread has just read its whole request may, and that request is answered all the same
/// (`admit`). A connection whose request has not arrived yet does make way: a client that opens
/// new connections faster than its others send their requests loses those others
/// (`MAX_ARRIVING`).
#[derive(Debug, Default)]
struct Admission {
    entries: HashMap<u64, Entry>,
    arriving: Shares,
    places: Shares,
}

impl Admission {
    fn shares(&self, stage: Stage) -> &Shares {
        if stage == Stage::Arriving {
            &self.arriving
        } else {
            &self.places
        }
    }

    fn remove(&mut self, id: u64) -> Option<Entry> {
        let entry = self.entries.remove(&id)?;
        if entry.stage == Stage::Arriving {
            self.arriving.remove(entry.group);
        } else {
            self.places.remove(entry.group);
        }
        Some(entry)
    }

    /// The connection at `stage` that makes way for a new one of `group` (`Shares::may_make_way`):
    /// of the client with the most connections there, the one that has waited longest, unless
    /// its client sent bytes it has not read.
    fn making_way(&self, group: IpAddr, stage: Stage, limits: ShareLimits) -> Option<u64> {
        let shares = self.shares(stage);
        let mut candidates: Vec<(u64, &Entry)> = self.entries.iter().filter(|(_, entry)| entry.stage == stage && entry.handle.is_some() && shares.may_make_way(group, entry.group, limits)).map(|(id, entry)| (*id, entry)).collect();
        candidates.sort_by(|(_, a), (_, b)| shares.of(b.group).cmp(&shares.of(a.group)).then(a.since.cmp(&b.since)));
        candidates.into_iter().find(|(_, entry)| entry.handle.as_ref().is_some_and(|handle| !handle.has_unread_bytes())).map(|(id, _)| id)
    }

    /// Closes connection `id` (its thread ends at once) and forgets it.
    fn make_way(&mut self, id: u64) {
        if let Some(handle) = self.remove(id).and_then(|entry| entry.handle) {
            handle.close();
        }
    }

    /// Connection `id` of `peer` was accepted: it waits for its first request without a place.
    /// When its client, or every client, has as many arriving already, one of them makes way.
    /// False when none can: it is refused.
    fn arrive(&mut self, id: u64, peer: IpAddr, handle: Option<Handle>, limits: ShareLimits) -> bool {
        let group = peer_group(peer);
        if !self.arriving.fits(group, limits) {
            let Some(old) = self.making_way(group, Stage::Arriving, limits) else {
                return false;
            };
            self.make_way(old);
        }
        self.entries.insert(id, Entry { handle, group, stage: Stage::Arriving, since: Instant::now() });
        self.arriving.add(group);
        true
    }

    /// The first request of connection `id` of `peer` has fully arrived: it takes a place and
    /// works on it, when need be the place of a connection that waits. Without a place it is
    /// refused and forgotten.
    ///
    /// Its thread read the request before it asked, and once the bytes are read nothing shows
    /// that a request is on its way (`Handle::has_unread_bytes`): in between, a newer connection
    /// may have closed this one to make way. A request that needs nothing more from the socket
    /// (`in_hand`) then takes a place as a new one would, and is answered as the connection's
    /// last (its receiving side is shut); one that still needs its body is refused.
    fn admit(&mut self, id: u64, peer: IpAddr, in_hand: bool, limits: ShareLimits) -> Admitted {
        let (group, admitted) = match self.entries.get(&id) {
            Some(entry) if entry.stage != Stage::Arriving => return Admitted::Yes,
            Some(entry) => (entry.group, Admitted::Yes),
            None if in_hand => (peer_group(peer), Admitted::Last),
            None => return Admitted::No,
        };
        if !self.places.fits(group, limits) {
            match self.making_way(group, Stage::Waiting, limits) {
                Some(old) => self.make_way(old),
                None => {
                    self.remove(id);
                    return Admitted::No;
                }
            }
        }
        match self.remove(id) {
            // Its arriving share is given back; it comes back with a place.
            Some(entry) => {
                self.entries.insert(id, Entry { stage: Stage::Working, since: Instant::now(), ..entry });
            }
            // Closed to make way: no handle, so it is never closed again, and it ends after its
            // answer.
            None => {
                self.entries.insert(id, Entry { handle: None, group, stage: Stage::Working, since: Instant::now() });
            }
        }
        self.places.add(group);
        admitted
    }

    /// Notes whether connection `id`, once it holds a place, works on a request or waits.
    fn mark(&mut self, id: u64, busy: bool) {
        if let Some(entry) = self.entries.get_mut(&id) {
            match (entry.stage, busy) {
                (Stage::Arriving, _) | (Stage::Working, true) | (Stage::Waiting, false) => {}
                (Stage::Waiting, true) => entry.stage = Stage::Working,
                (Stage::Working, false) => {
                    entry.stage = Stage::Waiting;
                    entry.since = Instant::now();
                }
            }
        }
    }

    /// Gives back what connection `id` holds (nothing when it was closed to make way).
    fn release(&mut self, id: u64) {
        self.remove(id);
    }

    /// Closes every connection that does not work on a request (a stopping server).
    fn close_waiting(&self) {
        for entry in self.entries.values() {
            if let (true, Some(handle)) = (entry.stage != Stage::Working, &entry.handle) {
                handle.close();
            }
        }
    }
}

/// The connections of one listener, and the next connection id.
#[derive(Default)]
struct Connections {
    admission: Mutex<Admission>,
    next: AtomicU64,
}

impl Connections {
    fn admission(&self) -> std::sync::MutexGuard<'_, Admission> {
        self.admission.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Closes the connections that wait (a stopping server).
    fn shut_down_idle(&self) {
        self.admission().close_waiting();
    }

    fn live(&self) -> usize {
        self.admission().entries.len()
    }
}

/// One running listener.
struct RunningListener {
    address: SocketAddr,
    thread: Option<JoinHandle<()>>,
    connections: Arc<Connections>,
}

/// The running server.
pub struct Server {
    frontend: RunningListener,
    backend: RunningListener,
    host: Ipv4Addr,
    lan: Vec<Ipv4Addr>,
    stop: Arc<AtomicBool>,
    shared: Arc<routes::Shared>,
}

/// A connection's entry and socket, owned by its thread. Dropping it gives the entry back BEFORE
/// the socket is closed (fields are dropped after `drop`): `Handle::close` must never cancel a
/// socket that was closed already.
struct Held {
    connections: Arc<Connections>,
    id: u64,
    stream: Option<TcpStream>,
}

impl Drop for Held {
    fn drop(&mut self) {
        self.connections.admission().release(self.id);
    }
}

/// The listener as the thread of one connection sees it.
struct ListenerGate<'a> {
    connections: &'a Connections,
    id: u64,
    peer: IpAddr,
    places: ShareLimits,
    rejector: Option<&'a mpsc::SyncSender<TcpStream>>,
}

impl http::Gate for ListenerGate<'_> {
    fn admit(&self, in_hand: bool) -> Admitted {
        self.connections.admission().admit(self.id, self.peer, in_hand, self.places)
    }

    fn busy(&self, busy: bool) {
        self.connections.admission().mark(self.id, busy);
    }

    fn refuse(&self, stream: TcpStream) {
        // A full queue closes the connection at once (dropping the stream).
        if let Some(rejector) = self.rejector {
            let _ = rejector.try_send(stream);
        }
    }
}

/// The `Server` header.
fn server_name() -> String {
    format!("{}/{}", crate::settings::APP_NAME, env!("CARGO_PKG_VERSION"))
}

/// The whole answer to a connection that gets no place.
fn refusal(server: &str) -> Vec<u8> {
    let mut body = serde_json::Map::new();
    body.insert("detail".to_owned(), serde_json::Value::from(messages::TOO_MANY_CONNECTIONS));
    let body = serde_json::Value::Object(body).to_string();
    format!("HTTP/1.1 503 Service Unavailable\r\nContent-Type: application/json\r\nContent-Length: {}\r\nServer: {server}\r\nX-Content-Type-Options: nosniff\r\nConnection: close\r\n\r\n{body}", body.len()).into_bytes()
}

/// A refused connection that has its 503 and is read from until it closes or its time is up.
struct Refused {
    stream: TcpStream,
    until: Instant,
    read: usize,
}

/// Sends the 503 to a refused connection without waiting for it: the socket is switched to
/// non-blocking, and the answer, a few hundred bytes, goes into its empty send buffer at once.
/// `None` when that failed (the connection is closed).
fn answer_refused(mut stream: TcpStream, answer: &[u8]) -> Option<Refused> {
    stream.set_nonblocking(true).ok()?;
    match stream.write(answer) {
        Ok(written) if written == answer.len() => {}
        _ => return None,
    }
    stream.shutdown(Shutdown::Write).ok()?;
    Some(Refused { stream, until: Instant::now() + Duration::from_millis(REJECT_LINGER_MS), read: 0 })
}

/// Reads what a refused client sent, without waiting. False when it is done: it closed, sent
/// `REJECT_LINGER_BYTES`, or had its `REJECT_LINGER_MS`.
fn keeps_lingering(refused: &mut Refused) -> bool {
    let mut chunk = [0_u8; 4096];
    while Instant::now() < refused.until && refused.read < REJECT_LINGER_BYTES {
        match refused.stream.read(&mut chunk) {
            Ok(0) => return false,
            Ok(count) => refused.read += count,
            Err(error) if error.kind() == stdio::ErrorKind::WouldBlock => return true,
            Err(_) => return false,
        }
    }
    false
}

/// The `web-reject` thread: answers refused connections with 503, then reads what each one sent
/// for a moment before closing it: closing a socket with unread bytes makes Windows reset the
/// connection, and the client would throw the answer away. The sockets are polled side by side
/// (`REJECT_POLL_MS`), so a client that sends nothing and never closes costs no other refused
/// client its answer. The thread ends when both accept loops have ended (they hold the
/// senders). `None` when it could not be started (refused connections are then closed without an
/// answer).
fn start_rejector() -> Option<mpsc::SyncSender<TcpStream>> {
    let (sender, receiver) = mpsc::sync_channel::<TcpStream>(REJECT_QUEUE);
    let answer = refusal(&server_name());
    let spawned = std::thread::Builder::new().name("web-reject".to_owned()).spawn(move || {
        let mut lingering: VecDeque<Refused> = VecDeque::new();
        loop {
            let first = if lingering.is_empty() {
                match receiver.recv() {
                    Ok(stream) => Some(stream),
                    Err(_) => return,
                }
            } else {
                match receiver.recv_timeout(Duration::from_millis(REJECT_POLL_MS)) {
                    Ok(stream) => Some(stream),
                    Err(mpsc::RecvTimeoutError::Timeout) => None,
                    Err(mpsc::RecvTimeoutError::Disconnected) => return,
                }
            };
            // What else is queued, at most a queue's worth, before the others are read again.
            for stream in first.into_iter().chain(std::iter::from_fn(|| receiver.try_recv().ok()).take(REJECT_QUEUE)) {
                if let Some(refused) = answer_refused(stream, &answer) {
                    if lingering.len() >= REJECT_LINGERING_MAX {
                        lingering.pop_front();
                    }
                    lingering.push_back(refused);
                }
            }
            lingering.retain_mut(keeps_lingering);
        }
    });
    match spawned {
        Ok(_) => Some(sender),
        Err(error) => {
            log::warn!("The web-reject thread could not be started: {error}");
            None
        }
    }
}

/// The limits of one accept loop.
#[derive(Clone, Copy)]
struct Limits {
    timeouts: Timeouts,
    places: ShareLimits,
    arriving: ShareLimits,
}

/// Serves one listener until `stop` is set.
fn accept_loop(listener: TcpListener, handler: Arc<routes::Routes>, stop: Arc<AtomicBool>, connections: Arc<Connections>, limits: Limits, rejector: Option<mpsc::SyncSender<TcpStream>>) {
    let name = server_name();
    let timeouts = limits.timeouts;
    for incoming in listener.incoming() {
        if stop.load(Ordering::SeqCst) {
            break;
        }
        let Ok(stream) = incoming else {
            // A connection that failed before it was accepted (the client gave up): go on.
            std::thread::sleep(Duration::from_millis(5));
            continue;
        };
        let Ok(peer) = stream.peer_addr() else { continue };
        let id = connections.next.fetch_add(1, Ordering::SeqCst);
        if !connections.admission().arrive(id, peer.ip(), Handle::of(&stream), limits.arriving) {
            // A full queue closes the connection at once (dropping the stream).
            if let Some(rejector) = &rejector {
                let _ = rejector.try_send(stream);
            }
            continue;
        }
        let _ = stream.set_nodelay(true);
        let held = Held { connections: Arc::clone(&connections), id, stream: Some(stream) };
        let handler = Arc::clone(&handler);
        let stop = Arc::clone(&stop);
        let name = name.clone();
        let rejector = rejector.clone();
        let places = limits.places;
        let spawned = std::thread::Builder::new().name("web-connection".to_owned()).spawn(move || {
            let mut held = held;
            let Some(stream) = held.stream.take() else { return };
            let gate = ListenerGate { connections: &held.connections, id: held.id, peer: peer.ip(), places, rejector: rejector.as_ref() };
            let left = http::Connection::new(stream, timeouts).serve(peer, handler.as_ref(), &stop, &name, &gate);
            // Closed when `held` is dropped, after its entry is given back.
            held.stream = left;
        });
        if let Err(error) = spawned {
            log::warn!("A connection of the browser pages could not be served: {error}");
        }
    }
}

impl Server {
    /// Binds both listeners and serves them.
    pub fn start(config: &ServerConfig, host: Arc<dyn WebHost>, library: Arc<Library>) -> Result<Server, BindError> {
        let listeners = Listeners::bind(config)?;
        Ok(Server::serve(config, listeners, host, library))
    }

    /// Serves listeners that are bound already.
    pub fn serve(config: &ServerConfig, listeners: Listeners, host: Arc<dyn WebHost>, library: Arc<Library>) -> Server {
        let posters = config.poster_cache_dir.clone().map(|dir| files::PosterService::new(crate::poster::PosterCache::new(dir)));
        let shared = Arc::new(routes::Shared { host, library, assets: config.assets, heic: files::HeicCache::new(config.web_cache_dir.clone()), posters });
        let stop = Arc::new(AtomicBool::new(false));
        let lan = config.lan_addresses();
        let address_of = |listener: &TcpListener| listener.local_addr().unwrap_or_else(|_| SocketAddr::V4(SocketAddrV4::new(config.host, 0)));
        let frontend_address = address_of(&listeners.frontend);
        let backend_address = address_of(&listeners.backend);
        let page_guard = guard::Guard::new(&config.allowed_hosts, &config.machine_names, &[]);
        let api_guard = guard::Guard::new(&config.allowed_hosts, &config.machine_names, &guard::page_origins(frontend_address.port(), &config.machine_names, &lan));
        let places = ShareLimits { max: config.max_connections, per_peer: config.max_connections_per_peer, reserved_local: config.reserved_for_this_computer };
        let arriving = ShareLimits { max: config.max_arriving, per_peer: config.max_arriving_per_peer, reserved_local: config.arriving_kept_for_this_computer };
        let limits = Limits { timeouts: config.timeouts, places, arriving };
        let rejector = start_rejector();
        let spawn = |listener: TcpListener, kind: routes::ListenerKind, guard: guard::Guard, name: &str| {
            let handler = Arc::new(routes::Routes { kind, guard, shared: Arc::clone(&shared) });
            let connections = Arc::new(Connections::default());
            let (stop, loop_connections, rejector) = (Arc::clone(&stop), Arc::clone(&connections), rejector.clone());
            let thread = match std::thread::Builder::new().name(name.to_owned()).spawn(move || accept_loop(listener, handler, stop, loop_connections, limits, rejector)) {
                Ok(thread) => Some(thread),
                Err(error) => {
                    log::warn!("The {name} thread could not be started: {error}");
                    None
                }
            };
            (thread, connections)
        };
        let (frontend_thread, frontend_connections) = spawn(listeners.frontend, routes::ListenerKind::Frontend, page_guard, "web-pages");
        let (backend_thread, backend_connections) = spawn(listeners.backend, routes::ListenerKind::Backend, api_guard, "web-api");
        Server {
            frontend: RunningListener { address: frontend_address, thread: frontend_thread, connections: frontend_connections },
            backend: RunningListener { address: backend_address, thread: backend_thread, connections: backend_connections },
            host: config.host,
            lan,
            stop,
            shared,
        }
    }

    /// The address the page listener is bound to.
    pub fn frontend_address(&self) -> SocketAddr {
        self.frontend.address
    }

    /// The address the API listener is bound to.
    pub fn backend_address(&self) -> SocketAddr {
        self.backend.address
    }

    /// The address both listeners are bound to.
    pub fn host(&self) -> Ipv4Addr {
        self.host
    }

    /// The LAN addresses the pages are served on.
    pub fn lan_addresses(&self) -> &[Ipv4Addr] {
        &self.lan
    }

    /// The browser imports folder of this server.
    pub fn library(&self) -> &Arc<Library> {
        &self.shared.library
    }

    /// Stops both listeners within `budget`: the accept threads are woken and joined, the
    /// connections that wait for a next request are closed, a request being served ends on its
    /// own thread (an upload being received ends at its next piece, see `Library::cancel_uploads`).
    /// True when both listeners are closed (their ports are free).
    pub fn stop(mut self, budget: Duration) -> bool {
        let deadline = Instant::now() + budget;
        self.stop.store(true, Ordering::SeqCst);
        self.frontend.connections.shut_down_idle();
        self.backend.connections.shut_down_idle();
        for listener in [&self.frontend, &self.backend] {
            // The accept thread waits in accept(): a connection of our own wakes it.
            let target = match listener.address {
                SocketAddr::V4(address) if address.ip().is_unspecified() => SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, address.port())),
                other => other,
            };
            let _ = TcpStream::connect_timeout(&target, Duration::from_millis(100));
        }
        let mut stopped = true;
        for listener in [&mut self.frontend, &mut self.backend] {
            if let Some(thread) = listener.thread.take() {
                while !thread.is_finished() && Instant::now() < deadline {
                    std::thread::sleep(Duration::from_millis(5));
                }
                if thread.is_finished() {
                    let _ = thread.join();
                } else {
                    stopped = false;
                }
            }
        }
        // Connections that came in meanwhile wait for nothing either.
        self.frontend.connections.shut_down_idle();
        self.backend.connections.shut_down_idle();
        stopped
    }

    /// How many connections are being served (both listeners).
    pub fn live_connections(&self) -> usize {
        self.frontend.connections.live() + self.backend.connections.live()
    }
}

/// Why `restart` did not move the server, and the server that runs now (the old one, or the old
/// configuration started again), if any.
pub struct RestartFailure {
    /// Why the new ports could not be bound.
    pub error: BindError,
    /// The server that serves now.
    pub server: Option<Server>,
}

/// Moves a running server to new ports or another address (the Browser view sheet's Save, the
/// LAN switch): the ports of `config` that the old server does not hold are bound first, so that
/// a port another program holds is refused before anything changes (`Err` gives the old server
/// back, still running). Then the old server stops, its uploads are cancelled, the remaining
/// ports are bound and the new server serves. Should a port the old server held not bind again,
/// the old configuration is started again and `Err` carries it.
pub fn restart(old: Option<Server>, config: &ServerConfig, previous: Option<&ServerConfig>, host: Arc<dyn WebHost>, library: Arc<Library>) -> Result<Server, Box<RestartFailure>> {
    let held: Vec<u16> = old.iter().flat_map(|server| [server.frontend_address().port(), server.backend_address().port()]).collect();
    let bind_exact = |port: u16, skip: Option<u16>| network::bind_listener(config.host, port, skip, if port == 0 { PortSearch::Upward } else { config.search });
    // 1. Ports nobody of ours holds: bound now, before the old server is touched.
    let mut backend = None;
    let mut frontend = None;
    if !held.contains(&config.backend_port) || config.backend_port == 0 {
        match bind_exact(config.backend_port, Some(config.frontend_port).filter(|port| *port != 0)) {
            Ok(listener) => backend = Some(listener),
            Err(error) => return Err(Box::new(RestartFailure { error, server: old })),
        }
    }
    if !held.contains(&config.frontend_port) || config.frontend_port == 0 {
        let skip = backend.as_ref().and_then(|listener: &TcpListener| listener.local_addr().ok()).map(|address| address.port()).or(Some(config.backend_port).filter(|port| *port != 0));
        match bind_exact(config.frontend_port, skip) {
            Ok(listener) => frontend = Some(listener),
            Err(error) => return Err(Box::new(RestartFailure { error, server: old })),
        }
    }
    // 2. The old server goes, with the uploads it was receiving.
    if let Some(old) = old {
        library.cancel_uploads();
        if !old.stop(Duration::from_millis(WEB_STOP_WAIT_MS)) {
            log::warn!("The browser pages did not stop within {WEB_STOP_WAIT_MS} ms.");
        }
    }
    // 3. The ports the old server held.
    let rebound = (|| -> Result<Listeners, BindError> {
        let backend = match backend {
            Some(listener) => listener,
            None => bind_exact(config.backend_port, Some(config.frontend_port))?,
        };
        let frontend = match frontend {
            Some(listener) => listener,
            None => bind_exact(config.frontend_port, backend.local_addr().ok().map(|address| address.port()))?,
        };
        Ok(Listeners { frontend, backend })
    })();
    match rebound {
        Ok(listeners) => Ok(Server::serve(config, listeners, host, library)),
        Err(error) => {
            let restored = previous.and_then(|previous| {
                let mut previous = previous.clone();
                previous.search = PortSearch::Upward;
                Server::start(&previous, Arc::clone(&host), Arc::clone(&library)).ok()
            });
            Err(Box::new(RestartFailure { error, server: restored }))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn address(text: &str) -> IpAddr {
        text.parse().expect("an address")
    }

    /// Accepted connections whose both ends the tests hold: a handle needs a real socket.
    struct Sockets {
        listener: TcpListener,
        kept: Vec<(TcpStream, TcpStream)>,
    }

    impl Sockets {
        fn new() -> Sockets {
            Sockets { listener: TcpListener::bind("127.0.0.1:0").expect("a listener"), kept: Vec::new() }
        }

        /// A new accepted connection whose client sent `request`; the handle of its server end.
        fn handle(&mut self, request: &[u8]) -> Option<Handle> {
            let mut client = TcpStream::connect(self.listener.local_addr().expect("address")).expect("connect");
            client.write_all(request).expect("the request");
            let (server, _) = self.listener.accept().expect("accept");
            let handle = Handle::of(&server).expect("a handle");
            // The bytes are in the server's socket before the test looks.
            let deadline = Instant::now() + Duration::from_secs(5);
            while !request.is_empty() && !handle.has_unread_bytes() && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(5));
            }
            self.kept.push((client, server));
            Some(handle)
        }
    }

    /// Connection `id` of `peer` arrives and its first request arrives too; it then works on it
    /// (`busy`) or waits. `Err` when it gets no place.
    fn take(admission: &mut Admission, sockets: &mut Sockets, id: u64, peer: &str, busy: bool, limits: ShareLimits) -> Result<(), ()> {
        let arriving = ShareLimits { max: 1000, per_peer: 1000, reserved_local: 0 };
        if !admission.arrive(id, address(peer), sockets.handle(b""), arriving) || admission.admit(id, address(peer), true, limits) != Admitted::Yes {
            return Err(());
        }
        admission.mark(id, busy);
        Ok(())
    }

    #[test]
    fn one_client_cannot_take_every_place_and_working_connections_keep_theirs() {
        let limits = ShareLimits { max: 4, per_peer: 2, reserved_local: 0 };
        let (mut admission, mut sockets) = (Admission::default(), Sockets::new());
        assert!(take(&mut admission, &mut sockets, 1, "127.0.0.1", true, limits).is_ok() && take(&mut admission, &mut sockets, 2, "127.0.0.1", true, limits).is_ok());
        assert!(take(&mut admission, &mut sockets, 3, "127.0.0.1", true, limits).is_err(), "the third busy connection of one client is refused");
        assert!(!admission.entries.contains_key(&3), "and forgotten");
        assert!(take(&mut admission, &mut sockets, 3, "127.0.0.2", true, limits).is_err(), "127.0.0.2 is this computer as well");
        assert!(take(&mut admission, &mut sockets, 4, "192.168.1.30", true, limits).is_ok(), "while another address is still served");
        assert!(take(&mut admission, &mut sockets, 5, "192.168.1.31", true, limits).is_ok());
        assert!(take(&mut admission, &mut sockets, 6, "192.168.1.32", true, limits).is_err(), "the listener is full of busy connections");
        admission.release(1);
        assert_eq!(admission.places.total, 3, "a place that was given back");
        assert!(take(&mut admission, &mut sockets, 6, "192.168.1.32", true, limits).is_ok());
        for id in [4, 4, 5] {
            admission.release(id);
        }
        assert!(!admission.places.groups.contains_key(&address("192.168.1.31")), "a client without connections is forgotten");
        assert_eq!((admission.places.total, admission.places.remote), (2, 1), "a second release of one connection changes nothing");
    }

    #[test]
    fn connections_take_a_place_only_once_their_first_request_has_arrived() {
        let places = ShareLimits { max: 4, per_peer: 2, reserved_local: 0 };
        let arriving = ShareLimits { max: 6, per_peer: 3, reserved_local: 0 };
        let (mut admission, mut sockets) = (Admission::default(), Sockets::new());
        // Three connections of this computer that send nothing: no place is taken.
        for id in 1..=3 {
            assert!(admission.arrive(id, address("127.0.0.1"), sockets.handle(b""), arriving));
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!((admission.arriving.total, admission.places.total), (3, 0));
        // A fourth, from another loopback address: the one that has waited longest makes way.
        assert!(admission.arrive(4, address("127.0.0.9"), sockets.handle(b""), arriving));
        assert!(!admission.entries.contains_key(&1) && admission.entries.contains_key(&2));
        let mut byte = [0_u8; 1];
        sockets.kept[0].1.set_read_timeout(Some(Duration::from_secs(5))).expect("timeout");
        let started = Instant::now();
        assert_eq!(sockets.kept[0].1.read(&mut byte).ok(), Some(0), "its thread finds the end");
        assert!(started.elapsed() < Duration::from_secs(1), "at once");
        // A connection whose request is in its socket, not read yet, never makes way.
        for id in 5..=7 {
            assert!(admission.arrive(id, address("192.168.1.30"), sockets.handle(b"GET / HTTP/1.1\r\n\r\n"), arriving));
        }
        assert!(!admission.arrive(8, address("192.168.1.30"), sockets.handle(b""), arriving), "nobody of that client may make way: refused");
        // Their requests come: each takes a place.
        assert_eq!((admission.admit(2, address("127.0.0.1"), true, places), admission.admit(5, address("192.168.1.30"), true, places)), (Admitted::Yes, Admitted::Yes));
        assert_eq!((admission.arriving.total, admission.places.total), (4, 2));
        assert_eq!(admission.admit(1, address("127.0.0.1"), false, places), Admitted::No, "a connection that made way before its request had arrived gets no place");
        assert_eq!((admission.arriving.total, admission.places.total), (4, 2));
    }

    #[test]
    fn a_request_read_before_its_connection_made_way_is_still_answered() {
        // E2E4-1: the thread of connection 1 has read its whole request (its socket shows no
        // unread bytes then) when newer connections arrive; it is the oldest and makes way.
        let places = ShareLimits { max: 4, per_peer: 2, reserved_local: 0 };
        let arriving = ShareLimits { max: 6, per_peer: 2, reserved_local: 0 };
        let (mut admission, mut sockets) = (Admission::default(), Sockets::new());
        assert!(admission.arrive(1, address("127.0.0.1"), sockets.handle(b""), arriving));
        std::thread::sleep(Duration::from_millis(2));
        assert!(admission.arrive(2, address("127.0.0.2"), sockets.handle(b""), arriving));
        std::thread::sleep(Duration::from_millis(2));
        assert!(admission.arrive(3, address("127.0.0.2"), sockets.handle(b""), arriving));
        assert!(!admission.entries.contains_key(&1), "the oldest made way");
        // Its request needs nothing more from the socket: it takes a place as a new one would,
        // and is answered as the connection's last.
        assert_eq!(admission.admit(1, address("127.0.0.1"), true, places), Admitted::Last);
        assert_eq!((admission.arriving.total, admission.places.total), (2, 1));
        assert!(admission.entries.get(&1).is_some_and(|entry| entry.stage == Stage::Working && entry.handle.is_none()));
        // It waits after its answer, but it is never closed for another one: it ends by itself.
        admission.mark(1, false);
        assert_eq!(admission.making_way(address("127.0.0.1"), Stage::Waiting, places), None);
        admission.release(1);
        assert_eq!((admission.places.total, admission.entries.len()), (0, 2), "its place is given back");
        // One that still needs its body from the closed socket is refused.
        std::thread::sleep(Duration::from_millis(2));
        assert!(admission.arrive(4, address("127.0.0.3"), sockets.handle(b""), arriving));
        assert!(!admission.entries.contains_key(&2));
        assert_eq!(admission.admit(2, address("127.0.0.2"), false, places), Admitted::No);
        // And one in hand finds no place when every place of its client works on a request.
        let one_place = ShareLimits { max: 4, per_peer: 1, reserved_local: 0 };
        assert_eq!(admission.admit(3, address("127.0.0.2"), true, one_place), Admitted::Yes);
        std::thread::sleep(Duration::from_millis(2));
        assert!(admission.arrive(5, address("127.0.0.1"), sockets.handle(b""), arriving));
        assert!(admission.arrive(6, address("127.0.0.1"), sockets.handle(b""), arriving));
        assert!(!admission.entries.contains_key(&4));
        assert_eq!(admission.admit(4, address("127.0.0.3"), true, one_place), Admitted::No);
        assert_eq!((admission.places.total, admission.arriving.total), (1, 2));
    }

    #[test]
    fn a_connection_that_waits_makes_way_for_a_new_request() {
        let limits = ShareLimits { max: 4, per_peer: 3, reserved_local: 0 };
        let (mut admission, mut sockets) = (Admission::default(), Sockets::new());
        // This computer: three connections that wait for a next request, the oldest first.
        for id in 1..=3 {
            take(&mut admission, &mut sockets, id, "127.0.0.1", false, limits).expect("a place");
            std::thread::sleep(Duration::from_millis(2));
        }
        assert!(take(&mut admission, &mut sockets, 4, "127.0.0.3", false, limits).is_ok(), "a new request of the same client");
        assert!(!admission.entries.contains_key(&1) && admission.entries.contains_key(&2), "took the place of the one idle longest");
        // A phone takes the last free place; then the listener is full, and a laptop's request
        // takes the place of the client with the most connections.
        take(&mut admission, &mut sockets, 5, "192.168.1.30", true, limits).expect("a free place");
        take(&mut admission, &mut sockets, 6, "192.168.1.31", true, limits).expect("an idle connection's place");
        assert!(admission.places.total == 4 && !admission.entries.contains_key(&2), "this computer's longest idle connection went");
        // Busy connections keep theirs: once every connection works on a request, a new one is
        // refused.
        admission.mark(3, true);
        admission.mark(4, true);
        assert!(take(&mut admission, &mut sockets, 7, "192.168.1.32", false, limits).is_err(), "nobody idle to give a place");
    }

    #[test]
    fn devices_on_the_network_never_take_what_is_kept_for_this_computer() {
        let limits = ShareLimits { max: 6, per_peer: 2, reserved_local: 2 };
        let (mut admission, mut sockets) = (Admission::default(), Sockets::new());
        // A device with several addresses fills every place it may take with busy connections.
        let mut id = 0;
        for device in ["192.168.1.40", "192.168.1.41"] {
            for _ in 0..2 {
                id += 1;
                take(&mut admission, &mut sockets, id, device, true, limits).expect("a place");
            }
        }
        assert!(take(&mut admission, &mut sockets, 99, "192.168.1.42", true, limits).is_err(), "the last places are kept for this computer");
        assert!(take(&mut admission, &mut sockets, 10, "127.0.0.1", true, limits).is_ok() && take(&mut admission, &mut sockets, 11, "127.0.0.1", true, limits).is_ok());
        // An idle connection of a device is taken for another device, never one of this computer.
        admission.mark(1, false);
        admission.mark(10, false);
        assert!(take(&mut admission, &mut sockets, 12, "192.168.1.42", true, limits).is_ok());
        assert!(!admission.entries.contains_key(&1) && admission.entries.contains_key(&10));
        // The same for arriving connections: a device never takes the share kept for this computer.
        let arriving = ShareLimits { max: 4, per_peer: 4, reserved_local: 2 };
        let mut arrivals = Admission::default();
        assert!(arrivals.arrive(1, address("127.0.0.1"), sockets.handle(b""), arriving));
        for id in 2..=5 {
            assert!(arrivals.arrive(id, address("192.168.1.50"), sockets.handle(b""), arriving));
        }
        assert!(arrivals.entries.contains_key(&1) && !arrivals.entries.contains_key(&2) && arrivals.arriving.remote == 2, "the device's own oldest made way");
    }
}
