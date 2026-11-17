import { gzipSync, strToU8 } from 'fflate';
import { describe, expect, it } from 'vitest';
import { detectArchiveKind, openArchive } from './archive';
import { DEFAULT_LIMITS } from './limits';
import type { SourceEntry } from './source';
import { GunzipReader, readTarEntries } from './tar';
import {
  IMAGES,
  blobBytes,
  gnuLongNameMember,
  makeFile,
  makeTar,
  makeZip,
  paxPathMember,
  tarFromFiles,
} from './testkit';
import { ImportError } from './types';
import { BlobCollector, decodeZipName } from './util';
import { readZipEntries } from './zip';

const BIG = 64 * 1024 * 1024;

async function codeOf(promise: Promise<unknown>): Promise<string> {
  try {
    await promise;
  } catch (err) {
    return err instanceof ImportError ? err.code : `not an ImportError: ${String(err)}`;
  }
  return 'resolved';
}

async function contents(entries: SourceEntry[]): Promise<Record<string, number[]>> {
  const out: Record<string, number[]> = {};
  for (const entry of entries) out[entry.path] = Array.from(await blobBytes(await entry.open(BIG)));
  return out;
}

/** Deterministic, poorly compressible bytes. */
function noise(length: number, seed = 1): Uint8Array<ArrayBuffer> {
  const out = new Uint8Array(length);
  let state = seed;
  for (let i = 0; i < length; i++) {
    state = (state * 1103515245 + 12345) & 0x7fffffff;
    out[i] = state >>> 16;
  }
  return out;
}

describe('detectArchiveKind', () => {
  it('goes by magic bytes before the extension', async () => {
    const zip = makeZip({ 'post.json': '[]' });
    const tar = tarFromFiles({ 'post.json': '[]' });
    expect(await detectArchiveKind(makeFile('export.zip', zip))).toBe('zip');
    expect(await detectArchiveKind(makeFile('export.tar', tar))).toBe('tar');
    expect(await detectArchiveKind(makeFile('export.tar.gz', gzipSync(tar)))).toBe('gzip');
    expect(await detectArchiveKind(makeFile('export.tgz', gzipSync(tar)))).toBe('gzip');
    // Wrong or missing extensions do not matter.
    expect(await detectArchiveKind(makeFile('export.bin', zip))).toBe('zip');
    expect(await detectArchiveKind(makeFile('export.zip', tar))).toBe('tar');
    expect(await detectArchiveKind(makeFile('export', gzipSync(tar)))).toBe('gzip');
    expect(await detectArchiveKind(makeFile('really-a-tar.tgz', tar))).toBe('tar');
  });

  it('rejects other archive types with a helpful message', async () => {
    const rar = makeFile('export.rar', new Uint8Array([0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x01, 0x00]));
    const sevenZip = makeFile('export.zip', new Uint8Array([0x37, 0x7a, 0xbc, 0xaf, 0x27, 0x1c, 0, 4]));
    for (const file of [rar, sevenZip, makeFile('export.7z', 'whatever'), makeFile('notes.txt', 'hello')]) {
      const error = await detectArchiveKind(file).then(
        () => null,
        (err: unknown) => err,
      );
      expect(error).toBeInstanceOf(ImportError);
      expect((error as ImportError).code).toBe('unsupported-archive');
      expect((error as ImportError).message).toMatch(/\.zip, \.tar or \.tar\.gz/);
      expect((error as ImportError).message).toMatch(/extracted folder/);
    }
  });

  it('reports an empty file as unreadable', async () => {
    expect(await codeOf(detectArchiveKind(makeFile('export.zip', new Uint8Array(0))))).toBe('read-failed');
  });
});

