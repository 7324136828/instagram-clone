import { createContext, useContext, useEffect, useRef, useState, useSyncExternalStore } from 'react';
import type { Photo } from '../data';
import { Icon } from '../icons';
import { PhotoView } from './common';
import { downloadName, MostVisiblePicker, reportableDuration, SharedFlag } from './videoPlayback';

/** Feed videos play with sound once the user turned it on for one of them, like on Instagram. */
const feedSound = new SharedFlag();

let picker: MostVisiblePicker | null = null;

/** The one picker of the page; null where IntersectionObserver is missing (then nothing autoplays). */
function feedPicker(): MostVisiblePicker | null {
  if (typeof IntersectionObserver === 'undefined') return null;
  picker ??= new MostVisiblePicker((callback) => new IntersectionObserver(callback, {
    threshold: [0, 0.2, 0.4, 0.6, 0.8, 1],
  }));
  return picker;
}

/** False while something covers the feed (a post, a dialog, auto view): feed videos stay paused. */
export const FeedPlaybackContext = createContext(true);

export interface PostVideoProps {
  photo: Photo;
  /** The video URL (photo.video). */
  src: string;
  /** feed: no controls, plays while it is the most visible feed video, tap for sound. detail: controls. */
  variant: 'feed' | 'detail';
  /** The slide is the one on screen (carousels keep the others paused). */
  current: boolean;
  autoPlay: boolean;
  fit: 'cover' | 'contain';
  onRatio?: (ratio: number) => void;
  /** Reported for the current slide; when given in the detail variant the video does not loop. */
  onEnded?: () => void;
  /** Length of the current slide's video: seconds, or Infinity while it plays without a known length. */
  onDuration?: (seconds: number) => void;
  /** The current slide's video cannot be played (it shows its poster instead). */
  onFailed?: () => void;
}

/**
 * A video slide. Plays muted (autoplay rules), loops in the feed, and falls back to its poster with
 * a download link when the browser cannot play the file (for example HEVC in a .mov).
 */
export function PostVideo({
  photo, src, variant, current, autoPlay, fit, onRatio, onEnded, onDuration, onFailed,
}: PostVideoProps) {
  const ref = useRef<HTMLVideoElement>(null);
  const [failedSrc, setFailedSrc] = useState<string | null>(null);
  const [mostVisible, setMostVisible] = useState(false);
  const allowed = useContext(FeedPlaybackContext);
  const soundOn = useSyncExternalStore(feedSound.subscribe, feedSound.get);
  const feed = variant === 'feed';
  const failed = failedSrc === src;
  const shouldPlay = autoPlay && current && !failed && (!feed || (mostVisible && allowed));

  // Feed: only the current slide competes for "most visible".
  useEffect(() => {
    const node = ref.current;
    const visibility = feed && current && !failed ? feedPicker() : null;
    if (!node || !visibility) {
      setMostVisible(false);
      return undefined;
    }
    return visibility.watch(node, setMostVisible);
  }, [feed, current, failed]);

  useEffect(() => {
    const node = ref.current;
    if (node && feed) node.muted = !soundOn;
  }, [feed, soundOn]);

  useEffect(() => {
    const node = ref.current;
    if (!node) return;
    if (!shouldPlay) {
      node.pause();
      return;
    }
    node.play().catch(() => {
      // Autoplay with sound can be refused: retry muted. Otherwise it stays on its first frame.
      if (!node.muted && feed) {
        feedSound.set(false);
        node.muted = true;
        node.play().catch(() => undefined);
      }
    });
  }, [shouldPlay, feed]);

  // A slide whose metadata loaded before it came on screen reports its length now.
  const reportDuration = useRef(onDuration);
  const reportFailed = useRef(onFailed);
  useEffect(() => {
    reportDuration.current = onDuration;
    reportFailed.current = onFailed;
  });
  const report = (duration: number) => {
    const seconds = reportableDuration(duration);
    if (seconds !== null) reportDuration.current?.(seconds);
  };
  useEffect(() => {
    const node = ref.current;
    if (current && node && node.readyState >= 1) report(node.duration);
  }, [current]);
  // The slide on screen cannot be played (also when it failed before it came on screen).
  useEffect(() => {
    if (current && failed) reportFailed.current?.();
  }, [current, failed]);

  if (failed) {
    return (
      <div className="video video--failed" data-testid="video-unplayable">
        <PhotoView photo={{ key: photo.key, emoji: photo.emoji, src: photo.src, alt: photo.alt }} fill fit={fit} />
        <div className="video__failed">
          <p>This video can&apos;t be played in this browser.</p>
          <a className="video__download" href={src} download={downloadName(src, photo.videoMime)}>
            <Icon name="download" size={16} /> Download video
          </a>
        </div>
      </div>
    );
  }

  const toggleSound = () => feedSound.set(!soundOn);

  return (
    <div className={`video video--${variant}`}>
      <video
        ref={ref}
        className={`video__el video__el--${fit}`}
        src={src}
        poster={photo.src}
        aria-label={photo.alt}
        preload={feed ? 'metadata' : 'auto'}
        playsInline
        muted
        loop={feed || !onEnded}
        controls={!feed}
        // A slide that is not on screen is aria-hidden: its controls must not take focus either.
        tabIndex={current ? undefined : -1}
        data-testid="post-video"
        onClick={feed ? toggleSound : undefined}
        onLoadedMetadata={(e) => {
          const { videoWidth, videoHeight, duration } = e.currentTarget;
          // Metadata without a picture: the browser cannot decode the video track.
          if (videoWidth === 0 || videoHeight === 0) {
            setFailedSrc(src);
            return;
          }
          onRatio?.(videoWidth / videoHeight);
          if (current) report(duration);
        }}
        // A video without a length in its metadata learns it while playing (or once it ended).
        onDurationChange={(e) => { if (current && e.currentTarget.readyState >= 1) report(e.currentTarget.duration); }}
        onEnded={() => { if (current) onEnded?.(); }}
        onError={() => setFailedSrc(src)}
      />
      {feed && (
        <button
          className="video__sound"
          type="button"
          aria-label={soundOn ? 'Turn sound off' : 'Turn sound on'}
          data-testid="video-sound"
          onClick={toggleSound}
          onDoubleClick={(e) => e.stopPropagation()}
        >
          <Icon name={soundOn ? 'volume' : 'volumeOff'} size={16} strokeWidth={2.2} />
        </button>
      )}
    </div>
  );
}
