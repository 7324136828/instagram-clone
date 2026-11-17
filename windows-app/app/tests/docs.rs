//! The documentation is part of the contract.
//!
//! These tests read README.md and compare what it tells people with the code: every label in
//! bold and every quoted message must be a text of the interface, the keyboard table and the
//! option tables must be the tables the app prints with `--help`, the limits must be the
//! constants, and the settings and files it names must be the ones the code reads and writes.
//! They also run the build script for the generated files of a package (`build.bat
//! --package-files`) and check the real output: README.txt of both profiles and the index of
//! third-party licences.
//!
//! The conventions of the README that make this checkable:
//!
//! * `**bold**` is something on screen to click or look for, written exactly as the interface
//!   writes it, with " → " between the steps of a menu path. The lead-in of a bullet under
//!   "Highlights:" is the one other use of bold.
//! * A message of the app stands in backticks. Text in backticks that starts with a capital
//!   letter and has a space in it is taken for a message, and so is every such text in the first
//!   column of the Troubleshooting table.
//!
//! A text counts as real when it is a string of `app/src/strings.rs` or
//! `core/src/messages.rs` (comments and tests left out), or what one of their functions returns
//! for the example the README uses (`rendered_examples`).

use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::OnceLock;

use instagram_core::archive::{self, cache};
use instagram_core::library::LibraryLimits;
use instagram_core::test_folder::{self, TestFolder};
use instagram_core::web::{self as web_server, network, routes};
use instagram_core::{autoview, decode, export, manifest, messages, poster, settings, store, text, video_rules};
use instagram_windows_app::shortcuts::{self, CommandLineOption, SHORTCUTS};
use instagram_windows_app::{cli, layout, strings, theme, zoom};

// --- Files ---------------------------------------------------------------------------------------

/// The folder of the workspace.
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

/// A text file of the project, with Unix line ends.
fn read(relative: &str) -> String {
    let path = root().join(relative);
    std::fs::read_to_string(&path).unwrap_or_else(|error| panic!("{} cannot be read: {error}", path.display())).replace("\r\n", "\n")
}

/// README.md.
fn readme() -> &'static str {
    static README: OnceLock<String> = OnceLock::new();
    README.get_or_init(|| read("README.md"))
}

// --- Markdown ------------------------------------------------------------------------------------

/// The lines of a document, each with a flag that says whether it is inside a fenced code block
/// (the fence lines themselves count as inside).
fn lines_with_fences(document: &str) -> Vec<(&str, bool)> {
    let mut inside = false;
    document
        .lines()
        .map(|line| {
            let fence = line.trim_start().starts_with("```");
            let flagged = (line, inside || fence);
            if fence {
                inside = !inside;
            }
            flagged
        })
        .collect()
}

/// The document without its fenced code blocks.
fn prose(document: &str) -> String {
    lines_with_fences(document).into_iter().filter(|(_, fenced)| !fenced).map(|(line, _)| line).collect::<Vec<_>>().join("\n")
}

/// The part of the README under `heading` (written with its `#` marks), up to the next heading
/// of the same or a higher level.
fn section(heading: &str) -> String {
    let level = |line: &str| line.chars().take_while(|character| *character == '#').count();
    let wanted = level(heading);
    let mut collected: Option<Vec<&str>> = None;
    for (line, fenced) in lines_with_fences(readme()) {
        let is_heading = !fenced && line.starts_with('#');
        match &mut collected {
            None if is_heading && line == heading => collected = Some(Vec::new()),
            None => {}
            Some(_) if is_heading && level(line) <= wanted => break,
            Some(lines) => lines.push(line),
        }
    }
    collected.unwrap_or_else(|| panic!("README.md has no section \"{heading}\"")).join("\n")
}

/// A table of the README.
struct Table {
    header: Vec<String>,
    rows: Vec<Vec<String>>,
}

/// The cells of one table line; `\|` is a bar inside a cell.
fn cells(line: &str) -> Vec<String> {
    let inner = line.trim().trim_start_matches('|').trim_end_matches('|');
    inner.replace("\\|", "\u{0}").split('|').map(|cell| cell.replace('\u{0}', "|").trim().to_owned()).collect()
}

/// The tables in `text`, in their order.
fn tables(text: &str) -> Vec<Table> {
    let mut found: Vec<Table> = Vec::new();
    let mut open = false;
    for (line, fenced) in lines_with_fences(text) {
        if fenced || !line.trim_start().starts_with('|') {
            open = false;
            continue;
        }
        let row = cells(line);
        if !open {
            found.push(Table { header: row, rows: Vec::new() });
            open = true;
        } else if !row.iter().all(|cell| cell.chars().all(|character| character == '-')) {
            if let Some(table) = found.last_mut() {
                table.rows.push(row);
            }
        }
    }
    found
}

/// The one table of `text` whose first header cell is `first_header`.
fn table(text: &str, first_header: &str) -> Table {
    let mut matching: Vec<Table> = tables(text).into_iter().filter(|table| table.header.first().map(String::as_str) == Some(first_header)).collect();
    assert_eq!(matching.len(), 1, "expected exactly one table that starts with \"{first_header}\"");
    matching.remove(0)
}

/// The texts between pairs of `mark` in `text` (`**` for bold, a backtick for code), with line
/// breaks inside them read as one space.
fn spans(text: &str, mark: &str) -> Vec<String> {
    let mut found = Vec::new();
    let mut rest = text;
    while let Some(start) = rest.find(mark) {
        let after = &rest[start + mark.len()..];
        let Some(end) = after.find(mark) else {
            panic!("README.md has an unclosed {mark} near: {}", after.chars().take(60).collect::<String>());
        };
        found.push(after[..end].split_whitespace().collect::<Vec<_>>().join(" "));
        rest = &after[end + mark.len()..];
    }
    found
}

/// A cell without the backticks of code.
fn plain(cell: &str) -> String {
    cell.replace('`', "")
}

// --- The texts of the interface --------------------------------------------------------------------

/// The string literals of a Rust source file, unescaped. Comments and the test module are left
/// out: a text that is only mentioned there is not a text of the interface.
fn string_literals(source: &str) -> Vec<String> {
    let code = source.split("#[cfg(test)]").next().unwrap_or_default();
    let mut literals = Vec::new();
    let mut characters = code.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '/' if characters.peek() == Some(&'/') => {
                for skipped in characters.by_ref() {
                    if skipped == '\n' {
                        break;
                    }
                }
            }
            // A character literal such as '"' or '\'' must not open a string.
            '\'' => {
                let mut ahead = characters.clone();
                match (ahead.next(), ahead.next()) {
                    (Some('\\'), _) => {
                        characters.next();
                        characters.next();
                        for skipped in characters.by_ref() {
                            if skipped == '\'' {
                                break;
                            }
                        }
                    }
                    (Some(_), Some('\'')) => {
                        characters.next();
                        characters.next();
                    }
                    _ => {}
                }
            }
            '"' => {
                let mut literal = String::new();
                while let Some(inner) = characters.next() {
                    match inner {
                        '"' => break,
                        '\\' => match characters.next() {
                            Some('n') => literal.push('\n'),
                            Some('r') => literal.push('\r'),
                            Some('t') => literal.push('\t'),
                            Some('0') => literal.push('\0'),
                            Some('u') => {
                                let digits: String = characters.by_ref().skip(1).take_while(|digit| *digit != '}').collect();
                                literal.extend(u32::from_str_radix(&digits, 16).ok().and_then(char::from_u32));
                            }
                            // A backslash at the end of a line continues the literal after the
                            // indentation of the next line.
                            Some('\n') => {
                                while characters.peek().is_some_and(|next| next.is_whitespace()) {
                                    characters.next();
                                }
                            }
                            Some(other) => literal.push(other),
                            None => {}
                        },
                        other => literal.push(other),
                    }
                }
                literals.push(literal);
            }
            _ => {}
        }
    }
    literals
}

/// What functions of the interface return for the examples the README uses.
fn rendered_examples() -> Vec<String> {
    vec![
        strings::view_on("Instagram"),
        strings::view_profile_on("Instagram"),
        messages::showing_banner(5, 5, false, "Holiday"),
        messages::showing_banner(6, 6, true, "export-basic"),
        messages::showing_banner(5, 7, true, "export-problems"),
        messages::showing_banner(6, 6, true, "export-basic.zip"),
        messages::showing_banner(3, 3, true, "2 archives"),
        messages::warnings_label(3),
        messages::show_all_warnings(12),
        messages::window_title(Some("IMG_2.jpg")),
        messages::file_too_large("big.jpg", manifest::MAX_IMAGE_BYTES),
        messages::file_too_large("big.mp4", manifest::MAX_VIDEO_BYTES),
        messages::showing_loose_banner(0, 2, "Holiday"),
        messages::showing_loose_banner(5, 2, "Holiday"),
        messages::video_needs_codec("IMG_1.MOV", "HEVC", messages::CODEC_REMEDY_HEVC),
        messages::video_cannot_play("a.mp4", &messages::video_damaged("x")),
        messages::video_cannot_play("a.mp4", messages::VIDEO_NO_PICTURE),
        format!("{}{}", messages::preparing_progress(3, 12), messages::PROGRESS_CANCEL_SUFFIX),
        messages::download_finished(6, &[format!("{}.zip", export::DEFAULT_EXPORT_NAME)], &[]),
    ]
}

/// Every text of the interface: the literals of the two modules that hold them, and the
/// rendered examples.
fn interface_texts() -> &'static Vec<String> {
    static TEXTS: OnceLock<Vec<String>> = OnceLock::new();
    TEXTS.get_or_init(|| {
        let mut texts = string_literals(&read("app/src/strings.rs"));
        texts.extend(string_literals(&read("core/src/messages.rs")));
        texts.extend(rendered_examples());
        texts
    })
}

/// True when `label` is a text of the interface, letter for letter.
fn is_label(label: &str) -> bool {
    interface_texts().iter().any(|text| text == label)
}

/// True when `fragment` is part of a text of the interface.
fn is_message(fragment: &str) -> bool {
    interface_texts().iter().any(|text| text.contains(fragment))
}

// --- Labels and messages -------------------------------------------------------------------------

#[test]
fn the_literal_reader_understands_rust_strings() {
    let source = "// \"a comment\"\nconst A: &str = \"plain\"; // \"trailing\"\nlet b = ('\"', \"quote \\\" inside\", '\\'', \"dots\\u{2026}\");\nlet c = \"first \\\n    second\";\n#[cfg(test)]\nmod tests { const T: &str = \"only in a test\"; }";
    assert_eq!(string_literals(source), ["plain", "quote \" inside", "dots…", "first second"]);
}

