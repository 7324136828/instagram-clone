import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import { afterEach, describe, expect, it, vi } from 'vitest';
import {
  ActiveTimer, announcement, AUTO_VIEW_STORAGE_KEY, clampInterval, DEFAULT_AUTO_VIEW, formatInterval, initPlayer,
  loadAutoViewSettings, MAX_INTERVAL_SEC, MIN_INTERVAL_SEC, nextPosition, normalizeSettings, parseAutoViewHash,
  parseIntervalInput, planSlide, playerReducer, postProgress, previousPosition, reconcileCursor,
  saveAutoViewSettings, slideSeconds, startCursor, stepInterval, UNKNOWN_LENGTH_MAX_SEC, videoDeadlineSec,
  type PlayerAction, type PlayerState,
} from './autoView';
import { AutoViewPlayer, AutoViewSetup } from './components/AutoView';
import type { Post } from './data';

afterEach(() => {
  vi.unstubAllGlobals();
  vi.useRealTimers();
});

describe('interval', () => {
  it('clamps to whole seconds between the limits and falls back to the default for non-numbers', () => {
    expect(clampInterval(15)).toBe(15);
    expect(clampInterval(14.6)).toBe(15);
    expect(clampInterval(0)).toBe(MIN_INTERVAL_SEC);
    expect(clampInterval(-30)).toBe(MIN_INTERVAL_SEC);
    expect(clampInterval(99_999)).toBe(MAX_INTERVAL_SEC);
    expect(clampInterval(Number.NaN)).toBe(DEFAULT_AUTO_VIEW.intervalSec);
    expect(clampInterval(Number.POSITIVE_INFINITY)).toBe(DEFAULT_AUTO_VIEW.intervalSec);
  });

  it('steps through the preset values, also from a value between two presets', () => {
    expect(stepInterval(15, 1)).toBe(20);
    expect(stepInterval(15, -1)).toBe(10);
    expect(stepInterval(7, 1)).toBe(8);
    expect(stepInterval(7, -1)).toBe(5);
    expect(stepInterval(MIN_INTERVAL_SEC, -1)).toBe(MIN_INTERVAL_SEC);
    expect(stepInterval(MAX_INTERVAL_SEC, 1)).toBe(MAX_INTERVAL_SEC);
    expect(stepInterval(2000, 1)).toBe(MAX_INTERVAL_SEC);
  });

  it('formats seconds, minutes and the hour', () => {
    expect(formatInterval(15)).toBe('15 s');
    expect(formatInterval(60)).toBe('1 min');
    expect(formatInterval(90)).toBe('1 min 30 s');
    expect(formatInterval(MAX_INTERVAL_SEC)).toBe('1 h');
    expect(formatInterval(1)).toBe('2 s');
  });

  it('accepts only whole seconds within the limits as typed input', () => {
    expect(parseIntervalInput(' 15 ')).toBe(15);
    expect(parseIntervalInput('2')).toBe(2);
    expect(parseIntervalInput('3600')).toBe(3600);
    for (const bad of ['', 'abc', '1', '3601', '2.5', '-5', '1e3', '15s']) {
      expect(parseIntervalInput(bad)).toBeNull();
    }
  });
});

