import 'fake-indexeddb/auto';
import { IDBFactory } from 'fake-indexeddb';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  IMPORT_DB_NAME,
  clearSavedImport,
  importFromArchive,
  loadSavedImport,
  mergeImportResults,
  releaseImport,
  saveImport,
  setHeicConverter,
  setHeicNativeProbe,
  type ImportResult,
} from './index';
import { blobForUrl } from './registry';
import { IMAGES, VIDEOS, blobBytes, json, makeFile, makeZip, type FileMap } from './testkit';

const EXPORT: FileMap = {
  'post.json': json({
    exported_at: '2026-11-10T09:30:00Z',
    source: { type: 'profile', url: 'https://www.instagram.com/natgeo/' },
    posts: [
      {
        id: 'AAA111',
        url: 'https://www.instagram.com/p/AAA111/',
        images: ['posts/post1.jpg', 'posts/post4.heic'],
        user: { username: 'natgeo', full_name: 'National Geographic', verified: true, avatar: 'avatars/natgeo.jpg' },
        caption: 'First',
        comment_count: 1234,
        like_count: 56789,
        taken_at: '2026-11-01T15:04:05Z',
        alt: 'alt text',
        location: 'Paris, France',
        comments: [{ username: 'someone', text: 'Wow', like_count: 3 }],
      },
      { id: 'BBB222', image: 'posts/post1.jpg', user: 'natgeo', like_count: null },
      { id: 'CCC333', image: 'posts/missing.jpg', user: 'natgeo' },
    ],
  }),
  'posts/post1.jpg': IMAGES.jpeg(11),
  'posts/post4.heic': IMAGES.heic(14),
  'avatars/natgeo.jpg': IMAGES.jpeg(13),
};

const live: ImportResult[] = [];

async function importExport(files: FileMap = EXPORT): Promise<ImportResult> {
  const result = await importFromArchive(makeFile('export.zip', makeZip(files)));
  live.push(result);
  return result;
}

async function load(): Promise<ImportResult | null> {
  const result = await loadSavedImport();
  if (result) live.push(result);
  return result;
}

/** The result with every object URL replaced by the bytes it points to. */
async function materialise(result: ImportResult): Promise<unknown> {
  const bytes = async (url: string | undefined) => {
    if (url === undefined) return undefined;
    const blob = blobForUrl(url);
    if (!blob) throw new Error(`no blob behind ${url}`);
    return { type: blob.type, bytes: Array.from(await blobBytes(blob)) };
  };
  return {
    ...result,
    posts: await Promise.all(
      result.posts.map(async (post) => ({
        ...post,
        media: await Promise.all(
          post.media.map(async (item) => ({ ...item, url: await bytes(item.url), posterUrl: await bytes(item.posterUrl) })),
        ),
        user: { ...post.user, avatarUrl: await bytes(post.user.avatarUrl) },
      })),
    ),
  };
}

function allUrls(result: ImportResult): string[] {
  return result.posts.flatMap((post) => [
    ...post.media.flatMap((item) => (item.posterUrl ? [item.url, item.posterUrl] : [item.url])),
    ...(post.user.avatarUrl ? [post.user.avatarUrl] : []),
  ]);
}

/** Changes the stored metadata record in place, as another (older or newer) version could have written it. */
function rewrite(change: (meta: Record<string, unknown>) => void): Promise<void> {
  return new Promise<void>((resolve, reject) => {
    const open = indexedDB.open(IMPORT_DB_NAME);
    open.onsuccess = () => {
      const db = open.result;
      const name = db.objectStoreNames[0];
      const transaction = db.transaction(name, 'readwrite');
      const store = transaction.objectStore(name);
      const get = store.get('meta');
      get.onsuccess = () => {
        const meta = get.result as Record<string, unknown>;
        change(meta);
        store.put(meta, 'meta');
      };
      transaction.oncomplete = () => {
        db.close();
        resolve();
      };
      transaction.onerror = () => reject(transaction.error);
    };
  });
}

beforeEach(() => {
  globalThis.indexedDB = new IDBFactory();
  setHeicNativeProbe(async () => false);
  setHeicConverter(async () => new Blob([IMAGES.jpeg(99)], { type: 'image/jpeg' }));
});

afterEach(() => {
  for (const result of live.splice(0)) releaseImport(result);
  setHeicConverter(null);
  setHeicNativeProbe(null);
  vi.restoreAllMocks();
});