#[test]
fn every_label_in_bold_is_a_label_of_the_interface() {
    let text = prose(readme());
    let highlights = text.split("Highlights:").nth(1).and_then(|rest| rest.split("\n## ").next()).expect("the README has a Highlights list");
    // The lead-in of a bullet under "Highlights:" names a feature; it is not a label.
    let lead_ins: BTreeSet<String> = highlights.split("\n- ").skip(1).filter_map(|bullet| spans(bullet, "**").into_iter().next()).collect();
    assert!(lead_ins.len() >= 5, "the Highlights bullets open with a feature name in bold");

    let mut checked = 0;
    for bold in spans(&text, "**") {
        if lead_ins.contains(&bold) && !is_label(&bold) {
            continue;
        }
        for step in bold.split(" → ") {
            assert!(is_label(step), "README.md shows **{bold}**, but \"{step}\" is not a label in app/src/strings.rs or core/src/messages.rs");
            checked += 1;
        }
    }
    assert!(checked > 60, "only {checked} labels were found in bold; the README names many more");
}

#[test]
fn the_menus_the_readme_lists_are_the_menus_of_the_app() {
    // The sidebar, the More menu and the post options are listed in their real order.
    let views = section("## Views");
    let sidebar = [strings::NAV_HOME, strings::NAV_EXPLORE, strings::NAV_OPEN_PICTURES, strings::NAV_OPEN_FOLDER, strings::NAV_AUTO_VIEW, strings::NAV_MORE];
    let menu = [
        strings::MENU_AUTO_VIEW,
        strings::MENU_SWITCH_APPEARANCE,
        strings::MENU_OPEN_IN_BROWSER,
        strings::MENU_BROWSER_VIEW,
        strings::MENU_DOWNLOAD_ALL,
        strings::MENU_OPEN_BROWSER_IMPORTS,
        strings::MENU_OPEN_WEB_APP_LIBRARY,
        strings::MENU_CLOSE_PICTURES,
        strings::MENU_ABOUT,
    ];
    let listed = spans(&views, "**");
    let expected: Vec<&str> = sidebar.iter().chain(menu.iter()).copied().collect();
    assert_eq!(listed.iter().take(expected.len()).map(String::as_str).collect::<Vec<_>>(), expected, "the sidebar and the More menu");

    let options = table(&views, "Entry");
    let entries: Vec<String> = options.rows.iter().map(|row| row[0].replace("**", "")).collect();
    let expected = [
        strings::view_on("Instagram"),
        strings::view_profile_on("Instagram"),
        strings::SHEET_COPY_LINK.to_owned(),
        strings::SHEET_GO_TO_POST.to_owned(),
        strings::SHEET_EDIT_DESCRIPTION.to_owned(),
        strings::SHEET_DOWNLOAD_POST.to_owned(),
        strings::SHEET_SHOW_IN_FOLDER.to_owned(),
        strings::SHEET_AUTO_VIEW_FROM_HERE.to_owned(),
        strings::SHEET_CANCEL.to_owned(),
    ];
    assert_eq!(entries, expected, "the entries of More options");
}

#[test]
fn every_quoted_message_is_a_message_of_the_app() {
    let text = prose(readme());
    // The name of the GitHub workflow is the one capitalised phrase in backticks that is not a
    // text of the app; it is checked against the workflow file instead.
    let workflow = read("../.github/workflows/build-release.yml");
    let workflow_name = workflow.lines().find_map(|line| line.strip_prefix("name: ")).expect("the workflow has a name");

    let mut checked = 0;
    for code in spans(&text, "`") {
        let starts_with_capital = code.chars().next().is_some_and(|first| first.is_ascii_uppercase());
        if !starts_with_capital || !code.contains(' ') {
            continue;
        }
        if code == workflow_name {
            continue;
        }
        assert!(is_message(&code), "README.md quotes `{code}`, which the app does not say (see app/src/strings.rs and core/src/messages.rs)");
        checked += 1;
    }
    assert!(checked >= 20, "only {checked} quoted messages were found");
    assert!(text.contains(&format!("`{workflow_name}`")), "the README names the workflow as the workflow file does");
}

#[test]
fn the_troubleshooting_table_quotes_real_messages() {
    let rows = table(&section("### Troubleshooting"), "What you see").rows;
    assert!(rows.len() >= 12);
    let mut checked = 0;
    for row in &rows {
        assert_eq!(row.len(), 2, "a troubleshooting row has two cells: {row:?}");
        assert!(row[1].ends_with('.'), "the advice is written in full sentences: {}", row[1]);
        for quoted in spans(&row[0], "`") {
            // Options are checked with the option tables.
            if quoted.starts_with("--") {
                continue;
            }
            assert!(is_message(&quoted), "the Troubleshooting table quotes `{quoted}`, which the app does not say");
            checked += 1;
        }
    }
    assert!(checked >= 12, "only {checked} messages are quoted in the Troubleshooting table");
}

#[test]
fn examples_quoted_in_the_text_are_what_the_app_shows() {
    // Small texts that do not look like sentences: each is produced by the real function here
    // and must stand in the README in backticks.
    let examples = [
        (strings::POST_CAPTION_MORE.to_owned(), "the button that expands a description"),
        // The counter example of the clone's own test (library.spec.ts:636).
        (messages::caption_counter("  Low tide again 🌊\nThe second line.  "), "the counter of the editor"),
        (messages::auto_view_counter(1, 6), "the counter of auto view"),
        (format!("{}.zip", export::DEFAULT_EXPORT_NAME), "the suggested name of Download all"),
        (format!("{}.zip", export::single_post_export_name("IMG_2")), "the suggested name of Download post"),
        (export::archive_file_name(export::DEFAULT_EXPORT_NAME, 1, 2), "the first part of a split download"),
        (export::archive_file_name(export::DEFAULT_EXPORT_NAME, 2, 2), "the second part of a split download"),
        (export::EXPORT_FORMAT.to_owned(), "the format name of a download"),
        (export::MANIFEST_ENTRY_NAME.to_owned(), "the manifest of an export"),
        (manifest::MANIFEST_NAMES[1].to_owned(), "the other name of a manifest"),
        (messages::APP_TITLE.to_owned(), "the title of the README"),
    ];
    let text = readme();
    for (example, what) in examples {
        let found = if example == messages::APP_TITLE { text.starts_with(&format!("# {example}\n")) } else { text.contains(&format!("`{example}`")) };
        assert!(found, "README.md does not show {what} as the app writes it: {example}");
    }
    // The loose pictures in the order the README gives as its example.
    let mut names = ["IMG_10.jpg", "img_1.png", "IMG_2.jpg"];
    names.sort_by(|left, right| text::natural_cmp(left, right));
    assert!(text.contains(&format!("(`{}`, `{}`, `{}`)", names[0], names[1], names[2])), "the example of the order of loose pictures");
}

// --- Keyboard and command line -------------------------------------------------------------------

/// Where each shortcut is handled: the file and the text of the key in it. An empty list is a
/// key that egui handles itself (typing a line break, moving the focus, pressing the focused
/// button).
fn shortcut_handlers(shortcut: &shortcuts::Shortcut) -> Option<Vec<(&'static str, &'static str)>> {
    use shortcuts::{SCOPE_ANYWHERE, SCOPE_AUTO_VIEW, SCOPE_AUTO_VIEW_SETUP, SCOPE_DIALOGS, SCOPE_EDITOR, SCOPE_FEED, SCOPE_VIEWER};
    let handlers = match (shortcut.scope, shortcut.keys) {
        (SCOPE_ANYWHERE, "Ctrl+O") => vec![("app.rs", "KeyboardShortcut::new(Modifiers::COMMAND, Key::O)")],
        (SCOPE_ANYWHERE, "Ctrl+Shift+O") => vec![("app.rs", "KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::O)")],
        (SCOPE_ANYWHERE, "F11") => vec![("app.rs", "Key::F11")],
        (SCOPE_ANYWHERE, "Escape") => vec![("app.rs", "Key::Escape")],
        (SCOPE_FEED, "Home / End") => vec![("feed.rs", "Key::Home"), ("feed.rs", "Key::End"), ("explore.rs", "Key::Home"), ("explore.rs", "Key::End")],
        (SCOPE_FEED, "Page Up / Page Down") => vec![("feed.rs", "Key::PageUp"), ("feed.rs", "Key::PageDown"), ("explore.rs", "Key::PageUp"), ("explore.rs", "Key::PageDown")],
        (SCOPE_FEED, "Up / Down") => vec![("feed.rs", "Key::ArrowUp"), ("feed.rs", "Key::ArrowDown"), ("explore.rs", "Key::ArrowUp"), ("explore.rs", "Key::ArrowDown")],
        (SCOPE_VIEWER, "Left / Right") => vec![("viewer.rs", "Key::ArrowLeft"), ("viewer.rs", "Key::ArrowRight")],
        (SCOPE_VIEWER, "Home / End") => vec![("viewer.rs", "Key::Home"), ("viewer.rs", "Key::End")],
        (SCOPE_VIEWER, "+ / -") => vec![("viewer.rs", "Key::Plus"), ("viewer.rs", "Key::Minus")],
        (SCOPE_VIEWER, "0") => vec![("viewer.rs", "Key::Num0")],
        (SCOPE_VIEWER, "Space / K") => vec![("video/stage.rs", "Key::Space"), ("video/stage.rs", "Key::K")],
        (SCOPE_VIEWER, "M") => vec![("video/stage.rs", "Key::M")],
        (SCOPE_VIEWER, "Shift+Left / Shift+Right") => vec![("video/stage.rs", "pressed(Modifiers::SHIFT, key)"), ("video/stage.rs", "[(Key::ArrowLeft, false), (Key::ArrowRight, true)]")],
        (SCOPE_VIEWER, "Escape") => vec![("viewer.rs", "Key::Escape"), ("state.rs", "Action::ViewerEscape =>")],
        (SCOPE_EDITOR, "Ctrl+Enter") => vec![("editor.rs", "consume_key(Modifiers::COMMAND, Key::Enter)")],
        (SCOPE_EDITOR, "Enter") => vec![],
        (SCOPE_EDITOR, "Escape") => vec![("widgets.rs", "Key::Escape")],
        (SCOPE_AUTO_VIEW_SETUP, "Enter") => vec![("autoview.rs", "field_focused && ctx.input_mut(|input| input.consume_key(Modifiers::NONE, Key::Enter))")],
        (SCOPE_AUTO_VIEW, "Space") => vec![("autoview.rs", "Key::Space")],
        (SCOPE_AUTO_VIEW, "Left / Right") => vec![("autoview.rs", "Key::ArrowLeft"), ("autoview.rs", "Key::ArrowRight")],
        (SCOPE_AUTO_VIEW, "+ / -") => vec![("autoview.rs", "Key::Plus"), ("autoview.rs", "Key::Minus")],
        (SCOPE_AUTO_VIEW, "M") => vec![("video/stage.rs", "Key::M")],
        (SCOPE_AUTO_VIEW, "Escape") => vec![("autoview.rs", "Key::Escape")],
        (SCOPE_DIALOGS, "Escape") => vec![("widgets.rs", "Key::Escape"), ("shell.rs", "Key::Escape")],
        (SCOPE_DIALOGS, "Tab / Shift+Tab") => vec![],
        (SCOPE_DIALOGS, "Enter or Space") => vec![],
        _ => return None,
    };
    Some(handlers)
}

