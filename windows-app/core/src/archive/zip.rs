//! The read side of a zip archive: a port of the clone's random-access reader
//! (frontend/src/import/zip.ts), with the same constants, the same order of checks and the same
//! sentences.
//!
//! Only the central directory (a few kilobytes at the end of the file) is read to list the
//! members. A member is then copied on request straight from the file: a stored member byte for
//! byte, a deflated one through `flate2`'s raw deflate decoder, never producing more than the
//! member declares or the caller allows. The `zip` crate is not used for reading: its reader
//! refuses whole entries whose Unicode path field differs from the main name (for example by a
//! trailing dot or a control character) and decodes names without the UTF-8 flag as CP437 only,
//! while the clone tries UTF-8 first and accepts both (web brief 3.2).
//!
//! Member names are only ever compared as keys: they never become a file name on disk (the
//! archive cache names its files itself), so `../x`, `C:\x`, `con.jpg` or `a:b` cannot reach
//! the file system from here.

use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom, Write};

use flate2::read::DeflateDecoder;

use super::{decode_name, decode_zip_name, ArchiveError, ArchiveErrorCode};
use crate::manifest::{is_noise_path, normalize_entry_path};
use crate::messages;

const SIG_LOCAL: u32 = 0x0403_4b50;
const SIG_CENTRAL: u32 = 0x0201_4b50;
const SIG_EOCD: u32 = 0x0605_4b50;
const SIG_EOCD64: u32 = 0x0606_4b50;
const SIG_EOCD64_LOCATOR: u32 = 0x0706_4b50;
/// Fixed part of a local file header; the member's data follows it (after name and extra field).
const LOCAL_HEADER_BYTES: u64 = 30;
/// Fixed part of a central directory record.
const CENTRAL_HEADER_BYTES: usize = 46;
/// Fixed part of the end-of-central-directory record.
const EOCD_MIN: u64 = 22;
/// Longest archive comment after the end record.
const MAX_COMMENT: u64 = 0xffff;
/// The ZIP64 end record is 56 bytes long (without its extensible data).
const EOCD64_BYTES: u64 = 56;
/// A central directory larger than this is refused (zip.ts `MAX_CENTRAL_DIRECTORY_BYTES`).
pub const MAX_CENTRAL_DIRECTORY_BYTES: u64 = 128 * 1024 * 1024;
/// Bytes copied or inflated per step.
const COPY_STEP: usize = 64 * 1024;
/// The largest offset the clone's `readU64` accepts: JavaScript numbers are exact up to 2^53.
const MAX_SAFE_HIGH_WORD: u32 = 0x1f_ffff;

/// `The zip file is damaged or incomplete ({detail}).`
fn corrupt(detail: &str) -> ArchiveError {
    ArchiveError::new(ArchiveErrorCode::ReadFailed, messages::zip_damaged(detail))
}

/// One member of the archive (a file; folders are not listed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ZipMember {
    /// The normalised path inside the archive (`manifest::normalize_entry_path`).
    pub path: String,
    /// General purpose flags; bit 0 means encrypted.
    pub flags: u16,
    /// The compression method: 0 stored, 8 deflate.
    pub method: u16,
    /// Bytes of the member's data in the archive.
    pub compressed_size: u64,
    /// Bytes of the member once uncompressed, as the directory declares them.
    pub size: u64,
    /// Where its local header starts, before the prefix shift.
    pub local_offset: u64,
}

/// Where the central directory is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Directory {
    /// Absolute position of the central directory in the file.
    start: u64,
    length: u64,
    /// Added to every offset stored in the archive (non-zero for self-extracting prefixes).
    shift: u64,
}

/// An opened zip archive: the file and its listed members.
#[derive(Debug)]
pub struct ZipReader<R> {
    input: R,
    len: u64,
    shift: u64,
    members: Vec<ZipMember>,
}

