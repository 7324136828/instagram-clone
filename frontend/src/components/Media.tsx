import { useEffect, useRef, useState, type CSSProperties, type ReactNode, type TouchEvent } from 'react';
import { formatCount, postPhotos, type Photo, type Post } from '../data';
import { Icon } from '../icons';
import { siteName } from '../importedPosts';
import { ExternalLink, knownRatio, PhotoView } from './common';
import { PostVideo } from './Video';

/** Instagram's feed frame: no wider than 1.91:1 (landscape), no taller than 4:5 (portrait). */
const MAX_RATIO = 1.91;
const MIN_RATIO = 4 / 5;

function clampRatio(ratio: number): number {
  return Math.min(MAX_RATIO, Math.max(MIN_RATIO, ratio));
}

export interface PostMediaProps {
  post: Post;
  /** feed: full-width frame that follows the cover's aspect ratio. detail: fills its box, letterboxed. */
  variant: 'feed' | 'detail';
  /** Index of the slide on screen. */
  index: number;
  onIndexChange: (index: number) => void;
  /** Overlays drawn above the picture (badges, like animation). */
  children?: ReactNode;
  /** Videos start on their own (muted): in the feed while most visible, elsewhere when on screen. Default true. */
  autoPlay?: boolean;
  /** The video of the current slide played to its end (a detail video then does not loop). */
  onVideoEnded?: () => void;
  /** Length in seconds of the current slide's video, once known (Infinity: it plays, length unknown). */
  onVideoDuration?: (seconds: number) => void;
  /** The current slide's video cannot be played. */
  onVideoFailed?: () => void;
}

/**
 * The media area of a post: one picture or video, or a carousel with arrows, dots and an n/total
 * counter.
 */
export function PostMedia({
  post, variant, index, onIndexChange, children, autoPlay = true, onVideoEnded, onVideoDuration, onVideoFailed,
}: PostMediaProps) {
  const photos = postPhotos(post);
  const count = photos.length;
  const current = Math.min(Math.max(index, 0), count - 1);
  const coverSrc = photos[0].src ?? photos[0].video;
  const [measured, setMeasured] = useState<{ src: string; ratio: number } | null>(null);
  const ratio = measured && measured.src === coverSrc ? measured.ratio : knownRatio(coverSrc);
  const touchStart = useRef<{ x: number; y: number } | null>(null);
  const preloaded = useRef<HTMLImageElement | null>(null);
  const frame = useRef<HTMLDivElement>(null);
  /** An arrow was used while it had focus: focus has to survive that arrow disappearing. */
  const keepFocus = useRef(false);

  const go = (step: number) => {
    const next = Math.min(Math.max(current + step, 0), count - 1);
    if (next !== current) onIndexChange(next);
  };

  const page = (step: number, button: HTMLButtonElement) => {
    keepFocus.current = document.activeElement === button;
    go(step);
  };

  // The arrows are not rendered at the first / last slide. When the one that was just used is
  // gone, focus moves to the one that is left instead of being dropped to <body>.
  useEffect(() => {
    if (!keepFocus.current) return;
    keepFocus.current = false;
    const node = frame.current;
    if (node && !node.contains(document.activeElement)) node.querySelector<HTMLElement>('.media__nav')?.focus();
  }, [current]);

  // Slides outside the frame are lazy; warm up the next one so paging does not flash the placeholder.
  const nextSrc = ratio !== undefined ? photos[current + 1]?.src : undefined;
  useEffect(() => {
    if (!nextSrc) return;
    const image = new Image();
    image.decoding = 'async';
    image.src = nextSrc;
    preloaded.current = image;
  }, [nextSrc]);

  const onTouchStart = (e: TouchEvent) => {
    const touch = e.touches[0];
    touchStart.current = touch ? { x: touch.clientX, y: touch.clientY } : null;
  };
  const onTouchEnd = (e: TouchEvent) => {
    const start = touchStart.current;
    const touch = e.changedTouches[0];
    touchStart.current = null;
    if (!start || !touch) return;
    const dx = touch.clientX - start.x;
    const dy = touch.clientY - start.y;
    if (Math.abs(dx) > 40 && Math.abs(dx) > Math.abs(dy) * 1.5) go(dx < 0 ? 1 : -1);
  };

  const style: CSSProperties | undefined = variant === 'feed'
    ? { aspectRatio: `${clampRatio(ratio ?? 1).toFixed(4)} / 1` }
    : undefined;
  const swipe = count > 1 ? { onTouchStart, onTouchEnd } : undefined;

  return (
    <div className={`media media--${variant}${photos[current].video ? ' media--video' : ''}`} style={style} data-slide={current + 1} data-slides={count} ref={frame} {...swipe}>
      <div className="media__track" style={{ transform: `translateX(${-current * 100}%)` }}>
        {photos.map((photo, i) => {
          const key = photo.src ?? photo.video;
          const onRatio = i === 0 && key ? (r: number) => setMeasured({ src: key, ratio: r }) : undefined;
          const fit = variant === 'detail' ? 'contain' : 'cover';
          return (
            <div className="media__slide" key={i} aria-hidden={i === current ? undefined : true}>
              {photo.video ? (
                <PostVideo
                  photo={photo}
                  src={photo.video}
                  variant={variant}
                  current={i === current}
                  autoPlay={autoPlay}
                  fit={fit}
                  onRatio={onRatio}
                  onEnded={onVideoEnded}
                  onDuration={onVideoDuration}
                  onFailed={onVideoFailed}
                />
              ) : (
                <PhotoView photo={photo} fill fit={fit} onRatio={onRatio} />
              )}
            </div>
          );
        })}
      </div>

      {/* A video post of which only the cover picture was exported. */}
      {post.isVideo && !photos[0].video && (
        <span className="media__badge" role="img" aria-label="Video" title="Video: only the cover image was exported">
          <Icon name="clip" size={20} />
        </span>
      )}

      {count > 1 && (
        <>
          {current > 0 && (
            <button
              className="media__nav media__nav--prev"
              type="button"
              aria-label="Previous photo"
              onClick={(e) => page(-1, e.currentTarget)}
              onDoubleClick={(e) => e.stopPropagation()}
            >
              <Icon name="chevronLeft" size={16} strokeWidth={2.6} />
            </button>
          )}
          {current < count - 1 && (
            <button
              className="media__nav media__nav--next"
              type="button"
              aria-label="Next photo"
              onClick={(e) => page(1, e.currentTarget)}
              onDoubleClick={(e) => e.stopPropagation()}
            >
              <Icon name="chevronRight" size={16} strokeWidth={2.6} />
            </button>
          )}
          <span className="media__counter" aria-live="polite" data-testid="carousel-counter">{current + 1}/{count}</span>
          <span className="media__dots" aria-hidden="true">
            {photos.map((_, i) => (
              <span key={i} className={`media__dot ${i === current ? 'media__dot--active' : ''}`} />
            ))}
          </span>
        </>
      )}

      {children}
    </div>
  );
}

