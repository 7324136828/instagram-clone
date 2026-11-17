import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import backendLibrary from '../../../backend/app/library.py?raw';
import {
  ImportError,
  LIBRARY_COMMIT_LIMITS,
  importFromArchive,
  importMany,
  libraryFileUrl,
  prepareLibraryUpload,
  prepareLibraryUploads,
  releaseImport,
  resultFromLibrary,
  setHeicConverter,
  setHeicNativeProbe,
  type ImportResult,
  type LibraryCommitLimits,
  type LibraryCommitPost,
  type LibraryPostEntry,
  type LibraryResponse,
  type LibraryUploadPlan,
} from './index';
import { IMAGES, VIDEOS, blobBytes, json, makeFile, makeZip, type FileMap } from './testkit';

const results: ImportResult[] = [];
async function track(promise: Promise<ImportResult>): Promise<ImportResult> {
  const result = await promise;
  results.push(result);
  return result;
}

beforeEach(() => {
  setHeicNativeProbe(async () => false);
});

afterEach(() => {
  for (const result of results.splice(0)) releaseImport(result);
  setHeicConverter(null);
  setHeicNativeProbe(null);
  vi.unstubAllGlobals();
});

const fileUrl = (path: string) => libraryFileUrl(path, 7);

function library(posts: unknown[], extra: Partial<LibraryResponse> = {}): LibraryResponse {
  return {
    directory: 'C:\\Users\\me\\AppData\\Local\\Temp\\instagram_cache',
    revision: 7,
    updated_at: '2026-11-10T09:30:00Z',
    imports: [],
    posts: posts as LibraryPostEntry[],
    ...extra,
  };
}

describe('libraryFileUrl', () => {
  it('encodes every path segment and adds the revision', () => {
    expect(libraryFileUrl('posts/C5xYz_1.mp4', 3)).toBe('/api/library/files/posts/C5xYz_1.mp4?r=3');
    expect(libraryFileUrl('avatars/a b#?%.jpg', 12)).toBe('/api/library/files/avatars/a%20b%23%3F%25.jpg?r=12');
  });
});

