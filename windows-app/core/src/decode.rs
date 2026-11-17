//! Decodes a picture file into RGBA pixels for display: what the browser does for the reference
//! web project (spec-viewer 14.5; spike-notes 4 and 5).
//!
//! ```text
//! JPEG, PNG, GIF, WebP, BMP   the `image` crate (pure Rust); the first frame of an animation
//! everything else             Windows Imaging Component (HEIC / HEIF and AVIF need the codecs
//!                             of the Microsoft Store; the error says so when they are missing)
//! ```
//!
//! The format is decided by the file's first bytes (`sniff`), never by its extension. A file the
//! `image` crate fails on gets a second chance with Windows Imaging Component (WIC), which reads
//! some variants the pure-Rust decoders do not.
//!
//! Orientation: the EXIF orientation is applied, as a browser does, so the reported size is the
//! size on screen. The `image` crate reads it for JPEG, PNG and WebP; through WIC it is read for
//! JPEG and TIFF files. HEIC and AVIF files are NOT turned by this module: the Microsoft HEIF
//! decoder applies the rotation and mirroring stored in the file (`irot`, `imir`) itself, and the
//! EXIF orientation of such a file only repeats them (a test checks the rotation on a machine
//! that has the codec).
//!
//! Size: a picture larger than the caller's limit is scaled down on the CPU (the GPU has a
//! maximum texture side and only a bilinear filter), with a Catmull-Rom filter for the `image`
//! crate and the Fant filter for WIC. Transparent pictures are scaled with premultiplied alpha,
//! so the colour of invisible pixels does not bleed into edges. A picture with more than
//! `MAX_SOURCE_PIXELS` pixels is refused before any pixel is decoded (a decompression bomb is a
//! small file that claims an enormous size).
//!
//! The file itself is never changed and is closed again when a function returns: exports copy
//! the bytes on disk.
//!
//! Decoding is slow (tens to hundreds of milliseconds) and WIC needs COM: call these functions
//! on a worker thread, NEVER on the UI thread. COM is initialised per call as a multithreaded
//! apartment and released again; on a thread that already has another apartment it is used as
//! it is. (Putting the UI thread into the multithreaded apartment makes winit panic when it
//! creates the window, spike-notes 5.)
//!
//! This is the only module with `unsafe` code besides the single Win32 call in `text`: the WIC
//! calls, all inside the private module `wic`. On other platforms the module compiles and the
//! WIC path reports an error.

use std::error::Error as _;
use std::fs::File;
use std::io::{BufReader, ErrorKind};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::Path;

use image::imageops::{self, FilterType};
use image::metadata::Orientation;
use image::{DynamicImage, ImageBuffer, ImageDecoder, ImageError, ImageFormat, ImageReader, ImageResult, Limits, Pixel};

use crate::messages::{self, display_name};
use crate::model::MediaKind;
use crate::sniff::{self, MediaFormat};

/// The largest texture side egui's wgpu backend accepts (spike-notes 1). `decode_picture` is
/// called with at most this, usually with less (the size on screen in pixels).
pub const MAX_TEXTURE_SIDE: u32 = 8192;

/// A picture with more pixels than this (width x height of the file) is not decoded: 256
/// megapixels would need a gigabyte of RGBA. It is reported with `messages::picture_too_large`
/// and can still be exported.
pub const MAX_SOURCE_PIXELS: u64 = 256 * 1024 * 1024;

/// The memory budget for the decoded pixels of one picture: `MAX_SOURCE_PIXELS` as RGBA, one
/// gibibyte. A picture within the pixel limit whose own pixel format needs more (16 bits per
/// channel) is refused as too large as well.
pub const MAX_DECODED_BYTES: u64 = MAX_SOURCE_PIXELS * 4;

/// The short side, in pixels, the app asks `decode_thumbnail` for by default: a grid tile is a
/// third of the 935 px grid, shown at up to 150 % display scaling.
pub const DEFAULT_THUMBNAIL_SIDE: u32 = 480;

/// What a decoder of the `image` crate may allocate beside the pixels themselves.
const DECODER_HEADROOM_BYTES: u64 = 64 * 1024 * 1024;

/// The Catmull-Rom resize of the `image` crate keeps an intermediate picture of 32-bit floats
/// (source width x target height, four channels). Above this size the plain area average is
/// used instead, which needs no intermediate picture.
const MAX_RESIZE_SCRATCH_BYTES: u64 = 256 * 1024 * 1024;

/// Bytes per pixel of that intermediate picture.
const RESIZE_SCRATCH_BYTES_PER_PIXEL: u64 = 16;

/// A picture at least this many times larger than its target is first averaged down to
/// `PRESHRINK_FACTOR` times the target: the result looks the same and takes a fraction of the
/// time, which matters for the thumbnails of camera pictures.
const PRESHRINK_MIN_RATIO: u32 = 4;

/// See `PRESHRINK_MIN_RATIO`.
const PRESHRINK_FACTOR: u32 = 2;

/// Results of Windows Imaging Component and of the HEIF decoder behind it that mean "the data
/// is damaged", not "no codec": WINCODEC_ERR_BADIMAGE, _BADHEADER, _FRAMEMISSING,
/// _BADMETADATAHEADER, _BADSTREAMDATA, _STREAMREAD, _STREAMNOTAVAILABLE and Media Foundation's
/// MF_E_INVALID_FILE_FORMAT (what a truncated HEIC file gives, spike-notes 5).
const HRESULTS_OF_DAMAGE: [u32; 8] = [
    HRESULT_BAD_IMAGE,
    0x8898_2F61,
    0x8898_2F62,
    0x8898_2F63,
    0x8898_2F70,
    0x8898_2F72,
    0x8898_2F73,
    0xC00D_36BE,
];

/// WINCODEC_ERR_BADIMAGE.
const HRESULT_BAD_IMAGE: u32 = 0x8898_2F60;

/// The facility of an HRESULT that wraps a Win32 error: file not found, access denied, sharing
/// violation. The file could not be read at all.
const HRESULT_FACILITY_WIN32: u32 = 0x8007;

/// A decoded picture, ready for `egui::ColorImage::from_rgba_unmultiplied`.
#[derive(Clone, PartialEq, Eq)]
pub struct DecodedImage {
    /// Width of `rgba` in pixels, after orientation and scaling.
    pub width: u32,
    /// Height of `rgba` in pixels, after orientation and scaling.
    pub height: u32,
    /// `width * height * 4` bytes: straight (not premultiplied) RGBA, row by row from the top.
    pub rgba: Vec<u8>,
    /// Width of the picture in the file, after orientation: what "100 %" zoom and the feed's
    /// aspect ratio refer to.
    pub source_width: u32,
    /// Height of the picture in the file, after orientation.
    pub source_height: u32,
}

impl std::fmt::Debug for DecodedImage {
    /// The pixels are left out: a debug print of megabytes helps nobody.
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DecodedImage")
            .field("width", &self.width)
            .field("height", &self.height)
            .field("rgba_bytes", &self.rgba.len())
            .field("source_width", &self.source_width)
            .field("source_height", &self.source_height)
            .finish()
    }
}

/// Why a picture could not be decoded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecodeErrorKind {
    /// The file cannot be opened or read.
    Unreadable,
    /// No decoder for the format: the WIC codec is missing, or the platform has no WIC.
    NoDecoder,
    /// The data is damaged, truncated or not a picture.
    Damaged,
    /// More than `MAX_SOURCE_PIXELS`, or more memory than `MAX_DECODED_BYTES`.
    TooLarge,
}

/// Raised when a picture cannot be shown. `message` is a sentence for the failed-picture
/// placeholder (`messages::picture_not_shown`, `picture_needs_codec`, `picture_too_large`,
/// `file_unreadable`, `file_empty`, `file_is_a_video`) that quotes the file name; the picture
/// can still be exported.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecodeError {
    /// The kind of failure, for tests and for the UI's choice of placeholder.
    pub kind: DecodeErrorKind,
    /// The sentence for the placeholder.
    pub message: String,
}

impl DecodeError {
    /// An error of `kind` with its sentence.
    pub fn new(kind: DecodeErrorKind, message: impl Into<String>) -> DecodeError {
        DecodeError { kind, message: message.into() }
    }
}

impl std::fmt::Display for DecodeError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for DecodeError {}

