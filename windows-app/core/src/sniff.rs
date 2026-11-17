//! What a file is, decided from its first bytes: a byte-for-byte port of the clone's sniffer
//! (frontend/src/import/sniff.ts; spec-export B).
//!
//! File extensions are never trusted. A picture is supported when its leading bytes say so, and
//! the extension a file gets inside an export comes from here, never from its name. The same
//! rules decide which loose files are pictures at all.
//!
//! ```text
//! jpeg  FF D8 FF                                  png   89 50 4E 47 0D 0A 1A 0A
//! gif   "GIF87a" / "GIF89a"                       webp  "RIFF" .... "WEBP"
//! avif, heic, mp4, mov   an ISO "ftyp" box, by its major and compatible brands
//! bmp   "BM" + header fields (only without an ftyp box)
//! webm  an EBML header whose DocType is exactly "webm"
//! ```

use std::fs::File;
use std::io::{self, Read};
use std::path::Path;

use crate::messages;
use crate::model::MediaKind;

/// How many leading bytes of a file are read for sniffing (`min(file size, 256)`).
pub const SNIFF_BYTES: usize = 256;

/// The formats the clone imports and exports.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MediaFormat {
    /// A JPEG picture.
    Jpeg,
    /// A PNG picture.
    Png,
    /// A GIF picture, still or animated (the app shows the first frame).
    Gif,
    /// A WebP picture.
    Webp,
    /// An AVIF picture or picture sequence.
    Avif,
    /// HEIC and generic HEIF (`mif1` / `msf1`): both are exported with the extension "heic".
    Heic,
    /// A Windows bitmap.
    Bmp,
    /// An MP4-family video with any brand other than QuickTime's.
    Mp4,
    /// A QuickTime video (major brand "qt  ").
    Mov,
    /// A WebM video.
    Webm,
}

impl MediaFormat {
    /// The extension of the file inside an export, without the dot (export.ts `EXTENSIONS`):
    /// jpeg -> "jpg", every other format its own name.
    pub fn extension(self) -> &'static str {
        match self {
            MediaFormat::Jpeg => "jpg",
            MediaFormat::Png => "png",
            MediaFormat::Gif => "gif",
            MediaFormat::Webp => "webp",
            MediaFormat::Avif => "avif",
            MediaFormat::Heic => "heic",
            MediaFormat::Bmp => "bmp",
            MediaFormat::Mp4 => "mp4",
            MediaFormat::Mov => "mov",
            MediaFormat::Webm => "webm",
        }
    }

    /// Picture or video.
    pub fn kind(self) -> MediaKind {
        match self {
            MediaFormat::Mp4 | MediaFormat::Mov | MediaFormat::Webm => MediaKind::Video,
            _ => MediaKind::Image,
        }
    }

    /// True for the formats the `image` crate decodes in this build (JPEG, PNG, GIF, WebP, BMP);
    /// AVIF and HEIC go through Windows Imaging Component (see `decode`).
    pub fn has_builtin_decoder(self) -> bool {
        matches!(
            self,
            MediaFormat::Jpeg | MediaFormat::Png | MediaFormat::Gif | MediaFormat::Webp | MediaFormat::Bmp
        )
    }
}

/// The result of sniffing a supported file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Sniffed {
    /// What the first bytes say the file is.
    pub format: MediaFormat,
    /// `format.kind()`, kept beside it because most callers only ask for the kind.
    pub kind: MediaKind,
    /// The MIME type the clone reports: "image/jpeg", "image/png", "image/gif", "image/webp",
    /// "image/avif", "image/heic" ("image/heif" when only the generic brands `mif1` / `msf1`
    /// identify the file), "image/bmp", "video/mp4", "video/quicktime", "video/webm".
    pub mime: &'static str,
}

impl Sniffed {
    fn new(format: MediaFormat, mime: &'static str) -> Sniffed {
        Sniffed { format, kind: format.kind(), mime }
    }
}

/// One four-character brand of an ftyp box, as raw bytes.
type Brand = [u8; 4];

/// HEVC-coded HEIF pictures and picture sequences.
const HEVC_BRANDS: [&Brand; 8] = [b"heic", b"heix", b"hevc", b"hevx", b"heim", b"heis", b"hevm", b"hevs"];
/// HEIF without a statement about the codec: decided by the compatible brands.
const HEIF_GENERIC_BRANDS: [&Brand; 2] = [b"mif1", b"msf1"];
const AVIF_BRANDS: [&Brand; 2] = [b"avif", b"avis"];
/// ISO-BMFF brands of audio-only files (iTunes audio, audio books, Flash audio).
const AUDIO_BRANDS: [&Brand; 5] = [b"M4A ", b"M4B ", b"M4P ", b"F4A ", b"F4B "];

const JPEG_MAGIC: [u8; 3] = [0xFF, 0xD8, 0xFF];
const PNG_MAGIC: [u8; 8] = [0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A];
const EBML_MAGIC: [u8; 4] = [0x1A, 0x45, 0xDF, 0xA3];
/// The id of the DocType element of an EBML header, with its length marker bit.
const EBML_DOC_TYPE_ID: u32 = 0x4282;
/// A DocType longer than this is cut: no real one is.
const MAX_DOC_TYPE_BYTES: u64 = 32;
/// DIB header sizes of the BMP variants in use (OS/2 1.x, Windows 3.x, V2, V3, OS/2 2.x, V4, V5).
const BMP_DIB_HEADER_SIZES: [u32; 7] = [12, 40, 52, 56, 64, 108, 124];
/// An MPEG transport stream repeats its sync byte 0x47 every this many bytes.
const TS_PACKET_BYTES: usize = 188;