describe('saveImport / loadSavedImport / clearSavedImport', () => {
  it('uses the documented database name', () => {
    expect(IMPORT_DB_NAME).toBe('ig-clone-import');
  });

  it('returns null when nothing was saved', async () => {
    expect(await loadSavedImport()).toBeNull();
  });

  it('round-trips an import with fresh object URLs and identical content', async () => {
    const original = await importExport();
    expect(original.posts).toHaveLength(2);
    expect(original.warnings).toHaveLength(1);
    expect(await saveImport(original)).toBe(true);

    const restored = await load();
    expect(restored).not.toBeNull();
    const loaded = restored as ImportResult;
    expect(await materialise(loaded)).toEqual(await materialise(original));

    // Fresh URLs, independent of the original result.
    const originalUrls = new Set(allUrls(original));
    for (const url of allUrls(loaded)) expect(originalUrls.has(url)).toBe(false);
    // Shared files still share one URL after loading.
    expect(loaded.posts[1].media[0].url).toBe(loaded.posts[0].media[0].url);
    expect(new Set(allUrls(loaded)).size).toBe(3);

    releaseImport(original);
    expect(await materialise(loaded)).toMatchObject({ sourceName: 'export.zip', sourceKind: 'archive', totalEntries: 3 });
  });

  it('stores the converted image, so HEIC is not converted again on load', async () => {
    const converter = vi.fn(async () => new Blob([IMAGES.jpeg(77)], { type: 'image/jpeg' }));
    setHeicConverter(converter);
    const original = await importExport();
    expect(converter).toHaveBeenCalledTimes(1);
    expect(await saveImport(original)).toBe(true);

    const loaded = (await load()) as ImportResult;
    expect(converter).toHaveBeenCalledTimes(1);
    const heic = loaded.posts[0].media[1];
    expect(heic).toMatchObject({ path: 'posts/post4.heic', mime: 'image/jpeg', converted: true });
    expect(Array.from(await blobBytes(blobForUrl(heic.url) as Blob))).toEqual(Array.from(IMAGES.jpeg(77)));
  });

  it('survives the release of the original and a "reload" (fresh module state is not needed)', async () => {
    const original = await importExport();
    expect(await saveImport(original)).toBe(true);
    releaseImport(original);
    const first = (await load()) as ImportResult;
    const second = (await load()) as ImportResult;
    expect(first.posts).toHaveLength(2);
    expect(await materialise(second)).toEqual(await materialise(first));
    // A loaded result can be saved again.
    releaseImport(second);
    expect(await saveImport(first)).toBe(true);
  });

  it('clears the saved import', async () => {
    expect(await saveImport(await importExport())).toBe(true);
    await clearSavedImport();
    expect(await loadSavedImport()).toBeNull();
    await clearSavedImport(); // clearing nothing is fine
  });

  it('replaces the previous import completely', async () => {
    expect(await saveImport(await importExport())).toBe(true);
    const other = await importExport({
      'post.json': json([{ id: 'ZZZ', user: 'other', image: 'posts/z.png' }]),
      'posts/z.png': IMAGES.png(42),
    });
    expect(await saveImport(other)).toBe(true);
    const loaded = (await load()) as ImportResult;
    expect(loaded.posts.map((post) => post.id)).toEqual(['ZZZ']);

    // No blob of the first import is left behind.
    const keys = await new Promise<IDBValidKey[]>((resolve, reject) => {
      const open = indexedDB.open(IMPORT_DB_NAME);
      open.onerror = () => reject(open.error);
      open.onsuccess = () => {
        const db = open.result;
        const request = db.transaction(db.objectStoreNames[0]).objectStore(db.objectStoreNames[0]).getAllKeys();
        request.onsuccess = () => {
          db.close();
          resolve(request.result);
        };
        request.onerror = () => reject(request.error);
      };
    });
    expect(keys).toHaveLength(2); // one blob + the metadata record
  });

  it('lets the last writer win when saves and clears race', async () => {
    const first = await importExport();
    const second = await importExport({
      'post.json': json([{ id: 'SECOND', user: 'other', image: 'posts/z.png' }]),
      'posts/z.png': IMAGES.png(42),
    });

    // Two saves started back to back: only the newer one is written.
    const [a, b] = await Promise.all([saveImport(first), saveImport(second)]);
    expect([a, b]).toEqual([false, true]);
    expect(((await load()) as ImportResult).posts.map((post) => post.id)).toEqual(['SECOND']);

    // A save overtaken by a clear must not resurrect the import.
    const overtaken = saveImport(first);
    const cleared = clearSavedImport();
    expect(await overtaken).toBe(false);
    await cleared;
    expect(await loadSavedImport()).toBeNull();

    // A clear followed by a save leaves the save.
    const clearedFirst = clearSavedImport();
    const saved = saveImport(first);
    await clearedFirst;
    expect(await saved).toBe(true);
    expect(((await load()) as ImportResult).posts.map((post) => post.id)).toEqual(['AAA111', 'BBB222']);
  });

  it('snapshots the blobs synchronously, so releasing right after saving is safe', async () => {
    const original = await importExport();
    const saving = saveImport(original);
    releaseImport(original);
    expect(await saving).toBe(true);
    expect(((await load()) as ImportResult).posts).toHaveLength(2);
  });

  it('refuses to save a result that was already released', async () => {
    const original = await importExport();
    releaseImport(original);
    expect(await saveImport(original)).toBe(false);
    expect(await loadSavedImport()).toBeNull();
  });

  it('keeps the previous import when a save fails half way', async () => {
    const first = await importExport();
    expect(await saveImport(first)).toBe(true);
    const second = await importExport({
      'post.json': json([{ id: 'SECOND', user: 'other', image: 'posts/z.png' }]),
      'posts/z.png': IMAGES.png(42),
    });
    // Quota exceeded while writing: the put throws inside the transaction.
    const put = vi.spyOn(Object.getPrototypeOf(await objectStoreProbe()), 'put').mockImplementation(() => {
      throw new DOMException('quota', 'QuotaExceededError');
    });
    expect(await saveImport(second)).toBe(false);
    put.mockRestore();
    expect(((await load()) as ImportResult).posts.map((post) => post.id)).toEqual(['AAA111', 'BBB222']);
  });

  it('never throws when IndexedDB is missing or broken', async () => {
    const original = await importExport();

    // @ts-expect-error simulate a browser without IndexedDB
    delete globalThis.indexedDB;
    expect(await saveImport(original)).toBe(false);
    expect(await loadSavedImport()).toBeNull();
    await expect(clearSavedImport()).resolves.toBeUndefined();

    Object.defineProperty(globalThis, 'indexedDB', {
      configurable: true,
      get() {
        throw new DOMException('denied', 'SecurityError');
      },
    });
    expect(await saveImport(original)).toBe(false);
    expect(await loadSavedImport()).toBeNull();
    await expect(clearSavedImport()).resolves.toBeUndefined();

    const failing = {
      open: () => {
        const request = { error: new DOMException('nope', 'InvalidStateError') } as Record<string, unknown>;
        setTimeout(() => (request.onerror as () => void)?.(), 0);
        return request;
      },
    };
    Object.defineProperty(globalThis, 'indexedDB', { configurable: true, writable: true, value: failing });
    expect(await saveImport(original)).toBe(false);
    expect(await loadSavedImport()).toBeNull();
    await expect(clearSavedImport()).resolves.toBeUndefined();

    // @ts-expect-error garbage instead of a result must not throw either
    expect(await saveImport(null)).toBe(false);
    // @ts-expect-error
    expect(await saveImport({ posts: 'nope' })).toBe(false);
  });

  it('ignores stored data it does not understand and re-checks links on load', async () => {
    const original = await importExport();
    expect(await saveImport(original)).toBe(true);

    // Tamper with the stored record: other schema versions are ignored, links are re-sanitised.
    await rewrite((meta) => {
      const posts = meta.posts as Array<Record<string, unknown>>;
      posts[0].permalink = 'javascript:alert(1)';
      (posts[0].user as Record<string, unknown>).profileUrl = 'javascript:alert(1)';
      meta.sourceUrl = 'data:text/html,x';
    });
    const loaded = (await load()) as ImportResult;
    expect(loaded.posts[0].permalink).toBeUndefined();
    expect(loaded.posts[0].user.profileUrl).toBeUndefined();
    expect(loaded.sourceUrl).toBeUndefined();
    expect(loaded.posts[1].user.profileUrl).toBe('https://www.instagram.com/natgeo/');

    // Links saved by an older version that still accepted any instagram.com path.
    await rewrite((meta) => {
      const posts = meta.posts as Array<Record<string, unknown>>;
      posts[0].permalink = 'https://www.instagram.com/accounts/login/?next=%2Fp%2Fabc%2F';
      (posts[0].user as Record<string, unknown>).profileUrl = 'https://www.instagram.com/accounts/logout/';
      posts[1].permalink = 'https://www.instagram.com/accounts/logout/';
      meta.sourceUrl = 'https://www.instagram.com/oauth/authorize?client_id=1';
    });
    const older = (await load()) as ImportResult;
    expect(older.posts.map((post) => post.permalink)).toEqual([undefined, undefined]);
    expect(older.posts[0].user.profileUrl).toBeUndefined();
    expect(older.sourceUrl).toBeUndefined();

    await rewrite((meta) => {
      meta.schema = 999;
    });
    expect(await loadSavedImport()).toBeNull();
  });
});

