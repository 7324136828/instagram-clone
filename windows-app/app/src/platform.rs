//! The few things the app asks Windows for outside the window: opening a link in the browser,
//! showing a file or a folder in Explorer, the folder a download is offered to go to, the
//! priority of its background threads, and the console that `--help` and `--version` print to
//! (the last two are the only `unsafe` of this crate).
//!
//! Links come from an opened export. The core crate only keeps links that passed its
//! `sanitize_link` (http or https, normalised, awkward characters percent-encoded), so they are
//! safe to hand to the shell; the scheme is checked here once more because this is the last stop.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Raised when a link could not be handed to the browser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlatformError {
    /// A complete sentence for the user.
    pub message: String,
}

impl std::fmt::Display for PlatformError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PlatformError {}

/// True for a link the app is willing to open: http or https, without white space or quotes.
pub fn is_openable_link(url: &str) -> bool {
    let lower = url.to_ascii_lowercase();
    (lower.starts_with("https://") || lower.starts_with("http://")) && !url.chars().any(|c| c.is_whitespace() || c.is_control() || c == '"')
}

/// The system's `rundll32.exe`, by its full path so that nothing else of that name is started.
fn rundll32() -> PathBuf {
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    PathBuf::from(root).join("System32").join("rundll32.exe")
}

/// Opens `url` in the default browser.
pub fn open_link(url: &str) -> Result<(), PlatformError> {
    if !is_openable_link(url) {
        return Err(PlatformError { message: format!("\"{url}\" is not a link this app opens.") });
    }
    // The classic way to open a URL with its registered handler without a console window.
    Command::new(rundll32())
        .arg("url.dll,FileProtocolHandler")
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|error| PlatformError { message: format!("The link could not be opened: {error}") })
}

/// The path as Explorer understands it: without the `\\?\` prefix that long paths carry inside
/// the app (`\\?\C:\a` becomes `C:\a`, `\\?\UNC\server\share` becomes `\\server\share`).
pub fn explorer_path(file: &Path) -> String {
    let text = file.display().to_string();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{rest}")
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        rest.to_owned()
    } else {
        text
    }
}

/// The argument that makes Explorer open the folder of `file` with the file selected. Explorer
/// reads its command line itself: the path follows the comma directly and stands in quotes, so
/// that spaces and commas in it are taken literally. A path with a quote (no Windows file name
/// has one) is refused.
pub fn explorer_select_argument(file: &Path) -> Option<String> {
    let path = explorer_path(file);
    if path.is_empty() || path.contains('"') {
        return None;
    }
    Some(format!("/select,\"{path}\""))
}

/// Opens the folder of `file` in Explorer with the file selected ("Show in folder").
pub fn show_in_folder(file: &Path) -> Result<(), PlatformError> {
    let name = instagram_core::messages::display_name(file);
    if !file.exists() {
        return Err(PlatformError { message: format!("\"{name}\" cannot be shown in its folder: the file is no longer there.") });
    }
    let Some(argument) = explorer_select_argument(file) else {
        return Err(PlatformError { message: format!("\"{name}\" cannot be shown in its folder: its path cannot be passed to Explorer.") });
    };
    log::info!("Explorer is started with {argument}");
    spawn_explorer(&argument).map_err(|error| PlatformError { message: format!("\"{name}\" could not be shown in its folder: {error}") })
}

/// Opens `folder` in Explorer ("Open data folder").
pub fn open_folder(folder: &Path) -> Result<(), PlatformError> {
    let path = explorer_path(folder);
    if !folder.is_dir() {
        return Err(PlatformError { message: format!("\"{path}\" cannot be opened: the folder is not there.") });
    }
    if path.contains('"') {
        return Err(PlatformError { message: format!("\"{path}\" cannot be opened: its path cannot be passed to Explorer.") });
    }
    log::info!("Explorer is started with \"{path}\"");
    spawn_explorer(&format!("\"{path}\"")).map_err(|error| PlatformError { message: format!("\"{path}\" could not be opened: {error}") })
}

/// Where a Save dialog starts when no download was saved before: the user's Downloads folder,
/// or Documents when there is none.
pub fn downloads_folder() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("USERPROFILE")?);
    ["Downloads", "Documents"].iter().map(|name| home.join(name)).find(|folder| folder.is_dir())
}

/// Where an Open dialog starts when nothing was opened before: the user's Pictures folder, as an
/// image viewer's dialog does.
pub fn pictures_folder() -> Option<PathBuf> {
    first_folder_in(Path::new(&std::env::var_os("USERPROFILE")?), &["Pictures"])
}

/// The first of `names` that is a folder in `home`.
pub fn first_folder_in(home: &Path, names: &[&str]) -> Option<PathBuf> {
    names.iter().map(|name| home.join(name)).find(|folder| folder.is_dir())
}