fn u16_at(bytes: &[u8], offset: usize) -> u16 {
    u16::from_le_bytes([bytes[offset], bytes[offset + 1]])
}

fn u32_at(bytes: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes([bytes[offset], bytes[offset + 1], bytes[offset + 2], bytes[offset + 3]])
}

/// zip.ts `readU64`: a 64-bit little-endian number, refused above 2^53 as the clone does.
fn u64_at(bytes: &[u8], offset: usize) -> Result<u64, ArchiveError> {
    let low = u32_at(bytes, offset);
    let high = u32_at(bytes, offset + 4);
    if high > MAX_SAFE_HIGH_WORD {
        return Err(corrupt("offset out of range"));
    }
    Ok((u64::from(high) << 32) | u64::from(low))
}

/// Reads `length` bytes at `start`; fewer only at the end of the input.
fn read_at<R: Read + Seek>(input: &mut R, start: u64, length: u64) -> Result<Vec<u8>, ArchiveError> {
    let unreadable = |error: io::Error| {
        ArchiveError::new(ArchiveErrorCode::ReadFailed, format!("The file could not be read ({}).", messages::describe_error(&error.to_string())))
    };
    input.seek(SeekFrom::Start(start)).map_err(unreadable)?;
    let mut bytes = Vec::new();
    input.by_ref().take(length).read_to_end(&mut bytes).map_err(unreadable)?;
    Ok(bytes)
}

/// zip.ts `locateDirectory`: the end record, the ZIP64 record when there is one, and the shift of
/// a self-extracting prefix.
fn locate_directory<R: Read + Seek>(input: &mut R, size: u64) -> Result<Directory, ArchiveError> {
    if size < EOCD_MIN {
        return Err(corrupt("too short"));
    }
    let tail_length = size.min(EOCD_MIN + MAX_COMMENT + 20);
    let tail_start = size - tail_length;
    let tail = read_at(input, tail_start, tail_length)?;

    let minimum = EOCD_MIN as usize;
    let eocd = (0..=tail.len().saturating_sub(minimum))
        .rev()
        .find(|&position| {
            tail.len() >= position + minimum
                && u32_at(&tail, position) == SIG_EOCD
                && position + minimum + usize::from(u16_at(&tail, position + 20)) <= tail.len()
        })
        .ok_or_else(|| corrupt("no end-of-archive record"))?;

    let mut length = u64::from(u32_at(&tail, eocd + 12));
    let mut offset = u64::from(u32_at(&tail, eocd + 16));
    let mut end = tail_start + eocd as u64;
    let this_disk = u16_at(&tail, eocd + 4);
    if this_disk != 0xffff && this_disk != u16_at(&tail, eocd + 6) {
        return Err(ArchiveError::new(ArchiveErrorCode::ReadFailed, messages::ZIP_MULTI_PART));
    }

    // ZIP64: the real numbers live in a separate record announced by a locator before the end.
    if eocd >= 20 && u32_at(&tail, eocd - 20) == SIG_EOCD64_LOCATOR {
        let declared = u64_at(&tail, eocd - 20 + 8)?;
        let behind_locator = (tail_start + eocd as u64).checked_sub(20 + EOCD64_BYTES);
        for position in [Some(declared), behind_locator].into_iter().flatten() {
            if position + EOCD64_BYTES > size {
                continue;
            }
            let record = read_at(input, position, EOCD64_BYTES)?;
            if record.len() < EOCD64_BYTES as usize || u32_at(&record, 0) != SIG_EOCD64 {
                continue;
            }
            length = u64_at(&record, 40)?;
            offset = u64_at(&record, 48)?;
            end = position;
            break;
        }
    }

    if length > MAX_CENTRAL_DIRECTORY_BYTES {
        return Err(ArchiveError::new(ArchiveErrorCode::TooLarge, messages::ZIP_TOO_MANY_FILES));
    }
    // The directory normally ends where the end record starts; any difference to the stored
    // offset means data was prepended to the archive (self-extracting stubs).
    let (start, shift) = match end.checked_sub(length) {
        Some(start) if start >= offset => (start, start - offset),
        _ => (offset, 0),
    };
    if start.checked_add(length).is_none_or(|directory_end| directory_end > size) {
        return Err(corrupt("bad directory position"));
    }
    Ok(Directory { start, length, shift })
}

