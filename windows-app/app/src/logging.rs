//! The log file `app.log` in the data folder, and the panic hook.
//!
//! A release build has no console (it is a GUI-subsystem program), so anything written to stderr
//! is lost when the app is started from Explorer. Warnings, errors and a panic therefore go to a
//! file the user can send along with a problem report. Debug builds also print to stderr.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use instagram_core::text;

/// A log file larger than this is set aside as `app.log.old` at the start, so that it cannot
/// grow without bound.
const MAX_LOG_BYTES: u64 = 1024 * 1024;
/// Setting this environment variable to `debug` also logs debug messages of the app.
const LOG_LEVEL_VARIABLE: &str = "INSTAGRAM_WINDOWS_APP_LOG";

/// Writes log records to the file and, in debug builds, to stderr.
struct AppLogger {
    file: Mutex<Option<File>>,
    to_stderr: bool,
    app_level: log::LevelFilter,
}

impl AppLogger {
    /// The app's own crates log at `app_level`; libraries only warnings and errors, they are
    /// chatty about things the user cannot act on.
    fn level_for(&self, target: &str) -> log::LevelFilter {
        if target.starts_with("instagram_windows_app") || target.starts_with("instagram_core") || target.starts_with("instagram-windows-app") {
            self.app_level
        } else {
            log::LevelFilter::Warn
        }
    }
}

/// "2026-11-12 14:03:07.512" in local time.
fn timestamp() -> String {
    let now = text::now_ms();
    let local = text::civil_from_epoch_ms(now.saturating_add(i64::from(text::local_offset_minutes(now)) * 60_000));
    format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}.{:03}", local.year, local.month, local.day, local.hour, local.minute, local.second, local.millisecond)
}

impl log::Log for AppLogger {
    fn enabled(&self, metadata: &log::Metadata<'_>) -> bool {
        metadata.level() <= self.level_for(metadata.target())
    }

    fn log(&self, record: &log::Record<'_>) {
        if !self.enabled(record.metadata()) {
            return;
        }
        let line = format!("{} {:<5} {}: {}\n", timestamp(), record.level(), record.target(), record.args());
        if self.to_stderr {
            eprint!("{line}");
        }
        if let Ok(mut file) = self.file.lock() {
            if let Some(file) = file.as_mut() {
                // A full disk must not take the app down; the record is dropped.
                let _ = file.write_all(line.as_bytes());
            }
        }
    }

    fn flush(&self) {
        if let Ok(mut file) = self.file.lock() {
            if let Some(file) = file.as_mut() {
                let _ = file.flush();
            }
        }
    }
}

/// Opens the log file for appending, setting a large old one aside first.
fn open_log_file(path: &Path) -> std::io::Result<File> {
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder)?;
    }
    if std::fs::metadata(path).map(|metadata| metadata.len() > MAX_LOG_BYTES).unwrap_or(false) {
        let mut old = path.as_os_str().to_owned();
        old.push(".old");
        let old = PathBuf::from(old);
        let _ = std::fs::remove_file(&old);
        std::fs::rename(path, &old)?;
    }
    OpenOptions::new().create(true).append(true).open(path)
}

/// Starts logging to `log_file` (and to stderr when `to_stderr`), and installs the panic hook.
/// Returns the reason when the file cannot be opened; logging then goes to stderr only.
pub fn init(log_file: &Path, to_stderr: bool) -> Result<(), String> {
    let opened = open_log_file(log_file);
    let failure = opened.as_ref().err().map(|error| format!("The log file {} could not be opened: {error}", log_file.display()));
    let app_level = match std::env::var(LOG_LEVEL_VARIABLE).as_deref() {
        Ok("debug") => log::LevelFilter::Debug,
        _ => log::LevelFilter::Info,
    };
    let logger = AppLogger { file: Mutex::new(opened.ok()), to_stderr, app_level };
    // The logger lives as long as the process.
    let logger: &'static AppLogger = Box::leak(Box::new(logger));
    if log::set_logger(logger).is_ok() {
        log::set_max_level(log::LevelFilter::Debug);
    }
    install_panic_hook();
    match failure {
        Some(message) => Err(message),
        None => Ok(()),
    }
}

/// Logs a panic with its location before the default hook prints it.
///
/// A panic is not treated as fatal here: a third-party picture decoder that panics on a damaged
/// file is caught inside the core crate and becomes an ordinary "could not be shown" message,
/// but this hook still runs first.
fn install_panic_hook() {
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let location = info.location().map(|location| format!("{}:{}", location.file(), location.line())).unwrap_or_else(|| "an unknown place".to_owned());
        let payload = info.payload();
        let message = payload.downcast_ref::<&str>().map(|text| (*text).to_owned()).or_else(|| payload.downcast_ref::<String>().cloned()).unwrap_or_else(|| "no message".to_owned());
        let thread = std::thread::current();
        log::error!("Panic in thread \"{}\" at {location}: {message}", thread.name().unwrap_or("unnamed"));
        log::logger().flush();
        previous(info);
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_timestamp_has_the_documented_shape() {
        let stamp = timestamp();
        assert_eq!(stamp.len(), 23, "{stamp}");
        let bytes = stamp.as_bytes();
        assert_eq!((bytes[4], bytes[7], bytes[10], bytes[13], bytes[16], bytes[19]), (b'-', b'-', b' ', b':', b':', b'.'));
    }

    #[test]
    fn a_large_log_is_set_aside() {
        let folder = instagram_core::test_folder::TestFolder::new("log-test").expect("a test folder");
        let path = folder.join("data").join("app.log");
        // The folder is created on demand.
        drop(open_log_file(&path).expect("first open"));
        std::fs::write(&path, vec![b'x'; MAX_LOG_BYTES as usize + 1]).expect("write");
        let mut file = open_log_file(&path).expect("second open");
        file.write_all(b"fresh\n").expect("append");
        drop(file);
        assert_eq!(std::fs::read(&path).expect("read"), b"fresh\n");
        assert_eq!(std::fs::metadata(folder.join("data").join("app.log.old")).expect("old").len(), MAX_LOG_BYTES + 1);
    }
}
