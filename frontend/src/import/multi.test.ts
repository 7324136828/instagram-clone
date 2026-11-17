import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  ImportError,
  MAX_SOURCES,
  importFromDataTransfer,
  importFromFiles,
  importMany,
  releaseImport,
  setHeicNativeProbe,
  type ImportProgress,
  type ImportResult,
} from './index';
import { importMany as importManyWithLimits } from './multi';
import { blobForUrl } from './registry';
import {
  IMAGES,
  VIDEOS,
  dirEntry,
  fakeDataTransfer,
  fileEntry,
  folderFiles,
  json,
  makeFile,
  makeZip,
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
  setHeicNativeProbe(null);
  vi.restoreAllMocks();
});

/** An export with one post per entry: [id, taken_at]. */
function exportOf(posts: Array<[string, string | undefined]>, fill = 1): FileMap {
  const files: FileMap = {
    'post.json': json(
      posts.map(([id, takenAt]) => ({
        id,
        user: { username: 'alice', avatar: 'avatars/alice.jpg' },
        image: `posts/${id}.jpg`,
        caption: `${id} from export ${fill}`,
        ...(takenAt ? { taken_at: takenAt } : {}),
      })),
    ),
    'avatars/alice.jpg': IMAGES.jpeg(fill),
  };
  for (const [id] of posts) files[`posts/${id}.jpg`] = IMAGES.jpeg(fill);
  return files;
}

const zip = (name: string, files: FileMap) => makeFile(name, makeZip(files), 'application/zip');
const PART1 = exportOf([['A', '2026-01-01T00:00:00Z'], ['B', undefined]], 1);
const PART2 = exportOf([['C', '2026-03-01T00:00:00Z'], ['A', '2026-01-01T00:00:00Z']], 2);

describe('importFromFiles with several archives', () => {
  it('imports each archive and merges them: later wins, newest first, sources listed', async () => {
    const result = await track(importFromFiles([zip('part1.zip', PART1), zip('part2.zip', PART2)]));
    expect(result.sourceKind).toBe('multiple');
    expect(result.sourceName).toBe('2 archives');
    expect(result.posts.map((post) => [post.id, post.caption])).toEqual([
      ['C', 'C from export 2'],
      ['A', 'A from export 2'],
      ['B', 'B from export 1'],
    ]);
    expect(result.totalEntries).toBe(4);
    expect(result.sources).toEqual([
      { name: 'part1.zip', kind: 'archive', posts: 2, warnings: 0 },
      { name: 'part2.zip', kind: 'archive', posts: 2, warnings: 0 },
    ]);
    expect(result.warnings.map((warning) => warning.code)).toEqual(['merged-duplicate']);
    // Every URL of the merged result is alive; releasing it gives all of them back.
    const urls = result.posts.flatMap((post) => [post.media[0].url, post.user.avatarUrl as string]);
    for (const url of urls) expect(blobForUrl(url)).toBeDefined();
    releaseImport(result);
    for (const url of urls) expect(blobForUrl(url)).toBeUndefined();
  });

  it('still imports a single archive exactly as before', async () => {
    const result = await track(importFromFiles([zip('part1.zip', PART1)]));
    expect(result.sourceKind).toBe('archive');
    expect(result.sourceName).toBe('part1.zip');
    expect(result.sources).toBeUndefined();
    expect(result.posts.map((post) => post.id)).toEqual(['A', 'B']); // the order of post.json
  });

  it('opens every archive inside a picked folder that has no post.json', async () => {
    const files = folderFiles('Downloads', { 'part1.zip': makeZip(PART1), 'part2.zip': makeZip(PART2), 'notes.txt': 'hi' });
    const result = await track(importFromFiles(files));
    expect(result.sourceName).toBe('2 archives');
    expect(result.posts).toHaveLength(3);
  });
});

