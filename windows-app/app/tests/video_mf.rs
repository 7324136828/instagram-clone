//! Media Foundation plays videos the tests make themselves, without a window (video brief 7):
//! every frame in order and the end, seeks while paused that show the frame of the new position,
//! looping without an end, the end once looping is switched off, a usable failure, and a quick
//! close. No GPU and no sound device are needed; on this machine they run in a few seconds.

#![cfg(windows)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use instagram_core::poster::fixture::{self, FixtureSpec};
use instagram_core::test_folder::TestFolder;
use instagram_windows_app::video::{Command, Phase, PlayerOptions, VideoFrame, VideoPlayer, Waker};

/// A fresh folder for one test, removed when the test ends, also when it fails. Media Foundation
/// lets go of a file that failed to load a moment after its player is closed (video brief 6.7);
/// the guard tries again for a while.
fn folder(test: &str) -> TestFolder {
    TestFolder::new(test).expect("test folder")
}

fn counting_waker() -> (Arc<AtomicU64>, Waker) {
    let count = Arc::new(AtomicU64::new(0));
    let inner = Arc::clone(&count);
    (count, Arc::new(move || {
        inner.fetch_add(1, Ordering::Relaxed);
    }))
}

/// The two-second test video of the fixture writer.
fn short_video(folder: &Path) -> PathBuf {
    let path = folder.join("clip.mp4");
    fixture::write_mp4(&path, FixtureSpec::SHORT).expect("the sink writer makes the fixture");
    path
}

/// The frame number the pattern of a frame shows.
fn number(frame: &VideoFrame) -> u32 {
    let [width, height] = frame.image.size;
    let rgba: Vec<u8> = frame.image.pixels.iter().flat_map(|pixel| pixel.to_array()).collect();
    fixture::frame_number(width, height, &rgba)
}

/// Waits for a frame newer than `serial`.
fn next_frame(player: &VideoPlayer, serial: u64, what: &str) -> VideoFrame {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(frame) = player.take_frame(serial) {
            return frame;
        }
        assert!(Instant::now() < deadline, "no frame: {what}; status {:?}", player.status());
        std::thread::sleep(Duration::from_millis(2));
    }
}

#[test]
fn a_generated_mp4_plays_to_its_end_with_every_frame_in_order() {
    let folder = folder("video-plays");
    let path = short_video(&folder);
    let (wakes, waker) = counting_waker();
    let player = VideoPlayer::open(&path, PlayerOptions::default(), waker).expect("player thread");
    let started = Instant::now();
    let mut serial = 0;
    let mut numbers = Vec::new();
    while started.elapsed() < Duration::from_secs(15) {
        if let Some(frame) = player.take_frame(serial) {
            serial = frame.serial;
            assert_eq!(frame.image.size, [320, 240]);
            numbers.push(number(&frame));
        }
        if player.status().phase == Phase::Ended {
            break;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    // The engine may say it has ended a moment before its "ended" event arrives.
    let deadline = Instant::now() + Duration::from_secs(1);
    while player.status().ended_events == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    let status = player.status();
    assert_eq!(status.phase, Phase::Ended, "status {status:?}");
    assert_eq!(status.ended_events, 1);
    assert!((status.duration.unwrap_or(0.0) - 2.0).abs() < 0.05, "duration {:?}", status.duration);
    assert_eq!(status.mime, "video/mp4", "sniffed by the player thread");
    assert!(status.has_video && !status.has_audio);
    // The first frame may come twice: once forced when the engine says it is ready (the safety
    // net of `poll_frame`, when the engine reports no new frame at that moment) and once more
    // when the engine reports it after all. Every other frame comes once, in order.
    if numbers.len() > 1 && numbers[0] == numbers[1] {
        numbers.remove(0);
    }
    assert!(numbers.len() >= 55, "only {} frames: {numbers:?}", numbers.len());
    assert_eq!(numbers.first(), Some(&0));
    assert_eq!(numbers.last(), Some(&59));
    assert!(numbers.windows(2).all(|pair| pair[1] > pair[0]), "frames out of order: {numbers:?}");
    assert!(wakes.load(Ordering::Relaxed) >= numbers.len() as u64, "the UI is woken for every frame");
    assert!(player.close().is_some());
}

#[test]
fn a_seek_while_paused_shows_the_frame_of_the_new_position() {
    let folder = folder("video-seek");
    let path = short_video(&folder);
    let (_wakes, waker) = counting_waker();
    let options = PlayerOptions { autoplay: false, ..PlayerOptions::default() };
    let player = VideoPlayer::open(&path, options, waker).expect("player thread");
    // The first frame is shown while paused.
    let first = next_frame(&player, 0, "the first frame while paused");
    assert_eq!(number(&first), 0);
    let mut serial = first.serial;
    for target in [1.5, 0.5, 1.0, 0.0, 1.9] {
        player.send(Command::Seek(target, true));
        let frame = next_frame(&player, serial, &format!("after seeking to {target}"));
        serial = frame.serial;
        assert_eq!(number(&frame), (target * 30.0_f64).floor() as u32, "seek to {target}");
        // The paused seek plays (muted) to its frame and pauses again.
        let deadline = Instant::now() + Duration::from_secs(2);
        while player.status().phase != Phase::Paused && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(2));
        }
        assert_eq!(player.status().phase, Phase::Paused);
    }
    player.close();
}

