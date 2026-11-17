/**
 * Persistence of the last import in IndexedDB, so that it survives a reload.
 *
 * Everything lives in one object store and every operation is a single transaction, so the
 * stored state is always either the complete previous import or the complete new one.
 * Operations are queued in call order; a save that was overtaken by a newer save or clear
 * is dropped (last writer wins). Nothing here throws: storage may be unavailable (private
 * mode), full, or blocked, and the import still works without it.
 */
import { blobForUrl, registerBlobUrl } from './registry';
import type {
  ImportResult,
  ImportSourceSummary,
  ImportWarning,
  ImportedComment,
  ImportedMedia,
  ImportedMediaType,
  ImportedPost,
  ImportedUser,
} from './types';
import { sanitizePostUrl, sanitizeProfileUrl, sanitizeSourceUrl } from './urls';

export const IMPORT_DB_NAME = 'ig-clone-import';
const DB_VERSION = 1;
const STORE = 'import';
const META_KEY = 'meta';
const BLOB_PREFIX = 'blob:';
/** 2: posts keep `media` (pictures and videos). 1 (still read): posts kept `images`. */
const SCHEMA = 2;
const LEGACY_SCHEMA = 1;
const OPEN_TIMEOUT_MS = 10_000;

/** One picture or video. Schema 1 stored pictures only, without `kind`. */
interface StoredMedia {
  blob: string;
  kind?: 'image' | 'video';
  path: string;
  mime: string;
  converted?: boolean;
  /** Blob key of a video's poster. */
  poster?: string;
  posterPath?: string;
  bytes?: number;
  alt?: string;
}

/** The kinds of result that are kept in the browser (a library result lives on the server). */
type StoredSourceKind = 'archive' | 'folder' | 'multiple';

interface StoredUser {
  username: string;
  fullName?: string;
  verified?: boolean;
  avatar?: string;
  profileUrl?: string;
}

interface StoredPost {
  id: string;
  permalink?: string;
  media: StoredMedia[];
  /** Schema 1. */
  images?: StoredMedia[];
  user: StoredUser;
  caption: string;
  commentCount: number;
  likeCount: number | null;
  takenAt?: string;
  mediaType: ImportedMediaType;
  alt?: string;
  location?: string;
  comments: ImportedComment[];
}

interface StoredImport {
  schema: number;
  savedAt: string;
  sourceName: string;
  sourceKind: StoredSourceKind;
  posts: StoredPost[];
  warnings: ImportWarning[];
  totalEntries: number;
  exportedAt?: string;
  sourceUrl?: string;
  sources?: ImportSourceSummary[];
  /** Keys (without prefix) of the blob records that belong to this import. */
  blobKeys: string[];
}

interface Snapshot {
  meta: StoredImport;
  blobs: Map<string, Blob>;
}

/** Captures the result and the blobs behind its object URLs. Null when a blob is gone. */
function takeSnapshot(result: ImportResult): Snapshot | null {
  const blobs = new Map<string, Blob>();
  const keysByUrl = new Map<string, string>();
  const keyFor = (url: string): string | null => {
    const known = keysByUrl.get(url);
    if (known) return known;
    const blob = blobForUrl(url);
    if (!blob) return null;
    const key = `b${keysByUrl.size}`;
    keysByUrl.set(url, key);
    blobs.set(key, blob);
    return key;
  };

  const sourceKind = result.sourceKind;
  if (sourceKind === 'library') return null; // the library is kept by the server
  const posts: StoredPost[] = [];
  for (const post of result.posts) {
    const media: StoredMedia[] = [];
    for (const item of post.media) {
      const key = keyFor(item.url);
      if (!key) return null; // already released: there is nothing left to store
      const stored: StoredMedia = { blob: key, kind: item.kind, path: item.path, mime: item.mime };
      if (item.converted) stored.converted = true;
      if (item.posterUrl !== undefined) {
        const poster = keyFor(item.posterUrl);
        if (poster) {
          stored.poster = poster;
          if (item.posterPath !== undefined) stored.posterPath = item.posterPath;
        }
      }
      if (item.bytes !== undefined) stored.bytes = item.bytes;
      if (item.alt !== undefined) stored.alt = item.alt;
      media.push(stored);
    }
    const user: StoredUser = { username: post.user.username };
    if (post.user.fullName !== undefined) user.fullName = post.user.fullName;
    if (post.user.verified !== undefined) user.verified = post.user.verified;
    if (post.user.profileUrl !== undefined) user.profileUrl = post.user.profileUrl;
    if (post.user.avatarUrl !== undefined) {
      const key = keyFor(post.user.avatarUrl);
      if (key) user.avatar = key;
    }
    const stored: StoredPost = {
      id: post.id,
      media,
      user,
      caption: post.caption,
      commentCount: post.commentCount,
      likeCount: post.likeCount,
      mediaType: post.mediaType,
      comments: post.comments.map((comment) => ({ ...comment })),
    };
    if (post.permalink !== undefined) stored.permalink = post.permalink;
    if (post.takenAt !== undefined) stored.takenAt = post.takenAt;
    if (post.alt !== undefined) stored.alt = post.alt;
    if (post.location !== undefined) stored.location = post.location;
    posts.push(stored);
  }
  if (posts.length === 0) return null;

  const meta: StoredImport = {
    schema: SCHEMA,
    savedAt: new Date().toISOString(),
    sourceName: result.sourceName,
    sourceKind,
    posts,
    warnings: result.warnings.map((warning) => ({ ...warning })),
    totalEntries: result.totalEntries,
    blobKeys: [...blobs.keys()],
  };
  if (result.exportedAt !== undefined) meta.exportedAt = result.exportedAt;
  if (result.sourceUrl !== undefined) meta.sourceUrl = result.sourceUrl;
  if (result.sources !== undefined) meta.sources = result.sources.map((source) => ({ ...source }));
  return { meta, blobs };
}