describe('importMany', () => {
  it('mixes archives and folders, and says which source it is reading', async () => {
    const events: ImportProgress[] = [];
    const result = await track(
      importMany(
        [
          { kind: 'archive', file: zip('part1.zip', PART1) },
          { kind: 'folder', files: folderFiles('second', PART2) },
        ],
        { onProgress: (progress) => events.push(progress) },
      ),
    );
    expect(result.sourceName).toBe('1 archive and 1 folder');
    expect(result.sources?.map((source) => [source.name, source.kind])).toEqual([
      ['part1.zip', 'archive'],
      ['second', 'folder'],
    ]);
    const messages = events.map((event) => event.message);
    expect(messages).toContain('Reading archive 1 of 2: part1.zip');
    expect(messages).toContain('Reading folder 2 of 2: second');
    expect(messages.some((message) => message.startsWith('second (folder 2 of 2): Loaded file'))).toBe(true);
    // One 'done', at the very end.
    expect(events.filter((event) => event.phase === 'done')).toEqual([
      { phase: 'done', done: 2, total: 2, message: 'Imported 3 posts from 1 archive and 1 folder' },
    ]);
    expect(events[events.length - 1].phase).toBe('done');
  });

  it('turns a source that cannot be imported into a warning and keeps the others', async () => {
    const result = await track(
      importMany([
        { kind: 'archive', file: makeFile('broken.zip', 'PK\u0003\u0004 not really a zip') },
        { kind: 'archive', file: zip('part1.zip', PART1) },
        { kind: 'folder', files: folderFiles('pictures', { 'a.jpg': IMAGES.jpeg() }), name: 'My pictures' },
      ]),
    );
    expect(result.posts.map((post) => post.id)).toEqual(['A', 'B']);
    expect(result.sourceKind).toBe('multiple');
    expect(result.sourceName).toBe('2 archives and 1 folder');
    expect(result.warnings.map((warning) => [warning.code, warning.message.slice(0, 40)])).toEqual([
      ['invalid-entry', '"broken.zip" could not be imported: The '],
      ['invalid-entry', '"My pictures" could not be imported: No '],
    ]);
    expect(result.sources).toEqual([
      { name: 'broken.zip', kind: 'archive', posts: 0, warnings: 0, error: expect.stringContaining('damaged') },
      { name: 'part1.zip', kind: 'archive', posts: 2, warnings: 0 },
      { name: 'My pictures', kind: 'folder', posts: 0, warnings: 0, error: expect.stringContaining('No post.json') },
    ]);
  });

  it('fails only when every source fails, naming each one', async () => {
    const error = await failure(
      importMany([
        { kind: 'archive', file: makeFile('one.zip', 'not a zip') },
        { kind: 'folder', files: folderFiles('two', { 'a.jpg': IMAGES.jpeg() }) },
      ]),
    );
    expect(error.code).toBe('read-failed'); // the code of the first failure
    expect(error.message).toMatch(/^None of the 2 selected archives or folders could be imported\./);
    expect(error.message).toContain('one.zip: ');
    expect(error.message).toContain('two: No post.json');
    // A single source fails with its own error.
    expect((await failure(importMany([{ kind: 'archive', file: makeFile('one.zip', 'not a zip') }]))).message).not.toContain(
      'None of',
    );
    expect((await failure(importMany([]))).code).toBe('no-manifest');
  });

  it('gives back everything when cancelled during a later source', async () => {
    const controller = new AbortController();
    const created: string[] = [];
    const create = URL.createObjectURL.bind(URL);
    vi.spyOn(URL, 'createObjectURL').mockImplementation((blob) => {
      const value = create(blob);
      created.push(value);
      return value;
    });
    const error = await failure(
      importMany(
        [
          { kind: 'archive', file: zip('part1.zip', PART1) },
          { kind: 'archive', file: zip('part2.zip', PART2) },
        ],
        {
          signal: controller.signal,
          onProgress: (progress) => {
            if (progress.message.startsWith('part2.zip') && progress.phase === 'images' && progress.done === 1) controller.abort();
          },
        },
      ),
    );
    expect(error.code).toBe('aborted');
    expect(created.length).toBeGreaterThan(2);
    for (const url of created) expect(blobForUrl(url)).toBeUndefined();
  });

  it('shares one media budget between the sources, and a failed source gives its share back', async () => {
    const big = (name: string, fill: number): FileMap => ({
      'post.json': json([{ id: name, user: 'a', video: `posts/${name}.mp4` }]),
      [`posts/${name}.mp4`]: VIDEOS.mp4(fill, 400),
    });
    const input = (name: string, fill: number) => ({ kind: 'archive' as const, file: zip(`${name}.zip`, big(name, fill)) });
    const result = await track(
      importManyWithLimits([input('one', 1), input('two', 2), input('three', 3)], { limits: { maxTotalMediaBytes: 1000 } }),
    );
    // one (400) + two (400) fit; three would make 1200.
    expect(result.posts.map((post) => post.id)).toEqual(['one', 'two']);
    expect(result.sources?.map((source) => source.error === undefined)).toEqual([true, true, false]);
    expect(result.warnings[0].message).toContain('add up to more than');

    // A source that fails half way does not keep its bytes: "mixed" (400 + a broken file) fails, "last" fits.
    const mixed: FileMap = {
      'post.json': json([{ id: 'm', user: 'a', media: ['posts/m.mp4', 'posts/n.mp4'] }]),
      'posts/m.mp4': VIDEOS.mp4(4, 400),
      'posts/n.mp4': VIDEOS.mp4(5, 400),
    };
    const second = await track(
      importManyWithLimits(
        [input('first', 6), { kind: 'archive', file: zip('mixed.zip', mixed) }, input('last', 7)],
        { limits: { maxTotalMediaBytes: 1000 } },
      ),
    );
    expect(second.posts.map((post) => post.id)).toEqual(['first', 'last']);
  });

  it(`imports at most ${MAX_SOURCES} sources and says so`, async () => {
    const files = Array.from({ length: MAX_SOURCES + 2 }, (_, i) =>
      zip(`p${i}.zip`, exportOf([[`P${i}`, undefined]], (i % 200) + 1)),
    );
    const result = await track(importFromFiles(files));
    expect(result.posts).toHaveLength(MAX_SOURCES);
    expect(result.sources).toHaveLength(MAX_SOURCES);
    expect(result.warnings[0]).toEqual({
      code: 'truncated',
      message: `${MAX_SOURCES + 2} archives or folders were selected; only the first ${MAX_SOURCES} were imported.`,
    });
  });
});

