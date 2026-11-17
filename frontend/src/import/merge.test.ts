import { afterEach, describe, expect, it, vi } from 'vitest';
import { describeSources, mergeImportResults } from './merge';
import { blobForUrl, registerBlobUrl, revokeBlobUrl } from './registry';
import type { ImportResult, ImportedPost } from './types';

const created: string[] = [];
function url(): string {
  const value = registerBlobUrl(new Blob(['x'], { type: 'image/jpeg' }));
  created.push(value);
  return value;
}

afterEach(() => {
  for (const value of created.splice(0)) revokeBlobUrl(value);
  vi.restoreAllMocks();
});

function post(id: string, takenAt: string | undefined, media = url(), avatar?: string): ImportedPost {
  const value: ImportedPost = {
    id,
    media: [{ kind: 'image', url: media, path: `posts/${id}.jpg`, mime: 'image/jpeg' }],
    user: avatar ? { username: 'a', avatarUrl: avatar } : { username: 'a' },
    caption: id,
    commentCount: 0,
    likeCount: null,
    mediaType: 'image',
    comments: [],
  };
  if (takenAt) value.takenAt = takenAt;
  return value;
}

function result(sourceName: string, posts: ImportedPost[], extra: Partial<ImportResult> = {}): ImportResult {
  return { sourceName, sourceKind: 'archive', posts, warnings: [], totalEntries: posts.length, ...extra };
}

describe('mergeImportResults', () => {
  it('returns a single result as it is and refuses to merge nothing', () => {
    const only = result('a.zip', [post('A', undefined)]);
    expect(mergeImportResults([only])).toBe(only);
    expect(() => mergeImportResults([])).toThrow(expect.objectContaining({ code: 'no-posts' }));
  });

  it('sorts newest first, keeps undated posts after the dated ones in their order, and lists the sources', () => {
    const a = result('a.zip', [post('A1', '2026-01-01T00:00:00Z'), post('A2', undefined), post('A3', '2026-03-01T00:00:00Z')], {
      totalEntries: 4,
      warnings: [{ code: 'missing-image', message: 'Post 4: the image "x.jpg" is not in the export.', postIndex: 3 }],
    });
    const b = result('photos', [post('B1', undefined), post('B2', '2026-02-01T00:00:00Z')], { sourceKind: 'folder' });
    const merged = mergeImportResults([a, b]);
    expect(merged.posts.map((item) => item.id)).toEqual(['A3', 'B2', 'A1', 'A2', 'B1']);
    expect(merged.sourceKind).toBe('multiple');
    expect(merged.sourceName).toBe('1 archive and 1 folder');
    expect(merged.totalEntries).toBe(6);
    expect(merged.sources).toEqual([
      { name: 'a.zip', kind: 'archive', posts: 3, warnings: 1 },
      { name: 'photos', kind: 'folder', posts: 2, warnings: 0 },
    ]);
    // Each warning says which source it is about.
    expect(merged.warnings).toEqual([
      { code: 'missing-image', message: 'a.zip: Post 4: the image "x.jpg" is not in the export.', postIndex: 3 },
    ]);
  });

  it('lets the later result win on the same id, in the earlier one\'s place, and says so', () => {
    const newer = { ...post('X', undefined), caption: 'the newer X' };
    const merged = mergeImportResults([
      result('old.zip', [post('X', undefined), post('Y', undefined)]),
      result('new.zip', [post('Z', undefined), newer]),
    ]);
    expect(merged.posts.map((item) => item.id)).toEqual(['X', 'Y', 'Z']);
    expect(merged.posts[0]).toBe(newer);
    expect(merged.warnings).toEqual([
      { code: 'merged-duplicate', message: 'The post "X" from new.zip replaces the one with the same id from old.zip.' },
    ]);
    expect(merged.sources?.map((source) => source.posts)).toEqual([2, 2]);
  });

  it('gives back the URLs only the replaced post used, and keeps shared ones', () => {
    const avatar = url();
    const oldX = post('X', '2026-01-01T00:00:00Z', url(), avatar);
    const keptY = post('Y', '2026-01-02T00:00:00Z', url(), avatar);
    const newX = post('X', '2026-01-03T00:00:00Z', url());
    const merged = mergeImportResults([result('old.zip', [oldX, keptY]), result('new.zip', [newX])]);
    expect(merged.posts).toEqual([newX, keptY]);
    expect(blobForUrl(oldX.media[0].url)).toBeUndefined();
    expect(blobForUrl(avatar)).toBeDefined(); // still used by Y
    expect(blobForUrl(newX.media[0].url)).toBeDefined();
    expect(blobForUrl(keptY.media[0].url)).toBeDefined();
  });

  it('does not change its inputs', () => {
    const a = result('a.zip', [post('X', '2026-01-01T00:00:00Z')], {
      warnings: [{ code: 'truncated', message: 'Something' }],
    });
    const b = result('b.zip', [post('X', '2026-01-02T00:00:00Z'), post('W', undefined)]);
    const before = structuredClone([a, b]);
    mergeImportResults([a, b]);
    expect([a, b]).toEqual(before);
  });

  it('flattens merged inputs: their sources are listed and their warnings are not named twice', () => {
    const first = mergeImportResults([result('a.zip', [post('A', undefined)]), result('b.zip', [post('A', undefined)])]);
    const added = mergeImportResults([first, result('c.zip', [post('C', undefined)])]);
    expect(added.sourceName).toBe('3 archives');
    expect(added.sources?.map((source) => source.name)).toEqual(['a.zip', 'b.zip', 'c.zip']);
    expect(added.warnings.map((warning) => warning.message)).toEqual([
      'The post "A" from b.zip replaces the one with the same id from a.zip.',
    ]);
  });

  it('keeps the newest export time and a source link only when all sources agree', () => {
    const link = 'https://www.instagram.com/natgeo/';
    const a = result('a.zip', [post('A', undefined)], { exportedAt: '2026-01-01T00:00:00.000Z', sourceUrl: link });
    const b = result('b.zip', [post('B', undefined)], { exportedAt: '2026-03-01T00:00:00.000Z', sourceUrl: link });
    const c = result('c.zip', [post('C', undefined)]);
    expect(mergeImportResults([a, b])).toMatchObject({ exportedAt: '2026-03-01T00:00:00.000Z', sourceUrl: link });
    const mixed = mergeImportResults([a, c]);
    expect(mixed.exportedAt).toBe('2026-01-01T00:00:00.000Z');
    expect(mixed.sourceUrl).toBeUndefined();
  });
});

describe('describeSources', () => {
  it.each([
    [[], 'Nothing'],
    [['archive'], '1 archive'],
    [['archive', 'archive', 'archive'], '3 archives'],
    [['folder', 'archive', 'folder'], '1 archive and 2 folders'],
    [['archive', 'folder', 'multiple'], '1 archive, 1 folder and 1 import'],
  ] as const)('%j -> %s', (kinds, text) => {
    expect(describeSources(kinds.map((kind) => ({ name: 'x', kind, posts: 1, warnings: 0 })))).toBe(text);
  });
});
