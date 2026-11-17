import {
  createContext, useContext, useEffect, useRef, useState,
  type AnchorHTMLAttributes, type CSSProperties, type ReactNode, type RefObject,
} from 'react';
import { photoBackgrounds, type Photo } from '../data';
import type { UserDirectory } from '../importedPosts';

function initial(username: string): string {
  const clean = username.replace(/[^a-zA-Z0-9]/g, '');
  return (clean[0] ?? '?').toUpperCase();
}

const AVATAR_TINT: Record<string, string> = {
  ocean: '#0b3d6b', forest: '#173a26', city: '#2b3a55', moon: '#2a1a4a',
  books: '#5b4228', floral: '#a03f75', sunset: '#7a2f5a', coffee: '#3b2a20',
  mountains: '#2f4a34', cat: '#8a4a2a', art: '#3a2a6a', finance: '#0a2a43',
};

const EMPTY_DIRECTORY: UserDirectory = new Map();

/** Authors of the current import, by username. Empty while the seed data is shown. */
export const UserDirectoryContext = createContext<UserDirectory>(EMPTY_DIRECTORY);

export function useUserDirectory(): UserDirectory {
  return useContext(UserDirectoryContext);
}

export interface AvatarProps {
  username: string;
  avatar: string;
  /**
   * Real profile picture. When omitted, the picture of the imported user with this username is
   * used if there is one; pass null to force the initial-letter avatar.
   */
  src?: string | null;
  size?: number;
  ring?: 'none' | 'story' | 'seen' | 'live';
  onClick?: () => void;
  className?: string;
  title?: string;
}

export function Avatar({
  username,
  avatar,
  src,
  size = 32,
  ring = 'none',
  onClick,
  className = '',
  title,
}: AvatarProps) {
  const directory = useUserDirectory();
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  const candidate = src === null ? undefined : src ?? directory.get(username)?.avatarSrc;
  const imageSrc = candidate && candidate !== failedSrc ? candidate : undefined;

  const bg = photoBackgrounds[avatar as keyof typeof photoBackgrounds] ?? photoBackgrounds.ocean;
  const tint = AVATAR_TINT[avatar] ?? '#262626';
  const style: CSSProperties = {
    width: size,
    height: size,
    background: bg,
    color: tint,
    fontSize: Math.max(10, size * 0.42),
  };
  const inner = (
    <span className="avatar__inner" style={style}>
      {imageSrc ? (
        <img
          className="avatar__img"
          src={imageSrc}
          alt=""
          loading="lazy"
          decoding="async"
          draggable={false}
          onError={() => setFailedSrc(imageSrc)}
        />
      ) : (
        <span className="avatar__initial">{initial(username)}</span>
      )}
    </span>
  );

  const ringClass =
    ring === 'story' ? 'avatar-ring avatar-ring--story'
      : ring === 'seen' ? 'avatar-ring avatar-ring--seen'
        : ring === 'live' ? 'avatar-ring avatar-ring--live'
          : '';

  const content = (
    <>
      {ring === 'live' && <span className="avatar__live">LIVE</span>}
      {inner}
    </>
  );

  const cls = `avatar ${ringClass} ${className}`;

  if (onClick) {
    return (
      <button type="button" className={cls} onClick={onClick} title={title ?? username} aria-label={username}>
        {content}
      </button>
    );
  }

  return (
    <span className={cls} title={title ?? username}>
      {content}
    </span>
  );
}

/** Width / height of the images that have finished loading, by src. */
const ratioCache = new Map<string, number>();

/** Aspect ratio of an image that already loaded once in this session. */
export function knownRatio(src: string | undefined): number | undefined {
  return src ? ratioCache.get(src) : undefined;
}

/** Forget measured images; called when the object URLs of an import are revoked. */
export function clearImageCache(): void {
  ratioCache.clear();
}

export interface PhotoViewProps {
  photo: Photo;
  className?: string;
  onClick?: () => void;
  children?: ReactNode;
  square?: boolean;
  /** Fill the parent box instead of reserving an aspect ratio. */
  fill?: boolean;
  /** How a real image fits its box: cropped (cover, default) or letterboxed on black (contain). */
  fit?: 'cover' | 'contain';
  /** Accessible name of the button variant. */
  label?: string;
  /** Reports width / height once the real image has loaded. */
  onRatio?: (ratio: number) => void;
}

/**
 * A post picture. Seed photos are a gradient with an emoji; imported photos are a real <img>
 * that sits on the same gradient while loading and falls back to it when it cannot be decoded.
 * A video without a poster shows its first frame (a paused, muted <video> that loads metadata only;
 * it is decorative: the places that use it, grid tiles and stories, are labelled themselves).
 */
