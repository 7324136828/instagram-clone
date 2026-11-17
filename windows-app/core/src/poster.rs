//! Posters of loose videos: the first frame of a video as a JPEG in an app-owned cache (video
//! brief 5.2).
//!
//! An export ships the poster of each video; a loose video file has none. What the feed, the
//! grid, a download and the browser pages show for it is made here: Media Foundation's source
//! reader decodes the first frame (4-66 ms in the video spike), the frame is turned by the
//! stream's rotation (the reader hands frames out as stored, phones store them sideways),
//! scaled to at most `POSTER_MAX_SIDE` pixels per side and written as a JPEG of quality
//! `POSTER_JPEG_QUALITY` into `%LOCALAPPDATA%\instagram-windows-app\posters`. Nothing is ever
//! written next to the video. The shell's thumbnail factory is not used: it writes into the
//! user's Explorer thumbnail cache and gives no usable reason when it fails.
//!
//! A poster is named after the video's folded path, size and modified time, so a changed video
//! gets a new poster; old ones go when the cache is cleaned at the start (`clean_up`, oldest
//! first, `POSTER_CACHE_MAX_BYTES`). Making a poster blocks for tens of milliseconds and enters
//! COM, so it is only called on worker threads (the picture loader, the export thread, the HTTP
//! threads), never on the UI thread.
//!
//! The source reader is also the app's codec probe: when a video cannot be played, the codec of
//! its first video stream names the Store extension the user needs (`probe_codec`).

use std::fs::{self, File};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::messages;
use crate::model::fold_path;
use crate::sniff;
use crate::text;
use crate::video_rules::{self, PlaybackFailure};

#[cfg(windows)]
#[doc(hidden)]
pub mod fixture;

/// The longest side of a poster in pixels: Instagram's width. Posters of 1080p videos stay at
/// 60-200 KB.
pub const POSTER_MAX_SIDE: u32 = 1080;
/// JPEG quality of a poster (the quality of the browser copies of HEIC pictures).
pub const POSTER_JPEG_QUALITY: u8 = 90;
/// The poster cache is cut back to this size at the start, oldest posters first (about 2,000
/// posters).
pub const POSTER_CACHE_MAX_BYTES: u64 = 256 * 1024 * 1024;
/// How many samples the source reader is asked for before it is given up: a stream may begin
/// with samples that carry no picture yet.
const MAX_SAMPLES_READ: usize = 50;

/// A decoded frame: RGBA, opaque, rows from the top.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Frame {
    /// Pixels per row.
    pub width: u32,
    /// Rows.
    pub height: u32,
    /// `width * height * 4` bytes.
    pub rgba: Vec<u8>,
}

/// What the source reader found in a video.
#[derive(Clone, Debug, Default)]
pub struct Probe {
    /// The first frame, unturned, when it could be decoded.
    pub frame: Option<Frame>,
    /// The codec of the first video stream ("H.264", "HEVC", "VP9", ... or "FourCC 'apcn'").
    pub codec: Option<String>,
    /// Seconds, from the presentation descriptor (none for a WebM recorded in a browser).
    pub duration: Option<f64>,
    /// The visible size of the frame (the display aperture), unturned.
    pub size: Option<(u32, u32)>,
    /// Degrees the picture must be turned clockwise (`MF_MT_VIDEO_ROTATION`: 0, 90, 180, 270).
    pub rotation: u32,
    /// The step that failed, when one did.
    pub failure: Option<ProbeFailure>,
    /// How long the probe took.
    pub elapsed: Duration,
}

/// Why the source reader could not deliver the first frame.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProbeFailure {
    /// The step: "open", "source reader", "decoder", "read sample", "frame", ...
    pub step: &'static str,
    /// The HRESULT (0 when no call failed, for example a stream without frames).
    pub hresult: i32,
    /// "HRESULT 0x........: text", or a plain description.
    pub text: String,
}

/// Why no poster could be made. `message` is the sentence of `video_rules::classify`: it names
/// the file and says why (no decoder, damaged, not a video).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PosterError {
    /// The sentence for the user and the log.
    pub message: String,
}

impl PosterError {
    /// An error with its sentence.
    pub fn new(message: impl Into<String>) -> PosterError {
        PosterError { message: message.into() }
    }
}

impl std::fmt::Display for PosterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PosterError {}

