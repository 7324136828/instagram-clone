//! The browser pages from the app's side (web brief 4.3, 4.9 and the Browser view sheet): which
//! addresses and ports apply and where each value comes from, starting the core's HTTP server on
//! a thread of its own, moving it to other ports without restarting the app, publishing what is
//! open after every change, handing the browser's changes to the UI thread, and the clean stop.
//!
//! Precedence of every value: the command line (for this run) > settings.json > the defaults
//! (web brief 4.11). The Browser view sheet's Save writes `web_server`, `frontend_port`,
//! `backend_port` and `web_host` into settings.json (`settings::update_settings_file`) and
//! applies them at once, except a value the command line fixed: that one stays for this run and
//! the typed one is kept for later runs. So the sheet's field of such a value shows what
//! settings.json holds (`sheet_values`), never the run's value: a Save that only changes another
//! field must not turn a one-run value into a saved one.
//!
//! A start that listens on the network because settings.json says so (`web_host`, which Save
//! writes for "Allow other devices on this network") says so at once (`WebEvent::OpenToNetwork`):
//! an open port never comes back silently after a restart.
//!
//! Threads: the UI thread never binds a port (a probe of a closed port takes 150 ms) and never
//! waits for the server to stop; `web-start` and `web-restart` threads do that and hand the
//! result back through a channel that `poll` empties once per frame.

use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{mpsc, Arc};
use std::time::Duration;

use instagram_core::library::Library;
use instagram_core::messages;
use instagram_core::model::Collection;
use instagram_core::settings::{self, AppPaths, ProcessEnv, WebSettings};
use instagram_core::text;
use instagram_core::web::files::{HeicCache, WEB_CACHE_MAX_BYTES};
use instagram_core::web::{self, network, Bridge, FormatCache, PortSearch, Server, ServerConfig, Snapshot, Timeouts, WebCommand, WebFile, WebHost, UI_REPLY_TIMEOUT_MS, WEB_STOP_WAIT_MS};

use crate::loader::Waker;

/// Where a value of the browser pages comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValueSource {
    /// Nothing says otherwise.
    Default,
    /// settings.json.
    Settings,
    /// The command line, for this run.
    CommandLine,
    /// The Browser view sheet, in this run.
    Sheet,
}

/// A value and where it comes from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sourced<T> {
    /// The value.
    pub value: T,
    /// Its origin.
    pub source: ValueSource,
}

impl<T> Sourced<T> {
    fn new(value: T, source: ValueSource) -> Sourced<T> {
        Sourced { value, source }
    }
}

/// The browser pages' options of the command line (`--no-web`, `--host`, `--frontend-port`,
/// `--backend-port`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct WebOverrides {
    /// `--no-web`.
    pub no_web: bool,
    /// `--host ADDRESS`.
    pub host: Option<Ipv4Addr>,
    /// `--frontend-port PORT` (or `--frontend`).
    pub frontend_port: Option<u16>,
    /// `--backend-port PORT` (or `--backend`).
    pub backend_port: Option<u16>,
}

impl WebOverrides {
    /// True when the command line names an address or a port: a `--screenshot` run then starts
    /// the server too.
    pub fn names_an_address(&self) -> bool {
        self.host.is_some() || self.frontend_port.is_some() || self.backend_port.is_some()
    }
}

/// The values in effect, each with its origin.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WebPlan {
    /// The pages on or off.
    pub enabled: Sourced<bool>,
    /// The address to listen on.
    pub host: Sourced<Ipv4Addr>,
    /// The page address port (the first one tried at start-up).
    pub frontend_port: Sourced<u16>,
    /// The API address port.
    pub backend_port: Sourced<u16>,
}

impl WebPlan {
    /// The plan of a start: the command line, else settings.json, else the defaults.
    pub fn new(settings: &WebSettings, overrides: &WebOverrides) -> WebPlan {
        let pick = |cli: Option<u16>, file: Option<u16>, default: u16| match (cli, file) {
            (Some(port), _) => Sourced::new(port, ValueSource::CommandLine),
            (None, Some(port)) => Sourced::new(port, ValueSource::Settings),
            (None, None) => Sourced::new(default, ValueSource::Default),
        };
        WebPlan {
            enabled: if overrides.no_web {
                Sourced::new(false, ValueSource::CommandLine)
            } else {
                match settings.web_server {
                    Some(on) => Sourced::new(on, ValueSource::Settings),
                    None => Sourced::new(true, ValueSource::Default),
                }
            },
            host: match (overrides.host, settings.web_host) {
                (Some(host), _) => Sourced::new(host, ValueSource::CommandLine),
                (None, Some(host)) => Sourced::new(host, ValueSource::Settings),
                (None, None) => Sourced::new(Ipv4Addr::LOCALHOST, ValueSource::Default),
            },
            frontend_port: pick(overrides.frontend_port, settings.frontend_port, web::DEFAULT_FRONTEND_PORT),
            backend_port: pick(overrides.backend_port, settings.backend_port, web::DEFAULT_BACKEND_PORT),
        }
    }

