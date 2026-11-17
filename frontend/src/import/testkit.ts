/** Helpers for the unit tests: tiny image and video fixtures, an in-memory tar writer, File factories. */
import { strToU8, zipSync, type Zippable } from 'fflate';

function bytesOf(...parts: Array<string | number[]>): Uint8Array<ArrayBuffer> {
  const chunks = parts.map((part) => (typeof part === 'string' ? Array.from(strToU8(part, true)) : part));
  return new Uint8Array(chunks.flat());
}

function padded(head: Uint8Array, length: number, fill: number): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(Math.max(length, head.length)).fill(fill);
  out.set(head);
  return out;
}

function ftyp(major: string, compatible: string[]): Uint8Array<ArrayBuffer> {
  const size = 16 + compatible.length * 4;
  return bytesOf([0, 0, 0, size], 'ftyp', major, [0, 0, 0, 0], compatible.join(''));
}

/** Byte sequences that start like real images (enough for magic-byte sniffing). */
export const IMAGES = {
  jpeg: (fill = 1, length = 64) => padded(bytesOf([0xff, 0xd8, 0xff, 0xe0, 0x00, 0x10], 'JFIF\0'), length, fill),
  png: (fill = 2, length = 64) => padded(bytesOf([0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a]), length, fill),
  gif: (fill = 3, length = 64) => padded(bytesOf('GIF89a'), length, fill),
  webp: (fill = 4, length = 64) => padded(bytesOf('RIFF', [0x24, 0, 0, 0], 'WEBPVP8 '), length, fill),
  bmp: (fill = 5, length = 64) =>
    padded(bytesOf('BM', [0x40, 0, 0, 0], [0, 0, 0, 0], [0x36, 0, 0, 0], [40, 0, 0, 0]), length, fill),
  avif: (fill = 6, length = 64) => padded(ftyp('avif', ['avif', 'mif1', 'miaf']), length, fill),
  heic: (fill = 7, length = 64) => padded(ftyp('heic', ['mif1', 'heic']), length, fill),
  heifMif1: (fill = 8, length = 64) => padded(ftyp('mif1', ['mif1', 'heic']), length, fill),
  svg: () => strToU8('<svg xmlns="http://www.w3.org/2000/svg" onload="alert(1)"></svg>'),
};

/** An EBML header (the start of WebM and Matroska files) with the given DocType. */
function ebml(docType: string): Uint8Array<ArrayBuffer> {
  const docTypeElement = bytesOf([0x42, 0x82, 0x80 | docType.length], docType);
  // EBMLVersion 1, then the DocType; the header size is a one-byte variable-size integer.
  const body = bytesOf([0x42, 0x86, 0x81, 0x01], Array.from(docTypeElement));
  return bytesOf([0x1a, 0x45, 0xdf, 0xa3, 0x80 | body.length], Array.from(body));
}

/** Byte sequences that start like real videos (enough for magic-byte sniffing). */
export const VIDEOS = {
  mp4: (fill = 21, length = 64) => padded(ftyp('isom', ['isom', 'iso2', 'avc1', 'mp41']), length, fill),
  m4v: (fill = 22, length = 64) => padded(ftyp('M4V ', ['M4V ', 'M4A ', 'mp42', 'isom']), length, fill),
  mov: (fill = 23, length = 64) => padded(ftyp('qt  ', ['qt  ']), length, fill),
  webm: (fill = 24, length = 64) => padded(ebml('webm'), length, fill),
  /** Recognised, but not supported. */
  mkv: (fill = 25, length = 64) => padded(ebml('matroska'), length, fill),
  avi: (fill = 26, length = 64) => padded(bytesOf('RIFF', [0x24, 0, 0, 0], 'AVI LIST'), length, fill),
  wmv: (fill = 27, length = 64) => padded(bytesOf([0x30, 0x26, 0xb2, 0x75, 0x8e, 0x66, 0xcf, 0x11, 0xa6, 0xd9]), length, fill),
  flv: (fill = 28, length = 64) => padded(bytesOf('FLV', [0x01, 0x05]), length, fill),
  m4a: (fill = 29, length = 64) => padded(ftyp('M4A ', ['M4A ', 'mp42', 'isom']), length, fill),
};

export type FileMap = Record<string, Uint8Array | string>;

function toBytes(value: Uint8Array | string): Uint8Array {
  return typeof value === 'string' ? strToU8(value) : value;
}

export function json(value: unknown): string {
  return JSON.stringify(value);
}

/** Builds a zip in memory. `stored` writes members uncompressed. */
export function makeZip(files: FileMap, stored = false): Uint8Array<ArrayBuffer> {
  const tree: Zippable = {};
  for (const [name, value] of Object.entries(files)) tree[name] = [toBytes(value), { level: stored ? 0 : 6 }];
  return zipSync(tree);
}

export interface TarMember {
  name: string;
  data?: Uint8Array | string;
  /** Header type flag; '0' (regular file) by default. */
  type?: string;
  /** ustar prefix field. */
  prefix?: string;
  /** Write the GNU magic ("ustar  \0") instead of the POSIX one. */
  gnu?: boolean;
}

