/**
 * The backend library (%TEMP%/instagram_cache): the JSON it serves and accepts, turning a library
 * into an ImportResult, and turning a parsed import into an upload (staged files + commit body).
 * Nothing here talks to the network; the caller does the requests.
 */
import { finalMediaType } from './core';
import { convertHeic } from './heic';
import { LIBRARY_COMMIT_LIMITS, MAX_POSTS, MAX_SOURCES, formatBytes, type LibraryCommitLimits } from './limits';
import { parseManifest, parseTimestamp } from './manifest';
import { blobForUrl } from './registry';
import { SNIFF_BYTES, sniffImage } from './sniff';
import {
  ImportError,
  type ImportResult,
  type ImportSourceSummary,
  type ImportWarning,
  type ImportedMedia,
  type ImportedMediaKind,
  type ImportedMediaType,
  type ImportedPost,
  type ImportedUser,
} from './types';
import { describeError, readBytes } from './util';

/** One earlier import action recorded by the library. */
export interface LibraryImport {
  id: string;
  source_name: string;
  source_kind: 'archive' | 'folder' | 'multiple';
  imported_at: string;
  /** Posts the import contained, of which `new` were added and `updated` replaced older ones. */
  posts: number;
  new: number;
  updated: number;
}

/** A picture or video of a post entry (format v1 `media` object). */
export interface LibraryMediaEntry {
  type: ImportedMediaKind;
  path: string;
  poster?: string;
  alt?: string;
}

/** A post entry as the commit sends it; media paths are staged names (m000001.jpg). */
export interface LibraryCommitPost {
  id: string;
  url?: string;
  media_type: ImportedMediaType;
  media: LibraryMediaEntry[];
  user: { username: string; full_name?: string; verified?: boolean; profile_url?: string };
  caption: string;
  comment_count: number;
  like_count: number | null;
  taken_at?: string;
  alt?: string;
  location?: string;
  comments: Array<{ username: string; text: string; like_count: number; taken_at?: string }>;
}

/**
 * A post entry as the library returns it: paths relative to the cache folder, `image`/`images`
 * mirrors of `media` for older readers, and the avatar path on the user.
 */
export interface LibraryPostEntry extends Omit<LibraryCommitPost, 'user'> {
  image?: string;
  images?: string[];
  user: LibraryCommitPost['user'] & { avatar?: string };
}

/** GET /api/library (and the answer to a commit or a post deletion). */
export interface LibraryResponse {
  /** The cache folder on the server, for display. */
  directory: string;
  /** Bumped by every change; 0 for an empty library. */
  revision: number;
  updated_at: string | null;
  imports: LibraryImport[];
  posts: LibraryPostEntry[];
}

/** POST /api/library/uploads/{upload_id}/commit */
export interface LibraryCommitBody {
  source_name: string;
  source_kind: 'archive' | 'folder' | 'multiple';
  exported_at?: string;
  source_url?: string;
  posts: LibraryCommitPost[];
  /** Username -> staged name of the profile picture. */
  avatars?: Record<string, string>;
}

/** One file to PUT to /api/library/uploads/{upload_id}/files/{name}. */
export interface LibraryUploadFile {
  /** Staged name: "m" + six digits + extension, e.g. m000001.jpg. */
  name: string;
  blob: Blob;
  mime: string;
}

export interface LibraryUploadPlan {
  files: LibraryUploadFile[];
  commit: LibraryCommitBody;
}

/** The library may hold many imports; each of them was capped at MAX_POSTS. */
const LIBRARY_MAX_POSTS = MAX_POSTS * MAX_SOURCES;

/** File types the library stores, by extension (the server names files after their content). */
const TYPES_BY_EXTENSION: Readonly<Record<string, { kind: ImportedMediaKind; mime: string }>> = {
  jpg: { kind: 'image', mime: 'image/jpeg' },
  jpeg: { kind: 'image', mime: 'image/jpeg' },
  png: { kind: 'image', mime: 'image/png' },
  gif: { kind: 'image', mime: 'image/gif' },
  webp: { kind: 'image', mime: 'image/webp' },
  avif: { kind: 'image', mime: 'image/avif' },
  bmp: { kind: 'image', mime: 'image/bmp' },
  mp4: { kind: 'video', mime: 'video/mp4' },
  m4v: { kind: 'video', mime: 'video/mp4' },
  mov: { kind: 'video', mime: 'video/quicktime' },
  webm: { kind: 'video', mime: 'video/webm' },
};

