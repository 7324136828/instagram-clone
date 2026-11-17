//! Per-user settings and the locations of the files the app keeps.
//!
//! settings.json belongs to the user. It follows the contract of the reference web project
//! (backend/settings.py; spec-conventions 2), with this app's own name and keys:
//!
//! ```text
//! %APPDATA%\instagram-windows-app\settings.json      user-owned settings
//! %APPDATA%\instagram-windows-app\data\              default data folder (`data_directory`)
//!     descriptions.json   state.json   window.ron   app.log
//! ```
//!
//! * It is created on first run when absent, with exactly `{"data_directory": "<default>"}`,
//!   published so that a file another start created in the meantime is never overwritten.
//! * A valid file is NEVER rewritten: formatting, key order, a BOM and unknown keys stay exactly
//!   as the user wrote them. An invalid file is never modified either; the app stops with an
//!   error that names the path.
//! * `data_directory` (string): `%VAR%` and a leading `~` are expanded, a relative path resolves
//!   from the folder of settings.json. Default: the `data` folder beside settings.json, on
//!   purpose not `%TEMP%`: typed descriptions must survive a disk clean-up.
//! * `default_username` (string, optional): the author shown for loose pictures. Default: the
//!   Windows account name reduced to `[A-Za-z0-9._]{1,30}`, else "unknown".
//!
//! Large files the app makes itself live in the local (not roaming) profile, which is neither
//! configurable nor backed up with the roaming one:
//!
//! ```text
//! %LOCALAPPDATA%\instagram-windows-app\archives\     the archive cache (`archive::cache`)
//! %LOCALAPPDATA%\instagram-windows-app\library\      the browser imports folder (web brief 4.10)
//! ```
//!
//! The web app's library (the clone's `instagram_cache` folder) is only ever read: its location
//! follows the clone's own rules (`web_app_library_dir`).
//!
//! Nothing here reads the real environment directly: every function takes an `EnvLookup`, so
//! tests never touch the user's profile.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value};

use crate::manifest;
use crate::messages;
use crate::store::{self, WriteMode};

/// The folder name under `%APPDATA%`. It is this app's own: sharing the clone's name would make
/// the two apps share one settings.json.
pub const APP_NAME: &str = "instagram-windows-app";

/// The user's settings file, in the folder `APP_NAME` under `%APPDATA%`.
pub const SETTINGS_FILE_NAME: &str = "settings.json";
/// The default data folder, beside settings.json.
pub const DATA_FOLDER_NAME: &str = "data";
/// The descriptions the user typed (see `store`), in the data folder.
pub const DESCRIPTIONS_FILE_NAME: &str = "descriptions.json";
/// The interface state (see `store::UiState`), in the data folder.
pub const STATE_FILE_NAME: &str = "state.json";
/// eframe's window geometry (its persistence file).
pub const WINDOW_FILE_NAME: &str = "window.ron";
/// The log of the app, in the data folder.
pub const LOG_FILE_NAME: &str = "app.log";

/// The archive cache, in the local folder of the app.
pub const ARCHIVE_CACHE_FOLDER_NAME: &str = "archives";
/// The browser imports folder, in the local folder of the app.
pub const LIBRARY_FOLDER_NAME: &str = "library";
/// The local folder of the app when `LOCALAPPDATA` is not set (a test environment): a folder of
/// this name in the data folder.
pub const LOCAL_FALLBACK_FOLDER_NAME: &str = "local";

/// The name the reference web project's backend uses under `%APPDATA%` (backend/settings.py).
pub const WEB_APP_NAME: &str = "instagram-clone";
/// The variable that names the web app's library folder (settings.py `CACHE_DIRECTORY_ENVIRONMENT`).
pub const WEB_APP_LIBRARY_VARIABLE: &str = "INSTAGRAM_CACHE_DIR";
/// The key of the web app's settings.json that names its library folder.
pub const WEB_APP_LIBRARY_SETTING: &str = "instagram_cache_directory";
/// The web app's library folder under `%TEMP%` by default.
pub const WEB_APP_LIBRARY_FOLDER_NAME: &str = "instagram_cache";

/// The JPEG copies of HEIC pictures for browsers, in the local folder of the app.
pub const WEB_CACHE_FOLDER_NAME: &str = "web-cache";
/// The folder in `local_dir` that holds the posters of loose videos (video brief 5.2).
pub const POSTER_CACHE_FOLDER_NAME: &str = "posters";

/// The key of the data folder in settings.json.
pub const KEY_DATA_DIRECTORY: &str = "data_directory";
/// The key of the author of loose pictures in settings.json.
pub const KEY_DEFAULT_USERNAME: &str = "default_username";
/// The key that turns the browser pages on or off (web brief 4.11).
pub const KEY_WEB_SERVER: &str = "web_server";
/// The key of the page address port.
pub const KEY_FRONTEND_PORT: &str = "frontend_port";
/// The key of the API address port.
pub const KEY_BACKEND_PORT: &str = "backend_port";
/// The key of the address the browser pages listen on.
pub const KEY_WEB_HOST: &str = "web_host";
/// The key of the extra host names the browser pages answer to.
pub const KEY_ALLOWED_HOSTS: &str = "allowed_hosts";
/// The key of the browser imports folder.
pub const KEY_LIBRARY_DIRECTORY: &str = "library_directory";

/// Reading settings.json is retried this often while Windows answers "access denied" (the file
/// is being moved into place, or a virus scanner holds it): settings.py `_READ_ATTEMPTS`.
pub const READ_ATTEMPTS: u32 = 20;
/// The pause between two such attempts, in milliseconds (settings.py `_READ_RETRY_SECONDS`).
pub const READ_RETRY_MS: u64 = 25;

/// Longest default username taken from the Windows account name.
pub const MAX_ACCOUNT_NAME_CHARS: usize = 30;

/// Where an environment variable comes from. The app passes `ProcessEnv`; tests pass a map.
pub trait EnvLookup {
    /// The value of the variable `name`, or `None` when it is unset or not Unicode. Windows
    /// variable names are compared without regard to letter case.
    fn var(&self, name: &str) -> Option<String>;
}

/// The environment of this process.
#[derive(Clone, Copy, Debug, Default)]
pub struct ProcessEnv;

impl EnvLookup for ProcessEnv {
    fn var(&self, name: &str) -> Option<String> {
        std::env::var(name).ok()
    }
}

/// A fixed environment for tests: the keys are variable names, compared without regard to
/// letter case, as Windows does.
impl EnvLookup for HashMap<String, String> {
    fn var(&self, name: &str) -> Option<String> {
        self.iter().find(|(key, _)| key.eq_ignore_ascii_case(name)).map(|(_, value)| value.clone())
    }
}

/// Raised when the user's settings cannot be read or created. `message` is one line from
/// `messages::settings_*` that names the full path of the file and has no trailing period. The
/// app shows it in a message box and on stderr, and exits with code 1.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SettingsError {
    /// The line to show: it names the path and has no trailing period.
    pub message: String,
}