/// `side * numerator / denominator`, rounded to the nearest whole number (a half rounds up) and
/// at least 1. Whole-number arithmetic, so the result does not depend on floating point.
fn scale_side(side: u32, numerator: u32, denominator: u32) -> u32 {
    let doubled = u64::from(side) * u64::from(numerator) * 2 + u64::from(denominator);
    let scaled = doubled / (u64::from(denominator) * 2);
    u32::try_from(scaled).unwrap_or(u32::MAX).max(1)
}

/// The size of a picture scaled to fit a box of `max_side` x `max_side` pixels, keeping its
/// aspect ratio: unchanged when `max(width, height) <= max_side` (a picture is never scaled
/// up), otherwise `scale = max_side / max(width, height)` and each side is
/// `max(1, round(side * scale))`. (4000, 3000, 2000) -> (2000, 1500); (100, 50, 2000) ->
/// (100, 50); (8000, 10, 100) -> (100, 1). A `max_side` of 0 counts as 1.
pub fn fit_within(width: u32, height: u32, max_side: u32) -> (u32, u32) {
    let limit = max_side.max(1);
    let longer = width.max(height);
    if longer <= limit {
        return (width, height);
    }
    (scale_side(width, limit, longer), scale_side(height, limit, longer))
}

/// The size of a picture scaled so that its SHORTER side is at most `min_side` pixels, keeping
/// its aspect ratio, never scaled up, and with the longer side capped at `MAX_TEXTURE_SIDE`.
/// This is the size of a thumbnail that stays sharp when it is cover-cropped to a square of
/// `min_side`. (4000, 3000, 480) -> (640, 480); (300, 200, 480) -> (300, 200). A `min_side` of
/// 0 counts as 1.
pub fn cover_within(width: u32, height: u32, min_side: u32) -> (u32, u32) {
    let limit = min_side.max(1);
    let shorter = width.min(height);
    let longer = width.max(height);
    // The shorter side decides unless the picture is so elongated that its longer side would
    // still be above the texture limit: limit / shorter <= MAX_TEXTURE_SIDE / longer.
    let shorter_side_decides =
        shorter > limit && u64::from(limit) * u64::from(longer) <= u64::from(MAX_TEXTURE_SIDE) * u64::from(shorter);
    if shorter_side_decides {
        (scale_side(width, limit, shorter), scale_side(height, limit, shorter))
    } else {
        fit_within(width, height, MAX_TEXTURE_SIDE)
    }
}

/// How large the caller wants the picture.
#[derive(Clone, Copy, Debug)]
enum Target {
    /// `fit_within`: the whole picture inside a square of this side.
    Fit(u32),
    /// `cover_within`: the shorter side at most this long.
    Cover(u32),
}

impl Target {
    /// The size to deliver for a picture of `width` x `height` pixels (after orientation).
    fn size(self, width: u32, height: u32) -> (u32, u32) {
        match self {
            Target::Fit(max_side) => fit_within(width, height, max_side),
            Target::Cover(min_side) => cover_within(width, height, min_side),
        }
    }
}

/// True when the orientation turns the picture by a quarter: width and height change places.
fn swaps_sides(orientation: Orientation) -> bool {
    matches!(
        orientation,
        Orientation::Rotate90 | Orientation::Rotate270 | Orientation::Rotate90FlipH | Orientation::Rotate270FlipH
    )
}

/// The size on screen of a picture stored as `width` x `height` with `orientation`.
fn oriented_size(width: u32, height: u32, orientation: Orientation) -> (u32, u32) {
    if swaps_sides(orientation) {
        (height, width)
    } else {
        (width, height)
    }
}

/// A decoder of the `image` crate and the name of its format for messages.
#[derive(Clone, Copy)]
struct Builtin {
    format: ImageFormat,
    name: &'static str,
}

/// The `image` decoder for a sniffed format: the formats of `MediaFormat::has_builtin_decoder`.
fn builtin_decoder(format: MediaFormat) -> Option<Builtin> {
    let (format, name) = match format {
        MediaFormat::Jpeg => (ImageFormat::Jpeg, "JPEG"),
        MediaFormat::Png => (ImageFormat::Png, "PNG"),
        MediaFormat::Gif => (ImageFormat::Gif, "GIF"),
        MediaFormat::Webp => (ImageFormat::WebP, "WebP"),
        MediaFormat::Bmp => (ImageFormat::Bmp, "BMP"),
        MediaFormat::Avif | MediaFormat::Heic | MediaFormat::Mp4 | MediaFormat::Mov | MediaFormat::Webm => return None,
    };
    Some(Builtin { format, name })
}

/// What the first bytes of a file say about it.
struct Source {
    /// The file name for messages.
    file: String,
    /// The `image` decoder to try first, when the format has one.
    builtin: Option<Builtin>,
    /// The file is HEIC or AVIF: a failure of Windows Imaging Component is likely a missing
    /// codec, which the user can install. For anything else that hint would mislead.
    may_need_codec: bool,
}

/// Step 1 of every function: read the head of the file and sniff it. An unreadable file, an
/// empty file and a video end here.
fn inspect(path: &Path) -> Result<Source, DecodeError> {
    let file = display_name(path);
    let head = sniff::read_head(path).map_err(|error| {
        DecodeError::new(DecodeErrorKind::Unreadable, messages::file_unreadable(&file, &error.to_string()))
    })?;
    if head.is_empty() {
        return Err(DecodeError::new(DecodeErrorKind::Damaged, messages::file_empty(&file)));
    }
    let sniffed = sniff::sniff_media(&head);
    if sniffed.is_some_and(|sniffed| sniffed.kind == MediaKind::Video) {
        return Err(DecodeError::new(DecodeErrorKind::Damaged, messages::file_is_a_video(&file)));
    }
    let builtin = sniffed.and_then(|sniffed| builtin_decoder(sniffed.format));
    let may_need_codec = sniffed.is_some_and(|sniffed| matches!(sniffed.format, MediaFormat::Heic | MediaFormat::Avif));
    Ok(Source { file, builtin, may_need_codec })
}

/// A failure of the `image` crate, with what was learnt before it happened.
struct ImageFailure {
    error: DecodeError,
    /// The EXIF orientation, when the header could be read: the second chance with WIC uses it.
    orientation: Option<Orientation>,
}

impl ImageFailure {
    /// Only damaged data is worth a second decoder: an unreadable file stays unreadable and a
    /// picture that is too large stays too large.
    fn deserves_second_chance(&self) -> bool {
        self.error.kind == DecodeErrorKind::Damaged
    }
}

/// Why Windows Imaging Component did not deliver a picture.
struct WicFailure {
    /// The HRESULT; `None` on a platform without WIC.
    code: Option<i32>,
    /// "HRESULT 0x88982F50: The component cannot be found." or another reason.
    text: String,
    /// The size on screen of a picture that is above the limits.
    oversize: Option<(u32, u32)>,
}

/// The sentence and kind for a picture only WIC could have decoded. `may_need_codec` is
/// `Source::may_need_codec`: without it, content WIC has no decoder for is simply not a
/// supported picture (a file that changed into something else after it was opened).
fn wic_error(file: &str, failure: &WicFailure, may_need_codec: bool) -> DecodeError {
    if let Some((width, height)) = failure.oversize {
        return DecodeError::new(DecodeErrorKind::TooLarge, messages::picture_too_large(file, width, height));
    }
    let Some(code) = failure.code else {
        // No Windows Imaging Component on this platform: nothing to install.
        return DecodeError::new(DecodeErrorKind::NoDecoder, messages::picture_not_shown(file, &failure.text));
    };
    // The bit pattern of the HRESULT, which is how the codes are documented.
    let code = code as u32;
    if code >> 16 == HRESULT_FACILITY_WIN32 {
        return DecodeError::new(DecodeErrorKind::Unreadable, messages::file_unreadable(file, &failure.text));
    }
    if HRESULTS_OF_DAMAGE.contains(&code) {
        let reason = format!("the file is damaged or incomplete ({})", detail(&failure.text));
        return DecodeError::new(DecodeErrorKind::Damaged, messages::picture_not_shown(file, &reason));
    }
    if !may_need_codec {
        log::debug!("Windows Imaging Component cannot decode \"{file}\": {}", failure.text);
        return DecodeError::new(DecodeErrorKind::NoDecoder, messages::file_not_supported(file));
    }
    // Everything else is taken for a missing codec: Windows reports "The component cannot be
    // found." without the HEIF Image Extensions, and a Media Foundation error when only the
    // video codec behind them is missing.
    DecodeError::new(DecodeErrorKind::NoDecoder, messages::picture_needs_codec(file, &failure.text))
}