#[test]
fn the_keyboard_table_is_the_table_of_the_app() {
    let documented: Vec<Vec<String>> = table(&section("### Keyboard"), "Where").rows;
    let expected: Vec<Vec<String>> = SHORTCUTS.iter().map(|shortcut| vec![shortcut.scope.to_owned(), shortcut.keys.to_owned(), shortcut.action.to_owned()]).collect();
    assert_eq!(documented, expected, "the Keyboard table of README.md and shortcuts::SHORTCUTS differ");
}

#[test]
fn every_shortcut_is_handled_where_the_table_says() {
    let mut sources: BTreeMap<&str, String> = BTreeMap::new();
    for shortcut in SHORTCUTS {
        let handlers = shortcut_handlers(shortcut).unwrap_or_else(|| panic!("the test does not know where \"{}\" in \"{}\" is handled; add it to shortcut_handlers", shortcut.keys, shortcut.scope));
        for (file, needle) in handlers {
            let source = sources.entry(file).or_insert_with(|| read(&format!("app/src/{file}")));
            assert!(source.contains(needle), "\"{}\" in \"{}\" is documented, but app/src/{file} has no `{needle}`", shortcut.keys, shortcut.scope);
        }
    }
    // The scopes are named after what the interface calls these places.
    assert!(is_label(shortcuts::SCOPE_EDITOR) && is_label(shortcuts::SCOPE_AUTO_VIEW));
    // The numbers of the paragraph about the mouse.
    let keyboard = section("### Keyboard").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(keyboard.contains(&format!("a factor of {} per notch, {} per key press", zoom::WHEEL_NOTCH_STEP, zoom::KEY_STEP)), "the zoom steps");
}

fn option_rows(options: &[CommandLineOption]) -> Vec<Vec<String>> {
    options.iter().map(|option| vec![option.syntax.to_owned(), option.purpose.to_owned()]).collect()
}

#[test]
fn the_option_tables_are_the_options_of_the_app() {
    let command_line = section("### Command line");
    let documented: Vec<Vec<Vec<String>>> =
        tables(&command_line).into_iter().filter(|table| table.header[0] == "Option").map(|table| table.rows.iter().map(|row| row.iter().map(|cell| plain(cell)).collect()).collect()).collect();
    assert_eq!(documented.len(), 2, "one table of everyday options and one of development options");
    assert_eq!(documented[0], option_rows(shortcuts::OPTIONS), "the everyday options");
    assert_eq!(documented[1], option_rows(shortcuts::DEVELOPMENT_OPTIONS), "the development options");
    assert!(command_line.contains("instagram-windows-app.exe [options] [pictures or folders...]"));
    assert!(shortcuts::help_text().contains(&format!("{} [options] [pictures or folders...]", settings::APP_NAME)));
}

#[test]
fn every_documented_option_is_accepted_by_the_command_line() {
    for option in shortcuts::OPTIONS.iter().chain(shortcuts::DEVELOPMENT_OPTIONS) {
        let name = shortcuts::option_name(option);
        let mut arguments: Vec<OsString> = option.example.iter().map(OsString::from).collect();
        assert!(option.example.iter().any(|argument| argument.starts_with(name)), "the example of {} does not use it", option.syntax);
        arguments.push(OsString::from("C:\\Pictures\\a.jpg"));
        let parsed = cli::parse(&arguments).unwrap_or_else(|error| panic!("{} is documented but refused: {}", option.syntax, error.message));
        assert_eq!(parsed.paths, vec![PathBuf::from("C:\\Pictures\\a.jpg")], "{} must not swallow a path", option.syntax);
        if name == shortcuts::OPTION_HELP || name == shortcuts::OPTION_VERSION {
            assert!(shortcuts::information(&arguments).is_some(), "{name} prints something");
        } else {
            assert!(shortcuts::information(&arguments).is_none());
        }
    }
    // The examples of the README itself.
    let parsed = cli::parse(&["--autoview=15", "--loop=0", "--shuffle=1"].map(OsString::from)).expect("the auto view options of the README");
    let request = parsed.auto_view.expect("auto view is asked for");
    assert_eq!((request.interval_sec, request.loop_posts, request.shuffle), (Some(15), Some(false), Some(true)));
    let remembered = cli::parse(&[OsString::from("--autoview")]).expect("--autoview alone").auto_view.expect("auto view is asked for");
    assert_eq!(remembered.interval_sec, None, "--autoview alone uses the remembered time");
    // And what is not an option is refused, with the list of the everyday options.
    let refused = cli::parse(&[OsString::from("--frobnicate")]).expect_err("an unknown option").message;
    for option in shortcuts::OPTIONS {
        assert!(refused.contains(option.syntax));
    }
}

// --- Limits and numbers --------------------------------------------------------------------------

#[test]
fn the_limits_table_states_the_limits_of_the_code() {
    let count = |value: usize| text::format_count_exact(value as u64);
    let megapixels = decode::MAX_SOURCE_PIXELS / (1024 * 1024);
    let expected: Vec<(String, String)> = [
        ("One picture file", format!("{} (a larger file is skipped with a warning)", text::format_bytes(manifest::MAX_IMAGE_BYTES))),
        ("One loose video file", format!("{} (a larger file is skipped with a warning)", text::format_bytes(manifest::MAX_VIDEO_BYTES))),
        ("Files in an opened folder", count(manifest::MAX_SOURCE_ENTRIES)),
        ("Pixels of one picture", format!("{megapixels} megapixels (a larger picture is not shown, but can be downloaded)")),
        ("Longest side of a picture on screen", format!("{} pixels (a larger picture is scaled down)", count(decode::MAX_TEXTURE_SIDE as usize))),
        ("Zoom", format!("{} %", zoom::MAX_SCALE * 100.0)),
        ("Description", format!("{} characters", count(manifest::MAX_CAPTION_CHARS))),
        ("Posts of an export", format!("{} (further entries are ignored with a warning)", count(manifest::MAX_POSTS))),
        ("Pictures and videos per post of an export", count(manifest::MAX_IMAGES_PER_POST)),
        ("Sample comments per post of an export", count(manifest::MAX_COMMENTS_PER_POST)),
        ("`post.json`", text::format_bytes(manifest::MAX_MANIFEST_BYTES)),
        ("One video of an export", text::format_bytes(manifest::MAX_VIDEO_BYTES)),
        ("Export folders and archives opened at a time", format!("{} (further ones are left out with a warning)", count(manifest::MAX_SOURCES))),
        ("Pictures and videos of the archives of one opening", text::format_bytes(manifest::MAX_TOTAL_MEDIA_BYTES)),
        ("An unpacked `.tar.gz`", text::format_bytes(archive::MAX_GZIP_OUTPUT_BYTES)),
        (
            "Archive cache",
            format!(
                "{}; an archive not opened for {} days is removed; {} of the drive stays free",
                text::format_bytes(cache::ARCHIVE_CACHE_MAX_BYTES),
                cache::ARCHIVE_CACHE_MAX_AGE_DAYS,
                text::format_bytes(cache::ARCHIVE_CACHE_MIN_FREE_BYTES)
            ),
        ),
        ("Posts per downloaded archive", format!("{} (more are split into parts)", count(export::EXPORT_PART_POSTS))),
        ("Pictures and videos per downloaded archive", format!("{} (more is split into parts)", text::format_bytes(export::EXPORT_PART_BYTES))),
        ("Time per post in auto view", format!("{} to {} seconds", autoview::MIN_INTERVAL_SEC, autoview::MAX_INTERVAL_SEC)),
        ("Videos playing at a time", format!("1 (one player each for the feed, the viewer and auto view; {} at most)", video_rules::MAX_PLAYERS)),
        ("Frames of a video with a graphics card", format!("{} pixels on the longest side (a larger video is scaled down)", count(video_rules::GPU_MAX_FRAME_SIDE as usize))),
        ("Frames of a video without a graphics card", format!("{} pixels (640 x 360), {} repaints a second", count(video_rules::CPU_MAX_FRAME_PIXELS as usize), video_rules::CPU_MAX_REPAINT_HZ)),
        ("A paused video of the feed is kept", format!("{} seconds", video_rules::FEED_KEEP_PAUSED_SEC)),
        ("Poster of a loose video", format!("{} pixels on the longest side", count(poster::POSTER_MAX_SIDE as usize))),
        ("Poster cache", format!("{} (the oldest posters go first)", text::format_bytes(poster::POSTER_CACHE_MAX_BYTES))),
        ("Auto view waits for a video without a length", format!("{} seconds", autoview::UNKNOWN_LENGTH_MAX_SEC)),
    ]
    .into_iter()
    .map(|(label, value)| (label.to_owned(), value))
    .collect();
    let documented: Vec<(String, String)> = table(&section("### Limits"), "Limit").rows.into_iter().map(|row| (row[0].clone(), row[1].clone())).collect();
    assert_eq!(documented, expected, "the Limits table of README.md and the constants of the code differ");
}