describe('saved imports with videos, several sources and older records', () => {
  const VIDEO_EXPORT: FileMap = {
    'post.json': json([
      {
        id: 'VID1',
        user: { username: 'mira', avatar: 'avatars/mira.jpg' },
        taken_at: '2026-10-01T12:00:00Z',
        media: [
          { type: 'video', path: 'posts/clip.mp4', poster: 'posts/clip_poster.jpg', alt: 'A clip' },
          'posts/still.png',
        ],
      },
    ]),
    'posts/clip.mp4': VIDEOS.mp4(31, 300),
    'posts/clip_poster.jpg': IMAGES.jpeg(32),
    'posts/still.png': IMAGES.png(33),
    'avatars/mira.jpg': IMAGES.jpeg(34),
  };

  it('round-trips videos with their posters, sizes and alt texts', async () => {
    const original = await importExport(VIDEO_EXPORT);
    expect(original.posts[0].media.map((item) => item.kind)).toEqual(['video', 'image']);
    expect(await saveImport(original)).toBe(true);
    releaseImport(original);
    const loaded = (await load()) as ImportResult;
    const [video, still] = loaded.posts[0].media;
    expect(video).toMatchObject({
      kind: 'video',
      path: 'posts/clip.mp4',
      mime: 'video/mp4',
      posterPath: 'posts/clip_poster.jpg',
      bytes: 300,
      alt: 'A clip',
    });
    expect(Array.from(await blobBytes(blobForUrl(video.url) as Blob))).toEqual(Array.from(VIDEOS.mp4(31, 300)));
    expect(Array.from(await blobBytes(blobForUrl(video.posterUrl as string) as Blob))).toEqual(Array.from(IMAGES.jpeg(32)));
    expect(still).toMatchObject({ kind: 'image', mime: 'image/png' });
    expect(loaded.posts[0].mediaType).toBe('carousel');
  });

  it('keeps a merged result with its sources', async () => {
    const merged = mergeImportResults([await importExport(), await importExport(VIDEO_EXPORT)]);
    // The merged result owns the URLs of both imports now.
    live.splice(0, live.length, merged);
    expect(await saveImport(merged)).toBe(true);
    const loaded = (await load()) as ImportResult;
    expect(loaded.sourceKind).toBe('multiple');
    expect(loaded.sourceName).toBe('2 archives');
    expect(loaded.sources).toEqual(merged.sources);
    expect(loaded.posts.map((post) => post.id)).toEqual(merged.posts.map((post) => post.id));
  });

  it('restores an import saved by the previous version, which kept "images"', async () => {
    expect(await saveImport(await importExport())).toBe(true);
    await rewrite((meta) => {
      meta.schema = 1;
      for (const post of meta.posts as Array<Record<string, unknown>>) {
        post.images = (post.media as Array<Record<string, unknown>>).map(({ blob, path, mime, converted }) =>
          converted ? { blob, path, mime, converted } : { blob, path, mime },
        );
        delete post.media;
      }
    });
    const loaded = (await load()) as ImportResult;
    expect(loaded.posts.map((post) => post.id)).toEqual(['AAA111', 'BBB222']);
    expect(loaded.posts[0].media).toEqual([
      { kind: 'image', url: expect.any(String), path: 'posts/post1.jpg', mime: 'image/jpeg' },
      { kind: 'image', url: expect.any(String), path: 'posts/post4.heic', mime: 'image/jpeg', converted: true },
    ]);
    expect(Array.from(await blobBytes(blobForUrl(loaded.posts[0].media[0].url) as Blob))).toEqual(Array.from(IMAGES.jpeg(11)));
  });

  it('does not save a library result: the server keeps the library', async () => {
    const library: ImportResult = {
      sourceName: 'Library',
      sourceKind: 'library',
      posts: [
        {
          id: 'L1',
          media: [{ kind: 'image', url: '/api/library/files/posts/L1_1.jpg?r=1', path: 'posts/L1_1.jpg', mime: 'image/jpeg' }],
          user: { username: 'a' },
          caption: '',
          commentCount: 0,
          likeCount: null,
          mediaType: 'image',
          comments: [],
        },
      ],
      warnings: [],
      totalEntries: 1,
    };
    expect(await saveImport(library)).toBe(false);
    expect(await loadSavedImport()).toBeNull();
  });
});

/** An object store instance, to reach the prototype that put() lives on. */
function objectStoreProbe(): Promise<IDBObjectStore> {
  return new Promise((resolve, reject) => {
    const open = indexedDB.open(IMPORT_DB_NAME);
    open.onerror = () => reject(open.error);
    open.onsuccess = () => {
      const db = open.result;
      const store = db.transaction(db.objectStoreNames[0]).objectStore(db.objectStoreNames[0]);
      db.close();
      resolve(store);
    };
  });
}
