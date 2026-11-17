//! Every label, tooltip and message the user interface itself shows.
//!
//! The texts are verbatim from the reference web project where it has one (the references name
//! the file); texts marked NEW exist only in this app, because the web project cannot open a
//! picture without a `post.json`. Sentences the core crate produces (warnings, errors, the banner
//! line, the download messages) live in `instagram_core::messages`. Keeping all of them in two
//! modules lets a documentation test compare the README with what the app really says.

use instagram_core::messages;
use instagram_core::text::{format_count, format_count_exact};

// --- Window and dialogs ---------------------------------------------------------------------

/// The window title while no picture is in the viewer, and the title of the message box of a
/// fatal settings error.
pub const APP_TITLE: &str = messages::APP_TITLE;
/// NEW. The title of the file dialog of "Open pictures".
pub const DIALOG_OPEN_PICTURES: &str = "Open pictures";
/// NEW. The title of the folder dialog of "Open folder".
pub const DIALOG_OPEN_FOLDER: &str = "Open folder";
/// NEW. The first filter of "Open pictures": the picture and video extensions and the export
/// archives.
pub const FILTER_PICTURES_AND_EXPORTS: &str = "Pictures, videos and exports";
/// NEW. The name of the file-dialog filter that lists the picture and video extensions.
pub const FILTER_PICTURES: &str = "Pictures and videos";
/// NEW. The file-dialog filter of the export archives (.zip, .tar, .tgz, .tar.gz).
pub const FILTER_EXPORT_ARCHIVES: &str = "Export archives";
/// NEW. The name of the file-dialog filter that shows every file.
pub const FILTER_ALL_FILES: &str = "All files";

// --- Sidebar (Sidebar.tsx) ------------------------------------------------------------------

/// The wordmark, and the accessible name of the logo.
pub const WORDMARK: &str = "Instagram";
/// The accessible name of the navigation column.
pub const NAV_LABEL: &str = "Main navigation";
pub const NAV_HOME: &str = "Home";
pub const NAV_EXPLORE: &str = "Explore";
/// NEW. Takes the place of the web project's "Import".
pub const NAV_OPEN_PICTURES: &str = "Open pictures";
/// NEW.
pub const NAV_OPEN_FOLDER: &str = "Open folder";
pub const NAV_AUTO_VIEW: &str = "Auto view";
pub const NAV_MORE: &str = "More";

// --- More menu (Sidebar.tsx:125-173) --------------------------------------------------------

pub const MENU_AUTO_VIEW: &str = "Auto view";
pub const MENU_SWITCH_APPEARANCE: &str = "Switch appearance";
/// NEW (web brief 4.12). Opens the page address in the default browser; enabled while the
/// browser pages run.
pub const MENU_OPEN_IN_BROWSER: &str = "Open in browser";
/// NEW (lead's addition). Opens the Browser view sheet: addresses, ports, LAN.
pub const MENU_BROWSER_VIEW: &str = "Browser view";
/// The same wording as the banner button.
pub const MENU_DOWNLOAD_ALL: &str = "Download all";
/// NEW. Opens the library folder of the Instagram clone's backend as an export folder; listed
/// only while that folder holds a post.json.
pub const MENU_OPEN_WEB_APP_LIBRARY: &str = "Open the web app's library";
/// NEW (web brief 4.10). Opens the browser imports folder; listed only while it holds a post.json
/// and is not open.
pub const MENU_OPEN_BROWSER_IMPORTS: &str = "Open browser imports";
/// NEW.
pub const MENU_CLOSE_PICTURES: &str = "Close pictures";
/// NEW.
pub const MENU_ABOUT: &str = "About";

// --- Empty state and dropping (NEW: the web project shows its demo feed instead) ---------------

pub const EMPTY_TITLE: &str = "Open pictures to get started";
pub const EMPTY_OPEN_PICTURES: &str = "Open pictures";
pub const EMPTY_OPEN_FOLDER: &str = "Open folder";
pub const EMPTY_HINT: &str = "or drop pictures, videos or a folder here";
/// The title of the overlay while files are dragged over the window.
pub const DROP_TITLE: &str = "Drop pictures, videos or a folder";
/// The chip at the bottom of the window while a folder is being read.
pub const OPENING_PICTURES: &str = "Opening pictures…";

