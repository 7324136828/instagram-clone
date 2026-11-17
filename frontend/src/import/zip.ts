/**
 * Random-access ZIP reader.
 *
 * Only the central directory (a few kilobytes at the end of the file) is read up front.
 * Members are then read on demand straight from the File: stored members are zero-copy
 * slices (the caller copies what it keeps) and deflated members are inflated with fflate in
 * small steps. A 200 MB archive is therefore never held in memory as a whole and the tab
 * stays responsive.
 */
import { Inflate } from 'fflate';
import { formatBytes } from './limits';
import { isNoisePath, normalizeEntryPath } from './paths';
import type { SourceEntry } from './source';
import { ImportError } from './types';
import { BlobCollector, decodeName, decodeZipName, describeError, readBytes, throwIfAborted } from './util';

const SIG_LOCAL = 0x04034b50;
const SIG_CENTRAL = 0x02014b50;
const SIG_EOCD = 0x06054b50;
const SIG_EOCD64 = 0x06064b50;
const SIG_EOCD64_LOCATOR = 0x07064b50;
/** Fixed part of a local file header; the member's data follows it (after name and extra field). */
const LOCAL_HEADER_BYTES = 30;

const EOCD_MIN = 22;
const MAX_COMMENT = 0xffff;
const MAX_CENTRAL_DIRECTORY_BYTES = 128 * 1024 * 1024;
/** Compressed bytes read from the file per step. */
const READ_STEP = 1024 * 1024;
/** Compressed bytes handed to the inflater at once; bounds the output of a single call. */
const INFLATE_STEP = 64 * 1024;

function corrupt(detail: string): ImportError {
  return new ImportError('read-failed', `The zip file is damaged or incomplete (${detail}).`);
}

function view(bytes: Uint8Array): DataView {
  return new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
}

function readU64(data: DataView, offset: number): number {
  const low = data.getUint32(offset, true);
  const high = data.getUint32(offset + 4, true);
  if (high > 0x1fffff) throw corrupt('offset out of range');
  return high * 0x100000000 + low;
}

interface ZipMember {
  path: string;
  flags: number;
  method: number;
  compressedSize: number;
  size: number;
  localOffset: number;
}

interface Directory {
  /** Absolute position of the central directory in the file. */
  start: number;
  length: number;
  /** Added to every offset stored in the archive (non-zero for self-extracting prefixes). */
  shift: number;
}

async function locateDirectory(file: Blob, signal: AbortSignal | undefined): Promise<Directory> {
  const size = file.size;
  if (size < EOCD_MIN) throw corrupt('too short');
  const tailLength = Math.min(size, EOCD_MIN + MAX_COMMENT + 20);
  const tailStart = size - tailLength;
  const tail = await readBytes(file, tailStart, size);
  throwIfAborted(signal);
  const data = view(tail);

  let eocd = -1;
  for (let i = tail.length - EOCD_MIN; i >= 0; i--) {
    if (data.getUint32(i, true) === SIG_EOCD && i + EOCD_MIN + data.getUint16(i + 20, true) <= tail.length) {
      eocd = i;
      break;
    }
  }
  if (eocd < 0) throw corrupt('no end-of-archive record');

  let length = data.getUint32(eocd + 12, true);
  let offset = data.getUint32(eocd + 16, true);
  let end = tailStart + eocd;
  const thisDisk = data.getUint16(eocd + 4, true);
  if (thisDisk !== 0xffff && thisDisk !== data.getUint16(eocd + 6, true)) {
    throw new ImportError('read-failed', 'Multi-part zip files are not supported.');
  }

  // ZIP64: the real numbers live in a separate record announced by a locator before the EOCD.
  if (eocd >= 20 && data.getUint32(eocd - 20, true) === SIG_EOCD64_LOCATOR) {
    const declared = readU64(data, eocd - 20 + 8);
    const candidates = [declared, tailStart + eocd - 20 - 56];
    for (const position of candidates) {
      if (position < 0 || position + 56 > size) continue;
      const record = view(await readBytes(file, position, position + 56));
      throwIfAborted(signal);
      if (record.getUint32(0, true) !== SIG_EOCD64) continue;
      length = readU64(record, 40);
      offset = readU64(record, 48);
      end = position;
      break;
    }
  }

  if (length > MAX_CENTRAL_DIRECTORY_BYTES) {
    throw new ImportError('too-large', 'The zip file lists too many files to import.');
  }
  // The directory normally ends where the EOCD starts; any difference to the stored offset
  // means data was prepended to the archive (self-extracting stubs).
  let start = end - length;
  let shift = start - offset;
  if (start < 0 || shift < 0) {
    start = offset;
    shift = 0;
  }
  if (start < 0 || start + length > size) throw corrupt('bad directory position');
  return { start, length, shift };
}

