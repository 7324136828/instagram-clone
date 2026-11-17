import { gzipSync, strToU8 } from 'fflate';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { importArchive, importPathedFiles } from './core';
import {
  ARCHIVE_ACCEPT,
  ImportError,
  importFromArchive,
  importFromDataTransfer,
  importFromFiles,
  isArchiveFile,
  releaseImport,
  setHeicConverter,
  setHeicNativeProbe,
  type ImportProgress,
  type ImportResult,
} from './index';
import { blobForUrl } from './registry';
import {
  IMAGES,
  VIDEOS,
  blobBytes,
  dirEntry,
  fakeDataTransfer,
  fileEntry,
  folderFiles,
  json,
  makeFile,
  makeZip,
  tarFromFiles,
  type FakeEntry,
  type FileMap,
} from './testkit';

async function failure(promise: Promise<unknown>): Promise<ImportError> {
  try {
    await promise;
  } catch (err) {
    if (err instanceof ImportError) return err;
    throw new Error(`expected an ImportError, got ${String(err)}`);
  }
  throw new Error('expected the import to fail');
}

const zipFile = (files: FileMap, name = 'export.zip') => makeFile(name, makeZip(files), 'application/zip');

async function bytesBehind(url: string): Promise<number[]> {
  const blob = blobForUrl(url);
  if (!blob) throw new Error(`no blob registered for ${url}`);
  return Array.from(await blobBytes(blob));
}

const results: ImportResult[] = [];
/** Tracks results so that every test gives its object URLs back. */
async function track(promise: Promise<ImportResult>): Promise<ImportResult> {
  const result = await promise;
  results.push(result);
  return result;
}

beforeEach(() => {
  // Node has no createImageBitmap; make "no native HEIC support" explicit anyway.
  setHeicNativeProbe(async () => false);
});

afterEach(() => {
  for (const result of results.splice(0)) releaseImport(result);
  setHeicConverter(null);
  setHeicNativeProbe(null);
  vi.restoreAllMocks();
});

const MANIFEST = {
  format: 'instagram-export',
  version: 1,
  exported_at: '2026-11-10T09:30:00Z',
  source: { type: 'profile', url: 'https://www.instagram.com/natgeo/' },
  posts: [
    {
      id: 'AAA111',
      url: 'https://www.instagram.com/p/AAA111/',
      image: 'posts/post1.jpg',
      user: { username: 'natgeo', full_name: 'National Geographic', verified: true, avatar: 'avatars/natgeo.jpg' },
      caption: 'First',
      comment_count: 1234,
      like_count: 56789,
      taken_at: '2026-11-01T15:04:05Z',
    },
    { id: 'BBB222', image: 'posts/post2.png', user: 'natgeo', caption: 'Second', comment_count: '12', like_count: null },
  ],
};

const EXPORT: FileMap = {
  'post.json': json(MANIFEST),
  'posts/post1.jpg': IMAGES.jpeg(11),
  'posts/post2.png': IMAGES.png(12),
  'avatars/natgeo.jpg': IMAGES.jpeg(13),
};

/**
 * Makes files behave like files on disk: after `remove()`, neither they nor any slice taken
 * from them can be read any more (as when the user deletes, moves or re-saves the export).
 */
function onDisk(files: File[]): { files: File[]; remove: () => void } {
  let gone = false;
  const guard = <T extends Blob>(blob: T): T => {
    const slice = blob.slice.bind(blob);
    const read = (method: 'arrayBuffer' | 'text') => {
      const original = blob[method].bind(blob) as () => Promise<unknown>;
      return async () => {
        if (gone) throw new DOMException('The requested file could not be found.', 'NotFoundError');
        return original();
      };
    };
    Object.defineProperties(blob, {
      slice: { value: (...args: Parameters<Blob['slice']>) => guard(slice(...args)) },
      arrayBuffer: { value: read('arrayBuffer') },
      text: { value: read('text') },
      stream: {
        value: () => {
          throw new Error('not used by the import library');
        },
      },
    });
    return blob;
  };
  files.forEach(guard);
  return {
    files,
    remove: () => {
      gone = true;
    },
  };
}

describe('independence from the source file', () => {
  const sources: Array<[string, () => File[]]> = [
    ['a folder', () => folderFiles('my-export', EXPORT)],
    ['a zip written without compression', () => [makeFile('stored.zip', makeZip(EXPORT, true), 'application/zip')]],
    ['a deflated zip', () => [zipFile(EXPORT)]],
    ['a plain tar', () => [makeFile('export.tar', tarFromFiles(EXPORT))]],
    ['a tar.gz', () => [makeFile('export.tar.gz', gzipSync(tarFromFiles(EXPORT)))]],
  ];

  it.each(sources)('keeps every picture of %s readable after the source is deleted', async (_label, make) => {
    const source = onDisk(make());
    const result = await track(importFromFiles(source.files));
    expect(result.posts).toHaveLength(2);
    expect(result.warnings).toEqual([]);
    source.remove();
    expect(await bytesBehind(result.posts[0].media[0].url)).toEqual(Array.from(IMAGES.jpeg(11)));
    expect(await bytesBehind(result.posts[1].media[0].url)).toEqual(Array.from(IMAGES.png(12)));
    expect(await bytesBehind(result.posts[0].user.avatarUrl as string)).toEqual(Array.from(IMAGES.jpeg(13)));
    expect(blobForUrl(result.posts[0].media[0].url)?.type).toBe('image/jpeg');
  });

  it('keeps an unconverted HEIC readable too', async () => {
    setHeicNativeProbe(async () => true);
    const source = onDisk(
      folderFiles('x', { 'post.json': json([{ user: 'a', image: 'posts/p.heic' }]), 'posts/p.heic': IMAGES.heic(5) }),
    );
    const result = await track(importFromFiles(source.files));
    source.remove();
    expect(await bytesBehind(result.posts[0].media[0].url)).toEqual(Array.from(IMAGES.heic(5)));
  });

  it('reports a picture that disappears during the import instead of failing', async () => {
    const source = onDisk(
      folderFiles('x', {
        'post.json': json([
          { user: 'a', image: 'posts/a.jpg' },
          { user: 'a', image: 'posts/b.jpg' },
        ]),
        'posts/a.jpg': IMAGES.jpeg(1),
        'posts/b.jpg': IMAGES.jpeg(2),
      }),
    );
    const error = await failure(
      importFromFiles(source.files, {
        onProgress: (progress) => {
          if (progress.phase === 'manifest' && progress.done === 1) source.remove();
        },
      }),
    );
    expect(error.code).toBe('no-posts');
    expect(error.message).toContain('could not be read');
  });
});

describe('isArchiveFile / ARCHIVE_ACCEPT', () => {
  it('recognises supported archives by name or MIME type', () => {
    expect(isArchiveFile(makeFile('a.zip', ''))).toBe(true);
    expect(isArchiveFile(makeFile('A.ZIP', ''))).toBe(true);
    expect(isArchiveFile(makeFile('a.tar', ''))).toBe(true);
    expect(isArchiveFile(makeFile('a.tgz', ''))).toBe(true);
    expect(isArchiveFile(makeFile('a.tar.gz', ''))).toBe(true);
    expect(isArchiveFile(makeFile('download', '', 'application/zip'))).toBe(true);
    expect(isArchiveFile(makeFile('download', '', 'application/x-zip-compressed'))).toBe(true);
    expect(isArchiveFile(makeFile('download', '', 'application/gzip'))).toBe(true);
    expect(isArchiveFile(makeFile('post.json', '', 'application/json'))).toBe(false);
    expect(isArchiveFile(makeFile('photo.jpg', '', 'image/jpeg'))).toBe(false);
    expect(isArchiveFile(makeFile('a.rar', ''))).toBe(false);
    expect(ARCHIVE_ACCEPT).toContain('.zip');
    // `.tar.gz` alone is not matched by browsers that only look at the last extension.
    expect(ARCHIVE_ACCEPT.split(',')).toEqual(expect.arrayContaining(['.zip', '.tar', '.tgz', '.gz', 'application/gzip']));
  });
});