    /// True when the pages listen on every network adapter or on one LAN address.
    pub fn lan(&self) -> bool {
        !self.host.value.is_loopback()
    }

    /// True when a start with this plan must say that other devices can open the pages: it
    /// listens on the network and the command line did not ask for that in this run.
    pub fn announces_network(&self) -> bool {
        self.enabled.value && self.lan() && self.host.source != ValueSource::CommandLine
    }
}

/// What the Browser view sheet's fields and switches show when it opens: for each value, the one
/// a Save would keep for later runs. That is the value in effect, except where the command line
/// fixed it for this run: then settings.json's (or the default).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SheetValues {
    /// "Show the open posts in a browser".
    pub enabled: bool,
    /// "Allow other devices on this network".
    pub lan: bool,
    /// "Page address port".
    pub frontend_port: u16,
    /// "API address port".
    pub backend_port: u16,
}

impl SheetValues {
    /// The values of `plan`, with settings.json's in place of the command line's.
    pub fn of(plan: &WebPlan, settings: &WebSettings) -> SheetValues {
        let saved = |sourced: Sourced<u16>, file: u16| if sourced.source == ValueSource::CommandLine { file } else { sourced.value };
        SheetValues {
            enabled: if plan.enabled.source == ValueSource::CommandLine { settings.web_server_or_default() } else { plan.enabled.value },
            lan: if plan.host.source == ValueSource::CommandLine { !settings.web_host_or_default().is_loopback() } else { plan.lan() },
            frontend_port: saved(plan.frontend_port, settings.frontend_port_or_default()),
            backend_port: saved(plan.backend_port, settings.backend_port_or_default()),
        }
    }
}

/// How the browser pages are set up for this run.
#[derive(Debug, Clone)]
pub struct WebConfig {
    /// The keys of settings.json.
    pub settings: WebSettings,
    /// The command line.
    pub overrides: WebOverrides,
    /// The vendored pages.
    pub assets: &'static [WebFile],
    /// How long clients are waited for.
    pub timeouts: Timeouts,
    /// How long an HTTP thread waits for the UI thread.
    pub reply_timeout: Duration,
    /// Start the server at the first frame (false: only when the sheet turns it on).
    pub start: bool,
}

impl WebConfig {
    /// The configuration of a normal run.
    pub fn new(settings: WebSettings, overrides: WebOverrides, assets: &'static [WebFile]) -> WebConfig {
        WebConfig { settings, overrides, assets, timeouts: Timeouts::default(), reply_timeout: Duration::from_millis(UI_REPLY_TIMEOUT_MS), start: true }
    }

    /// A configuration that starts nothing by itself (headless tests, screenshots without an
    /// address on the command line).
    pub fn not_started(settings: WebSettings, assets: &'static [WebFile]) -> WebConfig {
        WebConfig { start: false, ..WebConfig::new(settings, WebOverrides::default(), assets) }
    }
}

/// Why the pages are off.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OffReason {
    /// `"web_server": false` in settings.json.
    Settings,
    /// `--no-web`.
    CommandLine,
    /// Turned off in the Browser view sheet.
    Sheet,
    /// Not started in this run (tests and screenshots).
    NotStarted,
}

/// Where the running server answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunningAddresses {
    /// The page listener's address.
    pub frontend: SocketAddr,
    /// The API listener's address.
    pub backend: SocketAddr,
    /// The LAN addresses the pages are served on.
    pub lan: Vec<Ipv4Addr>,
}