#[test]
fn the_numbers_in_the_text_are_the_numbers_of_the_code() {
    // The README is hard-wrapped: compare without regard to where its lines break.
    let text = readme().split_whitespace().collect::<Vec<_>>().join(" ");
    let megapixels = decode::MAX_SOURCE_PIXELS / (1024 * 1024);
    let presets = autoview::INTERVAL_PRESETS;
    let statements = [
        format!("a description of up to {} characters", text::format_count_exact(manifest::MAX_CAPTION_CHARS as u64)),
        format!("The limit is {} characters", text::format_count_exact(manifest::MAX_CAPTION_CHARS as u64)),
        format!("zoom around the pointer up to {} %", zoom::MAX_SCALE * 100.0),
        format!("({} seconds by default)", autoview::DEFAULT_INTERVAL_SEC),
        format!("({} to {} seconds, {} by default, with quick choices from {} seconds to {} minute)", autoview::MIN_INTERVAL_SEC, autoview::MAX_INTERVAL_SEC, autoview::DEFAULT_INTERVAL_SEC, presets[0], presets[presets.len() - 1] / 60),
        format!("at least {} seconds each", autoview::MIN_SLIDE_SEC),
        format!("narrower than {} pixels", theme::SIDEBAR_RAIL_MAX_WINDOW_WIDTH),
        format!("The smallest window is {} x {}", theme::MIN_WINDOW_SIZE[0], theme::MIN_WINDOW_SIZE[1]),
        format!("a {}-pixel column", theme::FEED_WIDTH),
        format!("between 4:5 and {}:1", layout::MAX_FEED_RATIO),
        format!("which lists the first {}", ["zero", "one", "two", "three", "four", "five", "six"][messages::MAX_WARNINGS_SHOWN]),
        format!("More than {} posts or {} of pictures and videos are split into parts", text::format_count_exact(export::EXPORT_PART_POSTS as u64), text::format_bytes(export::EXPORT_PART_BYTES)),
        format!("has more than {megapixels} megapixels"),
        format!("`{}`, version {}", export::EXPORT_FORMAT, export::EXPORT_VERSION),
        format!("decoded to at most {} pixels per side", text::format_count_exact(u64::from(decode::MAX_TEXTURE_SIDE))),
        format!("At most {} sources are read at a time", manifest::MAX_SOURCES),
        format!("may add up to {} of pictures and videos", text::format_bytes(manifest::MAX_TOTAL_MEDIA_BYTES)),
        format!("archives not opened for {} days go, and beyond {} the least recently used go first", cache::ARCHIVE_CACHE_MAX_AGE_DAYS, text::format_bytes(cache::ARCHIVE_CACHE_MAX_BYTES)),
        format!("Unpacking stops when less than {} would stay free", text::format_bytes(cache::ARCHIVE_CACHE_MIN_FREE_BYTES)),
        format!("`%LOCALAPPDATA%\\{}\\{}`", settings::APP_NAME, settings::ARCHIVE_CACHE_FOLDER_NAME),
        // Videos.
        format!("`%LOCALAPPDATA%\\{}\\{}`", settings::APP_NAME, settings::POSTER_CACHE_FOLDER_NAME),
        format!("at least {} % of its frame", (video_rules::MIN_VISIBLE_RATIO * 100.0).round()),
        format!("for {} seconds after a key or a move of the pointer", video_rules::CONTROLS_SHOWN_SEC),
        format!("Shift+Left and Shift+Right for {} seconds back or forward", video_rules::SEEK_STEP_SEC),
        format!("at most {} repaints a second", video_rules::CPU_MAX_REPAINT_HZ),
        // Every repaint of a playing video shows a new frame, so the beat is what is seen.
        "each with the newest frame".to_owned(),
        format!("the window showed about {} new frames a second", video_rules::CPU_MAX_REPAINT_HZ),
        format!("a playing video shows about {} new frames a second", video_rules::CPU_MAX_REPAINT_HZ),
        format!("the window is repainted at most {} times a second then", video_rules::CPU_MAX_REPAINT_HZ),
        // A minimised window pauses the video that plays (`VideoHub::window_minimized`).
        "Minimising the window pauses the video that plays, in the feed, the viewer or auto view; when the window is back it plays on where it was, unless you had paused it.".to_owned(),
        format!("A video of the feed that was paused for more than {} seconds (minimised or covered) starts again from its beginning.", video_rules::FEED_KEEP_PAUSED_SEC),
        "Time does not pass while the window is minimised, and a video on show pauses until the window is back".to_owned(),
        format!("at most {} seconds; a shorter video plays in a loop", autoview::UNKNOWN_LENGTH_MAX_SEC),
        format!("up to {} each, and play", text::format_bytes(manifest::MAX_VIDEO_BYTES)),
    ];
    for statement in statements {
        assert!(text.contains(&statement), "README.md does not say \"{statement}\" any more, or the code changed");
    }
    assert!((layout::MIN_FEED_RATIO - 4.0 / 5.0).abs() < f32::EPSILON, "the narrow end of the crop is 4:5");
    assert_eq!(autoview::Settings::default(), autoview::Settings { interval_sec: autoview::DEFAULT_INTERVAL_SEC, wait_for_videos: true, loop_posts: true, shuffle: false }, "loop is on and shuffle is off by default");
    assert_eq!(store::UiState::default().theme, store::Theme::Light, "the viewer starts in the light appearance");
}

// --- Videos --------------------------------------------------------------------------------------

#[test]
fn the_videos_section_says_what_plays_and_the_old_note_is_gone() {
    let videos = section("## Videos").split_whitespace().collect::<Vec<_>>().join(" ");
    // What plays, where, and with which controls, as the code names them.
    for label in [strings::VIDEO_TURN_SOUND_ON, strings::VIDEO_TURN_SOUND_OFF, strings::VIDEO_PLAY_BADGE, strings::VIDEO_PLAY, strings::VIDEO_PAUSE, strings::VIDEO_MUTE, strings::VIDEO_UNMUTE, strings::VIDEO_VOLUME, strings::VIDEO_SHOW_IN_FOLDER] {
        assert!(videos.contains(&format!("**{label}**")), "the Videos section names **{label}**");
    }
    for fact in ["MP4 (also `.m4v`), MOV and WebM", "HEVC Video Extensions", "VP9 Video Extensions", "AV1 Video Extension", "Web Media Extensions", "Space or K, M for the sound", "recorded sideways is shown upright", "about two seconds"] {
        assert!(videos.contains(fact), "the Videos section says: {fact}");
    }
    assert!(videos.contains(&format!("`{}`", video_rules::format_time_pair(3.2, Some(10.0)))), "the example of the time");
    // The auto view option and its command-line switch.
    let auto_view = section("## Auto view").split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(auto_view.contains(&format!("**{}** (on by default)", strings::AUTO_VIEW_WAIT_FOR_VIDEOS)));
    assert!(auto_view.contains("`--wait=0`"));
    assert!(autoview::Settings::default().wait_for_videos, "on by default");
    // The note of the time videos were not played is gone from every text people read.
    let help = shortcuts::help_text();
    let template = read("build.ps1");
    for (name, text) in [("README.md", readme().to_owned()), ("--help", help.clone()), ("the README.txt template", template.clone()), ("strings.rs", read("app/src/strings.rs")), ("messages.rs", read("core/src/messages.rs"))] {
        assert!(!text.contains("are not played"), "{name} still says that videos are not played");
    }
    assert!(help.contains("Videos (MP4, MOV, WebM) open like pictures and play"), "--help says that videos play");
    assert!(template.contains("Videos (MP4, MOV, WebM) play in the viewer, and in the feed with a"), "README.txt says where videos play");
    // Without a graphics card the feed does not play videos by itself: every text that says where
    // videos play says so, the intro of README.md and --help as well.
    let flat = |text: &str| text.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat(&help).contains("play in the viewer and auto view, and in the feed with a graphics card."), "--help names the graphics card");
    assert!(flat(readme()).contains("in the viewer and auto view, and in the feed with a graphics card. See [Videos](#videos)."), "the README intro names the graphics card");
    for (name, text) in [("README.md", flat(readme())), ("--help", flat(&help))] {
        assert!(!text.contains("play in the feed, the viewer and auto"), "{name} says without a condition that videos play in the feed");
    }
}

// --- Settings and files --------------------------------------------------------------------------

#[test]
fn the_settings_table_lists_the_keys_the_core_reads() {
    let configuration = section("## Configuration");
    let documented: Vec<String> = table(&configuration, "Setting").rows.iter().map(|row| plain(&row[0])).collect();
    assert_eq!(
        documented,
        [
            settings::KEY_DATA_DIRECTORY,
            settings::KEY_DEFAULT_USERNAME,
            settings::KEY_WEB_SERVER,
            settings::KEY_FRONTEND_PORT,
            settings::KEY_BACKEND_PORT,
            settings::KEY_WEB_HOST,
            settings::KEY_ALLOWED_HOSTS,
            settings::KEY_LIBRARY_DIRECTORY,
        ]
    );

    // The code reads exactly these keys from the document.
    let source = read("core/src/settings.rs");
    let code = source.split("#[cfg(test)]").next().unwrap_or_default();
    let reads: Vec<&str> = code.match_indices("document.get(").map(|(start, pattern)| code[start + pattern.len()..].split(')').next().unwrap_or_default()).collect();
    assert_eq!(
        reads,
        ["KEY_DATA_DIRECTORY", "KEY_DEFAULT_USERNAME", "KEY_WEB_SERVER", "KEY_FRONTEND_PORT", "KEY_BACKEND_PORT", "KEY_WEB_HOST", "KEY_ALLOWED_HOSTS", "KEY_LIBRARY_DIRECTORY"],
        "core/src/settings.rs reads other keys than README.md documents"
    );
    // Save in Browser view writes exactly the four keys the README names.
    let web = read("app/src/web.rs");
    for key in ["KEY_WEB_SERVER", "KEY_FRONTEND_PORT", "KEY_BACKEND_PORT", "KEY_WEB_HOST"] {
        assert!(web.contains(&format!("(settings::{key}, serde_json::Value::from(")), "Save does not write {key}");
    }
    let flat = configuration.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(flat.contains("which changes the values of `web_server`, `frontend_port`, `backend_port` and `web_host` and nothing else, the layout of the file included"));
    assert!(flat.contains("For the browser pages the command line wins for its run, then `settings.json`, then the defaults."));

    let location = format!("`%APPDATA%\\{}\\{}`", settings::APP_NAME, settings::SETTINGS_FILE_NAME);
    assert!(configuration.contains(&location), "the README names the settings file as {location}");
    assert!(configuration.contains(&format!("`%APPDATA%\\{}\\{}`", settings::APP_NAME, settings::DATA_FOLDER_NAME)), "the default data folder");
}

#[test]
fn the_settings_example_is_a_file_the_app_accepts() {
    let configuration = section("## Configuration");
    let example = configuration.split("```json\n").nth(1).and_then(|rest| rest.split("```").next()).expect("the Configuration section has a JSON example");
    let folder = TestFolder::new("docs-settings").expect("a folder for the example");
    let file = folder.join(settings::SETTINGS_FILE_NAME);
    std::fs::write(&file, example).expect("the example can be written");
    let environment: std::collections::HashMap<String, String> = [("USERNAME".to_owned(), "tester".to_owned())].into();
    let loaded = settings::load_or_create_at(&file, &environment).expect("the example of the README loads");
    assert_eq!(loaded.default_username, "lumen.atlas");
    assert_eq!(loaded.paths.data_dir, PathBuf::from("D:\\InstagramViewerData"));
    assert_eq!((loaded.web.frontend_port, loaded.web.backend_port), (Some(web_server::DEFAULT_FRONTEND_PORT), Some(web_server::DEFAULT_BACKEND_PORT)), "the example names the default ports");
    assert_eq!(loaded.web.allowed_hosts, ["photos.example"]);
    assert_eq!(std::fs::read_to_string(&file).expect("the file is still there"), example, "a valid settings file is never rewritten");
}

