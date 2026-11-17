import {
  useEffect, useMemo, useReducer, useRef, useState, type FormEvent, type KeyboardEvent as ReactKeyboardEvent,
} from 'react';
import {
  ActiveTimer, announcement, formatInterval, initPlayer, MAX_INTERVAL_SEC, MIN_INTERVAL_SEC, parseIntervalInput,
  planSlide, playerReducer, postProgress, previousPosition, slideSeconds, startCursor, stepInterval,
  type AutoViewSettings, type VideoState,
} from '../autoView';
import { formatCount, postPhotos, type Post } from '../data';
import { Icon } from '../icons';
import { siteName } from '../importedPosts';
import { Avatar, ExternalLink, isTopDialog, useDialogFocus } from './common';
import { captionPreview, PostMedia } from './Media';
import '../styles/autoview.css';

// --- Setup -------------------------------------------------------------------------------------

export interface AutoViewSetupProps {
  open: boolean;
  settings: AutoViewSettings;
  /** Number of posts auto view will go through. */
  postCount: number;
  onStart: (s: AutoViewSettings) => void;
  onClose: () => void;
}

const PRESETS = [5, 10, 15, 30, 60];

/**
 * Where focus goes after auto view when nothing opened it (the "#autoview" address) or its opener
 * is gone (a menu item): the banner's Auto view button, else the main region.
 */
function autoViewReturnTarget(): HTMLElement | null {
  const button = document.querySelector<HTMLElement>('[data-testid="auto-view-open"]');
  if (button) return button;
  const main = document.querySelector<HTMLElement>('main');
  // Focusable for this one focus only: clicks in the feed must not focus the whole region.
  if (main && !main.hasAttribute('tabindex')) {
    main.setAttribute('tabindex', '-1');
    main.addEventListener('blur', () => main.removeAttribute('tabindex'), { once: true });
  }
  return main;
}

/** The dialog that asks how long each post stays on screen before auto view starts. */
export function AutoViewSetup({ open, ...props }: AutoViewSetupProps): JSX.Element | null {
  // Mounted on each opening, so the form starts from the current settings.
  return open ? <SetupDialog {...props} /> : null;
}