describe('settings persistence', () => {
  function memoryStorage(initial: Record<string, string> = {}) {
    const data = new Map(Object.entries(initial));
    return {
      data,
      getItem: (key: string) => data.get(key) ?? null,
      setItem: (key: string, value: string) => {
        data.set(key, value);
      },
    };
  }

  it('round-trips the settings through localStorage', () => {
    const storage = memoryStorage();
    vi.stubGlobal('localStorage', storage);
    saveAutoViewSettings({ intervalSec: 30, waitForVideos: false, loop: false, shuffle: true });
    expect(JSON.parse(storage.data.get(AUTO_VIEW_STORAGE_KEY) ?? '')).toEqual({
      intervalSec: 30, waitForVideos: false, loop: false, shuffle: true,
    });
    expect(loadAutoViewSettings()).toEqual({ intervalSec: 30, waitForVideos: false, loop: false, shuffle: true });
  });

  it('returns a fresh copy of the defaults when nothing is stored', () => {
    vi.stubGlobal('localStorage', memoryStorage());
    const loaded = loadAutoViewSettings();
    expect(loaded).toEqual(DEFAULT_AUTO_VIEW);
    expect(loaded).not.toBe(DEFAULT_AUTO_VIEW);
  });

  it('repairs damaged or partial stored values field by field', () => {
    vi.stubGlobal('localStorage', memoryStorage({ [AUTO_VIEW_STORAGE_KEY]: '{not json' }));
    expect(loadAutoViewSettings()).toEqual(DEFAULT_AUTO_VIEW);
    vi.stubGlobal('localStorage', memoryStorage({
      [AUTO_VIEW_STORAGE_KEY]: JSON.stringify({ intervalSec: 1e9, loop: 'yes', shuffle: true }),
    }));
    expect(loadAutoViewSettings()).toEqual({ ...DEFAULT_AUTO_VIEW, intervalSec: MAX_INTERVAL_SEC, shuffle: true });
    expect(normalizeSettings(null)).toEqual(DEFAULT_AUTO_VIEW);
    expect(normalizeSettings({ intervalSec: '20' })).toEqual(DEFAULT_AUTO_VIEW);
  });

  it('survives a localStorage that throws on every access', () => {
    vi.stubGlobal('localStorage', {
      getItem: () => { throw new Error('SecurityError'); },
      setItem: () => { throw new Error('QuotaExceededError'); },
    });
    expect(loadAutoViewSettings()).toEqual(DEFAULT_AUTO_VIEW);
    expect(() => saveAutoViewSettings({ ...DEFAULT_AUTO_VIEW, intervalSec: 5 })).not.toThrow();
  });

  it('survives a localStorage property whose getter throws (blocked site data)', () => {
    const original = Object.getOwnPropertyDescriptor(globalThis, 'localStorage');
    Object.defineProperty(globalThis, 'localStorage', {
      configurable: true,
      get() { throw new Error('SecurityError'); },
    });
    try {
      expect(loadAutoViewSettings()).toEqual(DEFAULT_AUTO_VIEW);
      expect(() => saveAutoViewSettings(DEFAULT_AUTO_VIEW)).not.toThrow();
    } finally {
      if (original) Object.defineProperty(globalThis, 'localStorage', original);
      else Reflect.deleteProperty(globalThis, 'localStorage');
    }
  });
});

describe('parseAutoViewHash', () => {
  it('is null when the hash does not ask for auto view', () => {
    expect(parseAutoViewHash('')).toBeNull();
    expect(parseAutoViewHash('#')).toBeNull();
    expect(parseAutoViewHash('#profile')).toBeNull();
    expect(parseAutoViewHash('#loop=0')).toBeNull();
    expect(parseAutoViewHash('#autoviewer=15')).toBeNull();
  });

  it('reads the interval and the flags', () => {
    expect(parseAutoViewHash('#autoview')).toEqual({});
    expect(parseAutoViewHash('#autoview=')).toEqual({});
    expect(parseAutoViewHash('#autoview=15')).toEqual({ intervalSec: 15 });
    expect(parseAutoViewHash('autoview=30')).toEqual({ intervalSec: 30 });
    expect(parseAutoViewHash('#autoview=15s')).toEqual({ intervalSec: 15 });
    expect(parseAutoViewHash('#autoview=15&loop=0')).toEqual({ intervalSec: 15, loop: false });
    expect(parseAutoViewHash('#autoview=10&shuffle=1&wait=off&loop=true')).toEqual({
      intervalSec: 10, shuffle: true, waitForVideos: false, loop: true,
    });
    expect(parseAutoViewHash('#loop=no&AutoView=8&waitForVideos=yes')).toEqual({
      intervalSec: 8, loop: false, waitForVideos: true,
    });
    // A flag without a value switches it on.
    expect(parseAutoViewHash('#autoview&shuffle')).toEqual({ shuffle: true });
  });

  it('clamps the interval and ignores values it cannot read', () => {
    expect(parseAutoViewHash('#autoview=1')).toEqual({ intervalSec: MIN_INTERVAL_SEC });
    expect(parseAutoViewHash('#autoview=100000')).toEqual({ intervalSec: MAX_INTERVAL_SEC });
    expect(parseAutoViewHash('#autoview=abc&loop=maybe')).toEqual({});
    expect(parseAutoViewHash('#autoview=-5')).toEqual({});
  });
});