export function PhotoView({
  photo,
  className = '',
  onClick,
  children,
  square = true,
  fill = false,
  fit = 'cover',
  label = 'Open post',
  onRatio,
}: PhotoViewProps) {
  const frameOnly = !photo.src && !!photo.video;
  const src = photo.src ?? photo.video;
  const [settled, setSettled] = useState<{ src: string; ok: boolean } | null>(null);
  const failed = !!src && settled?.src === src && !settled.ok;
  const loaded = !!src && !failed && ((settled?.src === src && settled.ok) || ratioCache.has(src));
  const showImage = !!src && !failed;

  const style: CSSProperties = {
    // The gradient is the placeholder; once the image is in, the neutral class background applies.
    background: showImage && (loaded || fit === 'contain') ? undefined : photoBackgrounds[photo.key],
    aspectRatio: fill ? undefined : square ? '1 / 1' : '4 / 5',
  };
  const cls = [
    'photo',
    showImage ? 'photo--image' : '',
    showImage && fit === 'contain' ? 'photo--contain' : '',
    fill ? 'photo--fill' : '',
    className,
  ].filter(Boolean).join(' ');

  const measured = (width: number, height: number) => {
    if (!src) return;
    if (width > 0 && height > 0) {
      const ratio = width / height;
      ratioCache.set(src, ratio);
      onRatio?.(ratio);
    }
    setSettled({ src, ok: true });
  };

  let body: ReactNode;
  if (showImage && frameOnly) {
    body = (
      <video
        className="photo__img"
        src={src}
        aria-hidden="true"
        preload="metadata"
        muted
        playsInline
        tabIndex={-1}
        onLoadedData={(e) => measured(e.currentTarget.videoWidth, e.currentTarget.videoHeight)}
        onError={() => setSettled({ src, ok: false })}
      />
    );
  } else if (showImage) {
    body = (
      <img
        className="photo__img"
        src={src}
        alt={photo.alt ?? ''}
        loading="lazy"
        decoding="async"
        draggable={false}
        onLoad={(e) => measured(e.currentTarget.naturalWidth, e.currentTarget.naturalHeight)}
        onError={() => setSettled({ src, ok: false })}
      />
    );
  } else {
    body = (
      <span
        className="photo__subject"
        role={src ? 'img' : undefined}
        aria-label={src ? `${photo.alt ?? 'Photo'} (${frameOnly ? 'video' : 'image'} unavailable)` : undefined}
        aria-hidden={src ? undefined : true}
      >
        {photo.emoji}
      </span>
    );
  }

  if (onClick) {
    return (
      <button type="button" className={cls} style={style} onClick={onClick} aria-label={label}>
        {body}
        {children}
      </button>
    );
  }
  return (
    <div className={cls} style={style}>
      {body}
      {children}
    </div>
  );
}

export function VerifiedBadge({ size = 13 }: { size?: number }) {
  return (
    <svg
      className="verified-badge"
      width={size}
      height={size}
      viewBox="0 0 40 40"
      aria-label="Verified"
      role="img"
    >
      <path
        fill="#0095f6"
        d="M20 2.2 24.4 5.5 29.7 4.8 31.9 9.8 36.7 12.2 36 17.5 39.4 21.8 36 26.1 36.7 31.4 31.9 33.8 29.7 38.8 24.4 38.1 20 41.4 15.6 38.1 10.3 38.8 8.1 33.8 3.3 31.4 4 26.1.6 21.8 4 17.5 3.3 12.2 8.1 9.8 10.3 4.8 15.6 5.5 20 2.2Z"
      />
      <path
        d="m12.5 21 5 5 10-11"
        fill="none"
        stroke="#fff"
        strokeWidth="3.4"
        strokeLinecap="round"
        strokeLinejoin="round"
      />
    </svg>
  );
}

export function Spinner({ label }: { label?: string }) {
  return (
    <div className="spinner-wrap">
      <span className="spinner" />
      {label && <span className="spinner-label">{label}</span>}
    </div>
  );
}

export type ExternalLinkProps = Omit<AnchorHTMLAttributes<HTMLAnchorElement>, 'href' | 'target' | 'rel'> & {
  /** Must already be a trusted absolute URL (sanitised permalink / profile URL). */
  href: string;
};

/** A real link that leaves the app: always a new tab, never a window.opener or referrer leak. */
export function ExternalLink({ href, children, ...rest }: ExternalLinkProps) {
  return (
    <a {...rest} href={href} target="_blank" rel="noopener noreferrer">
      {children}
    </a>
  );
}

/**
 * #hashtag or @mention, only when it stands on its own (not inside a word, URL or e-mail address).
 * Group 1 is the character before the token, group 2 a hashtag, group 3 a username.
 */
