import { describe, expect, it } from 'vitest';
import { EntryIndex, type SourceEntry } from './source';

function entry(path: string): SourceEntry {
  return { path, size: 1, open: () => Promise.resolve(new Blob(['x'])) };
}

function indexOf(...paths: string[]): EntryIndex {
  return new EntryIndex(paths.map(entry));
}

describe('EntryIndex.resolve', () => {
  it('finds a file by its exact path, then case-insensitively', () => {
    const index = indexOf('post.json', 'posts/A.jpg', 'posts/a.jpg', 'posts/Café.png');
    expect(index.resolve('', 'posts/A.jpg')?.path).toBe('posts/A.jpg');
    expect(index.resolve('', 'posts/a.jpg')?.path).toBe('posts/a.jpg');
    expect(index.resolve('', 'POSTS/CAFÉ.PNG')?.path).toBe('posts/Café.png');
    expect(index.resolve('', 'posts/cafe\u0301.png')?.path).toBe('posts/Café.png'); // NFD, as macOS writes it
    expect(index.resolve('', 'posts/b.jpg')).toBeUndefined();
  });

  it.each([
    'C:\\Users\\me\\instagram-export\\posts\\post1.jpg',
    'C:/Users/me/instagram-export/posts/post1.jpg',
    '/home/me/instagram-export/posts/post1.jpg',
    'instagram-export/posts/post1.jpg',
    '~/export/posts/post1.jpg',
    'Export/POSTS/Post1.JPG',
  ])('finds %s although it was written from outside the export', (written) => {
    // post1.jpg also exists next to post.json and under avatars/: the longest match must win.
    const normalized = written.replace(/\\/g, '/').replace(/^\/+/, '');
    for (const root of ['', 'instagram-export', 'a/b']) {
      const at = (path: string) => (root ? `${root}/${path}` : path);
      const index = indexOf(at('post.json'), at('post1.jpg'), at('posts/post1.jpg'), at('avatars/post1.jpg'));
      expect(index.resolve(root, normalized)?.path).toBe(at('posts/post1.jpg'));
    }
  });

  it('never answers a path with a file from another folder of the export', () => {
    const index = indexOf('export/post.json', 'export/posts/a.jpg', 'export/posts/deep/b.jpg', 'outside/c.jpg', 'c.jpg');
    expect(index.resolve('export', 'avatars/a.jpg')).toBeUndefined();
    expect(index.resolve('export', 'x/y/avatars/a.jpg')).toBeUndefined();
    expect(index.resolve('export', 'deep/b.jpg')).toBeUndefined();
    // Nothing above the export root is reachable, however the path is written.
    expect(index.resolve('export', 'outside/c.jpg')).toBeUndefined();
    expect(index.resolve('export', 'export/outside/c.jpg')).toBeUndefined();
    expect(index.resolve('export', 'c.jpg')).toBeUndefined();
    // post.json is never an image.
    expect(index.resolve('export', 'somewhere/post.json')).toBeUndefined();
  });

  it('keeps the loose-file fallbacks: a lost folder, and a bare name found in a subfolder', () => {
    const loose = indexOf('post.json', 'post1.jpg');
    expect(loose.resolve('', 'posts/post1.jpg')?.path).toBe('post1.jpg');
    const nested = indexOf('post.json', 'posts/post1.jpg', 'posts/dup.jpg', 'avatars/dup.jpg');
    expect(nested.resolve('', 'post1.jpg')?.path).toBe('posts/post1.jpg');
    expect(nested.resolve('', 'dup.jpg')).toBeUndefined(); // ambiguous
  });

  it('copes with absurdly deep paths', () => {
    const index = indexOf('post.json', 'posts/post1.jpg');
    const deep = `${'x/'.repeat(200_000)}posts/post1.jpg`;
    const started = performance.now();
    expect(index.resolve('', deep)?.path).toBe('posts/post1.jpg');
    expect(index.resolve('', `${'x/'.repeat(200_000)}nope.jpg`)).toBeUndefined();
    expect(performance.now() - started).toBeLessThan(1000);
  });

  it('does not rescan the files of the same name for every unmatched path', () => {
    // A hostile archive: thousands of members called a.png, and a post.json full of paths that
    // match none of them. Every lookup used to walk (and case-fold) all of them again.
    const MEMBERS = 2000;
    const LOOKUPS = 1000;
    let pathReads = 0;
    const entries: SourceEntry[] = [entry('post.json')];
    for (let i = 0; i < MEMBERS; i++) {
      const path = `d${i}/a.png`;
      entries.push({
        get path() {
          pathReads++;
          return path;
        },
        size: 1,
        open: () => Promise.resolve(new Blob(['x'])),
      });
    }
    const index = new EntryIndex(entries);
    const afterIndexing = pathReads;
    for (let i = 0; i < LOOKUPS; i++) {
      expect(index.resolve('', `missing${i}/a.png`)).toBeUndefined();
      expect(index.resolve('', 'a.png')).toBeUndefined(); // ambiguous: 2000 candidates
      expect(index.resolve('', `x/y${i}/z/a.png`)).toBeUndefined();
    }
    // Linear in the number of members, independent of the number of lookups.
    expect(pathReads - afterIndexing).toBeLessThanOrEqual(MEMBERS * 4);
    expect(index.resolve('', 'D7/A.PNG')?.path).toBe('d7/a.png');
  });

  it('answers the by-name question once, not once per path', () => {
    // 20,000 members of one name and 10,000 paths that only the by-name rule can answer:
    // walking the candidates for each path takes seconds, remembering the answer does not.
    const entries: SourceEntry[] = [entry('post.json')];
    for (let i = 0; i < 20_000; i++) entries.push(entry(`d${i}/a.png`));
    const index = new EntryIndex(entries);
    const started = performance.now();
    let found = 0;
    for (let i = 0; i < 5000; i++) {
      if (index.resolve('', `missing${i}/a.png`)) found++;
      if (index.resolve('', 'a.png')) found++;
    }
    expect(performance.now() - started).toBeLessThan(500);
    expect(found).toBe(0);
    // A remembered "no" for one folder is not the answer for another.
    const mixed = indexOf('x/post.json', 'x/one.png', 'y/deep/one.png');
    expect(mixed.resolve('x', 'gone/one.png')?.path).toBe('x/one.png');
    expect(mixed.resolve('y', 'gone/one.png')).toBeUndefined();
    expect(mixed.resolve('y', 'one.png')?.path).toBe('y/deep/one.png');
    expect(mixed.resolve('x', 'one.png')?.path).toBe('x/one.png');
  });
});
