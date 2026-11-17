//! Core of the Instagram-style image viewer: the post model, the "instagram-export" format
//! (reading and writing), the files the app keeps, and the logic of the views. Nothing here
//! depends on a GUI toolkit, so every rule can be unit-tested without a window.
//!
//! The modules mirror the import code of the reference web project (the "clone"):
//!
//! ```text
//! model      Post, MediaItem, User, Comment, Collection, PostKey, Warning   (shared types)
//! messages   every user-visible sentence the core produces
//! text       JavaScript-compatible text, count, time and id helpers
//! urls       link sanitising, site label, validator-compatible link checks
//! sniff      what a file is, from its first bytes
//! manifest   lenient reader of post.json: limits, paths, posts and warnings
//! archive    export archives (.zip, .tar, .tar.gz): readers and the extraction cache
//! sources    several export folders and archives merged into one collection
//! catalog    opening paths: loose pictures, folders, exports, archives
//! store      descriptions.json and state.json (atomic JSON files)
//! settings   the settings.json contract and the app's file locations
//! export     the zip writer of "Download post" / "Download all"
//! library    the browser imports folder (port of the clone's library.py)
//! web        the browser pages: HTTP server, API routes, guard, snapshot of what is open
//! autoview   the slideshow: settings, order, navigation, timer, the video option
//! video_rules which feed video plays, times, frame sizes, the sentence of a failed video
//! poster     the first frame of a loose video as its poster, in an app-owned cache
//! decode     picture decoding to RGBA (image crate, Windows Imaging Component)
//! test_folder the folders of the tests of both crates, removed when a test ends (tests only)
//! ```
//!
//! Conventions shared by all modules: instants are `i64` milliseconds since the Unix epoch in UTC;
//! text lengths that the clone counts with JavaScript's `length` are UTF-16 code units
//! (`text::utf16_len`); a bad input file never panics, it becomes a `model::Warning` or an error
//! value whose message is a complete sentence that can be shown to the user.

// The user interface is written against this crate by other people: every public item says
// what it is, and the build tells when one does not.
#![warn(missing_docs)]

pub mod archive;
pub mod autoview;
pub mod catalog;
pub mod decode;
pub mod export;
pub mod library;
pub mod manifest;
pub mod messages;
pub mod model;
pub mod poster;
pub mod settings;
pub mod sniff;
pub mod sources;
pub mod store;
// The folders the tests of both crates write in, removed when a test ends; not used by the
// program (see the module).
#[doc(hidden)]
pub mod test_folder;
pub mod text;
pub mod urls;
pub mod video_rules;
pub mod web;
