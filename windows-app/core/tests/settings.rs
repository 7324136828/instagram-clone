//! The settings.json contract, through the public API only.
//!
//! These are the tests of the reference web project's backend/tests/test_settings.py that apply
//! to an app without a server (the line numbers in the comments are those of that file): the
//! file is created only when it is absent, simultaneous first starts agree, a byte order mark is
//! accepted, a valid file is never rewritten, an invalid file is never touched, unknown keys
//! stay, `%VAR%` and `~` are expanded, relative paths start at the settings folder, and every
//! error names the path.
//!
//! Nothing here touches the real user profile: each test has its own folder under the system's
//! temporary folder and passes a map as the environment.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Barrier;
use std::thread;

use instagram_core::settings::{self, Settings, APP_NAME};

/// A pretend user profile: `{root}\roaming` is `%APPDATA%`, `{root}\home` is `%USERPROFILE%`.
struct Profile {
    root: PathBuf,
}

impl Profile {
    fn new(tag: &str) -> Profile {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let unique = format!(
            "instagram-core-settings-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let root = std::env::temp_dir().join(unique);
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("a test folder can be created");
        Profile { root }
    }

    fn env(&self) -> HashMap<String, String> {
        self.env_with(&[])
    }

    /// The environment of this profile plus `extra` (a later entry replaces an earlier one).
    fn env_with(&self, extra: &[(&str, &str)]) -> HashMap<String, String> {
        let mut env = HashMap::new();
        env.insert("APPDATA".to_owned(), text(&self.root.join("roaming")));
        env.insert("USERPROFILE".to_owned(), text(&self.root.join("home")));
        env.insert("USERNAME".to_owned(), "Jane Doe".to_owned());
        for (name, value) in extra {
            env.insert((*name).to_owned(), (*value).to_owned());
        }
        env
    }

    fn settings_folder(&self) -> PathBuf {
        self.root.join("roaming").join(APP_NAME)
    }

    fn settings_file(&self) -> PathBuf {
        self.settings_folder().join("settings.json")
    }

    /// Writes settings.json as the user would, byte for byte.
    fn write_settings(&self, content: &[u8]) -> PathBuf {
        let path = self.settings_file();
        fs::create_dir_all(self.settings_folder()).expect("the settings folder");
        fs::write(&path, content).expect("the settings file");
        path
    }

    fn load(&self) -> Result<Settings, settings::SettingsError> {
        settings::load_or_create(&self.env())
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.root);
    }
}

fn text(path: &Path) -> String {
    path.to_str().expect("test paths are Unicode").to_owned()
}