const EXTENSIONS_BY_MIME: Readonly<Record<string, string>> = {
  'image/jpeg': 'jpg',
  'image/png': 'png',
  'image/gif': 'gif',
  'image/webp': 'webp',
  'image/avif': 'avif',
  'image/bmp': 'bmp',
  'video/mp4': 'mp4',
  'video/quicktime': 'mov',
  'video/webm': 'webm',
};

function typeOfPath(path: string): { kind: ImportedMediaKind; mime: string } | undefined {
  const dot = path.lastIndexOf('.');
  return dot < 0 ? undefined : TYPES_BY_EXTENSION[path.slice(dot + 1).toLowerCase()];
}

/**
 * URL of a library file: /api/library/files/ + each path segment encoded + ?r=<revision>
 * (the revision makes a replaced file a new URL for the browser cache).
 */
export function libraryFileUrl(path: string, revision: number): string {
  const encoded = path.split('/').map(encodeURIComponent).join('/');
  return `/api/library/files/${encoded}?r=${encodeURIComponent(String(revision))}`;
}

function librarySources(imports: unknown): ImportSourceSummary[] {
  if (!Array.isArray(imports)) return [];
  const sources: ImportSourceSummary[] = [];
  for (const item of imports as Array<Partial<LibraryImport> | null>) {
    if (typeof item !== 'object' || item === null || typeof item.source_name !== 'string') continue;
    const kind = item.source_kind === 'archive' || item.source_kind === 'folder' ? item.source_kind : 'multiple';
    const posts = typeof item.posts === 'number' && Number.isFinite(item.posts) ? item.posts : 0;
    sources.push({ name: item.source_name, kind, posts, warnings: 0 });
  }
  return sources;
}

/**
 * Turns the library JSON into an ImportResult whose media URLs are server URLs (`fileUrl(path)`,
 * usually `(path) => libraryFileUrl(path, library.revision)`); nothing is downloaded. The posts
 * go through the same lenient parser and link sanitisers as an export's post.json. The kind of a
 * file comes from its extension, which the server derives from the file's content. An empty
 * library gives a result without posts. `sourceKind` is 'library' and `sources` lists the
 * library's imports.
 */
export function resultFromLibrary(library: LibraryResponse, fileUrl: (path: string) => string): ImportResult {
  const entries: unknown = library?.posts;
  const result: ImportResult = {
    sourceName: 'Library',
    sourceKind: 'library',
    posts: [],
    warnings: [],
    totalEntries: Array.isArray(entries) ? entries.length : 0,
    sources: librarySources(library?.imports),
  };
  const updatedAt = parseTimestamp(library?.updated_at);
  if (updatedAt) result.exportedAt = updatedAt;
  if (!Array.isArray(entries) || entries.length === 0) return result;

  const manifest = parseManifest({ posts: entries }, LIBRARY_MAX_POSTS);
  const warnings: ImportWarning[] = [...manifest.warnings];
  for (const source of manifest.posts) {
    const media: ImportedMedia[] = [];
    for (const item of source.media) {
      const type = typeOfPath(item.path);
      if (!type) {
        warnings.push({
          code: 'unsupported-media',
          message: `Post ${source.index + 1}: "${item.path}" is not a picture or video the library can show.`,
          postIndex: source.index,
          path: item.path,
        });
        continue;
      }
      const loaded: ImportedMedia = { kind: type.kind, url: fileUrl(item.path), path: item.path, mime: type.mime };
      if (item.alt) loaded.alt = item.alt;
      if (type.kind === 'video' && item.posterPath && typeOfPath(item.posterPath)?.kind === 'image') {
        loaded.posterUrl = fileUrl(item.posterPath);
        loaded.posterPath = item.posterPath;
      }
      media.push(loaded);
    }
    if (media.length === 0) continue;

    const user: ImportedUser = { username: source.user.username };
    if (source.user.fullName) user.fullName = source.user.fullName;
    if (source.user.verified !== undefined) user.verified = source.user.verified;
    if (source.user.profileUrl) user.profileUrl = source.user.profileUrl;
    const avatarPath = source.user.avatarPath;
    if (avatarPath && typeOfPath(avatarPath)?.kind === 'image') user.avatarUrl = fileUrl(avatarPath);

    const post: ImportedPost = {
      id: source.id,
      media,
      user,
      caption: source.caption,
      commentCount: source.commentCount,
      likeCount: source.likeCount,
      mediaType: finalMediaType(source.mediaType, media),
      comments: source.comments,
    };
    if (source.permalink) post.permalink = source.permalink;
    if (source.takenAt) post.takenAt = source.takenAt;
    if (source.alt) post.alt = source.alt;
    if (source.location) post.location = source.location;
    result.posts.push(post);
  }
  result.warnings = warnings;
  return result;
}