describe('importFromDataTransfer with several items', () => {
  it('imports several dropped archives', async () => {
    const { dataTransfer, expire } = fakeDataTransfer([
      { entry: fileEntry(zip('part1.zip', PART1)), file: zip('part1.zip', PART1) },
      { entry: fileEntry(zip('part2.zip', PART2)), file: zip('part2.zip', PART2) },
    ]);
    const promise = importFromDataTransfer(dataTransfer);
    expire();
    const result = await track(promise);
    expect(result.sourceName).toBe('2 archives');
    expect(result.posts.map((post) => post.id)).toEqual(['C', 'A', 'B']);
  });

  it('imports dropped folders and archives together, each folder on its own', async () => {
    const folder = (name: string, files: FileMap) =>
      dirEntry(name, [
        fileEntry(makeFile('post.json', files['post.json'])),
        dirEntry('posts', Object.entries(files).filter(([path]) => path.startsWith('posts/')).map(([path, data]) => fileEntry(makeFile(path.slice(6), data)))),
        dirEntry('avatars', [fileEntry(makeFile('alice.jpg', files['avatars/alice.jpg']))]),
      ]);
    const archive = zip('third.zip', exportOf([['D', '2026-05-01T00:00:00Z']], 3));
    const { dataTransfer, expire } = fakeDataTransfer([
      { entry: folder('first', PART1) },
      { entry: fileEntry(archive), file: archive },
      { entry: folder('second', PART2) },
      { entry: dirEntry('empty', []) },
    ]);
    const promise = importFromDataTransfer(dataTransfer);
    expire();
    const result = await track(promise);
    expect(result.posts.map((post) => post.id)).toEqual(['D', 'C', 'A', 'B']);
    expect(result.sources?.map((source) => [source.name, source.kind, source.posts])).toEqual([
      ['third.zip', 'archive', 1],
      ['first', 'folder', 2],
      ['second', 'folder', 2],
      ['empty', 'folder', 0],
    ]);
    expect(result.warnings.map((warning) => warning.message)).toContain(
      '"empty" could not be imported: The dropped folder "empty" is empty.',
    );
  });

  it('still treats a loose post.json and its posts folder as one export', async () => {
    const manifest = makeFile('post.json', PART1['post.json']);
    const { dataTransfer } = fakeDataTransfer([
      { entry: fileEntry(manifest), file: manifest },
      { entry: dirEntry('posts', [fileEntry(makeFile('A.jpg', IMAGES.jpeg())), fileEntry(makeFile('B.jpg', IMAGES.jpeg()))]) },
    ]);
    const result = await track(importFromDataTransfer(dataTransfer));
    expect(result.sourceKind).toBe('folder');
    expect(result.posts.map((post) => post.id)).toEqual(['A', 'B']);
  });
});