describe('timing', () => {
  it('shares the interval between carousel slides, at least 2 s per slide', () => {
    expect(slideSeconds(15, 1)).toBe(15);
    expect(slideSeconds(15, 3)).toBe(5);
    expect(slideSeconds(15, 10)).toBe(2);
    expect(slideSeconds(4, 3)).toBe(2);
    expect(slideSeconds(15, 0)).toBe(15);
  });

  it('gives pictures and videos (when not waiting for them) the slide time, video plays in a loop', () => {
    const ended = { durationSec: 40, ended: false };
    expect(planSlide(15, false, true, ended)).toEqual({ dueMs: 15_000, expectedMs: 15_000, listenForEnd: false });
    expect(planSlide(15, true, false, ended)).toEqual({ dueMs: 15_000, expectedMs: 15_000, listenForEnd: false });
  });

  it('keeps a longer video until it ends, with a safety deadline', () => {
    const plan = planSlide(15, true, true, { durationSec: 40, ended: false });
    expect(plan).toEqual({ dueMs: videoDeadlineSec(40) * 1000, expectedMs: 40_000, listenForEnd: true });
    expect(videoDeadlineSec(40)).toBe(50);
    expect(videoDeadlineSec(8)).toBe(13);
  });

  it('moves on at the slide time once the video ended', () => {
    expect(planSlide(15, true, true, { durationSec: 40, ended: true }))
      .toEqual({ dueMs: 15_000, expectedMs: 15_000, listenForEnd: true });
  });

  it('lets a shorter video loop until the slide time is up', () => {
    expect(planSlide(15, true, true, { durationSec: 6, ended: false }))
      .toEqual({ dueMs: 15_000, expectedMs: 15_000, listenForEnd: false });
  });

  it('waits for the end of a playing video whose length is unknown, up to a cap', () => {
    const unknown = { durationSec: Number.POSITIVE_INFINITY, ended: false };
    expect(planSlide(3, true, true, unknown))
      .toEqual({ dueMs: UNKNOWN_LENGTH_MAX_SEC * 1000, expectedMs: 3_000, listenForEnd: true });
    expect(planSlide(3, true, true, { ...unknown, ended: true }))
      .toEqual({ dueMs: 3_000, expectedMs: 3_000, listenForEnd: true });
    expect(planSlide(3, true, false, unknown))
      .toEqual({ dueMs: 3_000, expectedMs: 3_000, listenForEnd: false });
    expect(planSlide(3600, true, true, unknown).dueMs).toBe(3_600_000);
  });

  it('falls back to the slide time when the length is missing or unusable', () => {
    for (const durationSec of [null, Number.NaN, Number.NEGATIVE_INFINITY, 0]) {
      expect(planSlide(15, true, true, { durationSec, ended: false }))
        .toEqual({ dueMs: 15_000, expectedMs: 15_000, listenForEnd: true });
    }
  });

  it('reports the progress of the whole post', () => {
    expect(postProgress(0, 1, 0, 10_000)).toBe(0);
    expect(postProgress(0, 1, 5_000, 10_000)).toBe(0.5);
    expect(postProgress(0, 1, 25_000, 10_000)).toBe(1);
    expect(postProgress(1, 4, 2_500, 5_000)).toBeCloseTo(0.375);
    expect(postProgress(9, 2, 0, 5_000)).toBe(0.5);
    expect(postProgress(0, 1, 100, 0)).toBe(1);
  });
});

describe('order and navigation', () => {
  const ids = ['a', 'b', 'c', 'd', 'e'];

  it('keeps the feed order and starts at the requested post', () => {
    expect(startCursor(ids, 2, false)).toEqual({ order: ids, position: 2 });
    expect(startCursor(ids, 99, false)).toEqual({ order: ids, position: 4 });
    expect(startCursor(ids, -3, false)).toEqual({ order: ids, position: 0 });
    expect(startCursor([], 3, false)).toEqual({ order: [], position: 0 });
  });

  it('shuffles with the start post first and every post exactly once', () => {
    const cursor = startCursor(ids, 2, true, () => 0);
    expect(cursor.position).toBe(0);
    expect(cursor.order[0]).toBe('c');
    expect([...cursor.order].sort()).toEqual(ids);
    expect(cursor.order).not.toEqual(['c', 'a', 'b', 'd', 'e']);
    // The random source is used, so another one gives another order.
    expect(startCursor(ids, 2, true, () => 0.999).order).not.toEqual(cursor.order);
  });

  it('follows removed and added posts and keeps the post on screen', () => {
    const cursor = { order: ['a', 'b', 'c', 'd'], position: 2 };
    expect(reconcileCursor(cursor, ['d', 'c', 'b', 'a'])).toBe(cursor);
    expect(reconcileCursor(cursor, ['a', 'c', 'd', 'x'])).toEqual({ order: ['a', 'c', 'd', 'x'], position: 1 });
    // The post on screen was removed: the one that took its place is shown.
    expect(reconcileCursor(cursor, ['a', 'b', 'd'])).toEqual({ order: ['a', 'b', 'd'], position: 2 });
    expect(reconcileCursor(cursor, ['a', 'd'])).toEqual({ order: ['a', 'd'], position: 1 });
    expect(reconcileCursor({ order: ['a', 'b'], position: 1 }, ['a'])).toEqual({ order: ['a'], position: 0 });
    expect(reconcileCursor(cursor, [])).toEqual({ order: [], position: 0 });
  });

  it('wraps around only when looping', () => {
    expect(nextPosition(1, 3, false)).toBe(2);
    expect(nextPosition(2, 3, false)).toBeNull();
    expect(nextPosition(2, 3, true)).toBe(0);
    expect(nextPosition(0, 0, true)).toBeNull();
    expect(previousPosition(1, 3, false)).toBe(0);
    expect(previousPosition(0, 3, false)).toBeNull();
    expect(previousPosition(0, 3, true)).toBe(2);
    expect(previousPosition(0, 0, true)).toBeNull();
  });
});

