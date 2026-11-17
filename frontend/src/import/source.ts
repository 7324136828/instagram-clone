/** A uniform view over "the files of an export", whether they sit in a folder or an archive. */
import { basename, dirname, foldPath, isNoisePath, joinPath, manifestRank, normalizeEntryPath } from './paths';
import { ImportError } from './types';

export interface SourceEntry {
  /** Normalised path inside the folder/archive (forward slashes, no leading slash). */
  path: string;
  /** Size in bytes as declared by the container (uncompressed). */
  size: number;
  /**
   * The content as a Blob. Zero-copy where the container allows it (folders, tar, stored zip
   * members), otherwise decompressed. Rejects with ImportError('too-large') when the content
   * turns out to exceed maxBytes and ImportError('read-failed') when it cannot be read.
   */
  open(maxBytes: number, signal?: AbortSignal): Promise<Blob>;
  /**
   * True when open() hands out bytes held in memory (decompressed members). Otherwise the Blob
   * is a window onto the user's file and stops working when that file is moved, changed or
   * deleted, so whatever is kept beyond the import has to be copied first.
   */
  inMemory?: boolean;
}

export interface PathedFile {
  path: string;
  file: File;
}

/** Entries for files that are already on hand (a picked or dropped folder, loose files). */
export function entriesFromFiles(files: readonly PathedFile[]): SourceEntry[] {
  const entries: SourceEntry[] = [];
  for (const { path: rawPath, file } of files) {
    const path = normalizeEntryPath(rawPath);
    if (!path || isNoisePath(path)) continue;
    entries.push({ path, size: file.size, open: () => Promise.resolve(file) });
  }
  return entries;
}

/**
 * How many trailing folders of a written path are tried when its leading part is foreign
 * ("C:/Users/me/export/posts/a.jpg"). Exports are one or two folders deep.
 */
const MAX_SUFFIX_SEGMENTS = 8;

interface NamedEntry {
  entry: SourceEntry;
  /** foldPath(entry.path), computed once. */
  folded: string;
}

/** Lookup structure over the entries of one source. */
export class EntryIndex {
  private readonly exact = new Map<string, SourceEntry>();
  private readonly folded = new Map<string, SourceEntry>();
  /** Image candidates (everything but post.json) by case-folded file name. */
  private readonly byName = new Map<string, NamedEntry[]>();
  /**
   * Answers of the by-name fallback. Without it, an archive with thousands of files of one
   * name and a post.json full of unmatched paths costs files x paths.
   */
  private readonly byNameAnswers = new Map<string, SourceEntry | undefined>();

  constructor(entries: readonly SourceEntry[]) {
    for (const entry of entries) {
      // A name stored twice in an archive: the later member replaces the earlier one.
      this.exact.set(entry.path, entry);
    }
    for (const [path, entry] of this.exact) {
      const folded = foldPath(path);
      if (!this.folded.has(folded)) this.folded.set(folded, entry);
      if (manifestRank(path) >= 0) continue;
      const name = basename(folded);
      const list = this.byName.get(name);
      if (list) list.push({ entry, folded });
      else this.byName.set(name, [{ entry, folded }]);
    }
  }

  get size(): number {
    return this.exact.size;
  }

  /** post.json (or posts.json), matched case-insensitively; the shallowest one wins. */
  findManifest(): SourceEntry | undefined {
    let best: SourceEntry | undefined;
    let bestKey: [number, number, string] | undefined;
    for (const entry of this.exact.values()) {
      const rank = manifestRank(entry.path);
      if (rank < 0) continue;
      const key: [number, number, string] = [entry.path.split('/').length, rank, entry.path];
      if (
        !bestKey ||
        key[0] < bestKey[0] ||
        (key[0] === bestKey[0] && (key[1] < bestKey[1] || (key[1] === bestKey[1] && key[2] < bestKey[2])))
      ) {
        best = entry;
        bestKey = key;
      }
    }
    return best;
  }

  /**
   * Finds the file for a path written in post.json, relative to the export root:
   * exact match, then case-insensitive, then the path without its leading folders (it was
   * written from outside the export), then (last resort, for loose files that lost their
   * folder) a unique file of the same name. Nothing outside the export root is ever returned.
   */
  resolve(root: string, relativePath: string): SourceEntry | undefined {
    const full = joinPath(root, relativePath);
    const direct = this.exact.get(full) ?? this.folded.get(foldPath(full));
    if (direct) return direct;

    const prefix = root ? `${foldPath(root)}/` : '';
    const segments = foldPath(relativePath).split('/');

    // "C:/Users/me/export/posts/a.jpg", "export/posts/a.jpg": the longest tail of at least
    // two segments that names a file of the export. A lone file name is left to the rule below.
    for (let drop = Math.max(1, segments.length - MAX_SUFFIX_SEGMENTS); drop < segments.length - 1; drop++) {
      const found = this.folded.get(prefix + segments.slice(drop).join('/'));
      if (found && manifestRank(found.path) < 0) return found;
    }

    // By file name, but only where it cannot pick up an unrelated file: "posts/a.jpg" may be
    // found as "a.jpg" right next to post.json (loose files), and a bare "a.jpg" may be found
    // in a subfolder. "avatars/a.jpg" is never satisfied by "posts/a.jpg".
    const name = segments[segments.length - 1];
    const sameName = this.byName.get(name);
    if (!sameName) return undefined;
    const bare = segments.length === 1;
    const key = `${bare ? 'bare' : 'path'}:${prefix}${name}`;
    if (!this.byNameAnswers.has(key)) {
      const candidates = sameName.filter(({ folded }) => {
        if (!folded.startsWith(prefix)) return false;
        return bare || !folded.slice(prefix.length).includes('/');
      });
      this.byNameAnswers.set(key, candidates.length === 1 ? candidates[0].entry : undefined);
    }
    return this.byNameAnswers.get(key);
  }
}

export function exportRootOf(manifest: SourceEntry): string {
  return dirname(manifest.path);
}

export function noManifestError(sourceName: string): ImportError {
  return new ImportError(
    'no-manifest',
    `No post.json was found in "${sourceName}". An export contains post.json next to a posts folder.`,
  );
}