/// Where a download gets the poster of a loose video from (video brief 5.8). The export thread
/// uses the app's `PosterCache`; tests inject their own.
pub trait PosterSource: Sync {
    /// The poster file of the loose video at `video` (sniffed type `mime`), made now when there
    /// is none; `None` when it cannot be made (the video is exported without one).
    fn poster_for(&self, video: &Path, mime: &str) -> Option<PathBuf>;
}

/// A source without posters: loose videos are exported without one.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoPosters;

impl PosterSource for NoPosters {
    fn poster_for(&self, _video: &Path, _mime: &str) -> Option<PathBuf> {
        None
    }
}

/// The app-owned folder of posters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PosterCache {
    dir: PathBuf,
}

/// Something that turns a video into its first frame (the source reader, or a test's stand-in).
pub type FrameMaker<'a> = &'a dyn Fn(&Path, &str) -> Result<(Frame, u32), PosterError>;

impl PosterCache {
    /// The cache in `dir` (`settings::AppPaths::poster_cache_dir`); the folder is made when the
    /// first poster is written.
    pub fn new(dir: PathBuf) -> PosterCache {
        PosterCache { dir }
    }

    /// The folder.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// The file name of the poster of a video with this folded path, size and modified time:
    /// 16 hex digits of FNV-1a 64 over `"{path}|{size}|{mtime}"` and ".jpg".
    pub fn file_name(folded_path: &str, size: u64, modified_ms: i64) -> String {
        format!("{:016x}.jpg", text::fnv1a_64(format!("{folded_path}|{size}|{modified_ms}").as_bytes()))
    }

    /// The poster file the video at `video` would have, from its size and modified time now.
    pub fn poster_path(&self, video: &Path) -> io::Result<PathBuf> {
        let metadata = fs::metadata(video)?;
        let modified = metadata.modified().map(text::epoch_ms_from_system_time).unwrap_or(0);
        Ok(self.dir.join(PosterCache::file_name(&fold_path(video), metadata.len(), modified)))
    }

    /// The poster of the video at `video` (sniffed type `mime`): the cached file when there is
    /// one (its modified time is renewed, so the clean-up keeps posters in use), else made now
    /// with the source reader. Call it on a worker thread.
    pub fn ensure(&self, video: &Path, mime: &str) -> Result<PathBuf, PosterError> {
        self.ensure_with(video, mime, &first_frame)
    }

    /// `ensure` for a video whose type is not known yet: its first bytes are sniffed.
    pub fn ensure_sniffed(&self, video: &Path) -> Result<PathBuf, PosterError> {
        let file = messages::display_name(video);
        let head = sniff::read_head(video).map_err(|error| PosterError::new(messages::file_unreadable(&file, &error.to_string())))?;
        match sniff::sniff_media(&head) {
            Some(sniffed) if sniffed.kind == crate::model::MediaKind::Video => self.ensure(video, sniffed.mime),
            _ => Err(PosterError::new(messages::file_not_supported(&file))),
        }
    }

    /// `ensure` with the frame maker as a parameter (tests). The maker returns the first frame
    /// and the rotation to apply.
    pub fn ensure_with(&self, video: &Path, mime: &str, maker: FrameMaker<'_>) -> Result<PathBuf, PosterError> {
        let file = messages::display_name(video);
        let target = self.poster_path(video).map_err(|error| PosterError::new(messages::file_unreadable(&file, &error.to_string())))?;
        if target.is_file() {
            // Renewed so that the clean-up, oldest first, keeps what is in use.
            if let Ok(handle) = File::options().write(true).open(&target) {
                let _ = handle.set_modified(SystemTime::now());
            }
            return Ok(target);
        }
        let started = std::time::Instant::now();
        let (frame, rotation) = maker(video, mime)?;
        let jpeg = encode_poster(&frame, rotation).map_err(|error| PosterError::new(messages::video_cannot_play(&file, &messages::video_damaged(&error))))?;
        let written = write_atomically(&self.dir, &target, &jpeg).map_err(|error| PosterError::new(messages::file_unreadable(&file, &error.to_string())));
        log::info!("poster for {file} in {} ms", started.elapsed().as_millis());
        written.map(|()| target)
    }

