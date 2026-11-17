import { describe, expect, it } from 'vitest';
import type { ImportResult, LibraryResponse } from './import';
import {
  COMMIT_MESSAGE, commitCounts, commitMessage, expectedPosts, failedSources, failedSourcesLine, formatBytes, importedFrom,
  reportOf, savedLine, savedReport, sourceLine, unsavedLine, uploadMessage,
} from './importReport';

function result(overrides: Partial<ImportResult> = {}): ImportResult {
  return { sourceName: 'a.zip', sourceKind: 'archive', posts: [], warnings: [], totalEntries: 0, ...overrides };
}

describe('the report of an import action', () => {
  it('keeps the counts and warnings, and lists the sources of a merged import only', () => {
    const warning = { code: 'missing-image' as const, message: 'Post 2: missing' };
    expect(reportOf(result({ totalEntries: 3, warnings: [warning] }))).toEqual({
      sourceName: 'a.zip', posts: 0, totalEntries: 3, warnings: [warning], sources: [],
    });
    const sources = [
      { name: 'part1.zip', kind: 'archive' as const, posts: 2, warnings: 0 },
      { name: 'part2.zip', kind: 'archive' as const, posts: 1, warnings: 1 },
    ];
    expect(reportOf(result({ sourceKind: 'multiple', sourceName: '2 archives', sources })).sources).toEqual(sources);
    expect(reportOf(result({ sources: [sources[0]] })).sources).toEqual([]);
  });

  it('does not count a post that a later archive replaced as a skipped entry', () => {
    const post = { id: 'a', media: [], user: { username: 'u' }, caption: '', commentCount: 0, likeCount: null, mediaType: 'image' as const, comments: [] };
    const merged = result({
      sourceKind: 'multiple',
      posts: [post, { ...post, id: 'b' }, { ...post, id: 'c' }],
      totalEntries: 5,
      warnings: [
        { code: 'merged-duplicate', message: 'The post "a" from part2.zip replaces the one from part1.zip.' },
        { code: 'missing-image', message: 'Post 4: missing' },
      ],
    });
    expect(expectedPosts(merged)).toBe(4);
    expect(reportOf(merged).totalEntries).toBe(4);
    expect(expectedPosts(result({ totalEntries: 0, posts: [post] }))).toBe(1);
  });

  it('takes what was saved from the library entry the commit appended', () => {
    const library: LibraryResponse = {
      directory: 'C:\\Temp\\instagram_cache',
      revision: 2,
      updated_at: null,
      posts: [],
      imports: [
        { id: '1', source_name: 'old.zip', source_kind: 'archive', imported_at: '', posts: 9, new: 9, updated: 0 },
        { id: '2', source_name: 'a.zip', source_kind: 'archive', imported_at: '', posts: 3, new: 2, updated: 1 },
      ],
    };
    expect(savedReport(reportOf(result()), library).saved).toEqual({ directory: 'C:\\Temp\\instagram_cache', added: 2, updated: 1 });
    expect(savedReport(reportOf(result()), { ...library, imports: [] }).saved).toEqual({
      directory: 'C:\\Temp\\instagram_cache', added: 0, updated: 0,
    });
    expect(commitCounts(library)).toEqual({ added: 2, updated: 1 });
    // An import saved in several parts passes the sum over its commits.
    expect(savedReport(reportOf(result()), library, { added: 5000, updated: 7 }).saved).toEqual({
      directory: 'C:\\Temp\\instagram_cache', added: 5000, updated: 7,
    });
  });

  it('says how much of an import saved in parts was not saved', () => {
    expect(unsavedLine({ saved: 5000, posts: 6000, reason: 'Saving to the library failed: no space.  ' })).toBe(
      'Saved 5,000 of 6,000 posts; the rest could not be saved: Saving to the library failed: no space.'
        + ' Import the archives again to save the rest (posts already saved are updated, not duplicated).',
    );
  });
});

