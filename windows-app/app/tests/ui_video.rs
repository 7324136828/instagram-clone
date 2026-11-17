//! Headless tests of videos in the interface (video brief 7), with a fake player backend: which
//! feed video plays, the sound button, the play badge on a software renderer, the controls and
//! keys of the viewer, auto view's "Let videos play to the end", the panel of a video that cannot
//! be played, and the banner of a folder of pictures and videos.

mod common;

use std::path::{Path, PathBuf};

use common::{config, count, count_containing, fixture, harness, profile, settle, settle_idle, TestFolder};
use egui::accesskit::Role;
use egui::{Event, Key, Modifiers, PointerButton};
use egui_kittest::kittest::Queryable as _;
use egui_kittest::Harness;
use instagram_core::catalog::{self, OpenOptions, OpenRequest};
use instagram_core::model::PostKey;
use instagram_core::video_rules::{PlaybackFailure, VideoCaps, HRESULT_UNSUPPORTED_BYTESTREAM};
use instagram_windows_app::app::App;
use instagram_windows_app::state::{Action, PlayerCommand, ScrollRequest, View};
use instagram_windows_app::video::{Command, FakeBackend, FakeScript, Phase};

/// The bytes of an MP4 the catalog takes for a video; no decoder could play them, the fake
/// backend does not try.
const MP4: &[u8] = b"\x00\x00\x00\x18ftypmp42\x00\x00\x00\x00mp42isom and no video";

/// A folder with one picture and the named videos, in the test's profile.
fn mixed_folder(profile: &Path, videos: &[&str]) -> PathBuf {
    let folder = profile.join("mixed");
    std::fs::create_dir_all(&folder).expect("folder");
    std::fs::copy(fixture("loose").join("IMG_2.jpg"), folder.join("a.jpg")).expect("a picture");
    for name in videos {
        std::fs::write(folder.join(name), MP4).expect("a video");
    }
    folder
}

/// An app with the folder open, the fake backend and the video rules of `caps`. The profile
/// comes first: bound first, it is dropped (and removed) after the harness.
fn video_app(test: &str, videos: &[&str], caps: VideoCaps, backend: &FakeBackend) -> (TestFolder, Harness<'static, App>, PathBuf) {
    let profile = profile(test);
    let folder = mixed_folder(&profile, videos);
    let mut config = config(&profile);
    config.video_backend = Some(Box::new(backend.clone()));
    config.video_caps = Some(caps);
    let mut app = App::new(config);
    let request = OpenRequest { paths: vec![folder.clone()], options: OpenOptions { default_username: "tester".to_owned() } };
    let opened = catalog::open_paths(&request, &app.services.store, &app.services.context).expect("the folder opens");
    app.show_opened(opened);
    let mut harness = harness(app);
    // A test frame is a quarter of a second: give the two clicks of a double click the time.
    harness.ctx.options_mut(|options| options.input_options.max_double_click_delay = 1.0);
    settle_idle(&mut harness);
    (profile, harness, folder)
}

/// The key of the post of `file`.
fn key_of(harness: &Harness<'_, App>, file: &str) -> PostKey {
    harness.state().state.posts().iter().find(|post| post.file_name().as_deref() == Some(file)).map(|post| post.key.clone()).expect("the post")
}

/// Scrolls the feed so that the card of `file` is at the top.
fn scroll_to(harness: &mut Harness<'_, App>, file: &str) {
    let key = key_of(harness, file);
    harness.state_mut().state.scroll_request = Some(ScrollRequest::Post(key));
    harness.state_mut().touch();
    settle_idle(harness);
}

/// The files the fake backend opened players for, in order.
fn opened(backend: &FakeBackend) -> Vec<String> {
    backend.log().opened.iter().map(|(path, _)| path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default()).collect()
}

