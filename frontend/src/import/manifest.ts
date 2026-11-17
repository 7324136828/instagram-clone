/**
 * Lenient parser for post.json ("instagram-export" format v1, see /instagram.md).
 * It only looks at the JSON: media paths are normalised here and resolved to files later.
 */
import {
  DEFAULT_LIMITS,
  MAX_ALT_CHARS,
  MAX_CAPTION_CHARS,
  MAX_COMMENTS_PER_POST,
  MAX_COMMENT_CHARS,
  MAX_ID_CHARS,
  MAX_IMAGES_PER_POST,
  MAX_LOCATION_CHARS,
  MAX_NAME_CHARS,
  MAX_USERNAME_CHARS,
} from './limits';
import { normalizeRelativePath } from './paths';
import {
  ImportError,
  type ImportWarning,
  type ImportWarningCode,
  type ImportedComment,
  type ImportedMediaKind,
  type ImportedMediaType,
} from './types';
import {
  instagramPostUrl,
  instagramProfileUrl,
  isInstagramUrl,
  sanitizeInstagramUrl,
  sanitizePostUrl,
  sanitizeProfileUrl,
  sanitizeSourceUrl,
  shortcodeFromUrl,
  usernameFromProfileUrl,
} from './urls';

export interface ManifestUser {
  username: string;
  fullName?: string;
  verified?: boolean;
  /** Normalised path (relative to post.json) of the avatar image, when it is a local file. */
  avatarPath?: string;
  profileUrl?: string;
}

/** One picture or video of a post, as post.json describes it. */
export interface ManifestMedia {
  /** Normalised path relative to post.json. */
  path: string;
  /**
   * What post.json says the file is ("video" for the video/videos fields). Only a hint for
   * messages: the file's bytes decide what it really is.
   */
  type?: ImportedMediaKind;
  /** Normalised path of the poster image, for a video. */
  posterPath?: string;
  alt?: string;
}

export interface ManifestPost {
  /** Zero-based index of the entry in post.json. */
  index: number;
  id: string;
  permalink?: string;
  /** Normalised media with de-duplicated paths, in display order; never empty. */
  media: ManifestMedia[];
  user: ManifestUser;
  caption: string;
  commentCount: number;
  likeCount: number | null;
  takenAt?: string;
  /**
   * The post-level type: as declared ("video", "carousel"), otherwise "carousel" for several
   * media and "image" for one. Whether a single file is a video is only known from its bytes.
   */
  mediaType: ImportedMediaType;
  alt?: string;
  location?: string;
  comments: ImportedComment[];
}

export interface ParsedManifest {
  posts: ManifestPost[];
  warnings: ImportWarning[];
  /** Number of entries listed in post.json, before any skipping or truncation. */
  totalEntries: number;
  exportedAt?: string;
  sourceUrl?: string;
}

type Dict = Record<string, unknown>;

/** What a link is rendered as; each kind only accepts the pages that fit its label. */
const LINK_KINDS = {
  post: { sanitize: sanitizePostUrl, target: 'an Instagram post' },
  profile: { sanitize: sanitizeProfileUrl, target: 'an Instagram profile' },
  source: { sanitize: sanitizeSourceUrl, target: 'an Instagram profile, post or feed' },
} as const;