#[test]
fn the_folders_of_the_tests_are_where_the_readme_says() {
    let tests = section("## Tests").split_whitespace().collect::<Vec<_>>().join(" ");
    let base = format!("`%TEMP%\\{}`", test_folder::TESTS_FOLDER_NAME);
    assert!(tests.contains(&format!("write in {base}, one folder `<test>-<process id>` per test")), "README.md names the tests' folder {base}");
    assert!(tests.contains(&format!("Set the environment variable `{}` to `1`", test_folder::KEEP_TEST_FILES_VARIABLE)));
    assert!(tests.contains(&format!("nothing in {base} is removed")));
    assert_eq!(test_folder::tests_folder(), std::env::temp_dir().join(test_folder::TESTS_FOLDER_NAME));
    let folder = TestFolder::new("docs-tests-folder").expect("a test folder");
    assert_eq!(folder.path(), test_folder::tests_folder().join(format!("docs-tests-folder-{}", std::process::id())));
}

#[test]
fn the_files_of_the_data_folder_are_the_files_the_app_keeps() {
    let configuration = section("## Configuration");
    let documented: Vec<String> = table(&configuration, "File").rows.iter().map(|row| plain(&row[0])).collect();
    assert_eq!(documented, [settings::DESCRIPTIONS_FILE_NAME, settings::STATE_FILE_NAME, settings::WINDOW_FILE_NAME, settings::LOG_FILE_NAME]);
    let paths = settings::AppPaths::new(PathBuf::from("C:\\profile\\settings.json"), PathBuf::from("C:\\profile\\data"));
    let kept: Vec<String> = [&paths.descriptions_file, &paths.state_file, &paths.window_file, &paths.log_file].iter().map(|path| messages::display_name(path)).collect();
    assert_eq!(documented, kept);

    // What state.json remembers, as the table says.
    let state = store::UiState { last_folder: Some(PathBuf::from("C:\\Pictures")), last_export_folder: Some(PathBuf::from("C:\\Downloads")), ..store::UiState::default() };
    let written: Vec<String> = state.to_json_text().lines().filter_map(|line| line.strip_prefix("  \"")).filter_map(|rest| rest.split('"').next()).map(str::to_owned).collect();
    assert_eq!(written, ["theme", "auto_view", "last_folder", "last_export_folder"], "the appearance, the auto view settings, the folder opened last and the folder of the last download");

    // The folders of the local profile, in the second table.
    let folders: Vec<String> = table(&configuration, "Folder").rows.iter().map(|row| plain(&row[0])).collect();
    assert_eq!(folders, [settings::ARCHIVE_CACHE_FOLDER_NAME, settings::LIBRARY_FOLDER_NAME, settings::WEB_CACHE_FOLDER_NAME, settings::POSTER_CACHE_FOLDER_NAME]);
    let local = paths.clone().with_local_dir(PathBuf::from("C:\\local\\instagram-windows-app"));
    assert_eq!(
        [messages::display_name(&local.archive_cache_dir), messages::display_name(&local.library_dir), messages::display_name(&local.web_cache_dir), messages::display_name(&local.poster_cache_dir)],
        [folders[0].clone(), folders[1].clone(), folders[2].clone(), folders[3].clone()]
    );
    assert!(configuration.contains(&format!("`%LOCALAPPDATA%\\{}`", settings::APP_NAME)), "the local folder of the app");

    // The log and its switch.
    let logging = read("app/src/logging.rs");
    assert!(logging.contains("const MAX_LOG_BYTES: u64 = 1024 * 1024;") && configuration.contains("A file over 1 MB is set aside as `app.log.old`"));
    assert!(logging.contains("\"INSTAGRAM_WINDOWS_APP_LOG\"") && configuration.contains("`INSTAGRAM_WINDOWS_APP_LOG`"));
}

// --- The browser pages -------------------------------------------------------------------------------

#[test]
fn the_browser_section_documents_the_api_the_limits_and_the_sheet() {
    let browser = section("## Browser");
    // The API table is the route table of the server (the one /docs shows), in the format of
    // the clone's README: "Method | Path | Behavior", methods and paths as code.
    let api = table(&browser, "Method");
    assert_eq!(api.header, ["Method", "Path", "Behavior"], "the header of the clone's API table");
    for row in &api.rows {
        for method in row[0].split(", ") {
            assert!(method.starts_with('`') && method.ends_with('`'), "the method {method} stands in backticks, as in the clone's table");
        }
        assert!(row[1].starts_with("`/api/") && row[1].ends_with('`'), "the path {} stands in backticks", row[1]);
    }
    let documented: Vec<(String, String, String)> = api.rows.iter().map(|row| (plain(&row[0]), plain(&row[1]), row[2].clone())).collect();
    let expected: Vec<(String, String, String)> = routes::API_ROUTES.iter().map(|(method, path, purpose)| ((*method).to_owned(), (*path).to_owned(), (*purpose).to_owned())).collect();
    assert_eq!(documented, expected, "the API table of README.md and routes::API_ROUTES differ");

    // The upload limits are the library's.
    let limits = LibraryLimits::default();
    let count = |value: usize| text::format_count_exact(value as u64);
    let expected: Vec<(String, String)> = [
        ("One picture", text::format_bytes(limits.max_image_bytes)),
        ("One video", text::format_bytes(limits.max_video_bytes)),
        ("One import (an upload session)", text::format_bytes(limits.max_session_bytes)),
        ("The whole folder", text::format_bytes(limits.max_cache_bytes)),
        ("Posts per import", count(limits.max_posts_per_commit)),
        ("Files per import", count(limits.max_files_per_session)),
        ("The commit of an import", text::format_bytes(limits.max_commit_bytes)),
        ("Imports being saved at once", count(limits.max_open_sessions)),
        ("Imports being saved at once from one device", count(limits.max_sessions_per_client)),
        ("Of those, kept for programs on this computer", count(limits.sessions_kept_for_this_computer)),
        ("Commits merged into the folder at the same time", count(limits.max_commits_at_once)),
        ("A commit waits for its turn at most", format!("{} seconds", limits.commit_wait.as_secs())),
        ("An import that received no file is dropped after", format!("{} minutes", limits.stale_empty_session.as_secs() / 60)),
        ("An idle import is dropped after", format!("{} minutes", limits.stale_staging.as_secs() / 60)),
        ("Free space that stays on the drive", text::format_bytes(limits.min_free_bytes)),
    ]
    .into_iter()
    .map(|(label, value)| (label.to_owned(), value))
    .collect();
    let documented: Vec<(String, String)> = table(&browser, "Upload limit").rows.into_iter().map(|row| (row[0].clone(), row[1].clone())).collect();
    assert_eq!(documented, expected, "the upload limits of README.md and LibraryLimits differ");

    // The addresses, the port search and the sheet.
    let flat = browser.split_whitespace().collect::<Vec<_>>().join(" ");
    // The limits of the HTTP layer that the safety paragraph names.
    let timeouts = web_server::Timeouts::default();
    for statement in [
        format!("Each address holds at most {} connections and one client at most {} of them", web_server::MAX_CONNECTIONS, web_server::MAX_CONNECTIONS_PER_PEER),
        "Every program on this computer is one client, whatever loopback address (`127.x.x.x`) it uses".to_owned(),
        format!("{} places of each address are kept for this computer", web_server::RESERVED_FOR_THIS_COMPUTER),
        format!("Every connection is closed after {} minutes or {} requests", timeouts.connection_life.as_secs() / 60, web_server::http::MAX_REQUESTS_PER_CONNECTION),
        format!(
            "A connection takes a place only once its first request has fully arrived. Until then it waits without one, for its first byte at most {} seconds and for the whole request at most {}; one client has at most {} such connections on an address and all clients {} ({} of them kept for this computer), and the one that has waited longest is closed for a new one",
            timeouts.first_byte.as_secs(),
            timeouts.head.as_secs(),
            web_server::MAX_ARRIVING_PER_PEER,
            web_server::MAX_ARRIVING,
            web_server::ARRIVING_KEPT_FOR_THIS_COMPUTER
        ),
        "A connection that waits for its next request, or that only reads what its client still sends after an answer, gives its place to a new request at once when the address is full".to_owned(),
        "A connection whose client has sent bytes that the viewer has not read yet is never closed for another one".to_owned(),
        format!("and the one that has waited longest is closed for a new one (only one of the same client while that client has its {}). A request that had arrived with its whole body when its connection was closed is answered all the same; one whose body was still on its way is answered `Too many connections`.", web_server::MAX_ARRIVING_PER_PEER),
        "So connections that send nothing, or only part of a request, take no place, however often they are opened again; what they can do is close their own client's connections before the requests of those arrive".to_owned(),
        format!("must keep up with {} a second: one that falls {} seconds behind is cut off", text::format_bytes(timeouts.min_bytes_per_second), timeouts.rate_grace.as_secs()),
        format!("counts for at most {} seconds ahead", timeouts.rate_grace.as_secs()),
        format!("One device can have at most {} imports open", limits.max_sessions_per_client),
        format!("programs on this computer share the user's {}, and {} of the {} are kept for them", limits.max_sessions_per_client, limits.sessions_kept_for_this_computer, limits.max_open_sessions),
        format!("An import that has received no file within {} minutes is dropped", limits.stale_empty_session.as_secs() / 60),
        "The commit that saves an import is written to the import's own folder as it arrives, so one that arrives slowly holds only its own connection and import".to_owned(),
        format!("at most {} imports are merged into the folder at a time, and another one waits up to {} seconds for its turn before it is answered `{}`", limits.max_commits_at_once, limits.commit_wait.as_secs(), messages::TOO_MANY_COMMITS),
        format!("see `{}` as the author of loose pictures unless `{}` names one", messages::UNKNOWN_USERNAME, settings::KEY_DEFAULT_USERNAME),
    ] {
        assert!(flat.contains(&statement), "the Browser section does not say: {statement}");
    }
    // What a program on this computer can still do to the browser there is a known limitation.
    let known = section("## Known limitations").split_whitespace().collect::<Vec<_>>().join(" ");
    let held = format!(
        "requests whose body or answer moves at the slowest pace the pages accept ({} a second), or moves nothing until it is cut off {} seconds later, sent again and again",
        text::format_bytes(timeouts.min_bytes_per_second),
        timeouts.rate_grace.as_secs()
    );
    assert!(known.contains(&held), "Known limitations does not say: {held}");
    assert!(known.contains("Connections that only wait never keep it out, and connections that send nothing or only part of a request take no place."));
    // What the limits do not promise (E2E4-1): a program on this computer can always disturb
    // the pages there; nothing may claim it never keeps a request out.
    assert!(known.contains("closes the browser's new connections there before their requests arrive, and some of the browser's requests then fail"));
    assert!(known.contains("no server can keep a program on the same computer from disturbing it"));
    let all = readme().split_whitespace().collect::<Vec<_>>().join(" ");
    for promise in ["never keep a request out", "or only part of a request, and connections that only wait, never keep it out"] {
        assert!(!all.contains(promise), "README.md promises what the server cannot keep: {promise}");
    }
    for statement in [
        format!("`http://127.0.0.1:{}/` by default", web_server::DEFAULT_FRONTEND_PORT),
        format!("`http://127.0.0.1:{}` by default", web_server::DEFAULT_BACKEND_PORT),
        format!("the next free port is used (at most {} are tried)", network::PORT_ATTEMPTS),
        format!("`%LOCALAPPDATA%\\{}\\{}`", settings::APP_NAME, settings::LIBRARY_FOLDER_NAME),
    ] {
        assert!(flat.contains(&statement), "the Browser section does not say: {statement}");
    }
    let labels = spans(&browser, "**");
    for label in [strings::BROWSER_VIEW_SERVER, strings::BROWSER_VIEW_PAGE_PORT, strings::BROWSER_VIEW_API_PORT, strings::BROWSER_VIEW_LAN, strings::BROWSER_VIEW_SAVE, strings::MENU_OPEN_IN_BROWSER, strings::MENU_OPEN_BROWSER_IMPORTS] {
        assert!(labels.iter().any(|bold| bold.split(" → ").any(|step| step == label)), "the Browser section does not name **{label}**");
    }
    assert!(readme().contains("### Browser view"));

    // How to open the pages, what is local, how other devices get in, what a browser may do.
    for heading in ["### Opening the pages", "### What the browser can and cannot do", "### API", "### Upload limits"] {
        assert!(browser.lines().any(|line| line == heading), "the Browser section has \"{heading}\"");
    }
    for statement in [
        "By default the pages are for this computer only: the viewer listens on 127.0.0.1".to_owned(),
        "Windows Defender Firewall usually asks the first time".to_owned(),
        "reload it to see what changed in the viewer since".to_owned(),
        "do not refresh by themselves".to_owned(),
        "The browser can:".to_owned(),
        "The browser cannot:".to_owned(),
        format!("**{}** under **{}**", strings::MENU_ABOUT, strings::ABOUT_BROWSER_OTHER_DEVICES),
    ] {
        assert!(flat.contains(&statement), "the Browser section does not say: {statement}");
    }
    // The sheet's own note says the same about reloading and the firewall.
    assert!(strings::ABOUT_BROWSER_RELOAD.contains("reload it") && strings::BROWSER_VIEW_LAN_NOTE.contains("firewall"));
    // No route of the API changes a setting: the table has none, and no file of the server
    // calls the one function that writes settings.json (Save of Browser view does, in app/src).
    assert!(routes::API_ROUTES.iter().all(|(_, path, _)| !path.contains("setting")));
    let server = root().join("core").join("src").join("web");
    let mut files = 0;
    for entry in std::fs::read_dir(&server).expect("core/src/web can be listed").flatten() {
        let source = std::fs::read_to_string(entry.path()).expect("a source file");
        assert!(!source.contains("update_settings_file"), "{} must not write settings.json", entry.path().display());
        files += 1;
    }
    assert!(files >= 5 && read("app/src/web.rs").contains("update_settings_file"), "Save of Browser view is the one writer");
}