// --- Banner above the feed (App.tsx:1159-1252) -----------------------------------------------

/// The accessible name of the banner.
pub const BANNER_LABEL: &str = "Opened pictures";
pub const BANNER_AUTO_VIEW: &str = "Auto view";
pub const BANNER_DOWNLOAD_ALL: &str = "Download all";
/// NEW. Takes the place of "Import more".
pub const BANNER_OPEN_MORE: &str = "Open more";
/// NEW. Takes the place of "Clear import".
pub const BANNER_CLOSE: &str = "Close";
/// The tooltip of "Download all".
pub const BANNER_DOWNLOAD_ALL_TOOLTIP: &str = messages::DOWNLOAD_ALL_TOOLTIP;

// --- Post card (Feed.tsx, Media.tsx) -----------------------------------------------------------

pub const POST_MORE_OPTIONS: &str = "More options";
pub const POST_LIKE: &str = "Like";
pub const POST_UNLIKE: &str = "Unlike";
pub const POST_COMMENT: &str = "Comment";
pub const POST_SHARE: &str = "Share";
pub const POST_SAVE: &str = "Save";
pub const POST_UNSAVE: &str = "Remove from saved";
pub const POST_PREVIOUS_PHOTO: &str = "Previous photo";
pub const POST_NEXT_PHOTO: &str = "Next photo";
pub const POST_VERIFIED: &str = "Verified";
/// The button that expands a collapsed caption; there is no "less".
pub const POST_CAPTION_MORE: &str = "more";
/// What follows a collapsed caption, before `POST_CAPTION_MORE`.
pub const POST_CAPTION_ELLIPSIS: &str = "… ";
/// The accessible name of the video badge.
pub const POST_VIDEO: &str = "Video";

// --- Videos (Video.tsx; NEW for the controls the browser draws itself) -------------------------

/// The feed's sound button while the sound is off (`Video.tsx`).
pub const VIDEO_TURN_SOUND_ON: &str = "Turn sound on";
/// The feed's sound button while the sound is on.
pub const VIDEO_TURN_SOUND_OFF: &str = "Turn sound off";
/// NEW. The play button of the controls bar.
pub const VIDEO_PLAY: &str = "Play";
/// NEW. The pause button of the controls bar.
pub const VIDEO_PAUSE: &str = "Pause";
/// NEW. The sound button of the controls bar while the sound is on.
pub const VIDEO_MUTE: &str = "Mute";
/// NEW. The sound button of the controls bar while the sound is off.
pub const VIDEO_UNMUTE: &str = "Unmute";
/// NEW. The volume slider of the controls bar.
pub const VIDEO_VOLUME: &str = "Volume";
/// NEW. The seek bar of the controls bar.
pub const VIDEO_POSITION: &str = "Video position";
/// NEW. The play badge of a feed video on a software renderer: it opens the viewer.
pub const VIDEO_PLAY_BADGE: &str = "Play video";
/// The button of the panel of a video that cannot be played (the clone's "Download video": the
/// file is already on this computer).
pub const VIDEO_SHOW_IN_FOLDER: &str = SHEET_SHOW_IN_FOLDER;
/// The separator between the username and the time in the header.
pub const POST_TIME_SEPARATOR: &str = "·";
/// The emoji on the placeholder of a picture that could not be shown.
pub const PLACEHOLDER_PHOTO_EMOJI: &str = "📷";
/// The emoji on the placeholder of a video without a cover picture.
pub const PLACEHOLDER_VIDEO_EMOJI: &str = "🎬";

/// The likes line of a post whose count is known: "1 like", "1,234 likes" as "1.2K likes"
/// (Feed.tsx:338-351). Returns the bold part and the plain rest.
pub fn likes_line(count: u64) -> (String, &'static str) {
    (format_count(count), if count == 1 { " like" } else { " likes" })
}

/// The likes line of a liked post whose count is unknown: "Liked by " + bold "you".
pub const LIKED_BY: &str = "Liked by ";
pub const LIKED_BY_YOU: &str = "you";

/// "View 1 comment" / "View all 1.2K comments" (Feed.tsx:307).
pub fn view_comments(count: u64) -> String {
    if count == 1 {
        "View 1 comment".to_owned()
    } else {
        format!("View all {} comments", format_count(count))
    }
}

