// The callbacks passed to page.evaluate() run in the browser, so this file needs the DOM types
// (the e2e tsconfig only has the Node ones).
/// <reference lib="dom" />
/// <reference lib="dom.iterable" />
import type { Locator, Page } from '@playwright/test';
import {
  SEED_USER, banner, card, expect, expectImportSuccess, fixture, openImportDialog, test, viewFeed,
} from './support/app';

/**
 * Video posts (fixtures/export-videos.zip, recorded by fixtures/record-videos.ts):
 *   FXTvideo001  WebM with a poster (orange)        FXTvideo002  MP4 without a poster (violet)
 *   FXTvideo003  carousel: picture, then WebM (teal) FXTvideo004  .mov the browser cannot play
 *   FXTvideo005  a plain picture
 * Browser mode (see support/app.ts); tests/library.spec.ts plays videos served by the library.
 */

interface VideoState {
  paused: boolean;
  muted: boolean;
  time: number;
  controls: boolean;
  loop: boolean;
  ready: number;
  src: string;
}

const videoState = (video: Locator) => video.evaluate((el: HTMLVideoElement): VideoState => ({
  paused: el.paused,
  muted: el.muted,
  time: el.currentTime,
  controls: el.controls,
  loop: el.loop,
  ready: el.readyState,
  src: el.getAttribute('src') ?? '',
}));

/** Videos of the page that are playing right now. */
const playingCount = (page: Page) => page.evaluate(
  () => Array.from(document.querySelectorAll('video')).filter((el) => !el.paused && !el.ended).length,
);

async function importVideos(page: Page): Promise<void> {
  await page.goto('/');
  await expect(page.getByText(SEED_USER).first()).toBeVisible();
  await openImportDialog(page);
  await page.getByTestId('import-archive-input').setInputFiles(fixture('export-videos.zip'));
  await expectImportSuccess(page, 'Imported 5 of 5 posts from export-videos.zip');
  await expect(page.getByTestId('import-warnings')).toHaveCount(0);
  await viewFeed(page);
}

async function expectPlaying(video: Locator): Promise<void> {
  await expect.poll(async () => {
    const state = await videoState(video);
    return !state.paused && state.time > 0.2;
  }, { message: 'video should be playing', timeout: 15_000 }).toBe(true);
}

test.describe('videos', () => {
  test('the visible feed video plays muted, alone, and pauses off screen; the speaker turns sound on', async ({ page }) => {
    await importVideos(page);
    await expect(banner(page)).toContainText('Showing 5 posts imported from export-videos.zip');

    const first = card(page, 'FXTvideo001').getByTestId('post-video');
    await expect(first).toHaveAttribute('poster', /^blob:/);
    await expect(first).toHaveAttribute('aria-label', 'Orange test clip with a moving white disc');
    await expectPlaying(first);
    const state = await videoState(first);
    expect(state).toMatchObject({ muted: true, controls: false, loop: true });
    expect(state.src).toMatch(/^blob:/);
    expect(await playingCount(page)).toBe(1);

    // Sound: the speaker button, and the same switch for the next video.
    const speaker = card(page, 'FXTvideo001').getByTestId('video-sound');
    await expect(speaker).toHaveAccessibleName('Turn sound on');
    await speaker.click();
    await expect(speaker).toHaveAccessibleName('Turn sound off');
    expect((await videoState(first)).muted).toBe(false);

    // The next video takes over once it is the most visible one; the first one pauses.
    const second = card(page, 'FXTvideo002').getByTestId('post-video');
    await page.evaluate(() => document.querySelector('[data-post-id="imp:FXTvideo002"] video')?.scrollIntoView({ block: 'center' }));
    await expectPlaying(second);
    await expect.poll(async () => (await videoState(first)).paused).toBe(true);
    expect((await videoState(second)).muted).toBe(false);
    expect(await playingCount(page)).toBe(1);

    // Clicking the video itself toggles the sound too.
    await second.click();
    await expect(card(page, 'FXTvideo002').getByTestId('video-sound')).toHaveAccessibleName('Turn sound on');
    expect((await videoState(second)).muted).toBe(true);

    // Nothing plays while a post is open on top of the feed.
    await card(page, 'FXTvideo002').getByRole('button', { name: 'Comment', exact: true }).click();
    const detail = page.getByRole('dialog', { name: 'Post by clip_fixture_demo' });
    await expect(detail).toBeVisible();
    await expect.poll(async () => (await videoState(second)).paused).toBe(true);
  });

  test('the post detail plays a video with controls, also inside a carousel', async ({ page }) => {
    await importVideos(page);
    await card(page, 'FXTvideo003').getByRole('button', { name: 'Comment', exact: true }).click();
    const detail = page.getByRole('dialog', { name: 'Post by fixture.video.studio' });
    await expect(detail).toBeVisible();

    // Slide 1 is a picture; slide 2 the video, which starts when it comes on screen.
    const video = detail.getByTestId('post-video');
    await expect(detail.getByTestId('carousel-counter')).toHaveText('1/2');
    expect((await videoState(video)).paused).toBe(true);
    await detail.getByRole('button', { name: 'Next photo' }).click();
    await expect(detail.getByTestId('carousel-counter')).toHaveText('2/2');
    await expectPlaying(video);
    expect(await videoState(video)).toMatchObject({ controls: true, muted: true });
    await expect(video).toHaveAttribute('aria-label', 'Video 2 of 2 by fixture.video.studio');

    // Back to the picture: the video stops.
    await detail.getByRole('button', { name: 'Previous photo' }).click();
    await expect.poll(async () => (await videoState(video)).paused).toBe(true);
  });

  test('a video the browser cannot play shows its placeholder and a download link', async ({ page }) => {
    await importVideos(page);
    const broken = card(page, 'FXTvideo004');
    await broken.scrollIntoViewIfNeeded();
    const fallback = broken.getByTestId('video-unplayable');
    await expect(fallback).toBeVisible();
    await expect(fallback).toContainText("This video can't be played in this browser.");
    const download = fallback.getByRole('link', { name: 'Download video' });
    await expect(download).toHaveAttribute('href', /^blob:/);
    await expect(download).toHaveAttribute('download', 'video.mov');
    await expect(broken.getByTestId('post-video')).toHaveCount(0);

    const [file] = await Promise.all([page.waitForEvent('download'), download.click()]);
    expect(file.suggestedFilename()).toBe('video.mov');
  });

  test('grid tiles show the poster or the first frame, with the clip badge', async ({ page }) => {
    await importVideos(page);
    await page.getByRole('navigation', { name: 'Main navigation' }).getByRole('button', { name: 'Explore' }).click();
    const tiles = page.locator('.explore__grid .tile');
    await expect(tiles).toHaveCount(5);

    // Poster: a picture; no poster: a paused video that shows its first frame.
    const withPoster = tiles.nth(0);
    await expect(withPoster.locator('img.photo__img')).toHaveAttribute('src', /^blob:/);
    await expect(withPoster.getByRole('img', { name: 'Video' })).toBeVisible();
    const frameOnly = tiles.nth(1).locator('video.photo__img');
    await expect(frameOnly).toHaveCount(1);
    await expect.poll(async () => (await videoState(frameOnly)).ready).toBeGreaterThanOrEqual(2);
    expect((await videoState(frameOnly)).paused).toBe(true);
    await expect(tiles.nth(1).getByRole('img', { name: 'Video' })).toBeVisible();
    // The carousel that mixes a picture and a video is a carousel.
    await expect(tiles.nth(2).getByRole('img', { name: 'Carousel' })).toBeVisible();
    expect(await playingCount(page)).toBe(0);
  });
});