    /// Cuts the folder back to `max_bytes`, oldest posters first (by modified time), and removes
    /// posters that were never finished. Run at the start on a thread of its own. Returns how
    /// many files were removed.
    pub fn clean_up(&self, max_bytes: u64) -> usize {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return 0;
        };
        let mut removed = 0;
        let mut posters: Vec<(SystemTime, u64, PathBuf)> = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            let Ok(metadata) = entry.metadata() else { continue };
            if !metadata.is_file() {
                continue;
            }
            if name.contains(".tmp-") {
                if fs::remove_file(entry.path()).is_ok() {
                    removed += 1;
                }
                continue;
            }
            if !is_poster_name(&name) {
                continue;
            }
            posters.push((metadata.modified().unwrap_or(SystemTime::UNIX_EPOCH), metadata.len(), entry.path()));
        }
        posters.sort();
        let mut total: u64 = posters.iter().map(|(_, size, _)| size).sum();
        for (_, size, path) in posters {
            if total <= max_bytes {
                break;
            }
            if fs::remove_file(&path).is_ok() {
                total -= size;
                removed += 1;
            }
        }
        removed
    }
}

impl PosterSource for PosterCache {
    fn poster_for(&self, video: &Path, mime: &str) -> Option<PathBuf> {
        match self.ensure(video, mime) {
            Ok(poster) => Some(poster),
            Err(error) => {
                // Posters are optional in the export format: the video goes without one.
                log::warn!("{error}");
                None
            }
        }
    }
}

/// True for the name of a finished poster: 16 hex digits and ".jpg".
fn is_poster_name(name: &str) -> bool {
    name.strip_suffix(".jpg").is_some_and(|stem| stem.len() == 16 && stem.bytes().all(|byte| byte.is_ascii_hexdigit()))
}

/// Writes `bytes` to `target` through a temporary file in `dir`, so that a half-written poster
/// is never used.
fn write_atomically(dir: &Path, target: &Path, bytes: &[u8]) -> io::Result<()> {
    fs::create_dir_all(dir)?;
    let name = target.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
    let unique = text::fnv1a_64(format!("{}|{:?}|{:?}", std::process::id(), std::thread::current().id(), SystemTime::now()).as_bytes()) as u32;
    let temporary = dir.join(format!("{name}.tmp-{unique:08x}"));
    fs::write(&temporary, bytes)?;
    if let Err(error) = fs::rename(&temporary, target) {
        let _ = fs::remove_file(&temporary);
        // Another thread may have finished the same poster first.
        if !target.is_file() {
            return Err(error);
        }
    }
    Ok(())
}

/// Turns `frame` by `rotation` degrees clockwise, scales it to at most `POSTER_MAX_SIDE` pixels
/// per side and encodes it as a JPEG.
pub fn encode_poster(frame: &Frame, rotation: u32) -> Result<Vec<u8>, String> {
    let image = image::RgbaImage::from_raw(frame.width, frame.height, frame.rgba.clone()).ok_or_else(|| "the frame has the wrong size".to_owned())?;
    let turned = match rotation % 360 {
        90 => image::imageops::rotate90(&image),
        180 => image::imageops::rotate180(&image),
        270 => image::imageops::rotate270(&image),
        _ => image,
    };
    let (width, height) = turned.dimensions();
    let longest = width.max(height);
    let scaled = if longest > POSTER_MAX_SIDE {
        let scale = f64::from(POSTER_MAX_SIDE) / f64::from(longest);
        let w = ((f64::from(width) * scale).round() as u32).max(1);
        let h = ((f64::from(height) * scale).round() as u32).max(1);
        image::imageops::resize(&turned, w, h, image::imageops::FilterType::Triangle)
    } else {
        turned
    };
    let (width, height) = scaled.dimensions();
    let rgb: Vec<u8> = scaled.as_raw().as_chunks::<4>().0.iter().flat_map(|pixel| [pixel[0], pixel[1], pixel[2]]).collect();
    let mut jpeg = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, POSTER_JPEG_QUALITY)
        .encode(&rgb, width, height, image::ExtendedColorType::Rgb8)
        .map_err(|error| error.to_string())?;
    Ok(jpeg)
}

/// The production frame maker: the source reader's first frame and rotation, or the classified
/// reason (`video_rules::classify`) when there is none.
fn first_frame(video: &Path, mime: &str) -> Result<(Frame, u32), PosterError> {
    let probe = probe(video, mime);
    match probe.frame {
        Some(frame) => Ok((frame, probe.rotation)),
        None => Err(PosterError::new(probe_sentence(video, mime, &probe))),
    }
}

/// The sentence for a probe that delivered no frame (video brief 6.8).
pub fn probe_sentence(video: &Path, mime: &str, probe: &Probe) -> String {
    let file = messages::display_name(video);
    let failure = probe.failure.clone().unwrap_or(ProbeFailure { step: "frame", hresult: 0, text: "the video has no frame".to_owned() });
    let playback = PlaybackFailure {
        media_error: 0,
        hresult: failure.hresult,
        text: failure.text,
        codec: probe.codec.clone(),
        no_picture: failure.step == "no video",
    };
    video_rules::classify(&file, mime, &playback)
}