/// The tooltip and accessible name of the link icon in a post header: "View on Instagram".
pub fn view_on(site: &str) -> String {
    format!("View on {site}")
}

/// The tooltip of the time label of a post with a link (Media.tsx:217-231).
pub fn open_post_on(site: &str) -> String {
    format!("Open this post on {site}")
}

/// The accessible name of the media area of a card: opens the viewer.
pub fn open_post_by(username: &str) -> String {
    format!("Open post by {username}")
}

/// The carousel counter: "2/5", without spaces (Media.tsx).
pub fn carousel_counter(slide: usize, total: usize) -> String {
    format!("{}/{}", format_count_exact(slide as u64), format_count_exact(total as u64))
}

/// The accessible name of dot `number` (1-based) under a carousel.
pub fn carousel_dot(number: usize, total: usize) -> String {
    format!("Photo {number} of {total}")
}

// --- Explore grid (Panels.tsx:292-366, Media.tsx:190-214) -----------------------------------------

/// The heading above the grid.
pub const EXPLORE_TITLE: &str = "Explore";
/// The accessible name of the badge of a tile with several pictures.
pub const TILE_CAROUSEL: &str = "Carousel";

// --- Viewer (PostDetail.tsx; previous / next and zoom are NEW) -------------------------------------

/// The accessible name of the viewer: "Post by {username}".
pub fn viewer_label(username: &str) -> String {
    format!("Post by {username}")
}
/// The X at the top right of the viewer.
pub const VIEWER_CLOSE: &str = "Close";
/// NEW. The round arrow at the left edge: the picture before this one.
pub const VIEWER_PREVIOUS: &str = "Previous picture";
/// NEW. The round arrow at the right edge: the picture after this one.
pub const VIEWER_NEXT: &str = "Next picture";
/// NEW. The zoom indicator: "150%".
pub fn zoom_percent(percent: u32) -> String {
    format!("{percent}%")
}
/// "3 likes" under a comment (PostDetail.tsx): shown when the comment has any.
pub fn comment_likes(count: u64) -> String {
    format!("{} {}", format_count(count), if count == 1 { "like" } else { "likes" })
}
/// The link under the comments of a post that has more of them on its site: "View 1 comment on
/// Instagram" / "View all 1,234 comments on Instagram" (PostDetail.tsx).
pub fn view_comments_on(count: u64, site: &str) -> String {
    if count == 1 {
        format!("View 1 comment on {site}")
    } else {
        format!("View all {} comments on {site}", format_count_exact(count))
    }
}

// --- Post options (ActionSheet.tsx) --------------------------------------------------------------

/// The accessible name of the action sheet.
pub const SHEET_LABEL: &str = "Post options";
/// "View profile on Instagram".
pub fn view_profile_on(site: &str) -> String {
    format!("View profile on {site}")
}
pub const SHEET_COPY_LINK: &str = "Copy link";
pub const SHEET_GO_TO_POST: &str = "Go to post";
pub const SHEET_EDIT_DESCRIPTION: &str = "Edit description";
pub const SHEET_DOWNLOAD_POST: &str = "Download post";
/// NEW. Takes the place of "About this account": opens Explorer with the file selected.
pub const SHEET_SHOW_IN_FOLDER: &str = "Show in folder";
pub const SHEET_AUTO_VIEW_FROM_HERE: &str = "Auto view from here";
pub const SHEET_CANCEL: &str = "Cancel";

// --- Edit description (EditCaptionDialog.tsx) ------------------------------------------------------

pub const EDIT_TITLE: &str = "Edit description";
/// The line under the title: "Post by " followed by the username in bold.
pub const EDIT_POST_BY: &str = "Post by ";
/// The accessible name of the field.
pub const EDIT_FIELD_LABEL: &str = "Description";
/// The placeholder of the empty field.
pub const EDIT_PLACEHOLDER: &str = "Write a description…";
pub const EDIT_SAVE: &str = "Save";
pub const EDIT_CANCEL: &str = "Cancel";

// --- Warnings dialog (ImportModal.tsx:56-122; the title and the button are NEW) --------------------

/// The title of the dialog that lists the warnings of what is open.
pub const WARNINGS_TITLE: &str = "Warnings";
/// The round button in its header, and the button under the list.
pub const WARNINGS_CLOSE: &str = "Close";

// --- End of the feed (Feed.tsx:396-403) ---------------------------------------------------------