describe('playerReducer', () => {
  const run = (state: PlayerState, ...actions: PlayerAction[]) => actions.reduce(playerReducer, state);
  const start = (order: string[], position = 0) => initPlayer({ order, position });
  const at = (state: PlayerState) => ({
    id: state.cursor.order[state.cursor.position], slide: state.slide, finished: state.finished,
  });

  it('steps through the slides of a carousel, then to the next post', () => {
    let state = start(['a', 'b']);
    state = run(state, { type: 'advance', slideCount: 3, loop: true });
    expect(at(state)).toEqual({ id: 'a', slide: 1, finished: false });
    state = run(state, { type: 'advance', slideCount: 3, loop: true }, { type: 'advance', slideCount: 3, loop: true });
    expect(at(state)).toEqual({ id: 'b', slide: 0, finished: false });
    expect(state.visit).toBe(1);
  });

  it('loops to the first post, or stops on the end screen', () => {
    const last = start(['a', 'b'], 1);
    expect(at(run(last, { type: 'advance', slideCount: 1, loop: true }))).toEqual({ id: 'a', slide: 0, finished: false });
    const ended = run(last, { type: 'advance', slideCount: 1, loop: false });
    expect(ended.finished).toBe(true);
    // Nothing moves on from the end screen by itself, and pausing does not apply there.
    expect(run(ended, { type: 'advance', slideCount: 1, loop: false }, { type: 'next', loop: false })).toBe(ended);
    expect(run(ended, { type: 'togglePause' })).toBe(ended);
    // Previous goes back to the last post; Restart to the first.
    expect(at(run(ended, { type: 'previous', loop: false }))).toEqual({ id: 'b', slide: 0, finished: false });
    expect(at(run(ended, { type: 'restart' }))).toEqual({ id: 'a', slide: 0, finished: false });
  });

  it('restarts a single post when it loops onto itself', () => {
    const state = run(start(['only']), { type: 'slide', slide: 1 }, { type: 'advance', slideCount: 2, loop: true });
    expect(at(state)).toEqual({ id: 'only', slide: 0, finished: false });
    expect(state.visit).toBe(1);
  });

  it('goes back only when there is somewhere to go', () => {
    const first = start(['a', 'b']);
    expect(run(first, { type: 'previous', loop: false })).toBe(first);
    expect(at(run(first, { type: 'previous', loop: true }))).toEqual({ id: 'b', slide: 0, finished: false });
  });

  it('keeps the pause across posts and clears it on restart', () => {
    const paused = run(start(['a', 'b']), { type: 'togglePause' }, { type: 'next', loop: true });
    expect(paused.paused).toBe(true);
    expect(at(paused).id).toBe('b');
    const restarted = run(paused, { type: 'next', loop: false }, { type: 'restart' });
    expect(restarted.paused).toBe(false);
    expect(run(paused, { type: 'togglePause' }).paused).toBe(false);
  });

  it('follows the posts: same post keeps its slide, a replaced one starts over', () => {
    const state = run(start(['a', 'b', 'c'], 1), { type: 'slide', slide: 2 });
    expect(run(state, { type: 'sync', ids: ['c', 'b', 'a'] })).toBe(state);
    const added = run(state, { type: 'sync', ids: ['a', 'b', 'c', 'd'] });
    expect(at(added)).toEqual({ id: 'b', slide: 2, finished: false });
    expect(added.visit).toBe(state.visit);
    const removed = run(state, { type: 'sync', ids: ['a', 'c'] });
    expect(at(removed)).toEqual({ id: 'c', slide: 0, finished: false });
    expect(removed.visit).toBe(state.visit + 1);
  });

  it('announces the position, the pause and the end', () => {
    const state = start(['a', 'b', 'c'], 1);
    expect(announcement(state, 'someone')).toBe('Post 2 of 3, by someone.');
    expect(announcement(state, undefined)).toBe('Post 2 of 3.');
    expect(announcement({ ...state, paused: true }, 'someone')).toBe('Auto view paused.');
    expect(announcement({ ...state, finished: true }, 'someone')).toBe('End of posts.');
    expect(announcement(start([]), undefined)).toBe('There are no posts to show.');
  });
});

