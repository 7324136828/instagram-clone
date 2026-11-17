/**
 * Auto view: a slideshow that shows the posts one after another. This module is the pure part
 * (settings, the "#autoview" hash, timing rules, navigation state and the pausable timer); the
 * dialogs are in components/AutoView.tsx.
 */

export interface AutoViewSettings {
  /** Seconds each post stays on screen (a carousel shares them between its slides). */
  intervalSec: number;
  /** A video post stays until its video has played to the end, even when that is longer. */
  waitForVideos: boolean;
  /** Start again from the first post after the last one; otherwise stop on an end screen. */
  loop: boolean;
  /** Show the posts in a random order, fixed when auto view starts. */
  shuffle: boolean;
}

export const DEFAULT_AUTO_VIEW: AutoViewSettings = { intervalSec: 15, waitForVideos: true, loop: true, shuffle: false };

export const MIN_INTERVAL_SEC = 2;
export const MAX_INTERVAL_SEC = 3600;

/** A carousel slide is never shown for less than this; the post gets more time instead. */
export const MIN_SLIDE_SEC = 2;

export const AUTO_VIEW_STORAGE_KEY = 'ig-clone-auto-view';

/** The values the "shorter / longer" controls step through. */
export const INTERVAL_STEPS: readonly number[] = [
  2, 3, 5, 8, 10, 15, 20, 30, 45, 60, 90, 120, 180, 300, 600, 900, 1800, 3600,
];

/** Whole seconds between MIN_INTERVAL_SEC and MAX_INTERVAL_SEC; anything that is not a number gives the default. */
export function clampInterval(n: number): number {
  if (!Number.isFinite(n)) return DEFAULT_AUTO_VIEW.intervalSec;
  return Math.min(MAX_INTERVAL_SEC, Math.max(MIN_INTERVAL_SEC, Math.round(n)));
}

/** The next value of INTERVAL_STEPS above (direction 1) or below (-1) the current interval. */
export function stepInterval(current: number, direction: 1 | -1): number {
  const value = clampInterval(current);
  if (direction > 0) return INTERVAL_STEPS.find((step) => step > value) ?? MAX_INTERVAL_SEC;
  for (let i = INTERVAL_STEPS.length - 1; i >= 0; i -= 1) {
    if (INTERVAL_STEPS[i] < value) return INTERVAL_STEPS[i];
  }
  return MIN_INTERVAL_SEC;
}

/** "15 s", "1 min", "1 min 30 s", "1 h". */
export function formatInterval(seconds: number): string {
  const total = clampInterval(seconds);
  if (total < 60) return `${total} s`;
  if (total >= 3600) return '1 h';
  const minutes = Math.floor(total / 60);
  const rest = total % 60;
  return rest === 0 ? `${minutes} min` : `${minutes} min ${rest} s`;
}

/**
 * The seconds typed in the setup dialog, or null when the text is not a whole number of seconds
 * between MIN_INTERVAL_SEC and MAX_INTERVAL_SEC (the user is told instead of being silently corrected).
 */
export function parseIntervalInput(text: string): number | null {
  const match = /^\s*(\d+)\s*$/.exec(text);
  if (!match) return null;
  const seconds = Number(match[1]);
  return seconds >= MIN_INTERVAL_SEC && seconds <= MAX_INTERVAL_SEC ? seconds : null;
}

/** Settings read from storage or a caller: every field is checked, unknown or broken ones get the default. */
export function normalizeSettings(value: unknown): AutoViewSettings {
  const source = typeof value === 'object' && value !== null ? value as Record<string, unknown> : {};
  const flag = (key: keyof AutoViewSettings, fallback: boolean) => (typeof source[key] === 'boolean' ? source[key] as boolean : fallback);
  return {
    intervalSec: typeof source.intervalSec === 'number' ? clampInterval(source.intervalSec) : DEFAULT_AUTO_VIEW.intervalSec,
    waitForVideos: flag('waitForVideos', DEFAULT_AUTO_VIEW.waitForVideos),
    loop: flag('loop', DEFAULT_AUTO_VIEW.loop),
    shuffle: flag('shuffle', DEFAULT_AUTO_VIEW.shuffle),
  };
}

/** The settings used last time; the defaults when there are none or storage is unavailable. */
export function loadAutoViewSettings(): AutoViewSettings {
  try {
    const raw = globalThis.localStorage.getItem(AUTO_VIEW_STORAGE_KEY);
    return raw ? normalizeSettings(JSON.parse(raw)) : { ...DEFAULT_AUTO_VIEW };
  } catch {
    // No storage (private mode, blocked site data, not a browser) or a damaged value.
    return { ...DEFAULT_AUTO_VIEW };
  }
}

