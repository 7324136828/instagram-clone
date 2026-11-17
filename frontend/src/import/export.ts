/**
 * Writes imported posts back out as an "instagram-export" v1 archive (see /instagram.md,
 * sections 1, 7 and 8): the layout the clone imports, so a download can be imported again.
 *
 *   post.json                    the posts in canonical form, each with its caption (description)
 *   posts/post<N>.<ext>          the picture or video of a single-media post
 *   posts/post<N>_<k>.<ext>      slide k of a carousel
 *   posts/<slide>_poster.<ext>   the poster image of a video slide
 *   avatars/<username>.<ext>     profile pictures
 *
 * N is the post's position in the export (across all parts), the extension comes from the file's
 * first bytes. Pictures and videos are stored as they are (they do not shrink), post.json is
 * deflated. A large export is split into parts (<name>-part1.zip, <name>-part2.zip, ...), each a
 * complete export with its own post.json and the avatars its posts need.
 *
 * The files are read from the URLs the posts are shown from: object URLs of an import kept in the
 * browser, or the backend library's /api/library/files/... URLs (see loadExportFile).
 */
import { Zip, ZipDeflate, ZipPassThrough, strToU8 } from 'fflate';
import { MAX_POSTS } from './limits';
import { blobForUrl } from './registry';
import { SNIFF_BYTES, sniffMedia, type SniffedMedia } from './sniff';
import { ImportError, type ImportedMediaKind, type ImportedMediaType, type ImportedPost } from './types';
import { BlobCollector, describeError, readBytes } from './util';

const MiB = 1024 * 1024;
const GiB = 1024 * MiB;

/** Media bytes per archive before the export is split: well below the 4 GB a plain ZIP can address. */
export const EXPORT_PART_BYTES = 2 * GiB;
/** Posts per archive: the clone imports at most this many from one archive. */
export const EXPORT_PART_POSTS = MAX_POSTS;
/** A post larger than this cannot go into a plain ZIP at all (room left for headers and post.json). */
const MAX_POST_BYTES = 4 * GiB - 64 * MiB;
/** Bytes read from a file and handed to the archive at a time. */
const READ_CHUNK_BYTES = 4 * MiB;
const MAX_STEM_CHARS = 64;
const WINDOWS_RESERVED = /^(?:con|prn|aux|nul|com[1-9]|lpt[1-9])$/i;

const EXTENSIONS: Readonly<Record<SniffedMedia['format'], string>> = {
  jpeg: 'jpg',
  png: 'png',
  gif: 'gif',
  webp: 'webp',
  avif: 'avif',
  bmp: 'bmp',
  heic: 'heic',
  mp4: 'mp4',
  mov: 'mov',
  webm: 'webm',
};

export interface ExportProgress {
  /** Posts handled so far. */
  done: number;
  total: number;
}

export interface ExportOptions {
  /** Archive name without ".zip"; parts get "-part1", "-part2" ... Default "instagram-export". */
  name?: string;
  /** The bytes behind a media URL of the posts. Default: loadExportFile. */
  loadFile?: (url: string, signal?: AbortSignal) => Promise<Blob>;
  signal?: AbortSignal;
  onProgress?: (progress: ExportProgress) => void;
  /** Media bytes per archive (default EXPORT_PART_BYTES). */
  maxPartBytes?: number;
  /** Posts per archive (default EXPORT_PART_POSTS). */
  maxPartPosts?: number;
  /** Written as exported_at and as the date of every file. Default: now. */
  now?: Date;
}

export interface ExportArchive {
  /** File name for the download, e.g. instagram-export.zip. */
  name: string;
  blob: Blob;
  /** Posts in this archive. */
  posts: number;
}

export interface ExportResult {
  /** One archive, or several parts of a large export; empty when no post could be exported. */
  archives: ExportArchive[];
  /** Posts written, over all archives. */
  posts: number;
  /** Files or posts that were left out, one sentence each. */
  problems: string[];
}

/** post.json `media` object. */
export interface ExportMediaEntry {
  type: ImportedMediaKind;
  path: string;
  poster?: string;
  alt?: string;
}

