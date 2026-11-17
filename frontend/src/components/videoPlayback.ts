/**
 * Feed video bookkeeping without React: which of the feed's videos may play (only the most visible
 * one) and the shared on/off flags behind feed sound.
 */

export interface VisibilityEntry {
  target: Element;
  intersectionRatio: number;
  isIntersecting: boolean;
}

export interface ObserverLike {
  observe(target: Element): void;
  unobserve(target: Element): void;
}

export type ObserverFactory = (callback: (entries: readonly VisibilityEntry[]) => void) => ObserverLike;

/**
 * The length a video element reports, for auto view: seconds when known, Infinity when the video
 * plays but cannot tell (a WebM without a duration, until it ended), null when there is none.
 */
export function reportableDuration(duration: number): number | null {
  if (duration === Number.POSITIVE_INFINITY) return duration;
  return Number.isFinite(duration) && duration > 0 ? duration : null;
}

/** A video has to be at least this much on screen to play. */
export const MIN_VISIBLE_RATIO = 0.4;

/** Tracks the visibility of the watched elements and tells each one whether it is the active one. */
export class MostVisiblePicker {
  private readonly ratios = new Map<Element, number>();
  private readonly listeners = new Map<Element, (active: boolean) => void>();
  private observer: ObserverLike | null = null;
  private active: Element | null = null;

  constructor(
    private readonly createObserver: ObserverFactory,
    private readonly minRatio = MIN_VISIBLE_RATIO,
  ) {}

  /** Starts watching `target`; `listener` hears when it becomes / stops being the active one. */
  watch(target: Element, listener: (active: boolean) => void): () => void {
    this.observer ??= this.createObserver((entries) => this.update(entries));
    this.listeners.set(target, listener);
    this.ratios.set(target, 0);
    this.observer.observe(target);
    return () => {
      this.observer?.unobserve(target);
      this.listeners.delete(target);
      this.ratios.delete(target);
      if (this.active === target) {
        this.active = null;
        this.pick();
      }
    };
  }

  private update(entries: readonly VisibilityEntry[]): void {
    for (const entry of entries) {
      if (!this.ratios.has(entry.target)) continue;
      this.ratios.set(entry.target, entry.isIntersecting ? entry.intersectionRatio : 0);
    }
    this.pick();
  }

  private pick(): void {
    const bestRatio = Math.max(0, ...this.ratios.values());
    let best: Element | null = null;
    if (bestRatio >= this.minRatio) {
      // Map order is watch order: on a tie the first one wins, unless the one playing is among them.
      for (const [target, ratio] of this.ratios) {
        if (ratio === bestRatio && (best === null || target === this.active)) best = target;
      }
    }
    if (best === this.active) return;
    const previous = this.active;
    this.active = best;
    if (previous) this.listeners.get(previous)?.(false);
    if (best) this.listeners.get(best)?.(true);
  }
}

/** An on/off value that several components follow (e.g. the sound of feed videos: one switch for the whole feed). */
export class SharedFlag {
  private on = false;
  private readonly subscribers = new Set<() => void>();

  get = (): boolean => this.on;

  set = (on: boolean): void => {
    if (on === this.on) return;
    this.on = on;
    for (const notify of this.subscribers) notify();
  };

  subscribe = (notify: () => void): (() => void) => {
    this.subscribers.add(notify);
    return () => {
      this.subscribers.delete(notify);
    };
  };
}

const EXTENSIONS: Readonly<Record<string, string>> = {
  'video/mp4': 'mp4',
  'video/quicktime': 'mov',
  'video/webm': 'webm',
};

/** Name offered for "Download video": the file name of a library URL, else video.<ext>. */
export function downloadName(url: string, mime: string | undefined): string {
  const match = /^\/api\/library\/files\/(?:[^?#]*\/)?([^/?#]+)/.exec(url);
  if (match) {
    try {
      return decodeURIComponent(match[1]);
    } catch {
      // Fall through to the generic name.
    }
  }
  return `video.${(mime && EXTENSIONS[mime]) || 'mp4'}`;
}