#[test]
fn a_looping_video_wraps_without_an_end_and_ends_once_looping_is_switched_off() {
    let folder = folder("video-loop");
    let path = short_video(&folder);
    let (_wakes, waker) = counting_waker();
    let options = PlayerOptions { looping: true, ..PlayerOptions::default() };
    let player = VideoPlayer::open(&path, options, waker).expect("player thread");
    let started = Instant::now();
    let mut serial = 0;
    let mut wrapped = false;
    let mut last = 0;
    while started.elapsed() < Duration::from_secs(10) && !wrapped {
        if let Some(frame) = player.take_frame(serial) {
            serial = frame.serial;
            let current = number(&frame);
            wrapped = current < last;
            last = current;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
    assert!(wrapped, "the frame numbers start again at the end");
    let status = player.status();
    assert_eq!(status.ended_events, 0, "no end while looping: {status:?}");
    assert_eq!(status.phase, Phase::Playing);
    // Switched off while playing: the video ends at its end, once.
    player.send(Command::Loop(false));
    let deadline = Instant::now() + Duration::from_secs(5);
    while player.status().phase != Phase::Ended && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    // The engine may say it has ended a moment before its "ended" event arrives.
    let deadline = Instant::now() + Duration::from_secs(1);
    while player.status().ended_events == 0 && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(2));
    }
    assert_eq!(player.status().phase, Phase::Ended);
    assert_eq!(player.status().ended_events, 1);
    player.close();
}

#[test]
fn a_file_that_is_no_video_fails_with_a_reason() {
    let folder = folder("video-error");
    let path = folder.join("text.mp4");
    // The bytes of an MP4 header, so that the player thread takes it for one, then nonsense.
    std::fs::write(&path, b"\x00\x00\x00\x14ftypmp42\x00\x00\x00\x00mp42 and then no video at all").expect("file");
    let (_wakes, waker) = counting_waker();
    let player = VideoPlayer::open(&path, PlayerOptions::default(), waker).expect("player thread");
    let deadline = Instant::now() + Duration::from_secs(10);
    while player.status().phase != Phase::Failed && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    let status = player.status();
    let error = status.error.clone().expect("an error");
    assert_eq!(error.media_error, 4, "MF_MEDIA_ENGINE_ERR_SRC_NOT_SUPPORTED: {error:?}");
    // The extended HRESULT depends on the bytes (seen: 0xC00D36C4, 0xC00D3E84), so only "a
    // failure with a text" is promised, and the sentence of a damaged file.
    assert!(error.hresult < 0 && error.text.starts_with("HRESULT 0x"), "{error:?}");
    let sentence = instagram_core::video_rules::classify("text.mp4", &status.mime, &error);
    assert!(sentence.starts_with("\"text.mp4\" can't be played: the file is damaged or incomplete ("), "{sentence}");
    player.close();
    // A text file is not even a video to the player thread.
    let plain = folder.join("notes.mp4");
    std::fs::write(&plain, b"just some notes").expect("file");
    let (_wakes, waker) = counting_waker();
    let player = VideoPlayer::open(&plain, PlayerOptions::default(), waker).expect("player thread");
    let deadline = Instant::now() + Duration::from_secs(5);
    while player.status().phase != Phase::Failed && Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(player.status().phase, Phase::Failed);
    player.close();
}

#[test]
fn closing_a_player_takes_a_moment_only() {
    let folder = folder("video-close");
    let path = short_video(&folder);
    let (_wakes, waker) = counting_waker();
    let options = PlayerOptions { looping: true, ..PlayerOptions::default() };
    let player = VideoPlayer::open(&path, options, waker).expect("player thread");
    let _ = next_frame(&player, 0, "the first frame");
    let started = Instant::now();
    let shutdown = player.close().expect("the thread ends");
    let took = started.elapsed();
    assert!(took < Duration::from_millis(100), "closing took {took:?} (Shutdown {shutdown:?})");
}
