//! The read side of a tar archive, plain or gzip-compressed: a port of the clone's reader
//! (frontend/src/import/tar.ts) with its limits and sentences: ustar with the prefix field (only
//! for POSIX "ustar\0"), GNU long names (`L`; `K` skipped), pax records `path` and `size` (`x`;
//! `g` skipped), signed or unsigned header checksums, base-256 numbers.
//!
//! Two inputs feed the same walk:
//!
//! * `PlainInput`: a .tar file with seeks. The walk only records where each member's data
//!   starts; members are copied afterwards (`copy_member`), and only those post.json needs.
//! * `GzInput`: the gzip stream of a .tar.gz, decoded with `flate2::read::MultiGzDecoder`. A
//!   gzip stream cannot be searched, so the walk hands every member to the caller as it passes
//!   (the archive cache keeps it), and the whole decompressed output is capped.
//!
//! Links, devices and folders carry no data; only types `0` and `7` are files. Two zero blocks
//! end the archive, a missing end marker is fine, and a member that is cut off ends the listing
//! (the complete members before it are kept). Member names are keys only, never file names.

use std::collections::HashMap;
use std::fs::File;
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};

use flate2::read::MultiGzDecoder;

use super::zip::{copy_limited, CopyFailure};
use super::{decode_name, ArchiveError, ArchiveErrorCode};
use crate::manifest::{is_noise_path, normalize_entry_path};
use crate::messages;

/// Tar files are made of blocks of this size.
const BLOCK: usize = 512;
/// GNU long names and pax records longer than this are skipped (tar.ts `MAX_META_BYTES`).
const MAX_META_BYTES: u64 = 1024 * 1024;
/// The largest size a member may declare: the clone's `Number.isSafeInteger`.
const MAX_SAFE_SIZE: u128 = (1 << 53) - 1;
/// Bytes decoded or skipped per step.
const STEP: usize = 64 * 1024;

/// The limits of a walk (the clone's `TarOptions`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TarLimits {
    /// More members than this are refused (`archive_more_files`).
    pub max_entries: usize,
    /// A streamed member larger than this is listed, but its content is not kept.
    pub max_member_bytes: u64,
}

/// One regular file of the archive.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TarEntry {
    /// The normalised path inside the archive.
    pub path: String,
    /// Its size in bytes.
    pub size: u64,
    /// Where its data starts in the (decompressed) tar stream.
    pub data_offset: u64,
    /// A streamed member over `TarLimits::max_member_bytes`: listed, its content not kept.
    pub too_large: bool,
}

/// What the walk reads from.
pub(crate) trait TarInput {
    /// Fills `buffer`; fewer bytes only at the end of the input.
    fn read_full(&mut self, buffer: &mut [u8]) -> Result<usize, ArchiveError>;
    /// Skips up to `length` bytes; fewer only at the end of the input.
    fn skip(&mut self, length: u64) -> Result<u64, ArchiveError>;
    /// Bytes consumed so far.
    fn position(&self) -> u64;
    /// True when skipped members can still be read later (random access).
    fn lazy(&self) -> bool;
}

/// A text of an I/O failure while reading the archive.
fn unreadable(error: &io::Error) -> ArchiveError {
    ArchiveError::new(ArchiveErrorCode::ReadFailed, format!("The file could not be read ({}).", messages::describe_error(&error.to_string())))
}

/// A plain .tar with random access.
pub(crate) struct PlainInput<R> {
    input: R,
    position: u64,
    len: u64,
}

impl<R: Read + Seek> PlainInput<R> {
    /// The tar file `input` of `len` bytes, read from its start.
    pub(crate) fn new(input: R, len: u64) -> PlainInput<R> {
        PlainInput { input, position: 0, len }
    }