function parseDirectory(bytes: Uint8Array, maxEntries: number): ZipMember[] {
  const data = view(bytes);
  const members: ZipMember[] = [];
  let position = 0;
  while (position + 46 <= bytes.length && data.getUint32(position, true) === SIG_CENTRAL) {
    const flags = data.getUint16(position + 8, true);
    const method = data.getUint16(position + 10, true);
    let compressedSize = data.getUint32(position + 20, true);
    let size = data.getUint32(position + 24, true);
    const nameLength = data.getUint16(position + 28, true);
    const extraLength = data.getUint16(position + 30, true);
    const commentLength = data.getUint16(position + 32, true);
    let localOffset = data.getUint32(position + 42, true);
    const nameStart = position + 46;
    const extraStart = nameStart + nameLength;
    const next = extraStart + extraLength + commentLength;
    if (next > bytes.length) throw corrupt('truncated directory');

    let name: string | undefined;
    for (let cursor = extraStart; cursor + 4 <= extraStart + extraLength; ) {
      const id = data.getUint16(cursor, true);
      const length = data.getUint16(cursor + 2, true);
      const body = cursor + 4;
      if (body + length > extraStart + extraLength) break;
      if (id === 0x0001) {
        // ZIP64: 8-byte values, present only for the fields that overflowed, in this order.
        let field = body;
        if (size === 0xffffffff && field + 8 <= body + length) {
          size = readU64(data, field);
          field += 8;
        }
        if (compressedSize === 0xffffffff && field + 8 <= body + length) {
          compressedSize = readU64(data, field);
          field += 8;
        }
        if (localOffset === 0xffffffff && field + 8 <= body + length) {
          localOffset = readU64(data, field);
        }
      } else if (id === 0x7075 && length > 5 && bytes[body] === 1) {
        // Info-ZIP Unicode Path: the UTF-8 name for archives whose main name is in a legacy charset.
        name = decodeName(bytes.subarray(body + 5, body + length));
      }
      cursor = body + length;
    }
    // Bit 11 marks UTF-8; without it most modern tools still write UTF-8, so try that first.
    name ??= decodeZipName(bytes.subarray(nameStart, extraStart));

    position = next;
    if (name.endsWith('/') || name.endsWith('\\')) continue; // directory
    const path = normalizeEntryPath(name);
    if (!path || isNoisePath(path)) continue;
    members.push({ path, flags, method, compressedSize, size, localOffset });
    if (members.length > maxEntries) {
      throw new ImportError('too-large', `The zip file contains more than ${maxEntries} files.`);
    }
  }
  return members;
}

async function inflateMember(
  file: Blob,
  dataStart: number,
  member: ZipMember,
  maxBytes: number,
  signal: AbortSignal | undefined,
): Promise<Blob> {
  const limit = Math.min(maxBytes, member.size);
  const output = new BlobCollector();
  let produced = 0;
  let overflow = false;
  const inflater = new Inflate((chunk) => {
    produced += chunk.length;
    if (produced > limit) {
      overflow = true;
      throw new Error('overflow');
    }
    output.add(chunk);
  });

  try {
    if (member.compressedSize === 0) inflater.push(new Uint8Array(0), true);
    for (let position = 0; position < member.compressedSize; position += READ_STEP) {
      throwIfAborted(signal);
      const end = Math.min(position + READ_STEP, member.compressedSize);
      const bytes = await readBytes(file, dataStart + position, dataStart + end);
      if (bytes.length !== end - position) throw corrupt('unexpected end of file');
      for (let offset = 0; offset < bytes.length; offset += INFLATE_STEP) {
        const stop = Math.min(offset + INFLATE_STEP, bytes.length);
        inflater.push(bytes.subarray(offset, stop), end === member.compressedSize && stop === bytes.length);
      }
    }
  } catch (err) {
    if (err instanceof ImportError) throw err;
    if (overflow) {
      // More data than the directory promised is corruption (or a bomb); more than the
      // caller's cap is simply too large.
      if (member.size > maxBytes) {
        throw new ImportError('too-large', `"${member.path}" is larger than ${formatBytes(maxBytes)}.`);
      }
      throw corrupt(`"${member.path}" is larger than declared`);
    }
    throw corrupt(`"${member.path}": ${describeError(err)}`);
  }
  if (produced !== member.size) throw corrupt(`"${member.path}" is shorter than declared`);
  return output.finish();
}