/** Remembers the settings for next time; silently does nothing when storage is unavailable or full. */
export function saveAutoViewSettings(s: AutoViewSettings): void {
  try {
    globalThis.localStorage.setItem(AUTO_VIEW_STORAGE_KEY, JSON.stringify(normalizeSettings(s)));
  } catch {
    // Not remembering the settings is harmless.
  }
}

const TRUE_WORDS = new Set(['', '1', 'true', 'yes', 'on']);
const FALSE_WORDS = new Set(['0', 'false', 'no', 'off']);

function parseFlag(value: string): boolean | undefined {
  const word = value.trim().toLowerCase();
  if (TRUE_WORDS.has(word)) return true;
  if (FALSE_WORDS.has(word)) return false;
  return undefined;
}

/**
 * Reads an auto view request from the address hash: "#autoview" (saved settings), "#autoview=15"
 * (15 seconds per post), "#autoview=15&loop=0&shuffle=1&wait=0". Null when the hash does not ask for
 * auto view. Values that cannot be read are left out, so the saved settings apply to them.
 */
export function parseAutoViewHash(hash: string): Partial<AutoViewSettings> | null {
  const body = hash.startsWith('#') ? hash.slice(1) : hash;
  const params = new Map<string, string>();
  for (const [key, value] of new URLSearchParams(body)) {
    const name = key.trim().toLowerCase();
    if (!params.has(name)) params.set(name, value);
  }
  const interval = params.get('autoview');
  if (interval === undefined) return null;

  const result: Partial<AutoViewSettings> = {};
  const seconds = /^\s*(\d+(?:\.\d+)?)\s*s?\s*$/i.exec(interval);
  if (seconds) result.intervalSec = clampInterval(Number(seconds[1]));
  const flags: Array<[string, 'loop' | 'shuffle' | 'waitForVideos']> = [
    ['loop', 'loop'], ['shuffle', 'shuffle'], ['wait', 'waitForVideos'], ['waitforvideos', 'waitForVideos'],
  ];
  for (const [key, field] of flags) {
    const raw = params.get(key);
    const value = raw === undefined ? undefined : parseFlag(raw);
    if (value !== undefined && result[field] === undefined) result[field] = value;
  }
  return result;
}

// --- Timing -------------------------------------------------------------------------------------

/** Seconds per carousel slide: the interval shared evenly, but never less than MIN_SLIDE_SEC. */
export function slideSeconds(intervalSec: number, slideCount: number): number {
  const slides = Math.max(1, Math.floor(slideCount));
  return Math.max(MIN_SLIDE_SEC, clampInterval(intervalSec) / slides);
}

/**
 * How long past its end a video may take before auto view moves on anyway (buffering, a video
 * paused with its own controls, a decode error after the length was known).
 */
export function videoDeadlineSec(durationSec: number): number {
  return durationSec + Math.max(5, durationSec * 0.25);
}

/**
 * The longest auto view waits for the end of a video that plays but has no known length (a WebM
 * recorded in a browser often has none until it ended). Its "ended" event normally comes first.
 */
export const UNKNOWN_LENGTH_MAX_SEC = 600;

export interface VideoState {
  /**
   * Length reported by the video; null until known (or when it never is, or the video failed).
   * Infinity: the video plays but cannot tell its length (it is only known once it ended).
   */
  durationSec: number | null;
  /** The video played to its end. */
  ended: boolean;
}

export interface SlidePlan {
  /** Active (not paused) milliseconds after which the slide is done. */
  dueMs: number;
  /** What the progress bar counts down: the time the slide is expected to take. */
  expectedMs: number;
  /**
   * Auto view needs the video's "ended" event (the video then plays once instead of looping).
   * False when the video is shorter than the slide time: it simply loops until the time is up.
   */
  listenForEnd: boolean;
}

/**
 * When the current slide is done. A picture (or a video when waitForVideos is off) gets its slide
 * time. With waitForVideos a video stays until the later of the slide time and its end; a video
 * that plays without a known length (Infinity) stays until it ends, up to UNKNOWN_LENGTH_MAX_SEC;
 * when there is no length at all (not loaded yet, or it failed) the slide time applies.
 */
