/**
 * Contract for importing an Instagram export (see /instagram.md for the on-disk format).
 *
 *   post.json
 *   posts/
 *     post1.jpg, post2.png, post3.webp, post4.heic, post5.mp4, post6.mov, ...
 *
 * The export is supplied either as an archive file (.zip, .tar, .tar.gz/.tgz) or as a
 * selected/dropped folder. Everything is processed in the browser; nothing is uploaded.
 */

/**
 * archive / folder: one source. multiple: several archives or folders merged in one import action.
 * library: the merged library kept by the backend in %TEMP%/instagram_cache.
 */
export type ImportSourceKind = 'archive' | 'folder' | 'multiple' | 'library';

export type ImportedMediaKind = 'image' | 'video';

export interface ImportedMedia {
  kind: ImportedMediaKind;
  /**
   * URL the browser can render: an object URL owned by the import (HEIC is converted), or a server URL
   * (/api/library/files/...) for a library result.
   */
  url: string;
  /** Path as written in post.json, normalised to forward slashes. */
  path: string;
  /** MIME type of the content behind `url` (after any conversion), e.g. image/jpeg, video/mp4. */
  mime: string;
  /** True when the original file was HEIC/HEIF and was converted for display. */
  converted?: boolean;
  /** Video only: poster image URL, when the export ships one. */
  posterUrl?: string;
  /** Video only: poster path as written in post.json. */
  posterPath?: string;
  /** Size in bytes of the content behind `url`, when known. */
  bytes?: number;
  /** Accessibility text of this one picture or video (a `media` object's "alt"), when given. */
  alt?: string;
}

/** @deprecated Use ImportedMedia. */
export type ImportedImage = ImportedMedia;

export interface ImportedUser {
  username: string;
  fullName?: string;
  verified?: boolean;
  /** Object URL for the profile picture, when the export ships one. */
  avatarUrl?: string;
  /** Sanitised https://www.instagram.com/<username>/ link. */
  profileUrl?: string;
}

export interface ImportedComment {
  id: string;
  username: string;
  text: string;
  likes: number;
  /** ISO 8601 timestamp when known. */
  takenAt?: string;
}

export type ImportedMediaType = 'image' | 'carousel' | 'video';

export interface ImportedPost {
  /** Unique within one import (shortcode when available, otherwise derived). */
  id: string;
  /** Sanitised https link to the post on instagram.com; undefined when absent or unsafe. */
  permalink?: string;
  /** At least one picture or video; the first one is the cover. */
  media: ImportedMedia[];
  user: ImportedUser;
  /** The post owner's first comment, i.e. the post description. May be empty. */
  caption: string;
  /** Total number of comments on the original post. */
  commentCount: number;
  /** Null when the export does not know it (e.g. the owner hides like counts). */
  likeCount: number | null;
  /** ISO 8601 timestamp when known. */
  takenAt?: string;
  mediaType: ImportedMediaType;
  alt?: string;
  location?: string;
  /** Optional sample of comments other than the caption. */
  comments: ImportedComment[];
}

export type ImportWarningCode =
  | 'missing-image'
  | 'unsupported-image'
  | 'unsupported-media'
  | 'conversion-failed'
  | 'invalid-entry'
  | 'unsafe-url'
  | 'duplicate-id'
  | 'truncated'
  | 'merged-duplicate';

export interface ImportWarning {
  code: ImportWarningCode;
  message: string;
  /** Zero-based index of the entry in post.json, when the warning is about one entry. */
  postIndex?: number;
  path?: string;
}

/** One archive or folder that contributed to a (merged) result. */
export interface ImportSourceSummary {
  name: string;
  /** 'multiple': one earlier import action of a library that itself merged several sources. */
  kind: 'archive' | 'folder' | 'multiple';
  /** Posts this source contributed before merging. */
  posts: number;
  warnings: number;
  /** Set when the source could not be imported at all. */
  error?: string;
}

export interface ImportResult {
  /** File or folder name the user picked ("3 archives" style text for a merged result). */
  sourceName: string;
  sourceKind: ImportSourceKind;
  posts: ImportedPost[];
  warnings: ImportWarning[];
  /** Number of entries listed in post.json (posts.length can be lower when entries were skipped). */
  totalEntries: number;
  /** ISO 8601 timestamp from the export metadata, when present. */
  exportedAt?: string;
  /** Sanitised URL of the Instagram page the export was taken from, when present. */
  sourceUrl?: string;
  /** The archives/folders behind a merged ('multiple') or library result. */
  sources?: ImportSourceSummary[];
}

export type ImportPhase = 'reading' | 'manifest' | 'images' | 'uploading' | 'saving' | 'done';

export interface ImportProgress {
  phase: ImportPhase;
  done: number;
  total: number;
  /** Short human readable status, e.g. "Converting image 3 of 12". */
  message: string;
}

export interface ImportOptions {
  onProgress?: (progress: ImportProgress) => void;
  signal?: AbortSignal;
}

export type ImportErrorCode =
  | 'unsupported-archive'
  | 'read-failed'
  | 'no-manifest'
  | 'invalid-json'
  | 'invalid-manifest'
  | 'no-posts'
  | 'too-large'
  | 'aborted';

/** Thrown when nothing usable could be imported. `message` is safe to show to the user. */
export class ImportError extends Error {
  readonly code: ImportErrorCode;

  constructor(code: ImportErrorCode, message: string) {
    super(message);
    this.name = 'ImportError';
    this.code = code;
  }
}