impl SettingsError {
    /// An error with its line.
    pub fn new(message: impl Into<String>) -> SettingsError {
        SettingsError { message: message.into() }
    }
}

impl std::fmt::Display for SettingsError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SettingsError {}

/// Where the app's files are. All paths are absolute. Folders are not created here; the code
/// that writes a file creates its folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AppPaths {
    /// `%APPDATA%\instagram-windows-app\settings.json`.
    pub settings_file: PathBuf,
    /// The data folder: `data_directory` of the settings, expanded and absolute.
    pub data_dir: PathBuf,
    /// `{data_dir}\descriptions.json` (see `store`).
    pub descriptions_file: PathBuf,
    /// `{data_dir}\state.json` (see `store`).
    pub state_file: PathBuf,
    /// `{data_dir}\window.ron`: eframe's `NativeOptions::persistence_path`.
    pub window_file: PathBuf,
    /// `{data_dir}\app.log`.
    pub log_file: PathBuf,
    /// `%LOCALAPPDATA%\instagram-windows-app` (`local_dir`): where the large files the app makes
    /// itself are kept.
    pub local_dir: PathBuf,
    /// `{local_dir}\archives`: the archive cache.
    pub archive_cache_dir: PathBuf,
    /// The browser imports folder: `library_directory` of the settings, else `{local_dir}\library`.
    pub library_dir: PathBuf,
    /// `{local_dir}\web-cache`: the JPEG copies of HEIC pictures for browsers.
    pub web_cache_dir: PathBuf,
    /// `{local_dir}\posters`: the first frames of loose videos (`poster::PosterCache`).
    pub poster_cache_dir: PathBuf,
    /// The web app's library folder, when it can be told (`web_app_library_dir`); only read.
    pub web_app_library_dir: Option<PathBuf>,
}

impl AppPaths {
    /// The file locations for a settings file and a data folder; the local folder is
    /// `{data_dir}\local` until `with_local_dir` says otherwise, and the web app's library is
    /// not known.
    pub fn new(settings_file: PathBuf, data_dir: PathBuf) -> AppPaths {
        let local_dir = data_dir.join(LOCAL_FALLBACK_FOLDER_NAME);
        AppPaths {
            descriptions_file: data_dir.join(DESCRIPTIONS_FILE_NAME),
            state_file: data_dir.join(STATE_FILE_NAME),
            window_file: data_dir.join(WINDOW_FILE_NAME),
            log_file: data_dir.join(LOG_FILE_NAME),
            archive_cache_dir: local_dir.join(ARCHIVE_CACHE_FOLDER_NAME),
            library_dir: local_dir.join(LIBRARY_FOLDER_NAME),
            web_cache_dir: local_dir.join(WEB_CACHE_FOLDER_NAME),
            poster_cache_dir: local_dir.join(POSTER_CACHE_FOLDER_NAME),
            local_dir,
            web_app_library_dir: None,
            settings_file,
            data_dir,
        }
    }

    /// The same locations with the local folder `local_dir` (and the folders in it).
    pub fn with_local_dir(mut self, local_dir: PathBuf) -> AppPaths {
        self.archive_cache_dir = local_dir.join(ARCHIVE_CACHE_FOLDER_NAME);
        self.library_dir = local_dir.join(LIBRARY_FOLDER_NAME);
        self.web_cache_dir = local_dir.join(WEB_CACHE_FOLDER_NAME);
        self.poster_cache_dir = local_dir.join(POSTER_CACHE_FOLDER_NAME);
        self.local_dir = local_dir;
        self
    }
}

/// The settings of the browser pages as settings.json gives them; `None` for a key the file does
/// not have (the default applies, or the command line).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WebSettings {
    /// `web_server`: the browser pages on (default) or off.
    pub web_server: Option<bool>,
    /// `frontend_port`: the page address port (default 3127).
    pub frontend_port: Option<u16>,
    /// `backend_port`: the API address port (default 8317).
    pub backend_port: Option<u16>,
    /// `web_host`: the address to listen on (default 127.0.0.1).
    pub web_host: Option<std::net::Ipv4Addr>,
    /// `allowed_hosts`: extra host names the pages answer to ("*" for any).
    pub allowed_hosts: Vec<String>,
    /// True when `library_directory` names the browser imports folder.
    pub library_directory_set: bool,
    /// True when `default_username` names the author of loose pictures; without it the author is
    /// the Windows account name, which other computers are not told.
    pub default_username_set: bool,
}

impl WebSettings {
    /// The page address port in effect without a command-line port.
    pub fn frontend_port_or_default(&self) -> u16 {
        self.frontend_port.unwrap_or(crate::web::DEFAULT_FRONTEND_PORT)
    }

    /// The API address port in effect without a command-line port.
    pub fn backend_port_or_default(&self) -> u16 {
        self.backend_port.unwrap_or(crate::web::DEFAULT_BACKEND_PORT)
    }

    /// The address in effect without `--host`.
    pub fn web_host_or_default(&self) -> std::net::Ipv4Addr {
        self.web_host.unwrap_or(std::net::Ipv4Addr::LOCALHOST)
    }

    /// Whether the pages are on without `--no-web`.
    pub fn web_server_or_default(&self) -> bool {
        self.web_server.unwrap_or(true)
    }
}

/// The local folder of the app: `{LOCALAPPDATA}\instagram-windows-app`; when `LOCALAPPDATA` is
/// unset or empty (a test environment), `{data_dir}\local`.
pub fn local_dir(env: &dyn EnvLookup, data_dir: &Path) -> PathBuf {
    match set_variable(env, "LOCALAPPDATA") {
        Some(local) => absolute_path(Path::new(&local)).join(APP_NAME),
        None => data_dir.join(LOCAL_FALLBACK_FOLDER_NAME),
    }
}

/// Where the reference web project keeps its library of imports (backend/settings.py
/// `configured_cache_directory`; web brief 3.7), for "Open the web app's library". The app only
/// reads that folder and never creates it.
///
/// 1. `INSTAGRAM_CACHE_DIR`, when it is set to an ABSOLUTE path after `%VAR%` and `~` are
///    expanded (a relative value counts from the web project's folder, which the app cannot
///    know: it is ignored);
/// 2. else `instagram_cache_directory` of `%APPDATA%\instagram-clone\settings.json`
///    (BOM, `%VAR%`, `~`, relative to that folder, as this app reads its own settings; a file or
///    a key with any problem is logged and treated as absent);
/// 3. else `%TEMP%\instagram_cache` (`TMP` when `TEMP` is not set).
pub fn web_app_library_dir(env: &dyn EnvLookup) -> Option<PathBuf> {
    if let Some(configured) = set_variable(env, WEB_APP_LIBRARY_VARIABLE).filter(|value| !is_blank(value)) {
        let expanded = PathBuf::from(expand_home(expand_variables(configured.trim(), env), env));
        return expanded.is_absolute().then(|| collapse_dots(&expanded));
    }
    if let Some(configured) = web_app_library_setting(env) {
        return Some(configured);
    }
    let temp = set_variable(env, "TEMP").or_else(|| set_variable(env, "TMP"))?;
    Some(absolute_path(Path::new(&temp)).join(WEB_APP_LIBRARY_FOLDER_NAME))
}