/// The double-click time of the mouse settings in milliseconds (Windows' default is 500).
pub fn double_click_ms() -> u32 {
    #[cfg(windows)]
    {
        // SAFETY: `GetDoubleClickTime` takes nothing and returns a number.
        unsafe { windows::Win32::UI::Input::KeyboardAndMouse::GetDoubleClickTime() }
    }
    #[cfg(not(windows))]
    {
        500
    }
}

/// How many lines one notch of the mouse wheel scrolls (the mouse settings; 3 by default). The
/// setting "one screen at a time" and a value that cannot be read count as the default.
pub fn wheel_scroll_lines() -> u32 {
    const DEFAULT_LINES: u32 = 3;
    #[cfg(windows)]
    {
        use windows::Win32::UI::WindowsAndMessaging::{SystemParametersInfoW, SPI_GETWHEELSCROLLLINES, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS};
        let mut lines: u32 = DEFAULT_LINES;
        // SAFETY: SPI_GETWHEELSCROLLLINES writes one UINT to the pointer, which points at `lines`.
        let read = unsafe { SystemParametersInfoW(SPI_GETWHEELSCROLLLINES, 0, Some((&raw mut lines).cast()), SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0)) };
        // WHEEL_PAGESCROLL (u32::MAX) asks for a page per notch; anything absurd is not a line count.
        match read {
            Ok(()) if (1..=100).contains(&lines) => lines,
            _ => DEFAULT_LINES,
        }
    }
    #[cfg(not(windows))]
    {
        DEFAULT_LINES
    }
}

/// The work area (the screen without the taskbar) of the screen at `near_px` (physical pixels,
/// for example the stored position of the window), or of the main screen, with its scale and
/// the frame a normal window has there. `None` when Windows does not say.
pub fn work_area(near_px: Option<(i32, i32)>) -> Option<crate::placement::WorkArea> {
    #[cfg(windows)]
    {
        use windows::Win32::Foundation::{POINT, RECT};
        use windows::Win32::Graphics::Gdi::{GetMonitorInfoW, MonitorFromPoint, MONITORINFO, MONITOR_DEFAULTTOPRIMARY};
        use windows::Win32::UI::HiDpi::{AdjustWindowRectExForDpi, GetDpiForMonitor, SetThreadDpiAwarenessContext, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, MDT_EFFECTIVE_DPI};
        use windows::Win32::UI::WindowsAndMessaging::{WINDOW_EX_STYLE, WS_OVERLAPPEDWINDOW};

        let (x, y) = near_px.unwrap_or((0, 0));
        // The window will be per-monitor DPI aware; so is this thread for the questions below,
        // so that every answer is in physical pixels, whatever the process was started as.
        // SAFETY: the calls get valid values and pointers to locals; the previous awareness of
        // the thread is put back before returning.
        unsafe {
            let previous = SetThreadDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
            let monitor = MonitorFromPoint(POINT { x, y }, MONITOR_DEFAULTTOPRIMARY);
            let mut info = MONITORINFO { cbSize: std::mem::size_of::<MONITORINFO>() as u32, ..Default::default() };
            let has_info = GetMonitorInfoW(monitor, &raw mut info).as_bool();
            let (mut dpi_x, mut dpi_y) = (0u32, 0u32);
            let has_dpi = GetDpiForMonitor(monitor, MDT_EFFECTIVE_DPI, &raw mut dpi_x, &raw mut dpi_y).is_ok() && dpi_x > 0;
            let mut frame = RECT::default();
            let has_frame = has_dpi && AdjustWindowRectExForDpi(&raw mut frame, WS_OVERLAPPEDWINDOW, false, WINDOW_EX_STYLE(0), dpi_x).is_ok();
            if !previous.is_invalid() {
                SetThreadDpiAwarenessContext(previous);
            }
            if !(has_info && has_frame) {
                return None;
            }
            let work = info.rcWork;
            Some(crate::placement::WorkArea {
                rect_px: [work.left as f32, work.top as f32, work.right as f32, work.bottom as f32],
                scale: dpi_x as f32 / 96.0,
                frame_px: [-frame.left as f32, -frame.top as f32, frame.right as f32, frame.bottom as f32],
            })
        }
    }
    #[cfg(not(windows))]
    {
        let _ = near_px;
        None
    }
}

/// Starts the system's Explorer with one argument, passed exactly as written.
#[cfg(windows)]
fn spawn_explorer(argument: &str) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt as _;
    let root = std::env::var_os("SystemRoot").unwrap_or_else(|| "C:\\Windows".into());
    // By its full path, so that nothing else of that name is started.
    Command::new(PathBuf::from(root).join("explorer.exe")).raw_arg(argument).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().map(|_| ())
}

/// There is no Explorer to start on another system.
#[cfg(not(windows))]
fn spawn_explorer(_argument: &str) -> std::io::Result<()> {
    Err(std::io::Error::new(std::io::ErrorKind::Unsupported, "this needs Windows Explorer"))
}