describe('importFromArchive', () => {
  it('imports a zip with post.json at the root', async () => {
    const result = await track(importFromArchive(zipFile(EXPORT)));
    expect(result.sourceName).toBe('export.zip');
    expect(result.sourceKind).toBe('archive');
    expect(result.totalEntries).toBe(2);
    expect(result.warnings).toEqual([]);
    expect(result.exportedAt).toBe('2026-11-10T09:30:00.000Z');
    expect(result.sourceUrl).toBe('https://www.instagram.com/natgeo/');
    expect(result.posts).toHaveLength(2);

    const [first, second] = result.posts;
    expect(first).toMatchObject({
      id: 'AAA111',
      permalink: 'https://www.instagram.com/p/AAA111/',
      caption: 'First',
      commentCount: 1234,
      likeCount: 56789,
      takenAt: '2026-11-01T15:04:05.000Z',
      mediaType: 'image',
      comments: [],
    });
    expect(first.user).toMatchObject({
      username: 'natgeo',
      fullName: 'National Geographic',
      verified: true,
      profileUrl: 'https://www.instagram.com/natgeo/',
    });
    expect(first.media).toEqual([
      { kind: 'image', url: expect.stringMatching(/^blob:/), path: 'posts/post1.jpg', mime: 'image/jpeg', bytes: 64 },
    ]);
    expect(await bytesBehind(first.media[0].url)).toEqual(Array.from(IMAGES.jpeg(11)));
    expect(blobForUrl(first.media[0].url)?.type).toBe('image/jpeg');
    expect(await bytesBehind(first.user.avatarUrl as string)).toEqual(Array.from(IMAGES.jpeg(13)));

    expect(second).toMatchObject({ id: 'BBB222', commentCount: 12, likeCount: null });
    expect(second.media[0].mime).toBe('image/png');
    expect(second.user.avatarUrl).toBeUndefined();
    expect(second.permalink).toBeUndefined();
  });

  it('imports a zip that wraps everything in one top-level folder and ignores __MACOSX noise', async () => {
    const wrapped: FileMap = {
      '__MACOSX/._my-export': 'noise',
      '__MACOSX/my-export/._post.json': '{"posts": "this is not the manifest"}',
      '__MACOSX/my-export/posts/._post1.jpg': 'noise',
      'my-export/.DS_Store': 'noise',
    };
    for (const [path, content] of Object.entries(EXPORT)) wrapped[`my-export/${path}`] = content;
    const result = await track(importFromArchive(zipFile(wrapped)));
    expect(result.posts.map((post) => post.id)).toEqual(['AAA111', 'BBB222']);
    expect(result.warnings).toEqual([]);
    expect(result.posts[0].user.avatarUrl).toBeDefined();
  });

  it('prefers the shallowest post.json and accepts posts.json and other casings', async () => {
    const nested = await track(
      importFromArchive(
        zipFile({
          'backup/old/post.json': json([{ user: 'old', image: 'x.jpg' }]),
          'backup/old/x.jpg': IMAGES.jpeg(),
          'backup/post.json': json([{ user: 'new', image: 'posts/y.jpg' }]),
          'backup/posts/y.jpg': IMAGES.jpeg(),
        }),
      ),
    );
    expect(nested.posts.map((post) => post.user.username)).toEqual(['new']);

    const alternative = await track(
      importFromArchive(zipFile({ 'Posts.JSON': json([{ user: 'a', image: 'posts/y.jpg' }]), 'posts/y.jpg': IMAGES.jpeg() })),
    );
    expect(alternative.posts).toHaveLength(1);
  });

  it('imports tar and tar.gz archives', async () => {
    const tar = tarFromFiles(EXPORT);
    for (const file of [
      makeFile('export.tar', tar),
      makeFile('export.tar.gz', gzipSync(tar)),
      makeFile('export.tgz', gzipSync(tar)),
    ]) {
      const result = await track(importFromArchive(file));
      expect(result.sourceName).toBe(file.name);
      expect(result.posts.map((post) => post.id)).toEqual(['AAA111', 'BBB222']);
      expect(await bytesBehind(result.posts[1].media[0].url)).toEqual(Array.from(IMAGES.png(12)));
      expect(result.warnings).toEqual([]);
    }
  });

  it('accepts a bare-array manifest, a BOM and UTF-16', async () => {
    const posts = [{ from: 'alice', path: 'posts/a.jpg', description: 'héllo 😀', comments_total: 3 }];
    const utf8Bom = new Uint8Array([0xef, 0xbb, 0xbf, ...strToU8(json(posts))]);
    const text = json(posts);
    const utf16 = new Uint8Array(2 + text.length * 2);
    utf16.set([0xff, 0xfe]);
    for (let i = 0; i < text.length; i++) {
      utf16[2 + i * 2] = text.charCodeAt(i) & 0xff;
      utf16[3 + i * 2] = text.charCodeAt(i) >> 8;
    }
    for (const manifest of [json(posts), utf8Bom, utf16]) {
      const result = await track(importFromArchive(zipFile({ 'post.json': manifest, 'posts/a.jpg': IMAGES.jpeg() })));
      expect(result.posts).toHaveLength(1);
      expect(result.posts[0]).toMatchObject({ caption: 'héllo 😀', commentCount: 3, user: { username: 'alice' } });
    }
  });

  it('resolves backslash paths, ./ and / prefixes and different casing', async () => {
    const result = await track(
      importFromArchive(
        zipFile({
          'post.json': json([
            { user: 'a', image: 'posts\\post1.jpg' },
            { user: 'a', image: './posts/post2.png' },
            { user: 'a', image: '/posts/post3.webp' },
            { user: 'a', image: 'POSTS/Post4.GIF' },
            { user: 'a', image: 'posts\\sub\\..\\post1.jpg' },
          ]),
          'posts/post1.jpg': IMAGES.jpeg(),
          'posts/post2.png': IMAGES.png(),
          'posts/post3.webp': IMAGES.webp(),
          'posts/post4.gif': IMAGES.gif(),
        }),
      ),
    );
    expect(result.warnings).toEqual([]);
    expect(result.posts.map((post) => post.media[0].path)).toEqual([
      'posts/post1.jpg',
      'posts/post2.png',
      'posts/post3.webp',
      'POSTS/Post4.GIF',
      'posts/post1.jpg',
    ]);
    expect(result.posts.map((post) => post.media[0].mime)).toEqual([
      'image/jpeg',
      'image/png',
      'image/webp',
      'image/gif',
      'image/jpeg',
    ]);
  });

  it('prefers the exact path over a case-insensitive match', async () => {
    const result = await track(
      importFromArchive(
        zipFile({
          'post.json': json([{ user: 'a', image: 'posts/A.jpg' }]),
          'posts/a.jpg': IMAGES.png(),
          'posts/A.jpg': IMAGES.jpeg(),
        }),
      ),
    );
    expect(result.posts[0].media[0].mime).toBe('image/jpeg');
  });

  it('finds images whose path was written absolute or with the export folder in front', async () => {
    const images = [
      'C:\\Users\\me\\instagram-export\\posts\\post1.jpg',
      'C:/Users/me/instagram-export/posts/post1.jpg',
      '/home/me/instagram-export/posts/post1.jpg',
      'instagram-export/posts/post1.jpg',
      '~/export/posts/post1.jpg',
    ];
    const user = { username: 'a', avatar: 'C:\\Users\\me\\instagram-export\\avatars\\a.jpg' };
    const posts = images.map((image) => ({ user, image }));
    for (const folder of ['', 'instagram-export/']) {
      const result = await track(
        importFromArchive(
          zipFile({
            [`${folder}post.json`]: json(posts),
            [`${folder}posts/post1.jpg`]: IMAGES.jpeg(1),
            [`${folder}avatars/post1.jpg`]: IMAGES.gif(2),
            [`${folder}avatars/a.jpg`]: IMAGES.png(3),
          }),
        ),
      );
      expect(result.warnings).toEqual([]);
      expect(result.posts.map((post) => post.media[0].mime)).toEqual(images.map(() => 'image/jpeg'));
      expect(blobForUrl(result.posts[0].user.avatarUrl as string)?.type).toBe('image/png');
    }
  });

  it('matches accented file names of zips made by 7-Zip and older Windows tools (CP437 names)', async () => {
    // Such tools store "é" as the single byte 0x82 and do not set the UTF-8 flag.
    const zip = makeZip({
      'post.json': json([
        { user: 'a', image: 'posts/café.jpg' },
        { user: 'a', image: 'posts/Übung ñ.png' },
      ]),
      'posts/caf#.jpg': IMAGES.jpeg(),
      'posts/#bung $.png': IMAGES.png(),
    });
    const patch = (ascii: string, bytes: number[]) => {
      const needle = strToU8(ascii);
      let patched = 0;
      for (let i = 0; i + needle.length <= zip.length; i++) {
        if (needle.every((byte, offset) => zip[i + offset] === byte)) {
          zip.set(bytes, i);
          patched++;
        }
      }
      expect(patched).toBe(2); // local header and central directory
    };
    patch('caf#.jpg', [0x63, 0x61, 0x66, 0x82, 0x2e, 0x6a, 0x70, 0x67]);
    patch('#bung $.png', [0x9a, 0x62, 0x75, 0x6e, 0x67, 0x20, 0xa4, 0x2e, 0x70, 0x6e, 0x67]);
    const result = await track(importFromArchive(makeFile('7zip.zip', zip)));
    expect(result.warnings).toEqual([]);
    expect(result.posts.map((post) => post.media[0].mime)).toEqual(['image/jpeg', 'image/png']);
  });

  it('never lets a path escape the export root', async () => {
    const result = await track(
      importFromArchive(
        zipFile({
          'secret.jpg': IMAGES.jpeg(),
          'export/post.json': json([
            { user: 'a', image: '../secret.jpg' },
            { user: 'a', image: 'posts/../../secret.jpg' },
            { user: 'a', image: '..\\secret.jpg' },
            { user: 'a', image: 'posts/ok.jpg' },
          ]),
          'export/posts/ok.jpg': IMAGES.jpeg(),
        }),
      ),
    );
    expect(result.posts.map((post) => post.media[0].path)).toEqual(['posts/ok.jpg']);
    expect(result.totalEntries).toBe(4);
    expect(result.warnings.map((warning) => [warning.code, warning.postIndex])).toEqual([
      ['invalid-entry', 0],
      ['invalid-entry', 1],
      ['invalid-entry', 2],
    ]);
  });

  it('skips entries whose image is missing, remote or unsupported, with warnings', async () => {
    const fetchSpy = vi.fn();
    vi.stubGlobal('fetch', fetchSpy);
    const result = await track(
      importFromArchive(
        zipFile({
          'post.json': json([
            { user: 'a', image: 'posts/missing.jpg' },
            { user: 'a', image: 'https://scontent.cdninstagram.com/v/t51/1.jpg' },
            { user: 'a', image: 'posts/vector.svg' },
            { user: 'a', image: 'posts/clip.mkv' },
            { user: 'a', image: 'posts/empty.jpg' },
            { user: 'a', image: 'posts/ok.jpg' },
            { user: 'a' },
          ]),
          'posts/vector.svg': IMAGES.svg(),
          'posts/clip.mkv': VIDEOS.mkv(),
          'posts/empty.jpg': new Uint8Array(0),
          'posts/ok.jpg': IMAGES.jpeg(),
        }),
      ),
    );
    vi.unstubAllGlobals();
    expect(fetchSpy).not.toHaveBeenCalled();
    expect(result.posts).toHaveLength(1);
    expect(result.totalEntries).toBe(7);
    expect(result.warnings.map((warning) => [warning.code, warning.postIndex, warning.path])).toEqual([
      ['missing-image', 0, 'posts/missing.jpg'],
      ['missing-image', 1, 'https://scontent.cdninstagram.com/v/t51/1.jpg'],
      ['unsupported-image', 2, 'posts/vector.svg'],
      ['unsupported-media', 3, 'posts/clip.mkv'],
      ['unsupported-image', 4, 'posts/empty.jpg'],
      ['invalid-entry', 6, undefined],
    ]);
    for (const warning of result.warnings) expect(warning.message.length).toBeGreaterThan(10);
  });

  it('identifies images by content, not by extension', async () => {
    const result = await track(
      importFromArchive(
        zipFile({
          'post.json': json([
            { user: 'a', image: 'posts/actually-webp.jpg' },
            { user: 'a', image: 'posts/actually-png.heic' },
            { user: 'a', image: 'posts/no-extension' },
            { user: 'a', image: 'posts/actually-avif.png' },
            { user: 'a', image: 'posts/picture.bmp' },
            { user: 'a', image: 'posts/actually-svg.png' },
          ]),
          'posts/actually-webp.jpg': IMAGES.webp(),
          'posts/actually-png.heic': IMAGES.png(),
          'posts/no-extension': IMAGES.jpeg(),
          'posts/actually-avif.png': IMAGES.avif(),
          'posts/picture.bmp': IMAGES.bmp(),
          'posts/actually-svg.png': IMAGES.svg(),
        }),
      ),
    );
    expect(result.posts.map((post) => post.media[0].mime)).toEqual([
      'image/webp',
      'image/png',
      'image/jpeg',
      'image/avif',
      'image/bmp',
    ]);
    expect(blobForUrl(result.posts[0].media[0].url)?.type).toBe('image/webp');
    expect(result.posts.every((post) => post.media[0].converted === undefined)).toBe(true);
    expect(result.warnings.map((warning) => warning.code)).toEqual(['unsupported-image']);
  });

  it('imports carousels and keeps working slides when one is missing', async () => {
    const result = await track(
      importFromArchive(
        zipFile({
          'post.json': json([
            { user: 'a', images: ['posts/p1_1.jpg', 'posts/p1_2.png', 'posts/p1_3.webp'], media_type: 'carousel' },
            { user: 'a', image: 'posts/p2_1.jpg', images: ['posts/p2_1.jpg', 'posts/p2_gone.jpg', 'posts/p2_3.jpg'] },
            { user: 'a', image: 'posts/p1_1.jpg', media_type: 'video' },
          ]),
          'posts/p1_1.jpg': IMAGES.jpeg(1),
          'posts/p1_2.png': IMAGES.png(2),
          'posts/p1_3.webp': IMAGES.webp(3),
          'posts/p2_1.jpg': IMAGES.jpeg(4),
          'posts/p2_3.jpg': IMAGES.jpeg(5),
        }),
      ),
    );
    expect(result.posts.map((post) => [post.mediaType, post.media.map((image) => image.path)])).toEqual([
      ['carousel', ['posts/p1_1.jpg', 'posts/p1_2.png', 'posts/p1_3.webp']],
      ['carousel', ['posts/p2_1.jpg', 'posts/p2_3.jpg']],
      ['video', ['posts/p1_1.jpg']],
    ]);
    expect(result.warnings.map((warning) => [warning.code, warning.postIndex])).toEqual([['missing-image', 1]]);
    // The same file used by two posts is loaded once and shares one URL.
    expect(result.posts[2].media[0].url).toBe(result.posts[0].media[0].url);
  });

  it('gives duplicate ids a suffix and a warning', async () => {
    const result = await track(
      importFromArchive(
        zipFile({
          'post.json': json([
            { id: 'dup', user: 'a', image: 'posts/a.jpg' },
            { id: 'dup', user: 'a', image: 'posts/a.jpg' },
            { id: 'dup', user: 'a', image: 'posts/a.jpg' },
          ]),
          'posts/a.jpg': IMAGES.jpeg(),
        }),
      ),
    );
    expect(result.posts.map((post) => post.id)).toEqual(['dup', 'dup-2', 'dup-3']);
    expect(result.warnings.map((warning) => [warning.code, warning.postIndex])).toEqual([
      ['duplicate-id', 1],
      ['duplicate-id', 2],
    ]);
  });

  it('reports a missing avatar once, not once per post', async () => {
    const user = { username: 'a', avatar: 'avatars/a.jpg' };
    const remote = { username: 'b', avatar: 'https://cdn.example/b.jpg' };
    const result = await track(
      importFromArchive(
        zipFile({
          'post.json': json([
            { user, image: 'posts/a.jpg' },
            { user, image: 'posts/a.jpg' },
            { user: remote, image: 'posts/a.jpg' },
          ]),
          'posts/a.jpg': IMAGES.jpeg(),
        }),
      ),
    );
    expect(result.posts).toHaveLength(3);
    expect(result.posts.every((post) => post.user.avatarUrl === undefined)).toBe(true);
    expect(result.warnings).toEqual([expect.objectContaining({ code: 'missing-image', path: 'avatars/a.jpg' })]);
    expect(result.warnings[0].postIndex).toBeUndefined();
  });

  describe('errors', () => {
    it('no-manifest', async () => {
      const error = await failure(importFromArchive(zipFile({ 'posts/a.jpg': IMAGES.jpeg(), 'readme.txt': 'hi' })));
      expect(error.code).toBe('no-manifest');
      expect(error.message).toContain('post.json');
      // A manifest that only exists as macOS noise does not count.
      expect((await failure(importFromArchive(zipFile({ '__MACOSX/post.json': '[]', '._post.json': '[]' })))).code).toBe(
        'no-manifest',
      );
    });

    it('invalid-json', async () => {
      const error = await failure(importFromArchive(zipFile({ 'post.json': '{ "posts": [', 'posts/a.jpg': IMAGES.jpeg() })));
      expect(error.code).toBe('invalid-json');
    });

    it('invalid-manifest', async () => {
      expect((await failure(importFromArchive(zipFile({ 'post.json': '"just a string"' })))).code).toBe('invalid-manifest');
      expect((await failure(importFromArchive(zipFile({ 'post.json': '{"version":1}' })))).code).toBe('invalid-manifest');
    });

    it('no-posts', async () => {
      expect((await failure(importFromArchive(zipFile({ 'post.json': '{"posts":[]}' })))).code).toBe('no-posts');
      const error = await failure(
        importFromArchive(
          zipFile({
            'post.json': json([
              { user: 'a', image: 'posts/gone.jpg' },
              { user: 'a', image: 'posts/vector.svg' },
            ]),
            'posts/vector.svg': IMAGES.svg(),
          }),
        ),
      );
      expect(error.code).toBe('no-posts');
      expect(error.message).toContain('posts/gone.jpg');
    });

    it('no-posts names a problem that really stopped the import, not a side note', async () => {
      // Pictures in the wrong folder, entries that do not say who they are from.
      const wrongFolder = await failure(
        importFromArchive(
          zipFile({
            'post.json': json([
              { image: 'posts/post1.jpg', caption: 'a' },
              { image: 'posts/post2.jpg', caption: 'b' },
            ]),
            'images/post1.jpg': IMAGES.jpeg(),
            'images/post2.jpg': IMAGES.jpeg(),
          }),
        ),
      );
      expect(wrongFolder.code).toBe('no-posts');
      expect(wrongFolder.message).toContain('First problem: Post 1: the image "posts/post1.jpg" is not in the export.');

      // Duplicate ids and a link that is dropped, pictures missing.
      const duplicates = await failure(
        importFromArchive(
          zipFile({
            'post.json': json([
              { id: 'x', user: 'a', image: 'posts/1.jpg', url: 'https://evil.example/' },
              { id: 'x', user: 'a', image: 'posts/2.jpg' },
            ]),
          }),
        ),
      );
      expect(duplicates.message).toContain('First problem: Post 1: the image "posts/1.jpg" is not in the export.');

      // A folder that holds nothing but post.json.
      const alone = await failure(
        importFromFiles(folderFiles('export', { 'post.json': json([{ image: 'posts/1.jpg' }]), 'notes.txt': 'x' })),
      );
      expect(alone.message).toBe(
        'The only post in post.json could not be imported. First problem: Post 1: the image "posts/1.jpg" is not in the export.',
      );

      // The earliest post that failed is the one that is named, whatever stage dropped it.
      const mixed = await failure(
        importFromArchive(
          zipFile({
            'post.json': json([{ caption: 'no image at all' }, { image: 'posts/vector.svg' }, { image: 'https://cdn.example/1.jpg' }]),
            'posts/vector.svg': IMAGES.svg(),
          }),
        ),
      );
      expect(mixed.message).toContain('First problem: Post 1 has no image path and was skipped.');
      const later = await failure(
        importFromArchive(
          zipFile({
            'post.json': json([{ image: 'posts/vector.svg' }, { caption: 'no image at all' }]),
            'posts/vector.svg': IMAGES.svg(),
          }),
        ),
      );
      expect(later.message).toContain('First problem: Post 1: "posts/vector.svg" is not a supported image');
    });

    it('unsupported-archive', async () => {
      const rar = makeFile('export.rar', new Uint8Array([0x52, 0x61, 0x72, 0x21, 0x1a, 0x07, 0x00, 0x00]));
      const error = await failure(importFromArchive(rar));
      expect(error.code).toBe('unsupported-archive');
      expect(error.message).toMatch(/\.zip/);
      expect(error.message).toMatch(/folder/);
      expect((await failure(importFromArchive(makeFile('photo.jpg', IMAGES.jpeg())))).code).toBe('unsupported-archive');
    });

    it('read-failed', async () => {
      const zip = makeZip(EXPORT);
      expect((await failure(importFromArchive(makeFile('cut.zip', zip.slice(0, zip.length - 40))))).code).toBe('read-failed');
    });

    it('too-large', async () => {
      const file = zipFile({ ...EXPORT, 'post.json': json({ ...MANIFEST, padding: 'x'.repeat(5000) }) });
      expect((await failure(importArchive(file, { limits: { maxManifestBytes: 1000 } }))).code).toBe('too-large');
      expect((await failure(importArchive(zipFile(EXPORT), { limits: { maxTotalMediaBytes: 100 } }))).code).toBe('too-large');
      expect((await failure(importArchive(zipFile(EXPORT), { limits: { maxSourceEntries: 2 } }))).code).toBe('too-large');
    });
  });

  it('skips single images over the size limit and truncates long manifests', async () => {
    const files: FileMap = {
      'post.json': json([
        { user: 'a', image: 'posts/small.jpg' },
        { user: 'a', image: 'posts/large.jpg' },
        { user: 'a', image: 'posts/small.jpg' },
      ]),
      'posts/small.jpg': IMAGES.jpeg(1, 100),
      'posts/large.jpg': IMAGES.jpeg(0, 50_000),
    };
    const sized = await track(importArchive(zipFile(files), { limits: { maxImageBytes: 1000 } }));
    expect(sized.posts).toHaveLength(2);
    expect(sized.warnings.map((warning) => [warning.code, warning.postIndex])).toEqual([['unsupported-image', 1]]);

    const truncated = await track(importArchive(zipFile(files), { limits: { maxPosts: 1 } }));
    expect(truncated.posts).toHaveLength(1);
    expect(truncated.totalEntries).toBe(3);
    expect(truncated.warnings.map((warning) => warning.code)).toEqual(['truncated']);
  });

  it('does not leak object URLs when the import fails', async () => {
    const created = vi.spyOn(URL, 'createObjectURL');
    const revoked = vi.spyOn(URL, 'revokeObjectURL');
    // The second image blows the total budget after the first one already has a URL.
    const files: FileMap = {
      'post.json': json([
        { user: 'a', image: 'posts/1.jpg' },
        { user: 'a', image: 'posts/2.jpg' },
      ]),
      'posts/1.jpg': IMAGES.jpeg(1, 100),
      'posts/2.jpg': IMAGES.jpeg(2, 100),
    };
    expect((await failure(importArchive(zipFile(files), { limits: { maxTotalMediaBytes: 150 } }))).code).toBe('too-large');
    expect(created.mock.calls.length).toBeGreaterThan(0);
    expect(revoked.mock.calls.map((call) => call[0]).sort()).toEqual(created.mock.results.map((r) => r.value as string).sort());
  });
});

