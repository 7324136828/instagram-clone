//! The user interface of the image viewer (eframe / egui).
//!
//! The format, the files and the logic live in the `instagram-core` crate, which has no GUI
//! dependency. This crate draws them in the style of the reference web project. It is a library
//! so that the headless tests in `tests/` can build the whole `App` without a window; `main.rs`
//! only reads the command line and the settings and starts eframe.
//!
//! How the pieces connect:
//!
//! ```text
//! main.rs      command line (cli.rs), settings, log file (logging.rs), window options and
//!              where the window opens (placement.rs)
//!   └─ app.rs        App: one frame = frame_logic + frame_ui; the eframe glue
//!        ├─ state.rs      AppState, Action, apply(): the only place that changes the state
//!        ├─ opening.rs    file dialogs and reading folders on background threads
//!        ├─ download.rs   the Save dialog and the export on background threads
//!        ├─ loader.rs     decoding pictures on worker threads, texture cache
//!        ├─ shell.rs      sidebar, More menu, empty state, drop overlay, About sheet
//!        ├─ feed.rs       the virtualised home feed and its post cards
//!        │    └─ banner.rs    the banner above the feed, with the progress of a download
//!        ├─ explore.rs    the grid of square tiles
//!        ├─ viewer.rs     the viewer: a post with its picture contained, previous / next, zoom
//!        │    └─ zoom.rs      pure zoom and pan arithmetic
//!        ├─ autoview.rs   auto view: the setup dialog and the running slide show
//!        ├─ sheets.rs     the "Post options" sheet, questions, and the warnings dialog
//!        ├─ editor.rs     the "Edit description" dialog
//!        ├─ browser.rs    the Browser view sheet: addresses, ports, LAN
//!        ├─ web.rs        the browser pages: the core's HTTP server started, moved, stopped;
//!        │                what is open published; the browser's changes to the UI thread
//!        ├─ video/        the video players (Media Foundation, one thread each), who plays,
//!        │                the controls bar, the sound button, the video on a stage
//!        └─ harness.rs    --screenshot / --scenario: the hidden development options
//! widgets.rs   text, buttons, avatar, sheets, toasts: shared by every view
//! media.rs     slides, placeholders and carousel controls: shared by feed, grid and viewer
//! icons.rs     the SVG glyphs, rasterised per size      gradients.rs  the CSS gradients
//! theme.rs     palettes, fonts, egui style              layout.rs     pure layout arithmetic
//! strings.rs   every label of the interface             platform.rs   links, Explorer, thread priority
//! shortcuts.rs the tables of keys and command-line options that --help prints
//! web_assets.rs the vendored browser pages (web/dist), embedded by build.rs
//! rendering.rs quick drawing without a graphics card: filtering in the shader, covered shapes
//! ```
//!
//! Layers, bottom to top: the main view (feed or grid), the viewer, auto view (which covers
//! everything below it), one dialog (post options, editor, auto view setup, warnings or About),
//! a question above that, the toasts, the drop overlay. `state.rs` describes which of them has
//! the keyboard.

pub mod app;
pub mod autoview;
pub mod banner;
pub mod browser;
pub mod cli;
pub mod download;
pub mod editor;
pub mod explore;
pub mod feed;
pub mod gradients;
pub mod harness;
pub mod icons;
pub mod layout;
pub mod loader;
pub mod logging;
pub mod media;
pub mod opening;
pub mod placement;
pub mod platform;
pub mod rendering;
pub mod sheets;
pub mod shell;
pub mod shortcuts;
pub mod state;
pub mod strings;
pub mod theme;
pub mod video;
pub mod viewer;
pub mod web;
pub mod web_assets;
pub mod widgets;
pub mod zoom;