describe('zip reader', () => {
  it('lists and reads deflated and stored members', async () => {
    const files = {
      'post.json': '{"posts":[]}',
      'posts/post1.jpg': IMAGES.jpeg(),
      'posts/big.bin': noise(300_000),
      'posts/zeros.bin': new Uint8Array(500_000),
    };
    for (const stored of [false, true]) {
      const entries = await readZipEntries(makeFile('e.zip', makeZip(files, stored)), 1000);
      expect(entries.map((entry) => entry.path).sort()).toEqual(Object.keys(files).sort());
      expect(entries.find((entry) => entry.path === 'posts/big.bin')?.size).toBe(300_000);
      const read = await contents(entries);
      expect(read['post.json']).toEqual(Array.from(strToU8(files['post.json'])));
      expect(read['posts/post1.jpg']).toEqual(Array.from(files['posts/post1.jpg']));
      expect(read['posts/big.bin']).toEqual(Array.from(files['posts/big.bin']));
      expect(read['posts/zeros.bin']).toEqual(Array.from(files['posts/zeros.bin']));
    }
  });

  it('handles UTF-8 names, backslash separators and ignores noise', async () => {
    const entries = await readZipEntries(
      makeFile(
        'e.zip',
        makeZip({
          'posts/日本語 фото é.jpg': IMAGES.jpeg(),
          'posts\\win.jpg': IMAGES.jpeg(),
          '__MACOSX/posts/._post1.jpg': 'resource fork',
          'posts/._post1.jpg': 'resource fork',
          'posts/.DS_Store': 'x',
          'posts/Thumbs.db': 'x',
          '../evil.jpg': IMAGES.jpeg(),
        }),
      ),
      1000,
    );
    expect(entries.map((entry) => entry.path).sort()).toEqual(['posts/win.jpg', 'posts/日本語 фото é.jpg']);
  });

  it('copes with data in front of the archive and a trailing comment', async () => {
    const zip = makeZip({ 'post.json': '[1,2,3]', 'posts/a.png': IMAGES.png() });
    const prefixed = new Uint8Array(1000 + zip.length);
    prefixed.set(zip, 1000);
    const entries = await readZipEntries(makeFile('sfx.zip', prefixed), 1000);
    expect((await contents(entries))['post.json']).toEqual(Array.from(strToU8('[1,2,3]')));

    const commented = new Uint8Array(zip.length + 5);
    commented.set(zip);
    commented[zip.length - 2] = 5; // comment length field of the end record
    commented.set(strToU8('hello'), zip.length);
    expect(await readZipEntries(makeFile('c.zip', commented), 1000)).toHaveLength(2);
  });

  it('refuses a member that is larger than allowed or larger than it claims', async () => {
    const zip = makeZip({ 'bomb.bin': new Uint8Array(2_000_000) });
    expect(zip.length).toBeLessThan(10_000);
    const [entry] = await readZipEntries(makeFile('bomb.zip', zip), 1000);
    expect(await codeOf(entry.open(1_000_000))).toBe('too-large');
    expect((await entry.open(2_000_000)).size).toBe(2_000_000);

    // Lie about the uncompressed size in the central directory (bytes 24..27 of the record).
    const lying = zip.slice();
    const view = new DataView(lying.buffer);
    let directory = -1;
    for (let i = lying.length - 4; i >= 0; i--) {
      if (view.getUint32(i, true) === 0x02014b50) {
        directory = i;
        break;
      }
    }
    view.setUint32(directory + 24, 1000, true);
    const [liar] = await readZipEntries(makeFile('liar.zip', lying), 1000);
    expect(liar.size).toBe(1000);
    expect(await codeOf(liar.open(BIG))).toBe('read-failed');
  });

  it('refuses an archive whose members overlap (one compressed stream listed many times)', async () => {
    // The cheap zip bomb: every directory record points at the same local header, so a few
    // kilobytes of archive unpack to "members x size" bytes.
    const picture = IMAGES.jpeg(0, 200_000);
    const honest = makeZip({ 'post.json': '[]', 'posts/1.jpg': picture, 'posts/2.jpg': picture, 'posts/3.jpg': picture });
    expect(await readZipEntries(makeFile('honest.zip', honest), 1000)).toHaveLength(4);

    const bomb = honest.slice();
    const view = new DataView(bomb.buffer);
    const records: number[] = [];
    for (let at = view.getUint32(bomb.length - 22 + 16, true); view.getUint32(at, true) === 0x02014b50; ) {
      records.push(at);
      at += 46 + view.getUint16(at + 28, true) + view.getUint16(at + 30, true) + view.getUint16(at + 32, true);
    }
    expect(records).toHaveLength(4);
    // Records 2 and 3 (2.jpg, 3.jpg) now claim the data of record 1 (1.jpg): same content, so
    // sizes and checksums still agree and nothing else gives the archive away.
    const shared = view.getUint32(records[1] + 42, true);
    view.setUint32(records[2] + 42, shared, true);
    view.setUint32(records[3] + 42, shared, true);
    expect(await codeOf(readZipEntries(makeFile('bomb.zip', bomb), 1000))).toBe('read-failed');

    // A member that reaches into the next one is refused as well.
    const reaching = honest.slice();
    const reachingView = new DataView(reaching.buffer);
    reachingView.setUint32(records[1] + 20, reachingView.getUint32(records[1] + 20, true) + 40, true);
    expect(await codeOf(readZipEntries(makeFile('reaching.zip', reaching), 1000))).toBe('read-failed');
  });

  it('reads names that are not UTF-8 as code page 437', () => {
    const bytes = (...values: number[]) => new Uint8Array(values);
    expect(decodeZipName(strToU8('posts/日本語 é.jpg'))).toBe('posts/日本語 é.jpg');
    expect(decodeZipName(bytes(0x63, 0x61, 0x66, 0x82))).toBe('café');
    expect(decodeZipName(bytes(0x80, 0x81, 0x84, 0x8e, 0x94, 0x99, 0x9a, 0xa4, 0xa5, 0xe1))).toBe('ÇüäÄöÖÜñÑß');
    expect(decodeZipName(bytes(0xff))).toBe('\u00a0');
    const all = decodeZipName(Uint8Array.from({ length: 128 }, (_, i) => 0x80 + i));
    expect(new Set(all).size).toBe(128);
  });

  it('rejects damaged archives and too many members', async () => {
    const zip = makeZip({ 'post.json': '[]', 'a.jpg': IMAGES.jpeg(), 'b.jpg': IMAGES.jpeg() });
    expect(await codeOf(readZipEntries(makeFile('cut.zip', zip.slice(0, zip.length - 30)), 1000))).toBe('read-failed');
    expect(await codeOf(readZipEntries(makeFile('tiny.zip', new Uint8Array([0x50, 0x4b, 3, 4])), 1000))).toBe('read-failed');
    expect(await codeOf(readZipEntries(makeFile('many.zip', zip), 2))).toBe('too-large');
  });

  it('stops when the signal is aborted', async () => {
    const controller = new AbortController();
    const entries = await readZipEntries(makeFile('e.zip', makeZip({ 'a.bin': noise(100_000) })), 1000);
    controller.abort();
    expect(await codeOf(entries[0].open(BIG, controller.signal))).toBe('aborted');
  });
});

