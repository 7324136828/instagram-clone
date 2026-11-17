import { unzipSync, strFromU8 } from 'fflate';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { importArchive } from './core';
import { exportPosts, loadExportFile, type ExportManifest, type ExportResult } from './export';
import { ImportError, releaseImport, setHeicNativeProbe, type ImportResult } from './index';
import { blobForUrl } from './registry';
import { IMAGES, VIDEOS, blobBytes, json, makeFile, makeZip, type FileMap } from './testkit';

const FILES = {
  cover: IMAGES.jpeg(11),
  clip: VIDEOS.mp4(12),
  clipPoster: IMAGES.png(13),
  second: IMAGES.png(14),
  reel: VIDEOS.webm(15),
  reelPoster: IMAGES.webp(16),
  avatar: IMAGES.jpeg(17),
};

const SOURCE: FileMap = {
  'post.json': json({
    format: 'instagram-export',
    version: 1,
    posts: [
      {
        id: 'C9aKq2LtXy1',
        url: 'https://www.instagram.com/p/C9aKq2LtXy1/',
        media: [
          { type: 'image', path: 'posts/post1_1.jpg' },
          { type: 'video', path: 'posts/post1_2.mp4', poster: 'posts/post1_2_poster.png', alt: 'The climb' },
        ],
        user: { username: 'lumen.atlas', full_name: 'Lumen Atlas', verified: true, avatar: 'avatars/lumen.atlas.jpg' },
        caption: 'Two mornings on the ridge 🌄\n\n#sunrise',
        comment_count: 12,
        like_count: 340,
        taken_at: '2026-11-01T15:04:05Z',
        media_type: 'carousel',
        alt: 'Cover alt',
        location: 'Chamonix, France',
        comments: [{ username: 'someone', text: 'Wow', like_count: 3, taken_at: '2026-11-01T16:00:00Z' }],
      },
      {
        id: 'C8ZmP4RvNd7',
        media: [{ type: 'image', path: 'posts/second.png' }],
        user: { username: 'lumen.atlas', avatar: 'avatars/lumen.atlas.jpg' },
        caption: '',
        comment_count: 0,
        like_count: null,
        taken_at: '2026-10-28T08:12:40Z',
      },
      {
        id: 'C7TtW0YhBs3',
        url: 'https://www.instagram.com/reel/C7TtW0YhBs3/',
        media: [{ type: 'video', path: 'posts/reel.webm', poster: 'posts/reel.webp' }],
        user: 'mira.films',
        caption: 'Behind the scenes, day 3.',
        comment_count: 4,
        like_count: 2100,
        taken_at: '2026-10-20T19:45:00Z',
        media_type: 'video',
      },
    ],
  }),
  'posts/post1_1.jpg': FILES.cover,
  'posts/post1_2.mp4': FILES.clip,
  'posts/post1_2_poster.png': FILES.clipPoster,
  'posts/second.png': FILES.second,
  'posts/reel.webm': FILES.reel,
  'posts/reel.webp': FILES.reelPoster,
  'avatars/lumen.atlas.jpg': FILES.avatar,
};

const NOW = new Date('2026-11-11T10:00:00Z');
const results: ImportResult[] = [];

async function imported(files: FileMap = SOURCE, name = 'export.zip'): Promise<ImportResult> {
  const result = await importArchive(makeFile(name, makeZip(files), 'application/zip'));
  results.push(result);
  return result;
}

async function unpack(result: ExportResult, index = 0): Promise<{ files: Record<string, Uint8Array>; manifest: ExportManifest }> {
  const files = unzipSync(await blobBytes(result.archives[index].blob));
  return { files, manifest: JSON.parse(strFromU8(files['post.json'])) as ExportManifest };
}

/** Loads like the app does, but fails for the URLs in `broken`. */
function failingFor(broken: Set<string>) {
  return async (url: string): Promise<Blob> => {
    if (broken.has(url)) throw new Error('gone');
    return loadExportFile(url);
  };
}

beforeEach(() => {
  setHeicNativeProbe(async () => false);
});

afterEach(() => {
  for (const result of results.splice(0)) releaseImport(result);
  setHeicNativeProbe(null);
  vi.restoreAllMocks();
});