describe('ActiveTimer', () => {
  function setup() {
    vi.useFakeTimers();
    let now = 0;
    const onDue = vi.fn();
    const timer = new ActiveTimer(onDue, () => now);
    const advance = (ms: number) => {
      now += ms;
      vi.advanceTimersByTime(ms);
    };
    return { timer, onDue, advance };
  }

  it('fires once when the active time reaches the due time', () => {
    const { timer, onDue, advance } = setup();
    timer.resume();
    timer.setDue(15_000);
    advance(14_999);
    expect(onDue).not.toHaveBeenCalled();
    advance(1);
    expect(onDue).toHaveBeenCalledTimes(1);
    advance(60_000);
    expect(onDue).toHaveBeenCalledTimes(1);
    expect(vi.getTimerCount()).toBe(0);
  });

  it('keeps the remaining time exactly across pause and resume, with no timer while paused', () => {
    const { timer, onDue, advance } = setup();
    timer.resume();
    timer.setDue(10_000);
    advance(4_000);
    timer.pause();
    expect(timer.running).toBe(false);
    expect(vi.getTimerCount()).toBe(0);
    advance(60_000);
    expect(timer.elapsedMs()).toBe(4_000);
    expect(onDue).not.toHaveBeenCalled();
    timer.resume();
    timer.resume();
    advance(5_999);
    expect(onDue).not.toHaveBeenCalled();
    advance(1);
    expect(onDue).toHaveBeenCalledTimes(1);
  });

  it('applies a new due time to the time already spent', () => {
    const { timer, onDue, advance } = setup();
    timer.resume();
    timer.setDue(30_000);
    advance(10_000);
    timer.setDue(20_000);
    advance(9_999);
    expect(onDue).not.toHaveBeenCalled();
    advance(1);
    expect(onDue).toHaveBeenCalledTimes(1);

    // A due time that already passed fires right away, but not synchronously.
    timer.reset();
    advance(8_000);
    timer.setDue(5_000);
    expect(onDue).toHaveBeenCalledTimes(1);
    advance(0);
    expect(onDue).toHaveBeenCalledTimes(2);
  });

  it('does not fire while paused, even when the due time is already reached', () => {
    const { timer, onDue, advance } = setup();
    timer.resume();
    advance(8_000);
    timer.pause();
    timer.setDue(5_000);
    advance(10_000);
    expect(onDue).not.toHaveBeenCalled();
    timer.resume();
    advance(0);
    expect(onDue).toHaveBeenCalledTimes(1);
  });

  it('starts a new slide from zero and waits for its due time', () => {
    const { timer, onDue, advance } = setup();
    timer.resume();
    timer.setDue(5_000);
    advance(4_000);
    timer.reset();
    expect(timer.elapsedMs()).toBe(0);
    expect(vi.getTimerCount()).toBe(0);
    advance(10_000);
    expect(onDue).not.toHaveBeenCalled();
    timer.setDue(5_000);
    advance(5_000);
    expect(onDue).toHaveBeenCalledTimes(1);
  });

  it('waits again when the timeout fires before the clock reached the due time', () => {
    vi.useFakeTimers();
    let now = 0;
    const onDue = vi.fn();
    const timer = new ActiveTimer(onDue, () => now);
    timer.resume();
    timer.setDue(1_000);
    now = 900;
    vi.advanceTimersByTime(1_000);
    expect(onDue).not.toHaveBeenCalled();
    now = 1_000;
    vi.advanceTimersByTime(100);
    expect(onDue).toHaveBeenCalledTimes(1);
  });
});