function isDict(value: unknown): value is Dict {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

/** comment_count, commentCount, Comment-Count, "comment count" ... all normalise to "commentcount". */
function normalizeKey(key: string): string {
  return key.toLowerCase().replace(/[^\p{L}\p{N}]+/gu, '');
}

/**
 * "post_image_path", "post caption", "postFrom": the field name without the "post" in front.
 * A separator or a camelCase hump is required, so that "postcode" and "poster" stay whole.
 */
function withoutPostPrefix(key: string): string | undefined {
  const match = /^post[^a-z0-9]+(.+)$/i.exec(key) ?? /^[Pp]ost(?=[A-Z][a-z])(.+)$/.exec(key);
  return match ? normalizeKey(match[1]) || undefined : undefined;
}

/** Alias-aware, case/underscore-insensitive view of a JSON object. */
class Fields {
  private readonly values = new Map<string, unknown>();

  constructor(source: Dict) {
    const keys = Object.keys(source);
    for (const key of keys) {
      const normalized = normalizeKey(key);
      if (!this.values.has(normalized)) this.values.set(normalized, source[key]);
    }
    // Second, so that a field under its plain name always wins over a "post ..." spelling.
    for (const key of keys) {
      const short = withoutPostPrefix(key);
      if (short && !this.values.has(short)) this.values.set(short, source[key]);
    }
  }

  /** First alias with a value other than undefined/null. */
  pick(aliases: readonly string[]): unknown {
    for (const alias of aliases) {
      const value = this.values.get(alias);
      if (value !== undefined && value !== null) return value;
    }
    return undefined;
  }
}

// Aliases, already normalised with normalizeKey().
const ALIASES = {
  posts: ['posts', 'items', 'entries', 'data'],
  image: [
    'image',
    'imagepath',
    'path',
    'file',
    'media',
    'src',
    'imagefile',
    'imagefilename',
    'filepath',
    'filename',
    'photo',
    'photopath',
    'picture',
    'img',
  ],
  images: ['images', 'imagepaths', 'mediapaths', 'photos', 'pictures'],
  /** The v1 media list; only an array or an object counts (a string is an "image" alias). */
  media: ['media', 'mediaitems'],
  mediaPath: ['path', 'file', 'src', 'url', 'filepath', 'filename'],
  mediaKind: ['type', 'kind', 'mediatype'],
  video: ['video', 'videopath', 'videofile'],
  videos: ['videos', 'videopaths'],
  poster: ['poster', 'thumbnail', 'cover', 'posterpath', 'thumbnailpath', 'coverpath', 'coverimage'],
  user: ['user', 'from', 'owner', 'author', 'username', 'fromuser', 'postedby'],
  username: ['username', 'name', 'handle'],
  fullName: ['fullname', 'displayname'],
  verified: ['verified', 'isverified'],
  avatar: ['avatar', 'avatarpath', 'profilepic'],
  profileUrl: ['profileurl', 'url'],
  entryProfileUrl: ['profileurl'],
  caption: [
    'caption',
    'description',
    'firstcomment',
    'text',
    'userfirstcomment',
    'usersfirstcomment',
    'veryfirstcomment',
    'userveryfirstcomment',
    'usersveryfirstcomment',
  ],
  captionText: ['text', 'caption', 'content', 'description'],
  commentCount: [
    'commentcount',
    'commentscount',
    'commentstotal',
    'totalcomments',
    'commenttotal',
    'commenttotalcount',
    'commentstotalcount',
    'totalcommentcount',
    'totalcommentscount',
    'numberofcomments',
    'numcomments',
  ],
  likeCount: ['likecount', 'likes', 'likescount', 'likestotal', 'totallikes', 'numberoflikes'],
  url: ['url', 'permalink', 'link', 'posturl'],
  id: ['id', 'shortcode', 'code'],
  shortcode: ['shortcode', 'code'],
  takenAt: ['takenat', 'timestamp', 'date', 'postedat', 'createdat'],
  mediaType: ['mediatype', 'type'],
  alt: ['alt', 'alttext', 'accessibilitycaption'],
  location: ['location', 'place'],
  locationName: ['name', 'title', 'label'],
  comments: ['comments', 'commentsample', 'samplecomments'],
  commentText: ['text', 'comment', 'content', 'body'],
  exportedAt: ['exportedat', 'createdat', 'date'],
  source: ['source', 'sourceurl'],
  sourceUrl: ['url', 'href', 'link'],
} as const;

/** Trims, drops NUL characters and caps the length without splitting a surrogate pair. */
function cleanText(value: unknown, maxChars: number): string {
  if (typeof value !== 'string' && typeof value !== 'number') return '';
  let text = String(value).replace(/\u0000/g, '').trim();
  if (text.length > maxChars) {
    text = text.slice(0, maxChars);
    const last = text.charCodeAt(text.length - 1);
    if (last >= 0xd800 && last <= 0xdbff) text = text.slice(0, -1);
  }
  return text;
}

function optionalText(value: unknown, maxChars: number): string | undefined {
  return cleanText(value, maxChars) || undefined;
}

function cleanUsername(value: unknown): string | undefined {
  if (typeof value !== 'string' && typeof value !== 'number') return undefined;
  const name = String(value).replace(/[\u0000-\u001f\u007f]/g, '').trim().replace(/^@+/, '').trim();
  return name ? name.slice(0, MAX_USERNAME_CHARS) : undefined;
}

function parseBoolean(value: unknown): boolean | undefined {
  if (typeof value === 'boolean') return value;
  if (value === 1 || value === 'true' || value === '1' || value === 'yes') return true;
  if (value === 0 || value === 'false' || value === '0' || value === 'no') return false;
  return undefined;
}

/** No count needs more characters than this ("9,007,199,254,740,991 comments" has 30). */
const MAX_COUNT_CHARS = 64;

/**
 * Digits with grouping, an optional magnitude, an optional unit word ("1,234 likes").
 * The text comes straight from post.json, so the pattern must stay linear: whitespace inside
 * the number is only taken when more of the number follows, and each optional part starts with
 * its own whitespace. That leaves a single way to match any input.
 */
const COUNT_RE = /^(\d(?:[\d.,'’_]|\s+(?=[\d.,'’_]))*)(?:\s*(k|m|b|thousand|million|billion))?(?:\s+[a-z]+)?$/;