describe('tar reader', () => {
  const open = (name: string, bytes: Uint8Array) => openArchive(makeFile(name, bytes), { limits: DEFAULT_LIMITS });

  it('reads plain tar and tar.gz', async () => {
    const files = {
      'post.json': '{"posts":[]}',
      'posts/post1.jpg': IMAGES.jpeg(),
      'posts/exact-block.bin': noise(1024),
      'posts/big.bin': noise(200_000, 7),
      'posts/empty.bin': new Uint8Array(0),
    };
    const tar = tarFromFiles(files);
    for (const [name, bytes] of [
      ['e.tar', tar],
      ['e.tar.gz', gzipSync(tar)],
      ['e.tgz', gzipSync(tar, { level: 0 })],
    ] as const) {
      const entries = await open(name, bytes);
      expect(entries.map((entry) => entry.path)).toEqual(Object.keys(files));
      const read = await contents(entries);
      expect(read['post.json']).toEqual(Array.from(strToU8(files['post.json'])));
      expect(read['posts/big.bin']).toEqual(Array.from(files['posts/big.bin']));
      expect(read['posts/exact-block.bin']).toEqual(Array.from(files['posts/exact-block.bin']));
      expect(read['posts/empty.bin']).toEqual([]);
    }
  });

  it('understands the ustar prefix, GNU long names and pax paths', async () => {
    const longDir = `export/${'d'.repeat(120)}`;
    const longPath = `export/posts/${'n'.repeat(180)}.jpg`;
    const paxPath = `export/posts/${'p'.repeat(150)} ü.jpg`;
    const tar = makeTar([
      { name: 'export/', type: '5' },
      { name: 'post.json', prefix: 'export', data: '[]' },
      { name: 'deep.jpg', prefix: longDir, data: IMAGES.jpeg() },
      gnuLongNameMember(longPath),
      { name: longPath.slice(0, 100), gnu: true, data: IMAGES.png() },
      paxPathMember(paxPath),
      { name: 'export/posts/truncated-name.jpg', data: IMAGES.webp() },
      // GNU archives keep other data where POSIX has the prefix: it must not become a path.
      { name: 'export/gnu.jpg', gnu: true, prefix: '1234567', data: IMAGES.gif() },
      { name: 'export/link.jpg', type: '2' },
      { name: 'export/__MACOSX/._x.jpg', data: 'noise' },
      { name: 'export/after.jpg', data: IMAGES.jpeg() },
    ]);
    for (const bytes of [tar, gzipSync(tar)]) {
      const entries = await open('e.tar', bytes);
      expect(entries.map((entry) => entry.path)).toEqual([
        'export/post.json',
        `${longDir}/deep.jpg`,
        longPath,
        paxPath,
        'export/gnu.jpg',
        'export/after.jpg',
      ]);
      const read = await contents(entries);
      expect(read[longPath]).toEqual(Array.from(IMAGES.png()));
      expect(read[paxPath]).toEqual(Array.from(IMAGES.webp()));
      expect(read['export/after.jpg']).toEqual(Array.from(IMAGES.jpeg()));
    }
  });

  it('rejects things that are not tar and damaged archives', async () => {
    expect(await codeOf(open('not-a.tar', noise(4096)))).toBe('unsupported-archive');
    expect(await codeOf(open('json.gz', gzipSync(strToU8('{"posts":[]}'.repeat(100)))))).toBe('unsupported-archive');

    const tar = tarFromFiles({ 'post.json': '[]', 'posts/a.jpg': IMAGES.jpeg(), 'posts/b.jpg': IMAGES.jpeg() });
    const damaged = tar.slice();
    damaged[1024 + 20] ^= 0xff; // inside the second header
    expect(await codeOf(open('bad.tar', damaged))).toBe('read-failed');

    const gz = gzipSync(tarFromFiles({ 'post.json': '[]', 'posts/a.bin': noise(50_000) }));
    expect(await codeOf(open('cut.tar.gz', gz.slice(0, gz.length - 2000)))).toBe('read-failed');
  });

  it('keeps complete members of a tar that was cut off', async () => {
    const tar = tarFromFiles({ 'post.json': '[]', 'posts/a.jpg': IMAGES.jpeg(), 'posts/b.bin': noise(5000) });
    const entries = await open('cut.tar', tar.slice(0, tar.length - 4096));
    expect(entries.map((entry) => entry.path)).toEqual(['post.json', 'posts/a.jpg']);
  });

  it('caps what a gzip stream may unpack to (gzip bomb)', async () => {
    const bomb = gzipSync(tarFromFiles({ 'post.json': '[]', 'posts/zeros.bin': new Uint8Array(3_000_000) }));
    expect(bomb.length).toBeLessThan(20_000);
    const reader = new GunzipReader(makeFile('bomb.tar.gz', bomb), 1_000_000);
    expect(await codeOf(readTarEntries(reader, { maxEntries: 100, maxMemberBytes: BIG }))).toBe('too-large');
  });

  it('lists but does not keep oversized members of a tar.gz', async () => {
    const gz = gzipSync(tarFromFiles({ 'post.json': '[]', 'posts/huge.bin': noise(100_000), 'posts/a.jpg': IMAGES.jpeg() }));
    const reader = new GunzipReader(makeFile('e.tar.gz', gz), BIG);
    const entries = await readTarEntries(reader, { maxEntries: 100, maxMemberBytes: 10_000 });
    expect(entries.map((entry) => entry.path)).toEqual(['post.json', 'posts/huge.bin', 'posts/a.jpg']);
    expect(await codeOf(entries[1].open(BIG))).toBe('too-large');
    expect(Array.from(await blobBytes(await entries[2].open(BIG)))).toEqual(Array.from(IMAGES.jpeg()));
  });

  it('enforces the member count limit and the abort signal', async () => {
    const tar = tarFromFiles({ 'a.jpg': IMAGES.jpeg(), 'b.jpg': IMAGES.jpeg(), 'c.jpg': IMAGES.jpeg() });
    const limits = { ...DEFAULT_LIMITS, maxSourceEntries: 2 };
    expect(await codeOf(openArchive(makeFile('e.tar', tar), { limits }))).toBe('too-large');

    const controller = new AbortController();
    controller.abort();
    expect(await codeOf(openArchive(makeFile('e.tar', tar), { limits: DEFAULT_LIMITS, signal: controller.signal }))).toBe(
      'aborted',
    );
    expect(
      await codeOf(openArchive(makeFile('e.tar.gz', gzipSync(tar)), { limits: DEFAULT_LIMITS, signal: controller.signal })),
    ).toBe('aborted');
  });
});