describe('exporting posts', () => {
  it('writes post.json, posts/ and avatars/ in the instagram.md layout, captions included', async () => {
    const source = await imported();
    const progress: Array<[number, number]> = [];
    const result = await exportPosts(source.posts, { now: NOW, onProgress: (p) => progress.push([p.done, p.total]) });

    expect(result.problems).toEqual([]);
    expect(result.posts).toBe(3);
    expect(result.archives.map((archive) => [archive.name, archive.posts])).toEqual([['instagram-export.zip', 3]]);
    expect(result.archives[0].blob.type).toBe('application/zip');
    expect(progress).toEqual([[0, 3], [1, 3], [2, 3], [3, 3]]);

    const { files, manifest } = await unpack(result);
    expect(Object.keys(files).sort()).toEqual([
      'avatars/lumen.atlas.jpg',
      'post.json',
      'posts/post1_1.jpg',
      'posts/post1_2.mp4',
      'posts/post1_2_poster.png',
      'posts/post2.png',
      'posts/post3.webm',
      'posts/post3_poster.webp',
    ]);
    // The original bytes, never re-encoded.
    expect(files['posts/post1_1.jpg']).toEqual(FILES.cover);
    expect(files['posts/post1_2.mp4']).toEqual(FILES.clip);
    expect(files['posts/post3.webm']).toEqual(FILES.reel);
    expect(files['avatars/lumen.atlas.jpg']).toEqual(FILES.avatar);

    expect(manifest).toMatchObject({
      format: 'instagram-export',
      version: 1,
      exported_at: '2026-11-11T10:00:00Z',
      source: { type: 'posts' },
    });
    const [first, second, third] = manifest.posts;
    expect(Object.keys(first)).toEqual([
      'id', 'url', 'media', 'user', 'caption', 'comment_count', 'like_count', 'taken_at', 'media_type', 'alt', 'location', 'comments',
    ]);
    expect(first).toMatchObject({
      id: 'C9aKq2LtXy1',
      url: 'https://www.instagram.com/p/C9aKq2LtXy1/',
      media: [
        { type: 'image', path: 'posts/post1_1.jpg' },
        { type: 'video', path: 'posts/post1_2.mp4', poster: 'posts/post1_2_poster.png', alt: 'The climb' },
      ],
      user: { username: 'lumen.atlas', full_name: 'Lumen Atlas', verified: true, avatar: 'avatars/lumen.atlas.jpg' },
      caption: 'Two mornings on the ridge 🌄\n\n#sunrise',
      comment_count: 12,
      like_count: 340,
      taken_at: '2026-11-01T15:04:05Z',
      media_type: 'carousel',
      alt: 'Cover alt',
      location: 'Chamonix, France',
      comments: [{ username: 'someone', text: 'Wow', like_count: 3, taken_at: '2026-11-01T16:00:00Z' }],
    });
    expect(second).toMatchObject({
      id: 'C8ZmP4RvNd7',
      media: [{ type: 'image', path: 'posts/post2.png' }],
      user: { username: 'lumen.atlas', avatar: 'avatars/lumen.atlas.jpg' },
      caption: '',
      like_count: null,
      media_type: 'image',
    });
    expect(second).not.toHaveProperty('comments');
    expect(third).toMatchObject({
      id: 'C7TtW0YhBs3',
      media: [{ type: 'video', path: 'posts/post3.webm', poster: 'posts/post3_poster.webp' }],
      user: { username: 'mira.films' },
      caption: 'Behind the scenes, day 3.',
      media_type: 'video',
    });
    expect(third.user).not.toHaveProperty('avatar');
  });

  it('imports back into the clone without a warning', async () => {
    const source = await imported();
    const exported = await exportPosts(source.posts, { now: NOW });
    // The downloaded archive itself, read by the clone's own ZIP reader.
    const again = await importArchive(new File([exported.archives[0].blob], 'instagram-export.zip', { type: 'application/zip' }));
    results.push(again);
    expect(again.warnings).toEqual([]);
    const summary = (result: ImportResult) => result.posts.map((post) => ({
      id: post.id,
      caption: post.caption,
      mediaType: post.mediaType,
      media: post.media.map((item) => [item.kind, item.mime, !!item.posterUrl]),
      likeCount: post.likeCount,
      takenAt: post.takenAt,
      avatar: !!post.user.avatarUrl,
    }));
    expect(summary(again)).toEqual(summary(source));
  });

  it('writes an edited description', async () => {
    const source = await imported();
    const edited = { ...source.posts[1], caption: 'A new description\nwith two lines ✨' };
    const result = await exportPosts([edited], { now: NOW, name: 'instagram-export-C8ZmP4RvNd7' });
    expect(result.archives[0].name).toBe('instagram-export-C8ZmP4RvNd7.zip');
    const { files, manifest } = await unpack(result);
    expect(Object.keys(files).sort()).toEqual(['avatars/lumen.atlas.jpg', 'post.json', 'posts/post1.png']);
    expect(manifest.posts).toHaveLength(1);
    expect(manifest.posts[0].caption).toBe('A new description\nwith two lines ✨');
  });

  it('leaves out files it cannot read, falls back to a poster, and numbers the posts it writes', async () => {
    const source = await imported();
    const [first, second, third] = source.posts;
    const broken = new Set([
      first.media[0].url, // the cover picture of the carousel
      second.media[0].url, // the only picture of post 2: nothing is left of it
      third.media[0].url, // the reel: its poster stands in for it
    ]);
    const result = await exportPosts(source.posts, { now: NOW, loadFile: failingFor(broken) });
    expect(result.posts).toBe(2);
    expect(result.problems).toEqual([
      'Post 1 (C9aKq2LtXy1): "posts/post1_1.jpg" could not be read: gone.',
      'Post 2 (C8ZmP4RvNd7): "posts/second.png" could not be read: gone.',
      'Post 2 (C8ZmP4RvNd7) was left out: none of its pictures or videos could be saved.',
      'Post 3 (C7TtW0YhBs3): "posts/reel.webm" could not be read: gone.',
      'Post 3 (C7TtW0YhBs3): only the poster of "posts/reel.webm" was saved.',
    ]);
    const { files, manifest } = await unpack(result);
    expect(Object.keys(files).sort()).toEqual([
      'avatars/lumen.atlas.jpg', 'post.json', 'posts/post1.mp4', 'posts/post1_poster.png', 'posts/post2.webp',
    ]);
    expect(manifest.posts.map((post) => [post.id, post.media_type, post.media])).toEqual([
      ['C9aKq2LtXy1', 'video', [{ type: 'video', path: 'posts/post1.mp4', poster: 'posts/post1_poster.png', alt: 'The climb' }]],
      ['C7TtW0YhBs3', 'video', [{ type: 'image', path: 'posts/post2.webp' }]],
    ]);
  });

  it('returns no archive when nothing could be read', async () => {
    const source = await imported();
    const result = await exportPosts(source.posts.slice(1, 2), { loadFile: async () => { throw new Error('gone'); } });
    expect(result.archives).toEqual([]);
    expect(result.posts).toBe(0);
    expect(result.problems.at(-1)).toBe('Post 1 (C8ZmP4RvNd7) was left out: none of its pictures or videos could be saved.');
  });

  it('splits a large export into complete parts numbered across parts', async () => {
    const source = await imported();
    // Post 1 has 256 bytes of files (with the avatar), posts 2 and 3 have 128 each.
    const result = await exportPosts(source.posts, { now: NOW, maxPartBytes: 200 });
    expect(result.archives.map((archive) => [archive.name, archive.posts])).toEqual([
      ['instagram-export-part1.zip', 1],
      ['instagram-export-part2.zip', 1],
      ['instagram-export-part3.zip', 1],
    ]);
    const part2 = await unpack(result, 1);
    expect(Object.keys(part2.files).sort()).toEqual(['avatars/lumen.atlas.jpg', 'post.json', 'posts/post2.png']);
    expect(part2.manifest.posts[0]).toMatchObject({ id: 'C8ZmP4RvNd7', user: { avatar: 'avatars/lumen.atlas.jpg' } });
    const part3 = await unpack(result, 2);
    expect(part3.manifest.posts[0].media[0].path).toBe('posts/post3.webm');

    const byPosts = await exportPosts(source.posts, { now: NOW, maxPartPosts: 2 });
    expect(byPosts.archives.map((archive) => archive.posts)).toEqual([2, 1]);
  });

  it('keeps ids unique and avatar file names safe', async () => {
    const source = await imported();
    const twin = { ...source.posts[1], user: { ...source.posts[1].user, username: '../CON' } };
    const result = await exportPosts([source.posts[1], twin], { now: NOW });
    const { files, manifest } = await unpack(result);
    expect(manifest.posts.map((post) => post.id)).toEqual(['C8ZmP4RvNd7', 'C8ZmP4RvNd7-2']);
    expect(manifest.posts[1].user.avatar).toBe('avatars/_CON.jpg');
    expect(files['avatars/_CON.jpg']).toEqual(FILES.avatar);
  });

  it('stops when cancelled', async () => {
    const source = await imported();
    const controller = new AbortController();
    const running = exportPosts(source.posts, {
      signal: controller.signal,
      onProgress: (progress) => { if (progress.done === 1) controller.abort(); },
    });
    await expect(running).rejects.toMatchObject({ code: 'aborted' });
    await expect(running).rejects.toBeInstanceOf(ImportError);
  });
});

describe('loading the files of a post', () => {
  it('uses the blob behind an object URL of the import', async () => {
    const source = await imported();
    const url = source.posts[0].media[0].url;
    expect(await loadExportFile(url)).toBe(blobForUrl(url));
  });

  it('fetches library files from this app only', async () => {
    const fetchMock = vi.spyOn(globalThis, 'fetch').mockImplementation(async (input) => {
      if (String(input) === '/api/library/files/posts/a_1.jpg?r=2') return new Response(new Blob([FILES.cover]));
      return new Response(null, { status: 404 });
    });
    expect(Array.from(await blobBytes(await loadExportFile('/api/library/files/posts/a_1.jpg?r=2')))).toEqual(Array.from(FILES.cover));
    await expect(loadExportFile('/api/library/files/posts/b_1.jpg?r=2')).rejects.toThrow('the server answered 404');
    await expect(loadExportFile('https://scontent.cdninstagram.com/x.jpg')).rejects.toThrow('no longer available');
    await expect(loadExportFile('blob:revoked')).rejects.toThrow('no longer available');
    expect(fetchMock).toHaveBeenCalledTimes(2);
  });
});