describe('HEIC / HEIF', () => {
  const heicExport = (): FileMap => ({
    'post.json': json([
      { id: 'one', user: 'a', image: 'posts/post1.heic' },
      { id: 'two', user: 'a', images: ['posts/post1.heic', 'posts/post2.HEIF'] },
      { id: 'three', user: 'a', image: 'posts/plain.jpg' },
    ]),
    'posts/post1.heic': IMAGES.heic(21),
    'posts/post2.HEIF': IMAGES.heifMif1(22),
    'posts/plain.jpg': IMAGES.jpeg(23),
  });

  it('converts to JPEG with the injected converter, once per file', async () => {
    const seen: Array<{ type: string; bytes: number[] }> = [];
    const converter = vi.fn(async (source: Blob) => {
      seen.push({ type: source.type, bytes: Array.from(await blobBytes(source)) });
      return new Blob([IMAGES.jpeg(99)], { type: 'image/jpeg' });
    });
    setHeicConverter(converter);
    const result = await track(importFromArchive(zipFile(heicExport())));

    expect(converter).toHaveBeenCalledTimes(2);
    expect(seen.map((call) => call.type).sort()).toEqual(['image/heic', 'image/heic']);
    expect(seen.map((call) => call.bytes[40]).sort()).toEqual([21, 22]);
    expect(result.warnings).toEqual([]);
    expect(result.posts[0].media).toEqual([
      { kind: 'image', url: expect.stringMatching(/^blob:/), path: 'posts/post1.heic', mime: 'image/jpeg', converted: true, bytes: 64 },
    ]);
    expect(await bytesBehind(result.posts[0].media[0].url)).toEqual(Array.from(IMAGES.jpeg(99)));
    expect(blobForUrl(result.posts[0].media[0].url)?.type).toBe('image/jpeg');
    expect(result.posts[1].media.map((image) => image.converted)).toEqual([true, true]);
    expect(result.posts[1].media[0].url).toBe(result.posts[0].media[0].url);
    expect(result.posts[2].media[0]).toEqual({ kind: 'image', url: expect.any(String), path: 'posts/plain.jpg', mime: 'image/jpeg', bytes: 64 });
  });

  it('labels the converted image by what the converter really produced', async () => {
    setHeicConverter(async () => new Blob([IMAGES.png(5)], { type: 'application/octet-stream' }));
    const result = await track(importFromArchive(zipFile(heicExport())));
    expect(result.posts[0].media[0]).toMatchObject({ mime: 'image/png', converted: true });
    expect(blobForUrl(result.posts[0].media[0].url)?.type).toBe('image/png');
  });

  it('keeps the original bytes when the browser decodes HEIC natively', async () => {
    const converter = vi.fn(async () => new Blob([IMAGES.jpeg(99)]));
    setHeicConverter(converter);
    setHeicNativeProbe(async () => true);
    const result = await track(importFromArchive(zipFile(heicExport())));
    expect(converter).not.toHaveBeenCalled();
    expect(result.posts[0].media[0]).toEqual({ kind: 'image', url: expect.any(String), path: 'posts/post1.heic', mime: 'image/heic', bytes: 64 });
    expect(await bytesBehind(result.posts[0].media[0].url)).toEqual(Array.from(IMAGES.heic(21)));
  });

  it('warns with conversion-failed and skips posts that have nothing else', async () => {
    setHeicConverter(async (source) => {
      const bytes = await blobBytes(source);
      if (bytes[40] === 21) throw new Error('decoder exploded');
      return new Blob([IMAGES.jpeg(99)], { type: 'image/jpeg' });
    });
    const result = await track(importFromArchive(zipFile(heicExport())));
    expect(result.posts.map((post) => post.id)).toEqual(['two', 'three']);
    expect(result.posts[0].media.map((image) => image.path)).toEqual(['posts/post2.HEIF']);
    expect(result.warnings.map((warning) => [warning.code, warning.postIndex, warning.path])).toEqual([
      ['conversion-failed', 0, 'posts/post1.heic'],
      ['conversion-failed', 1, 'posts/post1.heic'],
    ]);
    expect(result.warnings[0].message).toContain('decoder exploded');
  });

  it('treats converter output that is not an image as a failed conversion', async () => {
    setHeicConverter(async () => new Blob(['<html>not an image</html>'], { type: 'image/jpeg' }));
    const error = await failure(importFromArchive(zipFile({ 'post.json': json([{ user: 'a', image: 'p.heic' }]), 'p.heic': IMAGES.heic() })));
    expect(error.code).toBe('no-posts');
    expect(error.message).toMatch(/could not be converted/);
  });

  it('runs at most two conversions at a time', async () => {
    let active = 0;
    let peak = 0;
    setHeicConverter(async () => {
      active++;
      peak = Math.max(peak, active);
      await new Promise((resolve) => setTimeout(resolve, 5));
      active--;
      return new Blob([IMAGES.jpeg(99)], { type: 'image/jpeg' });
    });
    const files: FileMap = { 'post.json': '' };
    const posts = [];
    for (let i = 0; i < 9; i++) {
      files[`posts/${i}.heic`] = IMAGES.heic(i);
      posts.push({ user: 'a', image: `posts/${i}.heic` });
    }
    files['post.json'] = json(posts);
    const result = await track(importFromArchive(zipFile(files)));
    expect(result.posts).toHaveLength(9);
    expect(peak).toBe(2);
  });
});