/// The part of a buffer the sniffer looks at. The clone only ever sees the first `SNIFF_BYTES`
/// of a file, so a longer buffer is cut to give the same answer as the clone would.
fn head(bytes: &[u8]) -> &[u8] {
    &bytes[..bytes.len().min(SNIFF_BYTES)]
}

/// True when `bytes` holds `text` at `offset` (the clone's `ascii(bytes, offset, n) === text`).
fn has_at(bytes: &[u8], offset: usize, text: &[u8]) -> bool {
    bytes.get(offset..offset + text.len()) == Some(text)
}

/// The `ftyp` box an ISO-BMFF file (MP4, MOV, HEIF, AVIF) starts with.
struct FileTypeBox {
    major: Brand,
    compatible: Vec<Brand>,
}

fn brand_at(bytes: &[u8], offset: usize) -> Brand {
    [bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]]
}

/// sniff.ts `fileTypeBox`: `None` unless there are 12 bytes with "ftyp" at offset 4. Bytes 12..16
/// are the minor version and are ignored; compatible brands follow up to the end of the box.
fn file_type_box(bytes: &[u8]) -> Option<FileTypeBox> {
    if bytes.len() < 12 || !has_at(bytes, 4, b"ftyp") {
        return None;
    }
    let declared = u32::from_be_bytes(brand_at(bytes, 0)) as usize;
    // A declared size below 16 cannot hold a brand list: such a box is read to the end of the
    // buffer instead.
    let box_end = bytes.len().min(if declared >= 16 { declared } else { bytes.len() });
    let mut compatible = Vec::new();
    let mut offset = 16;
    while offset + 4 <= box_end {
        compatible.push(brand_at(bytes, offset));
        offset += 4;
    }
    Some(FileTypeBox { major: brand_at(bytes, 8), compatible })
}

fn in_set(set: &[&Brand], brand: &Brand) -> bool {
    set.contains(&brand)
}

/// sniff.ts `imageBrand`: the picture format an ftyp box stands for, if any. The order matters:
/// a generic HEIF major brand (`mif1`) with `avif` among the compatible brands is AVIF.
fn image_brand(file_type: &FileTypeBox) -> Option<Sniffed> {
    let avif = Sniffed::new(MediaFormat::Avif, "image/avif");
    let heic = Sniffed::new(MediaFormat::Heic, "image/heic");
    if in_set(&AVIF_BRANDS, &file_type.major) {
        return Some(avif);
    }
    if in_set(&HEVC_BRANDS, &file_type.major) {
        return Some(heic);
    }
    // Generic HEIF majors (mif1/msf1) and unknown majors are decided by the compatible brands.
    if file_type.compatible.iter().any(|brand| in_set(&AVIF_BRANDS, brand)) {
        return Some(avif);
    }
    if file_type.compatible.iter().any(|brand| in_set(&HEVC_BRANDS, brand)) {
        return Some(heic);
    }
    if in_set(&HEIF_GENERIC_BRANDS, &file_type.major) {
        return Some(Sniffed::new(MediaFormat::Heic, "image/heif"));
    }
    None
}

/// A brand is four printable ASCII characters; anything else is not an ISO-BMFF file.
fn is_brand(brand: &Brand) -> bool {
    brand.iter().all(|byte| (0x20..=0x7E).contains(byte))
}

/// sniff.ts `sniffImage`: rules 1 to 6 of `sniff_media`.
fn sniff_image(bytes: &[u8]) -> Option<Sniffed> {
    if bytes.starts_with(&JPEG_MAGIC) {
        return Some(Sniffed::new(MediaFormat::Jpeg, "image/jpeg"));
    }
    if bytes.starts_with(&PNG_MAGIC) {
        return Some(Sniffed::new(MediaFormat::Png, "image/png"));
    }
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        return Some(Sniffed::new(MediaFormat::Gif, "image/gif"));
    }
    if bytes.len() >= 12 && has_at(bytes, 0, b"RIFF") && has_at(bytes, 8, b"WEBP") {
        return Some(Sniffed::new(MediaFormat::Webp, "image/webp"));
    }
    if let Some(file_type) = file_type_box(bytes) {
        // An ISO-BMFF file is a picture by its brands or not at all: BMP is not tried.
        return image_brand(&file_type);
    }
    if bytes.len() >= 18 && bytes.starts_with(b"BM") {
        // "BM" alone is weak; also require zeroed reserved fields and a known DIB header size.
        let reserved_zero = bytes[6..10].iter().all(|byte| *byte == 0);
        let dib_size = u32::from_le_bytes(brand_at(bytes, 14));
        if reserved_zero && BMP_DIB_HEADER_SIZES.contains(&dib_size) {
            return Some(Sniffed::new(MediaFormat::Bmp, "image/bmp"));
        }
    }
    None
}