/// zip.ts `parseDirectory`: the members of the directory bytes, folders and noise left out.
/// More than `max_entries` members -> `zip_more_files`.
fn parse_directory(bytes: &[u8], max_entries: usize) -> Result<Vec<ZipMember>, ArchiveError> {
    let mut members: Vec<ZipMember> = Vec::new();
    let mut position = 0;
    while position + CENTRAL_HEADER_BYTES <= bytes.len() && u32_at(bytes, position) == SIG_CENTRAL {
        let flags = u16_at(bytes, position + 8);
        let method = u16_at(bytes, position + 10);
        let mut compressed_size = u64::from(u32_at(bytes, position + 20));
        let mut size = u64::from(u32_at(bytes, position + 24));
        let name_length = usize::from(u16_at(bytes, position + 28));
        let extra_length = usize::from(u16_at(bytes, position + 30));
        let comment_length = usize::from(u16_at(bytes, position + 32));
        let mut local_offset = u64::from(u32_at(bytes, position + 42));
        let name_start = position + CENTRAL_HEADER_BYTES;
        let extra_start = name_start + name_length;
        let extra_end = extra_start + extra_length;
        let next = extra_end + comment_length;
        if next > bytes.len() {
            return Err(corrupt("truncated directory"));
        }

        let mut name: Option<String> = None;
        let mut cursor = extra_start;
        while cursor + 4 <= extra_end {
            let id = u16_at(bytes, cursor);
            let length = usize::from(u16_at(bytes, cursor + 2));
            let body = cursor + 4;
            if body + length > extra_end {
                break;
            }
            if id == 0x0001 {
                // ZIP64: 8-byte values, present only for the fields that overflowed, in this order.
                let mut field = body;
                if size == 0xffff_ffff && field + 8 <= body + length {
                    size = u64_at(bytes, field)?;
                    field += 8;
                }
                if compressed_size == 0xffff_ffff && field + 8 <= body + length {
                    compressed_size = u64_at(bytes, field)?;
                    field += 8;
                }
                if local_offset == 0xffff_ffff && field + 8 <= body + length {
                    local_offset = u64_at(bytes, field)?;
                }
            } else if id == 0x7075 && length > 5 && bytes[body] == 1 {
                // Info-ZIP Unicode Path: the UTF-8 name for archives whose main name is in a
                // legacy charset.
                name = Some(decode_name(&bytes[body + 5..body + length]));
            }
            cursor = body + length;
        }
        // Bit 11 marks UTF-8; without it most modern tools still write UTF-8, so try that first.
        let name = name.unwrap_or_else(|| decode_zip_name(&bytes[name_start..extra_start]));

        position = next;
        if name.ends_with('/') || name.ends_with('\\') {
            continue;
        }
        let Some(path) = normalize_entry_path(&name) else { continue };
        if is_noise_path(&path) {
            continue;
        }
        members.push(ZipMember { path, flags, method, compressed_size, size, local_offset });
        if members.len() > max_entries {
            return Err(ArchiveError::new(ArchiveErrorCode::TooLarge, messages::zip_more_files(max_entries)));
        }
    }
    Ok(members)
}