impl RunningAddresses {
    /// The address a browser on this computer opens: 127.0.0.1 for every-adapter or loopback
    /// listeners (the server listens on IPv4 only, so not "localhost"), else the address itself.
    fn local_host(address: SocketAddr) -> String {
        match address {
            SocketAddr::V4(v4) if v4.ip().is_unspecified() || v4.ip().is_loopback() => Ipv4Addr::LOCALHOST.to_string(),
            other => other.ip().to_string(),
        }
    }

    /// The page address: `http://127.0.0.1:3127/`.
    pub fn page_url(&self) -> String {
        format!("{}/", network::http_url(Self::local_host(self.frontend), self.frontend.port()))
    }

    /// The API address: `http://127.0.0.1:8317` (its `/docs` lists the routes).
    pub fn api_url(&self) -> String {
        network::http_url(Self::local_host(self.backend), self.backend.port())
    }

    /// The page address on each LAN address.
    pub fn lan_page_urls(&self) -> Vec<String> {
        self.lan.iter().map(|address| format!("{}/", network::http_url(address, self.frontend.port()))).collect()
    }
}

/// The state of the server.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebStatus {
    /// Off, and why.
    Off(OffReason),
    /// A thread is binding the ports.
    Starting,
    /// Serving.
    Running(RunningAddresses),
    /// Could not be started; the reason.
    Failed(String),
}

/// What the Browser view sheet saves.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SaveRequest {
    /// The server switch.
    pub enabled: bool,
    /// "Allow other devices on this network": `0.0.0.0`, else `127.0.0.1`.
    pub lan: bool,
    /// The typed page address port.
    pub frontend_port: u16,
    /// The typed API address port.
    pub backend_port: u16,
    /// False when the page port field still shows what it showed when the sheet opened: a
    /// server running on another port (the configured one was taken at start-up) keeps it.
    pub frontend_changed: bool,
    /// The same for the API port field.
    pub backend_changed: bool,
}

/// What `poll` reports to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebEvent {
    /// The pages could not be started at start-up: the reason (a toast).
    NotStarted(String),
    /// The pages started on the network because settings.json says so (a toast).
    OpenToNetwork,
    /// A Save of the sheet ended: `Ok` with the note to show, or the sentence of the refusal.
    Saved(Result<&'static str, String>),
}

/// A port that runs elsewhere than configured: `(configured, running)`.
pub type PortMove = (u16, u16);

/// What a start or restart thread hands back.
struct TaskResult {
    server: Option<Server>,
    config: ServerConfig,
    error: Option<String>,
    save: Option<PendingSave>,
}

/// A Save that waits for its restart.
#[derive(Debug, Clone)]
struct PendingSave {
    plan: WebPlan,
    changes: Vec<(&'static str, serde_json::Value)>,
    note: &'static str,
}

/// The browser pages of the app.
pub struct WebService {
    config: WebConfig,
    plan: WebPlan,
    status: WebStatus,
    bridge: Option<Arc<Bridge>>,
    receiver: Option<mpsc::Receiver<WebCommand>>,
    library: Option<Arc<Library>>,
    server: Option<Server>,
    server_config: Option<ServerConfig>,
    task: Option<mpsc::Receiver<TaskResult>>,
    formats: Arc<FormatCache>,
    revision: u64,
    published_serial: Option<u64>,
    ever_opened: bool,
    library_dir: PathBuf,
    web_cache_dir: PathBuf,
    /// The posters of loose videos (served to browsers, made on the first request).
    poster_cache_dir: PathBuf,
    settings_file: PathBuf,
    machine_names: Vec<String>,
    discovered_lan: Vec<Ipv4Addr>,
    begun: bool,
    /// Asks the UI thread for a frame: a thread that finished must be taken over at once, also
    /// in an idle window that paints nothing by itself.
    waker: Option<Waker>,
}

impl WebService {
    /// The service for `config` with the app's files at `paths`. Nothing starts before `begin`.
    pub fn new(config: WebConfig, paths: &AppPaths) -> WebService {
        let plan = WebPlan::new(&config.settings, &config.overrides);
        let status = if !plan.enabled.value {
            WebStatus::Off(if plan.enabled.source == ValueSource::CommandLine { OffReason::CommandLine } else { OffReason::Settings })
        } else if config.start {
            WebStatus::Starting
        } else {
            WebStatus::Off(OffReason::NotStarted)
        };
        WebService {
            plan,
            status,
            bridge: None,
            receiver: None,
            library: None,
            server: None,
            server_config: None,
            task: None,
            formats: Arc::new(FormatCache::default()),
            revision: 0,
            published_serial: None,
            ever_opened: false,
            library_dir: paths.library_dir.clone(),
            web_cache_dir: paths.web_cache_dir.clone(),
            poster_cache_dir: paths.poster_cache_dir.clone(),
            settings_file: paths.settings_file.clone(),
            machine_names: network::machine_names(&ProcessEnv),
            discovered_lan: Vec::new(),
            begun: false,
            waker: None,
            config,
        }
    }