/// The clone's `sniffMedia` over the leading bytes of a file (pass at most `SNIFF_BYTES`; more
/// are ignored). `None` when the bytes are not a supported picture or video, including an empty
/// buffer. The rules, in evaluation order (spec-export B.1-B.3):
///
/// 1. starts `FF D8 FF` -> jpeg
/// 2. starts `89 50 4E 47 0D 0A 1A 0A` -> png
/// 3. at least 6 bytes, starts "GIF87a" or "GIF89a" -> gif
/// 4. at least 12 bytes, "RIFF" at 0 and "WEBP" at 8 -> webp
/// 5. an ftyp box (at least 12 bytes, "ftyp" at 4; `box_end = min(len, declared >= 16 ?
///    declared : len)` with `declared` the big-endian u32 at 0; major brand = bytes 8..12;
///    compatible brands = every 4 bytes from offset 16 while `offset + 4 <= box_end`), in this
///    order: major in {avif, avis} -> avif; major in HEVC -> heic; any compatible in {avif, avis}
///    -> avif; any compatible in HEVC -> heic; major in {mif1, msf1} -> heic ("image/heif").
///    HEVC = {heic, heix, hevc, hevx, heim, heis, hevm, hevs}. When an ftyp box exists and is not
///    an image, rule 6 is not tried.
/// 6. no ftyp box: at least 18 bytes, "BM", bytes 6..10 all zero, little-endian u32 at offset 14
///    in {12, 40, 52, 56, 64, 108, 124} -> bmp
/// 7. an ftyp box that is not an image, whose major brand is four printable ASCII characters
///    (0x20..=0x7E) and not an audio brand {"M4A ", "M4B ", "M4P ", "F4A ", "F4B "}: major
///    "qt  " -> mov, any other -> mp4. Only the major brand is tested against the audio set.
/// 8. an EBML header (`ebml_doc_type`) whose DocType is exactly "webm" -> webm
///
/// Vectors (sniff.test.ts): `00 00 00 14 "ftyp" "mp42" 20 20 20 20 "mp42"` -> mp4;
/// `00 00 00 1C "ftypmsf1" 20 20 20 20 "isommsf1avis"` -> avif; `00 00 00 10 "ftyp" 00 FF 01 02`
/// -> `None`; `1A 45 DF A3 40 0E 42 F7 81 01 42 82 40 04 77 65 62 6D` -> webm;
/// `1A 45 DF A3 84 42 86 81 01` -> `None`; `FF D8` alone, SVG, text -> `None`.
pub fn sniff_media(bytes: &[u8]) -> Option<Sniffed> {
    let bytes = head(bytes);
    if let Some(image) = sniff_image(bytes) {
        return Some(image);
    }
    if let Some(file_type) = file_type_box(bytes) {
        if is_brand(&file_type.major) && !in_set(&AUDIO_BRANDS, &file_type.major) {
            return Some(if &file_type.major == b"qt  " {
                Sniffed::new(MediaFormat::Mov, "video/quicktime")
            } else {
                Sniffed::new(MediaFormat::Mp4, "video/mp4")
            });
        }
    }
    if ebml_doc_type(bytes).as_deref() == Some("webm") {
        return Some(Sniffed::new(MediaFormat::Webm, "video/webm"));
    }
    None
}

/// Length in bytes of an EBML variable-size integer, from its first byte (0 when invalid).
fn vint_length(first: u8) -> usize {
    if first == 0 {
        0
    } else {
        first.leading_zeros() as usize + 1
    }
}

/// The value of an EBML variable-size integer of `length` bytes at `position`: the first byte
/// without its length marker, then the following bytes, big-endian. Bytes beyond the buffer count
/// as 0, as in the clone (`bytes[i] ?? 0`).
fn vint_value(bytes: &[u8], position: usize, length: usize) -> u64 {
    let byte = |index: usize| u64::from(bytes.get(index).copied().unwrap_or(0));
    let mut value = byte(position) & (0xFF_u64 >> length);
    for index in 1..length {
        value = value * 256 + byte(position + index);
    }
    value
}

/// The DocType of an EBML header, as the clone's `ebmlDocType` parses it (sniff.ts:98-136;
/// spec-export B.3, where the algorithm is written out). `None` when the bytes are not EBML
/// (they do not start `1A 45 DF A3`, or the header size is malformed); `Some("")` for an EBML
/// header without a DocType element (Matroska by default); otherwise the DocType text (at most
/// 32 bytes, Latin-1, trailing NULs removed), e.g. "webm" or "matroska".
pub fn ebml_doc_type(bytes: &[u8]) -> Option<String> {
    let bytes = head(bytes);
    if !bytes.starts_with(&EBML_MAGIC) {
        return None;
    }
    let size_length = vint_length(bytes.get(4).copied().unwrap_or(0));
    if size_length == 0 {
        return None;
    }
    let size = vint_value(bytes, 4, size_length);
    let mut position = 4 + size_length;
    // The buffer is at most SNIFF_BYTES long, so every position below fits a usize easily; the
    // header size itself may be any 56-bit number and is only ever used through this minimum.
    let end = bytes.len().min(position.saturating_add(usize::try_from(size).unwrap_or(usize::MAX)));

    while position < end {
        let id_length = vint_length(bytes[position]);
        if id_length == 0 || id_length > 4 || position + id_length >= end {
            break;
        }
        // The id keeps its length marker bit, which is how EBML ids are written (0x4282).
        let id = bytes[position..position + id_length].iter().fold(0_u32, |id, byte| id * 256 + u32::from(*byte));
        position += id_length;

        let length_of_size = vint_length(bytes[position]);
        if length_of_size == 0 || position + length_of_size > end {
            break;
        }
        let length = vint_value(bytes, position, length_of_size);
        position += length_of_size;

        if id == EBML_DOC_TYPE_ID {
            // Clipped to the buffer, not to the header: that is what the clone's ascii() does.
            let wanted = length.min(MAX_DOC_TYPE_BYTES) as usize;
            let text = &bytes[position.min(bytes.len())..(position + wanted).min(bytes.len())];
            let doc_type: String = text.iter().map(|byte| char::from(*byte)).collect();
            return Some(doc_type.trim_end_matches('\0').to_string());
        }
        position = position.saturating_add(usize::try_from(length).unwrap_or(usize::MAX));
    }
    Some(String::new())
}