/// zip.ts `assertNoOverlap`: every member owns its own stretch of the file. Members that share
/// bytes are how a few kilobytes are made to unpack to gigabytes (one compressed stream listed
/// many times), and no zip tool writes them: such an archive is refused as a whole.
fn assert_no_overlap(members: &[ZipMember]) -> Result<(), ArchiveError> {
    let mut by_offset: Vec<&ZipMember> = members.iter().collect();
    by_offset.sort_by_key(|member| member.local_offset);
    let mut end: u64 = 0;
    for member in by_offset {
        if member.local_offset < end {
            return Err(corrupt("files overlap"));
        }
        end = member.local_offset.saturating_add(LOCAL_HEADER_BYTES).saturating_add(member.compressed_size);
    }
    Ok(())
}

impl ZipReader<File> {
    /// Opens a zip file and lists its members (`open_with`).
    pub fn open(file: File, max_entries: usize) -> Result<ZipReader<File>, ArchiveError> {
        let len = file
            .metadata()
            .map_err(|error| ArchiveError::new(ArchiveErrorCode::ReadFailed, format!("The file could not be read ({}).", messages::describe_error(&error.to_string()))))?
            .len();
        ZipReader::open_with(file, len, max_entries)
    }
}

impl<R: Read + Seek> ZipReader<R> {
    /// zip.ts `readZipEntries`: lists the members of a zip archive of `len` bytes. Nothing but
    /// the central directory is read here. More than `max_entries` members are refused.
    pub fn open_with(mut input: R, len: u64, max_entries: usize) -> Result<ZipReader<R>, ArchiveError> {
        let directory = locate_directory(&mut input, len)?;
        let bytes = read_at(&mut input, directory.start, directory.length)?;
        let mut members = parse_directory(&bytes, max_entries)?;
        let mut shift = directory.shift;
        if members.is_empty() && directory.shift != 0 && directory.length > 0 {
            // The "prepended data" guess was wrong (junk between directory and end record):
            // trust the stored offset.
            let start = directory.start - directory.shift;
            members = parse_directory(&read_at(&mut input, start, directory.length)?, max_entries)?;
            shift = 0;
        }
        assert_no_overlap(&members)?;
        Ok(ZipReader { input, len, shift, members })
    }

    /// The members, in the order of the central directory.
    pub fn members(&self) -> &[ZipMember] {
        &self.members
    }

