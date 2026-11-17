/**
 * Adapter between the importer (./import) and the UI model (./data).
 *
 * The importer hands over an ImportResult whose pictures and videos are object URLs (or library
 * file URLs); this module turns it into the Post[] the feed renders and derives the per-user views (directory, stories).
 */
import {
  isInstagramUrl, sanitizePostUrl, sanitizeProfileUrl, sanitizeSourceUrl, type ImportResult, type ImportedMedia, type ImportedPost,
} from './import';
import { PHOTO_KEYS, type Comment, type Photo, type PhotoKey, type Post, type Story } from './data';

/** A user that appears as the author of at least one imported post. */
export interface DirectoryUser {
  username: string;
  fullName?: string;
  verified?: boolean;
  avatarSrc?: string;
  profileUrl?: string;
  /** Placeholder gradient used behind / instead of the profile picture. */
  avatar: PhotoKey;
  postCount: number;
}

export type UserDirectory = ReadonlyMap<string, DirectoryUser>;

/** Deterministic placeholder gradient for a username (FNV-1a). */
export function placeholderKey(username: string): PhotoKey {
  let hash = 0x811c9dc5;
  for (let i = 0; i < username.length; i++) {
    hash ^= username.charCodeAt(i);
    hash = Math.imul(hash, 0x01000193);
  }
  return PHOTO_KEYS[(hash >>> 0) % PHOTO_KEYS.length];
}

function parseTime(iso: string | undefined): number | null {
  if (!iso) return null;
  const ms = Date.parse(iso);
  return Number.isFinite(ms) ? ms : null;
}

/** Instagram-style age: "now", "35m", "7h", "2d", "3w". Empty when the timestamp is unknown. */
export function relativeTime(iso: string | undefined, now: number = Date.now()): string {
  const ms = parseTime(iso);
  if (ms === null) return '';
  const minutes = Math.floor((now - ms) / 60_000);
  if (minutes < 1) return 'now';
  if (minutes < 60) return `${minutes}m`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days < 7) return `${days}d`;
  return `${Math.floor(days / 7)}w`;
}

/** "NOVEMBER 1", or "NOVEMBER 1, 2024" when the post is not from the current year. */
export function dateLabel(iso: string | undefined, now: number = Date.now()): string | undefined {
  const ms = parseTime(iso);
  if (ms === null) return undefined;
  const date = new Date(ms);
  const sameYear = date.getFullYear() === new Date(now).getFullYear();
  return date
    .toLocaleDateString('en-US', sameYear
      ? { month: 'long', day: 'numeric' }
      : { month: 'long', day: 'numeric', year: 'numeric' })
    .toUpperCase();
}

/** "Instagram" for Instagram's own pages, else the host of the link ("example.com"); "the original" when unparseable. */
export function siteName(href: string | undefined): string {
  if (!href) return 'the original';
  if (isInstagramUrl(href)) return 'Instagram';
  try {
    return new URL(href).hostname.replace(/^www\./i, '') || 'the original';
  } catch {
    return 'the original';
  }
}

/**
 * Defence in depth for links that leave the app. The importer already sanitises them, but a
 * restored import comes out of IndexedDB, so the UI re-checks with the importer's own rules
 * before anything becomes an href: an http(s) link (never javascript:, data: ...) that, on
 * Instagram's own pages, leads to what its label promises.
 */
export const safeLinks = {
  /** "View on <site>" of a post; on Instagram: /p/<code>/, /reel/<code>/ ... */
  post: (value: string | undefined): string | undefined => sanitizePostUrl(value),
  /** "View profile on <site>"; on Instagram: /<username>/ */
  profile: (value: string | undefined): string | undefined => sanitizeProfileUrl(value),
  /** "View source on <site>" in the banner; on Instagram: the feed, a profile, a post or an explore page. */
  source: (value: string | undefined): string | undefined => sanitizeSourceUrl(value),
} as const;

/**
 * Files of the backend library (%TEMP%/instagram_cache) are served by this app's own API, on the
 * same origin: /api/library/files/<path>?r=<revision>. Nothing else on a server is ever rendered.
 */
