/** Image and video type detection by magic bytes. File extensions are never trusted. */

export type ImageFormat = 'jpeg' | 'png' | 'gif' | 'webp' | 'avif' | 'bmp' | 'heic';

export interface SniffedImage {
  format: ImageFormat;
  mime: string;
}

/** mp4 covers MP4 and M4V (and every other ISO-BMFF brand that is not an image or audio brand). */
export type VideoFormat = 'mp4' | 'mov' | 'webm';

export interface SniffedVideo {
  format: VideoFormat;
  mime: string;
}

export type SniffedMedia = ({ kind: 'image' } & SniffedImage) | ({ kind: 'video' } & SniffedVideo);

/** Number of leading bytes the sniffers want to see (enough for a long ftyp brand list). */
export const SNIFF_BYTES = 256;

const HEVC_BRANDS = new Set(['heic', 'heix', 'hevc', 'hevx', 'heim', 'heis', 'hevm', 'hevs']);
const HEIF_GENERIC_BRANDS = new Set(['mif1', 'msf1']);
const AVIF_BRANDS = new Set(['avif', 'avis']);
/** ISO-BMFF brands of audio-only files (iTunes audio, audio books, Flash audio). */
const AUDIO_BRANDS = new Set(['M4A ', 'M4B ', 'M4P ', 'F4A ', 'F4B ']);

function ascii(bytes: Uint8Array, start: number, length: number): string {
  let out = '';
  for (let i = start; i < start + length && i < bytes.length; i++) out += String.fromCharCode(bytes[i]);
  return out;
}

function startsWith(bytes: Uint8Array, signature: readonly number[], offset = 0): boolean {
  if (bytes.length < offset + signature.length) return false;
  for (let i = 0; i < signature.length; i++) {
    if (bytes[offset + i] !== signature[i]) return false;
  }
  return true;
}

interface FileTypeBox {
  major: string;
  compatible: string[];
}

/** The `ftyp` box an ISO-BMFF file (MP4, MOV, HEIF, AVIF) starts with, or null. */
function fileTypeBox(bytes: Uint8Array): FileTypeBox | null {
  if (bytes.length < 12 || ascii(bytes, 4, 4) !== 'ftyp') return null;
  const declared = ((bytes[0] << 24) | (bytes[1] << 16) | (bytes[2] << 8) | bytes[3]) >>> 0;
  const boxEnd = Math.min(bytes.length, declared >= 16 ? declared : bytes.length);
  const compatible: string[] = [];
  for (let offset = 16; offset + 4 <= boxEnd; offset += 4) compatible.push(ascii(bytes, offset, 4));
  return { major: ascii(bytes, 8, 4), compatible };
}

function imageBrand({ major, compatible }: FileTypeBox): SniffedImage | null {
  if (AVIF_BRANDS.has(major)) return { format: 'avif', mime: 'image/avif' };
  if (HEVC_BRANDS.has(major)) return { format: 'heic', mime: 'image/heic' };
  // Generic HEIF majors (mif1/msf1) and unknown majors are decided by the compatible brands.
  if (compatible.some((brand) => AVIF_BRANDS.has(brand))) return { format: 'avif', mime: 'image/avif' };
  if (compatible.some((brand) => HEVC_BRANDS.has(brand))) return { format: 'heic', mime: 'image/heic' };
  if (HEIF_GENERIC_BRANDS.has(major)) return { format: 'heic', mime: 'image/heif' };
  return null;
}

/** A brand is four printable ASCII characters; anything else is not an ISO-BMFF file. */
function isBrand(text: string): boolean {
  return /^[\x20-\x7e]{4}$/.test(text);
}

/**
 * Identifies an image from its first bytes (pass at least SNIFF_BYTES when available).
 * Returns null for anything that is not a supported raster image, including SVG and video.
 */
export function sniffImage(bytes: Uint8Array): SniffedImage | null {
  if (startsWith(bytes, [0xff, 0xd8, 0xff])) return { format: 'jpeg', mime: 'image/jpeg' };
  if (startsWith(bytes, [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a])) return { format: 'png', mime: 'image/png' };
  if (bytes.length >= 6) {
    const head = ascii(bytes, 0, 6);
    if (head === 'GIF87a' || head === 'GIF89a') return { format: 'gif', mime: 'image/gif' };
  }
  if (bytes.length >= 12 && ascii(bytes, 0, 4) === 'RIFF' && ascii(bytes, 8, 4) === 'WEBP') {
    return { format: 'webp', mime: 'image/webp' };
  }
  const box = fileTypeBox(bytes);
  if (box) return imageBrand(box);
  if (bytes.length >= 18 && bytes[0] === 0x42 && bytes[1] === 0x4d) {
    // "BM" alone is weak; also require zeroed reserved fields and a known DIB header size.
    const reservedZero = bytes[6] === 0 && bytes[7] === 0 && bytes[8] === 0 && bytes[9] === 0;
    const dibSize = bytes[14] | (bytes[15] << 8) | (bytes[16] << 16) | (bytes[17] << 24);
    if (reservedZero && [12, 40, 52, 56, 64, 108, 124].includes(dibSize)) return { format: 'bmp', mime: 'image/bmp' };
  }
  return null;
}