/// Puts the calling thread one step below normal priority.
///
/// The picture decoders call this. On a machine with few cores a decoder otherwise competes
/// with the UI thread as an equal: measured on two cores, the UI thread lost a whole scheduler
/// quantum (about 15 ms, a visible hitch while scrolling) whenever it woke a decoder. Below
/// normal, a decoder still gets every idle core but yields as soon as the UI thread has work.
pub fn run_below_normal_priority() {
    #[cfg(windows)]
    {
        use windows::Win32::System::Threading::{GetCurrentThread, SetThreadPriority, THREAD_PRIORITY_BELOW_NORMAL};
        // SAFETY: `GetCurrentThread` returns the pseudo handle of the calling thread, which is
        // always valid and needs no closing; `SetThreadPriority` only reads it.
        let result = unsafe { SetThreadPriority(GetCurrentThread(), THREAD_PRIORITY_BELOW_NORMAL) };
        if let Err(error) = result {
            log::warn!("The priority of a decoder thread could not be lowered: {error}");
        }
    }
}

/// Prints what `--help` or `--version` asked for.
///
/// A debug build is a console program, and a run whose output is redirected has a pipe or a
/// file as its standard output: printing is all it takes. A release build started from a
/// console window has no standard output at all, because Windows gives a windowed program
/// none. It then borrows the console of the program that started it for these lines. Started
/// from Explorer there is no console to borrow and nothing is shown.
pub fn print_information(text: &str) {
    use std::io::Write as _;
    #[cfg(windows)]
    {
        if !has_standard_output() {
            if let Some(mut console) = parent_console() {
                // The command prompt does not wait for a windowed program and has already
                // printed its prompt: the text starts on a line of its own.
                let _ = console.write_all(format!("\n{text}").as_bytes());
            }
            return;
        }
    }
    let mut output = std::io::stdout();
    let _ = output.write_all(text.as_bytes());
    let _ = output.flush();
}

/// True when the process was started with a standard output: a console, a pipe or a file.
#[cfg(windows)]
fn has_standard_output() -> bool {
    use windows::Win32::System::Console::{GetStdHandle, STD_OUTPUT_HANDLE};
    // SAFETY: `GetStdHandle` only reads the standard handle of this process.
    match unsafe { GetStdHandle(STD_OUTPUT_HANDLE) } {
        Ok(handle) => !handle.0.is_null(),
        Err(_) => false,
    }
}

/// The console of the program that started this one, opened for writing, when it has one.
#[cfg(windows)]
fn parent_console() -> Option<std::fs::File> {
    use windows::Win32::System::Console::{AttachConsole, ATTACH_PARENT_PROCESS};
    // SAFETY: `AttachConsole` takes no pointers; it fails without side effects when the parent
    // has no console or this process already has one.
    unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.ok()?;
    // "CONOUT$" names the screen of the console this process is attached to.
    std::fs::OpenOptions::new().write(true).open("CONOUT$").ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_plain_web_links_are_opened() {
        assert!(is_openable_link("https://www.instagram.com/p/FXTbasic001/"));
        assert!(is_openable_link("http://example.com/a?b=c&d=e"));
        assert!(!is_openable_link("file:///C:/Windows/System32/calc.exe"));
        assert!(!is_openable_link("javascript:alert(1)"));
        assert!(!is_openable_link("https://example.com/ a"));
        assert!(!is_openable_link("https://example.com/\" & calc"));
        assert!(!is_openable_link(""));
        assert!(open_link("calc.exe").is_err());
    }

    #[test]
    fn explorer_gets_the_path_in_quotes_and_without_the_long_path_prefix() {
        assert_eq!(explorer_select_argument(Path::new(r"C:\Pictures\Sunset at the pier (final), 2.webp")).as_deref(), Some(r#"/select,"C:\Pictures\Sunset at the pier (final), 2.webp""#));
        assert_eq!(explorer_path(Path::new(r"\\?\C:\very\long\path\a.jpg")), r"C:\very\long\path\a.jpg");
        assert_eq!(explorer_path(Path::new(r"\\?\UNC\server\share\a.jpg")), r"\\server\share\a.jpg");
        assert_eq!(explorer_path(Path::new(r"\\server\share\a.jpg")), r"\\server\share\a.jpg");
        assert_eq!(explorer_select_argument(Path::new("")), None);
        assert_eq!(explorer_select_argument(Path::new("a\"b.jpg")), None);
    }

    #[test]
    fn a_file_that_is_gone_is_reported_and_explorer_is_not_started() {
        let missing = std::env::temp_dir().join("instagram-windows-app-tests").join("no-such-picture.jpg");
        let error = show_in_folder(&missing).expect_err("the file does not exist");
        assert_eq!(error.message, "\"no-such-picture.jpg\" cannot be shown in its folder: the file is no longer there.");
    }
}