/// The cause a decoder of the `image` crate gave, when it is fit for a sentence the user reads:
/// short, and no dump of the decoder's inner values (some of them print Rust structures such
/// as `ChunkType { type: ... }`). The whole cause goes to the log either way.
fn readable_cause(file: &str, cause: &str) -> Option<String> {
    const MAX_CAUSE_CHARS: usize = 60;
    let cause = detail(cause);
    log::debug!("The decoder's reason for \"{file}\": {cause}");
    let readable = cause.chars().count() <= MAX_CAUSE_CHARS && !cause.contains(['{', '}']);
    readable.then_some(cause)
}

/// The sentence and kind for an error of the `image` crate. `size` is the size on screen when
/// the header had been read.
fn image_error(file: &str, name: &str, error: &ImageError, size: Option<(u32, u32)>) -> DecodeError {
    let damaged = |reason: String| DecodeError::new(DecodeErrorKind::Damaged, messages::picture_not_shown(file, &reason));
    match error {
        ImageError::Limits(_) => {
            let message = match size {
                Some((width, height)) => messages::picture_too_large(file, width, height),
                None => messages::picture_not_shown(file, "it needs more memory than one picture may use"),
            };
            DecodeError::new(DecodeErrorKind::TooLarge, message)
        }
        ImageError::IoError(io) if io.kind() == ErrorKind::UnexpectedEof => {
            damaged(format!("the {name} data ends too early"))
        }
        ImageError::IoError(io) => {
            DecodeError::new(DecodeErrorKind::Unreadable, messages::file_unreadable(file, &io.to_string()))
        }
        ImageError::Decoding(decoding) => {
            damaged(match decoding.source().and_then(|cause| readable_cause(file, &cause.to_string())) {
                Some(cause) => format!("the {name} data is damaged ({cause})"),
                None => format!("the {name} data is damaged"),
            })
        }
        ImageError::Unsupported(unsupported) => damaged(match readable_cause(file, &unsupported.to_string()) {
            Some(cause) => format!("this kind of {name} file is not supported ({cause})"),
            None => format!("this kind of {name} file is not supported"),
        }),
        other => damaged(readable_cause(file, &other.to_string()).unwrap_or_else(|| format!("the {name} data is damaged"))),
    }
}

/// The text of a decoder's error as a detail inside a sentence: on one line (some decoders end
/// their messages with a line break) and without a final period.
fn detail(text: &str) -> String {
    let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
    one_line.trim_end_matches('.').to_owned()
}

/// Runs `work` (calls into the `image` crate) and turns a panic of a decoder into an error: a
/// bad file must never take a worker thread down. `work` stores the orientation as soon as it
/// knows it, so that it survives a failure.
fn guarded<T>(
    file: &str,
    name: &str,
    work: impl FnOnce(&mut Option<Orientation>) -> Result<T, DecodeError>,
) -> Result<T, ImageFailure> {
    let mut orientation = None;
    // The closure only writes `orientation`, which holds a plain value whatever happens.
    match catch_unwind(AssertUnwindSafe(|| work(&mut orientation))) {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(ImageFailure { error, orientation }),
        Err(_) => {
            let reason = format!("the {name} decoder stopped unexpectedly");
            let error = DecodeError::new(DecodeErrorKind::Damaged, messages::picture_not_shown(file, &reason));
            Err(ImageFailure { error, orientation })
        }
    }
}

/// The limits handed to the `image` crate: the pixels of one picture plus some working memory.
fn decoder_limits() -> Limits {
    let mut limits = Limits::default();
    limits.max_alloc = Some(MAX_DECODED_BYTES + DECODER_HEADROOM_BYTES);
    limits
}

/// Opens the file with the decoder of its sniffed format (never the one its extension names)
/// and reads the header.
fn open_decoder(path: &Path, format: ImageFormat) -> ImageResult<impl ImageDecoder> {
    let file = File::open(path).map_err(ImageError::IoError)?;
    let mut reader = ImageReader::with_format(BufReader::new(file), format);
    reader.limits(decoder_limits());
    reader.into_decoder()
}

/// The header of a picture: its decoder, the stored width and height and the EXIF orientation
/// (a missing or unreadable one counts as "upright"). `found` receives the orientation.
fn read_header(
    path: &Path,
    file: &str,
    builtin: Builtin,
    found: &mut Option<Orientation>,
) -> Result<(impl ImageDecoder, u32, u32, Orientation), DecodeError> {
    let mut decoder = open_decoder(path, builtin.format).map_err(|error| image_error(file, builtin.name, &error, None))?;
    let (width, height) = decoder.dimensions();
    let orientation = decoder.orientation().unwrap_or(Orientation::NoTransforms);
    *found = Some(orientation);
    if width == 0 || height == 0 {
        let reason = format!("the {} picture has no pixels", builtin.name);
        return Err(DecodeError::new(DecodeErrorKind::Damaged, messages::picture_not_shown(file, &reason)));
    }
    Ok((decoder, width, height, orientation))
}

/// Decodes with the `image` crate: header, limits, pixels of the first frame, then `finish`.
fn decode_with_image(path: &Path, file: &str, builtin: Builtin, target: Target) -> Result<DecodedImage, ImageFailure> {
    guarded(file, builtin.name, |found| {
        let (decoder, width, height, orientation) = read_header(path, file, builtin, found)?;
        let size = oriented_size(width, height, orientation);
        if u64::from(width) * u64::from(height) > MAX_SOURCE_PIXELS || decoder.total_bytes() > MAX_DECODED_BYTES {
            return Err(DecodeError::new(DecodeErrorKind::TooLarge, messages::picture_too_large(file, size.0, size.1)));
        }
        let image = DynamicImage::from_decoder(decoder)
            .map_err(|error| image_error(file, builtin.name, &error, Some(size)))?;
        Ok(finish(image, orientation, target))
    })
}

/// Reads only the header with the `image` crate: the size on screen.
fn probe_with_image(path: &Path, file: &str, builtin: Builtin) -> Result<(u32, u32), ImageFailure> {
    guarded(file, builtin.name, |found| {
        let (_decoder, width, height, orientation) = read_header(path, file, builtin, found)?;
        Ok(oriented_size(width, height, orientation))
    })
}

/// Scales a freshly decoded picture to its target size, turns it upright and converts it to
/// RGBA. The scaling comes first: turning a small picture is cheaper than turning a large one.
fn finish(image: DynamicImage, orientation: Orientation, target: Target) -> DecodedImage {
    let (source_width, source_height) = oriented_size(image.width(), image.height(), orientation);
    let (width, height) = target.size(source_width, source_height);
    // The size to scale to while the picture still lies as it is stored.
    let (stored_width, stored_height) = oriented_size(width, height, orientation);

    // Eight bits per channel are what the screen gets; three channels are quicker to scale
    // than four when there is no transparency.
    let mut image = if image.color().has_alpha() {
        DynamicImage::ImageRgba8(image.into_rgba8())
    } else {
        DynamicImage::ImageRgb8(image.into_rgb8())
    };
    if (stored_width, stored_height) != (image.width(), image.height()) {
        image = scale(image, stored_width, stored_height);
    }
    image.apply_orientation(orientation);
    let rgba = image.into_rgba8();
    DecodedImage { width: rgba.width(), height: rgba.height(), rgba: rgba.into_raw(), source_width, source_height }
}

/// Scales an 8-bit picture down to `width` x `height`. A picture with transparency is scaled
/// with premultiplied alpha, as browsers do: otherwise the (invisible) colour of transparent
/// pixels would darken or tint the edges next to them.
fn scale(image: DynamicImage, width: u32, height: u32) -> DynamicImage {
    match image {
        DynamicImage::ImageRgb8(buffer) => DynamicImage::ImageRgb8(resample(&buffer, width, height)),
        other => {
            let mut buffer = other.into_rgba8();
            let translucent = premultiply(&mut buffer);
            let mut scaled = resample(&buffer, width, height);
            if translucent {
                unpremultiply(&mut scaled);
            }
            DynamicImage::ImageRgba8(scaled)
        }
    }
}