const LIBRARY_FILE_RE = /^\/api\/library\/files\/(?![^?]*\/\.\.?(?:[/?]|$))[^\s#\\]+$/;

/**
 * Only locally held media may be rendered: object URLs of the import, inline pictures, or files of
 * the app's own library. The app never fetches remote pictures or videos.
 */
export function safeImageSrc(value: string | undefined): string | undefined {
  if (!value) return undefined;
  if (/^blob:/i.test(value) || LIBRARY_FILE_RE.test(value)) return value;
  return /^data:image\/(?:jpeg|png|gif|webp|avif|bmp);/i.test(value) ? value : undefined;
}

/** Same rule for videos; inline data is not accepted for them (they are never small). */
export function safeVideoSrc(value: string | undefined): string | undefined {
  if (!value) return undefined;
  return /^blob:/i.test(value) || LIBRARY_FILE_RE.test(value) ? value : undefined;
}

function toPhoto(media: ImportedMedia, key: PhotoKey, alt: string): Photo {
  const label = media.alt?.trim() || alt;
  if (media.kind !== 'video') return { key, emoji: '📷', src: safeImageSrc(media.url), alt: label };
  const video = safeVideoSrc(media.url);
  // A video that cannot be rendered still shows its poster, like a cover-only video post.
  if (!video) return { key, emoji: '🎬', src: safeImageSrc(media.posterUrl), alt: label };
  return { key, emoji: '🎬', src: safeImageSrc(media.posterUrl), alt: label, video, videoMime: media.mime || undefined };
}

function toPost(source: ImportedPost, now: number): Post {
  const { user } = source;
  const username = user.username;
  const key = placeholderKey(username);
  const total = source.media.length;
  const firstIsVideo = source.media[0]?.kind === 'video';
  const baseAlt = source.alt?.trim() || `${firstIsVideo ? 'Video' : 'Photo'} by ${username}`;

  const photos: Photo[] = source.media.map((media, i) => toPhoto(
    media,
    key,
    i === 0 ? baseAlt : `${media.kind === 'video' ? 'Video' : 'Photo'} ${i + 1} of ${total} by ${username}`,
  ));
  const cover: Photo = photos[0] ?? { key, emoji: '📷', alt: baseAlt };

  const comments: Comment[] = source.comments.map((comment, i) => ({
    id: `${i}:${comment.id}`,
    username: comment.username,
    text: comment.text,
    likes: Number.isFinite(comment.likes) && comment.likes > 0 ? comment.likes : 0,
    time: relativeTime(comment.takenAt, now),
  }));

  const likes = typeof source.likeCount === 'number' && Number.isFinite(source.likeCount) && source.likeCount >= 0
    ? source.likeCount
    : null;
  const commentCount = Number.isFinite(source.commentCount) && source.commentCount > 0 ? source.commentCount : 0;

  return {
    id: `imp:${source.id}`,
    username,
    verified: user.verified || undefined,
    time: relativeTime(source.takenAt, now),
    photo: cover,
    photos: photos.length > 1 ? photos : undefined,
    likes,
    caption: source.caption ?? '',
    comments,
    commentCount: Math.max(commentCount, comments.length),
    reposts: 0,
    shares: 0,
    date: dateLabel(source.takenAt, now),
    imported: true,
    permalink: safeLinks.post(source.permalink),
    profileUrl: safeLinks.profile(user.profileUrl),
    avatarSrc: safeImageSrc(user.avatarUrl),
    fullName: user.fullName?.trim() || undefined,
    location: source.location?.trim() || undefined,
    isVideo: source.mediaType === 'video' || firstIsVideo || undefined,
    takenAt: parseTime(source.takenAt) === null ? undefined : source.takenAt,
  };
}

/** Maps an import to feed posts, in the order of post.json. */
export function importToPosts(result: ImportResult, now: number = Date.now()): Post[] {
  const seen = new Set<string>();
  const posts: Post[] = [];
  for (const [index, source] of result.posts.entries()) {
    const post = { ...toPost(source, now), importIndex: index };
    // Ids are unique per the contract; a duplicate would break React keys, so it is made unique.
    let id = post.id;
    for (let n = 2; seen.has(id); n++) id = `${post.id}#${n}`;
    seen.add(id);
    posts.push(id === post.id ? post : { ...post, id });
  }
  return posts;
}

/** Distinct authors of the imported posts, in order of first appearance. */
export function buildUserDirectory(posts: readonly Post[]): Map<string, DirectoryUser> {
  const directory = new Map<string, DirectoryUser>();
  for (const post of posts) {
    if (!post.imported) continue;
    const known = directory.get(post.username);
    if (!known) {
      directory.set(post.username, {
        username: post.username,
        fullName: post.fullName,
        verified: post.verified,
        avatarSrc: post.avatarSrc,
        profileUrl: post.profileUrl,
        avatar: post.photo.key,
        postCount: 1,
      });
      continue;
    }
    known.postCount += 1;
    known.fullName ??= post.fullName;
    known.verified ??= post.verified;
    known.avatarSrc ??= post.avatarSrc;
    known.profileUrl ??= post.profileUrl;
  }
  return directory;
}

/** The stories tray stays a tray even when an import has thousands of authors. */
const MAX_STORIES = 30;

/** One story per imported author (the first MAX_STORIES), showing that author's first post. */
export function storiesFromPosts(posts: readonly Post[]): Story[] {
  const stories = new Map<string, Story>();
  for (const post of posts) {
    if (!post.imported || stories.has(post.username)) continue;
    if (stories.size >= MAX_STORIES) break;
    stories.set(post.username, {
      username: post.username,
      avatar: post.photo.key,
      seen: false,
      avatarSrc: post.avatarSrc,
      image: post.photo,
      time: post.time || undefined,
    });
  }
  for (const post of posts) {
    const story = stories.get(post.username);
    if (story && !story.avatarSrc && post.avatarSrc) story.avatarSrc = post.avatarSrc;
  }
  return [...stories.values()];
}