pub const FEED_END_TITLE: &str = "You're all caught up";
/// NEW wording of "You’ve seen every post of this import." for an opened export.
pub const FEED_END_POSTS: &str = "You’ve seen every post of this export.";
/// NEW wording of the same line for loose pictures.
pub const FEED_END_PICTURES: &str = "You’ve seen every picture.";

// --- Auto view (AutoView.tsx: a post is a picture or a video, so "post" as the clone says) -------

/// The title of the setup dialog and the accessible name of the running show.
pub const AUTO_VIEW_TITLE: &str = "Auto view";
/// The label of the seconds field (the web project's).
pub const AUTO_VIEW_TIME_LABEL: &str = "Time per post";
/// The word after the seconds field.
pub const AUTO_VIEW_SECONDS: &str = "seconds";
/// The checkbox of the video option (`AutoView.tsx:156-159`).
pub const AUTO_VIEW_WAIT_FOR_VIDEOS: &str = "Let videos play to the end";
/// The line under it. NEW wording, shorter than the clone's ("A video stays until it ends when
/// it is longer than the time per post."), whose last word stood alone on a second line here.
pub const AUTO_VIEW_WAIT_FOR_VIDEOS_NOTE: &str = "A video longer than the time per post plays to its end.";
/// The checkbox of the loop setting (the web project's).
pub const AUTO_VIEW_LOOP: &str = "Start again after the last post";
pub const AUTO_VIEW_SHUFFLE: &str = "Shuffle the order";
pub const AUTO_VIEW_START: &str = "Start";
pub const AUTO_VIEW_CANCEL: &str = "Cancel";
/// The X of the running show: its accessible name and its tooltip.
pub const AUTO_VIEW_EXIT_LABEL: &str = "Exit auto view";
pub const AUTO_VIEW_EXIT_TOOLTIP: &str = "Exit (Esc)";
pub const AUTO_VIEW_PREVIOUS_LABEL: &str = "Previous post";
pub const AUTO_VIEW_PREVIOUS_TOOLTIP: &str = "Previous (←)";
pub const AUTO_VIEW_NEXT_LABEL: &str = "Next post";
pub const AUTO_VIEW_NEXT_TOOLTIP: &str = "Next (→)";
pub const AUTO_VIEW_PAUSE_LABEL: &str = "Pause auto view";
pub const AUTO_VIEW_PAUSE_TOOLTIP: &str = "Pause (Space)";
pub const AUTO_VIEW_RESUME_LABEL: &str = "Resume auto view";
pub const AUTO_VIEW_RESUME_TOOLTIP: &str = "Resume (Space)";
/// The stepper of the time (the web project's).
pub const AUTO_VIEW_LESS_LABEL: &str = "Less time per post";
pub const AUTO_VIEW_LESS_TOOLTIP: &str = "Less time (−)";
pub const AUTO_VIEW_MORE_LABEL: &str = "More time per post";
pub const AUTO_VIEW_MORE_TOOLTIP: &str = "More time (+)";
/// The glyphs of the stepper: U+2212 and a plus sign.
pub const AUTO_VIEW_LESS_GLYPH: &str = "−";
pub const AUTO_VIEW_MORE_GLYPH: &str = "+";
/// The pill over the picture while the show is paused.
pub const AUTO_VIEW_PAUSED: &str = "Paused";
/// Leaves the show and opens the post in the viewer.
pub const AUTO_VIEW_OPEN_POST: &str = "Open post";
pub const AUTO_VIEW_RESTART: &str = "Restart";
pub const AUTO_VIEW_EXIT: &str = "Exit";

/// "56.8K likes" / "1 like" under the description of the running show.
pub fn auto_view_likes(count: u64) -> String {
    format!("{} {}", format_count(count), if count == 1 { "like" } else { "likes" })
}

/// "1.2K comments" / "1 comment" under the description of the running show.
pub fn auto_view_comments(count: u64) -> String {
    format!("{} {}", format_count(count), if count == 1 { "comment" } else { "comments" })
}

// --- Download (NEW: the web project hands its archives to the browser) ----------------------------

/// The title of the Save dialog of "Download all".
pub const DIALOG_DOWNLOAD_ALL: &str = "Download all";
/// The title of the Save dialog of "Download post".
pub const DIALOG_DOWNLOAD_POST: &str = "Download post";
/// The name of the Save dialog's filter.
pub const FILTER_ZIP: &str = "Zip archive";