/// The first `min(file size, SNIFF_BYTES)` bytes of a file. An empty file gives an empty vector.
pub fn read_head(path: &Path) -> io::Result<Vec<u8>> {
    read_head_of(&File::open(path)?)
}

/// `read_head` for a file that is already open, read from its current position (the start, for
/// a file that was just opened). For callers that also want the size or the modified time of
/// the same file and therefore open it themselves.
pub fn read_head_of(mut file: &File) -> io::Result<Vec<u8>> {
    let mut bytes = Vec::with_capacity(SNIFF_BYTES);
    // read_to_end keeps reading until the limit or the end of the file, also when the operating
    // system hands out the bytes in several pieces.
    (&mut file).take(SNIFF_BYTES as u64).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// `sniff_media(read_head(path))`. `Ok(None)` when the file is readable but not a supported
/// picture or video (also a zero-byte file); `Err` when it cannot be opened or read.
pub fn sniff_file(path: &Path) -> io::Result<Option<Sniffed>> {
    Ok(sniff_media(&read_head(path)?))
}

/// The clone's `describeUnsupportedMedia` (sniff.ts:161-174; spec-export B.4): for bytes that
/// `sniff_media` rejected, the name of a well-known format that is not supported, so the warning
/// can say what the file is. First match wins:
///
/// | Test | Text |
/// |---|---|
/// | EBML whose DocType is not "webm" (also "") | `a Matroska (MKV) video` |
/// | ftyp box whose major brand is an audio brand | `an audio file (M4A)` |
/// | at least 12 bytes, "RIFF" at 0, "AVI " at 8 | `an AVI video` |
/// | starts `30 26 B2 75 8E 66 CF 11` | `a Windows Media (WMV/ASF) video` |
/// | starts `46 4C 56 01` | `a Flash (FLV) video` |
/// | starts `00 00 01 BA` or `00 00 01 B3` | `an MPEG video` |
/// | more than 188 bytes, byte 0 and byte 188 are `47` | `an MPEG transport stream (TS) video` |
/// | starts "OggS" | `an Ogg file` |
///
/// `None` otherwise. The texts are the constants `messages::UNSUPPORTED_*`.
pub fn describe_unsupported_media(bytes: &[u8]) -> Option<&'static str> {
    let bytes = head(bytes);
    if ebml_doc_type(bytes).is_some_and(|doc_type| doc_type != "webm") {
        return Some(messages::UNSUPPORTED_MATROSKA);
    }
    if file_type_box(bytes).is_some_and(|file_type| in_set(&AUDIO_BRANDS, &file_type.major)) {
        return Some(messages::UNSUPPORTED_AUDIO);
    }
    if bytes.len() >= 12 && has_at(bytes, 0, b"RIFF") && has_at(bytes, 8, b"AVI ") {
        return Some(messages::UNSUPPORTED_AVI);
    }
    if bytes.starts_with(&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11]) {
        return Some(messages::UNSUPPORTED_WINDOWS_MEDIA);
    }
    if bytes.starts_with(&[0x46, 0x4C, 0x56, 0x01]) {
        return Some(messages::UNSUPPORTED_FLASH);
    }
    if bytes.starts_with(&[0x00, 0x00, 0x01, 0xBA]) || bytes.starts_with(&[0x00, 0x00, 0x01, 0xB3]) {
        return Some(messages::UNSUPPORTED_MPEG);
    }
    // MPEG transport stream: a sync byte every 188 bytes.
    if bytes.len() > TS_PACKET_BYTES && bytes[0] == 0x47 && bytes[TS_PACKET_BYTES] == 0x47 {
        return Some(messages::UNSUPPORTED_TRANSPORT_STREAM);
    }
    if bytes.starts_with(b"OggS") {
        return Some(messages::UNSUPPORTED_OGG);
    }
    None
}

#[cfg(test)]
mod tests {
    //! The vectors of the clone's sniff.test.ts, built with a port of its testkit.ts.

    use super::*;
    use std::path::PathBuf;

    const SAMPLE_BYTES: usize = 64;

    /// testkit.ts `padded`: `head`, filled up to 64 bytes with `fill`.
    fn padded(head: &[u8], fill: u8) -> Vec<u8> {
        let mut bytes = head.to_vec();
        if bytes.len() < SAMPLE_BYTES {
            bytes.resize(SAMPLE_BYTES, fill);
        }
        bytes
    }