/// Step 2 of `web_app_library_dir`: the folder the web app's settings.json names, if it names a
/// usable one.
fn web_app_library_setting(env: &dyn EnvLookup) -> Option<PathBuf> {
    let roaming = match set_variable(env, "APPDATA") {
        Some(roaming) => PathBuf::from(roaming),
        None => Path::new(&set_variable(env, "USERPROFILE")?).join("AppData").join("Roaming"),
    };
    let clone_settings_file = absolute_path(&roaming).join(WEB_APP_NAME).join(SETTINGS_FILE_NAME);
    let shown = clone_settings_file.display();
    let bytes = match fs::read(&clone_settings_file) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            log::warn!("The web app's settings at {shown} cannot be read: {error}");
            return None;
        }
    };
    let text = String::from_utf8_lossy(&bytes);
    let Ok(Value::Object(clone_settings)) = serde_json::from_str::<Value>(text.strip_prefix('\u{feff}').unwrap_or(&text)) else {
        log::warn!("The web app's settings at {shown} are not a JSON object; its default library folder is assumed.");
        return None;
    };
    match clone_settings.get(WEB_APP_LIBRARY_SETTING) {
        None | Some(Value::Null) => None,
        Some(Value::String(configured)) if !is_blank(configured) && !configured.contains('\0') => {
            let folder = clone_settings_file.parent().unwrap_or_else(|| Path::new(""));
            match expand_path(configured.trim(), env, folder) {
                Ok(path) => Some(path),
                Err(reason) => {
                    log::warn!("{WEB_APP_LIBRARY_SETTING} in {shown} is unusable ({reason}); its default library folder is assumed.");
                    None
                }
            }
        }
        Some(_) => {
            log::warn!("{WEB_APP_LIBRARY_SETTING} in {shown} is not a folder path; its default library folder is assumed.");
            None
        }
    }
}

/// The settings in effect for this run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Settings {
    /// Where the app's files are, with `data_directory` of the settings already expanded.
    pub paths: AppPaths,
    /// The author of loose pictures: `default_username` of the settings, else
    /// `account_username(env)`.
    pub default_username: String,
    /// The browser pages' keys of settings.json.
    pub web: WebSettings,
}

/// The value of a variable that is set to something: an empty value counts as unset
/// (settings.py: `os.environ.get("APPDATA") or ...`).
fn set_variable(env: &dyn EnvLookup, name: &str) -> Option<String> {
    env.var(name).filter(|value| !value.is_empty())
}

/// The path of settings.json: `{APPDATA}\instagram-windows-app\settings.json`; when `APPDATA`
/// is unset or empty, `{USERPROFILE}\AppData\Roaming\instagram-windows-app\settings.json`
/// (settings.py:26-31). `Err(messages::SETTINGS_FOLDER_UNKNOWN)` when neither is set. The result
/// is absolute (`std::path::absolute`); the file system is not touched.
pub fn settings_path(env: &dyn EnvLookup) -> Result<PathBuf, SettingsError> {
    let root = match set_variable(env, "APPDATA") {
        Some(roaming) => PathBuf::from(roaming),
        None => match set_variable(env, "USERPROFILE") {
            Some(home) => Path::new(&home).join("AppData").join("Roaming"),
            None => return Err(SettingsError::new(messages::SETTINGS_FOLDER_UNKNOWN)),
        },
    };
    Ok(absolute_path(&root).join(APP_NAME).join(SETTINGS_FILE_NAME))
}

/// The default data folder for a settings file: the folder `data` beside it.
pub fn default_data_directory(settings_file: &Path) -> PathBuf {
    settings_file.parent().unwrap_or_else(|| Path::new("")).join(DATA_FOLDER_NAME)
}

/// `load_or_create_at(&settings_path(env)?, env)`: what the app calls at start.
pub fn load_or_create(env: &dyn EnvLookup) -> Result<Settings, SettingsError> {
    load_or_create_at(&settings_path(env)?, env)
}

