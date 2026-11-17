//! The keyboard shortcuts and the command-line options, as tables.
//!
//! The keys themselves are handled where they act (`app.rs`, `feed.rs`, `explore.rs`,
//! `viewer.rs`, `editor.rs`, `autoview.rs`, `widgets.rs`). These tables are what the app tells
//! people about them: `--help` prints them, and the documentation test (`tests/docs.rs`)
//! compares the tables of the README with them row by row, tries every option on the real
//! command-line parser and looks for every key in the module that handles it. A key that is
//! added, renamed or removed therefore has to be changed here, and the README follows.

use std::ffi::OsString;

use instagram_core::settings::APP_NAME;

use crate::strings;

/// One keyboard shortcut.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Shortcut {
    /// Where the key acts: a view or a dialog, by the name the interface gives it.
    pub scope: &'static str,
    /// The key or keys as they are written for people: `Ctrl+O`, `Left` / `Right`.
    pub keys: &'static str,
    /// What happens, as a short phrase without a period.
    pub action: &'static str,
}

/// The scope of the keys that work in every view.
pub const SCOPE_ANYWHERE: &str = "Anywhere";
/// The scope of the keys of the home feed and the Explore grid.
pub const SCOPE_FEED: &str = "Home and Explore";
/// The scope of the keys of the viewer (the opened post).
pub const SCOPE_VIEWER: &str = "Viewer";
/// The scope of the keys of the "Edit description" dialog.
pub const SCOPE_EDITOR: &str = strings::EDIT_TITLE;
/// The scope of the keys of the setup dialog of auto view.
pub const SCOPE_AUTO_VIEW_SETUP: &str = "Auto view setup";
/// The scope of the keys of the running auto view.
pub const SCOPE_AUTO_VIEW: &str = strings::AUTO_VIEW_TITLE;
/// The scope of the keys every dialog, sheet and menu shares.
pub const SCOPE_DIALOGS: &str = "Dialogs and menus";

/// Every keyboard shortcut of the app, in the order the documentation lists them.
pub const SHORTCUTS: &[Shortcut] = &[
    Shortcut { scope: SCOPE_ANYWHERE, keys: "Ctrl+O", action: "Open pictures" },
    Shortcut { scope: SCOPE_ANYWHERE, keys: "Ctrl+Shift+O", action: "Open folder" },
    Shortcut { scope: SCOPE_ANYWHERE, keys: "F11", action: "Fullscreen on and off" },
    Shortcut { scope: SCOPE_ANYWHERE, keys: "Escape", action: "Close what is on top; with nothing open, leave fullscreen" },
    Shortcut { scope: SCOPE_FEED, keys: "Home / End", action: "Jump to the top or the bottom" },
    Shortcut { scope: SCOPE_FEED, keys: "Page Up / Page Down", action: "Scroll by one screen" },
    Shortcut { scope: SCOPE_FEED, keys: "Up / Down", action: "Scroll by a small step" },
    Shortcut { scope: SCOPE_VIEWER, keys: "Left / Right", action: "Previous or next slide of a carousel, then the previous or next picture" },
    Shortcut { scope: SCOPE_VIEWER, keys: "Home / End", action: "First or last picture" },
    Shortcut { scope: SCOPE_VIEWER, keys: "+ / -", action: "Zoom in or out" },
    Shortcut { scope: SCOPE_VIEWER, keys: "0", action: "Fit the picture to the window" },
    Shortcut { scope: SCOPE_VIEWER, keys: "Space / K", action: "Play or pause the video" },
    Shortcut { scope: SCOPE_VIEWER, keys: "M", action: "Sound of videos on or off" },
    Shortcut { scope: SCOPE_VIEWER, keys: "Shift+Left / Shift+Right", action: "Back or forward 5 seconds in the video" },
    Shortcut { scope: SCOPE_VIEWER, keys: "Escape", action: "Leave fullscreen, then close the viewer" },
    Shortcut { scope: SCOPE_EDITOR, keys: "Ctrl+Enter", action: "Save" },
    Shortcut { scope: SCOPE_EDITOR, keys: "Enter", action: "New line" },
    Shortcut { scope: SCOPE_EDITOR, keys: "Escape", action: "Close without saving" },
    Shortcut { scope: SCOPE_AUTO_VIEW_SETUP, keys: "Enter", action: "Start (while the seconds field has the focus)" },
    Shortcut { scope: SCOPE_AUTO_VIEW, keys: "Space", action: "Pause and resume; on the end screen, start again" },
    Shortcut { scope: SCOPE_AUTO_VIEW, keys: "Left / Right", action: "Previous or next post" },
    Shortcut { scope: SCOPE_AUTO_VIEW, keys: "+ / -", action: "More or less time per post" },
    Shortcut { scope: SCOPE_AUTO_VIEW, keys: "M", action: "Sound of videos on or off" },
    Shortcut { scope: SCOPE_AUTO_VIEW, keys: "Escape", action: "Exit auto view" },
    Shortcut { scope: SCOPE_DIALOGS, keys: "Escape", action: "Close the dialog, sheet or menu on top" },
    Shortcut { scope: SCOPE_DIALOGS, keys: "Tab / Shift+Tab", action: "Move the focus to the next or previous control" },
    Shortcut { scope: SCOPE_DIALOGS, keys: "Enter or Space", action: "Press the focused button" },
];