/// Multiplies the colour of every RGBA pixel by its alpha. Returns false, without touching
/// anything, when every pixel is opaque.
fn premultiply(rgba: &mut [u8]) -> bool {
    if rgba.as_chunks::<4>().0.iter().all(|pixel| pixel[3] == u8::MAX) {
        return false;
    }
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        for channel in &mut pixel[..3] {
            // Rounded division by 255; the result is at most the channel itself.
            *channel = ((u32::from(*channel) * alpha + 127) / 255) as u8;
        }
    }
    true
}

/// The reverse of `premultiply`: divides the colour of every RGBA pixel by its alpha. A colour
/// above its alpha (a filter can overshoot) is capped; a fully transparent pixel becomes
/// transparent black.
fn unpremultiply(rgba: &mut [u8]) {
    for pixel in rgba.as_chunks_mut::<4>().0 {
        let alpha = u32::from(pixel[3]);
        if alpha == u32::from(u8::MAX) {
            continue;
        }
        for channel in &mut pixel[..3] {
            *channel = match alpha {
                0 => 0,
                _ => ((u32::from(*channel) * 255 + alpha / 2) / alpha).min(255) as u8,
            };
        }
    }
}

/// Scales `buffer` down to `width` x `height` with the best filter that is affordable:
///
/// 1. a large reduction: area average down to twice the target, then Catmull-Rom;
/// 2. a small reduction of a very large picture: area average only (see
///    `MAX_RESIZE_SCRATCH_BYTES`);
/// 3. otherwise Catmull-Rom.
fn resample<P>(buffer: &ImageBuffer<P, Vec<u8>>, width: u32, height: u32) -> ImageBuffer<P, Vec<u8>>
where
    P: Pixel<Subpixel = u8> + 'static,
{
    let (source_width, source_height) = buffer.dimensions();
    if source_width / PRESHRINK_MIN_RATIO >= width && source_height / PRESHRINK_MIN_RATIO >= height {
        let smaller = imageops::thumbnail(buffer, width * PRESHRINK_FACTOR, height * PRESHRINK_FACTOR);
        return imageops::resize(&smaller, width, height, FilterType::CatmullRom);
    }
    let scratch_bytes = u64::from(source_width) * u64::from(height) * RESIZE_SCRATCH_BYTES_PER_PIXEL;
    if scratch_bytes > MAX_RESIZE_SCRATCH_BYTES {
        return imageops::thumbnail(buffer, width, height);
    }
    imageops::resize(buffer, width, height, FilterType::CatmullRom)
}

/// True when the picture shows nothing: every pixel is the same (one flat colour, or fully
/// transparent).
fn is_blank(image: &DecodedImage) -> bool {
    let (pixels, _) = image.rgba.as_chunks::<4>();
    pixels.first().is_none_or(|first| pixels.iter().all(|pixel| pixel == first))
}

/// The steps shared by `decode_picture` and `decode_thumbnail`.
fn decode(path: &Path, target: Target) -> Result<DecodedImage, DecodeError> {
    let source = inspect(path)?;
    let mut first_failure = None;
    if let Some(builtin) = source.builtin {
        match decode_with_image(path, &source.file, builtin, target) {
            Ok(image) => return Ok(image),
            Err(failure) if failure.deserves_second_chance() => first_failure = Some(failure),
            Err(failure) => return Err(failure.error),
        }
    }
    let known_orientation = first_failure.as_ref().and_then(|failure| failure.orientation);
    match wic::decode(path, target, known_orientation) {
        Ok(image) => {
            if let Some(failure) = first_failure {
                // A file that is cut off early comes out of WIC as a picture of the right size
                // in one flat colour. That shows nothing, so the reason is shown instead.
                if is_blank(&image) {
                    return Err(failure.error);
                }
                log::debug!("Decoded with Windows Imaging Component after: {}", failure.error);
            }
            Ok(image)
        }
        Err(wic_failure) => Err(match first_failure {
            // The built-in decoder knows the format, so its reason is the more specific one.
            Some(failure) => {
                log::debug!("Windows Imaging Component failed as well: {}", wic_failure.text);
                failure.error
            }
            None => wic_error(&source.file, &wic_failure, source.may_need_codec),
        }),
    }
}

/// Decodes the picture file at `path` for display, scaled down to fit `max_side` x `max_side`
/// (`fit_within`; pass at most `MAX_TEXTURE_SIDE`).
///
/// 1. Sniff the file (`sniff::read_head`, `sniff::sniff_media`). Unreadable -> `Unreadable`
///    (`messages::file_unreadable`, `{file}` = the file name). A zero-byte file -> `Damaged`
///    (`messages::file_empty`); a video -> `Damaged` (`messages::file_is_a_video`).
/// 2. A format with a built-in decoder (`MediaFormat::has_builtin_decoder`): decode with the
///    `image` crate by the SNIFFED format, first frame only, read the EXIF orientation
///    (`ImageDecoder::orientation`), scale with `FilterType::CatmullRom` (premultiplied when
///    the picture has transparency) and apply the orientation
///    (`DynamicImage::apply_orientation`). The header is read first: more than
///    `MAX_SOURCE_PIXELS` -> `TooLarge` without decoding.
/// 3. Anything else (HEIC, AVIF, unknown), and a file the `image` crate found damaged: Windows
///    Imaging Component (frame 0, converted to 32bpp RGBA, scaled with the Fant interpolation
///    mode). WIC is tolerant: of a truncated file it shows the part that is there, as a browser
///    does. When WIC fails too, or delivers nothing but one flat colour, a format
///    with a built-in decoder reports the reason of that decoder as `Damaged`
///    (`messages::picture_not_shown`). Otherwise the error is `Damaged`
///    when WIC says the data is damaged, and else `NoDecoder` (`messages::picture_needs_codec`,
///    which names the extensions to install and carries the HRESULT text).
/// 4. On platforms without WIC step 3 fails with `NoDecoder` and the reason
///    `messages::DECODER_ONLY_ON_WINDOWS`.
///
/// `source_width` / `source_height` are the oriented size before scaling. Never panics on a bad
/// file (a panic inside a decoder of the `image` crate is caught and reported as `Damaged`).
/// Call on a worker thread, never on the UI thread (see the module comment).
pub fn decode_picture(path: &Path, max_side: u32) -> Result<DecodedImage, DecodeError> {
    decode(path, Target::Fit(max_side))
}

/// Decodes a small version of the picture for the grid and the feed: like `decode_picture`, but
/// scaled with `cover_within(source size, min_side)` so that a cover-cropped square of
/// `min_side` pixels is sharp. Call on a worker thread.
pub fn decode_thumbnail(path: &Path, min_side: u32) -> Result<DecodedImage, DecodeError> {
    decode(path, Target::Cover(min_side))
}

/// The size of the picture in pixels, after EXIF orientation, read from the file's header
/// without decoding the pixels: what the layout needs (the feed's aspect ratio) before the
/// picture arrives. Formats with a built-in decoder are read with the `image` crate (the
/// decoder's dimensions plus the orientation); others, and files whose header the `image` crate
/// rejects, through WIC (`GetSize` of frame 0). Errors as for `decode_picture`, except that a
/// picture above `MAX_SOURCE_PIXELS` still reports its size (it is `decode_picture` that
/// refuses it). Cheap, but it reads the file: call on a worker thread.
pub fn probe_dimensions(path: &Path) -> Result<(u32, u32), DecodeError> {
    let source = inspect(path)?;
    let mut first_failure = None;
    if let Some(builtin) = source.builtin {
        match probe_with_image(path, &source.file, builtin) {
            Ok(size) => return Ok(size),
            Err(failure) if failure.deserves_second_chance() => first_failure = Some(failure),
            Err(failure) => return Err(failure.error),
        }
    }
    let known_orientation = first_failure.as_ref().and_then(|failure| failure.orientation);
    wic::probe(path, known_orientation).map_err(|wic_failure| match first_failure {
        Some(failure) => failure.error,
        None => wic_error(&source.file, &wic_failure, source.may_need_codec),
    })
}