// --- Questions (ConfirmDialog.tsx; the questions themselves are NEW) ------------------------------

pub const CONFIRM_CANCEL: &str = "Cancel";
/// Closing the pictures while a download is being prepared.
pub const CONFIRM_CLOSE_TITLE: &str = "Close pictures?";
pub const CONFIRM_CLOSE_MESSAGE: &str = "A download is still being prepared. Closing the pictures cancels it.";
pub const CONFIRM_CLOSE_ACCEPT: &str = "Close pictures";
/// A download in parts over the parts of an earlier one.
pub const CONFIRM_REPLACE_TITLE: &str = "Replace the earlier download?";
pub const CONFIRM_REPLACE_ACCEPT: &str = "Replace";
/// "This download may be split into parts. The folder already holds ...".
pub fn confirm_replace_message(count: usize, first: &str) -> String {
    let more = count.saturating_sub(1) as u64;
    let files = match more {
        0 => format!("\"{first}\""),
        1 => format!("\"{first}\" and 1 more part"),
        _ => format!("\"{first}\" and {} more parts", format_count_exact(more)),
    };
    format!("This download may be split into parts. The folder already holds {files} of an earlier download, which will be replaced.")
}
/// The notice about descriptions.json (`ConfirmKind::StoreNotice`): its title and its one row.
pub const NOTICE_STORE_TITLE: &str = "Saved descriptions";
pub const NOTICE_OK: &str = "OK";

// --- Opening (NEW) ---------------------------------------------------------------------------

/// The toast when files were added to the pictures on screen and none of them could be shown:
/// the reason of the first, and how many more there are (all of them are in the warnings).
pub fn files_not_added(first: &str, more: usize) -> String {
    match more {
        0 => first.to_owned(),
        1 => format!("{first} 1 more file could not be opened."),
        _ => format!("{first} {} more files could not be opened.", format_count_exact(more as u64)),
    }
}

// --- About (NEW) -------------------------------------------------------------------------------

/// The accessible name of the About sheet.
pub const ABOUT_LABEL: &str = "About";
/// The line under the title of the About text: the version of this build.
pub fn about_version() -> String {
    format!("Version {}", env!("CARGO_PKG_VERSION"))
}
/// What the app is, in one sentence.
pub const ABOUT_SUMMARY: &str = "A viewer for pictures and Instagram exports.";
/// The line above the path of the data folder.
pub const ABOUT_DATA_LEAD: &str = "The descriptions you write are kept on this computer in";
/// The line above the path of the settings file.
pub const ABOUT_SETTINGS_LEAD: &str = "Settings file";
/// The licence note.
pub const ABOUT_LICENCE: &str = "Licensed under the Apache License 2.0. Built with egui, wgpu, image, zip and other open-source libraries. The browser pages contain React, fflate and heic-to (LGPL-3.0).";
/// Opens the data folder in Explorer.
pub const ABOUT_OPEN_DATA_FOLDER: &str = "Open data folder";
/// The last row of the About sheet.
pub const ABOUT_CLOSE: &str = "Close";

// --- Browser pages: the Browser section of About and the Browser view sheet (web brief 4.12) -

/// The heading of the Browser section of About.
pub const ABOUT_BROWSER_TITLE: &str = "Browser";
/// What the browser pages are.
pub const ABOUT_BROWSER_LEAD: &str = "The posts that are open here can also be seen in a web browser.";
/// The label before the page address.
pub const ABOUT_BROWSER_THIS_COMPUTER: &str = "On this computer";
/// The label before the API address.
pub const ABOUT_BROWSER_API: &str = "API";
/// The label before the page addresses for other devices.
pub const ABOUT_BROWSER_OTHER_DEVICES: &str = "On other devices";
/// Other devices may open the pages, but no network address was found.
pub const ABOUT_BROWSER_NO_LAN_ADDRESS: &str = "No network address was found.";
/// While the `web-start` thread binds the ports.
pub const ABOUT_BROWSER_STARTING: &str = "Starting\u{2026}";
/// The pages show what was open when they were loaded.
pub const ABOUT_BROWSER_RELOAD: &str = "The browser shows what is open when the page is loaded: reload it to see changes made here.";
/// Off by settings.json.
pub const BROWSER_OFF_SETTINGS: &str = "The browser pages are off: \"web_server\" is false in settings.json.";
/// Off by the command line.
pub const BROWSER_OFF_CLI: &str = "The browser pages are off for this run (--no-web).";
/// Off by the Browser view sheet.
pub const BROWSER_OFF_SHEET: &str = "The browser pages are off. Turn them on in Browser view.";
/// Not started in this run (tests, screenshots without an address on the command line).
pub const BROWSER_NOT_STARTED: &str = "The browser pages are not started in this run. Turn them on in Browser view.";