/// One command-line option.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CommandLineOption {
    /// The option as it is written in the documentation: `--autoview[=SECONDS]`.
    pub syntax: &'static str,
    /// What it does, as a short phrase without a period.
    pub purpose: &'static str,
    /// Arguments that use the option in a valid way; the documentation test parses them.
    pub example: &'static [&'static str],
}

/// The option that prints `help_text` and ends the program.
pub const OPTION_HELP: &str = "--help";
/// The option that prints `version_line` and ends the program.
pub const OPTION_VERSION: &str = "--version";

/// The options for everyday use. Everything that is not an option is a picture or a folder.
pub const OPTIONS: &[CommandLineOption] = &[
    CommandLineOption { syntax: "--autoview[=SECONDS]", purpose: "Start auto view as soon as the pictures are open, with SECONDS (2 to 3600) per post or the remembered time", example: &["--autoview=30"] },
    CommandLineOption { syntax: "--loop=0|1", purpose: "With --autoview: start again after the last post (1) or stop there (0)", example: &["--autoview", "--loop=0"] },
    CommandLineOption { syntax: "--shuffle=0|1", purpose: "With --autoview: show the pictures in a random order (1) or in their own (0)", example: &["--autoview", "--shuffle=1"] },
    CommandLineOption { syntax: "--wait=0|1", purpose: "With --autoview: let a video longer than the time per post play to its end (1) or not (0)", example: &["--autoview", "--wait=0"] },
    CommandLineOption { syntax: OPTION_HELP, purpose: "Print the options and the keyboard shortcuts, then exit", example: &[OPTION_HELP] },
    CommandLineOption { syntax: OPTION_VERSION, purpose: "Print the name and the version, then exit", example: &[OPTION_VERSION] },
    CommandLineOption { syntax: "--no-web", purpose: "Do not start the browser pages in this run", example: &["--no-web"] },
    CommandLineOption { syntax: "--host ADDRESS", purpose: "Listen on this IPv4 address in this run (0.0.0.0 for every network adapter) instead of 127.0.0.1", example: &["--host", "127.0.0.1"] },
    CommandLineOption { syntax: "--frontend-port PORT", purpose: "The first port tried for the page address in this run (3127 by default); --frontend is the same", example: &["--frontend-port", "3127"] },
    CommandLineOption { syntax: "--backend-port PORT", purpose: "The first port tried for the API address in this run (8317 by default); --backend is the same", example: &["--backend-port", "8317"] },
];