function openDatabase(): Promise<IDBDatabase> {
  return new Promise<IDBDatabase>((resolve, reject) => {
    // Accessing the global can itself throw (sandboxed frames, some private modes).
    const factory: IDBFactory | undefined = globalThis.indexedDB;
    if (!factory) {
      reject(new Error('IndexedDB is not available'));
      return;
    }
    let settled = false;
    const request = factory.open(IMPORT_DB_NAME, DB_VERSION);
    const timer = setTimeout(() => {
      if (settled) return;
      settled = true;
      reject(new Error('IndexedDB did not open in time'));
    }, OPEN_TIMEOUT_MS);
    request.onupgradeneeded = () => {
      const db = request.result;
      if (!db.objectStoreNames.contains(STORE)) db.createObjectStore(STORE);
    };
    request.onsuccess = () => {
      clearTimeout(timer);
      const db = request.result;
      if (settled) {
        db.close();
        return;
      }
      settled = true;
      // Another tab upgrading the schema must not be blocked by this connection.
      db.onversionchange = () => db.close();
      resolve(db);
    };
    request.onerror = () => {
      clearTimeout(timer);
      if (settled) return;
      settled = true;
      reject(request.error ?? new Error('IndexedDB could not be opened'));
    };
  });
}

/** Runs `work` in one transaction and resolves with its value once the transaction commits. */
async function transact<T>(
  mode: IDBTransactionMode,
  work: (store: IDBObjectStore, resolveWith: (value: T) => void) => void,
  fallback: T,
): Promise<T> {
  const db = await openDatabase();
  try {
    return await new Promise<T>((resolve, reject) => {
      let value = fallback;
      const transaction = db.transaction(STORE, mode);
      transaction.oncomplete = () => resolve(value);
      transaction.onabort = () => reject(transaction.error ?? new Error('The transaction was aborted'));
      transaction.onerror = () => reject(transaction.error ?? new Error('The transaction failed'));
      try {
        work(transaction.objectStore(STORE), (result) => {
          value = result;
        });
      } catch (err) {
        // e.g. DataCloneError: undo whatever was queued so that the old state stays intact.
        try {
          transaction.abort();
        } catch {
          // already finished
        }
        reject(err);
      }
    });
  } finally {
    db.close();
  }
}

// Operations run strictly one after the other, in the order they were requested.
let queue: Promise<unknown> = Promise.resolve();
// Bumped by every save and clear; a queued save that is no longer the newest gives up.
let generation = 0;

function enqueue<T>(task: () => Promise<T>): Promise<T> {
  const next = queue.then(task, task);
  queue = next.catch(() => undefined);
  return next;
}

/** Persist the import (manifest + display-ready image blobs) in IndexedDB. Never throws. */
export function saveImport(result: ImportResult): Promise<boolean> {
  let snapshot: Snapshot | null;
  try {
    // Synchronously, so that a release right after the call cannot pull the blobs away.
    snapshot = takeSnapshot(result);
  } catch {
    snapshot = null;
  }
  if (!snapshot) return Promise.resolve(false);
  const { meta, blobs } = snapshot;
  const mine = ++generation;
  return enqueue(async () => {
    if (mine !== generation) return false; // overtaken by a newer save or a clear
    try {
      await transact<void>(
        'readwrite',
        (store) => {
          store.clear();
          for (const [key, blob] of blobs) store.put(blob, BLOB_PREFIX + key);
          store.put(meta, META_KEY);
        },
        undefined,
      );
      return true;
    } catch {
      return false;
    }
  }).catch(() => false);
}