    /// The values in effect.
    pub fn plan(&self) -> &WebPlan {
        &self.plan
    }

    /// What the Browser view sheet shows when it opens (`SheetValues`).
    pub fn sheet_values(&self) -> SheetValues {
        SheetValues::of(&self.plan, &self.config.settings)
    }

    /// The configuration of the run.
    pub fn config(&self) -> &WebConfig {
        &self.config
    }

    /// The state of the server.
    pub fn status(&self) -> &WebStatus {
        &self.status
    }

    /// The addresses while the server runs.
    pub fn running(&self) -> Option<&RunningAddresses> {
        match &self.status {
            WebStatus::Running(addresses) => Some(addresses),
            _ => None,
        }
    }

    /// The browser imports folder.
    pub fn library_dir(&self) -> &std::path::Path {
        &self.library_dir
    }

    /// True while a thread binds or moves the server.
    pub fn is_busy(&self) -> bool {
        self.task.is_some()
    }

    /// The snapshot published last (`None` before `begin`).
    pub fn snapshot(&self) -> Option<Arc<Snapshot>> {
        self.bridge.as_ref().map(|bridge| bridge.snapshot())
    }

    /// The configuration of a server with these values.
    fn server_config(&self, host: Ipv4Addr, frontend_port: u16, backend_port: u16, search: PortSearch) -> ServerConfig {
        let mut config = ServerConfig::new(host, frontend_port, backend_port, self.config.assets, self.web_cache_dir.clone());
        config.search = search;
        config.poster_cache_dir = Some(self.poster_cache_dir.clone());
        config.allowed_hosts = self.config.settings.allowed_hosts.clone();
        config.machine_names = self.machine_names.clone();
        config.discovered_lan = self.discovered_lan.clone();
        config.timeouts = self.config.timeouts;
        config
    }

    /// Called at every frame: at the first one, the bridge, the browser imports folder and the
    /// first snapshot are made and (when the plan says so) the server is started on the
    /// `web-start` thread. `App::new` starts no thread.
    pub fn begin(&mut self, collection: Option<&Collection>, serial: u64, waker: Waker) {
        if self.begun {
            return;
        }
        self.begun = true;
        let library = Arc::new(Library::new(self.library_dir.clone()));
        self.library = Some(Arc::clone(&library));
        let first = Arc::new(self.make_snapshot(collection));
        self.published_serial = Some(serial);
        self.waker = Some(waker.clone());
        let (bridge, receiver) = Bridge::new(first, Box::new(move || waker.wake()), self.config.reply_timeout);
        self.bridge = Some(bridge);
        self.receiver = Some(receiver);
        if self.plan.enabled.value && self.config.start {
            self.spawn_start(PortSearch::Upward, None);
        }
    }

    /// Starts the server with the plan's values on the `web-start` thread.
    fn spawn_start(&mut self, search: PortSearch, save: Option<PendingSave>) {
        let (Some(bridge), Some(library)) = (self.bridge.clone(), self.library.clone()) else {
            return;
        };
        let config = self.server_config(self.plan.host.value, self.plan.frontend_port.value, self.plan.backend_port.value, search);
        let web_cache_dir = self.web_cache_dir.clone();
        let waker = self.waker.clone();
        let (sender, receiver) = mpsc::channel();
        self.status = WebStatus::Starting;
        let spawned = std::thread::Builder::new().name("web-start".to_owned()).spawn(move || {
            let mut config = config;
            // Resolving the computer's name asks the system; it never waits on the UI thread.
            config.discovered_lan = network::discover_lan_ipv4(&ProcessEnv);
            library.claim_if_used();
            HeicCache::new(web_cache_dir).clean_up(WEB_CACHE_MAX_BYTES);
            let host: Arc<dyn WebHost> = bridge;
            let (server, error) = match Server::start(&config, host, library) {
                Ok(server) => (Some(server), None),
                Err(error) => (None, Some(error.message())),
            };
            let _ = sender.send(TaskResult { server, config, error, save });
            if let Some(waker) = waker {
                waker.wake();
            }
        });
        match spawned {
            Ok(_) => self.task = Some(receiver),
            Err(error) => self.status = WebStatus::Failed(error.to_string()),
        }
    }