/// The options for development, tests and the build (see `harness.rs`).
pub const DEVELOPMENT_OPTIONS: &[CommandLineOption] = &[
    CommandLineOption { syntax: "--profile-dir <folder>", purpose: "Use <folder> in place of %APPDATA% for settings.json and the data folder, and its subfolders Local and Temp in place of %LOCALAPPDATA% (the archive cache) and %TEMP%", example: &["--profile-dir", "profile"] },
    CommandLineOption { syntax: "--screenshot <png>", purpose: "Render with a hidden window, save the picture of the window as a PNG and exit", example: &["--screenshot", "shot.png"] },
    CommandLineOption { syntax: "--size <W>x<H>", purpose: "The inner size of the window in points (1440x900 with --screenshot)", example: &["--size", "1100x800"] },
    CommandLineOption { syntax: "--scale <factor>", purpose: "Pixels per point, from 0.25 to 8 (the display's scale otherwise)", example: &["--scale", "1.0"] },
    CommandLineOption { syntax: "--theme light|dark", purpose: "Start in this appearance, as if Switch appearance had chosen it", example: &["--theme", "dark"] },
    CommandLineOption { syntax: "--scenario <step>[,<step>...]", purpose: "Put the interface into a state after opening; an unknown step prints the list of steps", example: &["--scenario", "explore,viewer:0"] },
    CommandLineOption { syntax: "--watchdog <seconds>", purpose: "End the run after this long (a --screenshot run gives up after 120 by default)", example: &["--watchdog", "60"] },
    CommandLineOption { syntax: "--frame-log", purpose: "Write frame counts and timings to app.log", example: &["--frame-log"] },
    CommandLineOption { syntax: "--animate", purpose: "Keep animations running in a --screenshot run", example: &["--animate"] },
];

/// What `--version` prints: the name of the program and the version of this build.
pub fn version_line() -> String {
    format!("{APP_NAME} {}", env!("CARGO_PKG_VERSION"))
}

/// The syntax of `option` without its value: `--autoview[=SECONDS]` is `--autoview`.
pub fn option_name(option: &CommandLineOption) -> &'static str {
    let end = option.syntax.find(['[', '=', ' ']).unwrap_or(option.syntax.len());
    &option.syntax[..end]
}

/// The width `help_text` is laid out for: one column less than a console window has by default.
pub const HELP_COLUMNS: usize = 79;

/// One entry of the help: `name` in a column of its own, `explanation` beside it, wrapped at
/// `HELP_COLUMNS` with the following lines indented to the explanation.
fn help_entry(indent: usize, name: &str, name_column: usize, explanation: &str) -> String {
    let start = indent + name_column;
    let mut entry = format!("{:indent$}{name:<name_column$}", "");
    if name.len() >= name_column {
        // A name wider than its column: the explanation starts on the next line.
        entry.push_str(&format!("\n{:start$}", ""));
    }
    let mut column = start;
    for word in explanation.split(' ') {
        if column > start && column + 1 + word.len() > HELP_COLUMNS {
            entry.push_str(&format!("\n{:start$}", ""));
            column = start;
        }
        if column > start {
            entry.push(' ');
            column += 1;
        }
        entry.push_str(word);
        column += word.len();
    }
    entry.push('\n');
    entry
}

/// What `--help` prints: how to start the program, its options and its keyboard shortcuts.
pub fn help_text() -> String {
    const OPTION_COLUMN: usize = 31;
    const KEY_COLUMN: usize = 21;
    let mut text = format!("{}\n{}\n\n", version_line(), strings::ABOUT_SUMMARY);
    text.push_str(&format!("Usage: {APP_NAME} [options] [pictures or folders...]\n\n"));
    text.push_str("A picture opens with the other pictures of its folder, several pictures open\non their own, a folder opens the pictures directly in it, and a folder with a\npost.json (or that file) opens as an Instagram export. Export archives (.zip,\n.tar, .tar.gz, .tgz) open too, and several exports and archives are merged.\nVideos (MP4, MOV, WebM) open like pictures and play in the viewer and auto\nview, and in the feed with a graphics card.\nWhat is open can also be seen in a web browser: http://127.0.0.1:3127/ by\ndefault (More > Open in browser; More > Browser view for the ports).\n\nOptions:\n");
    let list = |options: &[CommandLineOption]| -> String { options.iter().map(|option| help_entry(2, option.syntax, OPTION_COLUMN, option.purpose)).collect() };
    text.push_str(&list(OPTIONS));
    text.push_str("\nOptions for development and automated runs:\n");
    text.push_str(&list(DEVELOPMENT_OPTIONS));
    text.push_str("\nKeyboard:\n");
    let mut scope = "";
    for shortcut in SHORTCUTS {
        if shortcut.scope != scope {
            scope = shortcut.scope;
            text.push_str(&format!("  {scope}\n"));
        }
        text.push_str(&help_entry(4, shortcut.keys, KEY_COLUMN, shortcut.action));
    }
    text.push_str(&format!("\nSettings: %APPDATA%\\{APP_NAME}\\settings.json\n"));
    text
}

