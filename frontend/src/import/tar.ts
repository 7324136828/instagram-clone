/**
 * TAR reader (ustar with prefix field, GNU long names, pax path/size records) on top of a
 * small sequential byte reader. Two readers exist:
 *
 *  - BlobReader: random access over an uncompressed .tar. Member data is never read here;
 *    members become zero-copy slices of the file (the caller copies what it keeps).
 *  - GunzipReader: streaming gunzip (fflate) of a .tar.gz, pulled chunk by chunk so that only
 *    the member currently being copied is in memory.
 */
import { Gunzip } from 'fflate';
import { formatBytes } from './limits';
import { isNoisePath, normalizeEntryPath } from './paths';
import type { SourceEntry } from './source';
import { ImportError } from './types';
import { BlobCollector, decodeName, describeError, readBytes, throwIfAborted } from './util';

const BLOCK = 512;
const MAX_META_BYTES = 1024 * 1024;

export interface ByteReader {
  /** Up to `length` bytes; fewer only at the end of the input. */
  read(length: number): Promise<Uint8Array>;
  skip(length: number): Promise<void>;
  /** The next `length` bytes as a Blob (shorter at the end of the input). */
  blob(length: number): Promise<Blob>;
  /** True when skipped members can still be read later (random access). */
  readonly lazy: boolean;
}

/** Random access over an uncompressed tar file. */
export class BlobReader implements ByteReader {
  readonly lazy = true;
  private position = 0;
  private windowStart = 0;
  private window: Uint8Array | null = null;
  private readonly source: Blob;
  private readonly onProgress: ((done: number, total: number) => void) | undefined;

  constructor(source: Blob, onProgress?: (done: number, total: number) => void) {
    this.source = source;
    this.onProgress = onProgress;
  }

  async read(length: number): Promise<Uint8Array> {
    const start = this.position;
    const end = Math.min(start + length, this.source.size);
    if (end <= start) return new Uint8Array(0);
    // Headers come in runs of a few blocks: read a small window instead of one block at a time.
    const cached = this.window;
    if (!cached || start < this.windowStart || end > this.windowStart + cached.length) {
      const windowEnd = Math.min(Math.max(end, start + 8 * BLOCK), this.source.size);
      this.window = await readBytes(this.source, start, windowEnd);
      this.windowStart = start;
    }
    this.position = end;
    this.onProgress?.(end, this.source.size);
    const offset = start - this.windowStart;
    return (this.window as Uint8Array).subarray(offset, offset + (end - start));
  }

  async skip(length: number): Promise<void> {
    this.position = Math.min(this.position + length, this.source.size);
  }

  async blob(length: number): Promise<Blob> {
    const start = this.position;
    const end = Math.min(start + length, this.source.size);
    this.position = end;
    return this.source.slice(start, end);
  }
}

/** Compressed bytes read from the file per step. */
const GZIP_READ_STEP = 1024 * 1024;
/** Compressed bytes handed to the inflater at once; bounds the output of a single call. */
const GZIP_PUSH_STEP = 64 * 1024;

/** Pull-based streaming gunzip. */
export class GunzipReader implements ByteReader {
  readonly lazy = false;
  private readonly chunks: Uint8Array[] = [];
  private headOffset = 0;
  private buffered = 0;
  private produced = 0;
  private inputPosition = 0;
  private pending: Uint8Array | null = null;
  private pendingOffset = 0;
  private finished = false;
  private readonly gunzip: Gunzip;
  private readonly source: Blob;
  private readonly maxOutputBytes: number;
  private readonly signal: AbortSignal | undefined;
  private readonly onProgress: ((done: number, total: number) => void) | undefined;

  constructor(
    source: Blob,
    maxOutputBytes: number,
    signal?: AbortSignal,
    onProgress?: (done: number, total: number) => void,
  ) {
    this.source = source;
    this.maxOutputBytes = maxOutputBytes;
    this.signal = signal;
    this.onProgress = onProgress;
    this.gunzip = new Gunzip((chunk) => {
      if (chunk.length === 0) return;
      this.produced += chunk.length;
      if (this.produced > this.maxOutputBytes) {
        throw new ImportError(
          'too-large',
          `The archive unpacks to more than ${formatBytes(this.maxOutputBytes)}, which is too large to import.`,
        );
      }
      this.chunks.push(chunk);
      this.buffered += chunk.length;
    });
  }