#[test]
fn the_help_names_the_default_addresses_of_the_code() {
    let help = shortcuts::help_text().split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(help.contains(&format!("http://127.0.0.1:{}/ by default", web_server::DEFAULT_FRONTEND_PORT)), "--help names the page address");
    let purpose = |name: &str| shortcuts::OPTIONS.iter().find(|option| shortcuts::option_name(option) == name).map(|option| option.purpose).unwrap_or_default();
    assert!(purpose("--frontend-port").contains(&format!("({} by default)", web_server::DEFAULT_FRONTEND_PORT)));
    assert!(purpose("--backend-port").contains(&format!("({} by default)", web_server::DEFAULT_BACKEND_PORT)));
    assert!(purpose("--host").contains("instead of 127.0.0.1"));
    let template = read("build.ps1");
    assert!(template.contains(&format!("http://127.0.0.1:{}/", web_server::DEFAULT_FRONTEND_PORT)) && template.contains(&format!("(default: {})", web_server::DEFAULT_BACKEND_PORT)), "the README.txt template names the default ports");
}

// --- Links and the vendored crate ----------------------------------------------------------------

/// The anchor GitHub gives a heading: lower case, spaces as hyphens, punctuation left out.
fn anchor(heading: &str) -> String {
    heading
        .trim_start_matches('#')
        .trim()
        .chars()
        .filter_map(|character| match character {
            ' ' => Some('-'),
            '-' | '_' => Some(character),
            other if other.is_alphanumeric() => Some(other.to_ascii_lowercase()),
            _ => None,
        })
        .collect()
}

#[test]
fn every_link_inside_the_readme_leads_to_a_heading() {
    assert_eq!(anchor("## Download post and Download all"), "download-post-and-download-all");
    assert_eq!(anchor("### What the browser can and cannot do"), "what-the-browser-can-and-cannot-do");
    let headings: BTreeSet<String> = lines_with_fences(readme()).into_iter().filter(|(line, fenced)| !fenced && line.starts_with('#')).map(|(line, _)| anchor(line)).collect();
    let text = prose(readme());
    let mut checked = 0;
    for (start, _) in text.match_indices("](#") {
        let target = text[start + 3..].split(')').next().unwrap_or_default();
        assert!(headings.contains(target), "README.md links to #{target}, but no heading has that anchor");
        checked += 1;
    }
    assert!(checked >= 20, "only {checked} links inside the README were found");
}

#[test]
fn the_vendored_egui_wgpu_is_the_one_in_the_build() {
    // The workspace puts the copy in vendor/ in place of the published crate, and Cargo.lock
    // records it as a path package. After an upgrade of egui the patch would silently stop
    // applying (cargo only warns), and the shader of a computer without a graphics card would be
    // the slow one again: this test fails instead.
    let workspace = read("Cargo.toml");
    assert!(workspace.contains("[patch.crates-io]\negui-wgpu = { path = \"vendor/egui-wgpu\" }"), "Cargo.toml patches egui-wgpu with vendor/egui-wgpu");
    let vendored = read("vendor/egui-wgpu/Cargo.toml");
    let version_of = |manifest: &str| manifest.lines().find_map(|line| line.strip_prefix("version = \"")).map(|rest| rest.trim_end_matches('"').to_owned()).unwrap_or_default();
    let version = version_of(&vendored);
    assert!(vendored.contains("name = \"egui-wgpu\""));
    assert!(workspace.contains(&format!("eframe = {{ version = \"{version}\"")), "vendor/egui-wgpu {version} is the version of eframe");
    let lock = read("Cargo.lock");
    let entry = lock.split("[[package]]").find(|block| block.contains("name = \"egui-wgpu\"\n")).expect("Cargo.lock lists egui-wgpu");
    assert!(entry.contains(&format!("version = \"{version}\"")) && !entry.contains("source = "), "Cargo.lock takes egui-wgpu from vendor/, not from the registry:{entry}");
    assert_eq!(lock.matches("name = \"egui-wgpu\"\n").count(), 1, "one egui-wgpu in the build");

    // The change itself: entry points without a hardware sample, chosen by the option.
    let shader = read("vendor/egui-wgpu/src/egui.wgsl");
    let patched = shader.split("instagram-windows-app patch").nth(1).expect("the shader carries the patch");
    for entry_point in ["fn fs_main_linear_framebuffer_in_shader", "fn fs_main_gamma_framebuffer_in_shader"] {
        assert!(patched.contains(entry_point), "{entry_point}");
    }
    // Its comments speak of the sample; its code has none.
    let code: String = patched.lines().map(|line| line.split("//").next().unwrap_or_default()).collect::<Vec<_>>().join("\n");
    assert!(code.contains("textureLoad(") && !code.contains("textureSample"), "the patched entry points load texels and contain no hardware sample");
    let renderer = read("vendor/egui-wgpu/src/renderer.rs");
    assert!(renderer.contains("(false, true) => \"fs_main_gamma_framebuffer_in_shader\""), "the renderer chooses them for predictable_texture_filtering");
    let patch = read("vendor/egui-wgpu.patch");
    assert!(patch.contains("+fn fs_main_gamma_framebuffer_in_shader") && patch.contains("predictable_texture_filtering"), "vendor/egui-wgpu.patch is the change");
    assert!(read("app/src/rendering.rs").contains("predictable_texture_filtering: true"), "the app asks for it");

    // Where people read about it.
    let third_party = section("## Third-party software");
    assert!(third_party.contains("`vendor/egui-wgpu`") && third_party.contains("`vendor/README.md`"));
    assert!(read("vendor/README.md").contains(&format!("`egui-wgpu` {version}")));
}

#[test]
fn the_vendored_pages_are_what_vendored_json_lists() {
    let vendored: serde_json::Value = serde_json::from_str(&read("web/VENDORED.json")).expect("web/VENDORED.json is JSON");
    assert_eq!(vendored["source"], serde_json::Value::from("instagram-clone/frontend"));
    assert!(vendored["source_sha256"].as_str().is_some_and(|hash| hash.len() == 64));
    let listed: BTreeMap<String, u64> = vendored["files"].as_array().expect("files").iter().map(|file| (file["path"].as_str().unwrap_or_default().to_owned(), file["bytes"].as_u64().unwrap_or(0))).collect();
    let dist = root().join("web").join("dist");
    let mut on_disk: BTreeMap<String, u64> = BTreeMap::new();
    let mut pending = vec![dist.clone()];
    while let Some(folder) = pending.pop() {
        for entry in std::fs::read_dir(&folder).expect("web/dist can be listed").flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else {
                let relative = path.strip_prefix(&dist).expect("inside").to_string_lossy().replace('\\', "/");
                on_disk.insert(relative, entry.metadata().map(|metadata| metadata.len()).unwrap_or(0));
            }
        }
    }
    assert_eq!(on_disk, listed, "web/dist and web/VENDORED.json differ: run update-web.bat");
    // Everything is embedded, and the licence index of the pages is among it.
    let embedded: BTreeSet<&str> = instagram_windows_app::web_assets::WEB_FILES.iter().map(|file| file.path).collect();
    assert_eq!(embedded, listed.keys().map(String::as_str).collect::<BTreeSet<&str>>());
    assert!(listed.contains_key("licenses/README.txt") && listed.contains_key("index.html"));
    // The end-to-end script is where the README says, with its own notes.
    assert!(root().join("web").join("e2e").join("web-e2e.mjs").is_file() && root().join("web").join("e2e").join("README.md").is_file());
}