function tarHeader(member: TarMember, size: number): Uint8Array {
  const header = new Uint8Array(512);
  const put = (text: string, offset: number, length: number) => header.set(strToU8(text).subarray(0, length), offset);
  const octal = (value: number, offset: number, length: number) =>
    put(value.toString(8).padStart(length - 1, '0'), offset, length - 1);
  put(member.name, 0, 100);
  octal(0o644, 100, 8);
  octal(0, 108, 8);
  octal(0, 116, 8);
  octal(size, 124, 12);
  octal(0, 136, 12);
  header.fill(0x20, 148, 156);
  put(member.type ?? '0', 156, 1);
  if (member.gnu) put('ustar  ', 257, 7);
  else {
    put('ustar', 257, 5);
    put('00', 263, 2);
  }
  if (member.prefix) put(member.prefix, 345, 155);
  let sum = 0;
  for (const byte of header) sum += byte;
  put(`${sum.toString(8).padStart(6, '0')}\0 `, 148, 8);
  return header;
}

/** Builds a tar archive in memory (512-byte blocks, two zero blocks at the end). */
export function makeTar(members: TarMember[]): Uint8Array<ArrayBuffer> {
  const blocks: Uint8Array[] = [];
  for (const member of members) {
    const data = toBytes(member.data ?? new Uint8Array(0));
    blocks.push(tarHeader(member, data.length));
    blocks.push(data);
    const padding = (512 - (data.length % 512)) % 512;
    if (padding) blocks.push(new Uint8Array(padding));
  }
  blocks.push(new Uint8Array(1024));
  const out = new Uint8Array(blocks.reduce((sum, block) => sum + block.length, 0));
  let offset = 0;
  for (const block of blocks) {
    out.set(block, offset);
    offset += block.length;
  }
  return out;
}

export function tarFromFiles(files: FileMap): Uint8Array<ArrayBuffer> {
  return makeTar(Object.entries(files).map(([name, data]) => ({ name, data })));
}

/** A pax extended header member that sets `path` for the member that follows it. */
export function paxPathMember(path: string): TarMember {
  const body = ` path=${path}\n`;
  let length = body.length + 1;
  while (String(length).length + strToU8(body).length !== length) length = String(length).length + strToU8(body).length;
  return { name: 'PaxHeader/entry', type: 'x', data: `${length}${body}` };
}

/** A GNU long-name member that names the member that follows it. */
export function gnuLongNameMember(path: string): TarMember {
  return { name: '././@LongLink', type: 'L', gnu: true, data: `${path}\0` };
}

export function makeFile(name: string, content: Uint8Array | string, type = ''): File {
  return new File([toBytes(content) as Uint8Array<ArrayBuffer>], name, { type });
}

/** Files as a folder picker (<input webkitdirectory>) delivers them. */
export function folderFiles(folder: string, files: FileMap): File[] {
  return Object.entries(files).map(([path, content]) => {
    const name = path.slice(path.lastIndexOf('/') + 1);
    const file = makeFile(name, content);
    Object.defineProperty(file, 'webkitRelativePath', { value: `${folder}/${path}`, configurable: true });
    return file;
  });
}

export async function blobBytes(blob: Blob): Promise<Uint8Array> {
  return new Uint8Array(await blob.arrayBuffer());
}

// --- drag and drop: file system entries and DataTransfer as browsers hand them out ---

export interface FakeEntry {
  isFile: boolean;
  isDirectory: boolean;
  name: string;
  fullPath: string;
}

export function fileEntry(file: File, parent = ''): FakeEntry & { file: (ok: (file: File) => void) => void } {
  return {
    isFile: true,
    isDirectory: false,
    name: file.name,
    fullPath: `${parent}/${file.name}`,
    file: (ok) => setTimeout(() => ok(file), 0),
  };
}

/** A directory entry whose reader hands out children in batches, like Chromium does. */
export function dirEntry(name: string, children: FakeEntry[], batchSize = 2, parent = ''): FakeEntry {
  return {
    isFile: false,
    isDirectory: true,
    name,
    fullPath: `${parent}/${name}`,
    createReader: () => {
      let position = 0;
      return {
        readEntries: (ok: (entries: FakeEntry[]) => void) => {
          const batch = children.slice(position, position + batchSize);
          position += batch.length;
          setTimeout(() => ok(batch), 0);
        },
      };
    },
  } as FakeEntry;
}

/** A DataTransfer whose items stop working once the "drop handler" has returned. */
export function fakeDataTransfer(items: Array<{ entry?: FakeEntry | null; file?: File | null; kind?: string }>, files: File[] = []) {
  let alive = true;
  const list = items.map((item) => ({
    kind: item.kind ?? 'file',
    type: '',
    webkitGetAsEntry: () => {
      if (!alive) throw new Error('DataTransferItem used after the drop handler returned');
      return item.entry ?? null;
    },
    getAsFile: () => {
      if (!alive) throw new Error('DataTransferItem used after the drop handler returned');
      return item.file ?? null;
    },
  }));
  const dataTransfer = { items: Object.assign(list, { length: list.length }), files } as unknown as DataTransfer;
  return {
    dataTransfer,
    expire: () => {
      alive = false;
    },
  };
}