function SetupDialog({ settings, postCount, onStart, onClose }: Omit<AutoViewSetupProps, 'open'>) {
  const dialogRef = useDialogFocus<HTMLDivElement>(autoViewReturnTarget);
  const inputRef = useRef<HTMLInputElement>(null);
  const [draft, setDraft] = useState(String(settings.intervalSec));
  const [waitForVideos, setWaitForVideos] = useState(settings.waitForVideos);
  const [loop, setLoop] = useState(settings.loop);
  const [shuffle, setShuffle] = useState(settings.shuffle);
  const [invalid, setInvalid] = useState(false);
  const empty = postCount === 0;

  // After useDialogFocus has remembered the opener: start in the seconds field.
  useEffect(() => {
    inputRef.current?.focus({ preventScroll: true });
    inputRef.current?.select();
  }, []);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    const intervalSec = parseIntervalInput(draft);
    if (intervalSec === null) {
      setInvalid(true);
      inputRef.current?.focus();
      return;
    }
    onStart({ intervalSec, waitForVideos, loop, shuffle });
  };

  const onKeyDown = (e: ReactKeyboardEvent) => {
    if (e.key !== 'Escape') return;
    e.stopPropagation();
    onClose();
  };

  return (
    <div className="sheet-overlay" onClick={onClose} role="presentation">
      <div
        className="sheet av-setup"
        role="dialog"
        aria-modal="true"
        aria-labelledby="av-setup-title"
        aria-describedby="av-setup-lead"
        data-testid="auto-view-setup"
        tabIndex={-1}
        ref={dialogRef}
        onClick={(e) => e.stopPropagation()}
        onKeyDown={onKeyDown}
      >
        <form onSubmit={submit} noValidate>
          <div className="av-setup__body">
            <h2 id="av-setup-title" className="av-setup__title">Auto view</h2>
            <p id="av-setup-lead" className="av-setup__lead">
              {empty
                ? 'There are no posts to show yet.'
                : `Shows ${postCount === 1 ? 'the post' : `the ${postCount.toLocaleString('en-US')} posts`} one after another and moves on by itself.`}
            </p>

            <div className="av-setup__field">
              <label htmlFor="av-setup-seconds" className="av-setup__label">Time per post</label>
              <div className="av-setup__seconds">
                <input
                  id="av-setup-seconds"
                  ref={inputRef}
                  type="number"
                  inputMode="numeric"
                  min={MIN_INTERVAL_SEC}
                  max={MAX_INTERVAL_SEC}
                  step={1}
                  value={draft}
                  aria-invalid={invalid || undefined}
                  aria-describedby={invalid ? 'av-setup-error av-setup-hint' : 'av-setup-hint'}
                  data-testid="auto-view-seconds"
                  onChange={(e) => {
                    setDraft(e.target.value);
                    setInvalid(false);
                  }}
                />
                <span aria-hidden="true">seconds</span>
              </div>
              <div className="av-setup__presets" role="group" aria-label="Quick choices">
                {PRESETS.map((seconds) => (
                  <button
                    key={seconds}
                    type="button"
                    className="av-setup__preset"
                    aria-pressed={draft.trim() === String(seconds)}
                    onClick={() => {
                      setDraft(String(seconds));
                      setInvalid(false);
                    }}
                  >
                    {formatInterval(seconds)}
                  </button>
                ))}
              </div>
              <p id="av-setup-hint" className="av-setup__hint">
                Between {MIN_INTERVAL_SEC} and {MAX_INTERVAL_SEC} seconds. A carousel shares the time between its slides
                (at least 2 seconds each).
              </p>
              {invalid && (
                <p id="av-setup-error" className="av-setup__error" role="alert">
                  Enter a whole number of seconds from {MIN_INTERVAL_SEC} to {MAX_INTERVAL_SEC}.
                </p>
              )}
            </div>

            <label className="av-setup__check">
              <input type="checkbox" checked={waitForVideos} onChange={(e) => setWaitForVideos(e.target.checked)} />
              <span>Let videos play to the end<span className="av-setup__sub">A video stays until it ends when it is longer than the time per post.</span></span>
            </label>
            <label className="av-setup__check">
              <input type="checkbox" checked={loop} onChange={(e) => setLoop(e.target.checked)} />
              <span>Start again after the last post</span>
            </label>
            <label className="av-setup__check">
              <input type="checkbox" checked={shuffle} onChange={(e) => setShuffle(e.target.checked)} />
              <span>Shuffle the order</span>
            </label>
          </div>
          <button className="sheet__item sheet__item--accent" type="submit" disabled={empty} data-testid="auto-view-start">
            Start
          </button>
          <button className="sheet__item" type="button" onClick={onClose}>Cancel</button>
        </form>
      </div>
    </div>
  );
}

// --- Player ------------------------------------------------------------------------------------

export interface AutoViewPlayerProps {
  posts: Post[];
  /** Index in `posts` of the first post to show. */
  startIndex: number;
  settings: AutoViewSettings;
  /** The time per post was changed while running. */
  onSettingsChange?: (s: AutoViewSettings) => void;
  /** Index in `posts` of the post that was on screen. */
  onExit: (index: number) => void;
  onOpenPost?: (post: Post) => void;
}

const NO_VIDEO: VideoState = { durationSec: null, ended: false };

function PauseGlyph() {
  return (
    <svg width={22} height={22} viewBox="0 0 24 24" fill="currentColor" aria-hidden="true" focusable="false">
      <rect x="6" y="4.5" width="4" height="15" rx="1" />
      <rect x="14" y="4.5" width="4" height="15" rx="1" />
    </svg>
  );
}