/// Starts Media Foundation once per process. It is never shut down: measured, an `MFShutdown`
/// per use cost 200 ms (it waits for its work queues), and the end of the process releases it.
/// Returns false when it cannot be started (an N edition of Windows without the Media Feature
/// Pack).
#[cfg(windows)]
pub fn start_media_foundation() -> bool {
    use windows::Win32::Media::MediaFoundation::{MFStartup, MFSTARTUP_FULL, MF_VERSION};
    static STARTED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    // SAFETY: MFStartup may be called from any thread; it is reference counted per process and
    // deliberately never balanced (see above).
    *STARTED.get_or_init(|| unsafe { MFStartup(MF_VERSION, MFSTARTUP_FULL) }.is_ok())
}

/// Not on other platforms.
#[cfg(not(windows))]
pub fn start_media_foundation() -> bool {
    false
}

/// Reads the codec, the length, the size, the rotation and the first frame of the video at
/// `path` with the source reader. `mime` (sniffed) chooses the parser, never the file name.
#[cfg(windows)]
pub fn probe(path: &Path, mime: &str) -> Probe {
    platform::probe(path, mime, true)
}

/// The codec of the first video stream only (no frame is decoded): what a failed playback
/// names. Cheap (a few milliseconds).
#[cfg(windows)]
pub fn probe_codec(path: &Path, mime: &str) -> Option<String> {
    platform::probe(path, mime, false).codec
}

/// Other platforms have no source reader.
#[cfg(not(windows))]
pub fn probe(_path: &Path, _mime: &str) -> Probe {
    Probe { failure: Some(ProbeFailure { step: "open", hresult: 0, text: messages::DECODER_ONLY_ON_WINDOWS.to_owned() }), ..Probe::default() }
}

/// Other platforms have no source reader.
#[cfg(not(windows))]
pub fn probe_codec(_path: &Path, _mime: &str) -> Option<String> {
    None
}

/// The codec name of a Media Foundation video subtype: the common ones by name, any other by
/// its FourCC. The MPEG-4 reader stores an unknown sample entry type big-endian in `data1`, so
/// it is printed in that byte order ("apcn", as written in the file).
#[cfg(windows)]
pub fn codec_name(subtype: &windows::core::GUID) -> String {
    use windows::Win32::Media::MediaFoundation::{MFVideoFormat_AV1, MFVideoFormat_H264, MFVideoFormat_HEVC, MFVideoFormat_MJPG, MFVideoFormat_MP4V, MFVideoFormat_VP80, MFVideoFormat_VP90};
    let known = [
        (MFVideoFormat_H264, "H.264"),
        (MFVideoFormat_HEVC, "HEVC"),
        (MFVideoFormat_VP90, "VP9"),
        (MFVideoFormat_VP80, "VP8"),
        (MFVideoFormat_AV1, "AV1"),
        (MFVideoFormat_MP4V, "MPEG-4 Part 2"),
        (MFVideoFormat_MJPG, "Motion JPEG"),
    ];
    match known.iter().find(|(guid, _)| guid == subtype) {
        Some((_, name)) => (*name).to_owned(),
        None => format!("FourCC '{}'", String::from_utf8_lossy(&subtype.data1.to_be_bytes())),
    }
}

/// Every `unsafe` call of the source reader.
#[cfg(windows)]
mod platform {
    use std::path::Path;
    use std::time::Instant;

    use windows::core::{Interface, HSTRING};
    use windows::Win32::Media::MediaFoundation::*;
    use windows::Win32::System::Com::{CoInitializeEx, CoUninitialize, COINIT_MULTITHREADED};

    use super::{Frame, Probe, ProbeFailure, MAX_SAMPLES_READ};

    /// COM (multithreaded apartment) for the length of one call on a worker thread.
    struct Apartment {
        entered: bool,
    }

    impl Apartment {
        fn enter() -> Apartment {
            // SAFETY: may be called on any thread; balanced in drop when it succeeded (also
            // S_FALSE: the thread was in the MTA already, and the count must still go down).
            let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            Apartment { entered: result.is_ok() }
        }
    }

    impl Drop for Apartment {
        fn drop(&mut self) {
            if self.entered {
                // SAFETY: balances the successful CoInitializeEx of `enter` on the same thread.
                unsafe { CoUninitialize() };
            }
        }
    }