/// Clicks the middle of the k-th widget named `label` with real pointer events.
fn click_at(harness: &Harness<'_, App>, center: egui::Pos2) {
    harness.event(Event::PointerMoved(center));
    harness.event(Event::PointerButton { pos: center, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    harness.event(Event::PointerButton { pos: center, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
}

#[test]
fn the_most_visible_feed_video_plays_muted_and_scrolling_hands_over() {
    let backend = FakeBackend::default();
    let (_profile, mut harness, _) = video_app("video-feed", &["b.mp4", "c.mp4"], VideoCaps::Gpu, &backend);
    assert_eq!(harness.state().state.view, View::Home);
    // The picture is on top; the video below it is not visible enough yet.
    assert!(opened(&backend).is_empty(), "{:?}", opened(&backend));

    scroll_to(&mut harness, "b.mp4");
    assert_eq!(opened(&backend), ["b.mp4"]);
    let options = backend.log().opened[0].1.clone();
    assert!(options.autoplay && options.looping && options.muted, "{options:?}");
    assert_eq!(count(&harness, "Turn sound on"), 2, "every video card has the sound button");

    // The next card takes over: one feed player at a time.
    scroll_to(&mut harness, "c.mp4");
    assert_eq!(opened(&backend), ["b.mp4", "c.mp4"]);
    assert_eq!(backend.log().closed.iter().filter(|path| path.ends_with("b.mp4")).count(), 1);
    assert_eq!(backend.log().live(), 1);
}

#[test]
fn the_viewer_pauses_the_feed_video_the_sound_button_switches_the_sound_and_a_double_click_likes() {
    let backend = FakeBackend::default();
    let (_profile, mut harness, _) = video_app("video-feed-viewer", &["b.mp4"], VideoCaps::Gpu, &backend);
    scroll_to(&mut harness, "b.mp4");
    assert_eq!(opened(&backend), ["b.mp4"]);

    // The sound button: the run-wide flag, the label, and the player hears it.
    harness.get_by_role_and_label(Role::Button, "Turn sound on").click();
    settle_idle(&mut harness);
    assert!(harness.state().state.video_sound_on);
    assert_eq!(count(&harness, "Turn sound off"), 1);
    assert!(backend.log().commands_for("b.mp4").contains(&Command::Mute(false)));

    // A double click on the video likes the post (its two clicks switch the sound twice).
    let key = key_of(&harness, "b.mp4");
    let media = harness.get_by_role_and_label(Role::Image, "Video by tester").rect().center();
    click_at(&harness, media);
    click_at(&harness, media);
    settle_idle(&mut harness);
    assert!(harness.state().state.post_ui(&key).liked);
    assert!(harness.state().state.video_sound_on, "switched off and on again");

    // The viewer covers the feed: the feed's video pauses, the viewer's plays.
    harness.state_mut().dispatch(Action::OpenViewer(key));
    settle_idle(&mut harness);
    assert_eq!(opened(&backend), ["b.mp4", "b.mp4"]);
    assert_eq!(backend.log().commands_for("b.mp4").iter().filter(|command| **command == Command::Pause).count(), 1);
    // Closing it closes its player and the feed plays on with its own.
    harness.key_press(Key::Escape);
    settle_idle(&mut harness);
    assert_eq!(backend.log().live(), 1);
    assert_eq!(backend.log().commands_for("b.mp4").last(), Some(&Command::Play));
}

#[test]
fn a_video_that_cannot_be_played_says_why_and_offers_show_in_folder() {
    let backend = FakeBackend::default();
    let failure = PlaybackFailure { media_error: 4, hresult: HRESULT_UNSUPPORTED_BYTESTREAM, text: "HRESULT 0xC00D36C4: The byte stream type of the given URL is unsupported.".to_owned(), codec: None, no_picture: false };
    backend.log().scripts.insert("b.mp4".to_owned(), FakeScript { failure: Some(failure), ..FakeScript::default() });
    let (_profile, mut harness, _) = video_app("video-failed", &["b.mp4"], VideoCaps::Gpu, &backend);
    scroll_to(&mut harness, "b.mp4");
    settle(&mut harness, "the failure is shown", |app| app.services.video.feed_failure(&instagram_windows_app::video::VideoKey::new(&app.state.posts()[1].key, 0)).is_some());
    let sentence = "\"b.mp4\" can't be played: the file is damaged or incomplete (The byte stream type of the given URL is unsupported).";
    assert_eq!(count(&harness, sentence), 1);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Show in folder").count(), 1);
    // In the viewer the same panel.
    let key = key_of(&harness, "b.mp4");
    harness.state_mut().dispatch(Action::OpenViewer(key));
    settle_idle(&mut harness);
    assert_eq!(count(&harness, sentence), 2, "the card behind the viewer and the viewer");
    // The viewer's panel keeps clear of the previous-post button on the stage.
    let previous = harness.get_by_role_and_label(Role::Button, "Previous picture").rect();
    for text in harness.query_all_by_label(sentence).map(|node| node.rect()) {
        assert!(!text.expand(4.0).intersects(previous), "the sentence {text:?} runs into the arrow {previous:?}");
    }
    assert!(harness.query_all_by_role_and_label(Role::Button, "Show in folder").all(|node| !node.rect().intersects(previous)));
}

#[test]
fn on_a_software_renderer_the_feed_shows_a_play_badge_that_opens_the_viewer() {
    let backend = FakeBackend::default();
    let (_profile, mut harness, _) = video_app("video-cpu", &["b.mp4"], VideoCaps::Cpu, &backend);
    scroll_to(&mut harness, "b.mp4");
    assert!(opened(&backend).is_empty(), "no autoplay");
    assert_eq!(count(&harness, "Turn sound on"), 0, "no sound button");
    harness.get_by_role_and_label(Role::Button, "Play video").click();
    settle_idle(&mut harness);
    let key = key_of(&harness, "b.mp4");
    assert_eq!(harness.state().state.viewer.as_ref().map(|viewer| viewer.key.clone()), Some(key));
    assert_eq!(opened(&backend), ["b.mp4"]);
    let options = backend.log().opened[0].1.clone();
    assert!(options.autoplay && options.target_size.is_some(), "the viewer plays at once, frames capped: {options:?}");
}

#[test]
fn the_viewer_has_the_controls_and_keys_of_a_video_and_the_arrows_still_move_on() {
    let backend = FakeBackend::default();
    let (_profile, mut harness, _) = video_app("video-viewer", &["b.mp4", "c.mp4"], VideoCaps::Cpu, &backend);
    let key = key_of(&harness, "b.mp4");
    harness.state_mut().dispatch(Action::OpenViewer(key.clone()));
    settle_idle(&mut harness);
    assert_eq!(opened(&backend), ["b.mp4"]);
    if let Some(status) = backend.log().status_mut("b.mp4") {
        status.current_time = 3.2;
    }

    // Space pauses; the controls stay while paused, with the time of the player.
    harness.key_press(Key::Space);
    settle_idle(&mut harness);
    assert_eq!(backend.log().commands_for("b.mp4").last(), Some(&Command::Pause));
    assert_eq!(count(&harness, "0:03 / 0:10"), 1);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Play").count(), 1);
    assert_eq!(harness.query_all_by_role_and_label(Role::Button, "Unmute").count(), 1);
    assert_eq!(count(&harness, "Video position"), 1);
    assert_eq!(count(&harness, "Volume"), 1);
    // K plays again.
    harness.key_press(Key::K);
    settle_idle(&mut harness);
    assert_eq!(backend.log().commands_for("b.mp4").last(), Some(&Command::Play));
    // M switches the run-wide sound.
    harness.key_press(Key::M);
    settle_idle(&mut harness);
    assert!(harness.state().state.video_sound_on);
    assert!(backend.log().commands_for("b.mp4").contains(&Command::Mute(false)));
    // Shift+Right: five seconds on, to the frame.
    harness.key_press_modifiers(Modifiers::SHIFT, Key::ArrowRight);
    settle_idle(&mut harness);
    assert!(backend.log().commands_for("b.mp4").contains(&Command::Seek(8.2, true)), "{:?}", backend.log().commands_for("b.mp4"));
    assert_eq!(harness.state().state.viewer.as_ref().map(|viewer| viewer.key.clone()), Some(key.clone()), "Shift+Right does not leave the post");
    // The zoom keys do nothing on a video.
    harness.key_press(Key::Plus);
    settle_idle(&mut harness);
    assert_eq!(harness.state().viewer.zoom_percent(), None);
    assert!(harness.state().viewer.zoom().is_fit());
    // Right still goes to the next post, whose video replaces this one.
    harness.key_press(Key::ArrowRight);
    settle_idle(&mut harness);
    assert_eq!(harness.state().state.viewer.as_ref().map(|viewer| viewer.key.clone()), Some(key_of(&harness, "c.mp4")));
    assert_eq!(opened(&backend), ["b.mp4", "c.mp4"]);
    assert!(backend.log().closed.iter().any(|path| path.ends_with("b.mp4")));
    harness.key_press(Key::ArrowLeft);
    settle_idle(&mut harness);
    assert_eq!(harness.state().state.viewer.as_ref().map(|viewer| viewer.key.clone()), Some(key));
}

/// Starts auto view at the post of `file` with `wait_for_videos`, the clock held by hand.
fn start_auto_view(harness: &mut Harness<'_, App>, file: &str, wait_for_videos: bool) {
    let index = harness.state().state.posts().iter().position(|post| post.file_name().as_deref() == Some(file)).expect("post");
    let settings = instagram_core::autoview::Settings { interval_sec: 15, wait_for_videos, loop_posts: false, shuffle: false };
    let app = harness.state_mut();
    app.state.auto_view_clock = Some(0.0);
    instagram_windows_app::state::start_auto_view(&mut app.state, &mut app.services, settings, index);
    app.touch();
    settle_idle(harness);
}

/// Moves the clock of auto view to `seconds`, a frame at a time.
fn auto_view_clock(harness: &mut Harness<'_, App>, seconds: f64) {
    let mut now = harness.state().state.auto_view_clock.unwrap_or(0.0);
    while now < seconds {
        now = (now + 0.5).min(seconds);
        harness.state_mut().state.auto_view_clock = Some(now);
        harness.run_steps(1);
    }
    settle_idle(harness);
}

/// The file of the post auto view shows.
fn auto_view_file(harness: &Harness<'_, App>) -> Option<String> {
    harness.state().state.auto_view_post().and_then(|(post, _)| post.file_name())
}

#[test]
fn auto_view_waits_for_the_end_of_a_long_video_when_asked_to() {
    let backend = FakeBackend::default();
    backend.log().scripts.insert("b.mp4".to_owned(), FakeScript { duration: Some(40.0), ..FakeScript::default() });
    let (_profile, mut harness, _) = video_app("video-auto-view", &["b.mp4", "c.mp4"], VideoCaps::Cpu, &backend);
    start_auto_view(&mut harness, "b.mp4", true);
    assert_eq!(auto_view_file(&harness).as_deref(), Some("b.mp4"));
    assert_eq!(opened(&backend), ["b.mp4"]);
    assert!(!backend.log().opened[0].1.looping, "auto view waits for the end: no loop");
    // Past the slide time the video stays: it is 40 s long.
    auto_view_clock(&mut harness, 20.0);
    assert_eq!(auto_view_file(&harness).as_deref(), Some("b.mp4"));
    // It ends: the show moves on.
    if let Some(status) = backend.log().status_mut("b.mp4") {
        status.phase = Phase::Ended;
        status.ended_events = 1;
    }
    auto_view_clock(&mut harness, 21.0);
    assert_eq!(auto_view_file(&harness).as_deref(), Some("c.mp4"));

    // Space pauses the show, and with it the video.
    harness.key_press(Key::Space);
    settle_idle(&mut harness);
    assert!(harness.state().state.auto_view.as_ref().is_some_and(|run| run.player.paused));
    assert_eq!(backend.log().commands_for("c.mp4").last(), Some(&Command::Pause));
    // M switches the sound.
    harness.key_press(Key::M);
    settle_idle(&mut harness);
    assert!(harness.state().state.video_sound_on);
    harness.state_mut().dispatch(Action::AutoViewCommand(PlayerCommand::TogglePause));
    settle_idle(&mut harness);
    assert_eq!(backend.log().commands_for("c.mp4").last(), Some(&Command::Play));
}

#[test]
fn without_the_option_or_with_a_failing_video_the_slide_time_applies() {
    let backend = FakeBackend::default();
    backend.log().scripts.insert("b.mp4".to_owned(), FakeScript { duration: Some(40.0), ..FakeScript::default() });
    let failure = PlaybackFailure { media_error: 4, hresult: -1, text: "HRESULT 0xFFFFFFFF: Broken.".to_owned(), codec: None, no_picture: false };
    backend.log().scripts.insert("c.mp4".to_owned(), FakeScript { failure: Some(failure), ..FakeScript::default() });
    let (_profile, mut harness, _) = video_app("video-auto-view-off", &["b.mp4", "c.mp4", "d.mp4"], VideoCaps::Cpu, &backend);
    start_auto_view(&mut harness, "b.mp4", false);
    assert!(backend.log().opened[0].1.looping, "a video that does not hold the show loops");
    auto_view_clock(&mut harness, 15.5);
    assert_eq!(auto_view_file(&harness).as_deref(), Some("c.mp4"), "the slide time, not the video's 40 s");

    // With the option on, a video that fails gets the slide time too.
    let backend = FakeBackend::default();
    let failure = PlaybackFailure { media_error: 4, hresult: -1, text: "HRESULT 0xFFFFFFFF: Broken.".to_owned(), codec: None, no_picture: false };
    backend.log().scripts.insert("b.mp4".to_owned(), FakeScript { failure: Some(failure), ..FakeScript::default() });
    let (_profile, mut harness, _) = video_app("video-auto-view-failed", &["b.mp4", "c.mp4"], VideoCaps::Cpu, &backend);
    start_auto_view(&mut harness, "b.mp4", true);
    auto_view_clock(&mut harness, 15.5);
    assert_eq!(auto_view_file(&harness).as_deref(), Some("c.mp4"));
}

/// The Play and Pause commands sent to the players of `file`, in order (the others, such as the
/// size of the frames, do not tell whether it plays).
fn plays_and_pauses(backend: &FakeBackend, file: &str) -> Vec<Command> {
    backend.log().commands_for(file).into_iter().filter(|command| matches!(command, Command::Play | Command::Pause)).collect()
}

/// One pass of a minimized window, as eframe runs it: only the logic half of a frame
/// (`App::logic` through `Context::run_logic`), with the window reported minimized. Nothing is
/// drawn, so no view claims its video.
fn minimized_pass(harness: &mut Harness<'_, App>) {
    let mut input = harness.input().clone();
    if let Some(viewport) = input.viewports.get_mut(&egui::ViewportId::ROOT) {
        viewport.minimized = Some(true);
    }
    let ctx = harness.ctx.clone();
    let _ = ctx.run_logic(&input, |ctx| harness.state_mut().frame_logic(ctx));
}

#[test]
fn a_minimized_window_pauses_the_video_that_plays_and_restoring_it_plays_on() {
    // The viewer.
    let backend = FakeBackend::default();
    let (_profile, mut harness, _) = video_app("video-minimized", &["b.mp4"], VideoCaps::Cpu, &backend);
    let key = key_of(&harness, "b.mp4");
    harness.state_mut().dispatch(Action::OpenViewer(key));
    settle_idle(&mut harness);
    assert_eq!(opened(&backend), ["b.mp4"]);
    assert!(plays_and_pauses(&backend, "b.mp4").is_empty(), "it plays from the start: {:?}", backend.log().commands_for("b.mp4"));
    for _ in 0..3 {
        minimized_pass(&mut harness);
    }
    assert_eq!(plays_and_pauses(&backend, "b.mp4"), [Command::Pause], "paused, once");
    assert_eq!(backend.log().live(), 1, "the viewer's player is kept");
    assert!(harness.state().state.viewer.is_some());
    // Restored: the next painted frame plays it on, with the same player.
    settle_idle(&mut harness);
    assert_eq!(plays_and_pauses(&backend, "b.mp4"), [Command::Pause, Command::Play]);
    assert_eq!(opened(&backend), ["b.mp4"]);
    // A video the user paused stays paused.
    harness.key_press(Key::Space);
    settle_idle(&mut harness);
    minimized_pass(&mut harness);
    settle_idle(&mut harness);
    assert_eq!(plays_and_pauses(&backend, "b.mp4"), [Command::Pause, Command::Play, Command::Pause]);
    assert_eq!(backend.log().live(), 1);

    // The feed, with a graphics card.
    let backend = FakeBackend::default();
    let (_profile, mut harness, _) = video_app("video-minimized-feed", &["b.mp4"], VideoCaps::Gpu, &backend);
    scroll_to(&mut harness, "b.mp4");
    assert_eq!(opened(&backend), ["b.mp4"]);
    minimized_pass(&mut harness);
    minimized_pass(&mut harness);
    assert_eq!(plays_and_pauses(&backend, "b.mp4"), [Command::Pause]);
    settle_idle(&mut harness);
    assert_eq!(plays_and_pauses(&backend, "b.mp4"), [Command::Pause, Command::Play]);
    assert_eq!(opened(&backend), ["b.mp4"], "where it was");
}

#[test]
fn auto_view_and_its_video_wait_while_the_window_is_minimized() {
    let backend = FakeBackend::default();
    backend.log().scripts.insert("b.mp4".to_owned(), FakeScript { duration: Some(40.0), ..FakeScript::default() });
    let (_profile, mut harness, _) = video_app("video-minimized-auto-view", &["b.mp4", "c.mp4"], VideoCaps::Cpu, &backend);
    // Fifteen seconds a post; the video loops and does not hold the show.
    start_auto_view(&mut harness, "b.mp4", false);
    auto_view_clock(&mut harness, 5.0);
    assert!(plays_and_pauses(&backend, "b.mp4").is_empty(), "it plays: {:?}", backend.log().commands_for("b.mp4"));
    // Minimized for a minute: the video pauses, the time of the slide stands still.
    for now in [5.0, 30.0, 65.0] {
        harness.state_mut().state.auto_view_clock = Some(now);
        minimized_pass(&mut harness);
    }
    assert_eq!(plays_and_pauses(&backend, "b.mp4"), [Command::Pause]);
    let run = harness.state().state.auto_view.clone().expect("the show");
    assert!(!run.player.paused && !run.timer.is_running(), "a minimized window is no pause of the user's");
    assert_eq!(auto_view_file(&harness).as_deref(), Some("b.mp4"));
    // Restored: the video plays on, and the post has the ten seconds it had left.
    settle_idle(&mut harness);
    assert_eq!(plays_and_pauses(&backend, "b.mp4"), [Command::Pause, Command::Play]);
    auto_view_clock(&mut harness, 74.5);
    assert_eq!(auto_view_file(&harness).as_deref(), Some("b.mp4"));
    auto_view_clock(&mut harness, 75.5);
    assert_eq!(auto_view_file(&harness).as_deref(), Some("c.mp4"));

    // Paused with Space, the show and its video stay paused after the window is restored.
    harness.key_press(Key::Space);
    settle_idle(&mut harness);
    let pauses = plays_and_pauses(&backend, "c.mp4").iter().filter(|command| **command == Command::Pause).count();
    minimized_pass(&mut harness);
    settle_idle(&mut harness);
    assert_eq!(plays_and_pauses(&backend, "c.mp4").last(), Some(&Command::Pause));
    assert_eq!(plays_and_pauses(&backend, "c.mp4").iter().filter(|command| **command == Command::Pause).count(), pauses, "nothing more to pause");
    assert!(harness.state().state.auto_view.as_ref().is_some_and(|run| run.player.paused));
}

#[test]
fn a_folder_of_pictures_and_videos_names_both_and_the_grid_marks_the_videos() {
    let backend = FakeBackend::default();
    let (_profile, mut harness, folder) = video_app("video-banner", &["b.mp4", "c.webm.mp4"], VideoCaps::Cpu, &backend);
    let name = folder.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    assert_eq!(count(&harness, &format!("Showing 3 pictures and videos from {name}")), 1);
    harness.get_by_role_and_label(Role::Button, "Explore").click();
    settle_idle(&mut harness);
    assert_eq!(count(&harness, "Video"), 2, "the clip badge of each video tile");
    assert_eq!(count_containing(&harness, "not played"), 0, "the old note is gone");
    assert!(opened(&backend).is_empty(), "the grid never plays");
}