/** Full-screen slideshow of the posts: one post at a time, moving on after the chosen time. */
export function AutoViewPlayer({
  posts, startIndex, settings, onSettingsChange, onExit, onOpenPost,
}: AutoViewPlayerProps): JSX.Element {
  const dialogRef = useDialogFocus<HTMLDivElement>(autoViewReturnTarget);
  const barRef = useRef<HTMLSpanElement>(null);
  const restartRef = useRef<HTMLButtonElement>(null);
  const playRef = useRef<HTMLButtonElement>(null);

  const ids = useMemo(() => posts.map((p) => p.id), [posts]);
  const byId = useMemo(() => new Map(posts.map((p) => [p.id, p])), [posts]);
  const idsKey = JSON.stringify(ids);
  const [state, dispatch] = useReducer(
    playerReducer,
    undefined,
    () => initPlayer(startCursor(ids, startIndex, settings.shuffle)),
  );
  // The posts changed while running (an import finished): follow them.
  const [syncedKey, setSyncedKey] = useState(idsKey);
  if (syncedKey !== idsKey) {
    setSyncedKey(idsKey);
    dispatch({ type: 'sync', ids });
  }

  // The time per post can change here (controls, keys) or from the parent (settings prop).
  const [intervalSec, setIntervalSec] = useState(settings.intervalSec);
  const [propInterval, setPropInterval] = useState(settings.intervalSec);
  if (propInterval !== settings.intervalSec) {
    setPropInterval(settings.intervalSec);
    setIntervalSec(settings.intervalSec);
  }

  const [hidden, setHidden] = useState(false);
  const { cursor, finished, paused, visit } = state;
  const total = cursor.order.length;
  const currentId = cursor.order[cursor.position];
  const post = currentId === undefined ? undefined : byId.get(currentId);
  const photos = post ? postPhotos(post) : [];
  const slideCount = photos.length;
  const slide = Math.min(state.slide, Math.max(0, slideCount - 1));
  const slideKey = `${visit}:${currentId ?? ''}:${slide}`;
  const active = !paused && !hidden && !finished && post !== undefined;

  // Length / end of the video on screen, for "let videos play to the end".
  const [video, setVideo] = useState<VideoState & { key: string }>({ ...NO_VIDEO, key: '' });
  const videoNow = video.key === slideKey ? video : NO_VIDEO;
  const plan = planSlide(
    slideSeconds(intervalSec, slideCount),
    !!photos[slide]?.video,
    settings.waitForVideos,
    videoNow,
  );
  const onVideoDuration = (seconds: number) => setVideo((v) => ({
    key: slideKey, durationSec: seconds, ended: v.key === slideKey && v.ended,
  }));
  const onVideoEnded = () => setVideo((v) => ({
    key: slideKey, durationSec: v.key === slideKey ? v.durationSec : null, ended: true,
  }));
  // A video that cannot be played has no end to wait for: the slide time applies.
  const onVideoFailed = () => setVideo({ key: slideKey, durationSec: null, ended: false });

  const advance = useRef<() => void>(() => undefined);
  useEffect(() => {
    advance.current = () => dispatch({ type: 'advance', slideCount, loop: settings.loop });
  });
  const [timer] = useState(() => new ActiveTimer(() => advance.current()));

  // A new slide starts from zero; a new due time (interval, video length) applies to the time already spent.
  const armedFor = useRef<string | null>(null);
  useEffect(() => {
    if (armedFor.current !== slideKey) {
      armedFor.current = slideKey;
      timer.reset();
    }
    timer.setDue(plan.dueMs);
  }, [timer, slideKey, plan.dueMs]);

  useEffect(() => {
    if (!active) {
      timer.pause();
      return undefined;
    }
    timer.resume();
    return () => timer.pause();
  }, [timer, active]);

  // The progress bar is painted from the timer on every frame, without re-rendering.
  useEffect(() => {
    const bar = barRef.current;
    if (!bar) return undefined;
    const paint = () => {
      bar.style.transform = `scaleX(${postProgress(slide, slideCount, timer.elapsedMs(), plan.expectedMs)})`;
    };
    paint();
    if (!active) return undefined;
    let frame = requestAnimationFrame(function tick() {
      paint();
      frame = requestAnimationFrame(tick);
    });
    return () => cancelAnimationFrame(frame);
  }, [timer, active, slideKey, slide, slideCount, plan.expectedMs]);

  useEffect(() => {
    const onVisibility = () => setHidden(document.hidden);
    onVisibility();
    document.addEventListener('visibilitychange', onVisibility);
    return () => document.removeEventListener('visibilitychange', onVisibility);
  }, []);

  // The buttons that had focus disappear with the end screen (and come back with Restart).
  const shownFinished = useRef(false);
  useEffect(() => {
    if (shownFinished.current === finished) return;
    shownFinished.current = finished;
    (finished ? restartRef : playRef).current?.focus();
  }, [finished]);

  const exit = () => onExit(Math.max(0, posts.findIndex((p) => p.id === currentId)));
  const changeInterval = (seconds: number) => {
    if (seconds === intervalSec) return;
    setIntervalSec(seconds);
    onSettingsChange?.({ ...settings, intervalSec: seconds });
  };
  const next = () => dispatch({ type: 'next', loop: settings.loop });
  const previous = () => dispatch({ type: 'previous', loop: settings.loop });
  const togglePause = () => dispatch({ type: 'togglePause' });

  const onKey = (e: KeyboardEvent) => {
    const target = e.target instanceof Element ? e.target : null;
    // Keys that belong to a field or to the video's own controls are left alone.
    const ownKeys = !!target?.closest('input, textarea, select, video, [contenteditable="true"]');
    const handled = () => e.preventDefault();
    switch (e.key) {
      case 'Escape':
        handled();
        exit();
        break;
      case ' ':
        if (ownKeys || target?.closest('button, a, summary') || finished) return;
        handled();
        togglePause();
        break;
      case 'ArrowRight':
        if (ownKeys) return;
        handled();
        next();
        break;
      case 'ArrowLeft':
        if (ownKeys) return;
        handled();
        previous();
        break;
      case '+':
      case '=':
        if (ownKeys) return;
        handled();
        changeInterval(stepInterval(intervalSec, 1));
        break;
      case '-':
      case '_':
        if (ownKeys) return;
        handled();
        changeInterval(stepInterval(intervalSec, -1));
        break;
    }
  };
  const latestKey = useRef(onKey);
  useEffect(() => {
    latestKey.current = onKey;
  });
  useEffect(() => {
    const listener = (e: KeyboardEvent) => {
      if (e.defaultPrevented || e.altKey || e.ctrlKey || e.metaKey || !isTopDialog(dialogRef.current)) return;
      latestKey.current(e);
    };
    window.addEventListener('keydown', listener);
    return () => window.removeEventListener('keydown', listener);
  }, [dialogRef]);

  const canGoBack = finished ? total > 0 : previousPosition(cursor.position, total, settings.loop) !== null;
  const caption = post ? post.caption.trim() : '';
  const preview = caption ? captionPreview(caption) : null;

  return (
    <div
      className="av"
      role="dialog"
      aria-modal="true"
      aria-label="Auto view"
      data-testid="auto-view"
      tabIndex={-1}
      ref={dialogRef}
    >
      <div className="av__progress" aria-hidden="true">
        {post && !finished && <span className="av__progress-bar" ref={barRef} data-testid="auto-view-progress" />}
      </div>

      <div className="av__top">
        <div className="av__author">
          {post && !finished && (
            <>
              <Avatar username={post.username} avatar={post.photo.key} src={post.avatarSrc} size={32} />
              <span className="av__username">{post.username}</span>
              {post.time && (
                <span className="av__time">
                  {post.takenAt ? <time dateTime={post.takenAt}>{post.time}</time> : post.time}
                </span>
              )}
            </>
          )}
        </div>
        {total > 0 && (
          <span className="av__counter" data-testid="auto-view-counter">
            {finished ? total : cursor.position + 1} / {total}
          </span>
        )}
        <button className="av__icon-btn" type="button" aria-label="Exit auto view" title="Exit (Esc)" onClick={exit}>
          <Icon name="close" size={24} />
        </button>
      </div>

      {total === 0 ? (
        <div className="av__end">
          <h2 className="av__end-title">There are no posts to show.</h2>
          <div className="av__end-actions">
            <button className="av__btn av__btn--ghost" type="button" onClick={exit}>Exit</button>
          </div>
        </div>
      ) : finished ? (
        <div className="av__end" data-testid="auto-view-end">
          <h2 className="av__end-title">End of posts</h2>
          <p className="av__end-text">You have seen all {total.toLocaleString('en-US')} {total === 1 ? 'post' : 'posts'}.</p>
          <div className="av__end-actions">
            <button className="av__btn av__btn--primary" type="button" ref={restartRef} onClick={() => dispatch({ type: 'restart' })}>
              Restart
            </button>
            <button className="av__btn av__btn--ghost" type="button" onClick={exit}>Exit</button>
          </div>
        </div>
      ) : (
        <>
          <div className="av__stage">
            {post && (
              <div className="av__media" key={`${visit}:${currentId}`} data-post-id={post.id}>
                <PostMedia
                  post={post}
                  variant="detail"
                  index={slide}
                  onIndexChange={(index) => dispatch({ type: 'slide', slide: index })}
                  autoPlay={active}
                  onVideoEnded={plan.listenForEnd ? onVideoEnded : undefined}
                  onVideoDuration={settings.waitForVideos ? onVideoDuration : undefined}
                  onVideoFailed={settings.waitForVideos ? onVideoFailed : undefined}
                >
                  {paused && <span className="av__paused" aria-hidden="true">Paused</span>}
                </PostMedia>
              </div>
            )}
          </div>

          {post && (
            <div className="av__info">
              {caption && (
                <p className="av__caption">
                  <span className="av__caption-user">{post.username}</span>{' '}
                  {preview ? `${preview.replace(/[\s.…]+$/u, '')}…` : caption}
                </p>
              )}
              <div className="av__meta">
                {post.likes !== null && (
                  <span>{formatCount(post.likes)} {post.likes === 1 ? 'like' : 'likes'}</span>
                )}
                <span>{formatCount(post.commentCount)} {post.commentCount === 1 ? 'comment' : 'comments'}</span>
                {post.permalink && (
                  <ExternalLink className="av__link" href={post.permalink} data-testid="auto-view-permalink">
                    <Icon name="external" size={14} /> View on {siteName(post.permalink)}
                  </ExternalLink>
                )}
                {onOpenPost && (
                  <button className="av__link" type="button" onClick={() => onOpenPost(post)}>Open post</button>
                )}
              </div>
            </div>
          )}
        </>
      )}

      {total > 0 && (
        <div className="av__controls" role="group" aria-label="Auto view controls">
          <button
            className="av__icon-btn"
            type="button"
            aria-label="Previous post"
            title="Previous (←)"
            disabled={!canGoBack}
            onClick={previous}
          >
            <Icon name="chevronLeft" size={24} strokeWidth={2.2} />
          </button>
          <button
            className="av__icon-btn av__play"
            type="button"
            ref={playRef}
            aria-label={paused ? 'Resume auto view' : 'Pause auto view'}
            title={paused ? 'Resume (Space)' : 'Pause (Space)'}
            disabled={finished}
            data-testid="auto-view-pause"
            onClick={togglePause}
          >
            {paused ? <Icon name="play" size={22} filled /> : <PauseGlyph />}
          </button>
          <button
            className="av__icon-btn"
            type="button"
            aria-label="Next post"
            title="Next (→)"
            disabled={finished}
            onClick={next}
          >
            <Icon name="chevronRight" size={24} strokeWidth={2.2} />
          </button>
          <div className="av__interval" role="group" aria-label="Time per post">
            <button
              className="av__step"
              type="button"
              aria-label="Less time per post"
              title="Less time (−)"
              disabled={intervalSec <= MIN_INTERVAL_SEC}
              onClick={() => changeInterval(stepInterval(intervalSec, -1))}
            >
              −
            </button>
            <output className="av__interval-value" aria-live="polite" data-testid="auto-view-interval">
              {formatInterval(intervalSec)}
            </output>
            <button
              className="av__step"
              type="button"
              aria-label="More time per post"
              title="More time (+)"
              disabled={intervalSec >= MAX_INTERVAL_SEC}
              onClick={() => changeInterval(stepInterval(intervalSec, 1))}
            >
              +
            </button>
          </div>
        </div>
      )}

      <p className="visually-hidden" aria-live="polite" aria-atomic="true" data-testid="auto-view-status">
        {announcement(state, post?.username)}
      </p>
    </div>
  );
}