  /** Decompresses until `needed` bytes are buffered or the input ends. */
  private async fill(needed: number): Promise<void> {
    while (this.buffered < needed && !this.finished) {
      throwIfAborted(this.signal);
      if (!this.pending || this.pendingOffset >= this.pending.length) {
        if (this.inputPosition >= this.source.size) {
          this.finished = true;
          break;
        }
        const end = Math.min(this.inputPosition + GZIP_READ_STEP, this.source.size);
        this.pending = await readBytes(this.source, this.inputPosition, end);
        this.pendingOffset = 0;
        this.inputPosition = end;
        if (this.pending.length === 0) {
          this.finished = true;
          break;
        }
      }
      const stop = Math.min(this.pendingOffset + GZIP_PUSH_STEP, this.pending.length);
      const last = stop === this.pending.length && this.inputPosition >= this.source.size;
      const piece = this.pending.subarray(this.pendingOffset, stop);
      this.pendingOffset = stop;
      try {
        this.gunzip.push(piece, last);
      } catch (err) {
        if (err instanceof ImportError) throw err;
        throw new ImportError(
          'read-failed',
          `The archive is damaged or incomplete (${describeError(err)}).`,
        );
      }
      if (last) this.finished = true;
      this.onProgress?.(this.inputPosition - (this.pending.length - stop), this.source.size);
    }
  }

  /** Removes up to `length` buffered bytes and returns them as views (no copying). */
  private async take(length: number): Promise<Uint8Array[]> {
    await this.fill(length);
    const parts: Uint8Array[] = [];
    let remaining = Math.min(length, this.buffered);
    this.buffered -= remaining;
    while (remaining > 0) {
      const head = this.chunks[0];
      const available = head.length - this.headOffset;
      if (available <= remaining) {
        parts.push(this.headOffset === 0 ? head : head.subarray(this.headOffset));
        this.chunks.shift();
        this.headOffset = 0;
        remaining -= available;
      } else {
        parts.push(head.subarray(this.headOffset, this.headOffset + remaining));
        this.headOffset += remaining;
        remaining = 0;
      }
    }
    return parts;
  }

  async read(length: number): Promise<Uint8Array> {
    const parts = await this.take(length);
    if (parts.length === 1) return parts[0];
    const out = new Uint8Array(parts.reduce((sum, part) => sum + part.length, 0));
    let offset = 0;
    for (const part of parts) {
      out.set(part, offset);
      offset += part.length;
    }
    return out;
  }

  async skip(length: number): Promise<void> {
    // Discard in bounded steps so that skipping a huge member never buffers it.
    let remaining = length;
    while (remaining > 0) {
      const parts = await this.take(Math.min(remaining, GZIP_READ_STEP));
      const got = parts.reduce((sum, part) => sum + part.length, 0);
      if (got === 0) break;
      remaining -= got;
    }
  }

  async blob(length: number): Promise<Blob> {
    const output = new BlobCollector();
    let remaining = length;
    while (remaining > 0) {
      // Bounded steps: at most one step of decompressed data lives outside the Blob parts.
      const taken = await this.take(Math.min(remaining, 4 * GZIP_READ_STEP));
      const got = taken.reduce((sum, part) => sum + part.length, 0);
      if (got === 0) break;
      for (const part of taken) output.add(part);
      remaining -= got;
    }
    return output.finish();
  }
}

function fieldText(block: Uint8Array, offset: number, length: number): string {
  let end = offset;
  while (end < offset + length && block[end] !== 0) end++;
  return decodeName(block.subarray(offset, end));
}

/** Numeric header field: ASCII octal, or GNU base-256 when the top bit of the first byte is set. */
function fieldNumber(block: Uint8Array, offset: number, length: number): number {
  if (block[offset] & 0x80) {
    let value = block[offset] & 0x7f;
    for (let i = offset + 1; i < offset + length; i++) value = value * 256 + block[i];
    return value;
  }
  let value = 0;
  let seen = false;
  for (let i = offset; i < offset + length; i++) {
    const code = block[i];
    if (code >= 0x30 && code <= 0x37) {
      value = value * 8 + (code - 0x30);
      seen = true;
    } else if (seen || (code !== 0x20 && code !== 0)) {
      break;
    }
  }
  return value;
}

function isZeroBlock(block: Uint8Array): boolean {
  for (let i = 0; i < block.length; i++) if (block[i] !== 0) return false;
  return true;
}

/** True when the 512-byte block carries a valid tar header checksum. */
export function isTarHeader(block: Uint8Array): boolean {
  if (block.length < BLOCK) return false;
  let unsigned = 0;
  let signed = 0;
  for (let i = 0; i < BLOCK; i++) {
    const byte = i >= 148 && i < 156 ? 0x20 : block[i];
    unsigned += byte;
    signed += byte > 127 ? byte - 256 : byte;
  }
  const stored = fieldNumber(block, 148, 8);
  return unsigned !== 8 * 0x20 && (stored === unsigned || stored === signed);
}

