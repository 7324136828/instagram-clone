//! The folders the tests of both crates write in: `%TEMP%\instagram-windows-app-tests\<test>-<process id>`.
//!
//! Not part of the program: it is public only so that the tests of the app crate and the
//! integration tests can use it (as `poster::fixture` is).
//!
//! A test makes its folder with `TestFolder::new`, and the folder goes when the guard is dropped:
//! when the test ends, also when it fails (a panic unwinds through the guard). Without that,
//! every `cargo test --workspace`, and so every `build.bat`, left about 94 MB of profiles in the
//! user's temporary folder. `INSTAGRAM_WINDOWS_APP_KEEP_TEST_FILES=1` keeps them for a look, and
//! each kept folder is named on the error output.
//!
//! A test process that is killed, or that ran with that variable, leaves its folders behind. The
//! first folder a test process makes therefore also removes the folders of test processes that
//! no longer run (`remove_leftovers`), so such leftovers go with the next test run.

use std::io::Write as _;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::sync::Once;
use std::time::{Duration, Instant, SystemTime};

/// The folder below the system's temporary folder that holds the tests' folders.
pub const TESTS_FOLDER_NAME: &str = "instagram-windows-app-tests";

/// Set to `1`, the tests keep their folders, and the next test run keeps the earlier ones too.
pub const KEEP_TEST_FILES_VARIABLE: &str = "INSTAGRAM_WINDOWS_APP_KEEP_TEST_FILES";

/// How long removing a test's folder is tried. Media Foundation lets go of a file that failed to
/// load a moment after its player closed (video brief 6.7), and threads of the app a test ran may
/// still be closing their files.
const REMOVE_TRIES_FOR: Duration = Duration::from_secs(2);

/// The pause between two tries.
const REMOVE_RETRY_PAUSE: Duration = Duration::from_millis(50);

/// The folder that holds the tests' folders: `%TEMP%\instagram-windows-app-tests`.
pub fn tests_folder() -> PathBuf {
    std::env::temp_dir().join(TESTS_FOLDER_NAME)
}

/// True when `INSTAGRAM_WINDOWS_APP_KEEP_TEST_FILES` is `1`.
pub fn keep_requested() -> bool {
    std::env::var_os(KEEP_TEST_FILES_VARIABLE).is_some_and(|value| value == "1")
}

/// The folder of one test, `<test>-<process id>` in `tests_folder()`; removed when dropped.
#[derive(Debug)]
pub struct TestFolder {
    path: PathBuf,
}

impl TestFolder {
    /// A new, empty folder for the test `test`. A name must be used by one test of a test
    /// process at a time; the process id keeps test processes that run side by side apart.
    pub fn new(test: &str) -> std::io::Result<TestFolder> {
        let base = tests_folder();
        static SWEEP: Once = Once::new();
        SWEEP.call_once(|| {
            if !keep_requested() {
                remove_leftovers(&base, std::process::id(), owner_runs);
            }
        });
        let path = base.join(format!("{test}-{}", std::process::id()));
        // A folder of that name was left by an earlier process that had the same number.
        if path.exists() {
            remove_folder(&path)?;
        }
        std::fs::create_dir_all(&path)?;
        Ok(TestFolder { path })
    }