describe('resultFromLibrary', () => {
  const entry: LibraryPostEntry = {
    id: 'C5xYz',
    url: 'https://www.instagram.com/reel/C5xYz/',
    media_type: 'video',
    media: [
      { type: 'video', path: 'posts/C5xYz_1.mp4', poster: 'posts/C5xYz_1_poster.jpg' },
      { type: 'image', path: 'posts/C5xYz_2.webp', alt: 'second slide' },
    ],
    image: 'posts/C5xYz_1_poster.jpg',
    images: ['posts/C5xYz_1_poster.jpg', 'posts/C5xYz_2.webp'],
    user: { username: 'someone', full_name: 'Some One', avatar: 'avatars/someone.png' },
    caption: 'A reel',
    comment_count: 12,
    like_count: 340,
    taken_at: '2026-10-01T12:00:00Z',
    comments: [{ username: 'other', text: 'Nice', like_count: 2 }],
  };

  it('turns library posts into a result with server URLs, without downloading anything', () => {
    const fetchSpy = vi.fn();
    vi.stubGlobal('fetch', fetchSpy);
    const result = resultFromLibrary(
      library([entry], {
        imports: [
          { id: 'i1', source_name: 'part1.zip', source_kind: 'archive', imported_at: '2026-11-09T00:00:00Z', posts: 3, new: 3, updated: 0 },
          { id: 'i2', source_name: '2 archives', source_kind: 'multiple', imported_at: '2026-11-10T00:00:00Z', posts: 5, new: 2, updated: 3 },
        ],
      }),
      fileUrl,
    );
    expect(fetchSpy).not.toHaveBeenCalled();
    expect(result).toMatchObject({
      sourceName: 'Library',
      sourceKind: 'library',
      totalEntries: 1,
      warnings: [],
      exportedAt: '2026-11-10T09:30:00.000Z',
      sources: [
        { name: 'part1.zip', kind: 'archive', posts: 3, warnings: 0 },
        { name: '2 archives', kind: 'multiple', posts: 5, warnings: 0 },
      ],
    });
    const [post] = result.posts;
    expect(post).toEqual({
      id: 'C5xYz',
      permalink: 'https://www.instagram.com/reel/C5xYz/',
      mediaType: 'video',
      media: [
        {
          kind: 'video',
          url: '/api/library/files/posts/C5xYz_1.mp4?r=7',
          path: 'posts/C5xYz_1.mp4',
          mime: 'video/mp4',
          posterUrl: '/api/library/files/posts/C5xYz_1_poster.jpg?r=7',
          posterPath: 'posts/C5xYz_1_poster.jpg',
        },
        { kind: 'image', url: '/api/library/files/posts/C5xYz_2.webp?r=7', path: 'posts/C5xYz_2.webp', mime: 'image/webp', alt: 'second slide' },
      ],
      user: {
        username: 'someone',
        fullName: 'Some One',
        profileUrl: 'https://www.instagram.com/someone/',
        avatarUrl: '/api/library/files/avatars/someone.png?r=7',
      },
      caption: 'A reel',
      commentCount: 12,
      likeCount: 340,
      takenAt: '2026-10-01T12:00:00.000Z',
      comments: [{ id: 'C5xYz-c1', username: 'other', text: 'Nice', likes: 2 }],
    });
  });

  it('gives an empty result for an empty library', () => {
    const result = resultFromLibrary(library([], { revision: 0, updated_at: null }), fileUrl);
    expect(result).toEqual({ sourceName: 'Library', sourceKind: 'library', posts: [], warnings: [], totalEntries: 0, sources: [] });
  });

  it('sanitises links and paths like an export, and skips files it cannot show', () => {
    const result = resultFromLibrary(
      library([
        { ...entry, id: 'evil', url: 'javascript:alert(1)', user: { username: 'x', profile_url: 'javascript:alert(1)', avatar: 'avatars/x.svg' } },
        { ...entry, id: 'odd', media: [{ type: 'video', path: 'posts/odd.mkv' }, { type: 'image', path: '../../etc/passwd.jpg' }] },
        { ...entry, id: 'poster', media: [{ type: 'video', path: 'posts/v.webm', poster: 'posts/v.mp4' }] },
      ]),
      fileUrl,
    );
    expect(result.posts.map((post) => post.id)).toEqual(['evil', 'poster']);
    const [evil, poster] = result.posts;
    expect(evil.permalink).toBeUndefined();
    expect(evil.user.profileUrl).toBe('https://www.instagram.com/x/'); // rebuilt from the username
    expect(evil.user.avatarUrl).toBeUndefined();
    expect(poster.media[0]).toMatchObject({ kind: 'video', mime: 'video/webm' });
    expect(poster.media[0].posterUrl).toBeUndefined();
    expect(result.warnings.map((warning) => [warning.code, warning.postIndex])).toEqual([
      ['unsafe-url', 0],
      ['unsafe-url', 0],
      ['invalid-entry', 1],
      ['unsupported-media', 1],
    ]);
  });
});

