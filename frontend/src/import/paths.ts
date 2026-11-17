/** Path handling for export members. All paths use forward slashes and have no leading slash. */

export type PathIssue = 'remote' | 'escape' | 'empty';

export type NormalizedPath = { path: string; issue?: undefined } | { path?: undefined; issue: PathIssue };

const REMOTE_RE = /^(?:[a-z][a-z0-9+.-]+:\/\/|(?:data|blob|javascript|file|mailto):)/i;

/**
 * Normalises an image path written in post.json (relative to post.json):
 * backslashes become slashes, a leading "./" or "/" is dropped, "." segments are removed and
 * ".." is resolved. A path that climbs out of the export root or points at a URL is rejected.
 */
export function normalizeRelativePath(raw: string): NormalizedPath {
  const trimmed = raw.trim();
  if (!trimmed) return { issue: 'empty' };
  if (REMOTE_RE.test(trimmed) || trimmed.startsWith('//')) return { issue: 'remote' };

  const segments: string[] = [];
  for (const segment of trimmed.replace(/\\/g, '/').split('/')) {
    if (segment === '' || segment === '.') continue;
    if (segment === '..') {
      if (segments.length === 0) return { issue: 'escape' };
      segments.pop();
      continue;
    }
    segments.push(segment);
  }
  if (segments.length === 0) return { issue: 'empty' };
  return { path: segments.join('/') };
}

/**
 * Normalises the name of a file inside an archive or a selected folder.
 * Returns null for names that are empty or contain ".." (never trusted, never needed).
 */
export function normalizeEntryPath(raw: string): string | null {
  const segments: string[] = [];
  for (const segment of raw.replace(/\\/g, '/').split('/')) {
    if (segment === '' || segment === '.') continue;
    if (segment === '..') return null;
    segments.push(segment);
  }
  return segments.length > 0 ? segments.join('/') : null;
}

export function basename(path: string): string {
  const cut = path.lastIndexOf('/');
  return cut < 0 ? path : path.slice(cut + 1);
}

export function dirname(path: string): string {
  const cut = path.lastIndexOf('/');
  return cut < 0 ? '' : path.slice(0, cut);
}

export function joinPath(root: string, relative: string): string {
  return root ? `${root}/${relative}` : relative;
}

/** Key for case-insensitive lookups. NFC so macOS (NFD) file names match JSON text. */
export function foldPath(path: string): string {
  return path.normalize('NFC').toLowerCase();
}

/** macOS resource forks, Finder/Explorer droppings: never part of an export. */
export function isNoisePath(path: string): boolean {
  const segments = path.split('/');
  if (segments.some((segment) => segment === '__MACOSX')) return true;
  const name = segments[segments.length - 1];
  if (name.startsWith('._')) return true;
  const lower = name.toLowerCase();
  return lower === '.ds_store' || lower === 'thumbs.db' || lower === 'desktop.ini';
}

const MANIFEST_NAMES = ['post.json', 'posts.json'];

/** 0 for post.json, 1 for posts.json, -1 otherwise (case-insensitive, by file name). */
export function manifestRank(path: string): number {
  return MANIFEST_NAMES.indexOf(basename(path).toLowerCase());
}