/** Parses pax extended header records ("<length> <key>=<value>\n"). */
function parsePax(data: Uint8Array): Map<string, string> {
  const records = new Map<string, string>();
  let position = 0;
  while (position < data.length) {
    let cursor = position;
    let length = 0;
    while (cursor < data.length && data[cursor] >= 0x30 && data[cursor] <= 0x39) {
      length = length * 10 + (data[cursor] - 0x30);
      cursor++;
    }
    if (cursor === position || data[cursor] !== 0x20 || length <= 0 || position + length > data.length) break;
    const record = data.subarray(cursor + 1, position + length);
    const equals = record.indexOf(0x3d);
    if (equals > 0) {
      let end = record.length;
      if (record[end - 1] === 0x0a) end--;
      records.set(decodeName(record.subarray(0, equals)), decodeName(record.subarray(equals + 1, end)));
    }
    position += length;
  }
  return records;
}

export interface TarOptions {
  maxEntries: number;
  /** Members larger than this are listed but their content is not kept (streaming readers). */
  maxMemberBytes: number;
  signal?: AbortSignal;
}

function tooLargeEntry(path: string, size: number, maxBytes: number): SourceEntry {
  return {
    path,
    size,
    open: () =>
      Promise.reject(new ImportError('too-large', `"${path}" is larger than ${formatBytes(maxBytes)}.`)),
  };
}

/** Walks a tar stream and returns its regular files. */
export async function readTarEntries(reader: ByteReader, options: TarOptions): Promise<SourceEntry[]> {
  const entries: SourceEntry[] = [];
  let longName: string | undefined;
  let pax: Map<string, string> | undefined;
  let zeroBlocks = 0;
  let first = true;

  for (;;) {
    throwIfAborted(options.signal);
    const header = await reader.read(BLOCK);
    if (header.length < BLOCK) break; // end of input (a missing end marker is tolerated)
    if (isZeroBlock(header)) {
      if (++zeroBlocks >= 2) break;
      continue;
    }
    zeroBlocks = 0;
    if (!isTarHeader(header)) {
      if (first) {
        throw new ImportError(
          'unsupported-archive',
          'The file is not a tar archive. Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.',
        );
      }
      throw new ImportError('read-failed', 'The tar archive is damaged (bad header checksum).');
    }
    first = false;

    const type = String.fromCharCode(header[156] || 0x30);
    let size = fieldNumber(header, 124, 12);
    if (!Number.isSafeInteger(size) || size < 0) throw new ImportError('read-failed', 'The tar archive is damaged (bad size).');
    // Links, devices and directories carry no data, whatever their size field says.
    if ('123456'.includes(type)) size = 0;
    const padding = (BLOCK - (size % BLOCK)) % BLOCK;

    if (type === 'L' || type === 'K' || type === 'x' || type === 'g') {
      // Metadata for the next member.
      if (size > MAX_META_BYTES) {
        await reader.skip(size + padding);
        continue;
      }
      const data = await reader.read(size);
      await reader.skip(padding);
      if (type === 'L') longName = fieldText(data, 0, data.length);
      else if (type === 'x') pax = parsePax(data);
      continue;
    }

    const paxSize = pax?.get('size');
    if (paxSize !== undefined && /^\d+$/.test(paxSize)) size = Number(paxSize);
    const dataPadding = (BLOCK - (size % BLOCK)) % BLOCK;

    let name = pax?.get('path') ?? longName;
    if (name === undefined) {
      name = fieldText(header, 0, 100);
      // The prefix field only exists in POSIX ustar ("ustar\0"); GNU tar reuses those bytes.
      const posix = header[257] === 0x75 && fieldText(header, 257, 6) === 'ustar' && header[262] === 0;
      const prefix = posix ? fieldText(header, 345, 155) : '';
      if (prefix) name = `${prefix}/${name}`;
    }
    longName = undefined;
    pax = undefined;

    const regular = type === '0' || type === '7';
    const path = regular && !name.endsWith('/') ? normalizeEntryPath(name) : null;
    if (!path || isNoisePath(path)) {
      await reader.skip(size + dataPadding);
      continue;
    }

    if (!reader.lazy && size > options.maxMemberBytes) {
      await reader.skip(size + dataPadding);
      entries.push(tooLargeEntry(path, size, options.maxMemberBytes));
    } else {
      const content = await reader.blob(size);
      await reader.skip(dataPadding);
      if (content.size < size) break; // cut-off archive: keep what is complete
      const memberSize = size;
      entries.push({
        path,
        size: memberSize,
        // A gunzipped member lives in memory; a member of a plain tar is a slice of the file.
        inMemory: !reader.lazy,
        open: (maxBytes) =>
          memberSize > maxBytes
            ? Promise.reject(new ImportError('too-large', `"${path}" is larger than ${formatBytes(maxBytes)}.`))
            : Promise.resolve(content),
      });
    }
    if (entries.length > options.maxEntries) {
      throw new ImportError('too-large', `The archive contains more than ${options.maxEntries} files.`);
    }
  }
  return entries;
}