/** post.json entry, in the canonical field order of instagram.md section 8. */
export interface ExportPostEntry {
  id: string;
  url?: string;
  media: ExportMediaEntry[];
  user: { username: string; full_name?: string; verified?: boolean; avatar?: string; profile_url?: string };
  caption: string;
  comment_count: number;
  like_count: number | null;
  taken_at?: string;
  media_type: ImportedMediaType;
  alt?: string;
  location?: string;
  comments?: Array<{ username: string; text: string; like_count: number; taken_at?: string }>;
}

export interface ExportManifest {
  format: 'instagram-export';
  version: 1;
  exported_at: string;
  source: { type: 'posts' };
  posts: ExportPostEntry[];
}

function cancelled(): ImportError {
  return new ImportError('aborted', 'The download was cancelled.');
}

const LIBRARY_FILE = /^\/api\/library\/files\//;

/**
 * The bytes behind a post's media URL: the Blob of an object URL the import holds, or a file of
 * the backend library fetched from this app's own server. Nothing else is ever fetched.
 */
export async function loadExportFile(url: string, signal?: AbortSignal): Promise<Blob> {
  const held = blobForUrl(url);
  if (held) return held;
  if (!LIBRARY_FILE.test(url)) throw new Error('the file is no longer available; import the export again');
  let response: Response;
  try {
    response = await fetch(url, { signal });
  } catch (err) {
    if (signal?.aborted) throw cancelled();
    throw new Error(`the server could not be reached (${describeError(err)})`);
  }
  if (!response.ok) throw new Error(`the server answered ${response.status}`);
  return response.blob();
}

/** "2026-11-01T15:04:05Z" (milliseconds only when there are any); undefined when unparseable. */
function isoTime(value: string | Date | undefined): string | undefined {
  if (value === undefined) return undefined;
  const ms = value instanceof Date ? value.getTime() : Date.parse(value);
  if (!Number.isFinite(ms)) return undefined;
  return new Date(ms).toISOString().replace(/\.000Z$/, 'Z');
}

/** A file name stem for a username: the characters Instagram allows, unique without letter case. */
function avatarStem(username: string, taken: Set<string>): string {
  let stem = username.replace(/[^A-Za-z0-9._-]+/g, '_').replace(/^\.+|\.+$/g, '').slice(0, MAX_STEM_CHARS) || 'user';
  if (WINDOWS_RESERVED.test(stem)) stem = `${stem}_`;
  let candidate = stem;
  for (let n = 2; taken.has(candidate.toLowerCase()); n++) candidate = `${stem}-${n}`;
  taken.add(candidate.toLowerCase());
  return candidate;
}

interface LoadedFile {
  blob: Blob;
  kind: ImportedMediaKind;
  extension: string;
}

interface LoadedSlide {
  file: LoadedFile;
  poster?: LoadedFile;
  alt?: string;
}

/** One archive being written. */
class PartWriter {
  readonly entries: ExportPostEntry[] = [];
  mediaBytes = 0;
  /** Username -> avatar path in this part. */
  readonly avatars = new Map<string, string>();
  readonly avatarStems = new Set<string>();
  private readonly collector = new BlobCollector();
  private readonly zip: Zip;
  private failure: Error | null = null;
  private complete = false;

  constructor(private readonly mtime: Date) {
    this.zip = new Zip((err, chunk, final) => {
      if (err) {
        this.failure = err;
        return;
      }
      this.collector.add(chunk);
      if (final) this.complete = true;
    });
  }

  private check(): void {
    if (this.failure) throw new ImportError('read-failed', `The archive could not be written: ${describeError(this.failure)}`);
  }

  async addFile(path: string, blob: Blob, signal: AbortSignal | undefined): Promise<void> {
    const file = new ZipPassThrough(path);
    file.mtime = this.mtime;
    this.zip.add(file);
    if (blob.size === 0) file.push(new Uint8Array(0), true);
    for (let start = 0; start < blob.size; start += READ_CHUNK_BYTES) {
      if (signal?.aborted) throw cancelled();
      const end = Math.min(blob.size, start + READ_CHUNK_BYTES);
      file.push(await readBytes(blob, start, end), end === blob.size);
      this.check();
    }
    this.check();
    this.mediaBytes += blob.size;
  }