/**
 * Parses a like/comment count: numbers, "1,234", "1 234", "12.3K", "1.2M", "3 thousand".
 * Returns null for anything invalid or negative.
 */
export function parseCount(value: unknown): number | null {
  if (typeof value === 'number') {
    if (!Number.isFinite(value) || value < 0) return null;
    return Math.min(Math.round(value), Number.MAX_SAFE_INTEGER);
  }
  // Instagram's own JSON nests counts as { "count": 12 }.
  if (isDict(value)) {
    const inner = value.count;
    return typeof inner === 'number' || typeof inner === 'string' ? parseCount(inner) : null;
  }
  if (typeof value !== 'string') return null;

  const text = value.trim().toLowerCase();
  if (text.length > MAX_COUNT_CHARS) return null;
  const match = COUNT_RE.exec(text);
  if (!match) return null;
  const digits = match[1].replace(/[\s'’_]/g, '');
  const suffix = match[2];
  const separators = digits.match(/[.,]/g) ?? [];

  let amount: number;
  if (separators.length === 0) {
    amount = Number(digits);
  } else if (!suffix && /^\d{1,3}(?:[.,]\d{3})+$/.test(digits)) {
    // 1,234 / 1.234.567: grouping separators.
    amount = Number(digits.replace(/[.,]/g, ''));
  } else {
    // The last separator is the decimal point (12.3K, 12,3K, 1.234,5).
    const last = Math.max(digits.lastIndexOf('.'), digits.lastIndexOf(','));
    amount = Number(`${digits.slice(0, last).replace(/[.,]/g, '')}.${digits.slice(last + 1) || '0'}`);
  }
  if (!Number.isFinite(amount)) return null;

  const multiplier =
    suffix === 'k' || suffix === 'thousand'
      ? 1e3
      : suffix === 'm' || suffix === 'million'
        ? 1e6
        : suffix === 'b' || suffix === 'billion'
          ? 1e9
          : 1;
  return Math.min(Math.round(amount * multiplier), Number.MAX_SAFE_INTEGER);
}

/**
 * Normalises a timestamp to an ISO 8601 string. Accepts ISO strings and epoch values in
 * seconds, milliseconds, microseconds or nanoseconds (as numbers or numeric strings).
 */
export function parseTimestamp(value: unknown): string | undefined {
  let ms: number;
  if (typeof value === 'number') {
    ms = epochToMs(value);
  } else if (typeof value === 'string') {
    const text = value.trim();
    if (!text) return undefined;
    if (/^\d+(?:\.\d+)?$/.test(text)) ms = epochToMs(Number(text));
    // Engines happily turn junk like "-5" into a date; insist on a four-digit year.
    else if (/\d{4}/.test(text)) ms = Date.parse(text);
    else return undefined;
  } else {
    return undefined;
  }
  if (!Number.isFinite(ms)) return undefined;
  const date = new Date(ms);
  const year = date.getUTCFullYear();
  if (Number.isNaN(year) || year < 1971 || year > 9999) return undefined;
  return date.toISOString();
}

function epochToMs(value: number): number {
  if (!Number.isFinite(value) || value <= 0) return NaN;
  if (value < 1e11) return Math.round(value * 1000); // seconds
  if (value < 1e14) return Math.round(value); // milliseconds
  if (value < 1e17) return Math.round(value / 1e3); // microseconds
  return Math.round(value / 1e6); // nanoseconds
}

function parseMediaType(value: unknown): ImportedMediaType | undefined {
  if (typeof value === 'number') {
    return value === 1 ? 'image' : value === 2 ? 'video' : value === 8 ? 'carousel' : undefined;
  }
  if (typeof value !== 'string') return undefined;
  const text = normalizeKey(value.trim());
  if (['image', 'photo', 'picture', 'graphimage', 'xdtgraphimage'].includes(text)) return 'image';
  if (['carousel', 'carouselalbum', 'album', 'sidecar', 'graphsidecar', 'xdtgraphsidecar', 'gallery'].includes(text)) {
    return 'carousel';
  }
  if (['video', 'reel', 'reels', 'clip', 'clips', 'igtv', 'graphvideo', 'xdtgraphvideo'].includes(text)) return 'video';
  return undefined;
}

/** "image"/"video" of a media object; anything else (including "carousel") says nothing. */
function mediaKind(value: unknown): ImportedMediaKind | undefined {
  const type = parseMediaType(value);
  return type === 'image' || type === 'video' ? type : undefined;
}

interface RawMedia {
  value: string;
  type?: ImportedMediaKind;
  poster?: string;
  alt?: string;
  /** The "image" field next to an "images" list. */
  cover?: boolean;
}

/** One item of a v1 "media" list: a path, or `{type?, path|file|src|url, poster?, alt?}`. */
function rawMediaItem(item: unknown): RawMedia | undefined {
  if (typeof item === 'string') return item.trim() ? { value: item } : undefined;
  if (!isDict(item)) return undefined;
  const fields = new Fields(item);
  let value = fields.pick([...ALIASES.mediaPath, ...ALIASES.image]);
  let type = mediaKind(fields.pick(ALIASES.mediaKind));
  if (typeof value !== 'string') {
    value = fields.pick(ALIASES.video);
    type ??= 'video';
  }
  if (typeof value !== 'string' || !value.trim()) return undefined;
  const raw: RawMedia = { value };
  if (type) raw.type = type;
  const poster = fields.pick(ALIASES.poster);
  if (typeof poster === 'string') raw.poster = poster;
  const alt = optionalText(fields.pick(ALIASES.alt), MAX_ALT_CHARS);
  if (alt) raw.alt = alt;
  return raw;
}

/** 53-bit string hash (cyrb53) in base 36. Not cryptographic: it only names posts. */
function hash53(text: string): string {
  let h1 = 0xdeadbeef;
  let h2 = 0x41c6ce57;
  for (let i = 0; i < text.length; i++) {
    const code = text.charCodeAt(i);
    h1 = Math.imul(h1 ^ code, 2654435761);
    h2 = Math.imul(h2 ^ code, 1597334677);
  }
  h1 = Math.imul(h1 ^ (h1 >>> 16), 2246822507) ^ Math.imul(h2 ^ (h2 >>> 13), 3266489909);
  h2 = Math.imul(h2 ^ (h2 >>> 16), 2246822507) ^ Math.imul(h1 ^ (h1 >>> 13), 3266489909);
  return (4294967296 * (2097151 & h2) + (h1 >>> 0)).toString(36);
}

/** "post-" + a fingerprint of the author, the date and the files of a post without an id. */
function derivedId(username: string | undefined, entry: Fields, media: readonly ManifestMedia[]): string {
  const takenAt = parseTimestamp(entry.pick(ALIASES.takenAt)) ?? '';
  const paths = media.map((item) => item.path).join('\n');
  return `post-${hash53(`${username ?? ''}\n${takenAt}\n${paths}`)}`;
}

/** Ids end up as React keys and URL fragments: keep them boring. */
function cleanId(value: unknown): string | undefined {
  if (typeof value !== 'string' && typeof value !== 'number') return undefined;
  const id = String(value)
    .trim()
    .replace(/[^A-Za-z0-9._-]+/g, '_')
    .slice(0, MAX_ID_CHARS);
  return /[A-Za-z0-9]/.test(id) ? id : undefined;
}

class Parser {
  readonly warnings: ImportWarning[] = [];
  private readonly usedIds = new Set<string>();
  private readonly defaultUser: ManifestUser | undefined;

  constructor(defaultUser: ManifestUser | undefined) {
    this.defaultUser = defaultUser;
  }

  warn(code: ImportWarningCode, message: string, postIndex?: number, path?: string): void {
    const warning: ImportWarning = { code, message };
    if (postIndex !== undefined) warning.postIndex = postIndex;
    if (path !== undefined) warning.path = path.slice(0, 500);
    this.warnings.push(warning);
  }

  /** Sanitised Instagram URL; a present but unacceptable value produces an 'unsafe-url' warning. */
  url(value: unknown, kind: keyof typeof LINK_KINDS, what: string, postIndex?: number): string | undefined {
    if (value === undefined || value === null || value === '') return undefined;
    const { sanitize, target } = LINK_KINDS[kind];
    const safe = sanitize(value);
    if (!safe) {
      const problem = sanitizeInstagramUrl(value)
        ? `is an instagram.com link, but not to ${target},`
        : 'is not a valid http(s) link';
      this.warn('unsafe-url', `${what} ${problem} and was ignored.`, postIndex);
    }
    return safe;
  }

  /**
   * `guessProfileUrl` is false for a post from another site: its author must not be linked to an
   * instagram.com profile of the same name.
   */
  user(value: unknown, entry: Fields | undefined, postIndex?: number, guessProfileUrl = true): ManifestUser | undefined {
    let username: string | undefined;
    let details: Fields | undefined;
    if (isDict(value)) {
      details = new Fields(value);
      username = cleanUsername(details.pick(ALIASES.username));
    } else {
      username = cleanUsername(value);
    }
    if (!username) return undefined;

    const user: ManifestUser = { username };
    // A user given as a plain string may still have its details next to it on the entry.
    const fullName = optionalText(details?.pick(ALIASES.fullName) ?? entry?.pick(ALIASES.fullName), MAX_NAME_CHARS);
    if (fullName) user.fullName = fullName;
    const verified = parseBoolean(details?.pick(ALIASES.verified) ?? entry?.pick(ALIASES.verified));
    if (verified !== undefined) user.verified = verified;

    const avatar = details?.pick(ALIASES.avatar) ?? entry?.pick(ALIASES.avatar);
    if (typeof avatar === 'string') {
      // Remote avatar URLs are never fetched; the avatar is optional, so they are just ignored.
      const normalized = normalizeRelativePath(avatar);
      if (normalized.path) user.avatarPath = normalized.path;
    }

    const who = postIndex === undefined ? 'The profile link' : `The profile link of post ${postIndex + 1}`;
    const profileUrl =
      this.url(details?.pick(ALIASES.profileUrl) ?? entry?.pick(ALIASES.entryProfileUrl), 'profile', who, postIndex) ??
      (guessProfileUrl ? instagramProfileUrl(username) : undefined);
    if (profileUrl) user.profileUrl = profileUrl;
    return user;
  }

  /** Raw media of an entry, before path normalisation: the v1 "media" list or the older fields. */
  private rawMedia(entry: Fields): RawMedia[] {
    const items: RawMedia[] = [];
    const full = () => items.length > MAX_IMAGES_PER_POST * 4;
    const media = entry.pick(ALIASES.media);
    if (Array.isArray(media) || isDict(media)) {
      for (const item of Array.isArray(media) ? media : [media]) {
        if (full()) break;
        const raw = rawMediaItem(item);
        if (raw) items.push(raw);
      }
      return items;
    }

    const collect = (value: unknown, depth: number, type: ImportedMediaKind | undefined): void => {
      if (full()) return;
      if (typeof value === 'string') {
        if (value.trim()) items.push(type ? { value, type } : { value });
      } else if (Array.isArray(value) && depth < 2) {
        for (const item of value) collect(item, depth + 1, type);
      } else if (isDict(value) && depth < 3) {
        const inner = new Fields(value).pick([...ALIASES.image, 'url']);
        if (typeof inner === 'string' && inner.trim()) items.push(type ? { value: inner, type } : { value: inner });
      }
    };
    collect(entry.pick(ALIASES.images), 0, undefined);
    const listed = items.length;
    collect(entry.pick(ALIASES.image), 0, undefined);
    // "image" is the cover: when it is not already part of "images" it goes first.
    if (listed > 0) for (const item of items.slice(listed)) item.cover = true;
    const firstVideo = items.length;
    collect(entry.pick(ALIASES.videos), 0, 'video');
    collect(entry.pick(ALIASES.video), 0, 'video');
    const poster = entry.pick(ALIASES.poster);
    if (typeof poster === 'string' && items.length > firstVideo) items[firstVideo].poster = poster;
    return items;
  }

  private media(entry: Fields, index: number): ManifestMedia[] {
    const media: ManifestMedia[] = [];
    for (const { value, type, poster, alt, cover } of this.rawMedia(entry)) {
      const normalized = normalizeRelativePath(value);
      if (normalized.issue === 'remote') {
        this.warn(
          'missing-image',
          `Post ${index + 1}: "${shorten(value)}" is a URL. ${type === 'video' ? 'Videos' : 'Images'} are not downloaded; the file must be inside the export.`,
          index,
          value,
        );
      } else if (normalized.issue === 'escape') {
        this.warn(
          'invalid-entry',
          `Post ${index + 1}: the ${type === 'video' ? 'video' : 'image'} path "${shorten(value)}" points outside the export and was ignored.`,
          index,
          value,
        );
      } else if (normalized.path && !media.some((item) => item.path === normalized.path)) {
        const item: ManifestMedia = { path: normalized.path };
        if (type) item.type = type;
        // Posters are optional: one that is remote or outside the export is simply not used.
        const posterPath = poster === undefined ? undefined : normalizeRelativePath(poster).path;
        if (posterPath) item.posterPath = posterPath;
        if (alt) item.alt = alt;
        if (cover) media.unshift(item);
        else media.push(item);
      }
    }

    if (media.length > MAX_IMAGES_PER_POST) {
      this.warn(
        'truncated',
        `Post ${index + 1} lists ${media.length} pictures and videos; only the first ${MAX_IMAGES_PER_POST} are imported.`,
        index,
      );
      media.length = MAX_IMAGES_PER_POST;
    }
    return media;
  }

  /** A derived id that repeats (the same files listed twice) is renamed without a warning. */
  private uniqueId(candidate: string, index: number, derived: boolean): string {
    let id = candidate;
    if (this.usedIds.has(id)) {
      let suffix = 2;
      while (this.usedIds.has(`${candidate}-${suffix}`)) suffix++;
      id = `${candidate}-${suffix}`;
      if (!derived) {
        this.warn('duplicate-id', `Post ${index + 1} reuses the id "${candidate}"; it was imported as "${id}".`, index);
      }
    }
    this.usedIds.add(id);
    return id;
  }

  /** The username an entry names (or the export's default user), without any warnings. */
  private usernameOf(entry: Fields): string | undefined {
    const value = entry.pick(ALIASES.user);
    const named = isDict(value) ? cleanUsername(new Fields(value).pick(ALIASES.username)) : cleanUsername(value);
    return named ?? this.defaultUser?.username;
  }

  private comments(value: unknown, postId: string): ImportedComment[] {
    if (!Array.isArray(value)) return [];
    const comments: ImportedComment[] = [];
    for (const item of value) {
      if (comments.length >= MAX_COMMENTS_PER_POST) break;
      if (!isDict(item)) continue;
      const fields = new Fields(item);
      const text = cleanText(fields.pick(ALIASES.commentText), MAX_COMMENT_CHARS);
      if (!text) continue;
      const author = fields.pick(ALIASES.user);
      const username =
        cleanUsername(isDict(author) ? new Fields(author).pick(ALIASES.username) : author) ?? 'unknown';
      const comment: ImportedComment = {
        id: `${postId}-c${comments.length + 1}`,
        username,
        text,
        likes: parseCount(fields.pick(ALIASES.likeCount)) ?? 0,
      };
      const takenAt = parseTimestamp(fields.pick(ALIASES.takenAt));
      if (takenAt) comment.takenAt = takenAt;
      comments.push(comment);
    }
    return comments;
  }

  post(raw: unknown, index: number): ManifestPost | null {
    if (!isDict(raw)) {
      this.warn('invalid-entry', `Post ${index + 1} is not a JSON object and was skipped.`, index);
      return null;
    }
    const entry = new Fields(raw);

    const before = this.warnings.length;
    const media = this.media(entry, index);
    if (media.length === 0) {
      // Path problems were already reported; only say something when there was nothing at all.
      if (this.warnings.length === before) {
        this.warn('invalid-entry', `Post ${index + 1} has no image path and was skipped.`, index);
      }
      return null;
    }

    const explicitShortcode = cleanId(entry.pick(ALIASES.shortcode));
    const permalink =
      this.url(entry.pick(ALIASES.url), 'post', `The link of post ${index + 1}`, index) ??
      (explicitShortcode ? instagramPostUrl(explicitShortcode) : undefined);

    // Only an Instagram link carries a shortcode; the path of another site's link says nothing.
    const fromInstagram = !permalink || isInstagramUrl(permalink);
    const givenId = cleanId(entry.pick(ALIASES.id)) ?? (fromInstagram ? shortcodeFromUrl(permalink) : undefined);
    // Without an id or a link, the id is derived from what the post is, not from where it is
    // listed: importing the same export again then updates its posts, while the first post of
    // an unrelated export does not replace them when imports are merged.
    const id = this.uniqueId(givenId ?? derivedId(this.usernameOf(entry), entry, media), index, !givenId);

    let user = this.user(entry.pick(ALIASES.user), entry, index, fromInstagram) ?? this.defaultUser;
    if (!user) {
      this.warn('invalid-entry', `Post ${index + 1} does not say who it is from; it is shown as "unknown".`, index);
      user = { username: 'unknown' };
    }

    const captionValue = entry.pick(ALIASES.caption);
    const caption = cleanText(
      isDict(captionValue) ? new Fields(captionValue).pick(ALIASES.captionText) : captionValue,
      MAX_CAPTION_CHARS,
    );

    const commentsValue = entry.pick(ALIASES.comments);
    const comments = this.comments(commentsValue, id);
    const commentCountValue = entry.pick(ALIASES.commentCount);
    const commentCount =
      commentCountValue !== undefined
        ? (parseCount(commentCountValue) ?? 0)
        : Array.isArray(commentsValue)
          ? comments.length
          : (parseCount(commentsValue) ?? 0);

    const declaredType = parseMediaType(entry.pick(ALIASES.mediaType));
    const mediaType: ImportedMediaType =
      declaredType === 'video' || declaredType === 'carousel'
        ? declaredType
        : media.length > 1
          ? 'carousel'
          : 'image';

    const post: ManifestPost = {
      index,
      id,
      media,
      user,
      caption,
      commentCount,
      likeCount: parseCount(entry.pick(ALIASES.likeCount)),
      mediaType,
      comments,
    };
    if (permalink) post.permalink = permalink;
    const takenAt = parseTimestamp(entry.pick(ALIASES.takenAt));
    if (takenAt) post.takenAt = takenAt;
    const alt = optionalText(entry.pick(ALIASES.alt), MAX_ALT_CHARS);
    if (alt) post.alt = alt;
    const locationValue = entry.pick(ALIASES.location);
    const location = optionalText(
      isDict(locationValue) ? new Fields(locationValue).pick(ALIASES.locationName) : locationValue,
      MAX_LOCATION_CHARS,
    );
    if (location) post.location = location;
    return post;
  }
}

function shorten(text: string): string {
  return text.length > 80 ? `${text.slice(0, 77)}...` : text;
}

/**
 * Turns the parsed JSON of post.json into normalised posts.
 * Throws ImportError('invalid-manifest') when the JSON is not an export at all and
 * ImportError('no-posts') when it lists no posts. Entries without a usable image path are
 * dropped with a warning; whether the image files exist is checked by the caller.
 */
export function parseManifest(json: unknown, maxPosts: number = DEFAULT_LIMITS.maxPosts): ParsedManifest {
  let entries: unknown[];
  let top: Fields | undefined;
  if (Array.isArray(json)) {
    entries = json;
  } else if (isDict(json)) {
    top = new Fields(json);
    const posts = top.pick(ALIASES.posts);
    if (!Array.isArray(posts)) {
      throw new ImportError(
        'invalid-manifest',
        'post.json does not contain a "posts" list. See instagram.md for the expected format.',
      );
    }
    entries = posts;
  } else {
    throw new ImportError(
      'invalid-manifest',
      'post.json must be a JSON object with a "posts" list, or a JSON list of posts.',
    );
  }
  if (entries.length === 0) {
    throw new ImportError('no-posts', 'post.json does not list any posts.');
  }

  // Export-level metadata, parsed first so that it can supply a default user.
  const meta = new Parser(undefined);
  let exportedAt: string | undefined;
  let sourceUrl: string | undefined;
  let defaultUser: ManifestUser | undefined;
  if (top) {
    exportedAt = parseTimestamp(top.pick(ALIASES.exportedAt));
    const source = top.pick(ALIASES.source);
    // A bare word such as "feed" is a label, not a link: only URL-looking strings are judged.
    const sourceLink = isDict(source) ? new Fields(source).pick(ALIASES.sourceUrl) : source;
    if (typeof sourceLink !== 'string' || /^(?:[a-z][a-z0-9+.-]*:|\/\/)/i.test(sourceLink.trim())) {
      sourceUrl = meta.url(sourceLink, 'source', 'The source link of the export');
    }
    defaultUser = meta.user(top.pick(ALIASES.user), undefined);
    if (!defaultUser) {
      const username = isInstagramUrl(sourceUrl) ? usernameFromProfileUrl(sourceUrl) : undefined;
      if (username) defaultUser = meta.user(username, undefined);
    }
  }

  const parser = new Parser(defaultUser);
  parser.warnings.push(...meta.warnings);
  const totalEntries = entries.length;
  if (totalEntries > maxPosts) {
    parser.warn(
      'truncated',
      `post.json lists ${totalEntries} posts; only the first ${maxPosts} are imported.`,
    );
  }

  const posts: ManifestPost[] = [];
  const count = Math.min(totalEntries, maxPosts);
  for (let index = 0; index < count; index++) {
    const post = parser.post(entries[index], index);
    if (post) posts.push(post);
  }

  const result: ParsedManifest = { posts, warnings: parser.warnings, totalEntries };
  if (exportedAt) result.exportedAt = exportedAt;
  if (sourceUrl) result.sourceUrl = sourceUrl;
  return result;
}

/** JSON.parse with the right ImportError. */
export function parseManifestText(text: string): unknown {
  try {
    return JSON.parse(text);
  } catch (err) {
    const detail = err instanceof Error ? ` (${err.message.slice(0, 160)})` : '';
    throw new ImportError('invalid-json', `post.json is not valid JSON${detail}.`);
  }
}