/// Create the defaults once; preserve settings the user has already written
/// (settings.py `load_settings`; spec-conventions 2.3-2.9).
///
/// 1. Create the folder of `settings_file` when it is missing.
/// 2. When the file is absent, create it with exactly `{"data_directory": "<default>"}` (the
///    absolute default folder; two-space indentation, a trailing newline, no BOM) through
///    `store::atomic_write` in `CreateNew` mode, so a file another start created in the meantime
///    is kept. No temporary file is left behind.
/// 3. Read the file: UTF-8 with an optional BOM; `PermissionDenied` is retried `READ_ATTEMPTS`
///    times, `READ_RETRY_MS` apart. Any failure of steps 1-3, including invalid UTF-8 and
///    invalid JSON -> `messages::settings_unreadable(path, error)`.
/// 4. The top-level value must be a JSON object -> else `messages::settings_not_an_object`.
/// 5. `data_directory`: absent -> the default, in memory only (the file is NOT rewritten);
///    present -> must be a string that is not blank and holds no NUL, else
///    `messages::settings_bad_data_directory` (so `null`, `42`, `false`, `""` and `"   "` fail);
///    then `expand_path`; a failure there -> `messages::settings_data_directory_error`.
/// 6. `default_username`: absent or `null` -> `account_username(env)`; present -> must be a
///    string that `manifest::clean_username` accepts, else
///    `messages::settings_bad_default_username`.
/// 7. Unknown keys are ignored and stay in the file. The file is never written in steps 3-7.
///
/// Folders named by the settings are not created here.
pub fn load_or_create_at(settings_file: &Path, env: &dyn EnvLookup) -> Result<Settings, SettingsError> {
    let settings_file = absolute_path(settings_file);
    let shown = settings_file.display().to_string();
    let folder = settings_file.parent().unwrap_or_else(|| Path::new("")).to_path_buf();
    let default_directory = default_data_directory(&settings_file);

    let text = read_or_create(&settings_file, &folder, &default_directory)
        .map_err(|reason| SettingsError::new(messages::settings_unreadable(&shown, &reason)))?;
    let document: Value = serde_json::from_str(&text)
        .map_err(|error| SettingsError::new(messages::settings_unreadable(&shown, &error.to_string())))?;
    let Value::Object(document) = document else {
        return Err(SettingsError::new(messages::settings_not_an_object(&shown)));
    };

    let data_dir = match document.get(KEY_DATA_DIRECTORY) {
        None => default_directory,
        Some(Value::String(configured)) if !is_blank(configured) && !configured.contains('\0') => {
            expand_path(configured, env, &folder).map_err(|reason| {
                SettingsError::new(messages::settings_data_directory_error(&shown, &reason))
            })?
        }
        Some(_) => return Err(SettingsError::new(messages::settings_bad_data_directory(&shown))),
    };
    let configured_username = document.get(KEY_DEFAULT_USERNAME);
    let default_username = match configured_username {
        None | Some(Value::Null) => account_username(env),
        Some(name @ Value::String(_)) => manifest::clean_username(name)
            .ok_or_else(|| SettingsError::new(messages::settings_bad_default_username(&shown)))?,
        Some(_) => return Err(SettingsError::new(messages::settings_bad_default_username(&shown))),
    };

    let mut web = WebSettings {
        web_server: match document.get(KEY_WEB_SERVER) {
            None | Some(Value::Null) => None,
            Some(Value::Bool(on)) => Some(*on),
            Some(_) => return Err(SettingsError::new(messages::settings_bad_web_server(&shown))),
        },
        frontend_port: read_port(document.get(KEY_FRONTEND_PORT), KEY_FRONTEND_PORT, &shown)?,
        backend_port: read_port(document.get(KEY_BACKEND_PORT), KEY_BACKEND_PORT, &shown)?,
        web_host: match document.get(KEY_WEB_HOST) {
            None | Some(Value::Null) => None,
            Some(Value::String(address)) => Some(address.trim().parse::<std::net::Ipv4Addr>().map_err(|_| SettingsError::new(messages::settings_bad_web_host(&shown)))?),
            Some(_) => return Err(SettingsError::new(messages::settings_bad_web_host(&shown))),
        },
        allowed_hosts: host_list(document.get(KEY_ALLOWED_HOSTS), &shown)?,
        library_directory_set: false,
        default_username_set: matches!(configured_username, Some(Value::String(_))),
    };
    if web.frontend_port_or_default() == web.backend_port_or_default() {
        return Err(SettingsError::new(messages::settings_same_ports(&shown)));
    }
    let library_dir = match document.get(KEY_LIBRARY_DIRECTORY) {
        None | Some(Value::Null) => None,
        Some(Value::String(configured)) if !is_blank(configured) && !configured.contains('\0') => {
            web.library_directory_set = true;
            Some(expand_path(configured, env, &folder).map_err(|reason| SettingsError::new(messages::settings_library_directory_error(&shown, &reason)))?)
        }
        Some(_) => return Err(SettingsError::new(messages::settings_bad_library_directory(&shown))),
    };

    let local = local_dir(env, &data_dir);
    let mut paths = AppPaths::new(settings_file, data_dir).with_local_dir(local);
    if let Some(library_dir) = library_dir {
        paths.library_dir = library_dir;
    }
    paths.web_app_library_dir = web_app_library_dir(env);
    Ok(Settings { paths, default_username, web })
}

/// A port of settings.json: absent or `null` -> `None`; a whole number from 1 to 65535; anything
/// else is a settings error that names the key (settings.py `_validate_ports`).
fn read_port(value: Option<&Value>, key: &str, shown: &str) -> Result<Option<u16>, SettingsError> {
    match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Number(number)) => match number.as_u64() {
            Some(port @ 1..=65535) => Ok(Some(port as u16)),
            _ => Err(SettingsError::new(messages::settings_bad_port(shown, key))),
        },
        Some(_) => Err(SettingsError::new(messages::settings_bad_port(shown, key))),
    }
}

/// settings.py `_host_list`: a list of strings, or one comma-separated string; blank names are
/// dropped.
fn host_list(value: Option<&Value>, shown: &str) -> Result<Vec<String>, SettingsError> {
    let items: Vec<&Value> = match value {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::String(text)) => return Ok(text.split(',').map(str::trim).filter(|name| !name.is_empty()).map(str::to_owned).collect()),
        Some(Value::Array(items)) => items.iter().collect(),
        Some(_) => return Err(SettingsError::new(messages::settings_hosts_not_a_list(shown))),
    };
    let mut names = Vec::new();
    for item in items {
        let Value::String(name) = item else {
            return Err(SettingsError::new(messages::settings_host_not_a_string(shown)));
        };
        if !name.trim().is_empty() {
            names.push(name.trim().to_owned());
        }
    }
    Ok(names)
}

/// Writes `changes` into settings.json (the Browser view sheet's Save): read, change only these
/// keys (a key that is there keeps its place, a new one goes to the end) and write back through
/// `store::atomic_write`. The file is the user's, so its text stays as it was around the changed
/// values: only those values are replaced, and a new key is added after the last one in the
/// layout of that one (`patch_object_text`); a file that cannot be patched so (an empty object)
/// is written anew as two-space JSON. Every other key, its value and the key order stay; a byte
/// order mark stays. When the file is not a JSON object or cannot be read or written, nothing on
/// disk changes and `Err` holds the reason (`messages::settings_not_saved` wraps it). A missing
/// file is created with the changes only.
pub fn update_settings_file(settings_file: &Path, changes: &[(&str, Value)]) -> Result<(), String> {
    let (mut document, original, bom) = match read_settings_bytes(settings_file) {
        Ok(bytes) => {
            let text = String::from_utf8(bytes).map_err(|error| messages::settings_file_not_utf8(&error.utf8_error().to_string()))?;
            let (text, bom) = match text.strip_prefix('\u{feff}') {
                Some(rest) => (rest.to_owned(), true),
                None => (text, false),
            };
            match serde_json::from_str::<Value>(&text) {
                Ok(Value::Object(document)) => (document, Some(text), bom),
                Ok(_) => return Err(messages::SETTINGS_FILE_NOT_AN_OBJECT.to_owned()),
                Err(error) => return Err(messages::settings_file_not_json(&error.to_string())),
            }
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => (Map::new(), None, false),
        Err(error) => return Err(error.to_string()),
    };
    for (key, value) in changes {
        // An existing key keeps its position (serde_json's map keeps insertion order).
        document.insert((*key).to_owned(), value.clone());
    }
    let patched = original.as_deref().and_then(|text| patch_object_text(text, changes)).filter(|patched| {
        // Only a text that reads back as exactly the changed document, keys in order, is used.
        matches!(serde_json::from_str::<Value>(patched), Ok(Value::Object(read)) if read == document && read.keys().eq(document.keys()))
    });
    let mut text = patched.unwrap_or_else(|| format!("{:#}\n", Value::Object(document)));
    if bom {
        text.insert(0, '\u{feff}');
    }
    store::atomic_write(settings_file, text.as_bytes(), WriteMode::Replace).map(|_| ()).map_err(|error| error.to_string())
}

/// A member of the top-level object of a JSON text: its key, where the key (with its quotes)
/// and the value are, and where the text before the key starts (after `{` or `,`).
struct Member {
    key: String,
    lead_start: usize,
    key_range: std::ops::Range<usize>,
    value_range: std::ops::Range<usize>,
}

/// The end of the JSON string that starts with the quote at `start` (after its closing quote).
fn string_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => return Some(index + 1),
            _ => index += 1,
        }
    }
    None
}