const HEIC_MIMES = new Set(['image/heic', 'image/heif']);

/**
 * The bytes to upload for one object URL. HEIC/HEIF pictures that the browser showed natively
 * (Safari) are converted to JPEG, because the library only stores widely supported formats.
 */
async function uploadable(blob: Blob, mime: string, what: string): Promise<{ blob: Blob; mime: string }> {
  if (!HEIC_MIMES.has(mime)) return { blob, mime };
  let converted: Blob;
  try {
    converted = await convertHeic(blob);
  } catch (err) {
    throw new ImportError('read-failed', `${what} is a HEIC/HEIF image that could not be converted: ${describeError(err)}`);
  }
  const sniffed = sniffImage(await readBytes(converted, 0, Math.min(converted.size, SNIFF_BYTES)));
  if (!sniffed || sniffed.format === 'heic') {
    throw new ImportError('read-failed', `${what} is a HEIC/HEIF image that could not be converted.`);
  }
  return { blob: converted, mime: sniffed.mime };
}

/** A file of the import as it will be uploaded, with the extension of its staged name. */
interface UploadableFile {
  blob: Blob;
  mime: string;
  extension: string;
}

/** A file a post needs: the object URL it is shown from and the upload made of it. */
interface FileRef {
  url: string;
  upload: UploadableFile;
}

/** One part being filled: its staged files, posts and avatars, and their cost against the limits. */
interface UploadPart {
  files: LibraryUploadFile[];
  /** Object URL -> staged name, so a file several posts of the part show is uploaded once. */
  staged: Map<string, string>;
  posts: LibraryCommitPost[];
  avatars: Map<string, string>;
  fileBytes: number;
  /** Upper bound of the UTF-8 size of the commit body (envelope, entries and separators). */
  commitBytes: number;
}

const utf8 = new TextEncoder();
/** UTF-8 bytes of a value's JSON (JSON.stringify escapes lone surrogates, so this is exact). */
function jsonBytes(value: unknown): number {
  return utf8.encode(JSON.stringify(value)).length;
}

/** Room left in the commit envelope for the " (part 12 of 34)" suffix of a split import. */
const PART_SUFFIX_RESERVE = 64;
/** The library keeps this many characters of a source name (MAX_SOURCE_NAME_CHARS). */
const MAX_SOURCE_NAME_CHARS = 255;

function stagedName(index: number, extension: string): string {
  return `m${String(index).padStart(6, '0')}.${extension}`;
}

function commitEntry(post: ImportedPost, media: LibraryMediaEntry[]): LibraryCommitPost {
  const { username, fullName, verified, profileUrl } = post.user;
  const user: LibraryCommitPost['user'] = { username };
  if (fullName !== undefined) user.full_name = fullName;
  if (verified !== undefined) user.verified = verified;
  if (profileUrl !== undefined) user.profile_url = profileUrl;

  const entry: LibraryCommitPost = {
    id: post.id,
    media_type: post.mediaType,
    media,
    user,
    caption: post.caption,
    comment_count: post.commentCount,
    like_count: post.likeCount,
    comments: post.comments.map((comment) => {
      const out: LibraryCommitPost['comments'][number] = {
        username: comment.username,
        text: comment.text,
        like_count: comment.likes,
      };
      if (comment.takenAt !== undefined) out.taken_at = comment.takenAt;
      return out;
    }),
  };
  if (post.permalink !== undefined) entry.url = post.permalink;
  if (post.takenAt !== undefined) entry.taken_at = post.takenAt;
  if (post.alt !== undefined) entry.alt = post.alt;
  if (post.location !== undefined) entry.location = post.location;
  return entry;
}