/** Remove the persisted import. Never throws. */
export function clearSavedImport(): Promise<void> {
  generation++;
  return enqueue(async () => {
    try {
      await transact<void>('readwrite', (store) => void store.clear(), undefined);
    } catch {
      // Nothing stored, or storage unavailable: either way there is nothing to clear.
    }
  }).catch(() => undefined);
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function isStoredImport(value: unknown): value is StoredImport {
  return (
    isRecord(value) &&
    (value.schema === SCHEMA || value.schema === LEGACY_SCHEMA) &&
    typeof value.sourceName === 'string' &&
    (value.sourceKind === 'archive' || value.sourceKind === 'folder' || value.sourceKind === 'multiple') &&
    Array.isArray(value.posts) &&
    Array.isArray(value.warnings) &&
    Array.isArray(value.blobKeys)
  );
}

function revive(meta: StoredImport, blobs: Map<string, Blob>): ImportResult | null {
  const urls = new Map<string, string>();
  const urlFor = (key: string | undefined): string | undefined => {
    if (!key) return undefined;
    const known = urls.get(key);
    if (known) return known;
    const blob = blobs.get(key);
    if (!blob) return undefined;
    const url = registerBlobUrl(blob);
    urls.set(key, url);
    return url;
  };

  const posts: ImportedPost[] = [];
  for (const stored of meta.posts) {
    if (!isRecord(stored) || !isRecord(stored.user)) continue;
    // Records saved before videos were supported list `images`.
    const items = Array.isArray(stored.media) ? stored.media : Array.isArray(stored.images) ? stored.images : [];
    const media: ImportedMedia[] = [];
    for (const item of items) {
      if (!isRecord(item)) continue;
      const url = urlFor(item.blob);
      if (!url) continue;
      const revived: ImportedMedia = {
        kind: item.kind === 'video' ? 'video' : 'image',
        url,
        path: String(item.path ?? ''),
        mime: String(item.mime ?? ''),
      };
      if (item.converted) revived.converted = true;
      const posterUrl = urlFor(item.poster);
      if (posterUrl) {
        revived.posterUrl = posterUrl;
        if (typeof item.posterPath === 'string') revived.posterPath = item.posterPath;
      }
      if (typeof item.bytes === 'number') revived.bytes = item.bytes;
      if (typeof item.alt === 'string') revived.alt = item.alt;
      media.push(revived);
    }
    if (media.length === 0) continue;

    const user: ImportedUser = { username: String(stored.user.username ?? 'unknown') };
    if (typeof stored.user.fullName === 'string') user.fullName = stored.user.fullName;
    if (typeof stored.user.verified === 'boolean') user.verified = stored.user.verified;
    // Stored data is ours, but links are re-checked anyway before they reach an href.
    const profileUrl = sanitizeProfileUrl(stored.user.profileUrl);
    if (profileUrl) user.profileUrl = profileUrl;
    const avatarUrl = urlFor(stored.user.avatar);
    if (avatarUrl) user.avatarUrl = avatarUrl;

    const post: ImportedPost = {
      id: String(stored.id),
      media,
      user,
      caption: typeof stored.caption === 'string' ? stored.caption : '',
      commentCount: typeof stored.commentCount === 'number' ? stored.commentCount : 0,
      likeCount: typeof stored.likeCount === 'number' ? stored.likeCount : null,
      mediaType: stored.mediaType === 'carousel' || stored.mediaType === 'video' ? stored.mediaType : 'image',
      comments: Array.isArray(stored.comments) ? stored.comments : [],
    };
    const permalink = sanitizePostUrl(stored.permalink);
    if (permalink) post.permalink = permalink;
    if (typeof stored.takenAt === 'string') post.takenAt = stored.takenAt;
    if (typeof stored.alt === 'string') post.alt = stored.alt;
    if (typeof stored.location === 'string') post.location = stored.location;
    posts.push(post);
  }
  if (posts.length === 0) return null;

  const result: ImportResult = {
    sourceName: meta.sourceName,
    sourceKind: meta.sourceKind,
    posts,
    warnings: meta.warnings,
    totalEntries: typeof meta.totalEntries === 'number' ? meta.totalEntries : posts.length,
  };
  if (typeof meta.exportedAt === 'string') result.exportedAt = meta.exportedAt;
  const sourceUrl = sanitizeSourceUrl(meta.sourceUrl);
  if (sourceUrl) result.sourceUrl = sourceUrl;
  if (Array.isArray(meta.sources)) result.sources = meta.sources;
  return result;
}

/** Load the persisted import with fresh object URLs, or null when there is none. Never throws. */
export function loadSavedImport(): Promise<ImportResult | null> {
  return enqueue(async () => {
    try {
      const stored = await transact<{ meta: unknown; blobs: Map<string, Blob> }>(
        'readonly',
        (store, resolveWith) => {
          const blobs = new Map<string, Blob>();
          const metaRequest = store.get(META_KEY);
          metaRequest.onsuccess = () => {
            const meta: unknown = metaRequest.result;
            resolveWith({ meta, blobs });
            if (!isStoredImport(meta)) return;
            for (const key of meta.blobKeys) {
              if (typeof key !== 'string') continue;
              const blobRequest = store.get(BLOB_PREFIX + key);
              blobRequest.onsuccess = () => {
                if (blobRequest.result instanceof Blob) blobs.set(key, blobRequest.result);
              };
            }
          };
        },
        { meta: undefined, blobs: new Map() },
      );
      return isStoredImport(stored.meta) ? revive(stored.meta, stored.blobs) : null;
    } catch {
      return null;
    }
  }).catch(() => null);
}