/// The end of the JSON value that starts at `start`.
fn value_end(bytes: &[u8], start: usize) -> Option<usize> {
    match bytes.get(start)? {
        b'"' => string_end(bytes, start),
        b'{' | b'[' => {
            let mut depth = 0usize;
            let mut index = start;
            while index < bytes.len() {
                match bytes[index] {
                    b'"' => {
                        index = string_end(bytes, index)?;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(index + 1);
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            None
        }
        _ => {
            let end = bytes[start..].iter().position(|byte| matches!(byte, b',' | b'}' | b']') || byte.is_ascii_whitespace()).map_or(bytes.len(), |offset| start + offset);
            (end > start).then_some(end)
        }
    }
}

/// The members of the top-level object of `text`, which serde_json has read as an object; `None`
/// when the text cannot be followed.
fn object_members(text: &str) -> Option<Vec<Member>> {
    let bytes = text.as_bytes();
    let skip = |mut index: usize| {
        while index < bytes.len() && bytes[index].is_ascii_whitespace() {
            index += 1;
        }
        index
    };
    let mut index = skip(0);
    if bytes.get(index) != Some(&b'{') {
        return None;
    }
    let mut members = Vec::new();
    loop {
        let lead_start = index + 1;
        index = skip(lead_start);
        match bytes.get(index)? {
            b'}' if members.is_empty() => return Some(members),
            b'"' => {}
            _ => return None,
        }
        let key_end = string_end(bytes, index)?;
        let key: String = serde_json::from_str(&text[index..key_end]).ok()?;
        let key_range = index..key_end;
        index = skip(key_end);
        if bytes.get(index) != Some(&b':') {
            return None;
        }
        let value_start = skip(index + 1);
        let end = value_end(bytes, value_start)?;
        members.push(Member { key, lead_start, key_range, value_range: value_start..end });
        index = skip(end);
        match bytes.get(index)? {
            b',' => {}
            b'}' => return Some(members),
            _ => return None,
        }
    }
}

/// `text` (a JSON object) with the values of `changes` in place of the old ones and new keys
/// after the last one, laid out like it (its indentation, its line end, its ": "); every other
/// byte stays. `None` for an object without members, which has no layout to follow.
fn patch_object_text(text: &str, changes: &[(&str, Value)]) -> Option<String> {
    let members = object_members(text)?;
    let last = members.last()?;
    let lead = match &text[last.lead_start..last.key_range.start] {
        "" => " ",
        lead => lead,
    };
    let between = &text[last.key_range.end..last.value_range.start];
    let mut edits: Vec<(usize, usize, String)> = Vec::new();
    let mut appended = String::new();
    for (key, value) in changes {
        let serialized = serde_json::to_string(value).ok()?;
        let mut found = false;
        for member in members.iter().filter(|member| member.key == *key) {
            edits.push((member.value_range.start, member.value_range.end, serialized.clone()));
            found = true;
        }
        if !found {
            appended.push_str(&format!(",{lead}{}{between}{serialized}", serde_json::to_string(key).ok()?));
        }
    }
    if !appended.is_empty() {
        edits.push((last.value_range.end, last.value_range.end, appended));
    }
    edits.sort_by_key(|(start, end, _)| (*start, *end));
    let mut patched = String::with_capacity(text.len() + 128);
    let mut cursor = 0;
    for (start, end, replacement) in edits {
        patched.push_str(&text[cursor..start]);
        patched.push_str(&replacement);
        cursor = end;
    }
    patched.push_str(&text[cursor..]);
    Some(patched)
}

/// Steps 1 to 3 of `load_or_create_at`: the text of settings.json without a byte order mark,
/// after creating the folder and, when the file is absent, the default file. `Err` holds the
/// reason as text.
fn read_or_create(settings_file: &Path, folder: &Path, default_directory: &Path) -> Result<String, String> {
    let read = || -> io::Result<Vec<u8>> {
        fs::create_dir_all(folder)?;
        // Like `Path.exists()` in the clone: a path that cannot be asked counts as absent, and
        // the creation below then reports why.
        if !settings_file.exists() {
            create_default_settings(settings_file, default_directory)?;
        }
        read_settings_bytes(settings_file)
    };
    let bytes = read().map_err(|error| error.to_string())?;
    let text = String::from_utf8(bytes).map_err(|error| error.utf8_error().to_string())?;
    Ok(match text.strip_prefix('\u{feff}') {
        Some(without_bom) => without_bom.to_owned(),
        None => text,
    })
}

/// Publishes the first settings.json: exactly `{"data_directory": "<default>"}`. A file that
/// another start of the app (or the user) created in the meantime is kept as it is.
fn create_default_settings(settings_file: &Path, default_directory: &Path) -> io::Result<()> {
    // A lossy conversion would write the name of another folder into the user's file.
    let directory = default_directory.to_str().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "the name of the settings folder is not valid Unicode")
    })?;
    let mut document = Map::new();
    document.insert(KEY_DATA_DIRECTORY.to_owned(), Value::from(directory));
    // "{:#}" is serde_json's pretty printer: two spaces, as the clone's `json.dump(indent=2)`.
    let text = format!("{:#}\n", Value::Object(document));
    store::atomic_write(settings_file, text.as_bytes(), WriteMode::CreateNew).map(|_| ())
}

/// The bytes of settings.json. On Windows, opening a file that another start is moving into
/// place (or that a virus scanner holds for a moment) fails with "access denied": such a read
/// is retried briefly (settings.py:70-83).
fn read_settings_bytes(settings_file: &Path) -> io::Result<Vec<u8>> {
    store::retry_while_in_use(READ_ATTEMPTS, &|_retry| READ_RETRY_MS, &mut || fs::read(settings_file))
}

/// Python's `not value.strip()`: nothing but white space (the separators U+001C to U+001F
/// included, which Python counts as white space and Rust does not).
fn is_blank(value: &str) -> bool {
    value.chars().all(|character| character.is_whitespace() || ('\u{1c}'..='\u{1f}').contains(&character))
}

