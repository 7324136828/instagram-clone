import { describe, expect, it } from 'vitest';
import {
  downloadName, MostVisiblePicker, reportableDuration, SharedFlag, type ObserverFactory, type VisibilityEntry,
} from './videoPlayback';

/** An IntersectionObserver stand-in: the test says how visible each element is. */
function fakeObserver() {
  let callback: ((entries: readonly VisibilityEntry[]) => void) | null = null;
  const observed = new Set<Element>();
  const factory: ObserverFactory = (cb) => {
    callback = cb;
    return { observe: (target) => observed.add(target), unobserve: (target) => observed.delete(target) };
  };
  const show = (...pairs: Array<[Element, number]>) => callback?.(pairs.map(([target, ratio]) => ({
    target, intersectionRatio: ratio, isIntersecting: ratio > 0,
  })));
  return { factory, show, observed };
}

const element = (name: string) => ({ name }) as unknown as Element;

function watchAll(picker: MostVisiblePicker, ...targets: Element[]) {
  const log: string[] = [];
  const stops = targets.map((target) => picker.watch(target, (active) => {
    log.push(`${(target as unknown as { name: string }).name}:${active ? 'play' : 'pause'}`);
  }));
  return { log, stops };
}

describe('only the most visible feed video plays', () => {
  it('plays the one that is most on screen and switches while scrolling', () => {
    const { factory, show } = fakeObserver();
    const picker = new MostVisiblePicker(factory, 0.4);
    const [a, b] = [element('a'), element('b')];
    const { log } = watchAll(picker, a, b);

    show([a, 0.9], [b, 0.1]);
    show([a, 0.6], [b, 0.45]);
    show([a, 0.3], [b, 0.8]);
    expect(log).toEqual(['a:play', 'a:pause', 'b:play']);
  });

  it('plays nothing while no video is visible enough', () => {
    const { factory, show } = fakeObserver();
    const picker = new MostVisiblePicker(factory, 0.4);
    const [a, b] = [element('a'), element('b')];
    const { log } = watchAll(picker, a, b);

    show([a, 0.39], [b, 0.2]);
    expect(log).toEqual([]);
    show([a, 0.5]);
    show([a, 0]);
    expect(log).toEqual(['a:play', 'a:pause']);
  });

  it('keeps the playing video on a tie instead of jumping to another one', () => {
    const { factory, show } = fakeObserver();
    const picker = new MostVisiblePicker(factory, 0.4);
    const [a, b] = [element('a'), element('b')];
    const { log } = watchAll(picker, a, b);

    show([b, 1]);
    show([a, 1]);
    expect(log).toEqual(['b:play']);
  });

  it('hands over to the next best one when the playing video goes away', () => {
    const { factory, show, observed } = fakeObserver();
    const picker = new MostVisiblePicker(factory, 0.4);
    const [a, b] = [element('a'), element('b')];
    const { log, stops } = watchAll(picker, a, b);

    show([a, 1], [b, 0.6]);
    stops[0]();
    expect(observed.has(a)).toBe(false);
    expect(log).toEqual(['a:play', 'b:play']);
  });

  it('ignores reports about elements it no longer watches', () => {
    const { factory, show } = fakeObserver();
    const picker = new MostVisiblePicker(factory, 0.4);
    const a = element('a');
    const { log, stops } = watchAll(picker, a);
    stops[0]();
    show([a, 1]);
    expect(log).toEqual([]);
  });
});

describe('a shared on/off flag', () => {
  it('starts muted and tells subscribers about changes only', () => {
    const sound = new SharedFlag();
    let calls = 0;
    const stop = sound.subscribe(() => { calls += 1; });
    expect(sound.get()).toBe(false);
    sound.set(true);
    sound.set(true);
    expect(sound.get()).toBe(true);
    expect(calls).toBe(1);
    stop();
    sound.set(false);
    expect(calls).toBe(1);
  });
});

describe('the name of a downloaded video', () => {
  it.each([
    ['/api/library/files/posts/C5xYz_1.mov?r=4', 'video/quicktime', 'C5xYz_1.mov'],
    ['/api/library/files/posts/a%20b.mp4', 'video/mp4', 'a b.mp4'],
    ['/api/library/files/posts/bad%E0.mp4', 'video/mp4', 'video.mp4'],
    ['blob:http://localhost/1234', 'video/webm', 'video.webm'],
    ['blob:http://localhost/1234', 'video/quicktime', 'video.mov'],
    ['blob:http://localhost/1234', undefined, 'video.mp4'],
  ])('%s (%s) is saved as %s', (url, mime, name) => {
    expect(downloadName(url, mime)).toBe(name);
  });
});

describe('the length a video reports to auto view', () => {
  it('passes a known length on, Infinity (plays, length unknown) too, and drops what is no length', () => {
    expect(reportableDuration(8.5)).toBe(8.5);
    expect(reportableDuration(Number.POSITIVE_INFINITY)).toBe(Number.POSITIVE_INFINITY);
    for (const value of [Number.NaN, 0, -1, Number.NEGATIVE_INFINITY]) expect(reportableDuration(value)).toBeNull();
  });
});