/// The names of everything directly in `folder`, sorted.
fn names_in(folder: &Path) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(folder)
        .expect("the folder can be listed")
        .map(|entry| entry.expect("an entry").file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

/// What the first start writes: one key, two-space indentation, a final newline, no BOM.
fn default_file_text(data_directory: &Path) -> String {
    let quoted = serde_json::to_string(&text(data_directory)).expect("a string is JSON");
    format!("{{\n  \"data_directory\": {quoted}\n}}\n")
}

#[test]
fn the_app_has_its_own_settings_folder() {
    // Sharing the reference project's name would make the two apps share one settings.json.
    assert_eq!(APP_NAME, "instagram-windows-app");
}

// test_settings.py:43-58
#[test]
fn first_load_creates_per_user_settings_with_the_default_data_folder() {
    let profile = Profile::new("first-load");
    let expected_settings = profile.settings_file();
    let expected_data = profile.settings_folder().join("data");
    assert!(!profile.settings_folder().exists());

    let loaded = profile.load().expect("the first start");

    assert_eq!(loaded.paths.settings_file, expected_settings);
    assert_eq!(loaded.paths.data_dir, expected_data);
    assert_eq!(loaded.paths.descriptions_file, expected_data.join("descriptions.json"));
    assert_eq!(loaded.paths.state_file, expected_data.join("state.json"));
    assert_eq!(loaded.paths.window_file, expected_data.join("window.ron"));
    assert_eq!(loaded.paths.log_file, expected_data.join("app.log"));
    assert_eq!(loaded.default_username, "JaneDoe", "the account name, reduced");

    let original = fs::read(&expected_settings).expect("the file was created");
    assert_eq!(String::from_utf8(original.clone()).expect("UTF-8"), default_file_text(&expected_data));
    let parsed: serde_json::Value = serde_json::from_slice(&original).expect("JSON");
    assert_eq!(parsed, serde_json::json!({ "data_directory": text(&expected_data) }));
    // No temporary file is left, and the data folder is not created by the settings code.
    assert_eq!(names_in(&profile.settings_folder()), ["settings.json"]);

    // The second start reads the same and writes nothing.
    assert_eq!(profile.load().expect("the second start"), loaded);
    assert_eq!(fs::read(&expected_settings).expect("readable"), original);
    assert_eq!(names_in(&profile.settings_folder()), ["settings.json"]);
}

// test_settings.py:60-79
#[test]
fn simultaneous_first_loads_read_complete_identical_defaults() {
    let profile = Profile::new("simultaneous");
    let workers = 8;
    let ready = Barrier::new(workers);

    let loaded: Vec<Settings> = thread::scope(|scope| {
        let handles: Vec<_> = (0..workers)
            .map(|_| {
                scope.spawn(|| {
                    ready.wait();
                    profile.load().expect("every start succeeds")
                })
            })
            .collect();
        handles.into_iter().map(|handle| handle.join().expect("no panic")).collect()
    });

    let expected_data = profile.settings_folder().join("data");
    for settings in &loaded {
        assert_eq!(settings.paths.settings_file, profile.settings_file());
        assert_eq!(settings.paths.data_dir, expected_data);
    }
    assert_eq!(
        fs::read_to_string(profile.settings_file()).expect("readable"),
        default_file_text(&expected_data)
    );
    assert_eq!(names_in(&profile.settings_folder()), ["settings.json"]);
}

// test_settings.py:415-431: a file that appears while this start prepares its own is kept.
#[test]
fn a_file_from_another_start_is_never_overwritten() {
    use instagram_core::store::{atomic_write, WriteMode};

    let profile = Profile::new("another-start");
    let content = b"{ \"data_directory\": \"elsewhere\", \"theme\": \"dark\" }\n";
    let path = profile.write_settings(content);

    // The creation step of the first start, arriving late: it must not replace the file.
    let created = atomic_write(&path, b"{\n  \"data_directory\": \"x\"\n}\n", WriteMode::CreateNew).expect("no error");
    assert!(!created);
    assert_eq!(fs::read(&path).expect("readable"), content);

    let loaded = profile.load().expect("the other start's file is valid");
    assert_eq!(loaded.paths.data_dir, profile.settings_folder().join("elsewhere"));
    assert_eq!(fs::read(&path).expect("readable"), content);
    assert_eq!(names_in(&profile.settings_folder()), ["settings.json"]);
}

// test_settings.py:97-105
#[test]
fn a_settings_file_that_stays_unreadable_is_reported() {
    let profile = Profile::new("unreadable");
    let path = profile.settings_file();
    // A folder where the file should be: every read fails.
    fs::create_dir_all(&path).expect("a folder in the way");

    let error = profile.load().expect_err("a folder is not a settings file");
    let prefix = format!("Cannot read or create settings at {}: ", path.display());
    assert!(error.message.starts_with(&prefix), "{error}");
    assert!(error.message.len() > prefix.len(), "the reason follows the path");
    assert!(path.is_dir());
    assert_eq!(names_in(&profile.settings_folder()), ["settings.json"]);
}

#[test]
fn a_settings_folder_that_cannot_be_created_is_reported() {
    let profile = Profile::new("no-folder");
    // A file has the name of the roaming folder.
    fs::write(profile.root.join("roaming"), b"not a folder").expect("a file in the way");

    let error = profile.load().expect_err("the folder cannot be created");
    let prefix = format!("Cannot read or create settings at {}: ", profile.settings_file().display());
    assert!(error.message.starts_with(&prefix), "{error}");
    assert_eq!(fs::read(profile.root.join("roaming")).expect("readable"), b"not a folder");
}

// test_settings.py:107-114
#[test]
fn existing_settings_preserve_formatting_and_unknown_keys() {
    let profile = Profile::new("preserve");
    let content = b"{\n  \"data_directory\": \"../my-data\",\n  \"theme\": \"dark\"\n}\n";
    let path = profile.write_settings(content);

    let loaded = profile.load().expect("valid settings");

    assert_eq!(loaded.paths.data_dir, profile.root.join("roaming").join("my-data"));
    assert_eq!(fs::read(&path).expect("readable"), content);
    assert_eq!(names_in(&profile.settings_folder()), ["settings.json"]);
}

// test_settings.py:280-310: unknown keys of every kind, the user's own layout and line endings.
#[test]
fn a_valid_file_is_never_rewritten_whatever_its_layout() {
    let profile = Profile::new("layout");
    let contents: [&[u8]; 5] = [
        b"{\"data_directory\":\"records\"}",
        b"{\r\n\t\"theme\": {\"color\": \"blue\", \"enabled\": true},\r\n\t\"custom\": [1, \"two\", null],\r\n\t\"data_directory\": \"records\"\r\n}\r\n",
        b"\n\n  { \"frontend_port\": 4510, \"backend_port\": 4520, \"data_directory\"  :  \"records\" }  \n\n",
        b"{\"data_directory\": \"records\", \"default_username\": null, \"data_Directory\": 42}",
        "{\"data_directory\": \"records\", \"notiz\": \"Grüße 🌊\"}".as_bytes(),
    ];
    for content in contents {
        let path = profile.write_settings(content);
        for _ in 0..2 {
            let loaded = profile.load().expect("valid settings");
            assert_eq!(loaded.paths.data_dir, profile.settings_folder().join("records"));
            assert_eq!(loaded.default_username, "JaneDoe");
            assert_eq!(fs::read(&path).expect("readable"), content);
        }
        assert_eq!(names_in(&profile.settings_folder()), ["settings.json"]);
    }
}

// test_settings.py:116-124
#[test]
fn a_missing_data_directory_uses_the_default_without_rewriting() {
    let profile = Profile::new("no-key");
    let contents: [&[u8]; 2] = [b"{ \"theme\": \"dark\" }\n", b"{}"];
    for content in contents {
        let path = profile.write_settings(content);
        let loaded = profile.load().expect("valid settings");
        assert_eq!(loaded.paths.data_dir, profile.settings_folder().join("data"));
        assert_eq!(fs::read(&path).expect("readable"), content);
    }
}

// test_settings.py:126-144
#[test]
fn invalid_settings_report_the_path_and_preserve_the_users_file() {
    let profile = Profile::new("invalid");
    let path = profile.settings_file();
    let shown = path.display().to_string();
    let unreadable = format!("Cannot read or create settings at {shown}: ");
    let not_an_object = format!("Invalid settings at {shown}: expected a JSON object");
    let bad_folder = format!("Invalid settings at {shown}: data_directory must be a non-empty folder path");
    let bad_name = format!("Invalid settings at {shown}: default_username must be a non-empty name");

    // (content, the whole message, or its beginning when the reason comes from the system)
    let cases: [(&[u8], &str, bool); 19] = [
        (b"{broken json", &unreadable, false),
        (b"", &unreadable, false),
        (b"{\"data_directory\": \"data\"", &unreadable, false),
        (b"\xff\xfe{\x00}\x00", &unreadable, false),
        (b"{\"data_directory\": \"\xff\"}", &unreadable, false),
        (b"null", &not_an_object, true),
        (b"[]", &not_an_object, true),
        (b"\"a string\"", &not_an_object, true),
        (b"42", &not_an_object, true),
        (b"{\"data_directory\": null}", &bad_folder, true),
        (b"{\"data_directory\": 42}", &bad_folder, true),
        (b"{\"data_directory\": false}", &bad_folder, true),
        (b"{\"data_directory\": \"\"}", &bad_folder, true),
        (b"{\"data_directory\": \"   \"}", &bad_folder, true),
        (b"{\"data_directory\": [\"data\"]}", &bad_folder, true),
        (b"{\"data_directory\": \"da\\u0000ta\"}", &bad_folder, true),
        (b"{\"data_directory\": \"data\", \"default_username\": 42}", &bad_name, true),
        (b"{\"data_directory\": \"data\", \"default_username\": true}", &bad_name, true),
        (b"{\"data_directory\": \"data\", \"default_username\": [\"jane\"]}", &bad_name, true),
    ];
    for (content, message, whole) in cases {
        profile.write_settings(content);
        let error = profile.load().expect_err("invalid settings");
        let label = String::from_utf8_lossy(content);
        if whole {
            assert_eq!(error.message, message, "{label}");
        } else {
            assert!(error.message.starts_with(message), "{label}: {error}");
            assert!(error.message.len() > message.len(), "{label}: the reason follows");
        }
        // Every message names the path, is one line and has no trailing period.
        assert!(error.message.contains(&shown), "{label}");
        assert!(!error.message.contains('\n') && !error.message.ends_with('.'), "{label}: {error}");
        assert_eq!(error.to_string(), error.message);
        // The user's file is exactly as it was, and nothing was put beside it.
        assert_eq!(fs::read(&path).expect("readable"), content, "{label}");
        assert_eq!(names_in(&profile.settings_folder()), ["settings.json"], "{label}");
    }
}

// test_settings.py:329-338
#[test]
fn a_utf8_byte_order_mark_is_accepted_and_kept() {
    let profile = Profile::new("bom");
    let content = "\u{feff}{\"data_directory\": \"records\", \"theme\": \"dark\"}".as_bytes();
    assert_eq!(&content[..3], b"\xEF\xBB\xBF");
    let path = profile.write_settings(content);

    let loaded = profile.load().expect("a BOM is fine");
    assert_eq!(loaded.paths.data_dir, profile.settings_folder().join("records"));
    assert_eq!(fs::read(&path).expect("readable"), content);
}

// test_settings.py:146-158
#[test]
fn data_directory_expands_environment_variables_and_home() {
    let profile = Profile::new("expand");
    let expanded_root = profile.root.join("expanded");
    let env = profile.env_with(&[("SETTINGS_TEST_STORAGE", text(&expanded_root).as_str())]);

    profile.write_settings(b"{\"data_directory\": \"%SETTINGS_TEST_STORAGE%/records\"}");
    let loaded = settings::load_or_create(&env).expect("valid settings");
    assert_eq!(loaded.paths.data_dir, expanded_root.join("records"));
    assert_eq!(loaded.paths.descriptions_file, expanded_root.join("records").join("descriptions.json"));

    profile.write_settings(b"{\"data_directory\": \"~/records\"}");
    let loaded = settings::load_or_create(&env).expect("valid settings");
    assert_eq!(loaded.paths.data_dir, profile.root.join("home").join("records"));

    // An absolute path is taken as it is.
    let absolute = profile.root.join("chosen-storage").join("records");
    let content = serde_json::json!({ "data_directory": text(&absolute), "theme": "dark" }).to_string();
    profile.write_settings(content.as_bytes());
    let loaded = settings::load_or_create(&env).expect("valid settings");
    assert_eq!(loaded.paths.data_dir, absolute);

    // A variable that is not set stays as written: a folder of that name beside settings.json.
    profile.write_settings(b"{\"data_directory\": \"%SETTINGS_TEST_MISSING%\"}");
    let loaded = settings::load_or_create(&env).expect("valid settings");
    assert_eq!(loaded.paths.data_dir, profile.settings_folder().join("%SETTINGS_TEST_MISSING%"));

    // A variable that expands to nothing leaves no folder to use.
    let blank = profile.env_with(&[("SETTINGS_TEST_STORAGE", "  ")]);
    profile.write_settings(b"{\"data_directory\": \"%SETTINGS_TEST_STORAGE%\"}");
    let error = settings::load_or_create(&blank).expect_err("no folder");
    assert_eq!(
        error.message,
        format!("Invalid data_directory in settings at {}: the path is empty", profile.settings_file().display())
    );
    // None of the folders named above was created by reading the settings.
    assert_eq!(names_in(&profile.root), ["roaming"]);
}

// test_settings.py:160-168
#[test]
fn the_settings_folder_falls_back_to_the_user_home() {
    let profile = Profile::new("fallback");
    let home = profile.root.join("home");
    let expected = home.join("AppData").join("Roaming").join(APP_NAME).join("settings.json");

    for appdata in [None, Some("")] {
        let mut env = HashMap::from([("USERPROFILE".to_owned(), text(&home))]);
        if let Some(value) = appdata {
            env.insert("APPDATA".to_owned(), value.to_owned());
        }
        assert_eq!(settings::settings_path(&env).expect("the home folder is known"), expected);
        let loaded = settings::load_or_create(&env).expect("the first start");
        assert_eq!(loaded.paths.settings_file, expected);
        assert_eq!(loaded.paths.data_dir, expected.parent().expect("a parent").join("data"));
        assert!(expected.is_file());
        // Without an account name the author of loose pictures is "unknown".
        assert_eq!(loaded.default_username, "unknown");
    }

    let nowhere: HashMap<String, String> = HashMap::new();
    let error = settings::load_or_create(&nowhere).expect_err("no folder is known");
    assert_eq!(error.message, "Cannot locate the settings folder: neither APPDATA nor USERPROFILE is set");
}

#[test]
fn the_default_author_comes_from_the_account_unless_the_settings_name_one() {
    let profile = Profile::new("author");
    let contents: [&[u8]; 2] = [b"{}", b"{\"default_username\": null}"];
    for content in contents {
        profile.write_settings(content);
        let loaded = profile.load().expect("valid settings");
        assert_eq!(loaded.default_username, "JaneDoe");
        assert!(!loaded.web.default_username_set, "the account name: other computers are not told it");
        let other = profile.env_with(&[("USERNAME", "用户")]);
        assert_eq!(settings::load_or_create(&other).expect("valid settings").default_username, "unknown");
    }
    profile.write_settings(br#"{"default_username": "lumen.atlas"}"#);
    assert!(profile.load().expect("valid settings").web.default_username_set, "a name the user chose");
}

#[test]
fn a_default_username_is_cleaned_like_the_author_of_an_export() {
    let profile = Profile::new("username");
    let path = profile.settings_file();

    // The clone's cleanUsername: trimmed, leading "@" removed. It does not have to look like an
    // Instagram name: the user decides what the author of their pictures is called.
    let accepted = [
        ("jane.doe", "jane.doe"),
        ("  @jane_doe  ", "jane_doe"),
        ("Jane Doe-Smith", "Jane Doe-Smith"),
        ("Zoë", "Zoë"),
    ];
    for (written, expected) in accepted {
        let content = serde_json::json!({ "default_username": written, "theme": "dark" }).to_string();
        profile.write_settings(content.as_bytes());
        let loaded = profile.load().expect("valid settings");
        assert_eq!(loaded.default_username, expected, "{written:?}");
        assert_eq!(loaded.paths.data_dir, profile.settings_folder().join("data"));
        assert_eq!(fs::read(&path).expect("readable"), content.as_bytes());
    }

    // A name with nothing in it is an error, not silently the account name.
    for written in ["", "   ", "@", " @@ "] {
        let content = serde_json::json!({ "default_username": written }).to_string();
        profile.write_settings(content.as_bytes());
        let error = profile.load().expect_err("an empty name");
        assert_eq!(
            error.message,
            format!("Invalid settings at {}: default_username must be a non-empty name", path.display()),
            "{written:?}"
        );
        assert_eq!(fs::read(&path).expect("readable"), content.as_bytes());
    }
}

#[test]
fn the_settings_file_may_be_named_directly() {
    let profile = Profile::new("direct");
    let given = profile.root.join("portable").join("..").join("portable").join(".").join("settings.json");
    let expected = profile.root.join("portable").join("settings.json");

    let loaded = settings::load_or_create_at(&given, &profile.env()).expect("the first start");
    assert_eq!(loaded.paths.settings_file, expected);
    assert_eq!(loaded.paths.data_dir, profile.root.join("portable").join("data"));
    assert_eq!(
        fs::read_to_string(&expected).expect("readable"),
        default_file_text(&profile.root.join("portable").join("data"))
    );
    assert_eq!(settings::default_data_directory(&expected), profile.root.join("portable").join("data"));
    // The real settings folder of this pretend profile was not touched.
    assert_eq!(names_in(&profile.root), ["portable"]);
}

// --- The browser pages' keys (web brief 4.11) and the Browser view sheet's Save -----------------

#[test]
fn the_browser_pages_keys_are_read_with_their_defaults() {
    let profile = Profile::new("web-keys");
    profile.write_settings(br#"{"data_directory": "data", "web_server": false, "frontend_port": 4000, "web_host": "0.0.0.0", "allowed_hosts": "photos.example, ,desk.example", "library_directory": "imports"}"#);
    let loaded = profile.load().expect("valid");
    assert_eq!(loaded.web.web_server, Some(false));
    assert_eq!(loaded.web.frontend_port, Some(4000));
    assert_eq!(loaded.web.backend_port, None);
    assert_eq!(loaded.web.backend_port_or_default(), 8317);
    assert_eq!(loaded.web.web_host, Some(std::net::Ipv4Addr::UNSPECIFIED));
    assert_eq!(loaded.web.allowed_hosts, ["photos.example", "desk.example"]);
    assert_eq!(loaded.paths.library_dir, profile.settings_folder().join("imports"));
    assert!(loaded.web.library_directory_set);
    let defaults = Profile::new("web-defaults");
    let loaded = defaults.load().expect("created");
    assert_eq!((loaded.web.frontend_port_or_default(), loaded.web.backend_port_or_default(), loaded.web.web_server_or_default()), (3127, 8317, true));
    assert_eq!(loaded.web.web_host_or_default(), std::net::Ipv4Addr::LOCALHOST);
    assert!(loaded.paths.library_dir.ends_with("library"));
}

#[test]
fn bad_browser_pages_keys_are_settings_errors_that_name_the_file() {
    let cases: [(&[u8], &str); 8] = [
        (br#"{"web_server": "yes"}"#, "web_server must be true or false"),
        (br#"{"frontend_port": 0}"#, "frontend_port must be an integer from 1 to 65535"),
        (br#"{"backend_port": 70000}"#, "backend_port must be an integer from 1 to 65535"),
        (br#"{"frontend_port": 3127.5}"#, "frontend_port must be an integer from 1 to 65535"),
        (br#"{"frontend_port": 8317}"#, "frontend_port and backend_port must be different"),
        (br#"{"web_host": "localhost"}"#, "web_host must be an IPv4 address such as 127.0.0.1 or 0.0.0.0"),
        (br#"{"allowed_hosts": 12}"#, "expected a list of host names"),
        (br#"{"allowed_hosts": ["a", 1]}"#, "every host name must be a string"),
    ];
    for (content, expected) in cases {
        let profile = Profile::new("web-bad");
        let path = profile.write_settings(content);
        let error = profile.load().expect_err(expected).message;
        assert!(error.contains(expected) && error.contains(&text(&path)), "{error}");
        assert_eq!(fs::read(&path).expect("the file"), content, "an invalid file is never touched");
    }
}

#[test]
fn saving_the_ports_changes_only_those_keys() {
    let profile = Profile::new("web-save");
    let original = "\u{feff}{\n    \"zeta\": 1,\n    \"data_directory\": \"data\",\n    \"frontend_port\": 4000,\n    \"nested\": {\"keep\": [1, 2.5, \"x\"]}\n}\n";
    let path = profile.write_settings(original.as_bytes());
    let changes = [("frontend_port", serde_json::Value::from(5000)), ("backend_port", serde_json::Value::from(5001)), ("web_host", serde_json::Value::from("0.0.0.0"))];
    settings::update_settings_file(&path, &changes).expect("saved");
    let written = fs::read_to_string(&path).expect("the file");
    // The file keeps its layout: only the changed value is replaced, the new keys follow the
    // last one with its indentation, and the nested object stays on its line.
    assert_eq!(written, "\u{feff}{\n    \"zeta\": 1,\n    \"data_directory\": \"data\",\n    \"frontend_port\": 5000,\n    \"nested\": {\"keep\": [1, 2.5, \"x\"]},\n    \"backend_port\": 5001,\n    \"web_host\": \"0.0.0.0\"\n}\n");
    let loaded = profile.load().expect("still valid");
    assert_eq!((loaded.web.frontend_port, loaded.web.backend_port), (Some(5000), Some(5001)));
    // Windows line ends and a one-line array of the user stay too.
    let original = "{\r\n  \"allowed_hosts\": [\"photos.example\"],\r\n  \"web_host\": \"0.0.0.0\",\r\n  \"zz_last\": [1, 2, {\"a\": true}]\r\n}";
    fs::write(&path, original).expect("a file of the user");
    settings::update_settings_file(&path, &[("web_host", serde_json::Value::from("127.0.0.1")), ("web_server", serde_json::Value::from(true))]).expect("saved");
    assert_eq!(fs::read_to_string(&path).expect("the file"), "{\r\n  \"allowed_hosts\": [\"photos.example\"],\r\n  \"web_host\": \"127.0.0.1\",\r\n  \"zz_last\": [1, 2, {\"a\": true}],\r\n  \"web_server\": true\r\n}");
    // An empty object has no layout to follow: written anew.
    fs::write(&path, "{}").expect("an empty object");
    settings::update_settings_file(&path, &[("web_server", serde_json::Value::from(false))]).expect("saved");
    assert_eq!(fs::read_to_string(&path).expect("the file"), "{\n  \"web_server\": false\n}\n");
    // No temporary file is left beside it.
    let names: Vec<String> = fs::read_dir(profile.settings_folder()).expect("the folder").flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    assert!(names.iter().all(|name| !name.ends_with(".tmp")), "{names:?}");
}

#[test]
fn an_invalid_settings_file_is_not_touched_by_a_save() {
    for (content, reason) in [(&b"{ \"frontend_port\": 4000, "[..], "it is not valid JSON ("), (&b"[1, 2]"[..], "it does not hold a JSON object")] {
        let profile = Profile::new("web-save-invalid");
        let path = profile.write_settings(content);
        let error = settings::update_settings_file(&path, &[("frontend_port", serde_json::Value::from(5000))]).expect_err("refused");
        assert!(error.starts_with(reason), "{error}");
        assert_eq!(fs::read(&path).expect("the file"), content, "nothing on disk changed");
    }
}