describe('options', () => {
  it('reports progress through the phases with accurate counts', async () => {
    const events: ImportProgress[] = [];
    const result = await track(importFromArchive(zipFile(EXPORT), { onProgress: (progress) => events.push({ ...progress }) }));
    expect(result.posts).toHaveLength(2);

    const phases = events.map((event) => event.phase);
    expect([...new Set(phases)]).toEqual(['reading', 'manifest', 'images', 'done']);
    for (const event of events) {
      expect(event.done).toBeGreaterThanOrEqual(0);
      expect(event.done).toBeLessThanOrEqual(event.total);
      expect(event.message).not.toBe('');
    }
    const images = events.filter((event) => event.phase === 'images');
    expect(images.map((event) => event.total)).toEqual([3, 3, 3, 3]); // 2 post images + 1 avatar
    expect(images.map((event) => event.done)).toEqual([0, 1, 2, 3]);
    expect(events[events.length - 1]).toMatchObject({ phase: 'done', done: 3, total: 3 });
  });

  it('reports byte progress while unpacking a tar.gz', async () => {
    const events: ImportProgress[] = [];
    const file = makeFile('export.tar.gz', gzipSync(tarFromFiles(EXPORT)));
    await track(importFromArchive(file, { onProgress: (progress) => events.push({ ...progress }) }));
    const reading = events.filter((event) => event.phase === 'reading');
    expect(reading.some((event) => event.total === file.size && event.done === file.size)).toBe(true);
  });

  it('survives a progress callback that throws', async () => {
    const result = await track(
      importFromArchive(zipFile(EXPORT), {
        onProgress: () => {
          throw new Error('UI bug');
        },
      }),
    );
    expect(result.posts).toHaveLength(2);
  });

  it('rejects with "aborted" when the signal is already aborted', async () => {
    const controller = new AbortController();
    controller.abort();
    expect((await failure(importFromArchive(zipFile(EXPORT), { signal: controller.signal }))).code).toBe('aborted');
    expect((await failure(importFromFiles(folderFiles('x', EXPORT), { signal: controller.signal }))).code).toBe('aborted');
  });

  it.each(['manifest', 'images'] as const)('aborts during the %s phase and gives back its URLs', async (phase) => {
    const created = vi.spyOn(URL, 'createObjectURL');
    const revoked = vi.spyOn(URL, 'revokeObjectURL');
    const controller = new AbortController();
    const files: FileMap = { 'post.json': '' };
    const posts = [];
    for (let i = 0; i < 12; i++) {
      files[`posts/${i}.jpg`] = IMAGES.jpeg(i);
      posts.push({ user: 'a', image: `posts/${i}.jpg` });
    }
    files['post.json'] = json(posts);
    const error = await failure(
      importFromArchive(zipFile(files), {
        signal: controller.signal,
        onProgress: (progress) => {
          if (progress.phase === phase && (phase === 'manifest' || progress.done === 4)) controller.abort();
        },
      }),
    );
    expect(error.code).toBe('aborted');
    expect(created.mock.calls.length).toBeLessThan(12);
    expect(revoked.mock.calls.length).toBe(created.mock.calls.length);
  });

  it('stays cancellable while post.json is matched against the files', async () => {
    // Every look at the clock is a second later, so each time slice counts as used up.
    const file = zipFile(EXPORT);
    let now = Date.now();
    vi.spyOn(Date, 'now').mockImplementation(() => (now += 1000));
    const controller = new AbortController();
    const phases: string[] = [];
    const error = await failure(
      importFromArchive(file, {
        signal: controller.signal,
        onProgress: (progress) => {
          phases.push(progress.phase);
          // A Cancel click that arrives as an event, right after post.json was read.
          if (progress.phase === 'manifest' && progress.done === 1) setTimeout(() => controller.abort(), 0);
        },
      }),
    );
    expect(error.code).toBe('aborted');
    expect(phases).not.toContain('images');
  });

  it('aborts a running HEIC conversion', async () => {
    const controller = new AbortController();
    setHeicConverter(
      () =>
        new Promise<Blob>(() => {
          controller.abort(); // never settles: only the abort can end this
        }),
    );
    const file = zipFile({ 'post.json': json([{ user: 'a', image: 'p.heic' }]), 'p.heic': IMAGES.heic() });
    expect((await failure(importFromArchive(file, { signal: controller.signal }))).code).toBe('aborted');
  });
});

