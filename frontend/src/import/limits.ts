/**
 * Safety limits for importing an export. They defuse zip bombs and keep a hostile or broken
 * post.json from exhausting memory. Hard limits raise ImportError('too-large'); soft limits
 * produce warnings and skip the offending item.
 */

const MiB = 1024 * 1024;

/** Entries of post.json beyond this are ignored with a 'truncated' warning. */
export const MAX_POSTS = 5000;
/** Hard limit for the size of post.json. */
export const MAX_MANIFEST_BYTES = 32 * MiB;
/** A single image (decoded from the archive) larger than this is skipped with a warning. */
export const MAX_IMAGE_BYTES = 64 * MiB;
/** A single video file larger than this is skipped with a warning. */
export const MAX_VIDEO_BYTES = 1024 * MiB;
/**
 * Hard limit for the sum of all image and video bytes of one import action (several archives or
 * folders imported together share it).
 */
export const MAX_TOTAL_MEDIA_BYTES = 4096 * MiB;
/** @deprecated Use MAX_TOTAL_MEDIA_BYTES; images and videos now share one budget. */
export const MAX_TOTAL_IMAGE_BYTES = MAX_TOTAL_MEDIA_BYTES;
/** Archives and folders beyond this in one import action are ignored with a 'truncated' warning. */
export const MAX_SOURCES = 50;
/** Hard limit for the number of files in the selected folder or archive. */
export const MAX_SOURCE_ENTRIES = 100_000;
/** Carousel slides (pictures and videos) beyond this are dropped with a 'truncated' warning. */
export const MAX_IMAGES_PER_POST = 30;
/** Sample comments beyond this are silently dropped. */
export const MAX_COMMENTS_PER_POST = 200;

export const MAX_CAPTION_CHARS = 10_000;
export const MAX_COMMENT_CHARS = 2000;
export const MAX_USERNAME_CHARS = 100;
export const MAX_NAME_CHARS = 200;
export const MAX_ALT_CHARS = 2000;
export const MAX_LOCATION_CHARS = 200;
export const MAX_ID_CHARS = 100;
export const MAX_URL_CHARS = 2048;

/**
 * What one upload session + commit of the backend library accepts (backend/app/library.py
 * LibraryLimits; a unit test reads that file and fails when the two drift apart). An import that
 * exceeds any of them is saved in several parts, see prepareLibraryUploads.
 */
export interface LibraryCommitLimits {
  /** Posts one commit may list (max_posts_per_commit). */
  maxPostsPerCommit: number;
  /** Bytes of one commit body, UTF-8 JSON (max_commit_bytes). */
  maxCommitBytes: number;
  /** Files one upload session may hold (max_files_per_session). */
  maxFilesPerUpload: number;
  /** Bytes of all files of one upload session (max_session_bytes). */
  maxUploadBytes: number;
}

export const LIBRARY_COMMIT_LIMITS: Readonly<LibraryCommitLimits> = Object.freeze({
  maxPostsPerCommit: 5000,
  maxCommitBytes: 32 * MiB,
  maxFilesPerUpload: 100_000,
  maxUploadBytes: 8 * 1024 * MiB,
});

/** Images and videos are read and prepared with this many parallel workers. */
export const IMAGE_CONCURRENCY = 3;
/** HEIC/HEIF conversions running at the same time (each one holds a full RGBA bitmap). */
export const HEIC_CONCURRENCY = 2;
/** A single HEIC conversion that takes longer than this counts as failed. */
export const HEIC_TIMEOUT_MS = 120_000;
/** JPEG quality used when converting HEIC/HEIF. */
export const HEIC_JPEG_QUALITY = 0.9;

/** The limits that vary per import; overridable internally (tests), fixed for the public API. */
export interface ImportLimits {
  maxPosts: number;
  maxManifestBytes: number;
  maxImageBytes: number;
  maxVideoBytes: number;
  maxTotalMediaBytes: number;
  maxSourceEntries: number;
}

export const DEFAULT_LIMITS: Readonly<ImportLimits> = Object.freeze({
  maxPosts: MAX_POSTS,
  maxManifestBytes: MAX_MANIFEST_BYTES,
  maxImageBytes: MAX_IMAGE_BYTES,
  maxVideoBytes: MAX_VIDEO_BYTES,
  maxTotalMediaBytes: MAX_TOTAL_MEDIA_BYTES,
  maxSourceEntries: MAX_SOURCE_ENTRIES,
});

export function formatBytes(bytes: number): string {
  if (bytes >= 1024 * MiB) return `${(bytes / (1024 * MiB)).toFixed(1).replace(/\.0$/, '')} GB`;
  if (bytes >= MiB) return `${(bytes / MiB).toFixed(1).replace(/\.0$/, '')} MB`;
  if (bytes >= 1024) return `${Math.round(bytes / 1024)} KB`;
  return `${bytes} bytes`;
}