/// Off by the Browser view sheet, said inside that sheet (its switch is just below).
pub const BROWSER_VIEW_OFF: &str = "The browser pages are off. Turn them on below.";
/// Not started in this run, said inside the Browser view sheet.
pub const BROWSER_VIEW_NOT_STARTED: &str = "The browser pages are not started in this run. Turn them on below.";

/// The title of the Browser view sheet (its accessible name too).
pub const BROWSER_VIEW_TITLE: &str = "Browser view";
/// What the sheet is about: About's lead, short enough for one line of the sheet.
pub const BROWSER_VIEW_LEAD: &str = "The open posts can also be seen in a web browser.";
/// About's note on reloading, short enough for one line of the sheet.
pub const BROWSER_VIEW_RELOAD: &str = "Reload a page to see what changed here since it was loaded.";
/// The server switch.
pub const BROWSER_VIEW_SERVER: &str = "Show the open posts in a browser";
/// The label of the page address link.
pub const BROWSER_VIEW_PAGE_ADDRESS: &str = "Page address";
/// The label of the API address link.
pub const BROWSER_VIEW_API_ADDRESS: &str = "API address";
/// The field of the page address port.
pub const BROWSER_VIEW_PAGE_PORT: &str = "Page address port";
/// The field of the API address port.
pub const BROWSER_VIEW_API_PORT: &str = "API address port";
/// The LAN switch.
pub const BROWSER_VIEW_LAN: &str = "Allow other devices on this network";
/// What the LAN switch means, with the firewall prompt.
pub const BROWSER_VIEW_LAN_NOTE: &str = "They can see the open posts and change their descriptions. Windows may ask whether to let the app through its firewall the first time.";
/// The values the command line fixed for this run, named once at the end of the sheet: their
/// controls show the saved values (the running addresses are in the lines at the top), and Save
/// keeps what they show. `None` when the command line fixed nothing.
pub fn browser_view_from_cli(server: bool, ports: bool, network: bool) -> Option<String> {
    let named: Vec<&str> = [(server, "the server switch"), (ports, "the ports"), (network, "the network switch")].into_iter().filter_map(|(fixed, name)| fixed.then_some(name)).collect();
    let list = match named.as_slice() {
        [] => return None,
        [only] => (*only).to_owned(),
        [first, second] => format!("{first} and {second}"),
        [first, second, third, ..] => format!("{first}, {second} and {third}"),
    };
    Some(format!("The command line sets {list} for this run. The controls show settings.json, and Save keeps them for later runs."))
}
/// The toast of a start that listens on the network because settings.json says so.
pub const BROWSER_OPEN_TO_NETWORK: &str = "The browser pages are open to other devices on this network. Turn that off in Browser view.";
/// Save.
pub const BROWSER_VIEW_SAVE: &str = "Save";
/// Save while the server moves.
pub const BROWSER_VIEW_SAVING: &str = "Saving\u{2026}";
/// Cancel.
pub const BROWSER_VIEW_CANCEL: &str = "Cancel";

/// A port option of the command line that is not a port.
pub fn cli_bad_port(option: &str, given: &str) -> String {
    format!("{option} expects a port number from 1 to 65535, not \"{given}\".")
}
/// `--host` that is not an IPv4 address.
pub fn cli_bad_host(given: &str) -> String {
    format!("--host expects an IPv4 address such as 127.0.0.1 or 0.0.0.0, not \"{given}\".")
}
/// `--frontend-port` and `--backend-port` with the same number.
pub const CLI_SAME_PORTS: &str = "The browser pages and the API need two different ports.";

/// The toast when the data folder cannot be shown.
pub fn data_folder_not_opened(path: &str, reason: &str) -> String {
    format!("The data folder \"{path}\" could not be opened: {reason}")
}

