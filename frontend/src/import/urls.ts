import { MAX_URL_CHARS } from './limits';

const USERNAME_RE = /^[A-Za-z0-9._]{1,30}$/;
const SHORTCODE_RE = /^[A-Za-z0-9_-]{1,64}$/;
/** First path segments of instagram.com that are not profiles. */
const RESERVED_SEGMENTS = new Set([
  'p',
  'reel',
  'reels',
  'tv',
  'explore',
  'stories',
  'accounts',
  'direct',
  'about',
  'developer',
  'legal',
  'web',
  'api',
  'graphql',
  'share',
  'oauth',
  'linkshim',
  'challenge',
  'login',
  'logout',
  'emails',
  'session',
  'ajax',
]);
const POST_SEGMENTS = new Set(['p', 'reel', 'reels', 'tv']);

/**
 * The hosts that serve Instagram's own pages. Deliberately not "any subdomain":
 * l.instagram.com forwards to arbitrary sites, api.instagram.com hosts OAuth consent pages,
 * and nothing an export links to lives anywhere else.
 */
const INSTAGRAM_HOSTS = new Set(['instagram.com', 'www.instagram.com', 'm.instagram.com', 'instagr.am']);

function isInstagramHost(hostname: string): boolean {
  return INSTAGRAM_HOSTS.has(hostname.toLowerCase());
}

/**
 * Returns a safe https URL on Instagram, or undefined.
 *
 * Only absolute http(s) URLs are accepted (http is upgraded). The host must be instagram.com,
 * www.instagram.com, m.instagram.com or instagr.am. Credentials and ports are dropped.
 * Everything else (javascript:, data:, protocol-relative URLs, look-alike hosts, other
 * subdomains, malformed input) is rejected.
 *
 * This checks where a link goes, not what it shows: use sanitizePostUrl, sanitizeProfileUrl
 * or sanitizeSourceUrl for a link that is rendered under a label such as "View on Instagram".
 */