    /// zip.ts `memberEntry().open`: copies member `index` into `out`, at most `max_bytes` of it.
    /// In this order: encrypted -> `zip_password`; another method than stored or deflate ->
    /// `zip_method`; declared larger than `max_bytes` -> `member_too_large`; the local header
    /// beyond the end, missing or the data cut off, a stored member whose sizes differ -> damaged;
    /// a deflated member that inflates to more than it declares, or to less -> damaged; a
    /// decoder error -> damaged with its text. Returns the bytes written (the declared size).
    pub fn copy_member(&mut self, index: usize, max_bytes: u64, out: &mut dyn Write) -> Result<u64, ArchiveError> {
        let Some(member) = self.members.get(index).cloned() else {
            return Err(corrupt("no such member"));
        };
        let path = member.path.as_str();
        if member.flags & 0x1 != 0 {
            return Err(ArchiveError::new(ArchiveErrorCode::ReadFailed, messages::zip_password(path)));
        }
        if member.method != 0 && member.method != 8 {
            return Err(ArchiveError::new(ArchiveErrorCode::ReadFailed, messages::zip_method(path, member.method)));
        }
        if member.size > max_bytes {
            return Err(ArchiveError::new(ArchiveErrorCode::TooLarge, messages::member_too_large(path, max_bytes)));
        }
        let header_start = member.local_offset.saturating_add(self.shift);
        if header_start.saturating_add(LOCAL_HEADER_BYTES) > self.len {
            return Err(corrupt(&format!("\"{path}\" starts beyond the end of the file")));
        }
        let header = read_at(&mut self.input, header_start, LOCAL_HEADER_BYTES)?;
        if header.len() < LOCAL_HEADER_BYTES as usize || u32_at(&header, 0) != SIG_LOCAL {
            return Err(corrupt(&format!("\"{path}\" has no local header")));
        }
        let data_start = header_start + LOCAL_HEADER_BYTES + u64::from(u16_at(&header, 26)) + u64::from(u16_at(&header, 28));
        if data_start.saturating_add(member.compressed_size) > self.len {
            return Err(corrupt(&format!("\"{path}\" is cut off")));
        }
        let unreadable = |error: io::Error| {
            ArchiveError::new(ArchiveErrorCode::ReadFailed, format!("The file could not be read ({}).", messages::describe_error(&error.to_string())))
        };
        self.input.seek(SeekFrom::Start(data_start)).map_err(unreadable)?;
        let compressed = (&mut self.input).take(member.compressed_size);

        if member.method == 0 {
            if member.compressed_size != member.size {
                return Err(corrupt(&format!("\"{path}\" has inconsistent sizes")));
            }
            return copy_limited(compressed, member.size, out).map_err(|failure| match failure {
                CopyFailure::Read(error) => corrupt(&format!("\"{path}\": {}", messages::describe_error(&error.to_string()))),
                CopyFailure::Write(error) => write_failed(&error),
                CopyFailure::TooMuch => corrupt(&format!("\"{path}\" is larger than declared")),
            });
        }

        // More data than the directory promised is corruption (or a bomb); more than the
        // caller's cap is simply too large.
        let limit = max_bytes.min(member.size);
        let produced = copy_limited(DeflateDecoder::new(compressed), limit, out).map_err(|failure| match failure {
            CopyFailure::Read(error) => corrupt(&format!("\"{path}\": {}", messages::describe_error(&error.to_string()))),
            CopyFailure::Write(error) => write_failed(&error),
            CopyFailure::TooMuch if member.size > max_bytes => {
                ArchiveError::new(ArchiveErrorCode::TooLarge, messages::member_too_large(path, max_bytes))
            }
            CopyFailure::TooMuch => corrupt(&format!("\"{path}\" is larger than declared")),
        })?;
        if produced != member.size {
            return Err(corrupt(&format!("\"{path}\" is shorter than declared")));
        }
        Ok(produced)
    }
}

/// A member copy that could not be written to the archive cache.
fn write_failed(error: &io::Error) -> ArchiveError {
    ArchiveError::new(ArchiveErrorCode::CacheFailed, messages::archive_cache_not_written(&error.to_string()))
}

/// Why `copy_limited` stopped.
pub(crate) enum CopyFailure {
    /// The input failed (a decoder error, or the file could not be read).
    Read(io::Error),
    /// The output failed (the archive cache).
    Write(io::Error),
    /// The input has more than the limit.
    TooMuch,
}