    /// Takes over what a start or restart thread finished.
    pub fn poll(&mut self) -> Vec<WebEvent> {
        let Some(receiver) = &self.task else {
            return Vec::new();
        };
        let result = match receiver.try_recv() {
            Ok(result) => result,
            Err(mpsc::TryRecvError::Empty) => return Vec::new(),
            Err(mpsc::TryRecvError::Disconnected) => {
                self.task = None;
                self.status = WebStatus::Failed(messages::SERVER_STOPPING.to_owned());
                return Vec::new();
            }
        };
        self.task = None;
        if !result.config.discovered_lan.is_empty() {
            self.discovered_lan = result.config.discovered_lan.clone();
        }
        let mut events = Vec::new();
        match (result.server, result.error) {
            (Some(server), error) => {
                let addresses = RunningAddresses { frontend: server.frontend_address(), backend: server.backend_address(), lan: server.lan_addresses().to_vec() };
                self.log_addresses(&addresses, &result.config);
                self.status = WebStatus::Running(addresses);
                self.server = Some(server);
                self.server_config = Some(result.config.clone());
                match (error, result.save) {
                    // A Save whose ports could not be bound: the old server (or the old
                    // configuration started again) serves; nothing was saved.
                    (Some(error), Some(_)) => events.push(WebEvent::Saved(Err(error))),
                    (_, Some(save)) => events.push(WebEvent::Saved(self.finish_save(save))),
                    (_, None) => {
                        if self.plan.announces_network() {
                            log::info!("The browser pages are open to other devices on this network (\"web_host\" {} in settings.json).", self.plan.host.value);
                            events.push(WebEvent::OpenToNetwork);
                        }
                    }
                }
            }
            (None, error) => {
                let reason = error.unwrap_or_default();
                match result.save {
                    Some(_) => {
                        log::warn!("The browser pages could not be moved: {reason}");
                        self.status = WebStatus::Failed(reason.clone());
                        events.push(WebEvent::Saved(Err(reason)));
                    }
                    None => {
                        log::warn!("{}", messages::browser_not_started(&reason));
                        self.status = WebStatus::Failed(reason.clone());
                        events.push(WebEvent::NotStarted(reason));
                    }
                }
            }
        }
        events
    }

    /// The log lines of a running server (the end-to-end script reads them).
    fn log_addresses(&self, addresses: &RunningAddresses, config: &ServerConfig) {
        log::info!("Browser pages: {}; API: {}", addresses.page_url().trim_end_matches('/'), addresses.api_url());
        for lan in &addresses.lan {
            log::info!("Browser pages on the network: {}; API: {}", network::http_url(lan, addresses.frontend.port()), network::http_url(lan, addresses.backend.port()));
        }
        if config.frontend_port != 0 && addresses.frontend.port() != config.frontend_port {
            log::info!("Port {} is busy; the browser pages use {}.", config.frontend_port, addresses.frontend.port());
        }
        if config.backend_port != 0 && addresses.backend.port() != config.backend_port {
            log::info!("Port {} is busy; the API uses {}.", config.backend_port, addresses.backend.port());
        }
    }

    /// The running port of each listener (page, API) when it differs from the configured one
    /// (that one was taken at start-up).
    pub fn moved_ports(&self) -> (Option<PortMove>, Option<PortMove>) {
        let (Some(config), Some(running)) = (&self.server_config, self.running()) else {
            return (None, None);
        };
        let moved = |configured: u16, running: u16| (configured != 0 && configured != running).then_some((configured, running));
        (moved(config.frontend_port, running.frontend.port()), moved(config.backend_port, running.backend.port()))
    }

    /// The next change a browser asked for.
    pub fn next_command(&mut self) -> Option<WebCommand> {
        self.receiver.as_ref().and_then(|receiver| receiver.try_recv().ok())
    }

