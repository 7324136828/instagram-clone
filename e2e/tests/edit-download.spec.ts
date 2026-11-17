// The callbacks passed to page.evaluate() run in the browser, so this file needs the DOM types
// (the e2e tsconfig only has the Node ones).
/// <reference lib="dom" />
/// <reference lib="dom.iterable" />
import type { Page } from '@playwright/test';
import { readFileSync } from 'node:fs';
import { join } from 'node:path';
import {
  SEED_USER, banner, card, expect, expectImportSuccess, fixture, openImportDialog, test, viewFeed,
} from './support/app';
import { DOWNLOADS, extractZip, validateExport } from './support/exportArchive';

/**
 * Editing a post's description and downloading posts, in browser mode (the default of
 * support/app: the import is kept in this browser's IndexedDB). tests/library.spec.ts covers the
 * same with the backend library.
 */

async function importBasic(page: Page): Promise<void> {
  await page.goto('/');
  await expect(page.getByText(SEED_USER).first()).toBeVisible();
  await openImportDialog(page);
  await page.getByTestId('import-archive-input').setInputFiles(fixture('export-basic.zip'));
  await expectImportSuccess(page, 'Imported 6 of 6 posts from export-basic.zip');
  await viewFeed(page);
}

/** The caption of a post in the import this browser keeps for the next visit (IndexedDB). */
const storedCaption = (page: Page, id: string) => page.evaluate((postId) => new Promise<string | null>((resolve) => {
  const open = indexedDB.open('ig-clone-import');
  open.onerror = () => resolve(null);
  open.onsuccess = () => {
    const db = open.result;
    if (!db.objectStoreNames.contains('import')) {
      db.close();
      resolve(null);
      return;
    }
    const request = db.transaction('import', 'readonly').objectStore('import').get('meta');
    request.onsuccess = () => {
      db.close();
      const posts = (request.result?.posts ?? []) as Array<{ id: string; caption: string }>;
      resolve(posts.find((post) => post.id === postId)?.caption ?? null);
    };
    request.onerror = () => {
      db.close();
      resolve(null);
    };
  };
}), id);

const sheet = (page: Page) => page.getByRole('dialog', { name: 'Post options' });
const editor = (page: Page) => page.getByRole('dialog', { name: 'Edit description' });
const toast = (page: Page, text: string) => page.getByTestId('toast').filter({ hasText: text });

test('an edited description is kept in this browser and goes into the downloads', async ({ page, outbound }) => {
  await importBasic(page);
  const first = card(page, 'FXTbasic001');

  await first.getByRole('button', { name: 'More options' }).click();
  await sheet(page).getByRole('button', { name: 'Edit description' }).click();
  await expect(editor(page)).toContainText('Kept in this browser with the import.');
  const field = editor(page).getByRole('textbox', { name: 'Description' });
  await expect(field).toBeFocused();
  await field.fill('Croissants 🥐\nBaked at dawn.');
  // Enter is a new line; Ctrl+Enter saves.
  await field.press('Control+Enter');
  await expect(editor(page)).toHaveCount(0);
  await expect(toast(page, 'Description updated.')).toBeVisible();
  await expect(first.locator('.post__caption')).toContainText('Croissants 🥐');
  await expect.poll(() => storedCaption(page, 'FXTbasic001')).toBe('Croissants 🥐\nBaked at dawn.');

  await page.reload();
  await expect(banner(page)).toContainText('Showing 6 posts imported from export-basic.zip');
  await expect(card(page, 'FXTbasic001').locator('.post__caption')).toContainText('Croissants 🥐');

  // One post: the HEIC picture, as the JPEG this browser converted it to.
  await card(page, 'FXTbasic004').getByRole('button', { name: 'More options' }).click();
  const single = page.waitForEvent('download');
  await sheet(page).getByRole('button', { name: 'Download post' }).click();
  const one = await single;
  expect(one.suggestedFilename()).toBe('instagram-export-FXTbasic004.zip');
  await one.saveAs(join(DOWNLOADS, 'browser-post.zip'));
  const oneFolder = join(DOWNLOADS, 'browser-post');
  expect(extractZip(join(DOWNLOADS, 'browser-post.zip'), oneFolder)).toEqual(['post.json', 'posts/post1.jpg']);
  let verdict = validateExport(oneFolder);
  expect(verdict.status, verdict.output).toBe(0);
  expect(verdict.output).not.toContain('WARN');

  // Everything, from the banner.
  const everything = page.waitForEvent('download');
  await banner(page).getByTestId('download-all').click();
  const all = await everything;
  expect(all.suggestedFilename()).toBe('instagram-export.zip');
  await all.saveAs(join(DOWNLOADS, 'browser-all.zip'));
  const allFolder = join(DOWNLOADS, 'browser-all');
  expect(extractZip(join(DOWNLOADS, 'browser-all.zip'), allFolder)).toEqual([
    'avatars/fixture.sunrise.bakery.jpg', 'avatars/fixture_tidewater.jpg', 'post.json',
    'posts/post1.jpg', 'posts/post2.png', 'posts/post3.webp', 'posts/post4.jpg', 'posts/post5_1.jpg', 'posts/post5_2.jpg',
    'posts/post6.jpg',
  ]);
  const manifest = JSON.parse(readFileSync(join(allFolder, 'post.json'), 'utf8')) as {
    posts: Array<{ id: string; caption: string; media: Array<{ path: string }>; user: { avatar?: string } }>;
  };
  expect(manifest.posts.map((post) => post.id)).toEqual([
    'FXTbasic001', 'FXTbasic002', 'FXTbasic003', 'FXTbasic004', 'FXTbasic005', 'FXTbasic006',
  ]);
  expect(manifest.posts[0]).toMatchObject({
    caption: 'Croissants 🥐\nBaked at dawn.',
    media: [{ path: 'posts/post1.jpg' }],
    user: { avatar: 'avatars/fixture.sunrise.bakery.jpg' },
  });
  expect(manifest.posts[2].caption).toBe('Quiet morning in the green room 🌿');
  verdict = validateExport(allFolder);
  expect(verdict.status, verdict.output).toBe(0);
  expect(verdict.output).not.toContain('WARN');
  await expect(toast(page, 'Downloaded 6 posts as instagram-export.zip.')).toBeVisible();

  // Nothing left the machine.
  expect(outbound).toEqual([]);
});