describe('importFromFiles', () => {
  it('imports a folder picked with webkitdirectory', async () => {
    const files = folderFiles('my-export', { ...EXPORT, '.DS_Store': 'noise', 'posts/._post1.jpg': 'noise' });
    const result = await track(importFromFiles(files));
    expect(result.sourceName).toBe('my-export');
    expect(result.sourceKind).toBe('folder');
    expect(result.posts.map((post) => post.id)).toEqual(['AAA111', 'BBB222']);
    expect(result.warnings).toEqual([]);
    expect(await bytesBehind(result.posts[0].media[0].url)).toEqual(Array.from(IMAGES.jpeg(11)));
    expect(result.posts[0].user.avatarUrl).toBeDefined();
  });

  it('accepts a FileList-like object and an iterable', async () => {
    const files = folderFiles('exp', EXPORT);
    const arrayLike: { length: number; [index: number]: File } = { length: files.length };
    files.forEach((file, index) => {
      arrayLike[index] = file;
    });
    expect((await track(importFromFiles(arrayLike))).posts).toHaveLength(2);
    expect((await track(importFromFiles(new Set(files)))).posts).toHaveLength(2);
  });

  it('imports a folder whose export sits one level deeper', async () => {
    const nested: FileMap = {};
    for (const [path, content] of Object.entries(EXPORT)) nested[`2026-11-10/${path}`] = content;
    const result = await track(importFromFiles(folderFiles('Downloads', nested)));
    expect(result.posts).toHaveLength(2);
    expect(result.sourceName).toBe('Downloads');
  });

  it('imports loose files without any folder information', async () => {
    const files = [
      makeFile('post.json', EXPORT['post.json']),
      makeFile('post1.jpg', EXPORT['posts/post1.jpg']),
      makeFile('post2.png', EXPORT['posts/post2.png']),
    ];
    const result = await track(importFromFiles(files));
    expect(result.sourceName).toBe('Selected files');
    expect(result.sourceKind).toBe('folder');
    expect(result.posts.map((post) => post.media[0].path)).toEqual(['posts/post1.jpg', 'posts/post2.png']);
    expect(result.warnings.map((warning) => warning.code)).toEqual(['missing-image']); // the avatar
  });

  it('delegates a single archive file to the archive import', async () => {
    const result = await track(importFromFiles([zipFile(EXPORT, 'my.zip')]));
    expect(result.sourceKind).toBe('archive');
    expect(result.sourceName).toBe('my.zip');
    // Even when the name and type say nothing, the content decides.
    const anonymous = await track(importFromFiles([makeFile('download', makeZip(EXPORT))]));
    expect(anonymous.sourceKind).toBe('archive');
  });

  it('opens the archive inside a picked folder that has no post.json', async () => {
    const files = folderFiles('Downloads', { 'export.zip': makeZip(EXPORT), 'notes.txt': 'hi' });
    const result = await track(importFromFiles(files));
    expect(result.sourceKind).toBe('archive');
    expect(result.posts).toHaveLength(2);
  });

  it('fails helpfully for empty selections, a lone post.json and folders without a manifest', async () => {
    expect((await failure(importFromFiles([]))).code).toBe('no-manifest');
    expect((await failure(importFromFiles(null as unknown as File[]))).code).toBe('no-manifest');
    const lone = await failure(importFromFiles([makeFile('post.json', EXPORT['post.json'])]));
    expect(lone.code).toBe('no-posts');
    expect(lone.message).toMatch(/folder/);
    expect((await failure(importFromFiles(folderFiles('pics', { 'a.jpg': IMAGES.jpeg(), 'b.jpg': IMAGES.jpeg() })))).code).toBe(
      'no-manifest',
    );
    expect((await failure(importFromFiles(folderFiles('pics', { 'a.jpg': IMAGES.jpeg() })))).code).toBe('no-manifest');
  });

  it('enforces the file count limit', async () => {
    const error = await failure(importPathedFiles(
      folderFiles('x', EXPORT).map((file) => ({ path: file.webkitRelativePath, file })),
      { limits: { maxSourceEntries: 3 } },
    ));
    expect(error.code).toBe('too-large');
  });
});