    fn failure(step: &'static str, error: &windows::core::Error) -> ProbeFailure {
        ProbeFailure { step, hresult: error.code().0, text: format!("HRESULT 0x{:08X}: {}", error.code().0 as u32, error.message().trim()) }
    }

    /// The probe; `decode` false stops after the codec, length and rotation.
    pub(super) fn probe(path: &Path, mime: &str, decode: bool) -> Probe {
        let started = Instant::now();
        let _apartment = Apartment::enter();
        let mut probe = Probe::default();
        if !super::start_media_foundation() {
            probe.failure = Some(ProbeFailure { step: "open", hresult: 0, text: "Media Foundation is not available on this Windows".to_owned() });
            return probe;
        }
        if let Err(failed) = read(path, mime, decode, &mut probe) {
            probe.failure = Some(failed);
        }
        probe.elapsed = started.elapsed();
        probe
    }

    /// Fills `probe` step by step; the first step that fails ends it.
    fn read(path: &Path, mime: &str, decode: bool, probe: &mut Probe) -> Result<(), ProbeFailure> {
        // SAFETY (whole function): plain Media Foundation calls with valid arguments on this
        // thread, which is in the MTA (Apartment) with Media Foundation started; every buffer is
        // read only while it is locked, and only within the length the lock reports.
        unsafe {
            let mut attributes = None;
            MFCreateAttributes(&mut attributes, 1).map_err(|error| failure("attributes", &error))?;
            let attributes = attributes.ok_or_else(|| ProbeFailure { step: "attributes", hresult: 0, text: "no attribute store".to_owned() })?;
            let _ = attributes.SetUINT32(&MF_SOURCE_READER_ENABLE_VIDEO_PROCESSING, 1);
            let stream = MFCreateFile(MF_ACCESSMODE_READ, MF_OPENMODE_FAIL_IF_NOT_EXIST, MF_FILEFLAGS_NONE, &HSTRING::from(path.as_os_str())).map_err(|error| failure("open", &error))?;
            if let Ok(stream_attributes) = stream.cast::<IMFAttributes>() {
                let _ = stream_attributes.SetString(&MF_BYTESTREAM_CONTENT_TYPE, &HSTRING::from(mime));
            }
            let reader = MFCreateSourceReaderFromByteStream(&stream, &attributes).map_err(|error| failure("source reader", &error))?;
            let first_video = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;
            match reader.GetNativeMediaType(first_video, 0) {
                Ok(native) => {
                    if let Ok(subtype) = native.GetGUID(&MF_MT_SUBTYPE) {
                        probe.codec = Some(super::codec_name(&subtype));
                    }
                    probe.rotation = native.GetUINT32(&MF_MT_VIDEO_ROTATION).unwrap_or(0);
                }
                Err(error) => return Err(failure("no video", &error)),
            }
            if let Ok(value) = reader.GetPresentationAttribute(MF_SOURCE_READER_MEDIASOURCE.0 as u32, &MF_PD_DURATION) {
                if let Ok(ticks) = u64::try_from(&value) {
                    probe.duration = Some(ticks as f64 / 10_000_000.0);
                }
            }
            if !decode {
                return Ok(());
            }
            let _ = reader.SetStreamSelection(MF_SOURCE_READER_ALL_STREAMS.0 as u32, false);
            let _ = reader.SetStreamSelection(first_video, true);
            let wanted = MFCreateMediaType().map_err(|error| failure("media type", &error))?;
            let _ = wanted.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video);
            let _ = wanted.SetGUID(&MF_MT_SUBTYPE, &MFVideoFormat_RGB32);
            // No decoder (or no converter) for this codec fails here.
            reader.SetCurrentMediaType(first_video, None, &wanted).map_err(|error| failure("decoder", &error))?;
            let current = reader.GetCurrentMediaType(first_video).map_err(|error| failure("current type", &error))?;
            let frame_size = current.GetUINT64(&MF_MT_FRAME_SIZE).unwrap_or(0);
            let (width, height) = ((frame_size >> 32) as u32, frame_size as u32);
            let stride = current.GetUINT32(&MF_MT_DEFAULT_STRIDE).map(|value| value as i32).unwrap_or(width as i32 * 4);
            // The visible part, when the coded frame is larger (1080 lines are coded as 1088).
            let mut visible = (width, height);
            let mut area = MFVideoArea::default();
            let blob = std::slice::from_raw_parts_mut(&mut area as *mut MFVideoArea as *mut u8, std::mem::size_of::<MFVideoArea>());
            if current.GetBlob(&MF_MT_MINIMUM_DISPLAY_APERTURE, blob, None).is_ok() && area.Area.cx > 0 && area.Area.cy > 0 {
                visible = ((area.Area.cx as u32).min(width), (area.Area.cy as u32).min(height));
            }
            if visible.0 == 0 || visible.1 == 0 {
                return Err(ProbeFailure { step: "frame", hresult: 0, text: "the video has no frame size".to_owned() });
            }
            probe.size = Some(visible);
            for _ in 0..MAX_SAMPLES_READ {
                let mut flags = 0u32;
                let mut sample = None;
                reader.ReadSample(first_video, 0, None, Some(&mut flags), None, Some(&mut sample)).map_err(|error| failure("read sample", &error))?;
                if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
                    return Err(ProbeFailure { step: "frame", hresult: 0, text: "the video has no frame".to_owned() });
                }
                let Some(sample) = sample else { continue };
                let frame = match sample.GetBufferByIndex(0).ok().and_then(|buffer| buffer.cast::<IMF2DBuffer>().ok()) {
                    // The real pitch of the rows (the decoder pads a width that is no multiple
                    // of 16, which MF_MT_DEFAULT_STRIDE does not always say).
                    Some(two_d) => {
                        let mut first_row = std::ptr::null_mut();
                        let mut pitch = 0i32;
                        two_d.Lock2D(&mut first_row, &mut pitch).map_err(|error| failure("lock", &error))?;

                        let frame = rows_to_frame(first_row, pitch, visible);
                        let _ = two_d.Unlock2D();
                        frame
                    }
                    None => {
                        let buffer = sample.ConvertToContiguousBuffer().map_err(|error| failure("buffer", &error))?;
                        let mut data = std::ptr::null_mut();
                        let mut length = 0u32;
                        buffer.Lock(&mut data, None, Some(&mut length)).map_err(|error| failure("lock", &error))?;

                        let bytes = std::slice::from_raw_parts(data, length as usize);
                        let frame = bgrx_to_frame(bytes, stride, height, visible);
                        let _ = buffer.Unlock();
                        frame
                    }
                };
                probe.frame = Some(frame.ok_or_else(|| ProbeFailure { step: "frame", hresult: 0, text: "the decoded frame is shorter than its size says".to_owned() })?);
                return Ok(());
            }
            Err(ProbeFailure { step: "frame", hresult: 0, text: "the video has no frame".to_owned() })
        }
    }

    /// The visible part of a locked 2D buffer of RGB32 pixels: `first_row` is the top row and
    /// `pitch` the distance to the next one in bytes (negative for a bottom-up frame).
    ///
    /// # Safety
    /// `first_row` and `pitch` must come from a successful `IMF2DBuffer::Lock2D` of a frame of at
    /// least `visible` pixels, still locked.
    unsafe fn rows_to_frame(first_row: *mut u8, pitch: i32, visible: (u32, u32)) -> Option<Frame> {
        let (width, height) = (visible.0 as usize, visible.1 as usize);
        if first_row.is_null() || (pitch.unsigned_abs() as usize) < width * 4 {
            return None;
        }
        let mut rgba = Vec::with_capacity(width * height * 4);
        for line in 0..height {
            // SAFETY: the caller's lock covers `height` rows `pitch` bytes apart from the top row.
            let row = unsafe { std::slice::from_raw_parts(first_row.offset(line as isize * pitch as isize), width * 4) };
            for pixel in row.as_chunks::<4>().0 {
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
            }
        }
        Some(Frame { width: visible.0, height: visible.1, rgba })
    }

    /// The visible part of an RGB32 frame (bytes B, G, R, X per pixel; rows `stride` bytes
    /// apart, bottom-up when negative) as opaque RGBA rows from the top. `None` when the buffer
    /// is shorter than its size says.
    fn bgrx_to_frame(bytes: &[u8], stride: i32, coded_height: u32, visible: (u32, u32)) -> Option<Frame> {
        let (width, height) = (visible.0 as usize, visible.1 as usize);
        // Measured: a 360-pixel-wide H.264 video comes as rows of 368 pixels (1472 bytes) while
        // MF_MT_DEFAULT_STRIDE says 1440. A buffer that holds whole rows of another length says
        // what the rows really are.
        let stated = stride.unsigned_abs() as usize;
        let rows = coded_height as usize;
        let row = if rows > 0 && bytes.len() != stated * rows && bytes.len().is_multiple_of(rows) && bytes.len() / rows >= width * 4 { bytes.len() / rows } else { stated };
        if row < width * 4 || bytes.len() < row * coded_height as usize || height > coded_height as usize {
            return None;
        }
        let mut rgba = Vec::with_capacity(width * height * 4);
        for line in 0..height {
            // A bottom-up frame stores its top row last.
            let source = if stride >= 0 { line } else { coded_height as usize - 1 - line };
            let start = source * row;
            for pixel in bytes[start..start + width * 4].as_chunks::<4>().0 {
                rgba.extend_from_slice(&[pixel[2], pixel[1], pixel[0], 255]);
            }
        }
        Some(Frame { width: visible.0, height: visible.1, rgba })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct TempFolder(PathBuf);

    impl TempFolder {
        fn new(label: &str) -> TempFolder {
            let folder = std::env::temp_dir().join(format!("instagram-core-poster-{label}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&folder);
            fs::create_dir_all(&folder).expect("a temporary folder");
            TempFolder(folder)
        }
    }

    impl Drop for TempFolder {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn red_frame(width: u32, height: u32) -> Frame {
        let mut rgba = Vec::new();
        for y in 0..height {
            for x in 0..width {
                // Red on the left half, blue on the right: a turn can be told apart.
                rgba.extend_from_slice(if x < width / 2 { &[250, 10, 10, 255] } else { &[10, 10, 250, 255] });
                let _ = y;
            }
        }
        Frame { width, height, rgba }
    }

    #[test]
    fn a_poster_is_named_after_the_path_size_and_time_of_its_video() {
        let one = PosterCache::file_name(r"c:\videos\a.mp4", 100, 5);
        assert!(is_poster_name(&one), "{one}");
        assert_eq!(one.len(), 20);
        assert_ne!(one, PosterCache::file_name(r"c:\videos\a.mp4", 101, 5));
        assert_ne!(one, PosterCache::file_name(r"c:\videos\a.mp4", 100, 6));
        assert_ne!(one, PosterCache::file_name(r"c:\videos\b.mp4", 100, 5));
        assert_eq!(one, PosterCache::file_name(r"c:\videos\a.mp4", 100, 5));
        assert!(!is_poster_name("notes.txt") && !is_poster_name("0123456789abcdef.jpg.tmp-1") && !is_poster_name("xyz.jpg"));
    }

    #[test]
    fn ensure_writes_through_a_temporary_file_once_and_reuses_it() {
        let folder = TempFolder::new("ensure");
        let video = folder.0.join("clip.mp4");
        fs::write(&video, b"a video").expect("video");
        let cache = PosterCache::new(folder.0.join("posters"));
        let calls = std::sync::atomic::AtomicUsize::new(0);
        let maker = |_: &Path, mime: &str| {
            assert_eq!(mime, "video/mp4");
            calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok((red_frame(40, 20), 0))
        };
        let poster = cache.ensure_with(&video, "video/mp4", &maker).expect("a poster");
        assert_eq!(poster.parent(), Some(cache.dir()));
        assert_eq!(image::image_dimensions(&poster).expect("a JPEG"), (40, 20));
        assert_eq!(cache.ensure_with(&video, "video/mp4", &maker).expect("again"), poster);
        assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1, "made once");
        // Nothing beside the video, nothing left half-written.
        let names: Vec<String> = fs::read_dir(&folder.0).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
        assert_eq!(names.len(), 2, "{names:?}");
        assert_eq!(fs::read_dir(cache.dir()).unwrap().count(), 1);
        // A changed video gets a new poster.
        fs::write(&video, b"a longer video").expect("video");
        assert_ne!(cache.ensure_with(&video, "video/mp4", &maker).expect("new"), poster);
    }

    #[test]
    fn a_failing_maker_writes_nothing_and_says_why() {
        let folder = TempFolder::new("fails");
        let video = folder.0.join("broken.mov");
        fs::write(&video, b"x").expect("video");
        let cache = PosterCache::new(folder.0.join("posters"));
        let error = cache.ensure_with(&video, "video/quicktime", &|_, _| Err(PosterError::new("no frame"))).expect_err("fails");
        assert_eq!(error.message, "no frame");
        assert!(!cache.dir().exists());
        let missing = cache.ensure_with(&folder.0.join("gone.mp4"), "video/mp4", &|_, _| Ok((red_frame(2, 2), 0))).expect_err("missing");
        assert!(missing.message.starts_with("\"gone.mp4\" could not be read: "), "{missing}");
        assert_eq!(cache.poster_for(&folder.0.join("gone.mp4"), "video/mp4"), None);
        assert_eq!(NoPosters.poster_for(&video, "video/mp4"), None);
    }

    #[test]
    fn posters_are_turned_and_scaled_to_their_longest_side() {
        let jpeg = encode_poster(&red_frame(2160, 1000), 90).expect("encodes");
        let decoded = image::load_from_memory(&jpeg).expect("a JPEG").to_rgb8();
        assert_eq!(decoded.dimensions(), (500, 1080));
        // Turned clockwise: the left (red) half is now on top.
        let top = decoded.get_pixel(250, 100);
        let bottom = decoded.get_pixel(250, 1000);
        assert!(top[0] > 200 && top[2] < 60, "{top:?}");
        assert!(bottom[2] > 200 && bottom[0] < 60, "{bottom:?}");
        let small = image::load_from_memory(&encode_poster(&red_frame(64, 48), 0).expect("encodes")).expect("a JPEG");
        assert_eq!((small.width(), small.height()), (64, 48));
        let upside_down = image::load_from_memory(&encode_poster(&red_frame(64, 48), 180).expect("encodes")).expect("a JPEG").to_rgb8();
        assert!(upside_down.get_pixel(5, 24)[2] > 200, "left is blue after half a turn");
        assert!(encode_poster(&Frame { width: 10, height: 10, rgba: vec![0; 3] }, 0).is_err());
    }

    #[test]
    fn the_clean_up_removes_the_oldest_posters_first_and_unfinished_ones() {
        let folder = TempFolder::new("clean");
        let cache = PosterCache::new(folder.0.clone());
        let now = SystemTime::now();
        let names = ["0000000000000001.jpg", "0000000000000002.jpg", "0000000000000003.jpg"];
        for (age, name) in names.iter().enumerate() {
            let path = folder.0.join(name);
            fs::write(&path, vec![0u8; 100]).unwrap();
            // The first is the newest, the last the oldest.
            File::options().write(true).open(&path).unwrap().set_modified(now - Duration::from_secs(60 * (age as u64 + 1))).unwrap();
        }
        fs::write(folder.0.join("0000000000000004.jpg.tmp-abcd1234"), b"half").unwrap();
        fs::write(folder.0.join("readme.txt"), vec![0u8; 1000]).unwrap();
        assert_eq!(cache.clean_up(250), 2, "the unfinished one and the oldest");
        let mut left: Vec<String> = fs::read_dir(&folder.0).unwrap().flatten().map(|entry| entry.file_name().to_string_lossy().into_owned()).collect();
        left.sort();
        assert_eq!(left, ["0000000000000001.jpg", "0000000000000002.jpg", "readme.txt"]);
        assert_eq!(cache.clean_up(POSTER_CACHE_MAX_BYTES), 0);
        assert_eq!(PosterCache::new(folder.0.join("missing")).clean_up(0), 0);
    }

    #[test]
    fn a_probe_without_a_frame_becomes_the_classified_sentence() {
        let probe = Probe { codec: Some("HEVC".to_owned()), failure: Some(ProbeFailure { step: "decoder", hresult: video_rules::HRESULT_CODEC_NOT_FOUND, text: "HRESULT 0xC00D5212: No suitable transform.".to_owned() }), ..Probe::default() };
        assert!(probe_sentence(Path::new("IMG_1.MOV"), "video/quicktime", &probe).ends_with("HEVC videos need the HEVC Video Extensions from the Microsoft Store."));
        let damaged = Probe { failure: Some(ProbeFailure { step: "source reader", hresult: 0xC00D_36C4_u32 as i32, text: "HRESULT 0xC00D36C4: The byte stream type of the given URL is unsupported.".to_owned() }), ..Probe::default() };
        assert_eq!(probe_sentence(Path::new("text.mp4"), "video/mp4", &damaged), "\"text.mp4\" can't be played: the file is damaged or incomplete (The byte stream type of the given URL is unsupported).");
        let sound = Probe { failure: Some(ProbeFailure { step: "no video", hresult: 0xC00D_36B3_u32 as i32, text: "HRESULT 0xC00D36B3: The stream number provided was invalid.".to_owned() }), ..Probe::default() };
        assert_eq!(probe_sentence(Path::new("song.mp4"), "video/mp4", &sound), "\"song.mp4\" can't be played: it has no picture.");
    }
}