    fn make_snapshot(&mut self, collection: Option<&Collection>) -> Snapshot {
        self.revision += 1;
        self.ever_opened |= collection.is_some();
        let updated_at = self.ever_opened.then(text::now_ms);
        let snapshot = Snapshot::new(self.revision, updated_at, collection, &self.library_dir, Arc::clone(&self.formats));
        if self.config.settings.default_username_set {
            snapshot
        } else {
            // The author of loose pictures is the Windows account name: not for other computers.
            snapshot.with_remote_loose_author(messages::UNKNOWN_USERNAME)
        }
    }

    /// Publishes the snapshot of `collection` when `serial` (`AppState::content_serial`) changed
    /// since the last one, and returns the snapshot in effect.
    pub fn publish(&mut self, collection: Option<&Collection>, serial: u64) -> Option<Arc<Snapshot>> {
        let bridge = self.bridge.clone()?;
        if self.published_serial != Some(serial) {
            let snapshot = Arc::new(self.make_snapshot(collection));
            bridge.publish(Arc::clone(&snapshot));
            self.published_serial = Some(serial);
            return Some(snapshot);
        }
        Some(bridge.snapshot())
    }

    /// The Browser view sheet's Save: the typed values are checked by binding (a typed port is
    /// never replaced by another one), applied, then written to settings.json. The result comes
    /// as `WebEvent::Saved` from `poll` (at once when nothing has to be bound).
    pub fn save(&mut self, request: SaveRequest) -> Option<Result<&'static str, String>> {
        let cli = self.config.overrides;
        let host = if request.lan { Ipv4Addr::UNSPECIFIED } else { Ipv4Addr::LOCALHOST };
        let keep = |current: Sourced<u16>, fixed: bool, typed: u16| if fixed { current } else { Sourced::new(typed, ValueSource::Sheet) };
        let plan = WebPlan {
            enabled: if cli.no_web { self.plan.enabled } else { Sourced::new(request.enabled, ValueSource::Sheet) },
            host: if cli.host.is_some() { self.plan.host } else { Sourced::new(host, ValueSource::Sheet) },
            frontend_port: keep(self.plan.frontend_port, cli.frontend_port.is_some(), request.frontend_port),
            backend_port: keep(self.plan.backend_port, cli.backend_port.is_some(), request.backend_port),
        };
        let changes: Vec<(&'static str, serde_json::Value)> = vec![
            (settings::KEY_WEB_SERVER, serde_json::Value::from(request.enabled)),
            (settings::KEY_FRONTEND_PORT, serde_json::Value::from(request.frontend_port)),
            (settings::KEY_BACKEND_PORT, serde_json::Value::from(request.backend_port)),
            (settings::KEY_WEB_HOST, serde_json::Value::from(host.to_string())),
        ];
        let fixed_by_cli = cli.no_web || cli.host.is_some() || cli.frontend_port.is_some() || cli.backend_port.is_some();
        let note = if fixed_by_cli { messages::BROWSER_SETTINGS_SAVED_FOR_LATER } else { messages::BROWSER_SETTINGS_SAVED };
        let save = PendingSave { plan, changes, note };

        if !plan.enabled.value {
            // Off: the server stops (on a thread: stopping waits for the listeners).
            if let Some(server) = self.server.take() {
                let library = self.library.clone();
                let _ = std::thread::Builder::new().name("web-stop".to_owned()).spawn(move || {
                    if let Some(library) = library {
                        library.cancel_uploads();
                    }
                    server.stop(Duration::from_millis(WEB_STOP_WAIT_MS));
                });
            }
            self.server_config = None;
            self.status = WebStatus::Off(if cli.no_web { OffReason::CommandLine } else { OffReason::Sheet });
            return Some(self.finish_save(save));
        }