    /// Copies the data of `entry` into `out` (tar.ts: a member of a plain tar is a slice of the
    /// file). A member larger than `max_bytes` -> `member_too_large`.
    pub(crate) fn copy_member(&mut self, entry: &TarEntry, max_bytes: u64, out: &mut dyn Write) -> Result<u64, ArchiveError> {
        if entry.size > max_bytes {
            return Err(ArchiveError::new(ArchiveErrorCode::TooLarge, messages::member_too_large(&entry.path, max_bytes)));
        }
        self.input.seek(SeekFrom::Start(entry.data_offset)).map_err(|error| unreadable(&error))?;
        let copied = copy_limited((&mut self.input).take(entry.size), entry.size, out).map_err(|failure| match failure {
            CopyFailure::Read(error) => unreadable(&error),
            CopyFailure::Write(error) => ArchiveError::new(ArchiveErrorCode::CacheFailed, messages::archive_cache_not_written(&error.to_string())),
            CopyFailure::TooMuch => unreadable(&io::Error::other("too much data")),
        })?;
        if copied != entry.size {
            // The file was shortened since it was listed.
            return Err(unreadable(&io::Error::from(io::ErrorKind::UnexpectedEof)));
        }
        Ok(copied)
    }
}

impl<R: Read + Seek> TarInput for PlainInput<R> {
    fn read_full(&mut self, buffer: &mut [u8]) -> Result<usize, ArchiveError> {
        self.input.seek(SeekFrom::Start(self.position)).map_err(|error| unreadable(&error))?;
        let read = read_into(&mut self.input, buffer).map_err(|error| unreadable(&error))?;
        self.position += read as u64;
        Ok(read)
    }

    fn skip(&mut self, length: u64) -> Result<u64, ArchiveError> {
        let target = self.position.saturating_add(length).min(self.len);
        let skipped = target.saturating_sub(self.position);
        self.position = target.max(self.position);
        Ok(skipped)
    }

    fn position(&self) -> u64 {
        self.position
    }

    fn lazy(&self) -> bool {
        true
    }
}

/// Reads until `buffer` is full or the input ends.
fn read_into(input: &mut impl Read, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match input.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}

/// The decompressed stream of a .tar.gz, capped at `max_output` bytes (tar.ts `GunzipReader`).
pub(crate) struct GzInput<R: Read> {
    decoder: MultiGzDecoder<BufReader<R>>,
    produced: u64,
    max_output: u64,
}

impl<R: Read> GzInput<R> {
    /// The gzip stream of `input`; more than `max_output` decompressed bytes are refused with
    /// `archive_unpacks_too_large`.
    pub(crate) fn new(input: R, max_output: u64) -> GzInput<R> {
        GzInput { decoder: MultiGzDecoder::new(BufReader::new(input)), produced: 0, max_output }
    }
}

impl<R: Read> TarInput for GzInput<R> {
    fn read_full(&mut self, buffer: &mut [u8]) -> Result<usize, ArchiveError> {
        // Never ask the decoder for more than one byte past the cap.
        let room = self.max_output.saturating_sub(self.produced).saturating_add(1);
        let wanted = buffer.len().min(usize::try_from(room).unwrap_or(usize::MAX));
        let read = read_into(&mut self.decoder, &mut buffer[..wanted])
            .map_err(|error| ArchiveError::new(ArchiveErrorCode::ReadFailed, messages::archive_damaged(&error.to_string())))?;
        self.produced += read as u64;
        if self.produced > self.max_output {
            return Err(ArchiveError::new(ArchiveErrorCode::TooLarge, messages::archive_unpacks_too_large(self.max_output)));
        }
        Ok(read)
    }

    fn skip(&mut self, length: u64) -> Result<u64, ArchiveError> {
        // Discard in bounded steps so that skipping a huge member never buffers it.
        let mut scratch = vec![0_u8; STEP];
        let mut skipped = 0;
        while skipped < length {
            let step = usize::try_from(length - skipped).unwrap_or(STEP).min(STEP);
            let read = self.read_full(&mut scratch[..step])?;
            if read == 0 {
                break;
            }
            skipped += read as u64;
        }
        Ok(skipped)
    }