export function planSlide(slideSec: number, isVideo: boolean, waitForVideos: boolean, video: VideoState): SlidePlan {
  const slideMs = slideSec * 1000;
  const plain = { dueMs: slideMs, expectedMs: slideMs };
  if (!isVideo || !waitForVideos) return { ...plain, listenForEnd: false };
  if (!video.ended && video.durationSec === Number.POSITIVE_INFINITY) {
    return { dueMs: Math.max(slideMs, UNKNOWN_LENGTH_MAX_SEC * 1000), expectedMs: slideMs, listenForEnd: true };
  }
  const duration = video.durationSec !== null && Number.isFinite(video.durationSec) && video.durationSec > 0
    ? video.durationSec
    : null;
  if (video.ended || duration === null) return { ...plain, listenForEnd: true };
  if (duration * 1000 < slideMs) return { ...plain, listenForEnd: false };
  return { dueMs: videoDeadlineSec(duration) * 1000, expectedMs: duration * 1000, listenForEnd: true };
}

/** Share of the post that has been shown (0..1): finished slides plus the part of the current one. */
export function postProgress(slide: number, slideCount: number, elapsedMs: number, expectedMs: number): number {
  const slides = Math.max(1, slideCount);
  const done = Math.min(Math.max(slide, 0), slides - 1);
  const part = expectedMs > 0 ? Math.min(1, Math.max(0, elapsedMs / expectedMs)) : 1;
  return Math.min(1, (done + part) / slides);
}

// --- Order and navigation -----------------------------------------------------------------------

export interface AutoViewCursor {
  /** Post ids in the order they are shown. */
  order: readonly string[];
  /** Index in `order` of the post on screen. */
  position: number;
}

/**
 * The order for a new auto view. In feed order it starts at `startIndex`; shuffled, the start post
 * comes first and the others follow in a random order.
 */
export function startCursor(
  ids: readonly string[], startIndex: number, shuffle: boolean, random: () => number = Math.random,
): AutoViewCursor {
  const start = ids.length === 0 ? 0 : Math.min(Math.max(Math.floor(startIndex) || 0, 0), ids.length - 1);
  if (!shuffle || ids.length < 2) return { order: [...ids], position: start };
  const rest = ids.filter((_, i) => i !== start);
  for (let i = rest.length - 1; i > 0; i -= 1) {
    const j = Math.min(i, Math.floor(random() * (i + 1)));
    [rest[i], rest[j]] = [rest[j], rest[i]];
  }
  return { order: [ids[start], ...rest], position: 0 };
}

/**
 * Follows a change of the posts while auto view runs (an import finished, a post was removed):
 * removed posts leave the order, new ones are added at the end, and the post on screen stays on
 * screen when it still exists (otherwise the one that took its place is shown).
 */
export function reconcileCursor(cursor: AutoViewCursor, ids: readonly string[]): AutoViewCursor {
  const present = new Set(ids);
  const kept = cursor.order.filter((id) => present.has(id));
  const known = new Set(kept);
  const order = [...kept, ...ids.filter((id) => !known.has(id))];
  if (order.length === cursor.order.length && order.every((id, i) => id === cursor.order[i])) return cursor;
  const current = cursor.order[cursor.position];
  const at = current === undefined ? -1 : order.indexOf(current);
  if (at >= 0) return { order, position: at };
  const removedBefore = cursor.order.slice(0, cursor.position).filter((id) => !present.has(id)).length;
  return { order, position: Math.max(0, Math.min(cursor.position - removedBefore, order.length - 1)) };
}

/** The position after `position`, or null at the end when not looping. */
export function nextPosition(position: number, length: number, loop: boolean): number | null {
  if (length === 0) return null;
  if (position + 1 < length) return position + 1;
  return loop ? 0 : null;
}

/** The position before `position`, or null at the start when not looping. */
export function previousPosition(position: number, length: number, loop: boolean): number | null {
  if (length === 0) return null;
  if (position > 0) return position - 1;
  return loop ? length - 1 : null;
}

export interface PlayerState {
  cursor: AutoViewCursor;
  /** Carousel slide on screen. */
  slide: number;
  /** Counts post changes, so that showing the same post again (a loop of one) restarts it. */
  visit: number;
  /** The last post was shown and loop is off: the end screen is up. */
  finished: boolean;
  /** Paused by the user (a hidden tab pauses too, but that is not part of this state). */
  paused: boolean;
}

export type PlayerAction =
  | { type: 'next'; loop: boolean }
  | { type: 'previous'; loop: boolean }
  /** The time of the current slide is up: next slide, else next post. */
  | { type: 'advance'; slideCount: number; loop: boolean }
  | { type: 'slide'; slide: number }
  | { type: 'togglePause' }
  | { type: 'restart' }
  | { type: 'sync'; ids: readonly string[] };