// The test environment has no DOM: the dialogs are rendered to static markup (effects, the timer and
// key handling do not run there; the e2e suite drives them in a browser).
describe('dialogs (static render)', () => {
  const noop = () => undefined;
  const post = (id: string, extra: Partial<Post> = {}): Post => ({
    id, username: `user_${id}`, time: '2d', photo: { key: 'ocean', emoji: '🌊' }, likes: 1234, caption: `Caption of ${id}`,
    comments: [], commentCount: 1, reposts: 0, shares: 0, imported: true, ...extra,
  });
  const setup = (props: Partial<Parameters<typeof AutoViewSetup>[0]> = {}) => renderToStaticMarkup(createElement(AutoViewSetup, {
    open: true, settings: DEFAULT_AUTO_VIEW, postCount: 3, onStart: noop, onClose: noop, ...props,
  }));
  const player = (props: Partial<Parameters<typeof AutoViewPlayer>[0]> = {}) => renderToStaticMarkup(createElement(AutoViewPlayer, {
    posts: [post('a'), post('b'), post('c')], startIndex: 0, settings: DEFAULT_AUTO_VIEW, onExit: noop, ...props,
  }));

  it('renders nothing while the setup is closed', () => {
    expect(setup({ open: false })).toBe('');
  });

  it('starts the setup form from the current settings', () => {
    const html = setup({ settings: { intervalSec: 30, waitForVideos: false, loop: true, shuffle: true } });
    expect(html).toContain('role="dialog"');
    expect(html).toContain('aria-modal="true"');
    expect(html).toMatch(/<input id="av-setup-seconds"[^>]*value="30"/);
    expect(html).toMatch(/aria-pressed="true"[^>]*>30 s</);
    expect(html).toMatch(/<input type="checkbox"\/><span>Let videos play/);
    expect(html).toMatch(/<input type="checkbox" checked=""\/><span>Start again/);
    expect(html).toMatch(/<input type="checkbox" checked=""\/><span>Shuffle/);
    expect(html).toContain('the 3 posts one after another');
    expect(html).not.toMatch(/type="submit" disabled/);
  });

  it('cannot start without posts', () => {
    const html = setup({ postCount: 0 });
    expect(html).toContain('There are no posts to show yet.');
    expect(html).toMatch(/type="submit" disabled=""/);
  });

  it('shows the start post with its author, caption, counts, counter and link', () => {
    const html = player({
      posts: [post('a'), post('b', { permalink: 'https://www.instagram.com/p/b/', likes: null }), post('c')],
      startIndex: 1,
      onOpenPost: noop,
    });
    expect(html).toContain('aria-label="Auto view"');
    expect(html).toMatch(/data-testid="auto-view-counter">2 \/ 3</);
    expect(html).toContain('data-post-id="b"');
    expect(html).toContain('Caption of b');
    expect(html).not.toContain('likes<');
    expect(html).toContain('1 comment<');
    expect(html).toMatch(/href="https:\/\/www.instagram.com\/p\/b\/" target="_blank" rel="noopener noreferrer"/);
    expect(html).toContain('>Open post<');
    expect(html).toContain('Post 2 of 3, by user_b.');
    expect(html).toMatch(/data-testid="auto-view-interval">15 s</);
    expect(html).toContain('aria-label="Pause auto view"');
    // The progress bar is decorative.
    expect(html).toMatch(/class="av__progress" aria-hidden="true"/);
  });

  it('shows the start post first when shuffled', () => {
    const posts = ['a', 'b', 'c', 'd', 'e', 'f'].map((id) => post(id));
    const html = player({ posts, startIndex: 4, settings: { ...DEFAULT_AUTO_VIEW, shuffle: true } });
    expect(html).toContain('data-post-id="e"');
    expect(html).toMatch(/data-testid="auto-view-counter">1 \/ 6</);
  });

  it('disables Previous on the first post unless looping', () => {
    expect(player({ settings: { ...DEFAULT_AUTO_VIEW, loop: false } })).toMatch(/aria-label="Previous post"[^>]*disabled=""/);
    expect(player()).not.toMatch(/aria-label="Previous post"[^>]*disabled=""/);
  });

  it('says so when there is nothing to show', () => {
    const html = player({ posts: [] });
    expect(html).toContain('There are no posts to show.');
    expect(html).not.toContain('auto-view-pause');
  });
});