const TOKEN_RE =
  /(^|[^\p{L}\p{N}_@#&/.])(?:#([\p{L}\p{M}\p{N}_]{1,100})|@([A-Za-z0-9_](?:[A-Za-z0-9_.]{0,28}[A-Za-z0-9_])?))(?![\p{L}\p{N}_])/gu;

export interface RichTextProps {
  text: string;
  /** Turn #hashtags and @mentions into links to instagram.com (imported content only). */
  linkify?: boolean;
}

/** Caption / comment text. The only hrefs built from manifest text are the strict tokens above. */
export function RichText({ text, linkify = false }: RichTextProps) {
  if (!linkify || !/[#@]/.test(text)) return <>{text}</>;
  const nodes: ReactNode[] = [];
  let last = 0;
  for (const match of text.matchAll(TOKEN_RE)) {
    const start = (match.index ?? 0) + match[1].length;
    const token = match[0].slice(match[1].length);
    const href = match[2] !== undefined
      ? `https://www.instagram.com/explore/tags/${encodeURIComponent(match[2])}/`
      : `https://www.instagram.com/${encodeURIComponent(match[3])}/`;
    if (start > last) nodes.push(text.slice(last, start));
    nodes.push(<ExternalLink key={start} className="rich-link" href={href}>{token}</ExternalLink>);
    last = start + token.length;
  }
  if (last < text.length) nodes.push(text.slice(last));
  return <>{nodes}</>;
}

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), summary, [tabindex]';

function focusableIn(root: HTMLElement): HTMLElement[] {
  return Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE)).filter(
    (el) => el.tabIndex >= 0 && el.getClientRects().length > 0,
  );
}

/** Open dialogs, outermost first; only the last one traps the Tab key. */
const dialogStack: HTMLElement[] = [];

/** True when `node` is the innermost open dialog: the one a key press is meant for. */
export function isTopDialog(node: HTMLElement | null): boolean {
  return node !== null && dialogStack[dialogStack.length - 1] === node;
}

/** Whether focus can sensibly go back to `element` when a dialog closes. */
function canReturnTo(element: HTMLElement | null): element is HTMLElement {
  return !!element && element.isConnected && element !== document.body && element !== document.documentElement;
}

/**
 * Focus management for a modal dialog: focus moves into the dialog when it mounts, Tab cycles
 * inside it, and focus returns to the previously focused element when it unmounts. When there is
 * none (the dialog opened by itself, or its opener is gone), focus goes to `fallback()` instead of
 * dropping to the top of the document.
 * Attach the returned ref to the dialog element (give it tabIndex={-1}).
 */
export function useDialogFocus<T extends HTMLElement>(fallback?: () => HTMLElement | null): RefObject<T> {
  const ref = useRef<T>(null);
  const fallbackRef = useRef(fallback);
  fallbackRef.current = fallback;

  useEffect(() => {
    const node = ref.current;
    if (!node) return undefined;
    const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
    dialogStack.push(node);
    if (!node.contains(document.activeElement)) node.focus({ preventScroll: true });

    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key !== 'Tab' || dialogStack[dialogStack.length - 1] !== node) return;
      const items = focusableIn(node);
      const active = document.activeElement;
      if (items.length === 0) {
        e.preventDefault();
        node.focus({ preventScroll: true });
        return;
      }
      const first = items[0];
      const last = items[items.length - 1];
      const inside = active instanceof Node && node.contains(active);
      if (e.shiftKey && (!inside || active === first || active === node)) {
        e.preventDefault();
        last.focus();
      } else if (!e.shiftKey && (!inside || active === last)) {
        e.preventDefault();
        first.focus();
      }
    };
    document.addEventListener('keydown', onKeyDown, true);

    return () => {
      document.removeEventListener('keydown', onKeyDown, true);
      const at = dialogStack.lastIndexOf(node);
      if (at >= 0) dialogStack.splice(at, 1);
      if (canReturnTo(previous)) {
        previous.focus({ preventScroll: true });
        return;
      }
      // Only when nothing else took focus meanwhile (another dialog, the page's own code).
      const now = document.activeElement;
      if (now && now !== document.body && now.isConnected && !node.contains(now)) return;
      fallbackRef.current?.()?.focus({ preventScroll: true });
    };
  }, []);

  return ref;
}

/**
 * Renders long lists in pages: `visible` items now, more when the sentinel element (rendered by
 * the caller after the list while `hasMore`) scrolls near the viewport.
 */
export function useIncremental(total: number, pageSize: number) {
  const [count, setCount] = useState(pageSize);
  const sentinel = useRef<HTMLDivElement>(null);
  const visible = Math.min(count, total);
  const hasMore = visible < total;

  useEffect(() => {
    const node = sentinel.current;
    if (!hasMore || !node) return undefined;
    if (typeof IntersectionObserver === 'undefined') {
      setCount(total);
      return undefined;
    }
    // Re-created after every page so a sentinel that is still in range triggers the next one.
    const observer = new IntersectionObserver((entries) => {
      if (entries.some((entry) => entry.isIntersecting)) setCount((c) => c + pageSize);
    }, { rootMargin: '900px 0px' });
    observer.observe(node);
    return () => observer.disconnect();
  }, [hasMore, pageSize, total, visible]);

  return { visible, hasMore, sentinel };
}