    /// testkit.ts `ftyp`: a box of exactly the declared size with a zero minor version.
    fn ftyp(major: &str, compatible: &[&str]) -> Vec<u8> {
        let size = 16 + compatible.len() * 4;
        let mut bytes = vec![0, 0, 0, size as u8];
        bytes.extend_from_slice(b"ftyp");
        bytes.extend_from_slice(major.as_bytes());
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        for brand in compatible {
            bytes.extend_from_slice(brand.as_bytes());
        }
        bytes
    }

    /// testkit.ts `ebml`: a header with EBMLVersion 1 and then the DocType.
    fn ebml(doc_type: &str) -> Vec<u8> {
        let mut body = vec![0x42, 0x86, 0x81, 0x01, 0x42, 0x82, 0x80 | doc_type.len() as u8];
        body.extend_from_slice(doc_type.as_bytes());
        let mut bytes = vec![0x1A, 0x45, 0xDF, 0xA3, 0x80 | body.len() as u8];
        bytes.extend_from_slice(&body);
        bytes
    }

    /// The 32-byte buffers of the brand tests: `text` after a four-byte box size, zero padded.
    fn brand_box(declared: u8, text: &str) -> Vec<u8> {
        let mut bytes = vec![0, 0, 0, declared];
        bytes.extend_from_slice(text.as_bytes());
        bytes.resize(32, 0);
        bytes
    }

    fn jpeg() -> Vec<u8> {
        padded(&[0xFF, 0xD8, 0xFF, 0xE0, 0x00, 0x10, b'J', b'F', b'I', b'F', 0], 1)
    }
    fn png() -> Vec<u8> {
        padded(&PNG_MAGIC, 2)
    }
    fn gif() -> Vec<u8> {
        padded(b"GIF89a", 3)
    }
    fn webp() -> Vec<u8> {
        padded(b"RIFF\x24\0\0\0WEBPVP8 ", 4)
    }
    fn bmp() -> Vec<u8> {
        padded(b"BM\x40\0\0\0\0\0\0\0\x36\0\0\0\x28\0\0\0", 5)
    }
    fn avif() -> Vec<u8> {
        padded(&ftyp("avif", &["avif", "mif1", "miaf"]), 6)
    }
    fn heic() -> Vec<u8> {
        padded(&ftyp("heic", &["mif1", "heic"]), 7)
    }
    fn heif_mif1() -> Vec<u8> {
        padded(&ftyp("mif1", &["mif1", "heic"]), 8)
    }
    fn svg() -> Vec<u8> {
        b"<svg xmlns=\"http://www.w3.org/2000/svg\" onload=\"alert(1)\"></svg>".to_vec()
    }
    fn mp4() -> Vec<u8> {
        padded(&ftyp("isom", &["isom", "iso2", "avc1", "mp41"]), 21)
    }
    fn m4v() -> Vec<u8> {
        padded(&ftyp("M4V ", &["M4V ", "M4A ", "mp42", "isom"]), 22)
    }
    fn mov() -> Vec<u8> {
        padded(&ftyp("qt  ", &["qt  "]), 23)
    }
    fn webm() -> Vec<u8> {
        padded(&ebml("webm"), 24)
    }
    fn mkv() -> Vec<u8> {
        padded(&ebml("matroska"), 25)
    }
    fn avi() -> Vec<u8> {
        padded(b"RIFF\x24\0\0\0AVI LIST", 26)
    }
    fn wmv() -> Vec<u8> {
        padded(&[0x30, 0x26, 0xB2, 0x75, 0x8E, 0x66, 0xCF, 0x11, 0xA6, 0xD9], 27)
    }
    fn flv() -> Vec<u8> {
        padded(b"FLV\x01\x05", 28)
    }
    fn m4a() -> Vec<u8> {
        padded(&ftyp("M4A ", &["M4A ", "mp42", "isom"]), 29)
    }

    fn image(format: MediaFormat, mime: &'static str) -> Option<Sniffed> {
        Some(Sniffed { format, kind: MediaKind::Image, mime })
    }

    fn video(format: MediaFormat, mime: &'static str) -> Option<Sniffed> {
        Some(Sniffed { format, kind: MediaKind::Video, mime })
    }