/// Decodes through Windows Imaging Component only, scaled to fit `max_side` (`decode_picture`
/// uses the same code as its fallback; tests call this directly). The EXIF orientation of a
/// JPEG or TIFF file is applied; a HEIC or AVIF file arrives upright from its decoder. `Err`
/// holds the reason, such as "HRESULT 0x88982F50: The component cannot be found." when no
/// installed codec knows the data, or `messages::DECODER_ONLY_ON_WINDOWS` on other platforms.
/// Call on a worker thread.
pub fn decode_with_wic(path: &Path, max_side: u32) -> Result<DecodedImage, String> {
    wic::decode(path, Target::Fit(max_side), None).map_err(|failure| match failure.oversize {
        Some((width, height)) => messages::picture_too_large(&display_name(path), width, height),
        None => failure.text,
    })
}

/// Windows Imaging Component: every `unsafe` block of this module.
#[cfg(windows)]
mod wic {
    use std::marker::PhantomData;
    use std::path::Path;

    use image::metadata::Orientation;
    use image::{DynamicImage, RgbaImage};
    use windows::core::{w, Interface, GUID, HSTRING};
    use windows::Win32::Foundation::GENERIC_READ;
    use windows::Win32::Graphics::Imaging::{
        CLSID_WICImagingFactory, GUID_ContainerFormatJpeg, GUID_ContainerFormatTiff, GUID_WICPixelFormat32bppPBGRA,
        GUID_WICPixelFormat32bppRGBA, IWICBitmapDecoder, IWICBitmapFrameDecode, IWICBitmapSource, IWICImagingFactory,
        WICBitmapDitherTypeNone, WICBitmapInterpolationModeFant, WICBitmapPaletteTypeCustom,
        WICDecodeMetadataCacheOnDemand,
    };
    use windows::Win32::System::Com::StructuredStorage::PROPVARIANT;
    use windows::Win32::System::Com::{
        CoCreateInstance, CoInitializeEx, CoUninitialize, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED,
    };
    use windows::Win32::System::Variant::VT_UI2;

    use super::{
        oriented_size, unpremultiply, DecodedImage, Target, WicFailure, HRESULT_BAD_IMAGE, MAX_SOURCE_PIXELS,
    };

    /// COM on the calling thread for the duration of one call.
    struct ComApartment {
        /// True when this guard initialised COM and therefore has to release it.
        entered: bool,
        /// COM belongs to a thread: the guard must be dropped where it was made.
        _on_this_thread: PhantomData<*const ()>,
    }

    impl ComApartment {
        /// Joins the multithreaded apartment. S_OK and S_FALSE (already initialised in this
        /// mode) each count one initialisation that `drop` balances. RPC_E_CHANGED_MODE means
        /// the thread already lives in a single-threaded apartment: COM is usable as it is and
        /// must not be released by this guard.
        fn enter() -> ComApartment {
            // SAFETY: CoInitializeEx may be called on any thread at any time; the reserved
            // pointer is null as required.
            let result = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
            ComApartment { entered: result.is_ok(), _on_this_thread: PhantomData }
        }
    }

    impl Drop for ComApartment {
        fn drop(&mut self) {
            if self.entered {
                // SAFETY: balances the successful CoInitializeEx of `enter` on the same thread
                // (the guard is not Send). Every COM object made under the guard is dropped
                // before it: the guard is the first local of the functions that create it.
                unsafe { CoUninitialize() };
            }
        }
    }

    /// The first frame of a picture file, opened but not decoded.
    struct Frame {
        factory: IWICImagingFactory,
        frame: IWICBitmapFrameDecode,
        /// Stored width in pixels, before `orientation`.
        width: u32,
        /// Stored height in pixels, before `orientation`.
        height: u32,
        orientation: Orientation,
    }

    fn failure(error: windows::core::Error) -> WicFailure {
        let code = error.code().0;
        // The bit pattern of the HRESULT in hexadecimal, as Microsoft documents the codes.
        let text = format!("HRESULT 0x{:08X}: {}", code as u32, error.message());
        WicFailure { code: Some(code), text: text.trim_end_matches([' ', ':']).to_owned(), oversize: None }
    }

    fn damaged(reason: &str) -> WicFailure {
        WicFailure { code: Some(HRESULT_BAD_IMAGE as i32), text: reason.to_owned(), oversize: None }
    }

    fn oversize(width: u32, height: u32) -> WicFailure {
        WicFailure { code: None, text: String::new(), oversize: Some((width, height)) }
    }

    /// The longest path Windows opens without the `\\?\` prefix (MAX_PATH, with its final NUL).
    const MAX_PLAIN_PATH_UNITS: usize = 260;