/// Copies everything `input` gives into `out`, refusing more than `limit` bytes; returns the
/// bytes copied. It reads one byte past the limit at most, so a bomb never produces more.
pub(crate) fn copy_limited(mut input: impl Read, limit: u64, out: &mut dyn Write) -> Result<u64, CopyFailure> {
    let mut buffer = vec![0_u8; COPY_STEP];
    let mut copied: u64 = 0;
    loop {
        let read = match input.read(&mut buffer) {
            Ok(0) => return Ok(copied),
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(CopyFailure::Read(error)),
        };
        if copied + read as u64 > limit {
            return Err(CopyFailure::TooMuch);
        }
        out.write_all(&buffer[..read]).map_err(CopyFailure::Write)?;
        copied += read as u64;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    use flate2::write::DeflateEncoder;
    use flate2::Compression;

    /// One member of a hand-made archive.
    struct Spec {
        name: Vec<u8>,
        data: Vec<u8>,
        method: u16,
        flags: u16,
        /// Overrides of the sizes the directory declares.
        declared_size: Option<u32>,
        extra: Vec<u8>,
    }

    fn spec(name: &str, data: &[u8]) -> Spec {
        Spec { name: name.as_bytes().to_vec(), data: data.to_vec(), method: 0, flags: 0, declared_size: None, extra: Vec::new() }
    }

    fn deflated(name: &str, data: &[u8]) -> Spec {
        let mut encoder = DeflateEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(data).unwrap();
        let compressed = encoder.finish().unwrap();
        Spec { method: 8, declared_size: Some(data.len() as u32), ..spec(name, &compressed) }
    }

    /// Writes a zip archive by hand: local headers, central directory, end record.
    fn build(members: &[Spec], prefix: &[u8]) -> Vec<u8> {
        let mut out = prefix.to_vec();
        let mut central = Vec::new();
        for member in members {
            let offset = (out.len() - prefix.len()) as u32;
            let size = member.declared_size.unwrap_or(member.data.len() as u32);
            let compressed = member.data.len() as u32;
            out.extend_from_slice(&SIG_LOCAL.to_le_bytes());
            out.extend_from_slice(&[20, 0]);
            out.extend_from_slice(&member.flags.to_le_bytes());
            out.extend_from_slice(&member.method.to_le_bytes());
            out.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
            out.extend_from_slice(&compressed.to_le_bytes());
            out.extend_from_slice(&size.to_le_bytes());
            out.extend_from_slice(&(member.name.len() as u16).to_le_bytes());
            out.extend_from_slice(&[0, 0]);
            out.extend_from_slice(&member.name);
            out.extend_from_slice(&member.data);

            central.extend_from_slice(&SIG_CENTRAL.to_le_bytes());
            central.extend_from_slice(&[20, 0, 20, 0]);
            central.extend_from_slice(&member.flags.to_le_bytes());
            central.extend_from_slice(&member.method.to_le_bytes());
            central.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&compressed.to_le_bytes());
            central.extend_from_slice(&size.to_le_bytes());
            central.extend_from_slice(&(member.name.len() as u16).to_le_bytes());
            central.extend_from_slice(&(member.extra.len() as u16).to_le_bytes());
            central.extend_from_slice(&[0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            central.extend_from_slice(&offset.to_le_bytes());
            central.extend_from_slice(&member.name);
            central.extend_from_slice(&member.extra);
        }
        let directory_offset = (out.len() - prefix.len()) as u32;
        out.extend_from_slice(&central);
        out.extend_from_slice(&SIG_EOCD.to_le_bytes());
        out.extend_from_slice(&[0, 0, 0, 0]);
        out.extend_from_slice(&(members.len() as u16).to_le_bytes());
        out.extend_from_slice(&(members.len() as u16).to_le_bytes());
        out.extend_from_slice(&(central.len() as u32).to_le_bytes());
        out.extend_from_slice(&directory_offset.to_le_bytes());
        out.extend_from_slice(&[0, 0]);
        out
    }

    fn open(bytes: Vec<u8>) -> Result<ZipReader<Cursor<Vec<u8>>>, ArchiveError> {
        let len = bytes.len() as u64;
        ZipReader::open_with(Cursor::new(bytes), len, 100_000)
    }

    fn paths(reader: &ZipReader<Cursor<Vec<u8>>>) -> Vec<&str> {
        reader.members().iter().map(|member| member.path.as_str()).collect()
    }

    fn copy(reader: &mut ZipReader<Cursor<Vec<u8>>>, index: usize, max: u64) -> Result<Vec<u8>, String> {
        let mut out = Vec::new();
        reader.copy_member(index, max, &mut out).map(|_| out).map_err(|error| error.message)
    }

    #[test]
    fn stored_and_deflated_members_are_read() {
        let text = b"hello hello hello hello hello hello".repeat(20);
        let mut reader = open(build(&[spec("post.json", b"[]"), deflated("posts/a.txt", &text), spec("posts/", b"")], b"")).unwrap();
        assert_eq!(paths(&reader), ["post.json", "posts/a.txt"], "folders are not listed");
        assert_eq!(copy(&mut reader, 0, 100).unwrap(), b"[]");
        assert_eq!(copy(&mut reader, 1, 10_000).unwrap(), text);
        assert_eq!(reader.members()[1].size, text.len() as u64);
    }

    #[test]
    fn names_are_utf8_first_then_cp437_and_the_unicode_field_wins() {
        let mut cp437 = spec("x", b"1");
        cp437.name = vec![b'c', b'a', b'f', 0x82, b'.', b'j', b'p', b'g'];
        let utf8 = spec("caf\u{e9}2.jpg", b"2");
        let mut unicode = spec("legacy.jpg", b"3");
        let name = "\u{65e5}\u{672c}.jpg".as_bytes();
        let mut extra = vec![0x75, 0x70];
        extra.extend_from_slice(&((5 + name.len()) as u16).to_le_bytes());
        extra.extend_from_slice(&[1, 0, 0, 0, 0]);
        extra.extend_from_slice(name);
        unicode.extra = extra;
        let reader = open(build(&[cp437, utf8, unicode], b"")).unwrap();
        assert_eq!(paths(&reader), ["caf\u{e9}.jpg", "caf\u{e9}2.jpg", "\u{65e5}\u{672c}.jpg"]);
    }

    #[test]
    fn zip64_sizes_and_offsets_are_taken_from_the_extra_field() {
        let mut member = spec("big.bin", b"abc");
        let mut extra = vec![0x01, 0x00, 16, 0];
        extra.extend_from_slice(&3_u64.to_le_bytes());
        extra.extend_from_slice(&3_u64.to_le_bytes());
        member.extra = extra;
        let mut bytes = build(&[member], b"");
        // Mark both sizes as "in the ZIP64 field" in the central record.
        let central = bytes.windows(4).position(|window| window == SIG_CENTRAL.to_le_bytes()).unwrap();
        bytes[central + 20..central + 28].copy_from_slice(&[0xff; 8]);
        let mut reader = open(bytes).unwrap();
        assert_eq!(reader.members()[0].size, 3);
        assert_eq!(copy(&mut reader, 0, 10).unwrap(), b"abc");
    }

    #[test]
    fn a_self_extracting_prefix_is_skipped() {
        let mut reader = open(build(&[spec("post.json", b"[1]")], &[0x4d, 0x5a, 0, 0, 1, 2, 3, 4, 5, 6])).unwrap();
        assert_eq!(copy(&mut reader, 0, 10).unwrap(), b"[1]");
    }

    #[test]
    fn overlapping_members_are_refused_as_a_whole() {
        let mut bytes = build(&[spec("a.jpg", b"aaaa"), spec("b.jpg", b"bbbb")], b"");
        // Point the second record at the first member's data.
        let second = bytes.windows(4).enumerate().filter(|(_, window)| *window == SIG_CENTRAL.to_le_bytes()).nth(1).unwrap().0;
        bytes[second + 42..second + 46].copy_from_slice(&0_u32.to_le_bytes());
        let error = open(bytes).unwrap_err();
        assert_eq!(error.message, "The zip file is damaged or incomplete (files overlap).");
    }

    #[test]
    fn member_problems_use_the_clones_sentences() {
        let mut encrypted = spec("posts/secret.jpg", b"x");
        encrypted.flags = 1;
        let mut lzma = spec("posts/packed.jpg", b"y");
        lzma.method = 12;
        let mut reader = open(build(&[encrypted, lzma, spec("posts/big.jpg", b"0123456789")], b"")).unwrap();
        assert_eq!(copy(&mut reader, 0, 10).unwrap_err(), "\"posts/secret.jpg\" is password protected and cannot be read.");
        assert_eq!(
            copy(&mut reader, 1, 10).unwrap_err(),
            "\"posts/packed.jpg\" uses a zip compression method that is not supported (12). Re-create the zip with standard (deflate) compression."
        );
        assert_eq!(copy(&mut reader, 2, 4).unwrap_err(), "\"posts/big.jpg\" is larger than 4 bytes.");
    }

    #[test]
    fn declared_sizes_that_lie_are_damage() {
        let data = b"0123456789".repeat(50);
        let mut smaller = deflated("posts/bomb.jpg", &data);
        smaller.declared_size = Some(100);
        let mut larger = deflated("posts/short.jpg", &data);
        larger.declared_size = Some(900);
        let mut stored = spec("posts/stored.jpg", b"abc");
        stored.declared_size = Some(5);
        let mut reader = open(build(&[smaller, larger, stored], b"")).unwrap();
        assert_eq!(copy(&mut reader, 0, 10_000).unwrap_err(), "The zip file is damaged or incomplete (\"posts/bomb.jpg\" is larger than declared).");
        assert_eq!(copy(&mut reader, 1, 10_000).unwrap_err(), "The zip file is damaged or incomplete (\"posts/short.jpg\" is shorter than declared).");
        assert_eq!(copy(&mut reader, 2, 10_000).unwrap_err(), "The zip file is damaged or incomplete (\"posts/stored.jpg\" has inconsistent sizes).");
    }

    #[test]
    fn broken_archives_are_refused_with_the_clones_details() {
        assert_eq!(open(b"PK\x05\x06".to_vec()).unwrap_err().message, "The zip file is damaged or incomplete (too short).");
        assert_eq!(open(vec![0; 64]).unwrap_err().message, "The zip file is damaged or incomplete (no end-of-archive record).");
        let mut multi = build(&[spec("a.jpg", b"a")], b"");
        let end = multi.len() - 22;
        multi[end + 4] = 1;
        assert_eq!(open(multi).unwrap_err().message, "Multi-part zip files are not supported.");
        let mut truncated = build(&[spec("a.jpg", b"a")], b"");
        let central = truncated.windows(4).position(|window| window == SIG_CENTRAL.to_le_bytes()).unwrap();
        truncated[central + 28] = 200;
        assert_eq!(open(truncated).unwrap_err().message, "The zip file is damaged or incomplete (truncated directory).");
        // A member whose data runs past the end of the file.
        let mut cut = build(&[spec("a.jpg", b"abcdef")], b"");
        let central = cut.windows(4).position(|window| window == SIG_CENTRAL.to_le_bytes()).unwrap();
        cut[central + 20..central + 24].copy_from_slice(&5000_u32.to_le_bytes());
        cut[central + 24..central + 28].copy_from_slice(&5000_u32.to_le_bytes());
        let mut reader = open(cut).unwrap();
        assert_eq!(copy(&mut reader, 0, 10_000).unwrap_err(), "The zip file is damaged or incomplete (\"a.jpg\" is cut off).");
    }

    #[test]
    fn more_members_than_the_cap_are_refused() {
        let members: Vec<Spec> = (0..5).map(|index| spec(&format!("{index}.jpg"), b"x")).collect();
        let bytes = build(&members, b"");
        let len = bytes.len() as u64;
        assert_eq!(ZipReader::open_with(Cursor::new(bytes.clone()), len, 5).unwrap().members().len(), 5);
        let error = ZipReader::open_with(Cursor::new(bytes), len, 4).unwrap_err();
        assert_eq!(error.message, "The zip file contains more than 4 files.");
        assert_eq!(error.code, ArchiveErrorCode::TooLarge);
    }

    #[test]
    fn hostile_names_are_only_keys() {
        let reader = open(build(&[spec("../evil.jpg", b"1"), spec("C:/x/post.json", b"2"), spec("con.jpg", b"3"), spec("a:b.jpg", b"4"), spec("__MACOSX/._a.jpg", b"5")], b"")).unwrap();
        // ".." makes a name unusable; the rest are plain keys (the cache never uses them on disk).
        assert_eq!(paths(&reader), ["C:/x/post.json", "con.jpg", "a:b.jpg"]);
    }
}