describe('prepareLibraryUpload', () => {
  const EXPORT: FileMap = {
    'post.json': json({
      exported_at: '2026-11-10T09:30:00Z',
      source: { type: 'profile', url: 'https://www.instagram.com/someone/' },
      posts: [
        {
          id: 'C5xYz',
          url: 'https://www.instagram.com/reel/C5xYz/',
          media_type: 'video',
          media: [{ type: 'video', path: 'posts/post5.mp4', poster: 'posts/post5_poster.jpg', alt: 'the clip' }],
          user: { username: 'someone', full_name: 'Some One', verified: true, avatar: 'avatars/someone.jpg' },
          caption: 'A reel',
          comment_count: 12,
          like_count: 340,
          taken_at: '2026-10-01T12:00:00Z',
          alt: 'post alt',
          location: 'Paris',
          comments: [{ username: 'other', text: 'Nice', like_count: 2, taken_at: '2026-10-02T12:00:00Z' }],
        },
        { id: 'P2', user: { username: 'someone', avatar: 'avatars/someone.jpg' }, images: ['posts/a.png', 'posts/b.webm', 'posts/post5_poster.jpg'] },
        { id: 'P3', user: 'other', image: 'posts/a.png', like_count: null },
      ],
    }),
    'posts/post5.mp4': VIDEOS.mp4(1, 200),
    'posts/post5_poster.jpg': IMAGES.jpeg(2),
    'posts/a.png': IMAGES.png(3),
    'posts/b.webm': VIDEOS.webm(4),
    'avatars/someone.jpg': IMAGES.jpeg(5),
  };

  it('stages every distinct file once and builds the commit body from the posts', async () => {
    const result = await track(importFromArchive(makeFile('export.zip', makeZip(EXPORT))));
    const plan = await prepareLibraryUpload(result);
    expect(plan.files.map((file) => [file.name, file.mime, file.blob.size])).toEqual([
      ['m000001.mp4', 'video/mp4', 200],
      ['m000002.jpg', 'image/jpeg', 64],
      ['m000003.jpg', 'image/jpeg', 64],
      ['m000004.png', 'image/png', 64],
      ['m000005.webm', 'video/webm', 64],
    ]);
    expect(Array.from(await blobBytes(plan.files[0].blob))).toEqual(Array.from(VIDEOS.mp4(1, 200)));
    expect(plan.commit).toEqual({
      source_name: 'export.zip',
      source_kind: 'archive',
      exported_at: '2026-11-10T09:30:00.000Z',
      source_url: 'https://www.instagram.com/someone/',
      avatars: { someone: 'm000003.jpg' },
      posts: [
        {
          id: 'C5xYz',
          url: 'https://www.instagram.com/reel/C5xYz/',
          media_type: 'video',
          media: [{ type: 'video', path: 'm000001.mp4', poster: 'm000002.jpg', alt: 'the clip' }],
          user: { username: 'someone', full_name: 'Some One', verified: true, profile_url: 'https://www.instagram.com/someone/' },
          caption: 'A reel',
          comment_count: 12,
          like_count: 340,
          taken_at: '2026-10-01T12:00:00.000Z',
          alt: 'post alt',
          location: 'Paris',
          comments: [{ username: 'other', text: 'Nice', like_count: 2, taken_at: '2026-10-02T12:00:00.000Z' }],
        },
        {
          id: 'P2',
          media_type: 'carousel',
          // The poster file is also a slide of this post: one upload serves both.
          media: [
            { type: 'image', path: 'm000004.png' },
            { type: 'video', path: 'm000005.webm' },
            { type: 'image', path: 'm000002.jpg' },
          ],
          user: { username: 'someone', profile_url: 'https://www.instagram.com/someone/' },
          caption: '',
          comment_count: 0,
          like_count: null,
          comments: [],
        },
        {
          id: 'P3',
          media_type: 'image',
          media: [{ type: 'image', path: 'm000004.png' }],
          user: { username: 'other', profile_url: 'https://www.instagram.com/other/' },
          caption: '',
          comment_count: 0,
          like_count: null,
          comments: [],
        },
      ],
    });
  });

  it('comes back unchanged through the library (staged names become stable paths)', async () => {
    const result = await track(importFromArchive(makeFile('export.zip', makeZip(EXPORT))));
    const plan = await prepareLibraryUpload(result);
    // What the server does on commit: move every staged file to a stable name.
    const stable = new Map(plan.files.map((file, i) => [file.name, `posts/f${i}.${file.name.split('.')[1]}`]));
    const moved = (name: string | undefined) => (name === undefined ? undefined : stable.get(name));
    const posts: LibraryPostEntry[] = plan.commit.posts.map((post: LibraryCommitPost) => ({
      ...post,
      media: post.media.map((item) => ({ ...item, path: moved(item.path) as string, ...(item.poster ? { poster: moved(item.poster) } : {}) })),
      user: { ...post.user, ...(plan.commit.avatars?.[post.user.username] ? { avatar: moved(plan.commit.avatars[post.user.username]) } : {}) },
    }));
    const back = resultFromLibrary(library(posts), fileUrl);
    expect(back.warnings).toEqual([]);
    const strip = (value: ImportResult) =>
      value.posts.map((post) => ({
        ...post,
        media: post.media.map(({ kind, mime, alt, posterUrl }) => ({ kind, mime, alt, poster: posterUrl !== undefined })),
        user: { ...post.user, avatarUrl: post.user.avatarUrl !== undefined },
      }));
    expect(strip(back)).toEqual(strip(result));
  });

  it('converts a HEIC picture the browser showed natively, because the library stores JPEG', async () => {
    setHeicNativeProbe(async () => true);
    const result = await track(
      importFromArchive(makeFile('x.zip', makeZip({ 'post.json': json([{ user: 'a', image: 'p.heic' }]), 'p.heic': IMAGES.heic(1) }))),
    );
    expect(result.posts[0].media[0].mime).toBe('image/heic');
    setHeicConverter(async () => new Blob([IMAGES.jpeg(9)], { type: 'image/jpeg' }));
    const plan = await prepareLibraryUpload(result);
    expect(plan.files.map((file) => [file.name, file.mime])).toEqual([['m000001.jpg', 'image/jpeg']]);
    expect(Array.from(await blobBytes(plan.files[0].blob))).toEqual(Array.from(IMAGES.jpeg(9)));

    setHeicConverter(async () => new Blob(['not an image']));
    await expect(prepareLibraryUpload(result)).rejects.toMatchObject({ code: 'read-failed', message: expect.stringContaining('HEIC') });
  });

  it('refuses a released import and the library itself', async () => {
    const result = await importFromArchive(makeFile('export.zip', makeZip(EXPORT)));
    releaseImport(result);
    const released = await prepareLibraryUpload(result).catch((err: unknown) => err);
    expect(released).toBeInstanceOf(ImportError);
    expect((released as ImportError).message).toContain('no longer available');
    await expect(prepareLibraryUpload(resultFromLibrary(library([]), fileUrl))).rejects.toMatchObject({ code: 'read-failed' });
  });

  it('names merged imports as such', async () => {
    const result = await track(importFromArchive(makeFile('export.zip', makeZip(EXPORT))));
    const plan = await prepareLibraryUpload({ ...result, sourceKind: 'multiple', sourceName: '2 archives' });
    expect(plan.commit).toMatchObject({ source_name: '2 archives', source_kind: 'multiple' });
  });
});