  finish(manifest: ExportManifest): Blob {
    const file = new ZipDeflate('post.json', { level: 6 });
    file.mtime = this.mtime;
    this.zip.add(file);
    file.push(strToU8(`${JSON.stringify(manifest, null, 2)}\n`), true);
    this.zip.end();
    this.check();
    if (!this.complete) throw new ImportError('read-failed', 'The archive could not be completed.');
    return new Blob([this.collector.finish()], { type: 'application/zip' });
  }
}

/**
 * Builds the archive(s) of `posts`, in their order, with their current captions. A file that
 * cannot be read or is not a picture or video is left out (a video falls back to its poster); a
 * post with nothing left is skipped. Both are listed in `problems`. Rejects with
 * ImportError('aborted') when `signal` aborts, and with ImportError('read-failed') when an
 * archive cannot be written.
 */
export async function exportPosts(posts: readonly ImportedPost[], options: ExportOptions = {}): Promise<ExportResult> {
  const {
    name = 'instagram-export',
    loadFile = loadExportFile,
    signal,
    onProgress,
    maxPartBytes = EXPORT_PART_BYTES,
    maxPartPosts = EXPORT_PART_POSTS,
  } = options;
  const now = options.now ?? new Date();
  const exportedAt = isoTime(now) ?? now.toISOString();
  const problems: string[] = [];
  const blobs: Blob[] = [];
  const counts: number[] = [];
  const usedIds = new Set<string>();
  /** Avatars are read once, whatever part they go into. */
  const avatarCache = new Map<string, LoadedFile | null>();
  let part = new PartWriter(now);
  let number = 0;

  const throwIfCancelled = () => {
    if (signal?.aborted) throw cancelled();
  };

  const load = async (url: string, what: string): Promise<LoadedFile> => {
    let blob: Blob;
    try {
      blob = await loadFile(url, signal);
    } catch (err) {
      throwIfCancelled();
      throw new Error(`${what} could not be read: ${describeError(err).replace(/\.$/, '')}.`);
    }
    throwIfCancelled();
    const sniffed = sniffMedia(await readBytes(blob, 0, Math.min(blob.size, SNIFF_BYTES)));
    if (!sniffed) throw new Error(`${what} is not a picture or video.`);
    return { blob, kind: sniffed.kind, extension: EXTENSIONS[sniffed.format] };
  };

  const loadSlides = async (post: ImportedPost, label: string): Promise<LoadedSlide[]> => {
    const slides: LoadedSlide[] = [];
    for (const item of post.media) {
      const what = `"${item.path}"`;
      const posterWhat = `The poster of "${item.path}"`;
      let file: LoadedFile | undefined;
      let poster: LoadedFile | undefined;
      try {
        file = await load(item.url, what);
      } catch (err) {
        throwIfCancelled();
        problems.push(`${label}: ${describeError(err)}`);
      }
      if (item.posterUrl && (!file || file.kind === 'video')) {
        try {
          poster = await load(item.posterUrl, posterWhat);
          if (poster.kind !== 'image') {
            problems.push(`${label}: ${posterWhat} is not a picture.`);
            poster = undefined;
          }
        } catch (err) {
          throwIfCancelled();
          problems.push(`${label}: ${describeError(err)}`);
        }
      }
      if (file) slides.push({ file, poster, alt: item.alt });
      else if (poster) {
        // The video is gone: its poster stands in for it, as instagram.md section 7 describes.
        problems.push(`${label}: only the poster of "${item.path}" was saved.`);
        slides.push({ file: poster, alt: item.alt });
      }
    }
    return slides;
  };

  const loadAvatar = async (url: string, label: string): Promise<LoadedFile | null> => {
    const known = avatarCache.get(url);
    if (known !== undefined) return known;
    let avatar: LoadedFile | null = null;
    try {
      const loaded = await load(url, 'The profile picture');
      if (loaded.kind === 'image') avatar = loaded;
      else problems.push(`${label}: the profile picture is not a picture.`);
    } catch (err) {
      throwIfCancelled();
      problems.push(`${label}: ${describeError(err)}`);
    }
    avatarCache.set(url, avatar);
    return avatar;
  };

  const closePart = () => {
    const manifest: ExportManifest = {
      format: 'instagram-export',
      version: 1,
      exported_at: exportedAt,
      source: { type: 'posts' },
      posts: part.entries,
    };
    blobs.push(part.finish(manifest));
    counts.push(part.entries.length);
  };

  onProgress?.({ done: 0, total: posts.length });
  for (const [index, post] of posts.entries()) {
    throwIfCancelled();
    const label = `Post ${index + 1} (${post.id})`;
    const slides = await loadSlides(post, label);
    throwIfCancelled();
    if (slides.length === 0) {
      problems.push(`${label} was left out: none of its pictures or videos could be saved.`);
      onProgress?.({ done: index + 1, total: posts.length });
      continue;
    }
    const avatar = post.user.avatarUrl ? await loadAvatar(post.user.avatarUrl, label) : null;
    throwIfCancelled();
    let bytes = 0;
    for (const slide of slides) bytes += slide.file.blob.size + (slide.poster?.blob.size ?? 0);
    if (avatar) bytes += avatar.blob.size;
    if (bytes > MAX_POST_BYTES) {
      problems.push(`${label} was left out: its files are too large for one archive.`);
      onProgress?.({ done: index + 1, total: posts.length });
      continue;
    }
    if (part.entries.length > 0 && (part.entries.length >= maxPartPosts || part.mediaBytes + bytes > maxPartBytes)) {
      closePart();
      part = new PartWriter(now);
    }

    number += 1;
    const media: ExportMediaEntry[] = [];
    for (const [slideIndex, slide] of slides.entries()) {
      const base = slides.length > 1 ? `posts/post${number}_${slideIndex + 1}` : `posts/post${number}`;
      const path = `${base}.${slide.file.extension}`;
      await part.addFile(path, slide.file.blob, signal);
      const entry: ExportMediaEntry = { type: slide.file.kind, path };
      if (slide.poster) {
        entry.poster = `${base}_poster.${slide.poster.extension}`;
        await part.addFile(entry.poster, slide.poster.blob, signal);
      }
      if (slide.alt && slide.alt !== post.alt) entry.alt = slide.alt;
      media.push(entry);
    }

    const { username, fullName, verified, profileUrl } = post.user;
    const user: ExportPostEntry['user'] = { username };
    if (fullName) user.full_name = fullName;
    if (verified !== undefined) user.verified = verified;
    if (avatar) {
      let path = part.avatars.get(username);
      if (!path) {
        path = `avatars/${avatarStem(username, part.avatarStems)}.${avatar.extension}`;
        await part.addFile(path, avatar.blob, signal);
        part.avatars.set(username, path);
      }
      user.avatar = path;
    }
    if (profileUrl) user.profile_url = profileUrl;

    let id = post.id;
    for (let n = 2; usedIds.has(id); n++) id = `${post.id}-${n}`;
    usedIds.add(id);

    const takenAt = isoTime(post.takenAt);
    const comments = post.comments.map((comment) => {
      const out: NonNullable<ExportPostEntry['comments']>[number] = {
        username: comment.username,
        text: comment.text,
        like_count: Number.isFinite(comment.likes) && comment.likes > 0 ? Math.round(comment.likes) : 0,
      };
      const at = isoTime(comment.takenAt);
      if (at) out.taken_at = at;
      return out;
    });
    // Field order as in instagram.md section 8.
    const entry: ExportPostEntry = {
      id,
      ...(post.permalink ? { url: post.permalink } : {}),
      media,
      user,
      caption: post.caption,
      comment_count: Number.isFinite(post.commentCount) && post.commentCount > 0 ? Math.round(post.commentCount) : 0,
      like_count: typeof post.likeCount === 'number' && Number.isFinite(post.likeCount) && post.likeCount >= 0
        ? Math.round(post.likeCount)
        : null,
      ...(takenAt ? { taken_at: takenAt } : {}),
      media_type: media.length > 1
        ? 'carousel'
        : media[0].type === 'video' || post.mediaType === 'video' ? 'video' : 'image',
      ...(post.alt ? { alt: post.alt } : {}),
      ...(post.location ? { location: post.location } : {}),
      ...(comments.length > 0 ? { comments } : {}),
    };
    part.entries.push(entry);
    onProgress?.({ done: index + 1, total: posts.length });
  }

  if (part.entries.length > 0) closePart();
  const archives = blobs.map((blob, index): ExportArchive => ({
    name: blobs.length > 1 ? `${name}-part${index + 1}.zip` : `${name}.zip`,
    blob,
    posts: counts[index],
  }));
  return { archives, posts: number, problems };
}