    fn position(&self) -> u64 {
        self.produced
    }

    fn lazy(&self) -> bool {
        false
    }
}

/// Opens the gzip stream of a .tar.gz file.
pub(crate) fn gz_input(file: File, max_output: u64) -> GzInput<File> {
    GzInput::new(file, max_output)
}

/// A text field of a header: up to the first NUL, UTF-8 or else Latin-1.
fn field_text(block: &[u8], offset: usize, length: usize) -> String {
    let field = &block[offset..offset + length];
    let end = field.iter().position(|byte| *byte == 0).unwrap_or(field.len());
    decode_name(&field[..end])
}

/// A numeric header field: ASCII octal, or GNU base-256 when the top bit of the first byte is
/// set. Leading spaces and NULs are skipped; the number ends at the first other character.
fn field_number(block: &[u8], offset: usize, length: usize) -> u128 {
    let field = &block[offset..offset + length];
    if field[0] & 0x80 != 0 {
        return field[1..].iter().fold(u128::from(field[0] & 0x7f), |value, byte| value.saturating_mul(256).saturating_add(u128::from(*byte)));
    }
    let mut value: u128 = 0;
    let mut seen = false;
    for code in field {
        if (b'0'..=b'7').contains(code) {
            value = value.saturating_mul(8).saturating_add(u128::from(code - b'0'));
            seen = true;
        } else if seen || (*code != b' ' && *code != 0) {
            break;
        }
    }
    value
}

fn is_zero_block(block: &[u8]) -> bool {
    block.iter().all(|byte| *byte == 0)
}

/// tar.ts `isTarHeader`: the block carries a valid header checksum, counted with unsigned or
/// with signed bytes, and is not all zeros apart from the checksum field.
pub fn is_tar_header(block: &[u8]) -> bool {
    if block.len() < BLOCK {
        return false;
    }
    let mut unsigned: i64 = 0;
    let mut signed: i64 = 0;
    for (index, byte) in block[..BLOCK].iter().enumerate() {
        let byte = if (148..156).contains(&index) { 0x20 } else { *byte };
        unsigned += i64::from(byte);
        signed += i64::from(byte as i8);
    }
    let stored = field_number(block, 148, 8);
    unsigned != 8 * 0x20 && (stored == unsigned as u128 || (signed >= 0 && stored == signed as u128))
}

/// tar.ts `parsePax`: records "<length> <key>=<value>\n".
fn parse_pax(data: &[u8]) -> HashMap<String, String> {
    let mut records = HashMap::new();
    let mut position = 0;
    while position < data.len() {
        let mut cursor = position;
        let mut length: usize = 0;
        while cursor < data.len() && data[cursor].is_ascii_digit() {
            length = length.saturating_mul(10).saturating_add(usize::from(data[cursor] - b'0'));
            cursor += 1;
        }
        if cursor == position || data.get(cursor) != Some(&b' ') || length == 0 || position.saturating_add(length) > data.len() {
            break;
        }
        let record = &data[cursor + 1..position + length];
        if let Some(equals) = record.iter().position(|byte| *byte == b'=').filter(|equals| *equals > 0) {
            let end = if record.last() == Some(&b'\n') { record.len() - 1 } else { record.len() };
            if equals < end {
                records.insert(decode_name(&record[..equals]), decode_name(&record[equals + 1..end]));
            } else {
                records.insert(decode_name(&record[..equals]), String::new());
            }
        }
        position += length;
    }
    records
}

/// The padding after `size` bytes of data up to the next block.
fn padding(size: u64) -> u64 {
    (BLOCK as u64 - size % BLOCK as u64) % BLOCK as u64
}

/// What `walk` hands a streamed member to: the input, the member's position in the listing, its
/// path and size; it returns how many bytes of the member it read.
pub(crate) type MemberContent<'a, I> = dyn FnMut(&mut I, usize, &str, u64) -> Result<u64, ArchiveError> + 'a;