// --- The repository ------------------------------------------------------------------------------

#[test]
fn the_project_layout_names_what_is_there() {
    let layout = section("## Project layout");
    let rows: Vec<&str> = layout.lines().filter(|line| !line.starts_with("```") && !line.trim().is_empty()).collect();
    assert!(rows.len() >= 12);
    for row in rows {
        let (paths, purpose) = row.split_once("  ").unwrap_or_else(|| panic!("a layout row is a path, spaces and a purpose: {row}"));
        assert!(!purpose.trim().is_empty(), "{paths} has no purpose");
        for path in paths.split(" / ") {
            assert!(root().join(path.trim()).exists(), "README.md lists {path} under Project layout, but it does not exist");
        }
    }
}

#[test]
fn the_quick_start_uses_the_scripts_of_the_project() {
    let quick_start = section("## Quick start");
    let build = section("## Build");
    for (text, script) in [(&quick_start, "setup.bat"), (&quick_start, "run.bat"), (&build, "build.bat"), (&build, "update-web.bat")] {
        assert!(text.contains(&format!(".\\{script}")), "{script} is shown with its command");
        let content = read(script);
        let lines: Vec<&str> = content.lines().collect();
        // The shape of every wrapper: quiet, local, in its own folder, arguments and exit code
        // passed on.
        assert_eq!(&lines[..3], ["@echo off", "setlocal", "cd /d \"%~dp0\""], "{script}");
        assert_eq!(lines.last().copied(), Some("exit /b %ERRORLEVEL%"), "{script}");
        assert!(content.contains("%*"), "{script} passes its arguments on");
        assert!(content.is_ascii(), "{script} is plain ASCII");
    }
    for script in ["setup.ps1", "build.ps1", "update-web.ps1"] {
        // Windows PowerShell 5.1 reads a script without a byte order mark in the code page of
        // the system: anything outside ASCII would be misread.
        assert!(read(script).is_ascii(), "{script} is plain ASCII");
    }
    assert!(read("run.bat").contains("cargo run --locked -p instagram-windows-app -- %*"));
    assert!(read("setup.ps1").contains("'fetch', '--locked'") && quick_start.contains("`cargo fetch --locked`"));
    let builder = read("build.ps1");
    for option in ["--skip-tests", "--profile-only"] {
        assert!(builder.contains(&format!("'{option}'")) && build.contains(option), "{option} of build.bat");
    }
    assert!(builder.contains("@('test', '--locked', '--workspace')") && build.contains("`cargo test --locked --workspace`"));
    // The static C runtime the Build section speaks of.
    assert!(read(".cargo/config.toml").contains("target-feature=+crt-static"));
    // The licence is the one the README names.
    assert!(read("LICENSE").contains("Apache License") && read("LICENSE").contains("Version 2.0, January 2004"));
}

// --- The crates and the generated files of a package ---------------------------------------------

/// One crate of the Windows build of the exe, as `cargo tree` lists it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct LinkedCrate {
    name: String,
    version: String,
    licence: String,
    repository: String,
}

/// The crates in the Windows build of the exe: third-party ones only, each once.
fn linked_crates() -> &'static Vec<LinkedCrate> {
    static CRATES: OnceLock<Vec<LinkedCrate>> = OnceLock::new();
    CRATES.get_or_init(|| {
        let cargo = std::env::var_os("CARGO").unwrap_or_else(|| OsString::from("cargo"));
        let output = Command::new(cargo)
            .args(["tree", "--locked", "-p", settings::APP_NAME, "--target", "x86_64-pc-windows-msvc", "-e", "normal", "--prefix", "none", "--format", "{p}|{l}|{r}"])
            .current_dir(root())
            .output()
            .expect("cargo tree can be started");
        assert!(output.status.success(), "cargo tree failed: {}", String::from_utf8_lossy(&output.stderr));
        let mut crates = BTreeSet::new();
        for line in String::from_utf8_lossy(&output.stdout).lines() {
            // A crate that was listed before is marked with "(*)" at the end of its line.
            let mut fields = line.trim_end_matches(" (*)").split('|');
            let mut package = fields.next().unwrap_or_default().split(' ');
            let name = package.next().unwrap_or_default().to_owned();
            let version = package.next().unwrap_or_default().trim_start_matches('v').to_owned();
            // The packages of this workspace are the program itself.
            if name.is_empty() || name == settings::APP_NAME || name == "instagram-core" {
                continue;
            }
            crates.insert(LinkedCrate { name, version, licence: fields.next().unwrap_or_default().to_owned(), repository: fields.next().unwrap_or_default().to_owned() });
        }
        assert!(crates.len() > 100, "cargo tree lists the crates of the exe");
        crates.into_iter().collect()
    })
}

#[test]
fn the_third_party_table_matches_the_crates_in_the_exe() {
    let third_party = section("## Third-party software");
    let rows = table(&third_party, "Package").rows;
    assert!(rows.len() >= 8);
    for row in rows {
        // "[name](address) version (...)"
        let name = row[0].split(']').next().unwrap_or_default().trim_start_matches('[');
        let address = row[0].split('(').nth(1).and_then(|rest| rest.split(')').next()).unwrap_or_default();
        let version = row[0].split(") ").nth(1).and_then(|rest| rest.split(' ').next()).unwrap_or_default();
        let found: Vec<&LinkedCrate> = linked_crates().iter().filter(|linked| linked.name == name && (linked.version == version || linked.version.starts_with(&format!("{version}.")))).collect();
        assert!(!found.is_empty(), "README.md lists {name} {version}, which is not in the exe (cargo tree has {:?})", linked_crates().iter().filter(|linked| linked.name == name).map(|linked| &linked.version).collect::<Vec<_>>());
        for linked in found {
            assert_eq!(row[2], linked.licence, "the licence of {name}");
            assert_eq!(address, linked.repository, "the address of {name}");
        }
        // The other crates named in the row are in the exe as well.
        for other in spans(&row[0], "`") {
            assert!(linked_crates().iter().any(|linked| linked.name == other), "README.md names the crate {other}, which is not in the exe");
        }
    }
    let all = readme().split_whitespace().collect::<Vec<_>>().join(" ");
    let rounded = (linked_crates().len() + 5) / 10 * 10;
    assert!(all.contains(&format!("built from about {rounded} Rust crates")) && all.contains(&format!("compiles about {rounded} crates")), "the exe is built from {} third-party crates", linked_crates().len());
}

/// The generated files of a package (`Debug/README.txt`, `Release/README.txt` and `licenses/`),
/// by their path in the package with `/` between its parts. The build script itself writes them
/// into a folder of this test run; the tests that share them may run at any time of the run, so
/// they are read into memory and the folder goes at once.
#[cfg(windows)]
fn package_files() -> &'static BTreeMap<String, Vec<u8>> {
    static FILES: OnceLock<BTreeMap<String, Vec<u8>>> = OnceLock::new();
    FILES.get_or_init(|| {
        let folder = TestFolder::new("docs-package").expect("a folder for the package files");
        let system = std::env::var_os("SystemRoot").unwrap_or_else(|| OsString::from("C:\\Windows"));
        let shell = Path::new(&system).join("System32").join("WindowsPowerShell").join("v1.0").join("powershell.exe");
        let output = Command::new(shell)
            .args(["-NoLogo", "-NoProfile", "-ExecutionPolicy", "Bypass", "-File"])
            .arg(root().join("build.ps1"))
            .arg("--package-files")
            .arg(folder.path())
            .current_dir(root())
            .output()
            .expect("PowerShell can be started");
        assert!(output.status.success(), "build.ps1 --package-files failed:\n{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        let mut files = BTreeMap::new();
        let mut pending = vec![folder.to_path_buf()];
        while let Some(next) = pending.pop() {
            for entry in std::fs::read_dir(&next).expect("the package files can be listed").flatten() {
                let path = entry.path();
                if path.is_dir() {
                    pending.push(path);
                } else {
                    let relative = path.strip_prefix(&*folder).expect("inside the folder").to_string_lossy().replace('\\', "/");
                    files.insert(relative, std::fs::read(&path).unwrap_or_else(|error| panic!("{} cannot be read: {error}", path.display())));
                }
            }
        }
        files
    })
}

/// One generated file of a package, by its path in the package (`package_files`).
#[cfg(windows)]
fn package_file(path: &str) -> &'static [u8] {
    package_files().get(path).map(Vec::as_slice).unwrap_or_else(|| panic!("the package has no {path}"))
}

/// Checks the rules of a generated text file and returns its lines: plain ASCII, Windows line
/// ends, no line over 78 columns unless it is one word that cannot be broken.
#[cfg(windows)]
fn generated_lines(path: &str) -> Vec<String> {
    let bytes = package_file(path);
    assert!(bytes.is_ascii(), "{path} is plain ASCII");
    let text = String::from_utf8(bytes.to_vec()).expect("ASCII is UTF-8");
    assert!(text.ends_with("\r\n"), "{path} ends with a line end");
    assert_eq!(text.matches('\n').count(), text.matches("\r\n").count(), "every line of {path} ends with CR LF");
    let lines: Vec<String> = text.split("\r\n").map(str::to_owned).collect();
    for line in &lines {
        assert!(line.len() <= 78 || !line.trim().contains(' '), "this line of {path} is longer than 78 columns: {line}");
        assert!(!line.contains('\t') && !line.contains('{') && !line.contains('}'), "this line of {path} has a tab or a placeholder: {line}");
    }
    lines
}