const EBML_MAGIC = [0x1a, 0x45, 0xdf, 0xa3] as const;
const EBML_DOCTYPE_ID = 0x4282;

/** Length in bytes of an EBML variable-size integer, from its first byte (0 when invalid). */
function vintLength(first: number): number {
  for (let length = 1; length <= 8; length++) {
    if (first & (0x80 >> (length - 1))) return length;
  }
  return 0;
}

/**
 * The DocType of an EBML header ("webm", "matroska"), "" when the header has none (the
 * specification's default is then "matroska"), or null when the bytes are not EBML at all.
 */
function ebmlDocType(bytes: Uint8Array): string | null {
  if (!startsWith(bytes, EBML_MAGIC)) return null;
  const sizeLength = vintLength(bytes[4] ?? 0);
  if (sizeLength === 0) return null;
  let size = (bytes[4] ?? 0) & (0xff >> sizeLength);
  for (let i = 1; i < sizeLength; i++) size = size * 256 + (bytes[4 + i] ?? 0);
  let position = 4 + sizeLength;
  const end = Math.min(bytes.length, position + size);
  while (position < end) {
    const idLength = vintLength(bytes[position]);
    if (idLength === 0 || idLength > 4 || position + idLength >= end) break;
    let id = 0;
    for (let i = 0; i < idLength; i++) id = id * 256 + bytes[position + i];
    position += idLength;
    const lengthOfSize = vintLength(bytes[position]);
    if (lengthOfSize === 0 || position + lengthOfSize > end) break;
    let length = bytes[position] & (0xff >> lengthOfSize);
    for (let i = 1; i < lengthOfSize; i++) length = length * 256 + bytes[position + i];
    position += lengthOfSize;
    if (id === EBML_DOCTYPE_ID) return ascii(bytes, position, Math.min(length, 32)).replace(/\0+$/, '');
    position += length;
  }
  return '';
}

/**
 * Identifies a supported image or video from its first bytes (pass at least SNIFF_BYTES when
 * available). Videos: MP4/M4V/MOV (an ISO-BMFF `ftyp` box with a brand that is neither an image
 * nor an audio brand) and WebM (EBML with DocType "webm"). Returns null for anything else.
 */
export function sniffMedia(bytes: Uint8Array): SniffedMedia | null {
  const image = sniffImage(bytes);
  if (image) return { kind: 'image', ...image };
  const box = fileTypeBox(bytes);
  if (box && isBrand(box.major) && !AUDIO_BRANDS.has(box.major)) {
    return box.major === 'qt  '
      ? { kind: 'video', format: 'mov', mime: 'video/quicktime' }
      : { kind: 'video', format: 'mp4', mime: 'video/mp4' };
  }
  if (ebmlDocType(bytes) === 'webm') return { kind: 'video', format: 'webm', mime: 'video/webm' };
  return null;
}

/**
 * Names a recognised video (or audio) container that the import does not support, e.g.
 * "a Matroska (MKV) video", so that the warning can say what the file is. Null otherwise.
 * Only meaningful for bytes that sniffMedia rejected.
 */
export function describeUnsupportedMedia(bytes: Uint8Array): string | null {
  const docType = ebmlDocType(bytes);
  if (docType !== null && docType !== 'webm') return 'a Matroska (MKV) video';
  const box = fileTypeBox(bytes);
  if (box && AUDIO_BRANDS.has(box.major)) return 'an audio file (M4A)';
  if (bytes.length >= 12 && ascii(bytes, 0, 4) === 'RIFF' && ascii(bytes, 8, 4) === 'AVI ') return 'an AVI video';
  if (startsWith(bytes, [0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11])) return 'a Windows Media (WMV/ASF) video';
  if (startsWith(bytes, [0x46, 0x4c, 0x56, 0x01])) return 'a Flash (FLV) video';
  if (startsWith(bytes, [0x00, 0x00, 0x01, 0xba]) || startsWith(bytes, [0x00, 0x00, 0x01, 0xb3])) return 'an MPEG video';
  // MPEG transport stream: a sync byte every 188 bytes.
  if (bytes.length > 188 && bytes[0] === 0x47 && bytes[188] === 0x47) return 'an MPEG transport stream (TS) video';
  if (ascii(bytes, 0, 4) === 'OggS') return 'an Ogg file';
  return null;
}