export function sanitizeInstagramUrl(value: unknown): string | undefined {
  if (typeof value !== 'string') return undefined;
  const raw = value.trim();
  if (!raw || raw.length > MAX_URL_CHARS) return undefined;
  // An explicit http(s) scheme is required: no relative, protocol-relative or exotic schemes.
  if (!/^https?:\/\//i.test(raw)) return undefined;
  // Whitespace, control characters and backslashes are parser-differential bait.
  if (/[\u0000- \u007f-\u009f\\]/.test(raw)) return undefined;

  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return undefined;
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return undefined;
  if (!isInstagramHost(url.hostname)) return undefined;

  // Rebuilt from parsed parts: https only, no user:password@, no port.
  return `https://${url.hostname.toLowerCase()}${url.pathname}${url.search}${url.hash}`;
}

/** True for a link on one of Instagram's own hosts (see INSTAGRAM_HOSTS). */
export function isInstagramUrl(value: string | undefined): boolean {
  if (!value) return false;
  try {
    return isInstagramHost(new URL(value).hostname);
  } catch {
    return false;
  }
}

/**
 * Returns a safe http(s) URL on any site, or undefined: the posts of an export may come from
 * anywhere, not only from Instagram.
 *
 * The same checks as sanitizeInstagramUrl apply (absolute http(s) only, no whitespace, control
 * characters or backslashes, bounded length), and credentials (user:password@) are dropped. Links
 * on Instagram's own hosts are returned exactly as sanitizeInstagramUrl does (https, no port); other
 * hosts keep their scheme and port. javascript:, data:, protocol-relative and malformed URLs are rejected.
 */
export function sanitizeWebUrl(value: unknown): string | undefined {
  const instagram = sanitizeInstagramUrl(value);
  if (instagram) return instagram;
  if (typeof value !== 'string') return undefined;
  const raw = value.trim();
  if (!raw || raw.length > MAX_URL_CHARS) return undefined;
  // The host must follow the two slashes directly: URL would read "https:///x" as the host "x".
  if (!/^https?:\/\/[^/?#]/i.test(raw)) return undefined;
  if (/[\u0000- \u007f-\u009f\\]/.test(raw)) return undefined;

  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return undefined;
  }
  if (url.protocol !== 'http:' && url.protocol !== 'https:') return undefined;
  if (!url.hostname) return undefined;
  // url.host is lower-case and carries the port, but never user:password@.
  return `${url.protocol}//${url.host}${url.pathname}${url.search}${url.hash}`;
}

/** https://www.instagram.com/<username>/ when the username is a plausible Instagram handle. */
export function instagramProfileUrl(username: string): string | undefined {
  return USERNAME_RE.test(username) ? `https://www.instagram.com/${username}/` : undefined;
}

/** https://www.instagram.com/p/<shortcode>/ when the shortcode is well formed. */
export function instagramPostUrl(shortcode: string): string | undefined {
  return SHORTCODE_RE.test(shortcode) ? `https://www.instagram.com/p/${shortcode}/` : undefined;
}

function pathSegments(url: string | undefined): string[] {
  if (!url) return [];
  try {
    return new URL(url).pathname.split('/').filter(Boolean);
  } catch {
    return [];
  }
}

function isProfileSegment(segment: string | undefined): boolean {
  return segment !== undefined && USERNAME_RE.test(segment) && !RESERVED_SEGMENTS.has(segment.toLowerCase());
}

/** /p/<code>/..., also below a username (/natgeo/p/<code>/) or as a share link (/share/reel/<code>/). */
function isPostPath(segments: readonly string[]): boolean {
  const at = segments.length >= 3 && (isProfileSegment(segments[0]) || segments[0].toLowerCase() === 'share') ? 1 : 0;
  return segments.length >= at + 2 && POST_SEGMENTS.has(segments[at].toLowerCase()) && SHORTCODE_RE.test(segments[at + 1]);
}

/**
 * The permalink of a post. Any http(s) link is accepted; on Instagram's own hosts it must also be
 * the permalink of a post or reel, so that "View on Instagram" never leads to the wrong page.
 */
export function sanitizePostUrl(value: unknown): string | undefined {
  const url = sanitizeWebUrl(value);
  if (!url) return undefined;
  return !isInstagramUrl(url) || isPostPath(pathSegments(url)) ? url : undefined;
}

/**
 * A profile page. Any http(s) link is accepted; on Instagram's own hosts it must be
 * https://www.instagram.com/<username>/.
 */
export function sanitizeProfileUrl(value: unknown): string | undefined {
  const url = sanitizeWebUrl(value);
  if (!url) return undefined;
  return !isInstagramUrl(url) || usernameFromProfileUrl(url) ? url : undefined;
}

/**
 * A page an export can be taken from. Any http(s) link is accepted; on Instagram's own hosts it
 * must be the home feed, a profile or one of its tabs, a post, or an explore page (hashtag, location).
 */
export function sanitizeSourceUrl(value: unknown): string | undefined {
  const url = sanitizeWebUrl(value);
  if (!url) return undefined;
  if (!isInstagramUrl(url)) return url;
  const segments = pathSegments(url);
  const ok =
    segments.length === 0 ||
    isPostPath(segments) ||
    segments[0].toLowerCase() === 'explore' ||
    isProfileSegment(segments[0]);
  return ok ? url : undefined;
}

/** Shortcode of a post permalink: /p/<code>/, /reel/<code>/, /reels/<code>/ or /tv/<code>/. */
export function shortcodeFromUrl(url: string | undefined): string | undefined {
  const segments = pathSegments(url);
  for (let i = 0; i < segments.length - 1; i++) {
    const kind = segments[i].toLowerCase();
    if ((kind === 'p' || kind === 'reel' || kind === 'reels' || kind === 'tv') && SHORTCODE_RE.test(segments[i + 1])) {
      return segments[i + 1];
    }
  }
  return undefined;
}

/** Username of a profile URL such as https://www.instagram.com/natgeo/. */
export function usernameFromProfileUrl(url: string | undefined): string | undefined {
  const segments = pathSegments(url);
  if (segments.length !== 1) return undefined;
  return isProfileSegment(segments[0]) ? segments[0] : undefined;
}