#[cfg(windows)]
#[test]
fn the_readme_of_a_package_follows_the_rules_and_states_the_facts() {
    for (profile, note) in [("Debug", "a console window opens with the program and shows its log"), ("Release", "optimised, without a console window")] {
        let lines = generated_lines(&format!("{profile}/README.txt"));
        let title = format!("{} {} - portable {profile} build", messages::APP_TITLE, env!("CARGO_PKG_VERSION"));
        assert_eq!(lines[0], title);
        assert_eq!(lines[1], "=".repeat(title.len()), "the title is underlined over its whole length");
        // A heading is a line followed by a row of dashes of the same length.
        let headings: Vec<&str> = lines.windows(2).filter(|pair| !pair[1].is_empty() && pair[1].chars().all(|character| character == '-')).map(|pair| pair[0].as_str()).collect();
        assert_eq!(headings, ["Start", "Opening pictures", "Browser pages", "Open with", "Settings and descriptions", "Command line", "Licences"]);
        for pair in lines.windows(2).filter(|pair| !pair[1].is_empty() && pair[1].chars().all(|character| character == '-')) {
            assert_eq!(pair[0].len(), pair[1].len(), "the heading \"{}\" is underlined over its whole length", pair[0]);
        }

        let text = lines.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
        let exe = format!("{}.exe", settings::APP_NAME);
        let mut facts = vec![
            note.to_owned(),
            format!("{exe} is the whole program"),
            format!("%APPDATA%\\{}\\{}", settings::APP_NAME, settings::SETTINGS_FILE_NAME),
            format!("%APPDATA%\\{}\\{}\\", settings::APP_NAME, settings::DATA_FOLDER_NAME),
            format!("\"{}\"", settings::KEY_DATA_DIRECTORY),
            format!("\"{}\"", settings::KEY_DEFAULT_USERNAME),
            settings::DESCRIPTIONS_FILE_NAME.to_owned(),
            settings::STATE_FILE_NAME.to_owned(),
            settings::WINDOW_FILE_NAME.to_owned(),
            settings::LOG_FILE_NAME.to_owned(),
            "never writes into the folders of your pictures".to_owned(),
            "licenses\\README.txt".to_owned(),
            "Apache License 2.0".to_owned(),
            "Files you add to this folder are kept when the build is updated.".to_owned(),
            format!("{} to {} seconds", autoview::MIN_INTERVAL_SEC, autoview::MAX_INTERVAL_SEC),
            // Archives: what opens, where their files go, and the limits of the cache.
            ".zip, .tar, .tar.gz or .tgz".to_owned(),
            "never next to the archive".to_owned(),
            format!("%LOCALAPPDATA%\\{}\\{}\\", settings::APP_NAME, settings::ARCHIVE_CACHE_FOLDER_NAME),
            format!("at most {}, nothing unused for {} days, and {} of the drive stays free", text::format_bytes(cache::ARCHIVE_CACHE_MAX_BYTES), cache::ARCHIVE_CACHE_MAX_AGE_DAYS, text::format_bytes(cache::ARCHIVE_CACHE_MIN_FREE_BYTES)),
            // The browser pages: where, how to open them, LAN and the firewall, the settings.
            format!("http://127.0.0.1:{}/", web_server::DEFAULT_FRONTEND_PORT),
            format!("More > {}", strings::MENU_OPEN_IN_BROWSER),
            format!("More > {}", strings::MENU_BROWSER_VIEW),
            format!("\"{}\"", strings::BROWSER_VIEW_LAN),
            "Windows Defender Firewall".to_owned(),
            format!("Browser view also sets the port of the page address ({}) and the port of the API address ({}) and saves them in settings.json", web_server::DEFAULT_FRONTEND_PORT, web_server::DEFAULT_BACKEND_PORT),
            "Reload the page to see what changed here".to_owned(),
            "The browser never changes or deletes your pictures, folders or archives.".to_owned(),
            format!("%LOCALAPPDATA%\\{}\\{}\\", settings::APP_NAME, settings::LIBRARY_FOLDER_NAME),
            format!("\"{}\"", settings::KEY_WEB_SERVER),
            format!("\"{}\"", settings::KEY_FRONTEND_PORT),
            format!("\"{}\"", settings::KEY_BACKEND_PORT),
            format!("\"{}\"", settings::KEY_WEB_HOST),
            format!("\"{}\"", settings::KEY_ALLOWED_HOSTS),
            format!("\"{}\"", settings::KEY_LIBRARY_DIRECTORY),
            "licenses\\web\\README.txt".to_owned(),
            // Videos.
            "Videos (MP4, MOV, WebM) play in the viewer, and in the feed with a graphics card (without one the feed shows the first frame with \"Play video\")".to_owned(),
            // Every folder the program fills under %LOCALAPPDATA%.
            format!("%LOCALAPPDATA%\\{}\\{}\\", settings::APP_NAME, settings::WEB_CACHE_FOLDER_NAME),
            "The web cache holds JPEG copies of HEIC pictures for the browser pages".to_owned(),
            // What the pages name as the place of imports.
            "they name the browser imports folder, never your own exports".to_owned(),
            format!("%LOCALAPPDATA%\\{}\\{}\\", settings::APP_NAME, settings::POSTER_CACHE_FOLDER_NAME),
            "never next to the video".to_owned(),
        ];
        // Every everyday option, and the labels the text names.
        facts.extend(shortcuts::OPTIONS.iter().map(|option| option.syntax.to_owned()));
        facts.extend([strings::NAV_OPEN_PICTURES, strings::NAV_OPEN_FOLDER, strings::SHEET_EDIT_DESCRIPTION, strings::SHEET_DOWNLOAD_POST, strings::BANNER_DOWNLOAD_ALL].map(str::to_owned));
        for fact in facts {
            assert!(text.contains(&fact), "README.txt of the {profile} build does not say: {fact}");
        }
    }
}

/// One block of the licence index: the first line (name, version, licence) and the files.
#[cfg(windows)]
#[derive(Debug, Default)]
struct IndexBlock {
    heading: String,
    licence: String,
    files: Vec<String>,
}

/// The blocks of the licence index. A block starts with a line at the left edge that is not a
/// section heading; its licence stands at the right edge of that line, or alone on the next
/// one; its files are the lines indented by six spaces.
#[cfg(windows)]
fn index_blocks(lines: &[String]) -> Vec<IndexBlock> {
    let mut blocks: Vec<IndexBlock> = Vec::new();
    let mut in_blocks = false;
    for (number, line) in lines.iter().enumerate() {
        let is_rule = |text: &String| !text.is_empty() && text.chars().all(|character| character == '-' || character == '=');
        if lines.get(number + 1).is_some_and(is_rule) {
            // A section heading. The blocks start after the introduction under the title.
            in_blocks = line != "Third-party software in this build";
            continue;
        }
        if !in_blocks || line.is_empty() || is_rule(line) {
            continue;
        }
        if !line.starts_with(' ') {
            // "name version      licence": the licence follows a run of spaces.
            let (heading, licence) = match line.find("  ") {
                Some(gap) => (line[..gap].to_owned(), line[gap..].trim().to_owned()),
                None => (line.clone(), String::new()),
            };
            blocks.push(IndexBlock { heading, licence, files: Vec::new() });
            continue;
        }
        let block = blocks.last_mut().expect("an indented line belongs to a block");
        // A licence too long for the first line stands alone at the right edge of the next one.
        let alone_at_the_right = number > 0 && lines[number - 1] == block.heading && line.len() == 78 && line.starts_with("          ");
        if alone_at_the_right {
            block.licence = line.trim().to_owned();
        } else if let Some(file) = line.strip_prefix("      ") {
            block.files.push(file.trim().to_owned());
        }
    }
    blocks
}

#[cfg(windows)]
#[test]
fn the_licence_index_covers_every_crate_in_the_exe() {
    let lines = generated_lines("licenses/README.txt");
    assert_eq!(lines[0], "Third-party software in this build");
    let blocks = index_blocks(&lines);
    let by_heading: BTreeMap<&str, &IndexBlock> = blocks.iter().map(|block| (block.heading.as_str(), block)).collect();

    // Every crate that cargo tree lists for the exe has a block with its licence and a text.
    for linked in linked_crates() {
        let heading = format!("{} {}", linked.name, linked.version);
        let block = by_heading.get(heading.as_str()).unwrap_or_else(|| panic!("the licence index has no block for the crate {heading}"));
        assert_eq!(block.licence, linked.licence, "the licence of {heading}");
        assert!(!block.files.is_empty(), "the licence index names no licence text for {heading}");
    }
    // And the index lists nothing else as a crate.
    let crates_at = lines.iter().position(|line| line == "Rust crates").expect("the index has a section for the crates");
    let listed = index_blocks(&lines[crates_at..]).len();
    assert_eq!(listed, linked_crates().len(), "the index lists as many crates as cargo tree");

    // The program itself, the Rust standard library and the fonts inside the exe.
    let own = by_heading.get(format!("{} {}", settings::APP_NAME, env!("CARGO_PKG_VERSION")).as_str()).expect("the program itself is listed");
    assert_eq!((own.licence.as_str(), own.files.as_slice()), ("Apache-2.0", ["LICENSE.txt".to_owned()].as_slice()));
    assert_eq!(package_file("licenses/LICENSE.txt"), std::fs::read(root().join("LICENSE")).expect("LICENSE"), "the licence of the program is shipped as it is");
    assert_eq!(by_heading.get("Rust standard library").expect("the standard library is listed").licence, "MIT OR Apache-2.0");
    for font in ["Ubuntu Light", "Hack"] {
        assert!(!by_heading.get(font).unwrap_or_else(|| panic!("the font {font} is listed")).files.is_empty());
    }
    // The crate built from vendor/ is third-party software like the others, and says so.
    let vendored = lines.iter().position(|line| line.starts_with("egui-wgpu ")).expect("egui-wgpu is listed");
    let block = lines[vendored..].iter().take_while(|line| !line.is_empty()).cloned().collect::<Vec<_>>().join(" ");
    assert!(block.contains("Built from the copy in the folder vendor"), "the block of egui-wgpu says it is built from vendor: {block}");

    // Every file the index names is there and not empty; nothing is there that it does not name.
    let mut named: BTreeSet<String> = blocks.iter().flat_map(|block| block.files.iter().cloned()).collect();
    for file in &named {
        let size = package_files().get(&format!("licenses/{file}")).unwrap_or_else(|| panic!("the index names {file}, which is not in the folder")).len();
        assert!(size > 0, "{file} is empty");
    }
    named.insert("README.txt".to_owned());
    for relative in package_files().keys().filter_map(|path| path.strip_prefix("licenses/")) {
        assert!(named.contains(relative), "licenses/{relative} is shipped, but the index does not say what it belongs to");
    }

    // A crate that is published without a licence file gets the standard text of its licence.
    let standard: BTreeSet<String> = named.iter().filter_map(|file| file.strip_prefix("standard/")).map(str::to_owned).collect();
    assert!(standard.contains("MIT.txt") && standard.contains("Apache-2.0.txt"));
    for file in standard {
        assert_eq!(package_file(&format!("licenses/standard/{file}")), std::fs::read(root().join("licenses").join("standard").join(&file)).expect("in the project"), "standard/{file} is the text of the project");
    }
}