describe('texts', () => {
  it.each([
    [{ name: 'p.zip', kind: 'archive' as const, posts: 1, warnings: 0 }, 'p.zip: 1 post'],
    [{ name: 'p.zip', kind: 'archive' as const, posts: 1200, warnings: 2 }, 'p.zip: 1,200 posts, 2 warnings'],
    [{ name: 'b.zip', kind: 'archive' as const, posts: 0, warnings: 0, error: 'No post.json was found.' }, 'b.zip: not imported (No post.json was found)'],
  ])('describes a source', (source, line) => {
    expect(sourceLine(source)).toBe(line);
  });

  it.each([
    [{ added: 4, updated: 2 }, '4 new posts and 2 updated'],
    [{ added: 1, updated: 0 }, '1 new post'],
    [{ added: 0, updated: 2 }, '2 updated posts'],
    [{ added: 0, updated: 0 }, 'no changes'],
  ])('describes what was saved', (counts, line) => {
    expect(savedLine({ directory: 'x', ...counts })).toBe(line);
  });

  it.each([
    [0, '0 bytes'], [1, '1 byte'], [1023, '1,023 bytes'], [1024, '1 KB'], [1536, '1.5 KB'],
    [12 * 1024 * 1024, '12 MB'], [3.45 * 1024 ** 3, '3.5 GB'], [Number.NaN, '0 bytes'],
  ])('formats %d bytes', (bytes, text) => {
    expect(formatBytes(bytes)).toBe(text);
  });

  it('says which file is being saved', () => {
    expect(uploadMessage({ loaded: 1536, total: 3 * 1024 * 1024, file: 2, files: 12 }))
      .toBe('Saving to your library: file 2 of 12');
    expect(uploadMessage({ loaded: 0, total: 10, file: 1, files: 1 }))
      .toBe('Saving 1 file to your library');
    expect(uploadMessage({ loaded: 0, total: 10, file: 7, files: 9 }, { part: 2, parts: 3 }))
      .toBe('Saving to your library: file 7 of 9 (part 2 of 3)');
    expect(uploadMessage({ loaded: 0, total: 10, file: 1, files: 9 }, { part: 1, parts: 1 }))
      .toBe('Saving to your library: file 1 of 9');
    expect(commitMessage(1, 1)).toBe(COMMIT_MESSAGE);
    expect(commitMessage(1, 2)).toBe('Adding part 1 of 2 to your library…');
  });
});

describe('where the posts of a finished import came from', () => {
  const source = (name: string, posts: number, error?: string) => ({
    name, kind: 'archive' as const, posts, warnings: 0, ...(error ? { error } : {}),
  });

  it('names the source of a single import', () => {
    expect(importedFrom(reportOf(result({ sourceName: 'a.zip', totalEntries: 0 }))))
      .toBe('a.zip');
  });

  it('counts only the sources that were imported when some failed', () => {
    const report = reportOf(result({
      sourceName: '5 archives',
      sourceKind: 'multiple',
      sources: [source('p1.zip', 20), source('bad.zip', 0, 'Not a zip'), source('p2.zip', 10), source('x.zip', 0, 'No post.json'), source('v.zip', 3)],
    }));
    expect(failedSources(report)).toBe(2);
    expect(importedFrom(report)).toBe('3 of 5 archives');
    expect(failedSourcesLine(report)).toBe('2 of 5 archives could not be imported.');
  });

  it('says "sources" for a mix of archives and folders', () => {
    const report = reportOf(result({
      sourceName: '1 archive and 1 folder',
      sourceKind: 'multiple',
      sources: [source('p1.zip', 2), { name: 'export', kind: 'folder', posts: 0, warnings: 0, error: 'No post.json' }],
    }));
    expect(importedFrom(report)).toBe('1 of 2 sources');
  });

  it('keeps the source name when every source was imported', () => {
    const report = reportOf(result({ sourceName: '2 archives', sourceKind: 'multiple', sources: [source('a.zip', 1), source('b.zip', 1)] }));
    expect(failedSources(report)).toBe(0);
    expect(importedFrom(report)).toBe('2 archives');
    expect(failedSourcesLine(report)).toBe('');
  });
});