describe('prepareLibraryUploads', () => {
  const NO_LIMITS: LibraryCommitLimits = {
    maxPostsPerCommit: Number.MAX_SAFE_INTEGER,
    maxCommitBytes: Number.MAX_SAFE_INTEGER,
    maxFilesPerUpload: Number.MAX_SAFE_INTEGER,
    maxUploadBytes: Number.MAX_SAFE_INTEGER,
  };
  const bytesOf = (value: unknown) => new TextEncoder().encode(JSON.stringify(value)).length;

  /** What the server checks on every upload session and commit, plus the split's own promises. */
  function expectValidParts(plans: LibraryUploadPlan[], result: ImportResult, limits: LibraryCommitLimits): void {
    expect(plans.flatMap((plan) => plan.commit.posts.map((post) => post.id))).toEqual(result.posts.map((post) => post.id));
    const avatarParts = new Map<string, number>();
    plans.forEach((plan, index) => {
      expect(plan.commit.posts.length).toBeGreaterThan(0);
      expect(plan.commit.posts.length).toBeLessThanOrEqual(limits.maxPostsPerCommit);
      expect(plan.files.length).toBeLessThanOrEqual(limits.maxFilesPerUpload);
      expect(plan.files.reduce((sum, file) => sum + file.blob.size, 0)).toBeLessThanOrEqual(limits.maxUploadBytes);
      expect(bytesOf(plan.commit)).toBeLessThanOrEqual(limits.maxCommitBytes);
      // Staged names restart in every session; every reference is a file of the same part, and no file is unused.
      const names = plan.files.map((file) => file.name);
      expect(names.map((name) => name.slice(0, 7))).toEqual(names.map((_, i) => `m${String(i + 1).padStart(6, '0')}`));
      const used = new Set<string>();
      for (const post of plan.commit.posts) {
        for (const item of post.media) {
          used.add(item.path);
          if (item.poster) used.add(item.poster);
        }
      }
      for (const [username, name] of Object.entries(plan.commit.avatars ?? {})) {
        used.add(name);
        expect(avatarParts.has(username)).toBe(false);
        avatarParts.set(username, index);
      }
      expect([...used].sort()).toEqual([...names].sort());
    });
    // A profile picture goes with the first part that has a post of its user.
    for (const post of result.posts) {
      if (!post.user.avatarUrl) continue;
      const first = plans.findIndex((plan) => plan.commit.posts.some((entry) => entry.user.username === post.user.username));
      expect(avatarParts.get(post.user.username)).toBe(first);
    }
  }

  it('is one part, identical to prepareLibraryUpload, when the import fits', async () => {
    const result = await track(importFromArchive(makeFile('export.zip', makeZip(EXPORT_FOR_PARTS))));
    const plans = await prepareLibraryUploads(result);
    expect(plans).toHaveLength(1);
    expect(plans[0]).toEqual(await prepareLibraryUpload(result));
    expect(plans[0].commit.source_name).toBe('export.zip');
  });

  it('splits by posts per commit; a shared file is uploaded with each part, an avatar once', async () => {
    const result = await track(importFromArchive(makeFile('export.zip', makeZip(EXPORT_FOR_PARTS))));
    const limits = { ...NO_LIMITS, maxPostsPerCommit: 2 };
    const plans = await prepareLibraryUploads(result, limits);
    expect(plans.map((plan) => plan.commit.posts.map((post) => post.id))).toEqual([['A1', 'A2'], ['B1', 'B2'], ['C1']]);
    expect(plans.map((plan) => plan.commit.source_name)).toEqual([
      'export.zip (part 1 of 3)',
      'export.zip (part 2 of 3)',
      'export.zip (part 3 of 3)',
    ]);
    expect(plans.map((plan) => plan.commit)).toEqual(plans.map(() => expect.objectContaining({
      source_kind: 'archive',
      exported_at: '2026-11-10T09:30:00.000Z',
      source_url: 'https://www.instagram.com/someone/',
    })));
    expect(plans.map((plan) => plan.commit.avatars)).toEqual([{ someone: 'm000002.jpg' }, { other: 'm000002.jpg' }, undefined]);
    // shared.png is shown by every part's posts.
    expect(plans.map((plan) => plan.files.map((file) => file.name))).toEqual([
      ['m000001.png', 'm000002.jpg', 'm000003.mp4', 'm000004.jpg'],
      ['m000001.png', 'm000002.jpg'],
      ['m000001.png'],
    ]);
    expectValidParts(plans, result, limits);
  });

  it('splits by commit body bytes, files and file bytes', async () => {
    const result = await track(importFromArchive(makeFile('export.zip', makeZip(EXPORT_FOR_PARTS))));
    const single = (await prepareLibraryUploads(result, NO_LIMITS))[0];
    const caseLimits: Array<[string, LibraryCommitLimits]> = [
      ['commit bytes', { ...NO_LIMITS, maxCommitBytes: Math.ceil(bytesOf(single.commit) / 2) }],
      ['files', { ...NO_LIMITS, maxFilesPerUpload: 3 }],
      ['file bytes', { ...NO_LIMITS, maxUploadBytes: 400 }],
    ];
    for (const [what, limits] of caseLimits) {
      const plans = await prepareLibraryUploads(result, limits);
      expect(plans.length, what).toBeGreaterThan(1);
      expectValidParts(plans, result, limits);
    }
  });

  it('refuses a post that does not fit into an upload on its own, before anything is sent', async () => {
    const result = await track(importFromArchive(makeFile('export.zip', makeZip(EXPORT_FOR_PARTS))));
    await expect(prepareLibraryUploads(result, { ...NO_LIMITS, maxUploadBytes: 150 })).rejects.toMatchObject({
      code: 'too-large',
      message: expect.stringContaining('Post A2 is too large'),
    });
    await expect(prepareLibraryUploads(result, { ...NO_LIMITS, maxCommitBytes: 600 })).rejects.toMatchObject({ code: 'too-large' });
  });

  it('keeps part names within what the library stores', async () => {
    const result = await track(importFromArchive(makeFile('export.zip', makeZip(EXPORT_FOR_PARTS))));
    const plans = await prepareLibraryUploads({ ...result, sourceName: 'x'.repeat(400) }, { ...NO_LIMITS, maxPostsPerCommit: 1 });
    const max = Number(/^MAX_SOURCE_NAME_CHARS = (\d+)$/m.exec(backendLibrary)?.[1]);
    expect(max).toBeGreaterThan(0);
    for (const plan of plans) expect(plan.commit.source_name.length).toBeLessThanOrEqual(max);
    expect(plans[4].commit.source_name).toMatch(/^x+ \(part 5 of 5\)$/);
  });

  it('saves more than one commit allows from several archives (6000 posts) in several parts', async () => {
    const part = (from: number, count: number): FileMap => ({
      'post.json': json(Array.from({ length: count }, (_, i) => ({ id: `p${from + i}`, user: 'someone', image: 'posts/a.png' }))),
      'posts/a.png': IMAGES.png(1),
    });
    const result = await track(importMany([
      { kind: 'archive', file: makeFile('part1.zip', makeZip(part(0, 3000))) },
      { kind: 'archive', file: makeFile('part2.zip', makeZip(part(3000, 3000))) },
    ]));
    expect(result.posts).toHaveLength(6000);
    // The single-upload variant fails before uploading anything instead of after every file.
    await expect(prepareLibraryUpload(result)).rejects.toMatchObject({
      code: 'too-large',
      message: expect.stringContaining('at most 5000 posts'),
    });
    const plans = await prepareLibraryUploads(result);
    expect(plans.map((plan) => plan.commit.posts.length)).toEqual([5000, 1000]);
    expect(plans.map((plan) => plan.commit.source_name)).toEqual(['2 archives (part 1 of 2)', '2 archives (part 2 of 2)']);
    expectValidParts(plans, result, LIBRARY_COMMIT_LIMITS);
  }, 60_000);
});