/// Expands a folder path from settings.json (settings.py:163-169; spec-conventions 2.7):
///
/// 1. `%NAME%` is replaced by the value of the variable `NAME` (`expand_variables`); an unknown
///    variable is left as written;
/// 2. a leading `~` (alone, or followed by "/" or "\") becomes the user's home: `USERPROFILE`,
///    else `HOME`; left as written when neither is set;
/// 3. a path that is not absolute resolves from `settings_dir`, the folder of settings.json;
/// 4. the result is made absolute and lexically normalised ("." and ".." collapsed) without
///    asking the file system (`std::path::absolute`).
///
/// `Err` holds the reason when the path cannot be made absolute (an empty result, a NUL).
/// Tested inputs of the clone: `%SETTINGS_TEST_STORAGE%/records`, `~/records`, `../my-data`.
pub fn expand_path(value: &str, env: &dyn EnvLookup, settings_dir: &Path) -> Result<PathBuf, String> {
    let expanded = expand_home(expand_variables(value, env), env);
    if expanded.contains('\0') {
        return Err("the path contains a NUL character".to_owned());
    }
    // Windows drops trailing spaces of a name, so a blank path would silently become the
    // settings folder itself.
    if is_blank(&expanded) {
        return Err("the path is empty".to_owned());
    }
    let path = PathBuf::from(expanded);
    // "C:folder" counts from the current folder of drive C, which depends on how the app was
    // started; joining it to the settings folder would keep it as it is. So it is refused.
    let mut components = path.components();
    let drive_letter = matches!(components.next(), Some(std::path::Component::Prefix(prefix)) if matches!(prefix.kind(), std::path::Prefix::Disk(_)));
    if drive_letter && !matches!(components.next(), Some(std::path::Component::RootDir)) {
        return Err("a drive letter must be followed by a backslash, as in C:\\folder".to_owned());
    }
    let path = if path.is_absolute() { path } else { settings_dir.join(path) };
    let absolute = std::path::absolute(&path).map_err(|error| error.to_string())?;
    Ok(collapse_dots(&absolute))
}

/// Step 2 of `expand_path`: `~`, `~/...` and `~\...` start at the user's home. `~name` (another
/// user's home) is not a form Windows knows and stays as written.
fn expand_home(path: String, env: &dyn EnvLookup) -> String {
    let Some(rest) = path.strip_prefix('~') else {
        return path;
    };
    if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')) {
        return path;
    }
    match set_variable(env, "USERPROFILE").or_else(|| set_variable(env, "HOME")) {
        Some(home) => format!("{home}{rest}"),
        None => path,
    }
}

/// `path` made absolute without asking the file system; unchanged when that is impossible (an
/// empty path), so that the error message of the later file access names what was given.
fn absolute_path(path: &Path) -> PathBuf {
    match std::path::absolute(path) {
        Ok(absolute) => collapse_dots(&absolute),
        Err(_) => path.to_path_buf(),
    }
}

/// Removes "." and resolves ".." by dropping the folder before it, by name only. Windows does
/// this in `std::path::absolute` already; other platforms keep "..", and the contract is the
/// same everywhere. A ".." directly under the root stays at the root.
fn collapse_dots(path: &Path) -> PathBuf {
    let mut collapsed = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => match collapsed.components().next_back() {
                Some(Component::Normal(_)) => {
                    collapsed.pop();
                }
                Some(Component::RootDir | Component::Prefix(_)) => {}
                Some(Component::ParentDir | Component::CurDir) | None => collapsed.push(".."),
            },
            other => collapsed.push(other.as_os_str()),
        }
    }
    collapsed
}

/// Replaces every `%NAME%` in `value` by the value of the variable `NAME`. A name that is unset
/// stays as written, including its percent signs; "%%" and a lone "%" stay as they are.
pub fn expand_variables(value: &str, env: &dyn EnvLookup) -> String {
    let mut expanded = String::with_capacity(value.len());
    let mut rest = value;
    while let Some(start) = rest.find('%') {
        expanded.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('%') else {
            // A lone "%": nothing more can be a variable.
            expanded.push_str(&rest[start..]);
            return expanded;
        };
        let name = &after[..end];
        let known = if name.is_empty() { None } else { env.var(name) };
        match known {
            Some(replacement) => expanded.push_str(&replacement),
            None => {
                expanded.push('%');
                expanded.push_str(name);
                expanded.push('%');
            }
        }
        rest = &after[end + 1..];
    }
    expanded.push_str(rest);
    expanded
}

/// The default author of loose pictures (design brief "Opening pictures"): the Windows account
/// name (`USERNAME`, else `USER`) reduced with `reduce_account_name`, else "unknown"
/// (`messages::UNKNOWN_USERNAME`).
pub fn account_username(env: &dyn EnvLookup) -> String {
    set_variable(env, "USERNAME")
        .or_else(|| set_variable(env, "USER"))
        .and_then(|name| reduce_account_name(&name))
        .unwrap_or_else(|| messages::UNKNOWN_USERNAME.to_owned())
}