/**
 * Every member owns its own stretch of the file. Members that share bytes are how a few
 * kilobytes are made to unpack to gigabytes (one compressed stream listed many times), and no
 * zip tool writes them: such an archive is refused as a whole.
 */
function assertNoOverlap(members: readonly ZipMember[]): void {
  const byOffset = [...members].sort((a, b) => a.localOffset - b.localOffset);
  let end = 0;
  for (const member of byOffset) {
    if (member.localOffset < end) throw corrupt('files overlap');
    end = member.localOffset + LOCAL_HEADER_BYTES + member.compressedSize;
  }
}

function memberEntry(file: Blob, member: ZipMember, shift: number): SourceEntry {
  return {
    path: member.path,
    size: member.size,
    // Deflated members are inflated into memory; stored ones are slices of the file.
    inMemory: member.method !== 0,
    async open(maxBytes, signal) {
      throwIfAborted(signal);
      if (member.flags & 0x1) {
        throw new ImportError('read-failed', `"${member.path}" is password protected and cannot be read.`);
      }
      if (member.method !== 0 && member.method !== 8) {
        throw new ImportError(
          'read-failed',
          `"${member.path}" uses a zip compression method that is not supported (${member.method}). Re-create the zip with standard (deflate) compression.`,
        );
      }
      if (member.size > maxBytes) {
        throw new ImportError('too-large', `"${member.path}" is larger than ${formatBytes(maxBytes)}.`);
      }
      const headerStart = member.localOffset + shift;
      if (headerStart + LOCAL_HEADER_BYTES > file.size) throw corrupt(`"${member.path}" starts beyond the end of the file`);
      const header = view(await readBytes(file, headerStart, headerStart + LOCAL_HEADER_BYTES));
      throwIfAborted(signal);
      if (header.getUint32(0, true) !== SIG_LOCAL) throw corrupt(`"${member.path}" has no local header`);
      const dataStart = headerStart + LOCAL_HEADER_BYTES + header.getUint16(26, true) + header.getUint16(28, true);
      if (dataStart + member.compressedSize > file.size) throw corrupt(`"${member.path}" is cut off`);

      if (member.method === 0) {
        if (member.compressedSize !== member.size) throw corrupt(`"${member.path}" has inconsistent sizes`);
        return file.slice(dataStart, dataStart + member.size);
      }
      return inflateMember(file, dataStart, member, maxBytes, signal);
    },
  };
}

/** Lists the files of a zip archive. Nothing but the central directory is read here. */
export async function readZipEntries(
  file: Blob,
  maxEntries: number,
  signal?: AbortSignal,
): Promise<SourceEntry[]> {
  const directory = await locateDirectory(file, signal);
  const bytes = await readBytes(file, directory.start, directory.start + directory.length);
  throwIfAborted(signal);
  let members = parseDirectory(bytes, maxEntries);
  let shift = directory.shift;
  if (members.length === 0 && directory.shift !== 0 && directory.length > 0) {
    // The "prepended data" guess was wrong (junk between directory and EOCD): trust the offset.
    const start = directory.start - directory.shift;
    members = parseDirectory(await readBytes(file, start, start + directory.length), maxEntries);
    shift = 0;
  }
  assertNoOverlap(members);
  return members.map((member) => memberEntry(file, member, shift));
}