/** Five posts: shared.png in all of them, two users with profile pictures. */
const EXPORT_FOR_PARTS: FileMap = {
  'post.json': json({
    exported_at: '2026-11-10T09:30:00Z',
    source: { type: 'profile', url: 'https://www.instagram.com/someone/' },
    posts: [
      { id: 'A1', user: { username: 'someone', avatar: 'avatars/someone.jpg' }, caption: 'x'.repeat(300), image: 'posts/shared.png' },
      {
        id: 'A2',
        user: { username: 'someone', avatar: 'avatars/someone.jpg' },
        media: [{ type: 'video', path: 'posts/v.mp4', poster: 'posts/v.jpg' }, { type: 'image', path: 'posts/shared.png' }],
      },
      { id: 'B1', user: { username: 'other', avatar: 'avatars/other.jpg' }, caption: 'y'.repeat(300), image: 'posts/shared.png' },
      { id: 'B2', user: { username: 'someone', avatar: 'avatars/someone.jpg' }, image: 'posts/shared.png' },
      { id: 'C1', user: { username: 'other', avatar: 'avatars/other.jpg' }, image: 'posts/shared.png' },
    ],
  }),
  'posts/shared.png': IMAGES.png(1),
  'posts/v.mp4': VIDEOS.mp4(2, 200),
  'posts/v.jpg': IMAGES.jpeg(3),
  'avatars/someone.jpg': IMAGES.jpeg(4),
  'avatars/other.jpg': IMAGES.jpeg(5),
};