test('the description can be edited from the post detail, and Escape or Cancel leave it as it was', async ({ page }) => {
  await importBasic(page);
  await card(page, 'FXTbasic003').getByRole('button', { name: 'Comment', exact: true }).click();
  const detail = page.locator('.pd-card[data-post-id="imp:FXTbasic003"]');
  await expect(detail).toBeVisible();

  await detail.getByRole('button', { name: 'More options' }).click();
  await sheet(page).getByRole('button', { name: 'Edit description' }).click();
  await editor(page).getByRole('textbox', { name: 'Description' }).fill('Not kept');
  await page.keyboard.press('Escape');
  await expect(editor(page)).toHaveCount(0);
  // Escape closed the editor only: the post stays open, once.
  await expect(detail).toBeVisible();
  await expect(page.locator('.pd-wrap')).toHaveCount(1);
  await expect(detail).toContainText('Quiet morning in the green room');

  await detail.getByRole('button', { name: 'More options' }).click();
  await sheet(page).getByRole('button', { name: 'Edit description' }).click();
  await editor(page).getByRole('textbox', { name: 'Description' }).fill('Not kept either');
  await editor(page).getByRole('button', { name: 'Cancel' }).click();
  await expect(page.locator('.pd-wrap')).toHaveCount(1);
  await expect(detail).toContainText('Quiet morning in the green room');

  await detail.getByRole('button', { name: 'More options' }).click();
  await sheet(page).getByRole('button', { name: 'Edit description' }).click();
  await editor(page).getByRole('textbox', { name: 'Description' }).fill('A greener room 🌿');
  await editor(page).getByRole('button', { name: 'Save' }).click();
  await expect(detail).toContainText('A greener room 🌿');
  await expect(card(page, 'FXTbasic003').locator('.post__caption')).toContainText('A greener room 🌿');
});

test('a demo post can be edited until the page is reloaded, but has nothing to download', async ({ page }) => {
  await page.goto('/');
  const post = page.locator('article.post').first();
  await expect(post).toBeVisible();
  await post.getByRole('button', { name: 'More options' }).click();
  await expect(sheet(page).getByRole('button', { name: 'Download post' })).toHaveCount(0);
  await sheet(page).getByRole('button', { name: 'Edit description' }).click();
  await expect(editor(page)).toContainText('Demo post: the change lasts until you reload the page.');
  await editor(page).getByRole('textbox', { name: 'Description' }).fill('An edited demo caption');
  await editor(page).getByRole('button', { name: 'Save' }).click();
  await expect(toast(page, 'Description updated.')).toBeVisible();
  await expect(post.locator('.post__caption')).toContainText('An edited demo caption');
  // Without an import there is no banner, so no "Download all" either.
  await expect(page.getByTestId('download-all')).toHaveCount(0);
});
