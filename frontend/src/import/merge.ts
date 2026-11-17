/** Merging the results of several imports (several archives/folders, or a new import into an old one). */
import { postUrls, revokeBlobUrl } from './registry';
import {
  ImportError,
  type ImportResult,
  type ImportSourceSummary,
  type ImportWarning,
  type ImportedPost,
} from './types';

/** The archives/folders behind a result: its `sources` when it was merged, otherwise itself. */
export function sourcesOf(result: ImportResult): ImportSourceSummary[] {
  if (result.sources && result.sources.length > 0) return result.sources.map((source) => ({ ...source }));
  const kind = result.sourceKind === 'archive' || result.sourceKind === 'folder' ? result.sourceKind : 'multiple';
  return [{ name: result.sourceName, kind, posts: result.posts.length, warnings: result.warnings.length }];
}

function counted(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? '' : 's'}`;
}

/** "3 archives", "2 archives and 1 folder", "1 folder and 2 imports". */
export function describeSources(sources: readonly ImportSourceSummary[]): string {
  const archives = sources.filter((source) => source.kind === 'archive').length;
  const folders = sources.filter((source) => source.kind === 'folder').length;
  const imports = sources.length - archives - folders;
  const parts: string[] = [];
  if (archives) parts.push(counted(archives, 'archive'));
  if (folders) parts.push(counted(folders, 'folder'));
  if (imports) parts.push(counted(imports, 'import'));
  if (parts.length <= 1) return parts[0] ?? 'Nothing';
  return `${parts.slice(0, -1).join(', ')} and ${parts[parts.length - 1]}`;
}

function takenAtMs(post: ImportedPost): number {
  return post.takenAt ? Date.parse(post.takenAt) : Number.NaN;
}

/** Newest first; posts without a (valid) date after all dated ones, in the order given. */
export function sortPosts(posts: readonly ImportedPost[]): ImportedPost[] {
  const dated = posts.filter((post) => Number.isFinite(takenAtMs(post)));
  const undated = posts.filter((post) => !Number.isFinite(takenAtMs(post)));
  // Array.prototype.sort is stable: posts of the same moment keep their order.
  dated.sort((a, b) => takenAtMs(b) - takenAtMs(a));
  return [...dated, ...undated];
}

/** Merges two or more results; see mergeImportResults. */
export function mergeResults(results: readonly ImportResult[]): ImportResult {
  const winners = new Map<string, { post: ImportedPost; from: number }>();
  const replaced: ImportedPost[] = [];
  const warnings: ImportWarning[] = [];
  results.forEach((result, from) => {
    // A merged result's warnings already say which source they are about.
    const prefix = result.sourceKind === 'multiple' ? '' : `${result.sourceName}: `;
    for (const warning of result.warnings) warnings.push({ ...warning, message: `${prefix}${warning.message}` });
    for (const post of result.posts) {
      const previous = winners.get(post.id);
      if (previous) {
        replaced.push(previous.post);
        warnings.push({
          code: 'merged-duplicate',
          message: `The post "${post.id}" from ${result.sourceName} replaces the one with the same id from ${results[previous.from].sourceName}.`,
        });
      }
      // Updating a Map entry keeps its position: the newer post takes the older one's place.
      winners.set(post.id, { post, from });
    }
  });

  const posts = sortPosts([...winners.values()].map(({ post }) => post));
  const kept = new Set(posts.flatMap(postUrls));
  for (const post of replaced) {
    for (const url of postUrls(post)) {
      if (!kept.has(url)) revokeBlobUrl(url);
    }
  }

  const sources = results.flatMap(sourcesOf);
  const merged: ImportResult = {
    sourceName: describeSources(sources),
    sourceKind: 'multiple',
    posts,
    warnings,
    totalEntries: results.reduce((sum, result) => sum + result.totalEntries, 0),
    sources,
  };
  const exportedAt = results
    .map((result) => result.exportedAt)
    .filter((value): value is string => typeof value === 'string' && Number.isFinite(Date.parse(value)))
    .sort((a, b) => Date.parse(b) - Date.parse(a))[0];
  if (exportedAt) merged.exportedAt = exportedAt;
  const sourceUrls = new Set(results.map((result) => result.sourceUrl));
  const [sourceUrl] = sourceUrls;
  if (sourceUrls.size === 1 && sourceUrl) merged.sourceUrl = sourceUrl;
  return merged;
}

/**
 * Merges import results into one, e.g. several archives imported together or a new import
 * added to the current one. The inputs are not modified, but the merged result takes over
 * their object URLs: release the merged result, never the inputs, afterwards.
 *
 * - Later results win: a post whose id was already imported replaces the earlier one (which
 *   keeps its place among undated posts) and a 'merged-duplicate' warning says so; the object
 *   URLs only the replaced post used are released.
 * - Posts are sorted by takenAt, newest first; undated posts follow in their original order.
 * - Warnings are prefixed with the name of the source they are about.
 * - `sourceKind` is 'multiple' and `sources` lists every archive/folder with its counts.
 * A single result is returned as it is.
 */
export function mergeImportResults(results: readonly ImportResult[]): ImportResult {
  if (results.length === 0) throw new ImportError('no-posts', 'There is nothing to merge.');
  if (results.length === 1) return results[0];
  return mergeResults(results);
}