/// tar.ts `readTarEntries`: walks a tar stream and returns its regular files, in order.
///
/// For a lazy input the walk records where each member's data starts and skips it. For a
/// streamed input it calls `content(input, index, path, size)` for each member that is kept
/// (up to `max_member_bytes`); `content` must read the member's data from `input` (all of it,
/// or as much as there is) and return how many bytes it read. A member whose data is cut off
/// ends the walk without being listed.
pub(crate) fn walk<I: TarInput>(input: &mut I, limits: &TarLimits, content: &mut MemberContent<'_, I>) -> Result<Vec<TarEntry>, ArchiveError> {
    let mut entries: Vec<TarEntry> = Vec::new();
    let mut long_name: Option<String> = None;
    let mut pax: Option<HashMap<String, String>> = None;
    let mut zero_blocks = 0;
    let mut first = true;
    let mut header = [0_u8; BLOCK];

    loop {
        if input.read_full(&mut header)? < BLOCK {
            // The end of the input: a missing end marker is tolerated.
            break;
        }
        if is_zero_block(&header) {
            zero_blocks += 1;
            if zero_blocks >= 2 {
                break;
            }
            continue;
        }
        zero_blocks = 0;
        if !is_tar_header(&header) {
            return Err(if first {
                ArchiveError::new(ArchiveErrorCode::Unsupported, messages::NOT_A_TAR)
            } else {
                ArchiveError::new(ArchiveErrorCode::ReadFailed, messages::TAR_BAD_CHECKSUM)
            });
        }
        first = false;

        let kind = if header[156] == 0 { b'0' } else { header[156] };
        let bad_size = || ArchiveError::new(ArchiveErrorCode::ReadFailed, messages::TAR_BAD_SIZE);
        let declared = field_number(&header, 124, 12);
        let mut size = if declared > MAX_SAFE_SIZE { return Err(bad_size()) } else { declared as u64 };
        // Links, devices and folders carry no data, whatever their size field says.
        if b"123456".contains(&kind) {
            size = 0;
        }

        if matches!(kind, b'L' | b'K' | b'x' | b'g') {
            // Metadata for the next member.
            if size > MAX_META_BYTES {
                input.skip(size + padding(size))?;
                continue;
            }
            let mut data = vec![0_u8; size as usize];
            let read = input.read_full(&mut data)?;
            data.truncate(read);
            input.skip(padding(size))?;
            match kind {
                b'L' => long_name = Some(field_text(&data, 0, data.len())),
                b'x' => pax = Some(parse_pax(&data)),
                _ => {}
            }
            continue;
        }

        if let Some(pax_size) = pax.as_ref().and_then(|records| records.get("size")) {
            if !pax_size.is_empty() && pax_size.bytes().all(|byte| byte.is_ascii_digit()) {
                size = pax_size.parse::<u64>().ok().filter(|value| u128::from(*value) <= MAX_SAFE_SIZE).ok_or_else(bad_size)?;
            }
        }
        let data_padding = padding(size);

        let name = match pax.as_ref().and_then(|records| records.get("path").cloned()).or_else(|| long_name.take()) {
            Some(name) => name,
            None => {
                let name = field_text(&header, 0, 100);
                // The prefix field only exists in POSIX ustar ("ustar\0"); GNU tar reuses those bytes.
                let posix = &header[257..262] == b"ustar" && header[262] == 0;
                let prefix = if posix { field_text(&header, 345, 155) } else { String::new() };
                if prefix.is_empty() {
                    name
                } else {
                    format!("{prefix}/{name}")
                }
            }
        };
        long_name = None;
        pax = None;

        let regular = kind == b'0' || kind == b'7';
        let path = if regular && !name.ends_with('/') { normalize_entry_path(&name) } else { None };
        let Some(path) = path.filter(|path| !is_noise_path(path)) else {
            input.skip(size + data_padding)?;
            continue;
        };

        if !input.lazy() && size > limits.max_member_bytes {
            input.skip(size + data_padding)?;
            entries.push(TarEntry { path, size, data_offset: input.position(), too_large: true });
        } else {
            let data_offset = input.position();
            let consumed = if input.lazy() { input.skip(size)? } else { content(input, entries.len(), &path, size)? };
            input.skip(data_padding)?;
            if consumed < size {
                // A cut-off archive: keep what is complete.
                break;
            }
            entries.push(TarEntry { path, size, data_offset, too_large: false });
        }
        if entries.len() > limits.max_entries {
            return Err(ArchiveError::new(ArchiveErrorCode::TooLarge, messages::archive_more_files(limits.max_entries)));
        }
    }
    Ok(entries)
}