export interface PostTileProps {
  post: Post;
  /** Picture to show instead of the post's cover (seed placeholder grids). */
  photo?: Photo;
  className?: string;
  /** Let the grid decide the height (tall mosaic tiles) instead of a square. */
  stretch?: boolean;
  /** Show the like / comment counts on hover and focus. */
  stats?: boolean;
  onOpen: (post: Post) => void;
}

/** A grid tile (Explore, profile, "more posts"): square cover crop that opens the post. */
export function PostTile({ post, photo = post.photo, className = '', stretch = false, stats = true, onOpen }: PostTileProps) {
  const multiple = (post.photos?.length ?? 0) > 1;
  return (
    <PhotoView
      photo={photo}
      square={!stretch}
      fill={stretch}
      className={`tile ${className}`}
      label={`Open post by ${post.username}`}
      onClick={() => onOpen(post)}
    >
      {multiple ? (
        <span className="tile__badge" role="img" aria-label="Carousel"><Icon name="carousel" size={18} /></span>
      ) : post.isVideo ? (
        <span className="tile__badge" role="img" aria-label="Video"><Icon name="clip" size={18} /></span>
      ) : null}
      {stats && (
        <span className="explore__overlay">
          {post.likes !== null && <><Icon name="heart" size={18} filled /> {formatCount(post.likes)}</>}
          <Icon name="comment" size={18} filled /> {formatCount(post.commentCount)}
        </span>
      )}
    </PhotoView>
  );
}

/** The post's age. On imported posts it is a link to the real post. */
export function PostTime({ post }: { post: Post }) {
  if (!post.time) return null;
  const label = post.takenAt ? <time dateTime={post.takenAt}>{post.time}</time> : post.time;
  if (!post.permalink) return <span className="post__time">{label}</span>;
  return (
    <ExternalLink
      className="post__time post__time--link"
      href={post.permalink}
      title="Open this post on Instagram"
      data-testid="post-time-link"
    >
      {label}
    </ExternalLink>
  );
}

/** Always-visible icon link from an imported post to the real post (on instagram.com or wherever it came from). */
export function ViewOnInstagram({ href }: { href: string }) {
  const label = `View on ${siteName(href)}`;
  return (
    <ExternalLink
      className="icon-btn post__external"
      href={href}
      aria-label={label}
      title={label}
      data-testid="post-permalink"
    >
      <Icon name="external" size={20} />
    </ExternalLink>
  );
}

const CAPTION_LIMIT = 96;

/**
 * Collapsed form of a long caption (at most ~96 characters and two lines), or null when the
 * caption is short enough to be shown in full.
 */
export function captionPreview(caption: string): string | null {
  let cut = Math.min(caption.length, CAPTION_LIMIT);
  const firstBreak = caption.indexOf('\n');
  const secondBreak = firstBreak < 0 ? -1 : caption.indexOf('\n', firstBreak + 1);
  if (secondBreak >= 0 && secondBreak < cut) {
    cut = secondBreak;
  } else if (cut < caption.length) {
    // Prefer a word boundary so a #hashtag or @mention is never cut in half.
    const space = caption.slice(0, cut + 1).search(/\s\S*$/);
    if (space > CAPTION_LIMIT * 0.6) cut = space;
    // Never split a surrogate pair (emoji).
    const code = caption.charCodeAt(cut - 1);
    if (code >= 0xd800 && code <= 0xdbff) cut -= 1;
  }
  if (cut >= caption.length) return null;
  const preview = caption.slice(0, cut).trimEnd();
  return preview.length < caption.trimEnd().length ? preview : null;
}