describe('releaseImport', () => {
  it('revokes every object URL exactly once and is idempotent', async () => {
    const result = await importFromArchive(
      zipFile({
        'post.json': json([
          { user: { username: 'a', avatar: 'avatars/a.jpg' }, images: ['posts/1.jpg', 'posts/2.jpg'] },
          { user: { username: 'a', avatar: 'avatars/a.jpg' }, image: 'posts/1.jpg' },
        ]),
        'posts/1.jpg': IMAGES.jpeg(1),
        'posts/2.jpg': IMAGES.jpeg(2),
        'avatars/a.jpg': IMAGES.jpeg(3),
      }),
    );
    const urls = new Set<string>();
    for (const post of result.posts) {
      for (const image of post.media) urls.add(image.url);
      if (post.user.avatarUrl) urls.add(post.user.avatarUrl);
    }
    expect(urls.size).toBe(3);

    const revoked = vi.spyOn(URL, 'revokeObjectURL');
    releaseImport(result);
    expect(revoked.mock.calls.map((call) => call[0]).sort()).toEqual([...urls].sort());
    for (const url of urls) expect(blobForUrl(url)).toBeUndefined();

    releaseImport(result);
    releaseImport(structuredClone(result));
    expect(revoked).toHaveBeenCalledTimes(3);
  });

  it('gives back the avatar of a post that was skipped', async () => {
    const created = vi.spyOn(URL, 'createObjectURL');
    const revoked = vi.spyOn(URL, 'revokeObjectURL');
    const result = await track(
      importFromArchive(
        zipFile({
          'post.json': json([
            { user: { username: 'gone', avatar: 'avatars/gone.jpg' }, image: 'posts/missing.jpg' },
            { user: 'a', image: 'posts/1.jpg' },
          ]),
          'posts/1.jpg': IMAGES.jpeg(1),
          'avatars/gone.jpg': IMAGES.jpeg(3),
        }),
      ),
    );
    expect(result.posts).toHaveLength(1);
    expect(created).toHaveBeenCalledTimes(2);
    expect(revoked).toHaveBeenCalledTimes(1);
    expect(revoked.mock.calls[0][0]).not.toBe(result.posts[0].media[0].url);
  });
});