    /// Where it is.
    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Deref for TestFolder {
    type Target = Path;

    fn deref(&self) -> &Path {
        &self.path
    }
}

impl AsRef<Path> for TestFolder {
    fn as_ref(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestFolder {
    fn drop(&mut self) {
        // Written to the error output directly, not with `eprintln!`: the test harness shows what
        // a test prints only when it fails, and the place of kept files is wanted either way.
        if keep_requested() {
            let _ = writeln!(std::io::stderr(), "{KEEP_TEST_FILES_VARIABLE}=1: the files of this test are kept in {}", self.path.display());
        } else if let Err(error) = remove_folder(&self.path) {
            let _ = writeln!(std::io::stderr(), "The test folder {} could not be removed ({error}); the next test run removes it.", self.path.display());
        }
    }
}

/// Removes `path` with everything in it, trying again for `REMOVE_TRIES_FOR` while something
/// still holds a file in it. A folder that is not there counts as removed.
fn remove_folder(path: &Path) -> std::io::Result<()> {
    let started = Instant::now();
    loop {
        match std::fs::remove_dir_all(path) {
            Ok(()) => return Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(error) if started.elapsed() >= REMOVE_TRIES_FOR => return Err(error),
            Err(_) => std::thread::sleep(REMOVE_RETRY_PAUSE),
        }
    }
}

/// The process that made a test folder: the number after the last dash of `name`, when there is
/// a test name before it.
fn owner_of(name: &str) -> Option<u32> {
    let (test, number) = name.rsplit_once('-')?;
    if test.is_empty() || number.is_empty() || !number.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    number.parse().ok()
}

/// Removes the folders in `base` that were made by test processes that no longer run (not `own`,
/// and `owner_runs` says no for the number and the time the folder last changed), and returns
/// how many. Anything else is left alone: files, folders whose name ends in no process number,
/// and folders whose process still runs.
fn remove_leftovers(base: &Path, own: u32, mut owner_runs: impl FnMut(u32, Option<SystemTime>) -> bool) -> usize {
    let Ok(entries) = std::fs::read_dir(base) else {
        return 0;
    };
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let Some(owner) = name.to_str().and_then(owner_of) else {
            continue;
        };
        if owner == own || !entry.file_type().is_ok_and(|kind| kind.is_dir()) {
            continue;
        }
        let changed = entry.metadata().and_then(|metadata| metadata.modified()).ok();
        if owner_runs(owner, changed) {
            continue;
        }
        // Nobody holds files of a process that ended: one try is enough.
        if std::fs::remove_dir_all(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// Whether a process that started at `started` can be the one that made a folder that last
/// changed at `changed`. Windows gives the numbers of ended processes to new ones, so a process
/// with the folder's number that started after the folder last changed is another one. (The
/// time a folder changed, not when it was made: Windows can give a folder made again under the
/// same name the time the old one was made.) What is not known counts as the owner.
fn may_own(started: Option<SystemTime>, changed: Option<SystemTime>) -> bool {
    match (started, changed) {
        (Some(started), Some(changed)) => started <= changed,
        _ => true,
    }
}

/// True while the process that made a folder (number `pid`, the folder last changed at
/// `changed`) runs.
fn owner_runs(pid: u32, changed: Option<SystemTime>) -> bool {
    match running_since(pid) {
        Running::No => false,
        Running::Since(started) => may_own(started, changed),
    }
}

/// Whether a process runs, and since when.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Running {
    /// No process has that number, or it has ended.
    No,
    /// One runs; when it started, if that can be read. A process that cannot be looked at
    /// (another user's) counts as running, so that nothing of it is touched.
    Since(Option<SystemTime>),
}

/// Whether process `pid` runs, and since when.
#[cfg(windows)]
fn running_since(pid: u32) -> Running {
    use windows::Win32::Foundation::{CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, STILL_ACTIVE};
    use windows::Win32::System::Threading::{GetExitCodeProcess, GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    // SAFETY: plain calls; the handle is ours and closed before the end.
    let handle = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) } {
        Ok(handle) => handle,
        // "The parameter is incorrect": no process has that number.
        Err(error) if error.code() == ERROR_INVALID_PARAMETER.to_hresult() => return Running::No,
        Err(_) => return Running::Since(None),
    };
    let mut code = 0_u32;
    // SAFETY: `handle` is open and `code` a live u32 the call writes.
    let read = unsafe { GetExitCodeProcess(handle, &mut code) };
    let (mut created, mut exited, mut kernel, mut user) = (FILETIME::default(), FILETIME::default(), FILETIME::default(), FILETIME::default());
    // SAFETY: `handle` is open and the four times are live values the call writes.
    let times = unsafe { GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) };
    // SAFETY: the handle was opened above and is not used after this.
    unsafe {
        let _ = CloseHandle(handle);
    }
    // A process that ended is still there while someone holds a handle of it; it has its exit
    // code then.
    if read.is_ok() && code != STILL_ACTIVE.0 as u32 {
        return Running::No;
    }
    // FILETIME counts 100-nanosecond steps since 1601-01-01 (UTC).
    let started = times.ok().and_then(|()| {
        let steps = (u64::from(created.dwHighDateTime) << 32) | u64::from(created.dwLowDateTime);
        let since_1970 = steps.checked_sub(116_444_736_000_000_000)?;
        SystemTime::UNIX_EPOCH.checked_add(Duration::from_nanos(since_1970.saturating_mul(100)))
    });
    Running::Since(started)
}

/// Elsewhere there is no cheap way to tell; nothing is removed.
#[cfg(not(windows))]
fn running_since(_pid: u32) -> Running {
    Running::Since(None)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_owner_of_a_folder_is_the_number_after_its_last_dash() {
        assert_eq!(owner_of("about-12080"), Some(12080));
        assert_eq!(owner_of("web-sheet-cli-all-9"), Some(9));
        for name in ["about", "about-", "-12", "video-12a", "a-99999999999", "a-+5"] {
            assert_eq!(owner_of(name), None, "{name}");
        }
    }

    #[test]
    fn a_test_folder_starts_empty_and_goes_with_its_guard() {
        let left = TestFolder::new("test-folder-guard").expect("a test folder");
        let path = left.path().to_path_buf();
        assert!(path.starts_with(tests_folder()) && path.ends_with(format!("test-folder-guard-{}", std::process::id())));
        std::fs::create_dir_all(path.join("profile").join("data")).expect("a folder in it");
        std::fs::write(path.join("profile").join("data").join("app.log"), b"a line").expect("a file in it");
        // As if an earlier process with this number had left it: a new folder of that name
        // starts empty all the same.
        std::mem::forget(left);
        let folder = TestFolder::new("test-folder-guard").expect("the folder again");
        assert_eq!(folder.path(), path);
        assert_eq!(std::fs::read_dir(&path).map(|entries| entries.count()).ok(), Some(0));
        std::fs::write(path.join("notes.txt"), b"written by the test").expect("a file in it");
        drop(folder);
        // The variable keeps the files on purpose; then there is nothing to check here.
        if !keep_requested() {
            assert!(!path.exists(), "{} is gone", path.display());
        }
    }

    #[test]
    fn leftovers_of_test_processes_that_ended_are_removed_and_nothing_else() {
        let base = TestFolder::new("test-folder-sweep").expect("a test folder");
        let own = 4242;
        for name in ["about-111", "web-sheet-cli-222", "mine-4242", "alive-444", "notes", "video-12a"] {
            std::fs::create_dir_all(base.join(name).join("instagram-windows-app").join("data")).expect("a folder");
            std::fs::write(base.join(name).join("instagram-windows-app").join("data").join("state.json"), b"{}").expect("a file");
        }
        std::fs::write(base.join("readme-333"), b"a file, not a folder").expect("a file");
        let mut asked = std::collections::BTreeMap::new();
        let removed = remove_leftovers(&base, own, |pid, changed| {
            asked.insert(pid, changed.is_some());
            pid == 444
        });
        assert_eq!(removed, 2);
        let mut left: Vec<String> = std::fs::read_dir(&*base).expect("the folder").flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["alive-444", "mine-4242", "notes", "readme-333", "video-12a"]);
        assert_eq!(asked.into_iter().collect::<Vec<_>>(), [(111, true), (222, true), (444, true)], "asked about the others, with the time each folder changed");
        assert_eq!(remove_leftovers(&base.join("missing"), own, |_, _| false), 0, "no folder, nothing to do");
    }