describe('LIBRARY_COMMIT_LIMITS', () => {
  /** The defaults of backend/app/library.py LibraryLimits, e.g. "max_commit_bytes: int = 32 * MiB". */
  function backendLimit(name: string): number {
    const block = /^class LibraryLimits\b[\s\S]*?(?=^\S)/m.exec(backendLibrary)?.[0] ?? '';
    const expression = new RegExp(`^\\s+${name}: int = ([0-9_ *A-Za-z]+)$`, 'm').exec(block)?.[1];
    if (!expression) throw new Error(`LibraryLimits.${name} not found in backend/app/library.py`);
    const units: Record<string, number> = { KiB: 1024, MiB: 1024 ** 2, GiB: 1024 ** 3 };
    return expression.split('*').reduce((product, factor) => {
      const token = factor.trim().replace(/_/g, '');
      const value = units[token] ?? Number(token);
      if (!Number.isFinite(value)) throw new Error(`Cannot read LibraryLimits.${name} = ${expression}`);
      return product * value;
    }, 1);
  }

  it('matches what the backend library accepts', () => {
    expect(LIBRARY_COMMIT_LIMITS).toEqual({
      maxPostsPerCommit: backendLimit('max_posts_per_commit'),
      maxCommitBytes: backendLimit('max_commit_bytes'),
      maxFilesPerUpload: backendLimit('max_files_per_session'),
      maxUploadBytes: backendLimit('max_session_bytes'),
    });
  });
});