function partSourceName(name: string, part: number, parts: number): string {
  if (parts === 1) return name;
  const suffix = ` (part ${part} of ${parts})`;
  const room = Math.max(0, MAX_SOURCE_NAME_CHARS - Array.from(suffix).length);
  return `${Array.from(name).slice(0, room).join('')}${suffix}`;
}

/**
 * Prepares saving a parsed (browser) import in the library, as one or more parts that each fit
 * one upload session + commit of the server (`limits`, see LIBRARY_COMMIT_LIMITS: posts per
 * commit, commit body bytes, files and file bytes per session). The parts are uploaded and
 * committed one after the other; most imports need a single part.
 *
 * In a part, every distinct picture, video, poster and avatar becomes one file with a staged
 * name (m000001.<ext>, the extension from its MIME type), and the commit body lists the posts
 * with `media` objects that reference those names (plus `avatars`: username -> staged name). A
 * file shown by posts of several parts is uploaded with each of them; a profile picture is sent
 * once, with the first post of its user (the library gives it to that user's later posts too).
 * Several parts are named "<source> (part 1 of 2)" and so on. The posts keep their order.
 *
 * Everything is checked before the first byte is uploaded. The result must still be live (not
 * released). Throws ImportError('read-failed') when a file is gone or cannot be stored, and
 * ImportError('too-large') when a single post does not fit into an upload on its own.
 */