export function initPlayer(cursor: AutoViewCursor): PlayerState {
  return { cursor, slide: 0, visit: 0, finished: false, paused: false };
}

function showPosition(state: PlayerState, position: number): PlayerState {
  return { ...state, cursor: { ...state.cursor, position }, slide: 0, visit: state.visit + 1, finished: false };
}

export function playerReducer(state: PlayerState, action: PlayerAction): PlayerState {
  const length = state.cursor.order.length;
  switch (action.type) {
    case 'next': {
      if (state.finished) return state;
      const next = nextPosition(state.cursor.position, length, action.loop);
      return next === null ? { ...state, finished: true } : showPosition(state, next);
    }
    case 'previous': {
      // From the end screen, back to the last post.
      if (state.finished) return length === 0 ? state : showPosition(state, length - 1);
      const previous = previousPosition(state.cursor.position, length, action.loop);
      return previous === null ? state : showPosition(state, previous);
    }
    case 'advance':
      if (state.finished) return state;
      if (state.slide + 1 < action.slideCount) return { ...state, slide: state.slide + 1 };
      return playerReducer(state, { type: 'next', loop: action.loop });
    case 'slide':
      return action.slide === state.slide ? state : { ...state, slide: Math.max(0, action.slide) };
    case 'togglePause':
      return state.finished ? state : { ...state, paused: !state.paused };
    case 'restart':
      return length === 0 ? state : { ...showPosition(state, 0), paused: false };
    case 'sync': {
      const cursor = reconcileCursor(state.cursor, action.ids);
      if (cursor === state.cursor) return state;
      const samePost = cursor.order[cursor.position] === state.cursor.order[state.cursor.position];
      return samePost ? { ...state, cursor } : { ...state, cursor, slide: 0, visit: state.visit + 1 };
    }
  }
}

/** What the screen reader hears when the post changes (or auto view is paused / at the end). */
export function announcement(state: PlayerState, username: string | undefined): string {
  const total = state.cursor.order.length;
  if (total === 0) return 'There are no posts to show.';
  if (state.finished) return 'End of posts.';
  if (state.paused) return 'Auto view paused.';
  const position = `Post ${state.cursor.position + 1} of ${total}`;
  return username ? `${position}, by ${username}.` : `${position}.`;
}

// --- Timer --------------------------------------------------------------------------------------

/**
 * Counts the active (not paused) time of the current slide and calls `onDue` once when it reaches
 * the due time. Pausing keeps the elapsed time exactly; the due time can change at any moment (a
 * new interval, a video length) and applies to the time already elapsed. Uses one setTimeout at a
 * time, none while paused or idle.
 */
export class ActiveTimer {
  private accumulatedMs = 0;
  private startedAt: number | null = null;
  private dueMs = Number.POSITIVE_INFINITY;
  private handle: ReturnType<typeof setTimeout> | null = null;

  constructor(
    private readonly onDue: () => void,
    private readonly now: () => number = () => performance.now(),
  ) {}

  get running(): boolean {
    return this.startedAt !== null;
  }

  elapsedMs(): number {
    return this.accumulatedMs + (this.startedAt === null ? 0 : this.now() - this.startedAt);
  }

  /** A new slide: elapsed time back to zero, no due time until setDue. Keeps running / paused. */
  reset(): void {
    this.accumulatedMs = 0;
    if (this.startedAt !== null) this.startedAt = this.now();
    this.dueMs = Number.POSITIVE_INFINITY;
    this.schedule();
  }

  /** Fires `onDue` once the elapsed time reaches `ms` (right away, asynchronously, when it already has). */
  setDue(ms: number): void {
    this.dueMs = ms;
    this.schedule();
  }

  pause(): void {
    if (this.startedAt === null) return;
    this.accumulatedMs = this.elapsedMs();
    this.startedAt = null;
    this.schedule();
  }

  resume(): void {
    if (this.startedAt !== null) return;
    this.startedAt = this.now();
    this.schedule();
  }

  private schedule(): void {
    if (this.handle !== null) {
      clearTimeout(this.handle);
      this.handle = null;
    }
    if (this.startedAt === null || !Number.isFinite(this.dueMs)) return;
    const wait = Math.max(0, this.dueMs - this.elapsedMs());
    this.handle = setTimeout(() => {
      this.handle = null;
      const remaining = this.dueMs - this.elapsedMs();
      // A clock that disagrees with the timer by a hair must not fire early (or not at all).
      if (remaining > 1) {
        this.schedule();
        return;
      }
      this.dueMs = Number.POSITIVE_INFINITY;
      this.onDue();
    }, wait);
  }
}