// --- videos ------------------------------------------------------------------------------

describe('videos', () => {
  const VIDEO_EXPORT: FileMap = {
    'post.json': json({
      format: 'instagram-export',
      version: 1,
      posts: [
        {
          id: 'C5xYz',
          url: 'https://www.instagram.com/reel/C5xYz/',
          media_type: 'video',
          media: [{ type: 'video', path: 'posts/post5.mp4', poster: 'posts/post5_poster.jpg' }],
          user: { username: 'someone', avatar: 'avatars/someone.jpg' },
          caption: 'A reel',
          taken_at: '2026-10-01T12:00:00Z',
        },
        { id: 'MIX', user: 'someone', media: ['posts/a.jpg', { path: 'posts/b.webm', alt: 'clip' }, 'posts/c.mov'] },
        { id: 'BARE', user: 'someone', video: 'posts/bare.m4v' },
      ],
    }),
    'posts/post5.mp4': VIDEOS.mp4(41, 400),
    'posts/post5_poster.jpg': IMAGES.jpeg(42),
    'posts/a.jpg': IMAGES.jpeg(43),
    'posts/b.webm': VIDEOS.webm(44, 300),
    'posts/c.mov': VIDEOS.mov(45, 200),
    'posts/bare.m4v': VIDEOS.m4v(46, 100),
    'avatars/someone.jpg': IMAGES.jpeg(47),
  };

  const sources: Array<[string, () => File[]]> = [
    ['a folder', () => folderFiles('my-export', VIDEO_EXPORT)],
    ['a zip written without compression', () => [makeFile('stored.zip', makeZip(VIDEO_EXPORT, true), 'application/zip')]],
    ['a deflated zip', () => [zipFile(VIDEO_EXPORT)]],
    ['a plain tar', () => [makeFile('export.tar', tarFromFiles(VIDEO_EXPORT))]],
    ['a tar.gz', () => [makeFile('export.tar.gz', gzipSync(tarFromFiles(VIDEO_EXPORT)))]],
  ];

  it.each(sources)('imports MP4, M4V, MOV and WebM from %s, with posters and the right types', async (_label, make) => {
    const result = await track(importFromFiles(make()));
    expect(result.warnings).toEqual([]);
    const [reel, mixed, bare] = result.posts;
    expect(reel.mediaType).toBe('video');
    expect(reel.media).toEqual([
      {
        kind: 'video',
        url: expect.stringMatching(/^blob:/),
        path: 'posts/post5.mp4',
        mime: 'video/mp4',
        bytes: 400,
        posterUrl: expect.stringMatching(/^blob:/),
        posterPath: 'posts/post5_poster.jpg',
      },
    ]);
    expect(await bytesBehind(reel.media[0].url)).toEqual(Array.from(VIDEOS.mp4(41, 400)));
    expect(blobForUrl(reel.media[0].url)?.type).toBe('video/mp4');
    expect(await bytesBehind(reel.media[0].posterUrl as string)).toEqual(Array.from(IMAGES.jpeg(42)));
    expect(mixed.mediaType).toBe('carousel');
    expect(mixed.media.map((item) => [item.kind, item.mime, item.alt])).toEqual([
      ['image', 'image/jpeg', undefined],
      ['video', 'video/webm', 'clip'],
      ['video', 'video/quicktime', undefined],
    ]);
    // A lone file that turns out to be a video makes a video post.
    expect(bare.mediaType).toBe('video');
    expect(bare.media[0]).toMatchObject({ kind: 'video', mime: 'video/mp4' });
  });

  it('plays videos from the source instead of copying them, while pictures are copied', async () => {
    const source = onDisk(folderFiles('my-export', VIDEO_EXPORT));
    const result = await track(importFromFiles(source.files));
    source.remove();
    expect(await bytesBehind(result.posts[0].media[0].posterUrl as string)).toEqual(Array.from(IMAGES.jpeg(42)));
    await expect(bytesBehind(result.posts[0].media[0].url)).rejects.toThrow();
  });

  it('trusts the bytes over the declared type and the extension', async () => {
    const result = await track(
      importFromArchive(
        zipFile({
          'post.json': json([
            { user: 'a', media: [{ type: 'video', path: 'posts/really-a-picture.mp4' }] },
            { user: 'a', image: 'posts/really-a-video.jpg' },
          ]),
          'posts/really-a-picture.mp4': IMAGES.png(1),
          'posts/really-a-video.jpg': VIDEOS.webm(2),
        }),
      ),
    );
    expect(result.posts.map((post) => [post.mediaType, post.media[0].kind, post.media[0].mime])).toEqual([
      ['image', 'image', 'image/png'],
      ['video', 'video', 'video/webm'],
    ]);
  });

  it('skips unsupported video formats with an unsupported-media warning that names the format', async () => {
    const result = await track(
      importFromArchive(
        zipFile({
          'post.json': json([
            { user: 'a', image: 'posts/ok.jpg' },
            { user: 'a', video: 'posts/a.mkv' },
            { user: 'a', video: 'posts/b.avi' },
            { user: 'a', video: 'posts/c.mp4' },
            { user: 'a', image: 'posts/d.wmv' },
            { user: 'a', video: 'posts/missing.mp4' },
          ]),
          'posts/ok.jpg': IMAGES.jpeg(),
          'posts/a.mkv': VIDEOS.mkv(),
          'posts/b.avi': VIDEOS.avi(),
          'posts/c.mp4': strToU8('not a video at all, just some text'),
          'posts/d.wmv': VIDEOS.wmv(),
        }),
      ),
    );
    expect(result.posts).toHaveLength(1);
    expect(result.warnings.map((warning) => [warning.code, warning.postIndex, warning.path])).toEqual([
      ['unsupported-media', 1, 'posts/a.mkv'],
      ['unsupported-media', 2, 'posts/b.avi'],
      ['unsupported-media', 3, 'posts/c.mp4'],
      ['unsupported-media', 4, 'posts/d.wmv'],
      ['missing-image', 5, 'posts/missing.mp4'],
    ]);
    const messages = result.warnings.map((warning) => warning.message);
    expect(messages[0]).toContain('is a Matroska (MKV) video, which is not supported');
    expect(messages[1]).toContain('is an AVI video');
    expect(messages[2]).toContain('is not a supported video (MP4, MOV or WebM)');
    expect(messages[4]).toContain('the video "posts/missing.mp4" is not in the export');
  });

  it('imports a video without a poster, and a video whose poster is missing or unusable with a note', async () => {
    const result = await track(
      importFromArchive(
        zipFile({
          'post.json': json([
            { id: 'plain', user: 'a', video: 'posts/v1.mp4' },
            { id: 'gone', user: 'a', media: [{ path: 'posts/v2.mp4', poster: 'posts/gone.jpg' }] },
            { id: 'clip', user: 'a', media: [{ path: 'posts/v3.mp4', poster: 'posts/v1.mp4' }] },
            { id: 'svg', user: 'a', media: [{ path: 'posts/v4.mp4', poster: 'posts/p.svg' }] },
          ]),
          'posts/v1.mp4': VIDEOS.mp4(1),
          'posts/v2.mp4': VIDEOS.mp4(2),
          'posts/v3.mp4': VIDEOS.mp4(3),
          'posts/v4.mp4': VIDEOS.mp4(4),
          'posts/p.svg': IMAGES.svg(),
        }),
      ),
    );
    expect(result.posts.map((post) => [post.id, post.media[0].kind, post.media[0].posterUrl])).toEqual([
      ['plain', 'video', undefined],
      ['gone', 'video', undefined],
      ['clip', 'video', undefined],
      ['svg', 'video', undefined],
    ]);
    expect(result.warnings.map((warning) => [warning.code, warning.postIndex, warning.path])).toEqual([
      ['missing-image', 1, 'posts/gone.jpg'],
      ['unsupported-image', 2, 'posts/v1.mp4'],
      ['unsupported-image', 3, 'posts/p.svg'],
    ]);
    expect(result.warnings.map((warning) => warning.message)).toEqual([
      'Post 2: the video "posts/v2.mp4" is shown without its poster: "posts/gone.jpg" is not in the export.',
      'Post 3: the video "posts/v3.mp4" is shown without its poster: "posts/v1.mp4" is a video, not a picture.',
      expect.stringMatching(/^Post 4: the video "posts\/v4.mp4" is shown without its poster: "posts\/p.svg" is not a supported image/),
    ]);
  });

  it('refuses a video as a profile picture, and gives back posters that pictures do not use', async () => {
    const created = vi.spyOn(URL, 'createObjectURL');
    const revoked = vi.spyOn(URL, 'revokeObjectURL');
    const result = await track(
      importFromArchive(
        zipFile({
          'post.json': json([
            { user: { username: 'a', avatar: 'avatars/a.mp4' }, media: [{ path: 'posts/p.jpg', poster: 'posts/poster.jpg' }] },
          ]),
          'posts/p.jpg': IMAGES.jpeg(1),
          'posts/poster.jpg': IMAGES.jpeg(2),
          'avatars/a.mp4': VIDEOS.mp4(3),
        }),
      ),
    );
    expect(result.posts[0].user.avatarUrl).toBeUndefined();
    expect(result.posts[0].media[0].posterUrl).toBeUndefined();
    expect(result.warnings.map((warning) => [warning.code, warning.path])).toEqual([['unsupported-image', 'avatars/a.mp4']]);
    expect(result.warnings[0].message).toContain('is a video, not a picture');
    // The picture and the unused poster were loaded; the avatar was refused before any URL existed.
    expect(created).toHaveBeenCalledTimes(2);
    expect(revoked).toHaveBeenCalledTimes(1);
    expect(revoked.mock.calls[0][0]).not.toBe(result.posts[0].media[0].url);
  });

  it('applies the video size limit to videos and the image limit to pictures', async () => {
    const files: FileMap = {
      'post.json': json([
        { user: 'a', video: 'posts/small.mp4' },
        { user: 'a', video: 'posts/big.mp4' },
        { user: 'a', image: 'posts/big.jpg' },
      ]),
      'posts/small.mp4': VIDEOS.mp4(1, 500),
      'posts/big.mp4': VIDEOS.mp4(2, 1500),
      'posts/big.jpg': IMAGES.jpeg(3, 600),
    };
    for (const file of [zipFile(files), makeFile('x.tar.gz', gzipSync(tarFromFiles(files)))]) {
      const result = await track(importArchive(file, { limits: { maxImageBytes: 100, maxVideoBytes: 1000 } }));
      expect(result.posts.map((post) => post.media[0].path)).toEqual(['posts/small.mp4']);
      expect(result.warnings.map((warning) => [warning.code, warning.postIndex])).toEqual([
        ['unsupported-media', 1],
        ['unsupported-image', 2],
      ]);
      expect(result.warnings[1].message).toContain('larger than 100 bytes');
    }
  });

  it('counts videos against the total media budget', async () => {
    const files: FileMap = {
      'post.json': json([{ user: 'a', video: 'posts/v.mp4' }, { user: 'a', image: 'posts/p.jpg' }]),
      'posts/v.mp4': VIDEOS.mp4(1, 500),
      'posts/p.jpg': IMAGES.jpeg(2),
    };
    const error = await failure(importArchive(zipFile(files), { limits: { maxTotalMediaBytes: 520 } }));
    expect(error.code).toBe('too-large');
    expect(error.message).toContain('pictures and videos');
  });
});