/// The heading of the settings file in the message box of a settings error.
pub const FATAL_SETTINGS_FILE: &str = "Settings file:";
/// The longest line of a path in that message box, in characters.
const FATAL_PATH_LINE_CHARS: usize = 60;

/// The message box of a fatal settings error. The line of stderr and the log names the file in
/// the middle ("Invalid settings at C:\...\settings.json: web_host must be ..."), which the box
/// would break inside folder names; the box says why first, then the file on lines of its own,
/// broken after backslashes. A message that does not name `file` is shown as it is.
pub fn fatal_dialog_text(message: &str, file: Option<&std::path::Path>) -> String {
    let Some(file) = file else {
        return message.to_owned();
    };
    let shown = file.display().to_string();
    let Some(at) = message.find(&shown) else {
        return message.to_owned();
    };
    let before = message[..at].trim_end();
    let lead = before.strip_suffix(" at").unwrap_or(before).trim_end_matches(':').trim();
    let reason = message[at + shown.len()..].trim_start_matches(':').trim();
    let sentence = match (lead.is_empty(), reason.is_empty()) {
        (false, false) => format!("{lead}: {reason}"),
        (false, true) => lead.to_owned(),
        (true, _) => reason.to_owned(),
    };
    let sentence = if sentence.ends_with('.') { sentence } else { format!("{sentence}.") };
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for part in shown.split_inclusive(['\\', '/']) {
        if !line.is_empty() && line.chars().count() + part.chars().count() > FATAL_PATH_LINE_CHARS {
            lines.push(std::mem::take(&mut line));
        }
        line.push_str(part);
    }
    lines.push(line);
    format!("{sentence}\n\n{FATAL_SETTINGS_FILE}\n{}", lines.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_message_box_of_a_settings_error_says_why_first_and_the_file_on_lines_of_its_own() {
        let file = std::path::Path::new(r"C:\Users\Someone\AppData\Local\Temp\a-rather-long-folder-name\profiles\settings-bad-host\instagram-windows-app\settings.json");
        let message = messages::settings_bad_web_host(&file.display().to_string());
        let text = fatal_dialog_text(&message, Some(file));
        let (sentence, rest) = text.split_once("\n\n").expect("a blank line");
        assert_eq!(sentence, "Invalid settings: web_host must be an IPv4 address such as 127.0.0.1 or 0.0.0.0.");
        let lines: Vec<&str> = rest.lines().collect();
        assert_eq!(lines[0], FATAL_SETTINGS_FILE);
        assert_eq!(lines[1..].concat(), file.display().to_string(), "the whole path, broken only between its parts");
        assert!(lines[1..].iter().all(|line| line.chars().count() <= FATAL_PATH_LINE_CHARS && (line.ends_with('\\') || line.ends_with("settings.json"))), "{lines:?}");
        // A message about something else is shown as it is.
        assert_eq!(fatal_dialog_text("The window could not be opened: no adapter", Some(file)), "The window could not be opened: no adapter");
        assert_eq!(fatal_dialog_text(&message, None), message);
    }

    #[test]
    fn likes_are_singular_only_at_exactly_one() {
        assert_eq!(likes_line(1), ("1".to_owned(), " like"));
        assert_eq!(likes_line(0), ("0".to_owned(), " likes"));
        assert_eq!(likes_line(56_789), ("56.8K".to_owned(), " likes"));
    }

    #[test]
    fn comments_line_follows_the_web_project() {
        assert_eq!(view_comments(1), "View 1 comment");
        assert_eq!(view_comments(1234), "View all 1.2K comments");
    }

    #[test]
    fn the_viewer_counts_comments_exactly_and_likes_compactly() {
        assert_eq!(view_comments_on(1, "Instagram"), "View 1 comment on Instagram");
        assert_eq!(view_comments_on(1234, "example.com"), "View all 1,234 comments on example.com");
        assert_eq!(comment_likes(1), "1 like");
        assert_eq!(comment_likes(3), "3 likes");
        assert_eq!(zoom_percent(150), "150%");
        assert_eq!(viewer_label("fixture_tidewater"), "Post by fixture_tidewater");
    }

    #[test]
    fn the_carousel_counter_has_no_spaces() {
        assert_eq!(carousel_counter(1, 2), "1/2");
    }
}