/// Lists a plain .tar (`walk` with seeks).
pub(crate) fn list_plain<R: Read + Seek>(input: &mut PlainInput<R>, limits: &TarLimits) -> Result<Vec<TarEntry>, ArchiveError> {
    walk(input, limits, &mut |_, _, _, _| Ok(0))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Cursor;

    use flate2::write::GzEncoder;
    use flate2::Compression;

    const LIMITS: TarLimits = TarLimits { max_entries: 100_000, max_member_bytes: 1024 * 1024 * 1024 };

    /// A header block: `name` (at most 100 bytes), `kind`, `size` in octal; `prefix` goes into
    /// the POSIX prefix field when `posix`.
    pub(crate) fn header(name: &str, kind: u8, size: u64, prefix: &str, posix: bool) -> [u8; BLOCK] {
        let mut block = [0_u8; BLOCK];
        block[..name.len()].copy_from_slice(name.as_bytes());
        block[100..107].copy_from_slice(b"0000644");
        block[124..135].copy_from_slice(format!("{size:011o}").as_bytes());
        block[156] = kind;
        if posix {
            block[257..263].copy_from_slice(b"ustar\0");
            block[263..265].copy_from_slice(b"00");
            block[345..345 + prefix.len()].copy_from_slice(prefix.as_bytes());
        } else {
            block[257..265].copy_from_slice(b"ustar  \0");
        }
        seal(&mut block);
        block
    }

    /// Writes the checksum of a header block.
    pub(crate) fn seal(block: &mut [u8; BLOCK]) {
        block[148..156].copy_from_slice(b"        ");
        let sum: u32 = block.iter().map(|byte| u32::from(*byte)).sum();
        block[148..155].copy_from_slice(format!("{sum:06o}\0").as_bytes());
    }

    /// A member: its header, its data and the padding.
    pub(crate) fn member(name: &str, data: &[u8]) -> Vec<u8> {
        let mut out = header(name, b'0', data.len() as u64, "", true).to_vec();
        out.extend_from_slice(data);
        out.resize(out.len() + padding(data.len() as u64) as usize, 0);
        out
    }

    /// A meta record (`L` or `x`) followed by its member.
    fn with_meta(kind: u8, meta: &[u8], name: &str, data: &[u8]) -> Vec<u8> {
        let mut out = header("././@LongLink", kind, meta.len() as u64, "", false).to_vec();
        out.extend_from_slice(meta);
        out.resize(out.len() + padding(meta.len() as u64) as usize, 0);
        out.extend(member(name, data));
        out
    }

    pub(crate) fn end() -> Vec<u8> {
        vec![0; 2 * BLOCK]
    }

    fn list(bytes: Vec<u8>) -> Result<Vec<TarEntry>, ArchiveError> {
        let len = bytes.len() as u64;
        list_plain(&mut PlainInput::new(Cursor::new(bytes), len), &LIMITS)
    }

    fn paths(entries: &[TarEntry]) -> Vec<&str> {
        entries.iter().map(|entry| entry.path.as_str()).collect()
    }

    #[test]
    fn ustar_prefix_gnu_long_names_and_pax_records_name_the_members() {
        let long = format!("export/{}/photo.jpg", "deep".repeat(30));
        let mut bytes = Vec::new();
        bytes.extend(header("post.json", b'0', 2, "export", true));
        bytes.extend_from_slice(b"[]");
        bytes.resize(BLOCK * 2, 0);
        bytes.extend(with_meta(b'L', format!("{long}\0").as_bytes(), "short-name.jpg", b"x"));
        bytes.extend(with_meta(b'x', b"29 path=export/pax/caf\xc3\xa9.jpg\n", "ignored.jpg", b"y"));
        // The GNU magic reuses the prefix bytes: they are not a prefix there.
        let mut gnu = header("gnu.jpg", b'0', 1, "", false);
        gnu[345..350].copy_from_slice(b"nope/");
        seal(&mut gnu);
        bytes.extend(gnu);
        bytes.extend_from_slice(b"z");
        bytes.resize(bytes.len() + BLOCK - 1, 0);
        bytes.extend(end());
        let entries = list(bytes).unwrap();
        assert_eq!(paths(&entries), ["export/post.json", long.as_str(), "export/pax/caf\u{e9}.jpg", "gnu.jpg"]);
    }

    #[test]
    fn pax_size_and_base_256_sizes_are_read() {
        // A pax record says 700 bytes; the header of the member says 0: the record wins.
        let meta = b"12 size=700\n";
        let mut bytes = header("././@PaxHeader", b'x', meta.len() as u64, "", false).to_vec();
        bytes.extend_from_slice(meta);
        bytes.resize(bytes.len() + padding(meta.len() as u64) as usize, 0);
        bytes.extend(header("big.bin", b'0', 0, "", true));
        bytes.extend(vec![7_u8; 700]);
        bytes.resize(bytes.len() + padding(700) as usize, 0);
        // GNU base-256: the top bit of the first byte, then the number in big-endian bytes.
        let mut base256 = header("base.bin", b'0', 0, "", true);
        base256[124..136].copy_from_slice(&[0x80, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x01, 0x02]);
        seal(&mut base256);
        bytes.extend(base256);
        bytes.extend(vec![1_u8; 0x102]);
        bytes.resize(bytes.len() + padding(0x102) as usize, 0);
        bytes.extend(end());
        let entries = list(bytes).unwrap();
        assert_eq!(paths(&entries), ["big.bin", "base.bin"]);
        assert_eq!(entries[0].size, 700);
        assert_eq!(entries[1].size, 0x102);
    }

    #[test]
    fn a_bad_checksum_is_not_a_tar_first_and_damage_later() {
        let mut broken = header("a.jpg", b'0', 0, "", true);
        broken[0] = b'b';
        let error = list([broken.to_vec(), end()].concat()).unwrap_err();
        assert_eq!(error.message, "The file is not a tar archive. Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.");
        assert_eq!(error.code, ArchiveErrorCode::Unsupported);
        let error = list([member("ok.jpg", b"1"), broken.to_vec(), end()].concat()).unwrap_err();
        assert_eq!(error.message, "The tar archive is damaged (bad header checksum).");
        let mut bad_size = header("huge.jpg", b'0', 0, "", true);
        bad_size[124..136].copy_from_slice(&[0xff; 12]);
        seal(&mut bad_size);
        assert_eq!(list([bad_size.to_vec(), end()].concat()).unwrap_err().message, "The tar archive is damaged (bad size).");
    }

    #[test]
    fn a_cut_off_member_ends_the_listing_and_a_missing_end_marker_is_fine() {
        let mut cut = [member("one.jpg", b"1111"), member("two.jpg", b"2222")].concat();
        assert_eq!(paths(&list(cut.clone()).unwrap()), ["one.jpg", "two.jpg"], "no end marker");
        cut.extend(header("three.jpg", b'0', 4000, "", true));
        cut.extend_from_slice(&[3_u8; 100]);
        assert_eq!(paths(&list(cut).unwrap()), ["one.jpg", "two.jpg"]);
    }

    #[test]
    fn links_devices_and_folders_carry_no_data() {
        let mut bytes = Vec::new();
        bytes.extend(header("folder/", b'5', 0, "", true));
        let mut link = header("link.jpg", b'2', 0, "", true);
        link[157..167].copy_from_slice(b"/etc/passw");
        seal(&mut link);
        bytes.extend(link);
        // A hard link that claims a size: the size is ignored, the next header follows at once.
        bytes.extend(header("hard.jpg", b'1', 1024, "", true));
        bytes.extend(member("real.jpg", b"r"));
        bytes.extend(member("../escape.jpg", b"e"));
        bytes.extend(member("._noise.jpg", b"n"));
        bytes.extend(end());
        assert_eq!(paths(&list(bytes).unwrap()), ["real.jpg"]);
    }

    #[test]
    fn more_members_than_the_cap_are_refused() {
        let bytes: Vec<u8> = (0..4).flat_map(|index| member(&format!("{index}.jpg"), b"x")).chain(end()).collect();
        let len = bytes.len() as u64;
        let limits = TarLimits { max_entries: 3, ..LIMITS };
        let error = list_plain(&mut PlainInput::new(Cursor::new(bytes), len), &limits).unwrap_err();
        assert_eq!(error.message, "The archive contains more than 3 files.");
    }

    fn gzip(bytes: &[u8]) -> Vec<u8> {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
    }

    fn stream(bytes: Vec<u8>, max_output: u64, limits: &TarLimits) -> Result<Vec<(String, Vec<u8>)>, ArchiveError> {
        let mut input = GzInput::new(Cursor::new(bytes), max_output);
        let mut kept: Vec<(String, Vec<u8>)> = Vec::new();
        walk(&mut input, limits, &mut |input, _, path, size| {
            let mut data = vec![0_u8; size as usize];
            let read = input.read_full(&mut data)?;
            data.truncate(read);
            kept.push((path.to_owned(), data));
            Ok(read as u64)
        })?;
        Ok(kept)
    }

    #[test]
    fn a_gzip_stream_hands_over_every_member_once() {
        let tar = [member("post.json", b"[]"), member("posts/a.jpg", b"aaaa"), end()].concat();
        let kept = stream(gzip(&tar), 1 << 30, &LIMITS).unwrap();
        assert_eq!(kept, [("post.json".to_owned(), b"[]".to_vec()), ("posts/a.jpg".to_owned(), b"aaaa".to_vec())]);
        // A member over the per-member cap is listed but not handed over.
        let limits = TarLimits { max_member_bytes: 3, ..LIMITS };
        let mut input = GzInput::new(Cursor::new(gzip(&tar)), 1 << 30);
        let entries = walk(&mut input, &limits, &mut |input, _, _, size| input.skip(size)).unwrap();
        assert_eq!(entries.iter().map(|entry| entry.too_large).collect::<Vec<_>>(), [false, true]);
    }

    #[test]
    fn the_output_of_a_gzip_stream_is_capped() {
        let tar = [member("posts/a.jpg", &[9_u8; 4096]), end()].concat();
        let error = stream(gzip(&tar), 2048, &LIMITS).unwrap_err();
        assert_eq!(error.message, "The archive unpacks to more than 2 KB, which is too large to import.");
        assert_eq!(error.code, ArchiveErrorCode::TooLarge);
    }

    #[test]
    fn a_damaged_gzip_stream_is_reported() {
        let tar = [member("posts/a.jpg", &[9_u8; 40_000]), end()].concat();
        let mut compressed = gzip(&tar);
        let middle = compressed.len() / 2;
        for byte in &mut compressed[middle..middle + 16] {
            *byte ^= 0x5a;
        }
        let error = stream(compressed, 1 << 30, &LIMITS).unwrap_err();
        assert!(error.message.starts_with("The archive is damaged or incomplete ("), "{}", error.message);
    }
}