        // The ports of the new server: a value the command line fixed, and a field the user did
        // not touch, keep the port the server runs on now.
        let running = self.running().cloned();
        let port = |sourced: Sourced<u16>, changed: bool, running: Option<u16>| match running {
            Some(running) if sourced.source == ValueSource::CommandLine || !changed => running,
            _ => sourced.value,
        };
        let frontend = port(plan.frontend_port, request.frontend_changed, running.as_ref().map(|running| running.frontend.port()));
        let backend = port(plan.backend_port, request.backend_changed, running.as_ref().map(|running| running.backend.port()));
        if frontend == backend {
            return Some(Err(messages::PORTS_MUST_DIFFER.to_owned()));
        }
        let unchanged = running.as_ref().is_some_and(|running| running.frontend.port() == frontend && running.backend.port() == backend)
            && self.server.as_ref().is_some_and(|server| server.host() == plan.host.value);
        if unchanged {
            return Some(self.finish_save(save));
        }
        let config = self.server_config(plan.host.value, frontend, backend, PortSearch::Exact);
        let previous = self.server_config.clone();
        let old = self.server.take();
        let (Some(bridge), Some(library)) = (self.bridge.clone(), self.library.clone()) else {
            return Some(Err(messages::browser_not_started(messages::SERVER_STOPPING)));
        };
        let waker = self.waker.clone();
        let (sender, receiver) = mpsc::channel();
        self.status = WebStatus::Starting;
        let spawned = std::thread::Builder::new().name("web-restart".to_owned()).spawn(move || {
            let mut config = config;
            if config.discovered_lan.is_empty() && !config.host.is_loopback() {
                config.discovered_lan = network::discover_lan_ipv4(&ProcessEnv);
            }
            let host: Arc<dyn WebHost> = bridge;
            let outcome = web::restart(old, &config, previous.as_ref(), host, library);
            let result = match outcome {
                Ok(server) => TaskResult { server: Some(server), config, error: None, save: Some(save) },
                Err(failure) => {
                    let failure = *failure;
                    let config = match (&failure.server, previous) {
                        (Some(_), Some(previous)) => previous,
                        _ => config,
                    };
                    TaskResult { server: failure.server, config, error: Some(failure.error.message()), save: Some(save) }
                }
            };
            let _ = sender.send(result);
            if let Some(waker) = waker {
                waker.wake();
            }
        });
        match spawned {
            Ok(_) => {
                self.task = Some(receiver);
                None
            }
            Err(error) => Some(Err(error.to_string())),
        }
    }