/// An account name reduced to what looks like an Instagram username: every character outside
/// `[A-Za-z0-9._]` is removed and the first `MAX_ACCOUNT_NAME_CHARS` (30) of the rest are kept;
/// `None` when nothing is left. "Jane Doe" -> "JaneDoe"; "José" -> "Jos"; "用户" -> `None`.
pub fn reduce_account_name(name: &str) -> Option<String> {
    let reduced: String = name
        .chars()
        .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '.' | '_'))
        .take(MAX_ACCOUNT_NAME_CHARS)
        .collect();
    if reduced.is_empty() {
        None
    } else {
        Some(reduced)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(name, value)| ((*name).to_owned(), (*value).to_owned())).collect()
    }

    /// An absolute folder that needs no file system: these tests only compute paths.
    fn base() -> PathBuf {
        std::path::absolute("settings-unit-test").expect("the current folder is known")
    }

    #[test]
    fn a_save_replaces_only_the_values_it_writes() {
        // One line, with strings that hold quotes, braces and brackets, and a nested object.
        let line = r#"{"a": "x\"}{", "b": {"c": [1, "]"]}, "frontend_port": 1}"#;
        let changes = [(KEY_FRONTEND_PORT, Value::from(2)), (KEY_WEB_SERVER, Value::from(false))];
        assert_eq!(patch_object_text(line, &changes).as_deref(), Some(r#"{"a": "x\"}{", "b": {"c": [1, "]"]}, "frontend_port": 2, "web_server": false}"#));
        // No space after the comma in the file: one is put in front of the new key.
        assert_eq!(patch_object_text(r#"{"a":1}"#, &[(KEY_WEB_HOST, Value::from("0.0.0.0"))]).as_deref(), Some(r#"{"a":1, "web_host":"0.0.0.0"}"#));
        // A key the file has twice gets the new value in both places (serde_json reads the last).
        assert_eq!(patch_object_text(r#"{"frontend_port": 1, "frontend_port": 2}"#, &changes[..1]).as_deref(), Some(r#"{"frontend_port": 2, "frontend_port": 2}"#));
        assert_eq!(patch_object_text("{ }", &changes), None, "nothing to follow: written anew");
        assert_eq!(patch_object_text("[1]", &changes), None);
    }

    #[test]
    fn variables_are_expanded_and_unknown_ones_stay_as_written() {
        let env = env(&[("STORAGE", "D:\\Data"), ("Empty", "")]);
        assert_eq!(expand_variables("%STORAGE%/records", &env), "D:\\Data/records");
        // Windows compares variable names without regard to letter case.
        assert_eq!(expand_variables("%storage%\\a\\%Storage%", &env), "D:\\Data\\a\\D:\\Data");
        assert_eq!(expand_variables("%MISSING%/records", &env), "%MISSING%/records");
        assert_eq!(expand_variables("%MISSING%%STORAGE%", &env), "%MISSING%D:\\Data");
        assert_eq!(expand_variables("a%EMPTY%b", &env), "ab");
        assert_eq!(expand_variables("100%", &env), "100%");
        assert_eq!(expand_variables("50%% of %STORAGE%", &env), "50%% of D:\\Data");
        assert_eq!(expand_variables("%%STORAGE%%", &env), "%%STORAGE%%");
        assert_eq!(expand_variables("%STORAGE", &env), "%STORAGE");
        assert_eq!(expand_variables("", &env), "");
        assert_eq!(expand_variables("plain/path", &env), "plain/path");
        assert_eq!(expand_variables("%ÜBER%ß", &env), "%ÜBER%ß");
    }

    #[test]
    fn relative_paths_resolve_from_the_settings_folder() {
        let folder = base().join("roaming").join(APP_NAME);
        let none = env(&[]);
        assert_eq!(expand_path("data", &none, &folder), Ok(folder.join("data")));
        assert_eq!(
            expand_path("../my-data", &none, &folder),
            Ok(base().join("roaming").join("my-data")),
            "the clone's test_existing_settings_preserve_formatting_and_unknown_keys"
        );
        assert_eq!(expand_path("./a/./b/../c", &none, &folder), Ok(folder.join("a").join("c")));
        let absolute = base().join("elsewhere");
        assert_eq!(expand_path(absolute.to_str().expect("unicode"), &none, &folder), Ok(absolute));
    }

    #[test]
    fn variables_and_home_are_expanded_in_paths() {
        let folder = base().join("roaming").join(APP_NAME);
        let expanded_root = base().join("expanded");
        let home = base().join("home");
        let env = env(&[
            ("SETTINGS_TEST_STORAGE", expanded_root.to_str().expect("unicode")),
            ("USERPROFILE", home.to_str().expect("unicode")),
            ("HOME", "/somewhere/else"),
        ]);
        // test_settings.py:146-158
        assert_eq!(
            expand_path("%SETTINGS_TEST_STORAGE%/records", &env, &folder),
            Ok(expanded_root.join("records"))
        );
        assert_eq!(expand_path("~/records", &env, &folder), Ok(home.join("records")));
        assert_eq!(expand_path("~\\records", &env, &folder), Ok(home.join("records")));
        assert_eq!(expand_path("~", &env, &folder), Ok(home.clone()));
        // Only a leading "~" on its own is the home folder.
        assert_eq!(expand_path("~records", &env, &folder), Ok(folder.join("~records")));
        assert_eq!(expand_path("a/~/b", &env, &folder), Ok(folder.join("a").join("~").join("b")));
        // An unknown variable is left as written, and is then a relative folder name.
        assert_eq!(expand_path("%NOT_SET%/records", &env, &folder), Ok(folder.join("%NOT_SET%").join("records")));
    }

    #[test]
    fn home_falls_back_to_home_and_then_stays_as_written() {
        let folder = base().join("roaming").join(APP_NAME);
        let home = base().join("unix-home");
        let only_home = env(&[("HOME", home.to_str().expect("unicode")), ("USERPROFILE", "")]);
        assert_eq!(expand_path("~/records", &only_home, &folder), Ok(home.join("records")));
        assert_eq!(expand_path("~/records", &env(&[]), &folder), Ok(folder.join("~").join("records")));
    }

    #[test]
    fn paths_that_cannot_be_folders_are_refused() {
        let folder = base();
        let env = env(&[("BLANK", "  "), ("NUL", "a\0b")]);
        assert_eq!(expand_path("", &env, &folder), Err("the path is empty".to_owned()));
        assert_eq!(expand_path("   ", &env, &folder), Err("the path is empty".to_owned()));
        assert_eq!(expand_path("%BLANK%", &env, &folder), Err("the path is empty".to_owned()));
        assert_eq!(expand_path("a\0b", &env, &folder), Err("the path contains a NUL character".to_owned()));
        assert_eq!(expand_path("%NUL%", &env, &folder), Err("the path contains a NUL character".to_owned()));
    }

    #[cfg(windows)]
    #[test]
    fn windows_paths_are_normalised() {
        let folder = PathBuf::from(r"C:\Users\Me\AppData\Roaming\instagram-windows-app");
        let none = env(&[]);
        // The documented example of the clone's README uses forward slashes.
        assert_eq!(expand_path("D:/CounterData", &none, &folder), Ok(PathBuf::from(r"D:\CounterData")));
        assert_eq!(expand_path(r"D:\a\..\b\.\c", &none, &folder), Ok(PathBuf::from(r"D:\b\c")));
        assert_eq!(
            expand_path("../../my-data", &none, &folder),
            Ok(PathBuf::from(r"C:\Users\Me\AppData\my-data"))
        );
        // A rooted path without a drive letter stays on the drive of the settings folder.
        assert_eq!(expand_path(r"\Pictures\data", &none, &folder), Ok(PathBuf::from(r"C:\Pictures\data")));
        assert_eq!(
            expand_path(r"\\server\share\data", &none, &folder),
            Ok(PathBuf::from(r"\\server\share\data"))
        );
        assert_eq!(expand_path(r"\\server\share", &none, &folder), Ok(PathBuf::from(r"\\server\share")));
        // "C:folder" would count from the current folder of the process on drive C.
        let refused = Err(r"a drive letter must be followed by a backslash, as in C:\folder".to_owned());
        assert_eq!(expand_path("C:relative-to-drive", &none, &folder), refused);
        assert_eq!(expand_path("D:", &none, &folder), refused);
        assert_eq!(expand_path(r"D:\", &none, &folder), Ok(PathBuf::from(r"D:\")));
    }

    #[test]
    fn dots_are_collapsed_by_name_only() {
        let root = base();
        assert_eq!(collapse_dots(&root.join("a").join("..").join("b")), root.join("b"));
        assert_eq!(collapse_dots(&root.join(".").join("a").join(".")), root.join("a"));
        assert_eq!(collapse_dots(Path::new("../a/../../b")), Path::new("..").join("..").join("b"));
        assert_eq!(collapse_dots(Path::new("a/b/../..")), PathBuf::new());
    }

    #[test]
    fn the_settings_file_is_in_the_roaming_folder() {
        let roaming = base().join("roaming");
        let home = base().join("home");
        let expected = roaming.join(APP_NAME).join("settings.json");

        let with_appdata =
            env(&[("APPDATA", roaming.to_str().expect("unicode")), ("USERPROFILE", home.to_str().expect("unicode"))]);
        assert_eq!(settings_path(&with_appdata), Ok(expected));

        // test_settings.py:160-168: without APPDATA the home folder decides.
        let fallback = home.join("AppData").join("Roaming").join(APP_NAME).join("settings.json");
        let only_home = env(&[("USERPROFILE", home.to_str().expect("unicode"))]);
        assert_eq!(settings_path(&only_home), Ok(fallback.clone()));
        let empty_appdata = env(&[("APPDATA", ""), ("UserProfile", home.to_str().expect("unicode"))]);
        assert_eq!(settings_path(&empty_appdata), Ok(fallback));

        let nothing = settings_path(&env(&[("USERPROFILE", "")])).expect_err("no folder is known");
        assert_eq!(nothing.message, "Cannot locate the settings folder: neither APPDATA nor USERPROFILE is set");
        assert_eq!(nothing.to_string(), nothing.message);
    }

    #[test]
    fn the_data_folder_and_its_files_follow_the_settings_file() {
        let settings_file = base().join(APP_NAME).join("settings.json");
        let data = default_data_directory(&settings_file);
        assert_eq!(data, base().join(APP_NAME).join("data"));
        let paths = AppPaths::new(settings_file.clone(), data.clone());
        assert_eq!(paths.settings_file, settings_file);
        assert_eq!(paths.descriptions_file, data.join("descriptions.json"));
        assert_eq!(paths.state_file, data.join("state.json"));
        assert_eq!(paths.window_file, data.join("window.ron"));
        assert_eq!(paths.log_file, data.join("app.log"));
    }

    #[test]
    fn the_local_folder_follows_localappdata() {
        let data = base().join("data");
        let local = base().join("Local");
        let with_local = env(&[("LOCALAPPDATA", local.to_str().expect("unicode"))]);
        assert_eq!(local_dir(&with_local, &data), local.join(APP_NAME));
        assert_eq!(local_dir(&env(&[("LOCALAPPDATA", "")]), &data), data.join("local"));
        assert_eq!(local_dir(&env(&[]), &data), data.join("local"));
        let paths = AppPaths::new(base().join("settings.json"), data.clone());
        assert_eq!(paths.archive_cache_dir, data.join("local").join("archives"));
        let paths = paths.with_local_dir(local.join(APP_NAME));
        assert_eq!(paths.archive_cache_dir, local.join(APP_NAME).join("archives"));
        assert_eq!(paths.poster_cache_dir, local.join(APP_NAME).join("posters"));
        assert_eq!(paths.library_dir, local.join(APP_NAME).join("library"));
        assert_eq!(paths.web_app_library_dir, None);
    }

    #[test]
    fn the_web_apps_library_follows_the_web_apps_rules() {
        let folder = crate::test_folder::TestFolder::new("settings-web").expect("a test folder");
        let roaming = folder.join("Roaming");
        let temp = folder.join("Temp");
        let roaming_text = roaming.to_str().expect("unicode").to_owned();
        let temp_text = temp.to_str().expect("unicode").to_owned();
        let base_env = |extra: &[(&str, &str)]| {
            let mut pairs = vec![("APPDATA", roaming_text.as_str()), ("TEMP", temp_text.as_str())];
            pairs.extend_from_slice(extra);
            env(&pairs)
        };
        // Nothing configured: the web app's default under %TEMP%.
        assert_eq!(web_app_library_dir(&base_env(&[])), Some(temp.join("instagram_cache")));
        assert_eq!(web_app_library_dir(&env(&[("TMP", temp_text.as_str())])), Some(temp.join("instagram_cache")));
        // An absolute INSTAGRAM_CACHE_DIR wins; a relative one cannot be placed and is ignored.
        let absolute = folder.join("elsewhere");
        let absolute_text = absolute.to_str().expect("unicode").to_owned();
        assert_eq!(web_app_library_dir(&base_env(&[("INSTAGRAM_CACHE_DIR", &absolute_text)])), Some(absolute.clone()));
        assert_eq!(web_app_library_dir(&base_env(&[("INSTAGRAM_CACHE_DIR", "relative\\cache")])), None);
        // The key of the web app's settings.json, relative to its folder.
        let clone_folder = roaming.join(WEB_APP_NAME);
        fs::create_dir_all(&clone_folder).expect("the web app's settings folder");
        fs::write(clone_folder.join("settings.json"), "\u{feff}{\"instagram_cache_directory\": \"../cache-here\"}").expect("written");
        assert_eq!(web_app_library_dir(&base_env(&[])), Some(roaming.join("cache-here")));
        // A broken file is treated as absent.
        fs::write(clone_folder.join("settings.json"), "{ not json").expect("written");
        assert_eq!(web_app_library_dir(&base_env(&[])), Some(temp.join("instagram_cache")));
        fs::write(clone_folder.join("settings.json"), "{\"instagram_cache_directory\": 42}").expect("written");
        assert_eq!(web_app_library_dir(&base_env(&[])), Some(temp.join("instagram_cache")));
    }

    #[test]
    fn account_names_are_reduced_to_username_characters() {
        assert_eq!(reduce_account_name("Jane Doe"), Some("JaneDoe".to_owned()));
        assert_eq!(reduce_account_name("José"), Some("Jos".to_owned()));
        assert_eq!(reduce_account_name("用户"), None);
        assert_eq!(reduce_account_name(""), None);
        assert_eq!(reduce_account_name("- @ -"), None);
        assert_eq!(reduce_account_name("j.doe_99"), Some("j.doe_99".to_owned()));
        assert_eq!(reduce_account_name("DOMAIN\\jane-doe"), Some("DOMAINjanedoe".to_owned()));
        let long = "a".repeat(31);
        assert_eq!(reduce_account_name(&long), Some("a".repeat(30)));
        // The limit counts kept characters, not the characters of the account name.
        assert_eq!(reduce_account_name(&format!("{}b", "- ".repeat(40))), Some("b".to_owned()));
    }

    #[test]
    fn the_default_author_is_the_account_name_or_unknown() {
        assert_eq!(account_username(&env(&[("USERNAME", "Jane Doe")])), "JaneDoe");
        assert_eq!(account_username(&env(&[("username", "jane")])), "jane");
        assert_eq!(account_username(&env(&[("USERNAME", ""), ("USER", "runner")])), "runner");
        // A name without a usable character does not fall through to USER: it is the account.
        assert_eq!(account_username(&env(&[("USERNAME", "用户"), ("USER", "runner")])), "unknown");
        assert_eq!(account_username(&env(&[])), "unknown");
    }

    #[test]
    fn blank_follows_the_python_rule() {
        assert!(is_blank(""));
        assert!(is_blank(" \t\r\n\u{a0}\u{3000}\u{1c}\u{1f}"));
        assert!(!is_blank(" a "));
        assert!(!is_blank("\0"));
    }
}