    #[test]
    fn recognises_every_picture_format_by_its_magic_bytes() {
        let cases: [(Vec<u8>, MediaFormat, &'static str); 8] = [
            (jpeg(), MediaFormat::Jpeg, "image/jpeg"),
            (png(), MediaFormat::Png, "image/png"),
            (gif(), MediaFormat::Gif, "image/gif"),
            (webp(), MediaFormat::Webp, "image/webp"),
            (bmp(), MediaFormat::Bmp, "image/bmp"),
            (avif(), MediaFormat::Avif, "image/avif"),
            (heic(), MediaFormat::Heic, "image/heic"),
            (heif_mif1(), MediaFormat::Heic, "image/heic"),
        ];
        for (bytes, format, mime) in cases {
            assert_eq!(sniff_image(&bytes), image(format, mime), "{format:?}");
            // sniffMedia recognises every supported image, as an image.
            assert_eq!(sniff_media(&bytes), image(format, mime), "{format:?}");
        }
        assert_eq!(sniff_media(&padded(b"GIF87a", 3)), image(MediaFormat::Gif, "image/gif"));
    }

    #[test]
    fn treats_the_hevc_and_generic_heif_brands_as_heic() {
        for brand in ["heic", "heix", "hevc", "hevx", "heim", "heis", "hevm", "hevs", "mif1", "msf1"] {
            let mut bytes = vec![0_u8; 32];
            bytes[..4].copy_from_slice(&[0, 0, 0, 24]);
            bytes[4..12].copy_from_slice(format!("ftyp{brand}").as_bytes());
            bytes[16..24].copy_from_slice(format!("{brand}miaf").as_bytes());
            let sniffed = sniff_media(&bytes).unwrap_or_else(|| panic!("{brand} was not recognised"));
            assert_eq!(sniffed.format, MediaFormat::Heic, "{brand}");
            assert_eq!(sniffed.kind, MediaKind::Image, "{brand}");
        }
    }

    #[test]
    fn generic_heif_without_a_codec_brand_reports_the_heif_mime_type() {
        let bytes = brand_box(24, "ftypmif1\0\0\0\0mif1miaf");
        assert_eq!(sniff_media(&bytes), image(MediaFormat::Heic, "image/heif"));
    }

    #[test]
    fn prefers_avif_when_a_generic_heif_major_brand_lists_avif_as_compatible() {
        let mut bytes = vec![0_u8; 32];
        bytes[..4].copy_from_slice(&[0, 0, 0, 28]);
        bytes[4..12].copy_from_slice(b"ftypmif1");
        bytes[16..28].copy_from_slice(b"mif1avifmiaf");
        assert_eq!(sniff_image(&bytes), image(MediaFormat::Avif, "image/avif"));
    }

    #[test]
    fn compatible_brands_beyond_the_declared_box_are_not_read() {
        // The box ends at byte 20, so the "avif" at 20..24 belongs to the next box.
        let mut bytes = vec![0_u8; 32];
        bytes[..4].copy_from_slice(&[0, 0, 0, 20]);
        bytes[4..12].copy_from_slice(b"ftypmif1");
        bytes[16..24].copy_from_slice(b"mif1avif");
        assert_eq!(sniff_media(&bytes), image(MediaFormat::Heic, "image/heif"));
        // A declared size below 16 is not believed: the brands are read to the end of the buffer.
        bytes[3] = 8;
        assert_eq!(sniff_media(&bytes), image(MediaFormat::Avif, "image/avif"));
    }

    #[test]
    fn rejects_svg_video_text_and_truncated_input_as_a_picture() {
        assert_eq!(sniff_image(&svg()), None);
        assert_eq!(sniff_image(&mp4()), None);
        assert_eq!(sniff_image(&webm()), None);
        assert_eq!(sniff_image(b"<!doctype html><html></html>"), None);
        assert_eq!(sniff_image(b"BM, this is just text that starts with BM"), None);
        assert_eq!(sniff_image(&[0xFF, 0xD8]), None);
        assert_eq!(sniff_image(&[]), None);
        assert_eq!(sniff_media(&svg()), None);
        assert_eq!(sniff_media(b"BM, this is just text that starts with BM"), None);
        assert_eq!(sniff_media(&[0xFF, 0xD8]), None);
    }

    #[test]
    fn a_bitmap_needs_zeroed_reserved_bytes_and_a_known_header_size() {
        assert!(sniff_media(&bmp()).is_some());
        let mut reserved = bmp();
        reserved[7] = 1;
        assert_eq!(sniff_media(&reserved), None);
        let mut header = bmp();
        header[14] = 41;
        assert_eq!(sniff_media(&header), None);
        for size in BMP_DIB_HEADER_SIZES {
            let mut bytes = bmp();
            bytes[14] = size as u8;
            assert_eq!(sniff_media(&bytes), image(MediaFormat::Bmp, "image/bmp"), "DIB header size {size}");
        }
        // 17 bytes are one too few to read the header size.
        assert_eq!(sniff_media(&bmp()[..17]), None);
        assert!(sniff_media(&bmp()[..18]).is_some());
    }

    #[test]
    fn recognises_videos() {
        assert_eq!(sniff_media(&mp4()), video(MediaFormat::Mp4, "video/mp4"));
        assert_eq!(sniff_media(&m4v()), video(MediaFormat::Mp4, "video/mp4"));
        assert_eq!(sniff_media(&mov()), video(MediaFormat::Mov, "video/quicktime"));
        assert_eq!(sniff_media(&webm()), video(MediaFormat::Webm, "video/webm"));
    }

    #[test]
    fn treats_other_ftyp_brands_as_mp4() {
        for brand in ["mp42", "avc1", "iso5", "dash", "3gp4", "MSNV", "XAVC"] {
            let bytes = brand_box(20, &format!("ftyp{brand}    {brand}"));
            assert_eq!(sniff_media(&bytes), video(MediaFormat::Mp4, "video/mp4"), "{brand}");
        }
        // The vector of the specification: 00 00 00 14 "ftyp" "mp42" 20 20 20 20 "mp42".
        let exact = [[0_u8, 0, 0, 0x14].as_slice(), b"ftypmp42    mp42".as_slice()].concat();
        assert_eq!(sniff_media(&exact), video(MediaFormat::Mp4, "video/mp4"));
    }

    #[test]
    fn keeps_heic_heif_and_avif_images_whatever_else_the_brand_list_says() {
        let bytes = brand_box(28, "ftypmsf1    isommsf1avis");
        assert_eq!(sniff_media(&bytes), image(MediaFormat::Avif, "image/avif"));
        assert_eq!(sniff_media(&heic()).map(|sniffed| sniffed.kind), Some(MediaKind::Image));
        assert_eq!(sniff_media(&heif_mif1()).map(|sniffed| sniffed.kind), Some(MediaKind::Image));
    }

    #[test]
    fn rejects_audio_unsupported_containers_junk_brands_text_and_truncated_input() {
        for bytes in [m4a(), mkv(), avi(), wmv(), flv(), svg()] {
            assert_eq!(sniff_media(&bytes), None);
        }
        let mut junk_brand = vec![0_u8; 32];
        junk_brand[..4].copy_from_slice(&[0, 0, 0, 16]);
        junk_brand[4..8].copy_from_slice(b"ftyp");
        junk_brand[8..12].copy_from_slice(&[0x00, 0xFF, 0x01, 0x02]);
        assert_eq!(sniff_media(&junk_brand), None);
        assert_eq!(sniff_media(b"ftyp"), None);
        assert_eq!(sniff_media(&[0x1A, 0x45, 0xDF]), None);
        assert_eq!(sniff_media(&[]), None);
        // Only the major brand is tested against the audio brands.
        for brand in ["M4A ", "M4B ", "M4P ", "F4A ", "F4B "] {
            assert_eq!(sniff_media(&ftyp(brand, &["isom", "mp42"])), None, "{brand}");
        }
    }

    #[test]
    fn reads_the_doc_type_wherever_it_sits_in_the_ebml_header() {
        // EBML header with a two-byte size, an unknown element first, then DocType "webm".
        let header: [u8; 14] = [0x1A, 0x45, 0xDF, 0xA3, 0x40, 0x0E, 0x42, 0xF7, 0x81, 0x01, 0x42, 0x82, 0x40, 0x04];
        let bytes = [header.as_slice(), b"webm".as_slice()].concat();
        assert_eq!(sniff_media(&bytes), video(MediaFormat::Webm, "video/webm"));
        assert_eq!(ebml_doc_type(&bytes).as_deref(), Some("webm"));
        // Without a DocType, EBML means Matroska.
        let bare = [0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42, 0x86, 0x81, 0x01];
        assert_eq!(sniff_media(&bare), None);
        assert_eq!(ebml_doc_type(&bare).as_deref(), Some(""));
    }

    #[test]
    fn the_doc_type_parser_survives_malformed_headers() {
        assert_eq!(ebml_doc_type(&[]), None);
        assert_eq!(ebml_doc_type(b"not ebml at all"), None);
        // The magic alone, and a size whose first byte is 0 (no length marker).
        assert_eq!(ebml_doc_type(&EBML_MAGIC), None);
        assert_eq!(ebml_doc_type(&[0x1A, 0x45, 0xDF, 0xA3, 0x00, 0x42, 0x82]), None);
        // An eight-byte header size that is far larger than the buffer.
        let huge = [0x1A, 0x45, 0xDF, 0xA3, 0x01, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0x42, 0x82, 0x84, b'w', b'e', b'b', b'm'];
        assert_eq!(ebml_doc_type(&huge).as_deref(), Some("webm"));
        // An element whose length runs past the buffer, an id of five bytes, a cut-off element.
        assert_eq!(ebml_doc_type(&[0x1A, 0x45, 0xDF, 0xA3, 0x8A, 0x42, 0x86, 0x08, 0xFF, 0xFF, 0xFF]).as_deref(), Some(""));
        assert_eq!(ebml_doc_type(&[0x1A, 0x45, 0xDF, 0xA3, 0x88, 0x08, 0x01, 0x02, 0x03, 0x04, 0x81, 0x00]).as_deref(), Some(""));
        assert_eq!(ebml_doc_type(&[0x1A, 0x45, 0xDF, 0xA3, 0x82, 0x42, 0x82]).as_deref(), Some(""));
        // A DocType that is cut off by the end of the buffer, and one padded with NULs.
        assert_eq!(ebml_doc_type(&[0x1A, 0x45, 0xDF, 0xA3, 0x88, 0x42, 0x82, 0x88, b'w', b'e']).as_deref(), Some("we"));
        assert_eq!(
            ebml_doc_type(&[0x1A, 0x45, 0xDF, 0xA3, 0x8B, 0x42, 0x82, 0x88, b'w', b'e', b'b', b'm', 0, 0, 0, 0]).as_deref(),
            Some("webm")
        );
        assert_eq!(ebml_doc_type(&mkv()).as_deref(), Some("matroska"));
    }

    #[test]
    fn names_well_known_unsupported_formats() {
        assert_eq!(describe_unsupported_media(&mkv()), Some("a Matroska (MKV) video"));
        assert_eq!(describe_unsupported_media(&avi()), Some("an AVI video"));
        assert_eq!(describe_unsupported_media(&wmv()), Some("a Windows Media (WMV/ASF) video"));
        assert_eq!(describe_unsupported_media(&flv()), Some("a Flash (FLV) video"));
        assert_eq!(describe_unsupported_media(&m4a()), Some("an audio file (M4A)"));
        assert_eq!(describe_unsupported_media(&[0, 0, 1, 0xBA, 0x44]), Some("an MPEG video"));
        assert_eq!(describe_unsupported_media(&[0, 0, 1, 0xB3, 0x44]), Some("an MPEG video"));
        assert_eq!(describe_unsupported_media(b"OggS "), Some("an Ogg file"));
        // EBML without a DocType is Matroska by default.
        assert_eq!(
            describe_unsupported_media(&[0x1A, 0x45, 0xDF, 0xA3, 0x84, 0x42, 0x86, 0x81, 0x01]),
            Some("a Matroska (MKV) video")
        );
    }

    #[test]
    fn recognises_an_mpeg_transport_stream_by_its_sync_bytes() {
        let mut stream = vec![0_u8; 256];
        stream[0] = 0x47;
        stream[188] = 0x47;
        assert_eq!(describe_unsupported_media(&stream), Some("an MPEG transport stream (TS) video"));
        stream[188] = 0;
        assert_eq!(describe_unsupported_media(&stream), None);
        // Exactly 188 bytes have no second sync byte to look at.
        let mut short = vec![0_u8; 188];
        short[0] = 0x47;
        assert_eq!(describe_unsupported_media(&short), None);
    }

    #[test]
    fn says_nothing_about_supported_media_or_unknown_bytes() {
        assert_eq!(describe_unsupported_media(&webm()), None);
        assert_eq!(describe_unsupported_media(&svg()), None);
        assert_eq!(describe_unsupported_media(b"hello world"), None);
        assert_eq!(describe_unsupported_media(&[]), None);
        assert_eq!(describe_unsupported_media(b"Ogg"), None);
    }

    #[test]
    fn only_the_first_256_bytes_count() {
        // A whole file handed in gives the answer its head gives: here the ftyp box declares 264
        // bytes and lists "avif" just behind the head.
        let mut bytes = vec![0_u8; 300];
        bytes[..4].copy_from_slice(&[0, 0, 1, 8]);
        bytes[4..12].copy_from_slice(b"ftypmif1");
        bytes[256..260].copy_from_slice(b"avif");
        assert_eq!(sniff_media(&bytes), image(MediaFormat::Heic, "image/heif"));
        assert_eq!(sniff_media(&bytes), sniff_media(&bytes[..SNIFF_BYTES]));
    }

    #[test]
    fn formats_know_their_extension_and_kind() {
        assert_eq!(MediaFormat::Jpeg.extension(), "jpg");
        assert_eq!(MediaFormat::Heic.extension(), "heic");
        assert_eq!(MediaFormat::Mov.kind(), MediaKind::Video);
        assert_eq!(MediaFormat::Avif.kind(), MediaKind::Image);
        assert!(MediaFormat::Webp.has_builtin_decoder());
        assert!(!MediaFormat::Heic.has_builtin_decoder());
    }

    fn fixture(relative: &str) -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests").join("fixtures").join(relative)
    }