// --- drag and drop -----------------------------------------------------------------------

describe('importFromDataTransfer', () => {
  it('walks a dropped folder, reading every batch of every directory', async () => {
    const posts = [];
    const postFiles: FakeEntry[] = [];
    for (let i = 0; i < 7; i++) {
      postFiles.push(fileEntry(makeFile(`post${i}.jpg`, IMAGES.jpeg(i))));
      posts.push({ user: 'a', image: `posts\\post${i}.jpg` });
    }
    const folder = dirEntry('dropped-export', [
      fileEntry(makeFile('.DS_Store', 'noise')),
      dirEntry('posts', postFiles, 3),
      dirEntry('__MACOSX', [fileEntry(makeFile('post.json', 'not json'))]),
      fileEntry(makeFile('post.json', json(posts))),
    ]);
    const { dataTransfer, expire } = fakeDataTransfer([{ entry: folder }]);
    const promise = importFromDataTransfer(dataTransfer);
    expire(); // the handler returns before anything asynchronous has happened
    const result = await track(promise);
    expect(result.sourceName).toBe('dropped-export');
    expect(result.sourceKind).toBe('folder');
    expect(result.posts).toHaveLength(7);
    expect(result.warnings).toEqual([]);
  });

  it('imports a dropped archive', async () => {
    const archive = zipFile(EXPORT, 'dropped.zip');
    const { dataTransfer, expire } = fakeDataTransfer([{ entry: fileEntry(archive), file: archive }]);
    const promise = importFromDataTransfer(dataTransfer);
    expire();
    const result = await track(promise);
    expect(result.sourceKind).toBe('archive');
    expect(result.sourceName).toBe('dropped.zip');
    expect(result.posts).toHaveLength(2);
  });

  it('imports post.json dropped together with the posts folder', async () => {
    const manifest = makeFile('post.json', EXPORT['post.json']);
    const postsDir = dirEntry('posts', [
      fileEntry(makeFile('post1.jpg', EXPORT['posts/post1.jpg'])),
      fileEntry(makeFile('post2.png', EXPORT['posts/post2.png'])),
    ]);
    const { dataTransfer, expire } = fakeDataTransfer([
      { entry: fileEntry(manifest), file: manifest },
      { entry: postsDir },
      { kind: 'string' },
    ]);
    const promise = importFromDataTransfer(dataTransfer);
    expire();
    const result = await track(promise);
    expect(result.sourceName).toBe('Selected files');
    expect(result.posts.map((post) => post.media[0].path)).toEqual(['posts/post1.jpg', 'posts/post2.png']);
  });

  it('falls back to dataTransfer.files when there are no items', async () => {
    const { dataTransfer } = fakeDataTransfer([], [zipFile(EXPORT, 'fallback.zip')]);
    const result = await track(importFromDataTransfer(dataTransfer));
    expect(result.sourceName).toBe('fallback.zip');
  });

  it('rejects (never throws synchronously) for empty or broken drops', async () => {
    expect((await failure(importFromDataTransfer(fakeDataTransfer([]).dataTransfer))).code).toBe('no-manifest');
    expect((await failure(importFromDataTransfer(fakeDataTransfer([{ entry: dirEntry('empty', []) }]).dataTransfer))).code).toBe(
      'no-manifest',
    );
    const broken = {
      get items(): never {
        throw new Error('boom');
      },
    } as unknown as DataTransfer;
    expect((await failure(importFromDataTransfer(broken))).code).toBe('read-failed');
  });
});
