// The callbacks passed to page.evaluate() run in the browser, so this file needs the DOM types.
/// <reference lib="dom" />
import type { Page } from '@playwright/test';
import {
  SEED_USER, banner, card, expect, expectImportSuccess, fixture, openImportDialog, test, viewFeed,
} from './support/app';

/**
 * Auto view (frontend/src/components/AutoView.tsx): how it is reached from the app (the banner,
 * the post action sheet, the More menu and the #autoview hash) and how the player behaves: it
 * moves on by itself, pauses, steps, waits for videos, stops at the end and gives focus back.
 */

/** The open auto view (the player or its setup): the dialog on top. */
const autoViewDialog = (page: Page) => page.locator('[role="dialog"][aria-modal="true"]').last();
const player = (page: Page) => page.getByTestId('auto-view');
const counter = (page: Page) => page.getByTestId('auto-view-counter');
const setupDialog = (page: Page) => page.getByTestId('auto-view-setup');
const anyDialog = (page: Page) => page.locator('[role="dialog"][aria-modal="true"]');

async function importArchive(page: Page, file: string, statusText: string): Promise<void> {
  await page.goto('/');
  await expect(page.getByText(SEED_USER).first()).toBeVisible();
  await openImportDialog(page);
  await page.getByTestId('import-archive-input').setInputFiles(fixture(file));
  await expectImportSuccess(page, statusText);
  await viewFeed(page);
}

const importBasic = (page: Page) => importArchive(page, 'export-basic.zip', 'Imported 6 of 6 posts from export-basic.zip');

/** Starts auto view from the setup dialog that is open, with the given time per post. */
async function startFromSetup(page: Page, seconds: number): Promise<void> {
  await expect(setupDialog(page)).toBeVisible();
  await page.getByTestId('auto-view-seconds').fill(String(seconds));
  await page.getByTestId('auto-view-start').click();
  await expect(setupDialog(page)).toHaveCount(0);
  await expect(player(page)).toBeVisible();
}

interface TimelineEntry {
  t: number;
  what: string;
}

/**
 * Records, with the page's clock, every change of the counter and every video of the player that
 * ends. Time is measured in the browser so the test's own round trips do not blur it.
 */
async function recordTimeline(page: Page): Promise<void> {
  await page.evaluate(() => {
    const entries: TimelineEntry[] = [];
    (window as unknown as { avTimeline: TimelineEntry[] }).avTimeline = entries;
    const log = (what: string) => entries.push({ t: performance.now(), what });
    const node = document.querySelector('[data-testid="auto-view-counter"]');
    if (!node) throw new Error('auto view is not running');
    log(`counter ${node.textContent ?? ''}`);
    new MutationObserver(() => log(`counter ${node.textContent ?? ''}`))
      .observe(node, { characterData: true, childList: true, subtree: true });
    // 'ended' does not bubble, but a capturing listener on the document sees it.
    document.addEventListener('ended', (e) => {
      if (e.target instanceof Element && e.target.closest('[data-testid="auto-view"]')) log('ended');
    }, true);
  });
}

const timeline = (page: Page) =>
  page.evaluate(() => (window as unknown as { avTimeline: TimelineEntry[] }).avTimeline);

/** When (page clock) the counter first read `text`. */
async function counterAt(page: Page, text: string): Promise<number> {
  const entry = (await timeline(page)).find((e) => e.what === `counter ${text}`);
  if (!entry) throw new Error(`the counter never read ${text}`);
  return entry.t;
}