describe('decompressed members larger than one Blob step', () => {
  it('BlobCollector keeps every byte, in order, across its steps', async () => {
    const collector = new BlobCollector(4);
    for (const chunk of [[1, 2, 3], [4, 5], [], [6], [7, 8, 9, 10, 11]]) collector.add(new Uint8Array(chunk));
    expect(Array.from(await blobBytes(collector.finish()))).toEqual([1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
    expect((new BlobCollector(4)).finish().size).toBe(0);
  });

  it('inflates a member of more than 16 MB (a video) completely, from zip and from tar.gz', async () => {
    const size = 17 * 1024 * 1024 + 123;
    const video = new Uint8Array(size);
    for (let i = 0; i < size; i++) video[i] = (i * 7) % 251;
    const zipped = makeFile('v.zip', makeZip({ 'v.mp4': video }));
    const [fromZip] = await readZipEntries(zipped, 10);
    const tarred = makeFile('v.tar.gz', gzipSync(tarFromFiles({ 'v.mp4': video })));
    const [fromTar] = await openArchive(tarred, { limits: DEFAULT_LIMITS });
    for (const entry of [fromZip, fromTar]) {
      expect(entry.inMemory).toBe(true);
      const bytes = await blobBytes(await entry.open(size));
      expect(bytes.length).toBe(size);
      expect(bytes.every((byte, i) => byte === (i * 7) % 251)).toBe(true);
    }
  });
});
