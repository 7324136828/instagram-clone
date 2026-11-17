//! The posters of loose videos with the real source reader (video brief 5.2, 7): the first frame
//! of videos the test makes itself, turned by the stream's rotation, the codec of a video Windows
//! cannot decode, a file that is no video, the WebM videos of the clone's fixture (VP8) and a VP9
//! WebM recorded in a browser.

#![cfg(windows)]

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};

use instagram_core::poster::fixture::{self, FixtureSpec};
use instagram_core::poster::{self, PosterCache};
use instagram_core::video_rules;

/// A folder of its own below the system's temporary folder, removed when dropped (with a few
/// attempts: Media Foundation lets go of a file that failed to load a moment late).
struct TempFolder(PathBuf);

impl TempFolder {
    fn new(label: &str) -> TempFolder {
        let folder = std::env::temp_dir().join(format!("instagram-core-poster-it-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).expect("a temporary folder");
        TempFolder(folder)
    }
}

impl Drop for TempFolder {
    fn drop(&mut self) {
        for _ in 0..40 {
            if fs::remove_dir_all(&self.0).is_ok() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
    }
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures")
}

#[test]
fn the_poster_is_the_first_frame_and_the_probe_names_the_codec() {
    let folder = TempFolder::new("first-frame");
    let path = folder.0.join("clip.mp4");
    fixture::write_mp4(&path, FixtureSpec::SHORT).expect("the sink writer makes the fixture");
    let probe = poster::probe(&path, "video/mp4");
    assert_eq!(probe.failure, None);
    assert_eq!(probe.codec.as_deref(), Some("H.264"));
    assert_eq!(probe.size, Some((320, 240)));
    assert_eq!(probe.rotation, 0);
    assert!(probe.duration.is_some_and(|seconds| (seconds - 2.0).abs() < 0.05), "{:?}", probe.duration);
    let frame = probe.frame.expect("a first frame");
    assert_eq!((frame.width, frame.height), (320, 240));
    assert_eq!(fixture::frame_number(frame.width as usize, frame.height as usize, &frame.rgba), 0);
    assert_eq!(poster::probe_codec(&path, "video/mp4").as_deref(), Some("H.264"));

    // The cache: a JPEG of the frame in its own folder, nothing beside the video.
    let cache = PosterCache::new(folder.0.join("posters"));
    let made = cache.ensure(&path, "video/mp4").expect("a poster");
    assert_eq!(image::image_dimensions(&made).expect("a JPEG"), (320, 240));
    assert_eq!(made.parent(), Some(cache.dir()));
    assert_eq!(cache.ensure_sniffed(&path).expect("found again"), made);
    let beside: Vec<String> = fs::read_dir(&folder.0).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
    assert_eq!(beside.len(), 2, "{beside:?}");
}

#[test]
fn a_width_that_is_no_multiple_of_16_is_read_row_by_row() {
    // 360 pixels are coded as 368: the decoder pads each row.
    let folder = TempFolder::new("padded");
    let path = folder.0.join("portrait.mp4");
    fixture::write_mp4(&path, FixtureSpec { width: 360, height: 640, ..FixtureSpec::SHORT }).expect("the sink writer makes the fixture");
    let frame = poster::probe(&path, "video/mp4").frame.expect("a first frame");
    assert_eq!((frame.width, frame.height), (360, 640));
    let luma = |x: usize, y: usize| frame.rgba[(y * 360 + x) * 4];
    // Frame 0: the white bar is at the left edge on every row; a skewed frame moves it.
    for y in [100, 300, 600] {
        assert!(luma(4, y) > 200, "the bar at row {y}: {}", luma(4, y));
        assert!(luma(40, y) < 200, "no bar beside it at row {y}: {}", luma(40, y));
    }
}

#[test]
fn a_video_a_phone_recorded_sideways_gets_an_upright_poster() {
    let folder = TempFolder::new("rotation");
    let path = folder.0.join("portrait.mp4");
    let spec = FixtureSpec { width: 320, height: 192, rotation: 90, ..FixtureSpec::SHORT };
    fixture::write_mp4(&path, spec).expect("the sink writer makes the fixture");
    let probe = poster::probe(&path, "video/mp4");
    assert_eq!(probe.rotation, 90, "the source reader reports the rotation");
    assert_eq!(probe.size, Some((320, 192)), "and hands the frame out as stored");
    let made = PosterCache::new(folder.0.join("posters")).ensure(&path, "video/mp4").expect("a poster");
    assert_eq!(image::image_dimensions(&made).expect("a JPEG"), (192, 320), "the poster is turned upright");
}

#[test]
fn a_codec_windows_lacks_is_named_and_a_file_that_is_no_video_is_damaged() {
    let folder = TempFolder::new("failures");
    let good = folder.0.join("good.mp4");
    fixture::write_mp4(&good, FixtureSpec::SHORT).expect("fixture");
    let unknown = folder.0.join("pro.mp4");
    assert!(fixture::replace_fourcc(&good, &unknown, b"avc1", b"apcn").expect("copy"), "the sample entry is found");
    let probe = poster::probe(&unknown, "video/mp4");
    assert_eq!(probe.codec.as_deref(), Some("FourCC 'apcn'"));
    let failure = probe.failure.clone().expect("no decoder");
    assert_eq!((failure.step, failure.hresult), ("decoder", video_rules::HRESULT_CODEC_NOT_FOUND));
    assert_eq!(poster::probe_sentence(&unknown, "video/mp4", &probe), "\"pro.mp4\" can't be played: Windows has no decoder for its FourCC 'apcn' video. Convert it to MP4 (H.264).");
    let cache = PosterCache::new(folder.0.join("posters"));
    assert_eq!(cache.ensure(&unknown, "video/mp4").expect_err("no poster").message, poster::probe_sentence(&unknown, "video/mp4", &probe));

    let text = folder.0.join("text.mp4");
    fs::write(&text, b"\x00\x00\x00\x14ftypmp42\x00\x00\x00\x00mp42 not a video").expect("file");
    let probe = poster::probe(&text, "video/mp4");
    assert!(probe.frame.is_none() && probe.failure.is_some());
    assert!(poster::probe_sentence(&text, "video/mp4", &probe).starts_with("\"text.mp4\" can't be played: the file is damaged or incomplete ("));
    assert!(cache.ensure(&text, "video/mp4").is_err());
    assert!(!cache.dir().exists() || fs::read_dir(cache.dir()).expect("listing").count() == 0, "nothing is written for a failure");

    let notes = folder.0.join("notes.txt");
    fs::write(&notes, b"just some notes").expect("file");
    assert!(cache.ensure_sniffed(&notes).expect_err("no video").message.contains("is not a supported image"));
}

/// The member `name` of the clone's export-videos.zip, written into `folder`.
fn clone_video(folder: &Path, name: &str) -> PathBuf {
    let archive = fs::File::open(fixtures().join("archives").join("export-videos.zip")).expect("the clone's fixture");
    let mut archive = zip::ZipArchive::new(archive).expect("a zip");
    let mut bytes = Vec::new();
    archive.by_name(name).expect("the member").read_to_end(&mut bytes).expect("bytes");
    let path = folder.join(name.rsplit('/').next().unwrap_or(name));
    fs::write(&path, bytes).expect("written");
    path
}

#[test]
fn the_webm_videos_of_the_clone_get_posters() {
    let folder = TempFolder::new("clone-webm");
    let cache = PosterCache::new(folder.0.join("posters"));
    for (name, size) in [("posts/reel1.webm", (240, 300)), ("posts/slide3_2.webm", (300, 240))] {
        let video = clone_video(&folder.0, name);
        let probe = poster::probe(&video, "video/webm");
        if probe.failure.as_ref().is_some_and(|failure| failure.hresult == video_rules::HRESULT_UNSUPPORTED_BYTESTREAM) {
            println!("SKIPPED {name}: this Windows reads no WebM ({:?})", probe.failure);
            continue;
        }
        assert_eq!(probe.codec.as_deref(), Some("VP8"), "{name}");
        assert_eq!(probe.size, Some(size), "{name}");
        let made = cache.ensure_sniffed(&video).expect("a poster");
        assert_eq!(image::image_dimensions(&made).expect("a JPEG"), size, "{name}");
    }
    // The clone's broken4.mov is no video Windows can read.
    let broken = clone_video(&folder.0, "posts/broken4.mov");
    assert!(cache.ensure_sniffed(&broken).is_err());
}

#[test]
fn a_vp9_webm_recorded_in_a_browser_gets_a_poster_without_a_length() {
    let folder = TempFolder::new("vp9");
    let video = folder.0.join("vp9.webm");
    fs::copy(fixtures().join("video").join("vp9_640x360.webm"), &video).expect("the fixture");
    let probe = poster::probe(&video, "video/webm");
    if let Some(failure) = probe.failure.as_ref().filter(|failure| failure.step == "decoder" || failure.hresult == video_rules::HRESULT_UNSUPPORTED_BYTESTREAM) {
        println!("SKIPPED a_vp9_webm_recorded_in_a_browser_gets_a_poster_without_a_length: no VP9 decoder or WebM reader on this Windows ({failure:?})");
        return;
    }
    assert_eq!(probe.codec.as_deref(), Some("VP9"));
    assert_eq!(probe.size, Some((640, 360)));
    assert_eq!(probe.duration, None, "a browser's recording has no length in its header");
    let made = PosterCache::new(folder.0.join("posters")).ensure(&video, "video/webm").expect("a poster");
    assert_eq!(image::image_dimensions(&made).expect("a JPEG"), (640, 360));
}
