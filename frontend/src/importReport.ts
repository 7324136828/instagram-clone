/**
 * What the UI tells the user about an import action: the summary shown in the import dialog and
 * behind the banner's warnings link. It outlives the parsed result (whose object URLs are released
 * once the posts are saved in the library).
 */
import type { ImportResult, ImportSourceSummary, ImportWarning, LibraryResponse } from './import';
import type { LibraryUploadProgress } from './library/client';

export interface ImportReport {
  /** File or folder name, or "2 archives and 1 folder" for a merged import. */
  sourceName: string;
  /** Posts the import action produced. */
  posts: number;
  /** Entries listed in the post.json files (posts can be lower when entries were skipped). */
  totalEntries: number;
  warnings: ImportWarning[];
  /** Every archive / folder of a merged import (empty for a single source). */
  sources: ImportSourceSummary[];
  /** Library mode: the posts were saved in the cache folder. */
  saved?: { directory: string; added: number; updated: number };
  /** Library mode: saving failed, the posts are shown from this browser only. */
  saveError?: string;
  /**
   * Library mode, an import saved in several parts (see prepareLibraryUploads): the parts after
   * the last saved one were not saved. The posts on screen are the library, with the saved parts.
   */
  unsaved?: { saved: number; posts: number; reason: string };
}

/** "1 post", "1,200 posts". */
export function countLabel(count: number, word: string): string {
  return `${count.toLocaleString('en-US')} ${word}${count === 1 ? '' : 's'}`;
}

/**
 * The entries of post.json that should have become posts: a post that a later archive replaced
 * (a 'merged-duplicate') was not skipped, so it does not count as one that is missing.
 */
export function expectedPosts(result: ImportResult): number {
  const replaced = result.warnings.filter((warning) => warning.code === 'merged-duplicate').length;
  return Math.max(result.totalEntries - replaced, result.posts.length);
}

export function reportOf(result: ImportResult): ImportReport {
  return {
    sourceName: result.sourceName,
    posts: result.posts.length,
    totalEntries: expectedPosts(result),
    warnings: result.warnings,
    sources: result.sources && result.sources.length > 1 ? result.sources : [],
  };
}

/** New and updated posts of the commit that produced `library` (it appended its own entry to `imports`). */
export function commitCounts(library: LibraryResponse): { added: number; updated: number } {
  const latest = library.imports[library.imports.length - 1];
  const count = (value: unknown) => (typeof value === 'number' && Number.isFinite(value) && value > 0 ? value : 0);
  return { added: count(latest?.new), updated: count(latest?.updated) };
}

/**
 * The report of an import that was saved. `counts` defaults to the commit that produced `library`;
 * an import saved in several parts passes the sum over its commits.
 */
export function savedReport(
  report: ImportReport,
  library: LibraryResponse,
  counts: { added: number; updated: number } = commitCounts(library),
): ImportReport {
  return { ...report, saved: { directory: library.directory, added: counts.added, updated: counts.updated } };
}

/**
 * "Saved 5,000 of 6,000 posts; the rest could not be saved: <reason> Import ..." for an import
 * whose later parts were not saved.
 */
export function unsavedLine(unsaved: NonNullable<ImportReport['unsaved']>): string {
  const reason = unsaved.reason.trim().replace(/[.\s]*$/, '.');
  return `Saved ${unsaved.saved.toLocaleString('en-US')} of ${countLabel(unsaved.posts, 'post')}; the rest could not be saved: `
    + `${reason} Import the archives again to save the rest (posts already saved are updated, not duplicated).`;
}

/** "part2.zip: 3 posts, 1 warning", or "broken.zip: not imported (reason)". */
export function sourceLine(source: ImportSourceSummary): string {
  if (source.error) return `${source.name}: not imported (${source.error.replace(/[.\s]+$/, '')})`;
  const parts = [countLabel(source.posts, 'post')];
  if (source.warnings > 0) parts.push(countLabel(source.warnings, 'warning'));
  return `${source.name}: ${parts.join(', ')}`;
}

/** "4 new posts and 2 updated", "1 new post", "2 updated posts", "no changes". */
export function savedLine(saved: NonNullable<ImportReport['saved']>): string {
  if (saved.added > 0 && saved.updated > 0) return `${countLabel(saved.added, 'new post')} and ${saved.updated.toLocaleString('en-US')} updated`;
  if (saved.added > 0) return countLabel(saved.added, 'new post');
  if (saved.updated > 0) return countLabel(saved.updated, 'updated post');
  return 'no changes';
}

const UNITS = ['KB', 'MB', 'GB', 'TB'];

/** 512 bytes, 1.5 KB, 12 MB, 3.4 GB. */
export function formatBytes(bytes: number): string {
  if (!Number.isFinite(bytes) || bytes < 1024) return countLabel(Math.max(0, Math.round(bytes || 0)), 'byte');
  let value = bytes / 1024;
  let unit = 0;
  while (value >= 1024 && unit < UNITS.length - 1) {
    value /= 1024;
    unit += 1;
  }
  return `${value < 10 ? value.toFixed(1).replace(/\.0$/, '') : Math.round(value)} ${UNITS[unit]}`;
}

/** Status line while the files of an import are sent to the library (the dialog shows the bytes under its bar). */
export function uploadMessage(progress: LibraryUploadProgress, part?: { part: number; parts: number }): string {
  const text = progress.files === 1
    ? 'Saving 1 file to your library'
    : `Saving to your library: file ${progress.file.toLocaleString('en-US')} of ${progress.files.toLocaleString('en-US')}`;
  return part && part.parts > 1 ? `${text} (part ${part.part} of ${part.parts})` : text;
}

/** Status line while the library merges an upload (after the last file was sent; it cannot be cancelled any more). */
export const COMMIT_MESSAGE = 'Adding the posts to your library…';

/** COMMIT_MESSAGE for one part of an import saved in several parts. */
export function commitMessage(part: number, parts: number): string {
  return parts > 1 ? `Adding part ${part} of ${parts} to your library…` : COMMIT_MESSAGE;
}

/** The archives / folders of a merged import that could not be imported at all. */
export function failedSources(report: ImportReport): number {
  return report.sources.filter((source) => source.error).length;
}

/**
 * Where the posts of a finished import came from, for its headline ("Imported 6 of 6 posts from
 * <this>"): the source name, or, when some sources of a merged import failed, how many of them
 * contributed ("3 of 5 archives") so that the failed ones are not counted as imported.
 */
export function importedFrom(report: ImportReport): string {
  const failed = failedSources(report);
  if (failed === 0) return report.sourceName;
  const total = report.sources.length;
  return `${(total - failed).toLocaleString('en-US')} of ${countLabel(total, sourceNoun(report))}`;
}

/** "2 of 5 archives could not be imported." (empty when every source was imported). */
export function failedSourcesLine(report: ImportReport): string {
  const failed = failedSources(report);
  if (failed === 0) return '';
  const total = report.sources.length;
  return `${failed.toLocaleString('en-US')} of ${countLabel(total, sourceNoun(report))} could not be imported.`;
}

/** What the sources of a merged import are: archives, folders, or a mix ("source"). */
function sourceNoun(report: ImportReport): string {
  const kinds = new Set(report.sources.map((source) => source.kind));
  if (kinds.size === 1 && kinds.has('archive')) return 'archive';
  if (kinds.size === 1 && kinds.has('folder')) return 'folder';
  return 'source';
}