    /// The end of a Save whose values apply: the plan takes them, settings.json gets them.
    fn finish_save(&mut self, save: PendingSave) -> Result<&'static str, String> {
        self.plan = save.plan;
        let changes: Vec<(&str, serde_json::Value)> = save.changes.iter().map(|(key, value)| (*key, value.clone())).collect();
        match settings::update_settings_file(&self.settings_file, &changes) {
            Ok(()) => {
                for (key, value) in &save.changes {
                    match *key {
                        settings::KEY_WEB_SERVER => self.config.settings.web_server = value.as_bool(),
                        settings::KEY_FRONTEND_PORT => self.config.settings.frontend_port = value.as_u64().and_then(|port| u16::try_from(port).ok()),
                        settings::KEY_BACKEND_PORT => self.config.settings.backend_port = value.as_u64().and_then(|port| u16::try_from(port).ok()),
                        settings::KEY_WEB_HOST => self.config.settings.web_host = value.as_str().and_then(|host| host.parse().ok()),
                        _ => {}
                    }
                }
                log::info!("The browser pages' settings were saved in {}.", self.settings_file.display());
                Ok(save.note)
            }
            Err(reason) => {
                log::warn!("settings.json could not be saved: {reason}");
                Err(messages::settings_not_saved(&reason))
            }
        }
    }

    /// Stops the server for the exit (web brief 4.3, at most `WEB_STOP_WAIT_MS`): no more
    /// changes are taken (a waiting HTTP thread answers `APP_CLOSING`), the listeners close, the
    /// browser imports folder is released.
    pub fn shut_down(&mut self) {
        if let Some(bridge) = &self.bridge {
            bridge.close();
        }
        self.receiver = None;
        if let Some(server) = self.server.take() {
            if let Some(library) = &self.library {
                library.cancel_uploads();
            }
            if !server.stop(Duration::from_millis(WEB_STOP_WAIT_MS)) {
                log::warn!("The browser pages did not stop within {WEB_STOP_WAIT_MS} ms.");
            }
        }
        if let Some(library) = &self.library {
            library.close();
        }
        self.status = WebStatus::Off(OffReason::NotStarted);
    }

    /// This computer's names, as the guard of the server knows them.
    pub fn machine_names(&self) -> &[String] {
        &self.machine_names
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_line_wins_over_settings_and_defaults() {
        let settings = WebSettings { frontend_port: Some(4000), web_host: Some(Ipv4Addr::UNSPECIFIED), web_server: Some(true), ..WebSettings::default() };
        let plan = WebPlan::new(&settings, &WebOverrides { backend_port: Some(9000), ..WebOverrides::default() });
        assert_eq!(plan.frontend_port, Sourced::new(4000, ValueSource::Settings));
        assert_eq!(plan.backend_port, Sourced::new(9000, ValueSource::CommandLine));
        assert_eq!(plan.host, Sourced::new(Ipv4Addr::UNSPECIFIED, ValueSource::Settings));
        assert!(plan.lan());
        let defaults = WebPlan::new(&WebSettings::default(), &WebOverrides::default());
        assert_eq!((defaults.frontend_port.value, defaults.backend_port.value, defaults.host.value, defaults.enabled.value), (3127, 8317, Ipv4Addr::LOCALHOST, true));
        assert_eq!(defaults.enabled.source, ValueSource::Default);
        let off = WebPlan::new(&settings, &WebOverrides { no_web: true, ..WebOverrides::default() });
        assert_eq!(off.enabled, Sourced::new(false, ValueSource::CommandLine));
        let off_in_file = WebPlan::new(&WebSettings { web_server: Some(false), ..WebSettings::default() }, &WebOverrides::default());
        assert_eq!(off_in_file.enabled, Sourced::new(false, ValueSource::Settings));
    }

    #[test]
    fn the_sheet_shows_the_saved_value_of_what_the_command_line_fixed() {
        let settings = WebSettings { frontend_port: Some(3301), backend_port: Some(3302), web_server: Some(true), ..WebSettings::default() };
        let cli = WebOverrides { frontend_port: Some(3311), backend_port: Some(3312), host: Some(Ipv4Addr::UNSPECIFIED), no_web: false };
        let plan = WebPlan::new(&settings, &cli);
        assert_eq!((plan.frontend_port.value, plan.backend_port.value), (3311, 3312), "the run uses the command line");
        assert_eq!(SheetValues::of(&plan, &settings), SheetValues { enabled: true, lan: false, frontend_port: 3301, backend_port: 3302 }, "the sheet shows what Save keeps");
        // Without a value in settings.json: the default, never the run's value.
        let plan = WebPlan::new(&WebSettings::default(), &WebOverrides { backend_port: Some(3312), no_web: true, ..WebOverrides::default() });
        assert_eq!(SheetValues::of(&plan, &WebSettings::default()), SheetValues { enabled: true, lan: false, frontend_port: 3127, backend_port: 8317 });
        // Nothing on the command line: the values in effect.
        let lan = WebSettings { web_host: Some(Ipv4Addr::UNSPECIFIED), frontend_port: Some(4000), ..WebSettings::default() };
        let plan = WebPlan::new(&lan, &WebOverrides::default());
        assert_eq!(SheetValues::of(&plan, &lan), SheetValues { enabled: true, lan: true, frontend_port: 4000, backend_port: 8317 });
    }

    #[test]
    fn a_start_on_the_network_says_so_unless_the_command_line_asked_for_it() {
        let lan = WebSettings { web_host: Some(Ipv4Addr::UNSPECIFIED), ..WebSettings::default() };
        assert!(WebPlan::new(&lan, &WebOverrides::default()).announces_network(), "web_host 0.0.0.0, written by Save");
        assert!(!WebPlan::new(&lan, &WebOverrides { host: Some(Ipv4Addr::UNSPECIFIED), ..WebOverrides::default() }).announces_network(), "--host for this run");
        assert!(!WebPlan::new(&lan, &WebOverrides { no_web: true, ..WebOverrides::default() }).announces_network(), "no pages at all");
        assert!(!WebPlan::new(&WebSettings::default(), &WebOverrides::default()).announces_network(), "127.0.0.1");
    }

    #[test]
    fn the_addresses_a_browser_opens_use_ipv4() {
        let addresses = RunningAddresses { frontend: "0.0.0.0:3127".parse().expect("address"), backend: "0.0.0.0:8317".parse().expect("address"), lan: vec![Ipv4Addr::new(192, 168, 1, 20)] };
        assert_eq!(addresses.page_url(), "http://127.0.0.1:3127/");
        assert_eq!(addresses.api_url(), "http://127.0.0.1:8317");
        assert_eq!(addresses.lan_page_urls(), ["http://192.168.1.20:3127/"]);
        let one = RunningAddresses { frontend: "192.168.1.20:3127".parse().expect("address"), backend: "192.168.1.20:8317".parse().expect("address"), lan: Vec::new() };
        assert_eq!(one.page_url(), "http://192.168.1.20:3127/");
    }
}
