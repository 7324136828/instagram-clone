/** Archive container detection (magic bytes first, then extension) and opening. */
import type { ImportLimits } from './limits';
import type { SourceEntry } from './source';
import { BlobReader, GunzipReader, readTarEntries } from './tar';
import { ImportError } from './types';
import { readBytes, throwIfAborted } from './util';
import { readZipEntries } from './zip';

export type ArchiveKind = 'zip' | 'tar' | 'gzip';

const ARCHIVE_NAME_RE = /\.(?:zip|tar|tgz|tar\.gz)$/i;
const ARCHIVE_MIME_TYPES = new Set([
  'application/zip',
  'application/x-zip',
  'application/x-zip-compressed',
  'application/x-tar',
  'application/x-gtar',
  'application/gzip',
  'application/x-gzip',
  'application/x-compressed-tar',
]);

/** True when the name or MIME type says "supported archive". */
export function looksLikeArchive(name: string, mimeType: string): boolean {
  return ARCHIVE_NAME_RE.test(name) || ARCHIVE_MIME_TYPES.has(mimeType.toLowerCase());
}

const HOW_TO_FIX = 'Use a .zip, .tar or .tar.gz file, or select the extracted folder instead.';

function unsupported(what: string): ImportError {
  return new ImportError('unsupported-archive', `${what} ${HOW_TO_FIX}`);
}

function matches(bytes: Uint8Array, signature: readonly number[], offset = 0): boolean {
  if (bytes.length < offset + signature.length) return false;
  return signature.every((byte, i) => bytes[offset + i] === byte);
}

const UNSUPPORTED_SIGNATURES: ReadonlyArray<{ name: string; signature: readonly number[] }> = [
  { name: 'RAR', signature: [0x52, 0x61, 0x72, 0x21, 0x1a, 0x07] },
  { name: '7z', signature: [0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c] },
  { name: 'xz', signature: [0xfd, 0x37, 0x7a, 0x58, 0x5a, 0x00] },
  { name: 'bzip2', signature: [0x42, 0x5a, 0x68] },
  { name: 'Zstandard', signature: [0x28, 0xb5, 0x2f, 0xfd] },
];

/** Identifies the container of an archive file. Throws ImportError('unsupported-archive'). */
export async function detectArchiveKind(file: File, signal?: AbortSignal): Promise<ArchiveKind> {
  if (file.size === 0) throw new ImportError('read-failed', `"${file.name}" is empty.`);
  const head = await readBytes(file, 0, Math.min(file.size, 512));
  throwIfAborted(signal);

  if (matches(head, [0x50, 0x4b, 0x03, 0x04]) || matches(head, [0x50, 0x4b, 0x05, 0x06]) || matches(head, [0x50, 0x4b, 0x07, 0x08])) {
    return 'zip';
  }
  if (matches(head, [0x1f, 0x8b])) return 'gzip';
  if (matches(head, [0x75, 0x73, 0x74, 0x61, 0x72], 257)) return 'tar'; // "ustar"

  for (const { name, signature } of UNSUPPORTED_SIGNATURES) {
    if (matches(head, signature)) throw unsupported(`${name} archives are not supported.`);
  }

  // No known signature: fall back to the extension (zip with a prefix, pre-POSIX tar).
  const lower = file.name.toLowerCase();
  if (lower.endsWith('.zip')) return 'zip';
  if (lower.endsWith('.tar')) return 'tar';
  if (/\.(?:rar|7z|xz|bz2|zst|lz|lzma|cab|iso)$/.test(lower)) {
    throw unsupported(`"${file.name}" is an archive type that is not supported.`);
  }
  throw unsupported(`"${file.name}" is not a supported archive.`);
}

export interface OpenArchiveOptions {
  limits: ImportLimits;
  signal?: AbortSignal;
  /** Progress of the indexing pass, in bytes of the archive file. */
  onProgress?: (done: number, total: number) => void;
}

/** Lists the files of an archive without loading it into memory. */
export async function openArchive(file: File, options: OpenArchiveOptions): Promise<SourceEntry[]> {
  const { limits, signal, onProgress } = options;
  const kind = await detectArchiveKind(file, signal);
  if (kind === 'zip') return readZipEntries(file, limits.maxSourceEntries, signal);

  const tarOptions = {
    maxEntries: limits.maxSourceEntries,
    maxMemberBytes: Math.max(limits.maxImageBytes, limits.maxVideoBytes, limits.maxManifestBytes),
    signal,
  };
  if (kind === 'tar') return readTarEntries(new BlobReader(file, onProgress), tarOptions);

  // A gzip stream cannot be searched, so its members are unpacked (into Blobs) in one pass.
  // The cap covers everything the stream may produce, skipped members included.
  const maxOutput = limits.maxTotalMediaBytes + limits.maxManifestBytes + 64 * 1024 * 1024;
  return readTarEntries(new GunzipReader(file, maxOutput, signal, onProgress), tarOptions);
}