test.describe('auto view', () => {
  test('started from the banner with 3 seconds per post: moves on, pauses, steps and gives focus back', async ({ page }) => {
    await importBasic(page);
    const opener = banner(page).getByRole('button', { name: 'Auto view' });
    await opener.click();
    await expect(page.getByTestId('auto-view-seconds')).toBeFocused();
    await startFromSetup(page, 3);

    await expect(counter(page)).toHaveText('1 / 6');
    await expect(page.getByTestId('auto-view-interval')).toHaveText('3 s');
    await expect(player(page).locator('[data-post-id="imp:FXTbasic001"]')).toBeVisible();
    await expect(page.getByTestId('auto-view-status')).toHaveText('Post 1 of 6, by fixture.sunrise.bakery.');
    // The progress bar fills up with the time spent on the post.
    const progress = () => page.getByTestId('auto-view-progress')
      .evaluate((el) => new DOMMatrixReadOnly(getComputedStyle(el).transform).a);
    await expect.poll(progress).toBeGreaterThan(0.2);

    // It moves on by itself, every 3 seconds.
    await recordTimeline(page);
    await expect(counter(page)).toHaveText('2 / 6', { timeout: 6_000 });
    await expect(counter(page)).toHaveText('3 / 6', { timeout: 6_000 });
    const step = (await counterAt(page, '3 / 6')) - (await counterAt(page, '2 / 6'));
    expect(step).toBeGreaterThan(2_800);
    expect(step).toBeLessThan(4_500);
    await expect(player(page).locator('[data-post-id="imp:FXTbasic003"]')).toBeVisible();

    // Pause: nothing moves, the progress bar stands still.
    const pause = page.getByTestId('auto-view-pause');
    await expect(pause).toHaveAccessibleName('Pause auto view');
    await pause.click();
    await expect(pause).toHaveAccessibleName('Resume auto view');
    await expect(page.getByTestId('auto-view-status')).toHaveText('Auto view paused.');
    const frozen = await progress();
    await page.waitForTimeout(4_000);
    await expect(counter(page)).toHaveText('3 / 6');
    expect(await progress()).toBeCloseTo(frozen, 3);

    // Next / Previous, with the buttons and the arrow keys (still paused, so nothing else moves).
    await player(page).getByRole('button', { name: 'Next post' }).click();
    await expect(counter(page)).toHaveText('4 / 6');
    await player(page).getByRole('button', { name: 'Previous post' }).click();
    await expect(counter(page)).toHaveText('3 / 6');
    await page.keyboard.press('ArrowRight');
    await expect(counter(page)).toHaveText('4 / 6');
    await page.keyboard.press('ArrowLeft');
    await expect(counter(page)).toHaveText('3 / 6');
    await expect(pause).toHaveAccessibleName('Resume auto view');

    // Space resumes it, and it moves on again.
    // (Space on a focused button presses that button, so it goes to the player itself.)
    await player(page).focus();
    await page.keyboard.press('Space');
    await expect(pause).toHaveAccessibleName('Pause auto view');
    await expect(counter(page)).toHaveText('4 / 6', { timeout: 6_000 });

    // Escape ends it and gives focus back to the button that opened it.
    await page.keyboard.press('Escape');
    await expect(anyDialog(page)).toHaveCount(0);
    await expect(opener).toBeFocused();
  });

  test('changing the time per post while running applies at once and only that is remembered', async ({ page }) => {
    await importBasic(page);
    await page.evaluate(() => { window.location.hash = '#autoview=2&loop=0'; });
    await expect(counter(page)).toHaveText('1 / 6');
    await expect(page.getByTestId('auto-view-interval')).toHaveText('2 s');

    await player(page).getByRole('button', { name: 'More time per post' }).click();
    await expect(page.getByTestId('auto-view-interval')).toHaveText('3 s');
    await page.keyboard.press('+');
    await expect(page.getByTestId('auto-view-interval')).toHaveText('5 s');
    await page.keyboard.press('-');
    await expect(page.getByTestId('auto-view-interval')).toHaveText('3 s');

    // The next posts get the new time.
    await recordTimeline(page);
    await expect(counter(page)).toHaveText('2 / 6', { timeout: 6_000 });
    await expect(counter(page)).toHaveText('3 / 6', { timeout: 6_000 });
    const step = (await counterAt(page, '3 / 6')) - (await counterAt(page, '2 / 6'));
    expect(step).toBeGreaterThan(2_800);
    expect(step).toBeLessThan(4_500);

    await player(page).getByRole('button', { name: 'Exit auto view' }).click();
    await expect(anyDialog(page)).toHaveCount(0);
    expect(new URL(page.url()).hash).toBe('');

    // The setup offers the new time, but "loop=0" was for that run only.
    await banner(page).getByRole('button', { name: 'Auto view' }).click();
    await expect(page.getByTestId('auto-view-seconds')).toHaveValue('3');
    await expect(setupDialog(page).getByRole('checkbox', { name: 'Start again after the last post' })).toBeChecked();
  });

  test('with "Start again" off, the last post leads to the end screen', async ({ page }) => {
    await importBasic(page);
    await page.evaluate(() => { window.location.hash = '#autoview=2&loop=0'; });
    await expect(counter(page)).toHaveText('1 / 6');
    // Previous has nowhere to go from the first post.
    await expect(player(page).getByRole('button', { name: 'Previous post' })).toBeDisabled();
    for (let i = 2; i <= 6; i += 1) {
      await page.keyboard.press('ArrowRight');
      await expect(counter(page)).toHaveText(`${i} / 6`);
    }

    const end = page.getByTestId('auto-view-end');
    await expect(end).toBeVisible({ timeout: 6_000 });
    await expect(end).toContainText('End of posts');
    await expect(end).toContainText('You have seen all 6 posts.');
    await expect(page.getByTestId('auto-view-status')).toHaveText('End of posts.');
    await expect(end.getByRole('button', { name: 'Restart' })).toBeFocused();
    await expect(page.getByTestId('auto-view-pause')).toBeDisabled();

    await end.getByRole('button', { name: 'Restart' }).click();
    await expect(counter(page)).toHaveText('1 / 6');
    await expect(page.getByTestId('auto-view-pause')).toBeFocused();

    // Exit from the end screen.
    for (let i = 2; i <= 6; i += 1) await page.keyboard.press('ArrowRight');
    await expect(end).toBeVisible({ timeout: 6_000 });
    await end.getByRole('button', { name: 'Exit' }).click();
    await expect(anyDialog(page)).toHaveCount(0);
    await expect(banner(page)).toBeVisible();
  });

  test('"Let videos play to the end" keeps a video on screen until it has ended', async ({ page }) => {
    await importArchive(page, 'export-videos.zip', 'Imported 5 of 5 posts from export-videos.zip');
    // FXTvideo003: a picture, then a 3 second video. With 2 seconds per post each slide gets 2 seconds,
    // but the video is longer than that, so it stays until its end.
    await card(page, 'FXTvideo003').getByRole('button', { name: 'More options' }).click();
    await page.getByTestId('post-options').getByRole('button', { name: 'Auto view from here' }).click();
    await expect(setupDialog(page).getByRole('checkbox', { name: /Let videos play to the end/ })).toBeChecked();
    await startFromSetup(page, 2);
    await expect(counter(page)).toHaveText('3 / 5');
    await recordTimeline(page);

    const video = player(page).getByTestId('post-video');
    await expect(video).toBeVisible({ timeout: 5_000 });
    await expect.poll(() => video.evaluate((el: HTMLVideoElement) => !el.paused && el.muted && el.currentTime > 0), { timeout: 5_000 })
      .toBe(true);
    await expect(counter(page)).toHaveText('4 / 5', { timeout: 10_000 });

    const entries = await timeline(page);
    const ended = entries.find((e) => e.what === 'ended');
    const start = entries[0].t;
    const moved = await counterAt(page, '4 / 5');
    expect(ended, 'the video played to its end').toBeDefined();
    expect(moved).toBeGreaterThanOrEqual(ended!.t);
    // 2 s for the picture + 3 s of video, not 2 + 2.
    expect(moved - start).toBeGreaterThan(4_700);
  });

  test('a video that does not know its length (WebM from a recorder) is also played to the end', async ({ page }) => {
    // Such a video reports Infinity as its duration until it has ended (Chromium, Firefox).
    await page.addInitScript(() => {
      const real = Object.getOwnPropertyDescriptor(HTMLMediaElement.prototype, 'duration');
      Object.defineProperty(HTMLMediaElement.prototype, 'duration', {
        configurable: true,
        get(this: HTMLMediaElement) {
          const value = real?.get?.call(this) as number;
          return this.ended || Number.isNaN(value) ? value : Number.POSITIVE_INFINITY;
        },
      });
    });
    await importArchive(page, 'export-videos.zip', 'Imported 5 of 5 posts from export-videos.zip');
    await card(page, 'FXTvideo003').getByRole('button', { name: 'More options' }).click();
    await page.getByTestId('post-options').getByRole('button', { name: 'Auto view from here' }).click();
    await startFromSetup(page, 2);
    await expect(counter(page)).toHaveText('3 / 5');
    await recordTimeline(page);

    const video = player(page).getByTestId('post-video');
    await expect(video).toBeVisible({ timeout: 5_000 });
    expect(await video.evaluate((el: HTMLVideoElement) => el.duration)).toBe(Number.POSITIVE_INFINITY);
    await expect(counter(page)).toHaveText('4 / 5', { timeout: 10_000 });

    const entries = await timeline(page);
    const ended = entries.find((e) => e.what === 'ended');
    expect(ended, 'the video played to its end').toBeDefined();
    expect(await counterAt(page, '4 / 5')).toBeGreaterThanOrEqual(ended!.t);
    expect(await counterAt(page, '4 / 5') - entries[0].t).toBeGreaterThan(4_700);
  });

  test('#autoview=2 plays the imported posts, 2 seconds each, and Escape ends it', async ({ page }) => {
    await importBasic(page);
    await page.evaluate(() => { window.location.hash = '#autoview=2'; });

    await expect(player(page)).toBeVisible();
    await expect(counter(page)).toHaveText('1 / 6');
    await expect(counter(page)).toHaveText('2 / 6', { timeout: 6_000 });

    await page.keyboard.press('Escape');
    await expect(anyDialog(page)).toHaveCount(0);
    // The address no longer asks for auto view, so a reload shows the feed.
    expect(new URL(page.url()).hash).toBe('');
  });

  test('a link with #autoview starts once the kept import has been restored', async ({ page }) => {
    await importBasic(page);
    // Saving in IndexedDB has no UI signal: wait until the import is kept.
    await expect.poll(() => page.evaluate(() => new Promise<boolean>((resolve) => {
      const open = indexedDB.open('ig-clone-import');
      open.onerror = () => resolve(false);
      open.onsuccess = () => {
        const db = open.result;
        if (!db.objectStoreNames.contains('import')) {
          db.close();
          resolve(false);
          return;
        }
        const request = db.transaction('import', 'readonly').objectStore('import').get('meta');
        request.onsuccess = () => { db.close(); resolve(request.result !== undefined); };
        request.onerror = () => { db.close(); resolve(false); };
      };
    }))).toBe(true);
    // A fresh load of the address with the hash (goto would only change the hash in place).
    await page.evaluate(() => window.history.replaceState(null, '', '/#autoview=2'));
    await page.reload();
    // The six imported posts, not the demo feed that is on screen for a moment.
    await expect(counter(page)).toHaveText('1 / 6');
    await page.keyboard.press('Escape');
    await expect(banner(page)).toBeVisible();
    // Nothing on the page opened it: focus goes to the banner's Auto view button, not to the top of the document.
    await expect(banner(page).getByRole('button', { name: 'Auto view' })).toBeFocused();
  });

  test('the banner, the post menu and the More menu open the auto view setup', async ({ page }) => {
    await importBasic(page);

    await banner(page).getByRole('button', { name: 'Auto view' }).click();
    await expect(autoViewDialog(page)).toBeVisible();
    await page.keyboard.press('Escape');
    await expect(anyDialog(page)).toHaveCount(0);

    const postMenu = card(page, 'FXTbasic002').getByRole('button', { name: 'More options' });
    await postMenu.click();
    await page.getByTestId('post-options').getByRole('button', { name: 'Auto view from here' }).click();
    await expect(page.getByTestId('post-options')).toHaveCount(0);
    await startFromSetup(page, 30);
    // From the post the menu belonged to.
    await expect(counter(page)).toHaveText('2 / 6');
    await page.keyboard.press('Escape');
    await expect(anyDialog(page)).toHaveCount(0);
    await expect(postMenu).toBeFocused();

    const more = page.getByRole('button', { name: 'More', exact: true });
    await more.click();
    await page.getByTestId('auto-view-open-menu').click();
    await startFromSetup(page, 30);
    await expect(counter(page)).toHaveText('1 / 6');
    await page.keyboard.press('Escape');
    await expect(anyDialog(page)).toHaveCount(0);
    // The menu item is gone: focus goes back to the button that opened the menu.
    await expect(more).toBeFocused();
  });
});