    /// The path in the form WIC can open. The standard library reaches files beyond the classic
    /// 260-character limit by itself; WIC hands the name to Windows as it is, so a long path is
    /// made absolute and given the `\\?\` prefix (`\\?\UNC\` for a network share) here.
    fn file_name_for_wic(path: &Path) -> HSTRING {
        let short = HSTRING::from(path.as_os_str());
        if short.len() < MAX_PLAIN_PATH_UNITS {
            return short;
        }
        let Ok(absolute) = std::path::absolute(path) else {
            return short;
        };
        let text = absolute.as_os_str().to_string_lossy();
        if text.starts_with(r"\\?\") || text.starts_with(r"\\.\") {
            return short;
        }
        // A lossy conversion would name another file: such a path is handed on as it is.
        if absolute.to_str().is_none() {
            return short;
        }
        let verbatim = match text.strip_prefix(r"\\") {
            Some(share) => format!(r"\\?\UNC\{share}"),
            None => format!(r"\\?\{text}"),
        };
        HSTRING::from(verbatim.replace('/', r"\"))
    }

    /// Opens `path` with whatever installed codec recognises its content and reads the size of
    /// frame 0. `known` is the orientation another decoder already read from the file.
    fn open_frame(path: &Path, known: Option<Orientation>) -> windows::core::Result<Frame> {
        let name = file_name_for_wic(path);
        // SAFETY: plain COM calls on interfaces that were just created on this thread, with
        // COM initialised by the caller's `ComApartment`. `name` outlives the call that reads
        // it and the two size pointers point at live locals.
        unsafe {
            let factory: IWICImagingFactory = CoCreateInstance(&CLSID_WICImagingFactory, None, CLSCTX_INPROC_SERVER)?;
            let decoder = factory.CreateDecoderFromFilename(&name, None, GENERIC_READ, WICDecodeMetadataCacheOnDemand)?;
            let frame = decoder.GetFrame(0)?;
            let (mut width, mut height) = (0_u32, 0_u32);
            frame.GetSize(&mut width, &mut height)?;
            let orientation = known.unwrap_or_else(|| exif_orientation(&decoder, &frame));
            Ok(Frame { factory, frame, width, height, orientation })
        }
    }

    /// The EXIF orientation (tag 274) of a JPEG or TIFF frame; "upright" when there is none or
    /// it cannot be read. Other containers are left alone on purpose: the HEIF decoder turns
    /// HEIC and AVIF pictures itself, so applying their EXIF orientation would turn them twice.
    fn exif_orientation(decoder: &IWICBitmapDecoder, frame: &IWICBitmapFrameDecode) -> Orientation {
        // SAFETY: COM calls on live interfaces. `value` is a zeroed PROPVARIANT (VT_EMPTY) that
        // the query fills; its union is only read as a 16-bit number after the type tag says
        // that it holds one.
        let exif = unsafe {
            let Ok(container) = decoder.GetContainerFormat() else {
                return Orientation::NoTransforms;
            };
            let query = if container == GUID_ContainerFormatJpeg {
                w!("/app1/ifd/{ushort=274}")
            } else if container == GUID_ContainerFormatTiff {
                w!("/ifd/{ushort=274}")
            } else {
                return Orientation::NoTransforms;
            };
            let Ok(reader) = frame.GetMetadataQueryReader() else {
                return Orientation::NoTransforms;
            };
            let mut value = PROPVARIANT::default();
            if reader.GetMetadataByName(query, &mut value).is_err() {
                return Orientation::NoTransforms;
            }
            let tagged = &value.Anonymous.Anonymous;
            if tagged.vt != VT_UI2 {
                return Orientation::NoTransforms;
            }
            tagged.Anonymous.uiVal
        };
        u8::try_from(exif).ok().and_then(Orientation::from_exif).unwrap_or(Orientation::NoTransforms)
    }

    /// `pixels` in another pixel format.
    fn convert(source: &Frame, pixels: &IWICBitmapSource, format: &GUID) -> windows::core::Result<IWICBitmapSource> {
        // SAFETY: COM calls on live interfaces of this thread; `format` points at a constant
        // of the windows crate.
        unsafe {
            let converter = source.factory.CreateFormatConverter()?;
            converter.Initialize(pixels, format, WICBitmapDitherTypeNone, None, 0.0, WICBitmapPaletteTypeCustom)?;
            converter.cast()
        }
    }

    /// Converts the frame to RGBA, scaled to its target size, and turns it upright.
    fn read_pixels(source: &Frame, target: Target) -> Result<DecodedImage, WicFailure> {
        if source.width == 0 || source.height == 0 {
            return Err(damaged("the picture has no pixels"));
        }
        let (source_width, source_height) = oriented_size(source.width, source.height, source.orientation);
        if u64::from(source.width) * u64::from(source.height) > MAX_SOURCE_PIXELS {
            return Err(oversize(source_width, source_height));
        }
        let (width, height) = target.size(source_width, source_height);
        // The size to scale to while the picture still lies as it is stored.
        let (stored_width, stored_height) = oriented_size(width, height, source.orientation);
        let scaled = (stored_width, stored_height) != (source.width, source.height);

        // At most MAX_SOURCE_PIXELS * 4 bytes, which fits the 32-bit buffer size of CopyPixels.
        let byte_count = stored_width as usize * stored_height as usize * 4;
        let mut rgba: Vec<u8> = Vec::new();
        if rgba.try_reserve_exact(byte_count).is_err() {
            return Err(oversize(source_width, source_height));
        }
        rgba.resize(byte_count, 0);

        // SAFETY: COM calls on live interfaces of this thread. CopyPixels writes `stored_height`
        // rows of `stored_width * 4` bytes, which is exactly the length of `rgba`; a null
        // rectangle means the whole picture.
        let copied = unsafe {
            (|| -> windows::core::Result<()> {
                let pixels = if scaled {
                    // Scaling mixes neighbouring pixels, which is only right with premultiplied
                    // alpha. The scaler may answer in another pixel format than it was given
                    // (it turned premultiplied RGBA into BGRA on the machine this was written
                    // on), so its result is converted once more to a format that is certain.
                    let premultiplied = convert(source, &source.frame.cast()?, &GUID_WICPixelFormat32bppPBGRA)?;
                    let scaler = source.factory.CreateBitmapScaler()?;
                    scaler.Initialize(&premultiplied, stored_width, stored_height, WICBitmapInterpolationModeFant)?;
                    convert(source, &scaler.cast()?, &GUID_WICPixelFormat32bppPBGRA)?
                } else {
                    convert(source, &source.frame.cast()?, &GUID_WICPixelFormat32bppRGBA)?
                };
                pixels.CopyPixels(std::ptr::null(), stored_width * 4, &mut rgba)
            })()
        };
        copied.map_err(failure)?;
        if scaled {
            // Premultiplied BGRA to straight RGBA.
            for pixel in rgba.as_chunks_mut::<4>().0 {
                pixel.swap(0, 2);
            }
            unpremultiply(&mut rgba);
        }

        if source.orientation != Orientation::NoTransforms {
            let Some(buffer) = RgbaImage::from_raw(stored_width, stored_height, rgba) else {
                return Err(damaged("the picture has an unexpected size"));
            };
            let mut image = DynamicImage::ImageRgba8(buffer);
            image.apply_orientation(source.orientation);
            rgba = image.into_rgba8().into_raw();
        }
        Ok(DecodedImage { width, height, rgba, source_width, source_height })
    }

    /// Decodes frame 0 of `path`, scaled for `target`.
    pub(super) fn decode(path: &Path, target: Target, known: Option<Orientation>) -> Result<DecodedImage, WicFailure> {
        let _apartment = ComApartment::enter();
        let source = open_frame(path, known).map_err(failure)?;
        read_pixels(&source, target)
    }

    /// The size on screen of frame 0 of `path`, without decoding it.
    pub(super) fn probe(path: &Path, known: Option<Orientation>) -> Result<(u32, u32), WicFailure> {
        let _apartment = ComApartment::enter();
        let source = open_frame(path, known).map_err(failure)?;
        if source.width == 0 || source.height == 0 {
            return Err(damaged("the picture has no pixels"));
        }
        Ok(oriented_size(source.width, source.height, source.orientation))
    }
}

/// Other platforms have no Windows Imaging Component: both functions report that.
#[cfg(not(windows))]
mod wic {
    use std::path::Path;

    use image::metadata::Orientation;

    use super::{DecodedImage, Target, WicFailure};
    use crate::messages;

    fn unavailable() -> WicFailure {
        WicFailure { code: None, text: messages::DECODER_ONLY_ON_WINDOWS.to_owned(), oversize: None }
    }

    pub(super) fn decode(_path: &Path, _target: Target, _known: Option<Orientation>) -> Result<DecodedImage, WicFailure> {
        Err(unavailable())
    }

    pub(super) fn probe(_path: &Path, _known: Option<Orientation>) -> Result<(u32, u32), WicFailure> {
        Err(unavailable())
    }
}

#[cfg(test)]
mod tests {
    //! The arithmetic and the wording; decoding real files is tested in core/tests/decode.rs.

    use super::*;
    use image::{GrayImage, Luma, Rgb, RgbImage, Rgba, RgbaImage};

    #[test]
    fn fit_within_keeps_the_aspect_ratio_and_never_scales_up() {
        assert_eq!(fit_within(4000, 3000, 2000), (2000, 1500));
        assert_eq!(fit_within(3000, 4000, 2000), (1500, 2000));
        assert_eq!(fit_within(100, 50, 2000), (100, 50));
        assert_eq!(fit_within(2000, 2000, 2000), (2000, 2000));
        assert_eq!(fit_within(2001, 2001, 2000), (2000, 2000));
        // A very thin picture keeps at least one pixel.
        assert_eq!(fit_within(8000, 10, 100), (100, 1));
        assert_eq!(fit_within(10, 8000, 100), (1, 100));
        // Halves round up: 9000 x 60 at 8192 is 8192 x 54.6.
        assert_eq!(fit_within(9000, 60, 8192), (8192, 55));
        assert_eq!(fit_within(3, 2, 2), (2, 1));
    }

    #[test]
    fn fit_within_survives_degenerate_input() {
        assert_eq!(fit_within(0, 0, 100), (0, 0));
        assert_eq!(fit_within(50, 40, 0), (1, 1));
        assert_eq!(fit_within(u32::MAX, u32::MAX, 8192), (8192, 8192));
        assert_eq!(fit_within(u32::MAX, 1, 8192), (8192, 1));
    }

    #[test]
    fn cover_within_limits_the_shorter_side() {
        assert_eq!(cover_within(4000, 3000, 480), (640, 480));
        assert_eq!(cover_within(3000, 4000, 480), (480, 640));
        assert_eq!(cover_within(300, 200, 480), (300, 200));
        assert_eq!(cover_within(720, 720, 480), (480, 480));
        assert_eq!(cover_within(480, 2000, 480), (480, 2000));
        assert_eq!(cover_within(481, 481, 480), (480, 480));
    }

    #[test]
    fn cover_within_caps_the_longer_side_at_the_texture_limit() {
        // A panorama: 480 px high would be 48,000 px wide.
        assert_eq!(cover_within(100_000, 1000, 480), (8192, 82));
        // Already short enough, but too long for a texture.
        assert_eq!(cover_within(20_000, 100, 480), (8192, 41));
        // Exactly at the limit the shorter side still decides.
        assert_eq!(cover_within(16_384, 960, 480), (8192, 480));
        assert_eq!(cover_within(0, 0, 480), (0, 0));
        // A side of 0 counts as 1.
        assert_eq!(cover_within(50, 40, 0), (1, 1));
        assert_eq!(cover_within(50, 40, 1), (1, 1));
    }

    #[test]
    fn quarter_turns_swap_width_and_height() {
        let upright = [
            Orientation::NoTransforms,
            Orientation::Rotate180,
            Orientation::FlipHorizontal,
            Orientation::FlipVertical,
        ];
        let turned =
            [Orientation::Rotate90, Orientation::Rotate270, Orientation::Rotate90FlipH, Orientation::Rotate270FlipH];
        for orientation in upright {
            assert_eq!(oriented_size(60, 40, orientation), (60, 40), "{orientation:?}");
        }
        for orientation in turned {
            assert_eq!(oriented_size(60, 40, orientation), (40, 60), "{orientation:?}");
        }
    }

    #[test]
    fn targets_use_their_rule() {
        assert_eq!(Target::Fit(2000).size(4000, 3000), (2000, 1500));
        assert_eq!(Target::Cover(480).size(4000, 3000), (640, 480));
    }

    #[test]
    fn premultiplying_leaves_opaque_pictures_alone() {
        let mut opaque = vec![10, 20, 30, 255, 40, 50, 60, 255];
        assert!(!premultiply(&mut opaque));
        assert_eq!(opaque, [10, 20, 30, 255, 40, 50, 60, 255]);
    }

    #[test]
    fn premultiplying_and_back_keeps_visible_colours() {
        let mut pixels = vec![255, 128, 0, 255, 255, 128, 0, 128, 200, 100, 50, 0, 90, 60, 30, 1];
        assert!(premultiply(&mut pixels));
        assert_eq!(pixels, [255, 128, 0, 255, 128, 64, 0, 128, 0, 0, 0, 0, 0, 0, 0, 1]);
        unpremultiply(&mut pixels);
        // Half transparent: the colour comes back within rounding. Fully transparent: black.
        assert_eq!(&pixels[..8], [255, 128, 0, 255, 255, 128, 0, 128]);
        assert_eq!(&pixels[8..12], [0, 0, 0, 0]);
        assert_eq!(pixels[15], 1);
    }

    #[test]
    fn unpremultiplying_caps_a_colour_above_its_alpha() {
        let mut pixels = vec![200, 100, 50, 100];
        unpremultiply(&mut pixels);
        assert_eq!(pixels, [255, 255, 128, 100]);
    }

    #[test]
    fn scaling_a_transparent_picture_does_not_darken_its_edges() {
        // Left: transparent black. Right: opaque white. Straight-alpha filtering would give
        // grey, half transparent pixels at the border.
        let picture = RgbaImage::from_fn(100, 60, |x, _| if x < 37 { Rgba([0, 0, 0, 0]) } else { Rgba([255, 255, 255, 255]) });
        let scaled = scale(DynamicImage::ImageRgba8(picture), 30, 18).into_rgba8();
        assert_eq!(scaled.dimensions(), (30, 18));
        let mut translucent = 0;
        for pixel in scaled.pixels() {
            let Rgba([red, green, blue, alpha]) = *pixel;
            if alpha > 0 {
                assert!(red >= 250 && green >= 250 && blue >= 250, "a visible pixel is {pixel:?}");
            }
            if alpha > 0 && alpha < 255 {
                translucent += 1;
            }
        }
        assert!(translucent > 0, "the border should have half transparent pixels");
        assert_eq!(scaled.get_pixel(0, 9)[3], 0);
        assert_eq!(*scaled.get_pixel(29, 9), Rgba([255, 255, 255, 255]));
    }

    #[test]
    fn scaling_keeps_the_colours_of_an_opaque_picture() {
        let picture = RgbImage::from_fn(400, 300, |x, _| if x < 200 { Rgb([200, 30, 30]) } else { Rgb([30, 30, 200]) });
        // A large reduction (area average first) and a small one (Catmull-Rom only).
        for (width, height) in [(40, 30), (300, 225)] {
            let scaled = scale(DynamicImage::ImageRgb8(picture.clone()), width, height).into_rgb8();
            assert_eq!(scaled.dimensions(), (width, height));
            assert_eq!(*scaled.get_pixel(width / 4, height / 2), Rgb([200, 30, 30]));
            assert_eq!(*scaled.get_pixel(width * 3 / 4, height / 2), Rgb([30, 30, 200]));
        }
    }

    #[test]
    fn a_very_large_picture_is_averaged_without_the_float_copy() {
        // 9000 x 2222 x 16 bytes would be 320 MiB of intermediate floats: the area average is
        // used. One channel keeps the test small; the rule does not depend on the channels.
        let picture = GrayImage::from_fn(9000, 4000, |x, _| Luma([if x < 4500 { 40 } else { 220 }]));
        let scratch = 9000 * 2222 * RESIZE_SCRATCH_BYTES_PER_PIXEL;
        assert!(scratch > MAX_RESIZE_SCRATCH_BYTES);
        let scaled = resample(&picture, 5000, 2222);
        assert_eq!(scaled.dimensions(), (5000, 2222));
        assert_eq!(*scaled.get_pixel(100, 1000), Luma([40]));
        assert_eq!(*scaled.get_pixel(4900, 1000), Luma([220]));
    }

    #[test]
    fn finish_scales_turns_and_reports_the_size_on_screen() {
        // 60 x 40, the left half red and the right half blue, stored turned (EXIF 6: to be
        // shown rotated a quarter clockwise): red ends up on top.
        let stored = RgbImage::from_fn(60, 40, |x, _| if x < 30 { Rgb([255, 0, 0]) } else { Rgb([0, 0, 255]) });
        let shown = finish(DynamicImage::ImageRgb8(stored.clone()), Orientation::Rotate90, Target::Fit(30));
        assert_eq!((shown.width, shown.height), (20, 30));
        assert_eq!((shown.source_width, shown.source_height), (40, 60));
        assert_eq!(shown.rgba.len(), 20 * 30 * 4);
        let pixel = |x: usize, y: usize| &shown.rgba[(y * 20 + x) * 4..(y * 20 + x) * 4 + 4];
        assert_eq!(pixel(10, 3), [255, 0, 0, 255]);
        assert_eq!(pixel(10, 26), [0, 0, 255, 255]);

        // Not scaled at all when it fits.
        let whole = finish(DynamicImage::ImageRgb8(stored), Orientation::NoTransforms, Target::Fit(8192));
        assert_eq!((whole.width, whole.height, whole.source_width, whole.source_height), (60, 40, 60, 40));
    }

    #[test]
    fn finish_reduces_sixteen_bit_and_grey_pictures_to_rgba() {
        let grey = image::ImageBuffer::<Luma<u16>, Vec<u16>>::from_pixel(8, 4, Luma([0x8080]));
        let shown = finish(DynamicImage::ImageLuma16(grey), Orientation::NoTransforms, Target::Fit(8192));
        assert_eq!((shown.width, shown.height), (8, 4));
        assert_eq!(&shown.rgba[..4], [128, 128, 128, 255]);
    }

    #[test]
    fn only_the_sniffed_picture_formats_have_a_builtin_decoder() {
        for format in [MediaFormat::Jpeg, MediaFormat::Png, MediaFormat::Gif, MediaFormat::Webp, MediaFormat::Bmp] {
            assert!(builtin_decoder(format).is_some(), "{format:?}");
            assert!(format.has_builtin_decoder(), "{format:?}");
        }
        for format in [MediaFormat::Avif, MediaFormat::Heic, MediaFormat::Mp4, MediaFormat::Mov, MediaFormat::Webm] {
            assert!(builtin_decoder(format).is_none(), "{format:?}");
            assert!(!format.has_builtin_decoder(), "{format:?}");
        }
    }

    fn wic_failure(code: u32, text: &str) -> WicFailure {
        WicFailure { code: Some(code as i32), text: text.to_owned(), oversize: None }
    }

    #[test]
    fn a_missing_codec_names_the_extensions_to_install() {
        let failure = wic_failure(0x8898_2F50, "HRESULT 0x88982F50: The component cannot be found.");
        let error = wic_error("holiday.heic", &failure, true);
        assert_eq!(error.kind, DecodeErrorKind::NoDecoder);
        assert_eq!(
            error.message,
            "\"holiday.heic\" could not be shown: Windows has no decoder for it (HRESULT 0x88982F50: The component cannot be found). HEIC and AVIF pictures need the HEIF Image Extensions from the Microsoft Store, together with the HEVC Video Extensions (HEIC) or the AV1 Video Extension (AVIF)."
        );
        // An error nobody has classified is taken for a missing codec as well.
        assert_eq!(wic_error("a.avif", &wic_failure(0xC00D_5212, "HRESULT 0xC00D5212"), true).kind, DecodeErrorKind::NoDecoder);
        // Content that is neither HEIC nor AVIF (a picture that became a text file after it was
        // opened) needs no codec from the Store: it is simply not a picture.
        let error = wic_error("d.jpg", &failure, false);
        assert_eq!(error.kind, DecodeErrorKind::NoDecoder);
        assert_eq!(error.message, messages::file_not_supported("d.jpg"));
        assert!(!error.message.contains("Microsoft Store"));
    }

    #[test]
    fn a_text_file_that_was_a_picture_is_not_blamed_on_a_missing_codec() {
        let folder = std::env::temp_dir().join(format!("instagram-core-decode-text-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("d.jpg");
        std::fs::write(&path, b"now text").unwrap();
        let error = decode_picture(&path, 256).expect_err("text is no picture");
        let _ = std::fs::remove_dir_all(&folder);
        assert!(!error.message.contains("Microsoft Store"), "{}", error.message);
        assert!(error.message.starts_with("\"d.jpg\" "), "{}", error.message);
    }

    #[test]
    fn damaged_data_is_not_blamed_on_a_missing_codec() {
        let text = "HRESULT 0xC00D36BE: The file does not conform to the relevant file format specification.";
        let error = wic_error("cut.heic", &wic_failure(0xC00D_36BE, text), true);
        assert_eq!(error.kind, DecodeErrorKind::Damaged);
        assert_eq!(
            error.message,
            "\"cut.heic\" could not be shown: the file is damaged or incomplete (HRESULT 0xC00D36BE: The file does not conform to the relevant file format specification)."
        );
        for code in HRESULTS_OF_DAMAGE {
            assert_eq!(wic_error("x", &wic_failure(code, "HRESULT"), true).kind, DecodeErrorKind::Damaged, "{code:08X}");
        }
    }

    #[test]
    fn a_file_system_error_of_wic_means_unreadable() {
        let text = "HRESULT 0x80070020: The process cannot access the file because it is being used by another process.";
        let error = wic_error("busy.heic", &wic_failure(0x8007_0020, text), true);
        assert_eq!(error.kind, DecodeErrorKind::Unreadable);
        assert_eq!(error.message, format!("\"busy.heic\" could not be read: {text}"));
    }

    #[test]
    fn oversize_and_missing_platform_have_their_own_sentences() {
        let too_large = WicFailure { code: None, text: String::new(), oversize: Some((30_000, 20_000)) };
        let error = wic_error("huge.heic", &too_large, true);
        assert_eq!(error.kind, DecodeErrorKind::TooLarge);
        assert_eq!(error.message, "\"huge.heic\" is too large to show (30,000 x 20,000 pixels).");

        let elsewhere = WicFailure { code: None, text: messages::DECODER_ONLY_ON_WINDOWS.to_owned(), oversize: None };
        let error = wic_error("a.heic", &elsewhere, true);
        assert_eq!(error.kind, DecodeErrorKind::NoDecoder);
        assert_eq!(error.message, "\"a.heic\" could not be shown: this format can only be decoded on Windows.");
    }

    #[test]
    fn image_crate_errors_become_sentences() {
        let eof = ImageError::IoError(std::io::Error::new(ErrorKind::UnexpectedEof, "eof"));
        let error = image_error("cut.png", "PNG", &eof, None);
        assert_eq!(error.kind, DecodeErrorKind::Damaged);
        assert_eq!(error.message, "\"cut.png\" could not be shown: the PNG data ends too early.");

        let denied = ImageError::IoError(std::io::Error::new(ErrorKind::PermissionDenied, "Access is denied."));
        let error = image_error("locked.png", "PNG", &denied, None);
        assert_eq!(error.kind, DecodeErrorKind::Unreadable);
        assert_eq!(error.message, "\"locked.png\" could not be read: Access is denied.");

        let limits = ImageError::Limits(image::error::LimitError::from_kind(image::error::LimitErrorKind::InsufficientMemory));
        let error = image_error("big.png", "PNG", &limits, Some((20_000, 10_000)));
        assert_eq!(error.kind, DecodeErrorKind::TooLarge);
        assert_eq!(error.message, "\"big.png\" is too large to show (20,000 x 10,000 pixels).");

        // zune-jpeg ends some of its messages with a line break.
        let broken = ImageError::Decoding(image::error::DecodingError::new(
            image::error::ImageFormatHint::Exact(ImageFormat::Jpeg),
            "invalid\n marker.\n",
        ));
        let error = image_error("odd.jpg", "JPEG", &broken, None);
        assert_eq!(error.kind, DecodeErrorKind::Damaged);
        assert_eq!(error.message, "\"odd.jpg\" could not be shown: the JPEG data is damaged (invalid marker).");

        // The png crate prints its structures; the user gets the plain sentence.
        let dump = ImageError::Decoding(image::error::DecodingError::new(
            image::error::ImageFormatHint::Exact(ImageFormat::Png),
            "Unrecognized critical chunk: ChunkType { type: \\0\\0\\0\\0, critical: true, private: false, reserved: false, safecopy: false }",
        ));
        let error = image_error("b damaged picture.png", "PNG", &dump, None);
        assert_eq!(error.message, "\"b damaged picture.png\" could not be shown: the PNG data is damaged.");
        let long = ImageError::Decoding(image::error::DecodingError::new(
            image::error::ImageFormatHint::Exact(ImageFormat::Png),
            "a reason that goes on and on and on and on and on and on and on and on",
        ));
        assert_eq!(image_error("long.png", "PNG", &long, None).message, "\"long.png\" could not be shown: the PNG data is damaged.");
    }

    #[test]
    fn a_damaged_png_is_explained_in_one_plain_sentence() {
        // The signature and the header chunk of a real PNG (33 bytes), then zeros where the next
        // chunk should be: the png crate then reports a chunk type of four zero bytes.
        let mut png = Vec::new();
        image::RgbaImage::new(4, 4).write_to(&mut std::io::Cursor::new(&mut png), ImageFormat::Png).unwrap();
        let mut bytes = png[..33].to_vec();
        bytes.extend(std::iter::repeat_n(0u8, 2000));
        let folder = std::env::temp_dir().join(format!("instagram-core-decode-png-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("b damaged picture.png");
        std::fs::write(&path, &bytes).unwrap();
        let error = decode_picture(&path, 256).expect_err("the chunks are damaged");
        let _ = std::fs::remove_dir_all(&folder);
        assert!(!error.message.contains(['{', '}']), "{}", error.message);
        assert!(error.message.starts_with("\"b damaged picture.png\" could not be shown: "), "{}", error.message);
    }

    #[test]
    fn a_panicking_decoder_becomes_an_error() {
        // The panic message itself goes to the captured output of this test.
        let outcome = guarded::<()>("odd.gif", "GIF", |found| {
            *found = Some(Orientation::Rotate90);
            panic!("index out of bounds");
        });
        let failure = outcome.expect_err("a failure");
        assert_eq!(failure.error.kind, DecodeErrorKind::Damaged);
        assert_eq!(failure.error.message, "\"odd.gif\" could not be shown: the GIF decoder stopped unexpectedly.");
        assert_eq!(failure.orientation, Some(Orientation::Rotate90));
        assert!(failure.deserves_second_chance());
    }

    #[test]
    fn a_picture_in_one_flat_colour_counts_as_blank() {
        let picture = |rgba: Vec<u8>| DecodedImage { width: 2, height: 1, rgba, source_width: 2, source_height: 1 };
        assert!(is_blank(&picture(vec![0; 8])));
        assert!(is_blank(&picture(vec![9, 9, 9, 255, 9, 9, 9, 255])));
        assert!(!is_blank(&picture(vec![9, 9, 9, 255, 9, 9, 10, 255])));
        assert!(!is_blank(&picture(vec![0, 0, 0, 0, 0, 0, 0, 1])));
    }

    #[test]
    fn the_debug_print_leaves_the_pixels_out() {
        let picture = DecodedImage { width: 2, height: 1, rgba: vec![0; 8], source_width: 4, source_height: 2 };
        assert_eq!(
            format!("{picture:?}"),
            "DecodedImage { width: 2, height: 1, rgba_bytes: 8, source_width: 4, source_height: 2 }"
        );
        let error = DecodeError::new(DecodeErrorKind::Damaged, "\"a.jpg\" is an empty file.");
        assert_eq!(error.to_string(), "\"a.jpg\" is an empty file.");
    }

    #[test]
    fn names_fall_back_to_the_whole_path() {
        assert_eq!(display_name(Path::new("C:/pictures/IMG_2.jpg")), "IMG_2.jpg");
        assert_eq!(display_name(Path::new("..")), "..");
    }
}