    #[test]
    fn sniffs_the_fixture_files_by_content_not_by_name() {
        let cases = [
            ("loose/IMG_2.jpg", MediaFormat::Jpeg),
            ("loose/img_1.png", MediaFormat::Png),
            ("loose/Sunset at the pier (final).webp", MediaFormat::Webp),
            ("loose/portrait.heic", MediaFormat::Heic),
            ("export-basic/posts/post4.heic", MediaFormat::Heic),
            // Wrong extensions: a WebP named .png and a PNG named .jpg.
            ("export-lenient/posts/lenient2.png", MediaFormat::Webp),
            ("export-lenient/posts/lenient4.jpg", MediaFormat::Png),
        ];
        for (relative, format) in cases {
            let sniffed = sniff_file(&fixture(relative)).unwrap_or_else(|error| panic!("{relative}: {error}"));
            assert_eq!(sniffed.map(|sniffed| sniffed.format), Some(format), "{relative}");
            assert_eq!(sniffed.map(|sniffed| sniffed.kind), Some(MediaKind::Image), "{relative}");
        }
    }

    #[test]
    fn unsupported_empty_and_missing_files() {
        // Text is readable but not a picture; so is a zero-byte file.
        assert_eq!(sniff_file(&fixture("loose/notes.txt")).unwrap(), None);
        assert_eq!(sniff_file(&fixture("loose/broken.jpg")).unwrap(), None);
        assert_eq!(read_head(&fixture("loose/broken.jpg")).unwrap(), Vec::<u8>::new());
        assert!(sniff_file(&fixture("loose/no-such-file.jpg")).is_err());
        // A folder cannot be read as a file.
        assert!(sniff_file(&fixture("loose")).is_err());
    }

    #[test]
    fn the_head_is_at_most_256_bytes() {
        let head = read_head(&fixture("loose/IMG_2.jpg")).unwrap();
        assert_eq!(head.len(), SNIFF_BYTES);
        assert!(head.starts_with(&JPEG_MAGIC));
        let small = read_head(&fixture("loose/notes.txt")).unwrap();
        assert_eq!(small.len(), 32);
    }
}