export async function prepareLibraryUploads(
  result: ImportResult,
  limits: Readonly<LibraryCommitLimits> = LIBRARY_COMMIT_LIMITS,
): Promise<LibraryUploadPlan[]> {
  const sourceKind = result.sourceKind;
  if (sourceKind === 'library') {
    throw new ImportError('read-failed', 'These posts are already in the library.');
  }

  // Each object URL is read (and a HEIC picture converted) once, whatever part it goes into.
  const prepared = new Map<string, UploadableFile>();
  const prepare = async (url: string, mime: string, what: string): Promise<FileRef> => {
    const known = prepared.get(url);
    if (known) return { url, upload: known };
    const blob = blobForUrl(url);
    if (!blob) {
      throw new ImportError('read-failed', `${what} is no longer available. Import the export again.`);
    }
    const converted = await uploadable(blob, blob.type || mime, what);
    const extension = EXTENSIONS_BY_MIME[converted.mime];
    if (!extension) {
      throw new ImportError('read-failed', `${what} has a type the library does not store (${converted.mime || 'unknown'}).`);
    }
    const upload: UploadableFile = { ...converted, extension };
    prepared.set(url, upload);
    return { url, upload };
  };

  const envelope: LibraryCommitBody = {
    source_name: result.sourceName,
    source_kind: sourceKind,
    posts: [],
    avatars: {},
  };
  if (result.exportedAt !== undefined) envelope.exported_at = result.exportedAt;
  if (result.sourceUrl !== undefined) envelope.source_url = result.sourceUrl;
  const envelopeBytes = jsonBytes(envelope) + PART_SUFFIX_RESERVE;
  const newPart = (): UploadPart => ({
    files: [], staged: new Map(), posts: [], avatars: new Map(), fileBytes: 0, commitBytes: envelopeBytes,
  });

  const parts: UploadPart[] = [];
  let part = newPart();
  /** Users whose profile picture is in this part or an earlier one. */
  const avatarsSent = new Set<string>();

  for (const post of result.posts) {
    // Everything the post needs, in staging order: each slide, then its poster; the avatar last.
    const media: Array<{ item: ImportedMedia; file: FileRef; poster?: FileRef }> = [];
    for (const item of post.media) {
      const file = await prepare(item.url, item.mime, `"${item.path}"`);
      const poster = item.kind === 'video' && item.posterUrl
        ? await prepare(item.posterUrl, '', `The poster "${item.posterPath ?? item.path}"`)
        : undefined;
      media.push({ item, file, poster });
    }
    const username = post.user.username;
    const avatar = post.user.avatarUrl && !avatarsSent.has(username)
      ? await prepare(post.user.avatarUrl, '', `The profile picture of ${username}`)
      : undefined;

    // The post as it would go into `target`: the files it adds and what the part would cost then.
    const layout = (target: UploadPart) => {
      const added = new Map<string, { name: string; upload: UploadableFile }>();
      const nameOf = (ref: FileRef): string => {
        const known = target.staged.get(ref.url) ?? added.get(ref.url)?.name;
        if (known) return known;
        const name = stagedName(target.files.length + added.size + 1, ref.upload.extension);
        added.set(ref.url, { name, upload: ref.upload });
        return name;
      };
      const entries = media.map(({ item, file, poster }) => {
        const entry: LibraryMediaEntry = { type: item.kind, path: nameOf(file) };
        if (poster) entry.poster = nameOf(poster);
        if (item.alt) entry.alt = item.alt;
        return entry;
      });
      const avatarName = avatar ? nameOf(avatar) : undefined;
      const entry = commitEntry(post, entries);
      let addedBytes = 0;
      for (const file of added.values()) addedBytes += file.upload.blob.size;
      let commitBytes = jsonBytes(entry) + 1;
      if (avatarName) commitBytes += utf8.encode(`${JSON.stringify(username)}:${JSON.stringify(avatarName)}`).length + 1;
      const fits = target.posts.length + 1 <= limits.maxPostsPerCommit
        && target.files.length + added.size <= limits.maxFilesPerUpload
        && target.fileBytes + addedBytes <= limits.maxUploadBytes
        && target.commitBytes + commitBytes <= limits.maxCommitBytes;
      return { entry, added, addedBytes, commitBytes, avatarName, fits };
    };

    let placed = layout(part);
    if (!placed.fits && part.posts.length > 0) {
      parts.push(part);
      part = newPart();
      placed = layout(part);
    }
    if (!placed.fits) {
      throw new ImportError(
        'too-large',
        `Post ${post.id} is too large to save in the library (${placed.added.size} files, `
          + `${formatBytes(placed.addedBytes)} of media and ${formatBytes(placed.commitBytes)} of post data; `
          + `one upload holds at most ${limits.maxFilesPerUpload} files, ${formatBytes(limits.maxUploadBytes)} `
          + `and ${formatBytes(limits.maxCommitBytes)} of post data).`,
      );
    }
    for (const [url, { name, upload }] of placed.added) {
      part.files.push({ name, blob: upload.blob, mime: upload.mime });
      part.staged.set(url, name);
    }
    part.fileBytes += placed.addedBytes;
    part.commitBytes += placed.commitBytes;
    part.posts.push(placed.entry);
    if (placed.avatarName) {
      part.avatars.set(username, placed.avatarName);
      avatarsSent.add(username);
    }
  }
  parts.push(part);

  return parts.map((filled, index) => {
    const commit: LibraryCommitBody = {
      source_name: partSourceName(result.sourceName, index + 1, parts.length),
      source_kind: sourceKind,
      posts: filled.posts,
    };
    if (filled.avatars.size > 0) commit.avatars = Object.fromEntries(filled.avatars);
    if (result.exportedAt !== undefined) commit.exported_at = result.exportedAt;
    if (result.sourceUrl !== undefined) commit.source_url = result.sourceUrl;
    return { files: filled.files, commit };
  });
}

/**
 * Prepares saving a parsed import in the library in a single upload (see prepareLibraryUploads
 * for the plan). Throws ImportError('too-large') before anything is uploaded when the import
 * needs several uploads; prepareLibraryUploads splits it instead.
 */
export async function prepareLibraryUpload(
  result: ImportResult,
  limits: Readonly<LibraryCommitLimits> = LIBRARY_COMMIT_LIMITS,
): Promise<LibraryUploadPlan> {
  const plans = await prepareLibraryUploads(result, limits);
  if (plans.length > 1) {
    throw new ImportError(
      'too-large',
      `This import is too large to save in the library in one upload (${result.posts.length} posts; `
        + `one upload holds at most ${limits.maxPostsPerCommit} posts, ${formatBytes(limits.maxCommitBytes)} `
        + `of post data, ${limits.maxFilesPerUpload} files and ${formatBytes(limits.maxUploadBytes)}).`,
    );
  }
  return plans[0];
}
