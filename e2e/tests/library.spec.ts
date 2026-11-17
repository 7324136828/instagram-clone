// The callbacks passed to page.evaluate() run in the browser, so this file needs the DOM types
// (the e2e tsconfig only has the Node ones).
/// <reference lib="dom" />
/// <reference lib="dom.iterable" />
import type { APIRequestContext, Page, Request } from '@playwright/test';
import { existsSync, readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { crc32 } from 'node:zlib';
import {
  CACHE_DIR, SEED_USER, banner, card, expect, expectImportSuccess, fixture, importDialog, importStatus,
  openImportDialog, sidewaysOverflow, test, viewFeed,
} from './support/app';
import { DOWNLOADS, extractZip, validateExport } from './support/exportArchive';

/**
 * The library: every import is saved by the backend in its cache folder (%TEMP%/instagram_cache
 * in real use; here CACHE_DIR inside e2e/test-results, see playwright.config.ts) and merged with
 * what is already there. These tests share that folder: like every spec file, this one runs in
 * one worker, test after test (fullyParallel is off), and each test starts from an empty library.
 */
test.use({ libraryMode: true });

const clearLibrary = async ({ request }: { request: APIRequestContext }) => {
  const cleared = await request.delete('/api/library');
  expect(cleared.status()).toBe(204);
};
test.beforeEach(clearLibrary);
// Nothing is left behind for the next run.
test.afterAll(clearLibrary);

interface CachedPost {
  id: string;
  caption: string;
  media_type: string;
  media: Array<{ type: string; path: string; poster?: string }>;
  comments?: Array<{ username: string; text: string }>;
}

interface CachedManifest {
  format: string;
  version: number;
  library: { revision: number; imports: Array<{ source_name: string; source_kind: string; posts: number; new: number; updated: number }> };
  posts: CachedPost[];
}

function cachedManifest(): CachedManifest {
  return JSON.parse(readFileSync(join(CACHE_DIR, 'post.json'), 'utf8')) as CachedManifest;
}

/**
 * Runs the export validator of instagram.md (section 9, the igx:validate.mjs block) on the cache
 * folder: the library has to be a valid export, without warnings.
 */
const validateCache = () => validateExport(CACHE_DIR);

const cachedFiles = (folder: string) => (existsSync(join(CACHE_DIR, folder)) ? readdirSync(join(CACHE_DIR, folder)).sort() : []);

/** Every file post.json names under posts/ (media and posters), sorted. */
const referencedPostFiles = (manifest: CachedManifest) => manifest.posts
  .flatMap((post) => post.media.flatMap((media) => [media.path, ...(media.poster ? [media.poster] : [])]))
  .map((path) => path.replace(/^posts\//, ''))
  .sort();

async function openApp(page: Page): Promise<void> {
  await page.goto('/');
  await expect(page.getByText(SEED_USER).first()).toBeVisible();
}

async function importFiles(page: Page, files: string[], statusText: string | RegExp): Promise<void> {
  await openImportDialog(page);
  await page.getByTestId('import-archive-input').setInputFiles(files.map(fixture));
  await expectImportSuccess(page, statusText);
}

/** Whether this browser keeps an import in IndexedDB (saving it there has no UI signal). */
const keptInBrowser = (page: Page) => page.evaluate(() => new Promise<boolean>((resolve) => {
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
}));

/** Every file sent to the library is refused, as when the cache folder is full. */
const FILES = '**/api/library/uploads/*/files/*';
const refuseFiles = (page: Page) => page.route(FILES, (route) => route.fulfill({
  status: 507,
  contentType: 'application/json',
  body: JSON.stringify({ detail: 'There is no space left in the cache folder' }),
}));

const libraryRequests = (page: Page) => {
  const seen: string[] = [];
  page.on('request', (request: Request) => {
    const url = new URL(request.url());
    if (url.pathname.startsWith('/api/library')) seen.push(`${request.method()} ${url.pathname}`);
  });
  return seen;
};

/** A stored (uncompressed) zip of `files`, built in memory. */
function zipOf(files: Record<string, Buffer>): Buffer {
  const locals: Buffer[] = [];
  const centrals: Buffer[] = [];
  let offset = 0;
  for (const [name, data] of Object.entries(files)) {
    const nameBytes = Buffer.from(name, 'utf8');
    const crc = crc32(data);
    const local = Buffer.alloc(30);
    local.writeUInt32LE(0x04034b50, 0); local.writeUInt16LE(20, 4); local.writeUInt16LE(0x0800, 6);
    local.writeUInt32LE(crc, 14); local.writeUInt32LE(data.length, 18); local.writeUInt32LE(data.length, 22);
    local.writeUInt16LE(nameBytes.length, 26);
    const central = Buffer.alloc(46);
    central.writeUInt32LE(0x02014b50, 0); central.writeUInt16LE(20, 4); central.writeUInt16LE(20, 6);
    central.writeUInt16LE(0x0800, 8); central.writeUInt32LE(crc, 16); central.writeUInt32LE(data.length, 20);
    central.writeUInt32LE(data.length, 24); central.writeUInt16LE(nameBytes.length, 28); central.writeUInt32LE(offset, 42);
    locals.push(local, nameBytes, data);
    centrals.push(central, nameBytes);
    offset += local.length + nameBytes.length + data.length;
  }
  const directory = Buffer.concat(centrals);
  const end = Buffer.alloc(22);
  const count = Object.keys(files).length;
  end.writeUInt32LE(0x06054b50, 0); end.writeUInt16LE(count, 8); end.writeUInt16LE(count, 10);
  end.writeUInt32LE(directory.length, 12); end.writeUInt32LE(offset, 16);
  return Buffer.concat([...locals, directory, end]);
}

/** An archive with `count` small posts (ids <prefix>00001...) that all show the same picture. */
function bigArchive(name: string, prefix: string, count: number) {
  const posts = Array.from({ length: count }, (_, index) => ({
    id: `${prefix}${String(index + 1).padStart(5, '0')}`,
    image: 'posts/p.png',
    user: { username: 'fixture.bulk' },
    caption: `Bulk post ${index + 1}`,
  }));
  const manifest = { format: 'instagram-export', version: 1, posts };
  const buffer = zipOf({
    'post.json': Buffer.from(JSON.stringify(manifest), 'utf8'),
    'posts/p.png': readFileSync(join(fixture('export-basic'), 'posts', 'post2.png')),
  });
  return { name, mimeType: 'application/zip', buffer };
}

test.describe('the library in the cache folder', () => {
  test('an import is saved in the cache folder, with upload progress, and is still there after a reload', async ({ page }) => {
    const requests = libraryRequests(page);
    await openApp(page);
    await openImportDialog(page);
    await expect(page.getByTestId('import-storage-note')).toContainText(`Imports are added to your library, which this app's server keeps in ${CACHE_DIR}`);
    // Bulk files trickle in, so the upload phase can be seen.
    await page.route('**/api/library/uploads/*/files/*', async (route) => {
      await new Promise((resolve) => setTimeout(resolve, 150));
      await route.continue();
    });
    await page.getByTestId('import-archive-input').setInputFiles(fixture('export-basic.zip'));
    const progress = page.getByTestId('import-progress');
    await expect(progress).toHaveAttribute('data-phase', 'uploading');
    await expect(progress).toHaveAccessibleName('Saving progress');
    await expect(importStatus(page)).toContainText(/Saving to your library: file \d+ of \d+/);
    await expectImportSuccess(page, 'Imported 6 of 6 posts from export-basic.zip');
    await expect(page.getByTestId('import-saved')).toHaveText(`Saved in your library (6 new posts): ${CACHE_DIR}`);
    await viewFeed(page);

    await expect(banner(page).getByTestId('library-summary')).toHaveText(
      `Showing 6 posts from your library (1 import) · saved in ${CACHE_DIR}`,
    );
    await expect(banner(page).getByRole('button', { name: 'Clear library' })).toBeVisible();
    await expect(banner(page).getByRole('button', { name: 'Import more' })).toBeVisible();
    expect(requests.filter((r) => r.startsWith('PUT ')).length).toBeGreaterThanOrEqual(8);
    expect(requests).toContain('POST /api/library/uploads');

    // On disk: an export of its own, which the clone (and instagram.md's validator) can read.
    const manifest = cachedManifest();
    expect(manifest).toMatchObject({ format: 'instagram-export', version: 1 });
    expect(manifest.posts.map((post) => post.id)).toEqual([
      'FXTbasic001', 'FXTbasic002', 'FXTbasic003', 'FXTbasic004', 'FXTbasic005', 'FXTbasic006',
    ]);
    expect(manifest.library.imports).toMatchObject([{ source_name: 'export-basic.zip', source_kind: 'archive', posts: 6, new: 6 }]);
    for (const post of manifest.posts) {
      for (const media of post.media) expect(existsSync(join(CACHE_DIR, media.path)), media.path).toBe(true);
    }
    expect(cachedFiles('avatars').length).toBeGreaterThan(0);
    // The HEIC picture was converted: the library only stores formats every browser shows.
    expect(cachedFiles('posts').some((name) => /\.hei[cf]$/i.test(name))).toBe(false);
    expect(cachedFiles('.staging')).toEqual([]);
    // Comments keep their authors.
    expect(manifest.posts[0].comments?.map((comment) => comment.username)).toEqual(['fixture_tidewater', 'pixel.orchard.demo']);
    const verdict = validateCache();
    expect(verdict.status, verdict.output).toBe(0);
    expect(verdict.output).not.toContain('WARN');

    // The pictures come from the library, not from the browser.
    const picture = card(page, 'FXTbasic001').locator('img.photo__img');
    await expect(picture).toHaveAttribute('src', /^\/api\/library\/files\/posts\/.+\?r=\d+$/);
    await expect.poll(() => picture.evaluate((el: HTMLImageElement) => el.naturalWidth)).toBeGreaterThan(0);

    await page.reload();
    await expect(banner(page).getByTestId('library-summary')).toContainText('Showing 6 posts from your library (1 import)');
    await expect.poll(() => card(page, 'FXTbasic001').locator('img.photo__img').evaluate((el: HTMLImageElement) => el.naturalWidth))
      .toBeGreaterThan(0);
  });

  test('several archives picked at once are merged, and the next import adds to the library', async ({ page }) => {
    await openApp(page);
    await importFiles(page, ['export-part1.zip', 'export-part2.zip'], 'Imported 3 of 3 posts from 2 archives');
    await expect(page.getByTestId('import-sources').getByRole('listitem')).toHaveText([
      'export-part1.zip: 2 posts', 'export-part2.zip: 2 posts',
    ]);
    await expect(page.getByTestId('import-saved')).toContainText('Saved in your library (3 new posts)');
    await viewFeed(page);
    await expect(banner(page).getByTestId('library-summary')).toContainText('Showing 3 posts from your library (1 import)');

    let manifest = cachedManifest();
    expect(manifest.posts.map((post) => post.id)).toEqual(['FXTpart0003', 'FXTpart0002', 'FXTpart0001']);
    const shared = manifest.posts[1];
    expect(shared.caption).toBe('Part 2: second version of the shared post');
    expect(shared.media).toHaveLength(1);
    expect(shared.media[0]).toMatchObject({ type: 'image', path: expect.stringMatching(/^posts\/.+\.jpg$/) });
    expect(manifest.library.imports).toMatchObject([{ source_name: '2 archives', source_kind: 'multiple', posts: 3 }]);
    // The video of part 1's copy of the shared post was not kept: posts/ holds exactly what post.json names.
    expect(cachedFiles('posts').filter((name) => name.endsWith('.webm'))).toEqual([]);
    expect(cachedFiles('posts')).toEqual(referencedPostFiles(manifest));
    expect(cachedFiles('.staging')).toEqual([]);

    // A second import action: added to the library.
    await banner(page).getByRole('button', { name: 'Import more' }).click();
    await expect(importDialog(page).getByRole('radio')).toHaveCount(0);
    await page.getByTestId('import-archive-input').setInputFiles(fixture('export-videos.zip'));
    await expectImportSuccess(page, 'Imported 5 of 5 posts from export-videos.zip');
    await expect(page.getByTestId('import-saved')).toContainText('Saved in your library (5 new posts)');
    await viewFeed(page);
    await expect(banner(page).getByTestId('library-summary')).toContainText('Showing 8 posts from your library (2 imports)');
    manifest = cachedManifest();
    expect(manifest.posts).toHaveLength(8);
    expect(manifest.posts.find((post) => post.id === 'FXTvideo001')?.media[0]).toMatchObject({
      type: 'video',
      path: expect.stringMatching(/^posts\/.+\.webm$/),
      poster: expect.stringMatching(/^posts\/.+\.jpg$/),
    });
    expect(manifest.posts.map((post) => post.id)).toEqual([
      'FXTvideo001', 'FXTvideo002', 'FXTvideo003', 'FXTvideo004', 'FXTvideo005', 'FXTpart0003', 'FXTpart0002', 'FXTpart0001',
    ]);
    expect(cachedFiles('posts')).toEqual(referencedPostFiles(manifest));

    // The server sends parts of a video (HTTP Range), which is what seeking needs.
    const videoPath = manifest.posts.find((post) => post.id === 'FXTvideo001')!.media[0].path;
    const url = `/api/library/files/${videoPath.split('/').map(encodeURIComponent).join('/')}`;
    const whole = await page.request.get(url);
    expect(whole.status()).toBe(200);
    expect(whole.headers()['accept-ranges']).toBe('bytes');
    expect(whole.headers()['content-type']).toBe('video/webm');
    const part = await page.request.get(url, { headers: { Range: 'bytes=100-199' } });
    expect(part.status()).toBe(206);
    expect(part.headers()['content-range']).toBe(`bytes 100-199/${(await whole.body()).length}`);
    expect(Buffer.compare(await part.body(), (await whole.body()).subarray(100, 200))).toBe(0);

    // The same archive again replaces its posts instead of adding copies.
    await banner(page).getByRole('button', { name: 'Import more' }).click();
    await page.getByTestId('import-archive-input').setInputFiles(fixture('export-part2.zip'));
    await expectImportSuccess(page, 'Imported 2 of 2 posts from export-part2.zip');
    await expect(page.getByTestId('import-saved')).toContainText('Saved in your library (2 updated posts)');
    await viewFeed(page);
    await expect(banner(page).getByTestId('library-summary')).toContainText('Showing 8 posts from your library (3 imports)');
  });

  test('videos are played from the library', async ({ page }) => {
    await openApp(page);
    await importFiles(page, ['export-videos.zip'], 'Imported 5 of 5 posts from export-videos.zip');
    await viewFeed(page);

    const video = card(page, 'FXTvideo001').getByTestId('post-video');
    await expect(video).toHaveAttribute('src', /^\/api\/library\/files\/posts\/.+\.webm\?r=\d+$/);
    await expect(video).toHaveAttribute('poster', /^\/api\/library\/files\/posts\/.+\.jpg\?r=\d+$/);
    await expect.poll(() => video.evaluate((el: HTMLVideoElement) => !el.paused && el.currentTime > 0.2), { timeout: 15_000 })
      .toBe(true);
    // Seeking needs HTTP range requests.
    const seeked = await video.evaluate(async (el: HTMLVideoElement) => {
      el.pause();
      el.currentTime = 1.5;
      await new Promise((resolve) => { el.addEventListener('seeked', resolve, { once: true }); });
      return el.currentTime;
    });
    expect(seeked).toBeCloseTo(1.5, 1);

    // The .mov that cannot be played can still be downloaded, under its library name.
    const broken = card(page, 'FXTvideo004');
    await broken.scrollIntoViewIfNeeded();
    const download = broken.getByRole('link', { name: 'Download video' });
    await expect(download).toHaveAttribute('href', /^\/api\/library\/files\/posts\/.+\.mov\?r=\d+$/);
    await expect(download).toHaveAttribute('download', /\.mov$/);

    // The library with videos and posters is a valid export too.
    const verdict = validateCache();
    expect(verdict.status, verdict.output).toBe(0);
    expect(verdict.output).not.toContain('WARN');
  });

  test('"Clear library" asks first, then removes the posts and their files', async ({ page }) => {
    await openApp(page);
    await importFiles(page, ['export-videos.zip'], 'Imported 5 of 5 posts from export-videos.zip');
    await viewFeed(page);
    expect(existsSync(join(CACHE_DIR, 'post.json'))).toBe(true);

    await banner(page).getByRole('button', { name: 'Clear library' }).click();
    const confirm = page.getByRole('alertdialog', { name: 'Clear library?' });
    await expect(confirm).toBeVisible();
    await expect(confirm).toContainText(`This deletes the 5 imported posts and their pictures and videos from ${CACHE_DIR}`);
    await confirm.getByRole('button', { name: 'Cancel' }).click();
    await expect(confirm).toHaveCount(0);
    expect(existsSync(join(CACHE_DIR, 'post.json'))).toBe(true);

    // Escape cancels too.
    await banner(page).getByRole('button', { name: 'Clear library' }).click();
    await page.keyboard.press('Escape');
    await expect(confirm).toHaveCount(0);

    await banner(page).getByRole('button', { name: 'Clear library' }).click();
    await confirm.getByRole('button', { name: 'Clear library' }).click();
    await expect(page.getByTestId('toast').filter({ hasText: 'Library cleared' })).toBeVisible();
    await expect(banner(page)).toHaveCount(0);
    await expect(page.getByText(SEED_USER).first()).toBeVisible();
    expect(existsSync(join(CACHE_DIR, 'post.json'))).toBe(false);
    expect(cachedFiles('posts')).toEqual([]);
    expect(cachedFiles('avatars')).toEqual([]);

    await page.reload();
    await expect(page.getByText(SEED_USER).first()).toBeVisible();
    await expect(banner(page)).toHaveCount(0);
  });

  test('when saving fails, the import is shown from this browser and the dialog says why', async ({ page }) => {
    const requests = libraryRequests(page);
    await page.route('**/api/library/uploads/*/files/*', (route) => route.fulfill({
      status: 507,
      contentType: 'application/json',
      body: JSON.stringify({ detail: 'There is no space left in the cache folder' }),
    }));
    await openApp(page);
    await importFiles(page, ['export-basic.zip'], 'Imported 6 of 6 posts from export-basic.zip');
    await expect(page.getByTestId('import-save-error')).toContainText(
      'Not saved in the library. "m000001.jpg" was not accepted: There is no space left in the cache folder. These posts are shown from this browser only.',
    );
    await viewFeed(page);
    await expect(banner(page)).toContainText('Showing 6 posts imported from export-basic.zip');
    await expect(banner(page).getByTestId('library-save-error')).toBeVisible();
    await expect(banner(page).getByRole('button', { name: 'Clear import' })).toBeVisible();
    await expect.poll(() => card(page, 'FXTbasic001').locator('img.photo__img').getAttribute('src')).toMatch(/^blob:/);

    // The upload session was dropped and nothing reached the library.
    await expect.poll(() => requests.some((r) => /^DELETE \/api\/library\/uploads\/[0-9a-f]+$/.test(r))).toBe(true);
    expect(existsSync(join(CACHE_DIR, 'post.json'))).toBe(false);
    await expect.poll(() => cachedFiles('.staging')).toEqual([]);

    // From now on this tab works like browser mode: no Add/Replace confusion with the library.
    await banner(page).getByRole('button', { name: 'Import another' }).click();
    await expect(page.getByTestId('import-storage-note')).toContainText('Everything stays in this browser.');
  });

  test('after a failed save, the next import stays in this browser and is added to the posts on screen', async ({ page }) => {
    const requests = libraryRequests(page);
    await refuseFiles(page);
    await openApp(page);
    await importFiles(page, ['export-part1.zip'], 'Imported 2 of 2 posts from export-part1.zip');
    await expect(page.getByTestId('import-save-error')).toBeVisible();
    await viewFeed(page);
    // The server could take files again: this page still promised to keep imports in the browser.
    await page.unroute(FILES);
    const sessions = requests.filter((r) => r === 'POST /api/library/uploads').length;
    expect(sessions).toBe(1);
    const sent = requests.length;

    await banner(page).getByRole('button', { name: 'Import another' }).click();
    await expect(page.getByTestId('import-storage-note')).toContainText(
      'Everything stays in this browser. Nothing is uploaded. Reload the page to save to the library again.',
    );
    await expect(importDialog(page).getByRole('radio', { name: 'Add to current import' })).toBeChecked();
    await page.getByTestId('import-archive-input').setInputFiles(fixture('export-part2.zip'));
    await expectImportSuccess(page, 'Imported 2 of 2 posts from export-part2.zip');
    await expect(page.getByTestId('import-saved')).toHaveCount(0);
    await viewFeed(page);
    // Added, as the dialog said: part 1 and part 2 share one post.
    await expect(banner(page)).toContainText('Showing 3 posts imported from 2 archives');
    await expect(card(page, 'FXTpart0001')).toBeVisible();
    await expect(card(page, 'FXTpart0003')).toBeVisible();
    // Nothing was uploaded.
    expect(requests.filter((r) => r === 'POST /api/library/uploads').length).toBe(sessions);
    expect(requests.slice(sent).filter((r) => !r.startsWith('GET '))).toEqual([]);
    expect(existsSync(join(CACHE_DIR, 'post.json'))).toBe(false);
  });

  test('posts kept in this browser after a failed save are offered for saving on the next visit', async ({ page }) => {
    await refuseFiles(page);
    await openApp(page);
    await importFiles(page, ['export-basic.zip'], 'Imported 6 of 6 posts from export-basic.zip');
    await viewFeed(page);
    await expect(banner(page).getByTestId('library-save-error')).toContainText('kept in this browser only');
    await expect.poll(() => keptInBrowser(page)).toBe(true);

    // Next visit: the library works again (and is empty). The posts are not lost from view.
    await page.unroute(FILES);
    await page.reload();
    const copy = page.getByTestId('browser-copy');
    await expect(copy).toContainText('6 posts from export-basic.zip are kept in this browser only: they are not in your library.');
    await copy.getByRole('button', { name: 'Save to library' }).click();
    await expectImportSuccess(page, 'Imported 6 of 6 posts from export-basic.zip');
    await expect(page.getByTestId('import-saved')).toContainText('Saved in your library (6 new posts)');
    await viewFeed(page);
    await expect(banner(page).getByTestId('library-summary')).toContainText('Showing 6 posts from your library (1 import)');
    await expect(copy).toHaveCount(0);
    expect(cachedManifest().posts).toHaveLength(6);

    // Saved: the browser's copy is gone, and is not offered again.
    await expect.poll(() => keptInBrowser(page)).toBe(false);
    await page.reload();
    await expect(banner(page).getByTestId('library-summary')).toContainText('Showing 6 posts from your library');
    await expect(copy).toHaveCount(0);
  });

  test('posts kept in this browser can be discarded instead', async ({ page }) => {
    await refuseFiles(page);
    await openApp(page);
    await importFiles(page, ['export-part1.zip'], 'Imported 2 of 2 posts from export-part1.zip');
    await viewFeed(page);
    await expect.poll(() => keptInBrowser(page)).toBe(true);
    await page.unroute(FILES);
    await page.reload();

    const copy = page.getByTestId('browser-copy');
    await expect(copy).toContainText('2 posts from export-part1.zip are kept in this browser only');
    await copy.getByRole('button', { name: 'Discard' }).click();
    await expect(copy).toHaveCount(0);
    await expect(page.getByTestId('toast').filter({ hasText: 'Removed the posts kept in this browser' })).toBeVisible();
    await expect.poll(() => keptInBrowser(page)).toBe(false);
    await page.reload();
    await expect(page.getByText(SEED_USER).first()).toBeVisible();
    await expect(copy).toHaveCount(0);
    expect(existsSync(join(CACHE_DIR, 'post.json'))).toBe(false);
  });

  test('once the library is merging an import it can no longer be cancelled, and it is shown as saved', async ({ page }) => {
    let release: () => void = () => undefined;
    const held = new Promise<void>((resolve) => { release = resolve; });
    let committed = false;
    await page.route('**/api/library/uploads/*/commit', async (route) => {
      // The server has merged the import; only its answer is late.
      const response = await route.fetch();
      committed = true;
      await held;
      await route.fulfill({ response });
    });
    await openApp(page);
    await openImportDialog(page);
    await page.getByTestId('import-archive-input').setInputFiles(fixture('export-basic.zip'));
    await expect(page.getByTestId('import-committing')).toBeVisible();
    await expect(importStatus(page)).toHaveText('Adding the posts to your library…');
    await expect(importDialog(page).getByRole('button', { name: 'Cancel' })).toHaveCount(0);
    await expect.poll(() => committed).toBe(true);

    // The chip of a closed dialog offers no Cancel either.
    await page.getByTestId('import-close').click();
    const chip = page.getByTestId('import-chip');
    await expect(chip).toContainText('Saving…');
    await expect(chip.getByRole('button', { name: 'Cancel' })).toHaveCount(0);
    await chip.getByRole('button', { name: 'Show' }).click();

    release();
    await expectImportSuccess(page, 'Imported 6 of 6 posts from export-basic.zip');
    await expect(page.getByTestId('import-saved')).toContainText('Saved in your library (6 new posts)');
    await viewFeed(page);
    await expect(banner(page).getByTestId('library-summary')).toContainText('Showing 6 posts from your library (1 import)');
  });

  test('an import of more than 5,000 posts from two archives is saved in two parts', async ({ page }) => {
    test.setTimeout(240_000);
    const requests = libraryRequests(page);
    await openApp(page);
    await openImportDialog(page);
    await page.getByTestId('import-archive-input').setInputFiles([
      bigArchive('bulk-a.zip', 'BLKa', 3000), bigArchive('bulk-b.zip', 'BLKb', 3000),
    ]);
    await expect(importStatus(page)).toContainText(/part [12] of 2/, { timeout: 120_000 });
    await expectImportSuccess(page, 'Imported 6,000 of 6,000 posts from 2 archives');
    await expect(page.getByTestId('import-saved')).toHaveText(`Saved in your library (6,000 new posts): ${CACHE_DIR}`);
    await expect(page.getByTestId('import-unsaved')).toHaveCount(0);
    expect(requests.filter((r) => r === 'POST /api/library/uploads')).toHaveLength(2);
    const manifest = cachedManifest();
    expect(manifest.posts).toHaveLength(6000);
    expect(manifest.library.imports.map((entry) => [entry.source_name, entry.posts, entry.new])).toEqual([
      ['2 archives (part 1 of 2)', 5000, 5000],
      ['2 archives (part 2 of 2)', 1000, 1000],
    ]);
    await viewFeed(page);
    await expect(banner(page).getByTestId('library-summary')).toContainText('Showing 6,000 posts from your library (2 imports)');
  });

  test('when a later part of a large import fails, the parts already saved are shown and the rest is reported', async ({ page }) => {
    test.setTimeout(240_000);
    let commits = 0;
    await page.route('**/api/library/uploads/*/commit', async (route) => {
      commits += 1;
      if (commits === 1) return route.continue();
      return route.fulfill({ status: 507, contentType: 'application/json', body: JSON.stringify({ detail: 'There is no space left in the cache folder' }) });
    });
    await openApp(page);
    await openImportDialog(page);
    await page.getByTestId('import-archive-input').setInputFiles([
      bigArchive('bulk-a.zip', 'BLKa', 3000), bigArchive('bulk-b.zip', 'BLKb', 3000),
    ]);
    await expectImportSuccess(page, 'Imported 6,000 of 6,000 posts from 2 archives');
    await expect(page.getByTestId('import-partial')).toBeVisible();
    await expect(page.getByTestId('import-saved')).toContainText('Saved in your library (5,000 new posts)');
    await expect(page.getByTestId('import-unsaved')).toContainText(
      'Saved 5,000 of 6,000 posts; the rest could not be saved: Saving to the library failed: There is no space left in the cache folder.'
        + ' Import the archives again to save the rest (posts already saved are updated, not duplicated).',
    );
    expect(cachedManifest().posts).toHaveLength(5000);
    await viewFeed(page);
    // The library is on screen (not a browser-only copy that a reload would lose).
    await expect(banner(page).getByTestId('library-summary')).toContainText('Showing 5,000 posts from your library (1 import)');
    await expect(banner(page).getByTestId('library-save-error')).toHaveCount(0);
    await expect.poll(() => keptInBrowser(page)).toBe(false);
  });

  test('cancelling a large import after its first part was saved keeps that part and shows it', async ({ page }) => {
    test.setTimeout(240_000);
    let sessions = 0;
    await page.route('**/api/library/uploads', async (route) => {
      if (route.request().method() === 'POST') sessions += 1;
      await route.continue();
    });
    // The files of the second part never arrive.
    await page.route('**/api/library/uploads/*/files/*', async (route) => {
      if (sessions < 2) return route.continue();
      await new Promise(() => undefined);
    });
    await openApp(page);
    await openImportDialog(page);
    await page.getByTestId('import-archive-input').setInputFiles([
      bigArchive('bulk-a.zip', 'BLKa', 3000), bigArchive('bulk-b.zip', 'BLKb', 3000),
    ]);
    await expect(importStatus(page)).toContainText('(part 2 of 2)', { timeout: 120_000 });
    await importDialog(page).getByRole('button', { name: 'Cancel' }).click();
    await expectImportSuccess(page, 'Imported 6,000 of 6,000 posts from 2 archives');
    await expect(page.getByTestId('import-unsaved')).toContainText(
      'Saved 5,000 of 6,000 posts; the rest could not be saved: The rest of the import was cancelled.',
    );
    expect(cachedManifest().posts).toHaveLength(5000);
    await viewFeed(page);
    await expect(banner(page).getByTestId('library-summary')).toContainText('Showing 5,000 posts from your library (1 import)');
  });

  test('a merged import with archives that could not be imported says so in its headline', async ({ page }) => {
    await openApp(page);
    await importFiles(page, ['export-part1.zip', 'garbage.zip', 'export-part2.zip'], 'Imported 3 of 3 posts from 2 of 3 archives');
    await expect(page.getByTestId('import-partial')).toBeVisible();
    await expect(page.getByTestId('import-partial-note')).toHaveText('1 of 3 archives could not be imported.');
    await expect(page.getByTestId('import-sources').getByRole('listitem').nth(1)).toContainText('garbage.zip: not imported');
  });

  test('Cancel while saving drops the upload and keeps the library as it was', async ({ page }) => {
    const requests = libraryRequests(page);
    let release: () => void = () => undefined;
    const held = new Promise<void>((resolve) => { release = resolve; });
    await page.route('**/api/library/uploads/*/files/*', async (route) => {
      await held;
      await route.continue().catch(() => undefined);
    });
    await openApp(page);
    await openImportDialog(page);
    await page.getByTestId('import-archive-input').setInputFiles(fixture('export-basic.zip'));
    await expect(page.getByTestId('import-progress')).toHaveAttribute('data-phase', 'uploading');
    await importDialog(page).getByRole('button', { name: 'Cancel' }).click();
    await expect(importStatus(page)).toHaveAttribute('data-state', 'idle');
    release();

    await expect.poll(() => requests.some((r) => /^DELETE \/api\/library\/uploads\/[0-9a-f]+$/.test(r))).toBe(true);
    await page.keyboard.press('Escape');
    await expect(banner(page)).toHaveCount(0);
    expect(existsSync(join(CACHE_DIR, 'post.json'))).toBe(false);
  });

  test('without a usable library the app falls back to this browser and says so', async ({ page }) => {
    await page.route((url) => url.pathname === '/api/library', (route) => route.fulfill({
      status: 503,
      contentType: 'application/json',
      body: JSON.stringify({ detail: 'The library folder X cannot be used: access denied' }),
    }));
    await openApp(page);
    await openImportDialog(page);
    await expect(page.getByTestId('import-storage-note')).toHaveText(
      'The library could not be opened: The library folder X cannot be used: access denied. Everything stays in this browser. Nothing is uploaded.',
    );
    await page.getByTestId('import-archive-input').setInputFiles(fixture('export-basic.zip'));
    await expectImportSuccess(page, 'Imported 6 of 6 posts from export-basic.zip');
    await expect(page.getByTestId('import-saved')).toHaveCount(0);
    await viewFeed(page);
    await expect(banner(page)).toContainText('Showing 6 posts imported from export-basic.zip');
    await expect(banner(page).getByRole('button', { name: 'Clear import' })).toBeVisible();
    expect(existsSync(join(CACHE_DIR, 'post.json'))).toBe(false);
  });
});

test.describe('editing and downloading library posts', () => {
  const caption = (page: Page, id: string) => card(page, id).locator('.post__caption');

  async function openEditor(page: Page, id: string) {
    await card(page, id).getByRole('button', { name: 'More options' }).click();
    await page.getByRole('dialog', { name: 'Post options' }).getByRole('button', { name: 'Edit description' }).click();
    const editor = page.getByRole('dialog', { name: 'Edit description' });
    await expect(editor).toBeVisible();
    return editor;
  }

  test('an edited description is saved in the library post.json and is still there after a reload', async ({ page }) => {
    await openApp(page);
    await importFiles(page, ['export-basic.zip'], 'Imported 6 of 6 posts from export-basic.zip');
    await viewFeed(page);
    const requests = libraryRequests(page);

    const editor = await openEditor(page, 'FXTbasic002');
    const field = editor.getByRole('textbox', { name: 'Description' });
    await expect(field).toBeFocused();
    await expect(field).toHaveValue('潮が引いたあとの静かな海 🌊\nВечерний отлив и тишина.\nمساء هادئ على الشاطئ\n#tidepool #lowtide');
    await expect(editor).toContainText('Saved in post.json of your library.');
    // Nothing changed yet: nothing to save.
    await expect(editor.getByRole('button', { name: 'Save' })).toBeDisabled();
    await field.fill('  Low tide again 🌊\nThe second line.  ');
    await expect(editor).toContainText('38 / 10,000');
    await editor.getByRole('button', { name: 'Save' }).click();

    await expect(editor).toHaveCount(0);
    await expect(page.getByTestId('toast').filter({ hasText: 'Description saved in your library.' })).toBeVisible();
    await expect(caption(page, 'FXTbasic002')).toContainText('Low tide again 🌊');
    // The only change sent (pictures may still be loading meanwhile).
    expect(requests.filter((line) => !line.startsWith('GET '))).toEqual(['PATCH /api/library/posts/FXTbasic002']);
    // Trimmed, and nothing else of the post changed.
    const saved = cachedManifest();
    expect(saved.posts.find((post) => post.id === 'FXTbasic002')?.caption).toBe('Low tide again 🌊\nThe second line.');
    expect(saved.posts.map((post) => post.id)).toEqual([
      'FXTbasic001', 'FXTbasic002', 'FXTbasic003', 'FXTbasic004', 'FXTbasic005', 'FXTbasic006',
    ]);
    expect(saved.library.imports).toHaveLength(1);
    const verdict = validateCache();
    expect(verdict.status, verdict.output).toBe(0);

    await page.reload();
    await expect(caption(page, 'FXTbasic002')).toContainText('Low tide again 🌊');
  });

  test('when the description cannot be saved, the editor says why and keeps the text', async ({ page }) => {
    await openApp(page);
    await importFiles(page, ['export-basic.zip'], 'Imported 6 of 6 posts from export-basic.zip');
    await viewFeed(page);
    await page.route('**/api/library/posts/*', (route) => route.fulfill({
      status: 503,
      contentType: 'application/json',
      body: JSON.stringify({ detail: 'The library folder cannot be used' }),
    }));
    const editor = await openEditor(page, 'FXTbasic003');
    await editor.getByRole('textbox', { name: 'Description' }).fill('Not saved');
    await editor.getByRole('button', { name: 'Save' }).click();
    await expect(editor.getByTestId('edit-caption-error')).toHaveText(
      'The description could not be saved: The library folder cannot be used.',
    );
    await expect(editor.getByRole('textbox', { name: 'Description' })).toHaveValue('Not saved');
    await page.keyboard.press('Escape');
    await expect(editor).toHaveCount(0);
    await expect(caption(page, 'FXTbasic003')).toContainText('Quiet morning in the green room');
    expect(cachedManifest().posts.find((post) => post.id === 'FXTbasic003')?.caption).toBe('Quiet morning in the green room 🌿');
  });

  test('"Download post" and "Download all" give instagram-export archives with the descriptions', async ({ page, request }) => {
    await openApp(page);
    await importFiles(page, ['export-basic.zip'], 'Imported 6 of 6 posts from export-basic.zip');
    const edited = await request.patch('/api/library/posts/FXTbasic005', { data: { caption: 'Two tides, edited ✏️' } });
    expect(edited.status()).toBe(200);
    await page.reload();
    await expect(caption(page, 'FXTbasic005')).toContainText('Two tides, edited ✏️');

    // One post: a carousel of two pictures, with its author's profile picture.
    await card(page, 'FXTbasic005').getByRole('button', { name: 'More options' }).click();
    const single = page.waitForEvent('download');
    await page.getByRole('dialog', { name: 'Post options' }).getByRole('button', { name: 'Download post' }).click();
    const one = await single;
    expect(one.suggestedFilename()).toBe('instagram-export-FXTbasic005.zip');
    await one.saveAs(join(DOWNLOADS, 'library-post.zip'));
    const oneFolder = join(DOWNLOADS, 'library-post');
    expect(extractZip(join(DOWNLOADS, 'library-post.zip'), oneFolder)).toEqual([
      'avatars/fixture_tidewater.jpg', 'post.json', 'posts/post1_1.jpg', 'posts/post1_2.jpg',
    ]);
    const onePost = JSON.parse(readFileSync(join(oneFolder, 'post.json'), 'utf8'));
    expect(onePost).toMatchObject({ format: 'instagram-export', version: 1, source: { type: 'posts' } });
    expect(onePost.posts).toEqual([expect.objectContaining({
      id: 'FXTbasic005',
      url: 'https://www.instagram.com/p/FXTbasic005/',
      caption: 'Two tides, edited ✏️',
      media_type: 'carousel',
      media: [{ type: 'image', path: 'posts/post1_1.jpg' }, { type: 'image', path: 'posts/post1_2.jpg' }],
      user: expect.objectContaining({ username: 'fixture_tidewater', avatar: 'avatars/fixture_tidewater.jpg' }),
    })]);
    let verdict = validateExport(oneFolder);
    expect(verdict.status, verdict.output).toBe(0);
    expect(verdict.output).not.toContain('WARN');
    await expect(page.getByTestId('toast').filter({ hasText: 'Downloaded 1 post as instagram-export-FXTbasic005.zip.' })).toBeVisible();

    // Every post, newest first, as the library holds them.
    const everything = page.waitForEvent('download');
    await banner(page).getByTestId('download-all').click();
    const all = await everything;
    expect(all.suggestedFilename()).toBe('instagram-export.zip');
    await all.saveAs(join(DOWNLOADS, 'library-all.zip'));
    const allFolder = join(DOWNLOADS, 'library-all');
    expect(extractZip(join(DOWNLOADS, 'library-all.zip'), allFolder)).toEqual([
      'avatars/fixture.sunrise.bakery.jpg', 'avatars/fixture_tidewater.jpg', 'post.json',
      // The HEIC picture of post 4 is the JPEG the library keeps.
      'posts/post1.jpg', 'posts/post2.png', 'posts/post3.webp', 'posts/post4.jpg', 'posts/post5_1.jpg', 'posts/post5_2.jpg',
      'posts/post6.jpg',
    ]);
    const allPosts = JSON.parse(readFileSync(join(allFolder, 'post.json'), 'utf8')) as { posts: Array<{ id: string; caption: string }> };
    expect(allPosts.posts.map((post) => post.id)).toEqual([
      'FXTbasic001', 'FXTbasic002', 'FXTbasic003', 'FXTbasic004', 'FXTbasic005', 'FXTbasic006',
    ]);
    expect(allPosts.posts[4].caption).toBe('Two tides, edited ✏️');
    expect(allPosts.posts[1].caption).toBe('潮が引いたあとの静かな海 🌊\nВечерний отлив и тишина.\nمساء هادئ على الشاطئ\n#tidepool #lowtide');
    verdict = validateExport(allFolder);
    expect(verdict.status, verdict.output).toBe(0);
    expect(verdict.output).not.toContain('WARN');
    await expect(page.getByTestId('toast').filter({ hasText: 'Downloaded 6 posts as instagram-export.zip.' })).toBeVisible();

    // The download imports into the clone again: the same six posts, with the edited description.
    await openImportDialog(page);
    await page.getByTestId('import-archive-input').setInputFiles(join(DOWNLOADS, 'library-all.zip'));
    await expectImportSuccess(page, 'Imported 6 of 6 posts from library-all.zip');
    await viewFeed(page);
    await expect(banner(page).getByTestId('import-warnings-open')).toHaveCount(0);
    const merged = cachedManifest();
    expect(merged.library.imports.at(-1)).toMatchObject({ source_name: 'library-all.zip', posts: 6, new: 0, updated: 6 });
    expect(merged.posts.find((post) => post.id === 'FXTbasic005')?.caption).toBe('Two tides, edited ✏️');
  });
});

for (const viewport of [{ width: 390, height: 844 }, { width: 320, height: 640 }]) {
  test.describe(`the library on a ${viewport.width}px wide screen`, () => {
    test.use({ viewport });

    test('banner, dialog and the Clear question fit the screen', async ({ page }) => {
      await openApp(page);
      await page.getByTestId('import-open-mobile').click();
      await expect(importDialog(page)).toBeVisible();
      await expect(importDialog(page).getByRole('button', { name: 'Select archive files' })).toBeInViewport();
      expect(await sidewaysOverflow(page)).toBeLessThanOrEqual(0);
      await page.getByTestId('import-archive-input').setInputFiles([fixture('export-part1.zip'), fixture('export-part2.zip')]);
      await expectImportSuccess(page, 'Imported 3 of 3 posts from 2 archives');
      await expect(page.getByTestId('import-saved')).toBeInViewport();
      expect(await sidewaysOverflow(page)).toBeLessThanOrEqual(0);
      await viewFeed(page);

      await expect(banner(page).getByTestId('library-summary')).toBeVisible();
      for (const name of ['Auto view', 'Download all', 'Import more', 'Clear library']) {
        await expect(banner(page).getByRole('button', { name })).toBeInViewport();
      }
      expect(await sidewaysOverflow(page)).toBeLessThanOrEqual(0);

      await banner(page).getByRole('button', { name: 'Clear library' }).click();
      const confirm = page.getByRole('alertdialog', { name: 'Clear library?' });
      await expect(confirm.getByRole('button', { name: 'Cancel' })).toBeInViewport();
      expect(await sidewaysOverflow(page)).toBeLessThanOrEqual(0);
      await confirm.getByRole('button', { name: 'Cancel' }).click();
    });
  });
}