/// What to print when the command line only asks for information: the help for `--help`, the
/// version for `--version` (the help wins when both are given). `None` for every other command
/// line. It is looked at before anything else, so that neither option reads the settings or
/// opens a window.
pub fn information(arguments: &[OsString]) -> Option<String> {
    let has = |name: &str| arguments.iter().any(|argument| argument == name);
    if has(OPTION_HELP) {
        Some(help_text())
    } else if has(OPTION_VERSION) {
        Some(format!("{}\n", version_line()))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn the_version_names_the_program_and_this_build() {
        assert_eq!(version_line(), format!("instagram-windows-app {}", env!("CARGO_PKG_VERSION")));
        assert_eq!(information(&arguments(&["--version"])), Some(format!("{}\n", version_line())));
        assert_eq!(information(&arguments(&["pictures", "--version"])), Some(format!("{}\n", version_line())));
    }

    #[test]
    fn the_help_lists_every_option_and_every_shortcut() {
        let help = information(&arguments(&["--help", "--version"])).expect("help");
        assert!(help.starts_with(&version_line()));
        // Long explanations are wrapped: compare without regard to where the lines break.
        let unwrapped = help.split_whitespace().collect::<Vec<_>>().join(" ");
        for option in OPTIONS.iter().chain(DEVELOPMENT_OPTIONS) {
            assert!(unwrapped.contains(&format!("{} {}", option.syntax, option.purpose)), "{} is missing from the help", option.syntax);
        }
        for shortcut in SHORTCUTS {
            assert!(unwrapped.contains(&format!("{} {}", shortcut.keys, shortcut.action)), "{} is missing from the help", shortcut.keys);
        }
        assert!(help.is_ascii(), "the help is printed to a console of any code page");
        for line in help.lines() {
            assert!(line.len() <= HELP_COLUMNS, "this line of the help is wider than a console window: {line}");
            assert_eq!(line, line.trim_end(), "this line of the help ends with a space");
        }
    }

    #[test]
    fn a_long_name_puts_its_explanation_on_the_next_line() {
        let entry = help_entry(4, "Shift+Left / Shift+Right", 21, "Back");
        assert_eq!(entry, format!("    Shift+Left / Shift+Right\n{:25}Back\n", ""));
    }

    #[test]
    fn long_explanations_wrap_under_their_first_word() {
        let entry = help_entry(2, "--name", 10, "word ".repeat(20).trim_end());
        let lines: Vec<&str> = entry.lines().collect();
        assert!(lines.len() > 1);
        assert!(lines[0].starts_with("  --name    word word"));
        assert!(lines[1].starts_with("            word"));
        assert!(lines.iter().all(|line| line.len() <= HELP_COLUMNS));
    }

    #[test]
    fn other_command_lines_ask_for_no_information() {
        assert_eq!(information(&arguments(&[])), None);
        assert_eq!(information(&arguments(&["C:\\Pictures\\a.jpg", "--autoview=5"])), None);
        // Only the exact option counts: a file may be called anything.
        assert_eq!(information(&arguments(&["--versions", "help", "-h"])), None);
    }

    #[test]
    fn option_names_stop_before_the_value() {
        let names: Vec<&str> = OPTIONS.iter().chain(DEVELOPMENT_OPTIONS).map(option_name).collect();
        assert_eq!(names, ["--autoview", "--loop", "--shuffle", "--wait", "--help", "--version", "--no-web", "--host", "--frontend-port", "--backend-port", "--profile-dir", "--screenshot", "--size", "--scale", "--theme", "--scenario", "--watchdog", "--frame-log", "--animate"]);
    }
}