    #[test]
    fn a_process_that_started_after_the_folder_last_changed_did_not_make_it() {
        let now = SystemTime::now();
        let earlier = now - Duration::from_secs(3600);
        assert!(may_own(Some(earlier), Some(now)), "started before: it may have made it");
        assert!(may_own(Some(now), Some(now)));
        assert!(!may_own(Some(now), Some(earlier)), "a newer process with the old number");
        assert!(may_own(None, Some(earlier)) && may_own(Some(now), None), "what is not known keeps the folder");
    }

    #[cfg(windows)]
    #[test]
    fn this_process_runs_and_one_that_ended_does_not() {
        let Running::Since(Some(started)) = running_since(std::process::id()) else {
            panic!("this process runs, since a known time");
        };
        assert!(started <= SystemTime::now());
        // A folder this process makes or changes is its own.
        let folder = TestFolder::new("test-folder-owner").expect("a test folder");
        let changed = std::fs::metadata(&*folder).and_then(|metadata| metadata.modified()).ok();
        assert!(owner_runs(std::process::id(), changed));
        assert!(!owner_runs(std::process::id(), Some(started - Duration::from_secs(60))), "a folder of an earlier process with this number");
        let system = std::env::var_os("SystemRoot").map_or_else(|| PathBuf::from("C:\\Windows"), PathBuf::from);
        let mut child = std::process::Command::new(system.join("System32").join("cmd.exe")).args(["/C", "exit 0"]).spawn().expect("cmd starts");
        let pid = child.id();
        child.wait().expect("cmd ends");
        // The child's handle is still open here: the process has ended but is still known.
        assert_eq!(running_since(pid), Running::No, "an ended process whose handle is held");
        drop(child);
    }
}
